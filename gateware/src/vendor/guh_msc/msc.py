# Copyright (c) 2024 S. Holzapfel <me@sebholzapfel.com>
#
# SPDX-License-Identifier: BSD-3-Clause
#
# Vendored from guh @ d44315 (github upstream) — see gateware/pyproject.toml pin.
# Diverges from upstream: SCSI WRITE(10) + bulk-OUT data phase (M6b,
# src/top/mbsid/M6_USB_STORAGE.md §4b). Candidate for an upstream PR.
"""
USB Mass Storage Class engine.

Enumerates USB mass storage devices and provides a simple streaming
read interface for user gateware, to fetch desired raw blocks.

A lot of this comes from the various sources linked at:

    https://www.downtowndougbrown.com/2018/12/usb-mass-storage-with-embedded-devices-tips-and-quirks/

"""

from amaranth import *
from amaranth.lib import data, enum, fifo, stream, wiring
from amaranth.lib.cdc import ResetInserter
from amaranth.lib.memory import Memory as LibMemory   # aliased: `from ... import *`
                                                      # below re-exports old hdl.Memory
from amaranth.lib.wiring import In, Out

from luna.gateware.stream.future import Packet

from guh.usbh.enumerator import USBHostEnumerator
# TransferType/DataPID are numerically identical to guh's; TransferResponse
# adds NYET=7 (see sie.py's docstring — the 2026-07-15 write-failure fix).
from .sie import USBSIE, TransferType, TransferResponse, DataPID
from guh.usbh.descriptor import USBDescriptorParser, EndpointFilter
from guh.protocol.descriptors import *

# ============================================================
# SCSI command / wrapper data structures
# ============================================================

CBW_SIGNATURE = 0x43425355
CSW_SIGNATURE = 0x53425355

class SCSIOpCode(enum.Enum, shape=unsigned(8)):
    TEST_UNIT_READY  = 0x00
    REQUEST_SENSE    = 0x03
    READ_CAPACITY_10 = 0x25
    READ_10          = 0x28
    WRITE_10         = 0x2A


class CBWFlags(enum.Enum, shape=unsigned(8)):
    DATA_OUT = 0x00  # Host to device
    DATA_IN  = 0x80  # Device to host


class CSWStatus(enum.Enum, shape=unsigned(8)):
    PASSED      = 0x00
    FAILED      = 0x01
    PHASE_ERROR = 0x02

class CDB6(data.Struct):
    opcode:   unsigned(8)
    _misc:    unsigned(32)
    control:  unsigned(8)
    _padding: unsigned(80)


class CDB10(data.Struct):
    opcode:      unsigned(8)
    flags:       unsigned(8)
    lba_be:      unsigned(32)  # big-endian
    group:       unsigned(8)
    xfer_len_be: unsigned(16)  # big-endian
    control:     unsigned(8)
    _padding:    unsigned(48)


class CBW(data.Struct):
    dCBWSignature:          unsigned(32)
    dCBWTag:                unsigned(32)
    dCBWDataTransferLength: unsigned(32)
    bmCBWFlags:             unsigned(8)
    bCBWLUN:                unsigned(4)
    _reserved1:             unsigned(4)
    bCBWCBLength:           unsigned(5)
    _reserved2:             unsigned(3)
    CBWCB:                  data.UnionLayout({
        "cdb6":  CDB6,
        "cdb10": CDB10,
    })


class CSW(data.Struct):
    dCSWSignature:   unsigned(32)
    dCSWTag:         unsigned(32)
    dCSWDataResidue: unsigned(32)
    bCSWStatus:      unsigned(8)


class ReadCapacity10Response(data.Struct):
    last_lba_be:   unsigned(32)  # big-endian
    block_size_be: unsigned(32)  # big-endian

CBW_SIZE_BYTES = CBW.as_shape().size // 8
CSW_SIZE_BYTES = CSW.as_shape().size // 8
READ_CAPACITY_SIZE_BYTES = ReadCapacity10Response.as_shape().size // 8

# Default max bytes per bulk-OUT transaction. The guh SIE tx FIFO is 64 deep
# and captures its transmit length once (tx_len = tx_fifo.w_level) at
# xfer.start (guh/usbh/sie.py IDLE state), so a single OUT transaction is
# hard-capped at 64 bytes — there is no streaming feed-while-transmitting. A
# 512-byte block is therefore sent as 8 x 64-byte OUT transactions with PID
# toggling per ACK. Overridable per instance (`tx_chunk_bytes`): the 2026-07-15
# hardware bring-up saw 64-byte data packets get NO handshake from a real
# drive while 31-byte CBWs work, with the emitted packet proven bit-perfect at
# the UTMI level in sim (tests/test_guh_sie_tx_packets.py) — a smaller chunk
# A/Bs whether the failure below UTMI (ULPI translator / PHY) is
# length-dependent. NOTE: intermediate short packets are technically out of
# spec for bulk (all but the last should be wMaxPacketSize); most drives
# tolerate it, and a CSW/sense complaint is itself a diagnostic answer.
_TX_CHUNK_BYTES = 64

# ============================================================
# USB MSC / SCSI Command Wrapper Engine
# ============================================================

class SCSIBulkHost(wiring.Component):

    """
    SCSI command wrapper transport engine (bulk-only BBB).
    Issues CBWs, parses CSWs (protocol which encapsulates the actual commands)
    On read ops, data is streamed out rx_data.

    TODO: drop non-streaming mode and punt this to higher layers.
    TODO: handle more error conditions.
    """

    class Command(data.Struct):
        start:       unsigned(1)
        data_len:    unsigned(32)
        stream_data: unsigned(1) # 0=capture to self.captured, 1=stream to rx_data
                                 # TODO: probably cleaner to drop self.captured...
        data_dir:    unsigned(1) # 0=IN (device->host), 1=OUT (host->device)
        cdb:         data.UnionLayout({
            "cdb6":  CDB6,
            "cdb10": CDB10,
        })

    class Status(data.Struct):
        idle:     unsigned(1)
        done:     unsigned(1)
        error:    unsigned(1)
        rejected: unsigned(1)

    cmd:      In(Command)
    status:   Out(Status)
    rx_data:  Out(stream.Signature(Packet(unsigned(8))))
    tx_data:  In(stream.Signature(unsigned(8)))   # OUT payload (data_dir=1 cmds)
    captured: Out(ReadCapacity10Response)
    csw:      Out(CSW)   # last received CSW (status/residue diagnostics)
    # Diagnostics: on a rejected command (Default arm — response neither ACK
    # nor NAK), latch WHAT the SIE reported and WHERE in the exchange.
    reject_response: Out(3)   # TransferResponse value (3=STALL, 4=TIMEOUT, ...)
    reject_phase:    Out(3)   # 0=none yet, 1=CBW, 2=DATA-TX, 3=CSW, 4=DATA-RX,
                              # 5=CTRL (CLEAR_FEATURE recovery itself failed)
    reject_txdone:   Out(4)   # DATA-TX rejects: 32-byte units already ACKed
    # NYET handshakes seen this command (CBW + DATA-TX phases, saturating).
    # Diagnostic for the skipped PING protocol: a STALL with nyets>0 could be
    # a strict HS device objecting to OUT-without-PING after NYET; nyets=0
    # rules that out.
    nyets: Out(8)
    # Live exchange phase (same encoding as reject_phase, 0=idle). Latched
    # OUTSIDE the watchdog reset domain by the CSR peripheral so a wedged
    # engine still reports WHERE it was stuck after the watchdog wipes it
    # (the 2026-07-15 64GB-stick hang came back rej=0/0/0 — all evidence
    # zeroed by the very reset that ended the hang).
    phase_o: Out(3)
    # BOT §6.3.1 CSW validation result: 1 = the last command's CSW failed the
    # signature/tag/length check (transport likely desynced — the MSC layer
    # escalates to Reset Recovery). Registered; cleared on the next cmd.start.
    csw_bad_o: Out(1)
    # Live read-path diagnostics. Firmware samples these before the outer
    # 10-second watchdog can reset the engine.
    rx_bytes_o:       Out(10)  # bytes accepted from the SIE this data-IN phase
    stream_mode_o:    Out(1)   # command's sampled stream_data value
    data_len_512_o:   Out(1)   # sampled data_len equals one 512-byte block
    # REQUEST SENSE support: key/ASC/ASCQ view of the capture buffer
    # ([19:16]=sense key, [15:8]=ASC, [7:0]=ASCQ), valid after a REQUEST
    # SENSE command completes.
    sense: Out(20)
    # BOT §5.3.4 Reset Recovery: strobe rr_start (engine must be IDLE) to run
    # Bulk-Only Mass Storage Reset -> clear IN halt -> clear OUT halt ->
    # reset both toggles. rr_done strobes on success; failure surfaces as
    # status.done+rejected with reject_phase=5 (CTRL), same as clear-halt.
    rr_start: In(1)
    rr_done:  Out(1)

    def __init__(self, *, enumerator=None, tx_chunk_bytes=_TX_CHUNK_BYTES,
                 fullspeed_only=False, **kwargs):
        # `enumerator` is an injection seam for sim (a stub SIE); production code
        # leaves it None and gets the real USBHostEnumerator built from **kwargs.
        assert 1 <= tx_chunk_bytes <= 64   # SIE tx FIFO depth
        self.tx_chunk_bytes = tx_chunk_bytes
        if enumerator is not None:
            self.enumerator = enumerator
        else:
            self.enumerator = USBHostEnumerator(
                **kwargs,
                config_number=1,
                parser=USBDescriptorParser(
                    endpoint_filter=EndpointFilter.IN_AND_OUT,
                    transfer_type=EndpointTransferType.BULK,
                    interface_class=InterfaceClass.MASS_STORAGE,
                    interface_subclass=MSCSubClass.SCSI_TRANSPARENT,
                    interface_protocol=MSCProtocol.BULK_ONLY,
                ),
            )
            # Swap the stock SIE for the vendored one (NYET decode — see
            # sie.py's docstring). fullspeed_only skips the HS chirp so the
            # device operates at FS, where the 64-byte bulk-OUT chunking is
            # exactly wMaxPacketSize (legal) and PING does not exist —
            # round-eight fix for the two critical HS violations.
            self.enumerator.sie = USBSIE(
                bus=kwargs.get("bus"),
                handle_clocking=kwargs.get("handle_clocking", True),
                fullspeed_only=fullspeed_only)
        super().__init__()

    def elaborate(self, platform):
        m = Module()

        m.submodules.enumerator = enum = self.enumerator
        packet_layout = Packet(unsigned(8))

        # RX FIFO with Packet framing (512 bytes + some margin)
        m.submodules.rx_fifo = rx_fifo = DomainRenamer("usb")(fifo.SyncFIFOBuffered(
            width=packet_layout.size, depth=600))
        wiring.connect(m, rx_fifo.r_stream, wiring.flipped(self.rx_data))

        cbw_tag = Signal(32, init=1)
        tx_byte_idx = Signal(6)
        rx_byte_idx = Signal(10)
        rx_data_count = Signal(16)
        data_len = Signal(32)

        csw_sig = Signal(CSW)
        csw_flat = csw_sig.as_value()
        # Non-streamed capture buffer, sized for the widest capture command:
        # REQUEST SENSE (18 bytes) > READ CAPACITY (8 bytes). `captured` views
        # the low 8 bytes; `sense` picks key/ASC/ASCQ out of the sense layout.
        captured_flat = Signal(unsigned(8 * 18))
        m.d.comb += self.captured.eq(ReadCapacity10Response(captured_flat[0:64]))
        m.d.comb += self.sense.eq(Cat(
            captured_flat[13*8:14*8],    # ASCQ (byte 13)
            captured_flat[12*8:13*8],    # ASC  (byte 12)
            captured_flat[2*8:2*8+4],    # sense key (byte 2, low nibble)
        ))
        m.d.comb += self.csw.eq(csw_sig)

        endp_in = enum.parser.o.i_endp.number
        endp_out = enum.parser.o.o_endp.number
        pid_in = Signal(DataPID, init=DataPID.DATA0)
        pid_out = Signal(DataPID, init=DataPID.DATA0)

        rx_packet = packet_layout(rx_fifo.w_stream.payload)
        stream_mode = Signal()
        data_dir_r = Signal()   # latched cmd.data_dir (0=IN, 1=OUT)
        m.d.comb += [
            self.rx_bytes_o.eq(Mux(
                rx_data_count >= 0x3FF, 0x3FF, rx_data_count[:10])),
            self.stream_mode_o.eq(stream_mode),
            self.data_len_512_o.eq(data_len == 512),
        ]

        # --- STALL recovery (BOT §6.7: clear the endpoint halt, then read the
        # CSW to learn WHY the device bailed). ch_ep is the CLEAR_FEATURE
        # wIndex low byte: endpoint number, bit 7 = IN direction.
        ch_ep = Signal(8)
        ch_retry = Signal(2)          # SETUP NAK/TIMEOUT retries (bounded)
        csw_halt_retried = Signal()   # only one CSW clear-halt per command
        setup_ix = Signal(range(8))
        rr_setup = Signal()     # SETUP loader source: 0=CLEAR_FEATURE, 1=MSC reset
        ch_next = Signal(2)     # after CLEAR-HALT-STATUS-WAIT ACK: 0 = go read
                                 # the CSW (mid-command recovery, pre-round-eight
                                 # behavior); 1 = recovery: clear OUT halt next;
                                 # 2 = recovery: finished, strobe rr_done.
        # Two SETUP payloads share the loader. CLEAR_FEATURE(ENDPOINT_HALT):
        # bmRequestType=0x02 (endpoint), bRequest=1, wValue=0, wIndex=ch_ep.
        # Bulk-Only Mass Storage Reset (BOT §5.3.4): bmRequestType=0x21
        # (class, interface), bRequest=0xFF, wValue=0, wIndex=interface.
        # wIndex is hardwired to interface 0: the guh descriptor parser
        # exposes only endpoint numbers, and BOT thumb drives are
        # single-interface in practice (documented limitation).
        ch_setup_byte = Signal(8)
        with m.Switch(setup_ix):
            for i, (cf_b, rr_b) in enumerate([(0x02, 0x21), (0x01, 0xFF),
                                              (0x00, 0x00), (0x00, 0x00),
                                              (None, 0x00), (0x00, 0x00),
                                              (0x00, 0x00), (0x00, 0x00)]):
                with m.Case(i):
                    m.d.comb += ch_setup_byte.eq(
                        Mux(rr_setup, rr_b,
                            ch_ep if cf_b is None else cf_b))

        # --- bulk-OUT (write) data phase state ---------------------------------
        # tx_chunk_bytes-sized chunks (default 64, see _TX_CHUNK_BYTES). The
        # SIE drains its own tx FIFO on
        # every transfer completion (guh/usbh/sie.py IPD_DRAIN_TX), including on a
        # NAK, so a NAK'd chunk's payload is gone from the SIE. We therefore keep
        # a local replay buffer here, filled from tx_data during DATA-TX-LOAD and
        # re-fed (same PID) from DATA-TX-RELOAD if the device NAKs.
        m.submodules.tx_replay = tx_replay = LibMemory(
            shape=unsigned(8), depth=self.tx_chunk_bytes, init=[])
        replay_wport = tx_replay.write_port(domain="usb")
        replay_rport = tx_replay.read_port(domain="comb")

        tx_sent  = Signal(range(self.tx_chunk_bytes + 1))  # bytes fed to SIE this txn
        tx_total = Signal(32)                          # bytes ACKed this phase
        tx_remaining = Signal(32)
        chunk_len = Signal(range(self.tx_chunk_bytes + 1))
        m.d.comb += [
            tx_remaining.eq(data_len - tx_total),
            chunk_len.eq(Mux(tx_remaining >= self.tx_chunk_bytes,
                             self.tx_chunk_bytes, tx_remaining)),
        ]

        # Build CBW from command
        cbw_sig = Signal(CBW)
        cdb_opcode = CDB6(self.cmd.cdb.cdb6).opcode
        cdb_len = Signal(5)
        m.d.comb += cdb_len.eq(Mux(
            (cdb_opcode == SCSIOpCode.TEST_UNIT_READY) | (cdb_opcode == SCSIOpCode.REQUEST_SENSE),
            6, 10))

        m.d.comb += [
            cbw_sig.dCBWSignature.eq(CBW_SIGNATURE),
            cbw_sig.dCBWTag.eq(cbw_tag),
            cbw_sig.dCBWDataTransferLength.eq(self.cmd.data_len),
            # Direction is now explicit (self.cmd.data_dir) rather than inferred
            # from data_len. Zero-length commands keep DATA_OUT(0x00), matching
            # upstream exactly.
            cbw_sig.bmCBWFlags.eq(Mux(self.cmd.data_len > 0,
                                      Mux(self.cmd.data_dir,
                                          CBWFlags.DATA_OUT, CBWFlags.DATA_IN),
                                      CBWFlags.DATA_OUT)),
            cbw_sig.bCBWCBLength.eq(cdb_len),
            cbw_sig.CBWCB.eq(self.cmd.cdb),
        ]

        cbw_flat = cbw_sig.as_value()
        cbw_byte_out = Signal(8)
        m.d.comb += cbw_byte_out.eq((cbw_flat >> (tx_byte_idx * 8)) & 0xFF)

        def start_bulk_out(endp):
            return [
                enum.ctrl.xfer.start.eq(1),
                enum.ctrl.xfer.type.eq(TransferType.OUT),
                enum.ctrl.xfer.data_pid.eq(pid_out),
                enum.ctrl.xfer.dev_addr.eq(enum.status.dev_addr),
                enum.ctrl.xfer.ep_addr.eq(endp),
            ]

        def start_bulk_in(endp):
            return [
                enum.ctrl.xfer.start.eq(1),
                enum.ctrl.xfer.type.eq(TransferType.IN),
                enum.ctrl.xfer.data_pid.eq(pid_in),
                enum.ctrl.xfer.dev_addr.eq(enum.status.dev_addr),
                enum.ctrl.xfer.ep_addr.eq(endp),
            ]

        m.d.comb += self.status.idle.eq(0)

        with m.FSM(domain="usb") as fsm:

            with m.State("WAIT-ENUMERATION"):
                with m.If(enum.status.enumerated & enum.parser.o.valid):
                    m.next = "IDLE"

            with m.State("IDLE"):
                m.d.comb += self.status.idle.eq(1)
                with m.If(self.rr_start):
                    m.d.usb += [
                        rr_setup.eq(1),
                        ch_retry.eq(0),
                        setup_ix.eq(0),
                    ]
                    m.next = "CLEAR-HALT-LOAD"
                with m.Elif(self.cmd.start):
                    m.d.usb += [
                        tx_byte_idx.eq(0),
                        rx_data_count.eq(0),
                        data_len.eq(self.cmd.data_len),
                        stream_mode.eq(self.cmd.stream_data),
                        data_dir_r.eq(self.cmd.data_dir),
                        tx_total.eq(0),
                        self.nyets.eq(0),
                        csw_halt_retried.eq(0),
                        ch_retry.eq(0),
                        ch_next.eq(0),
                        # Per-command reject scope: the CSR peripheral latches
                        # these on change-to-nonzero, so they must return to 0
                        # between commands or a stale reject would re-latch
                        # after the peripheral's own clear-on-strobe.
                        self.reject_response.eq(0),
                        self.reject_phase.eq(0),
                        self.reject_txdone.eq(0),
                        self.csw_bad_o.eq(0),
                    ]
                    m.next = "CBW-LOAD"

            with m.State("CBW-LOAD"):
                m.d.comb += [
                    enum.ctrl.txs.valid.eq(1),
                    enum.ctrl.txs.payload.eq(cbw_byte_out),
                ]
                with m.If(enum.ctrl.txs.ready):
                    m.d.usb += tx_byte_idx.eq(tx_byte_idx + 1)
                    with m.If(tx_byte_idx == CBW_SIZE_BYTES - 1):
                        m.d.usb += tx_byte_idx.eq(0)
                        m.next = "CBW-XFER"

            with m.State("CBW-XFER"):
                with m.If(enum.ctrl.status.idle):
                    m.d.comb += start_bulk_out(endp_out)
                    m.next = "CBW-WAIT"

            with m.State("CBW-WAIT"):
                with m.If(enum.ctrl.status.idle):
                    with m.Switch(enum.ctrl.status.response):
                        # NYET (HS only) = packet ACCEPTED, endpoint busy for
                        # the NEXT one — advance exactly like ACK. The "busy"
                        # half is handled by the existing NAK-replay path on
                        # the following transaction (we skip the optional
                        # PING protocol; devices tolerate OUT->NAK).
                        with m.Case(TransferResponse.ACK,
                                    TransferResponse.NYET):
                            m.d.usb += [
                                pid_out.eq(Mux(pid_out, DataPID.DATA0, DataPID.DATA1)),
                                rx_byte_idx.eq(0),
                                rx_data_count.eq(0),
                            ]
                            # .as_value() comparison: the production SIE's
                            # `response` is an EnumView of guh's enum class;
                            # `==` across enum classes raises TypeError (the
                            # m.Case arms compare by value and don't care).
                            with m.If((enum.ctrl.status.response.as_value()
                                       == TransferResponse.NYET.value)
                                      & (self.nyets != 0xFF)):
                                m.d.usb += self.nyets.eq(self.nyets + 1)
                            with m.If(data_len > 0):
                                with m.If(data_dir_r):
                                    m.next = "DATA-TX-START"
                                with m.Else():
                                    m.next = "DATA"
                            with m.Else():
                                m.next = "CSW"
                        with m.Case(TransferResponse.NAK):
                            # NAK on the CBW OUT is flow control (drive busy,
                            # e.g. flash housekeeping right after a write),
                            # NOT rejection — re-send the identical CBW with
                            # the same PID (toggle advances only on ACK).
                            # Found on hardware 2026-07-15: the Default arm
                            # below failed every export whose CBW was NAKed;
                            # reads never provoked it (idle drives accept
                            # CBWs immediately). The CBW bytes regenerate
                            # combinationally from self.cmd (held by the
                            # caller's *-WAIT state), so CBW-LOAD refills the
                            # drained SIE FIFO correctly. Endless NAK is
                            # backstopped by the caller's watchdog, same as
                            # the CSW retry loop. Upstream guh has this same
                            # bug on the read path — PR candidate.
                            m.d.usb += tx_byte_idx.eq(0)
                            m.next = "CBW-LOAD"
                        with m.Default():
                            m.d.usb += [
                                self.reject_response.eq(enum.ctrl.status.response),
                                self.reject_phase.eq(1),   # CBW
                            ]
                            m.d.comb += [
                                self.status.done.eq(1),
                                self.status.rejected.eq(1),
                            ]
                            m.next = "IDLE"

            with m.State("DATA"):
                with m.If(enum.ctrl.status.idle):
                    m.d.comb += start_bulk_in(endp_in)
                    m.next = "DATA-RX"

            with m.State("DATA-RX"):
                with m.If(stream_mode):
                    m.d.comb += [
                        enum.ctrl.rxs.ready.eq(rx_fifo.w_stream.ready),
                        rx_fifo.w_stream.valid.eq(enum.ctrl.rxs.valid),
                        rx_packet.data.eq(enum.ctrl.rxs.payload),
                        rx_packet.first.eq(rx_data_count == 0),
                        rx_packet.last.eq(rx_data_count == (data_len - 1)),
                    ]
                with m.Else():
                    m.d.comb += enum.ctrl.rxs.ready.eq(1)
                    with m.If(enum.ctrl.rxs.valid):
                        m.d.usb += captured_flat.word_select(rx_byte_idx, 8).eq(enum.ctrl.rxs.payload)

                with m.If(enum.ctrl.rxs.valid & enum.ctrl.rxs.ready):
                    m.d.usb += [
                        rx_byte_idx.eq(rx_byte_idx + 1),
                        rx_data_count.eq(rx_data_count + 1),
                    ]

                with m.If(enum.ctrl.status.idle):
                    with m.Switch(enum.ctrl.status.response):
                        with m.Case(TransferResponse.ACK):
                            m.d.usb += pid_in.eq(Mux(pid_in, DataPID.DATA0, DataPID.DATA1))
                            with m.If(rx_data_count >= data_len):
                                m.d.usb += rx_byte_idx.eq(0)
                                m.next = "CSW"
                            with m.Else():
                                m.next = "DATA"
                        with m.Case(TransferResponse.NAK):
                            m.next = "DATA"
                        with m.Default():
                            # STALL/TIMEOUT/CRC on the data-IN phase: fail the
                            # command instead of looping forever. (Gap found
                            # 2026-07-15 alongside the CSW-RX one below —
                            # previously no Default arm existed and the switch
                            # simply retried nothing, wedging until watchdog.)
                            m.d.usb += [
                                self.reject_response.eq(enum.ctrl.status.response),
                                self.reject_phase.eq(4),   # DATA-RX
                            ]
                            m.d.comb += [
                                self.status.done.eq(1),
                                self.status.rejected.eq(1),
                            ]
                            m.next = "IDLE"

            # --- bulk-OUT (write) data phase -----------------------------------
            # One OUT transaction per chunk_len (<= _TX_CHUNK_BYTES) bytes.

            with m.State("DATA-TX-START"):
                m.d.usb += tx_sent.eq(0)
                m.next = "DATA-TX-LOAD"

            with m.State("DATA-TX-LOAD"):
                # Fresh chunk: pull bytes from tx_data into the SIE tx FIFO, and
                # mirror each into the replay buffer for a possible NAK re-send.
                m.d.comb += [
                    enum.ctrl.txs.valid.eq(self.tx_data.valid),
                    enum.ctrl.txs.payload.eq(self.tx_data.payload),
                    self.tx_data.ready.eq(enum.ctrl.txs.ready),
                    replay_wport.addr.eq(tx_sent),
                    replay_wport.data.eq(self.tx_data.payload),
                    replay_wport.en.eq(self.tx_data.valid & enum.ctrl.txs.ready),
                ]
                with m.If(self.tx_data.valid & enum.ctrl.txs.ready):
                    m.d.usb += tx_sent.eq(tx_sent + 1)
                    with m.If(tx_sent == chunk_len - 1):
                        m.d.usb += tx_sent.eq(0)
                        m.next = "DATA-TX-XFER"

            with m.State("DATA-TX-RELOAD"):
                # NAK re-send: re-fill the SIE tx FIFO from the replay buffer
                # (comb read port -> byte available at addr=tx_sent this cycle).
                m.d.comb += [
                    replay_rport.addr.eq(tx_sent),
                    enum.ctrl.txs.valid.eq(1),
                    enum.ctrl.txs.payload.eq(replay_rport.data),
                ]
                with m.If(enum.ctrl.txs.ready):
                    m.d.usb += tx_sent.eq(tx_sent + 1)
                    with m.If(tx_sent == chunk_len - 1):
                        m.d.usb += tx_sent.eq(0)
                        m.next = "DATA-TX-XFER"

            with m.State("DATA-TX-XFER"):
                with m.If(enum.ctrl.status.idle):
                    m.d.comb += start_bulk_out(endp_out)
                    m.next = "DATA-TX-WAIT"

            with m.State("DATA-TX-WAIT"):
                with m.If(enum.ctrl.status.idle):
                    with m.Switch(enum.ctrl.status.response):
                        # NYET = accepted, same as ACK (see CBW-WAIT note) —
                        # this was the 2026-07-15 hardware failure: HS drives
                        # NYET write-data packets as flow control, and the
                        # stock SIE reported that as TIMEOUT (rej=4/2/0).
                        with m.Case(TransferResponse.ACK,
                                    TransferResponse.NYET):
                            m.d.usb += [
                                pid_out.eq(Mux(pid_out, DataPID.DATA0, DataPID.DATA1)),
                                tx_total.eq(tx_total + chunk_len),
                            ]
                            # .as_value() comparison: the production SIE's
                            # `response` is an EnumView of guh's enum class;
                            # `==` across enum classes raises TypeError (the
                            # m.Case arms compare by value and don't care).
                            with m.If((enum.ctrl.status.response.as_value()
                                       == TransferResponse.NYET.value)
                                      & (self.nyets != 0xFF)):
                                m.d.usb += self.nyets.eq(self.nyets + 1)
                            with m.If(tx_total + chunk_len >= data_len):
                                m.d.usb += rx_byte_idx.eq(0)
                                m.next = "CSW"
                            with m.Else():
                                m.next = "DATA-TX-START"
                        with m.Case(TransferResponse.NAK):
                            # Same data, same PID: replay from the local buffer.
                            m.d.usb += tx_sent.eq(0)
                            m.next = "DATA-TX-RELOAD"
                        with m.Case(TransferResponse.STALL):
                            # The device halted the bulk-OUT endpoint to
                            # truncate the data phase (legal per BOT §6.7.3 —
                            # e.g. it already knows the write fails). Recover
                            # per spec: CLEAR_FEATURE(ENDPOINT_HALT) on the
                            # OUT endpoint, then read the CSW, which reports
                            # WHY (and unlocks the auto-REQUEST-SENSE path).
                            # Found on hardware 2026-07-15 round five: the
                            # 8GB stick STALLed after 2 data packets and,
                            # with no recovery, every later CBW bounced off
                            # the halted endpoint until the watchdog.
                            # reject_* is latched as a diagnostic breadcrumb
                            # even though the command itself continues.
                            m.d.usb += [
                                self.reject_response.eq(enum.ctrl.status.response),
                                self.reject_phase.eq(2),   # DATA-TX
                                self.reject_txdone.eq(tx_total[5:9]),
                                ch_ep.eq(endp_out),
                                ch_retry.eq(0),
                            ]
                            m.next = "CLEAR-HALT-LOAD"
                        with m.Default():
                            m.d.usb += [
                                self.reject_response.eq(enum.ctrl.status.response),
                                self.reject_phase.eq(2),   # DATA-TX
                                self.reject_txdone.eq(tx_total[5:9]),
                            ]
                            m.d.comb += [
                                self.status.done.eq(1),
                                self.status.rejected.eq(1),
                            ]
                            m.next = "IDLE"

            with m.State("CSW"):
                with m.If(enum.ctrl.status.idle):
                    m.d.comb += start_bulk_in(endp_in)
                    m.d.usb += rx_byte_idx.eq(0)
                    m.next = "CSW-RX"

            with m.State("CSW-RX"):
                m.d.comb += enum.ctrl.rxs.ready.eq(1)
                with m.If(enum.ctrl.rxs.valid):
                    m.d.usb += [
                        csw_flat.word_select(rx_byte_idx, 8).eq(enum.ctrl.rxs.payload),
                        rx_byte_idx.eq(rx_byte_idx + 1),
                    ]

                with m.If(enum.ctrl.status.idle):
                    with m.Switch(enum.ctrl.status.response):
                        with m.Case(TransferResponse.ACK):
                            # BOT §6.3.1: the host shall consider the CSW
                            # valid only if it is exactly 13 bytes, carries
                            # the CSW signature, and echoes the CBW's tag.
                            # Anything else means the transport is desynced
                            # (e.g. stale IN data misparsed as a CSW) and
                            # MUST NOT be reported as command status —
                            # before round eight a garbage "CSW" with a zero
                            # status byte reported false success.
                            csw_ok = ((rx_byte_idx == CSW_SIZE_BYTES)
                                      & (csw_sig.dCSWSignature == CSW_SIGNATURE)
                                      & (csw_sig.dCSWTag == cbw_tag))
                            m.d.usb += [
                                pid_in.eq(Mux(pid_in, DataPID.DATA0, DataPID.DATA1)),
                                cbw_tag.eq(cbw_tag + 1),
                            ]
                            with m.If(csw_ok):
                                m.d.comb += [
                                    self.status.done.eq(1),
                                    self.status.error.eq(csw_sig.bCSWStatus != CSWStatus.PASSED),
                                ]
                            with m.Else():
                                m.d.usb += [
                                    self.csw_bad_o.eq(1),
                                    self.reject_response.eq(enum.ctrl.status.response),
                                    self.reject_phase.eq(3),   # CSW
                                ]
                                m.d.comb += [
                                    self.status.done.eq(1),
                                    self.status.rejected.eq(1),
                                ]
                            m.next = "IDLE"
                        with m.Case(TransferResponse.NAK):
                            m.next = "CSW"
                        with m.Case(TransferResponse.STALL):
                            # A STALLed CSW: BOT §5.3.3/6.7.2 says clear the
                            # IN endpoint's halt and retry the CSW read —
                            # once. A second STALL means reset recovery is
                            # needed; fail the command promptly (found on
                            # hardware 2026-07-15: with no Default arm the
                            # FSM sat in CSW-RX until the 10 s watchdog,
                            # whose reset also zeroed these diagnostics).
                            m.d.usb += [
                                self.reject_response.eq(enum.ctrl.status.response),
                                self.reject_phase.eq(3),   # CSW
                            ]
                            with m.If(~csw_halt_retried):
                                m.d.usb += [
                                    csw_halt_retried.eq(1),
                                    ch_ep.eq(0x80 | endp_in),
                                    ch_retry.eq(0),
                                ]
                                m.next = "CLEAR-HALT-LOAD"
                            with m.Else():
                                m.d.comb += [
                                    self.status.done.eq(1),
                                    self.status.rejected.eq(1),
                                ]
                                m.next = "IDLE"
                        with m.Default():
                            # TIMEOUT/CRC on the CSW — fail promptly.
                            m.d.usb += [
                                self.reject_response.eq(enum.ctrl.status.response),
                                self.reject_phase.eq(3),   # CSW
                            ]
                            m.d.comb += [
                                self.status.done.eq(1),
                                self.status.rejected.eq(1),
                            ]
                            m.next = "IDLE"

            # --- CLEAR_FEATURE(ENDPOINT_HALT) recovery ---------------------
            # A control transfer on ep0 driven through the same pass-through
            # SIE interface the bulk states use (mirrors the enumerator's
            # SETUP helpers): SETUP(DATA0, 8 bytes) -> IN status stage
            # (DATA1, ZLP). On success the halted endpoint's data toggle
            # resets to DATA0 on both sides (USB 2.0 §9.4.5) and we proceed
            # to the CSW, which reports why the device halted mid-command.

            with m.State("CLEAR-HALT-LOAD"):
                m.d.comb += [
                    enum.ctrl.txs.valid.eq(1),
                    enum.ctrl.txs.payload.eq(ch_setup_byte),
                ]
                with m.If(enum.ctrl.txs.ready):
                    m.d.usb += setup_ix.eq(setup_ix + 1)
                    with m.If(setup_ix == 7):
                        m.d.usb += setup_ix.eq(0)
                        m.next = "CLEAR-HALT-XFER"

            with m.State("CLEAR-HALT-XFER"):
                with m.If(enum.ctrl.status.idle):
                    m.d.comb += [
                        enum.ctrl.xfer.start.eq(1),
                        enum.ctrl.xfer.type.eq(TransferType.SETUP),
                        enum.ctrl.xfer.data_pid.eq(DataPID.DATA0),
                        enum.ctrl.xfer.dev_addr.eq(enum.status.dev_addr),
                        enum.ctrl.xfer.ep_addr.eq(0),
                    ]
                    m.next = "CLEAR-HALT-SETUP-WAIT"

            with m.State("CLEAR-HALT-SETUP-WAIT"):
                with m.If(enum.ctrl.status.idle):
                    with m.Switch(enum.ctrl.status.response):
                        with m.Case(TransferResponse.ACK):
                            m.next = "CLEAR-HALT-STATUS"
                        with m.Case(TransferResponse.NAK,
                                    TransferResponse.TIMEOUT):
                            # SETUP may not be NAKed per spec, but tolerate a
                            # flaky link with a bounded retry (the SIE drains
                            # its tx FIFO on completion, so reload the bytes).
                            with m.If(ch_retry == 3):
                                m.d.usb += [
                                    self.reject_response.eq(
                                        enum.ctrl.status.response),
                                    self.reject_phase.eq(5),   # CTRL
                                    rr_setup.eq(0),
                                    ch_next.eq(0),
                                ]
                                m.d.comb += [
                                    self.status.done.eq(1),
                                    self.status.rejected.eq(1),
                                ]
                                m.next = "IDLE"
                            with m.Else():
                                m.d.usb += ch_retry.eq(ch_retry + 1)
                                m.next = "CLEAR-HALT-LOAD"
                        with m.Default():
                            m.d.usb += [
                                self.reject_response.eq(
                                    enum.ctrl.status.response),
                                self.reject_phase.eq(5),   # CTRL
                                rr_setup.eq(0),
                                ch_next.eq(0),
                            ]
                            m.d.comb += [
                                self.status.done.eq(1),
                                self.status.rejected.eq(1),
                            ]
                            m.next = "IDLE"

            with m.State("CLEAR-HALT-STATUS"):
                with m.If(enum.ctrl.status.idle):
                    m.d.comb += [
                        enum.ctrl.xfer.start.eq(1),
                        enum.ctrl.xfer.type.eq(TransferType.IN),
                        enum.ctrl.xfer.data_pid.eq(DataPID.DATA1),
                        enum.ctrl.xfer.dev_addr.eq(enum.status.dev_addr),
                        enum.ctrl.xfer.ep_addr.eq(0),
                    ]
                    m.next = "CLEAR-HALT-STATUS-WAIT"

            with m.State("CLEAR-HALT-STATUS-WAIT"):
                m.d.comb += enum.ctrl.rxs.ready.eq(1)   # drain the status ZLP
                with m.If(enum.ctrl.status.idle):
                    with m.Switch(enum.ctrl.status.response):
                        with m.Case(TransferResponse.ACK):
                            with m.If(rr_setup):
                                # MSC reset done -> clear the IN halt next.
                                m.d.usb += [
                                    rr_setup.eq(0),
                                    ch_ep.eq(0x80 | endp_in),
                                    ch_retry.eq(0),
                                    ch_next.eq(1),
                                ]
                                m.next = "CLEAR-HALT-LOAD"
                            with m.Elif(ch_next == 1):
                                # IN halt cleared -> reset its toggle, clear
                                # the OUT halt next (USB 2.0 §9.4.5).
                                m.d.usb += [
                                    pid_in.eq(DataPID.DATA0),
                                    ch_ep.eq(endp_out),
                                    ch_retry.eq(0),
                                    ch_next.eq(2),
                                ]
                                m.next = "CLEAR-HALT-LOAD"
                            with m.Elif(ch_next == 2):
                                # OUT halt cleared -> recovery complete.
                                m.d.usb += [
                                    pid_out.eq(DataPID.DATA0),
                                    ch_next.eq(0),
                                ]
                                m.d.comb += self.rr_done.eq(1)
                                m.next = "IDLE"
                            with m.Else():
                                # Pre-round-eight path: mid-command clear-halt
                                # -> reset the one toggle and read the CSW.
                                with m.If(ch_ep[7]):
                                    m.d.usb += pid_in.eq(DataPID.DATA0)
                                with m.Else():
                                    m.d.usb += pid_out.eq(DataPID.DATA0)
                                m.d.usb += rx_byte_idx.eq(0)
                                m.next = "CSW"
                        with m.Case(TransferResponse.NAK):
                            m.next = "CLEAR-HALT-STATUS"
                        with m.Default():
                            m.d.usb += [
                                self.reject_response.eq(
                                    enum.ctrl.status.response),
                                self.reject_phase.eq(5),   # CTRL
                                rr_setup.eq(0),
                                ch_next.eq(0),
                            ]
                            m.d.comb += [
                                self.status.done.eq(1),
                                self.status.rejected.eq(1),
                            ]
                            m.next = "IDLE"

        # Live phase decode for phase_o (see port comment). Same encoding as
        # reject_phase; 0 = idle/enumeration. REGISTERED: this decode reads
        # the whole FSM state register and feeds a cross-module diagnostic
        # path (engine -> top glue -> CSR peripheral change-detect); a comb
        # version measurably worsened routing congestion at 94% LUT. One
        # cycle of lag is irrelevant for a stuck-phase breadcrumb.
        ph_cbw = (fsm.ongoing("CBW-LOAD") | fsm.ongoing("CBW-XFER")
                  | fsm.ongoing("CBW-WAIT"))
        ph_tx = (fsm.ongoing("DATA-TX-START") | fsm.ongoing("DATA-TX-LOAD")
                 | fsm.ongoing("DATA-TX-RELOAD") | fsm.ongoing("DATA-TX-XFER")
                 | fsm.ongoing("DATA-TX-WAIT"))
        ph_csw = fsm.ongoing("CSW") | fsm.ongoing("CSW-RX")
        ph_rx = fsm.ongoing("DATA") | fsm.ongoing("DATA-RX")
        ph_ctrl = (fsm.ongoing("CLEAR-HALT-LOAD") | fsm.ongoing("CLEAR-HALT-XFER")
                   | fsm.ongoing("CLEAR-HALT-SETUP-WAIT")
                   | fsm.ongoing("CLEAR-HALT-STATUS")
                   | fsm.ongoing("CLEAR-HALT-STATUS-WAIT"))
        m.d.usb += self.phase_o.eq(
            Mux(ph_cbw, 1, Mux(ph_tx, 2, Mux(ph_csw, 3,
                Mux(ph_rx, 4, Mux(ph_ctrl, 5, 0))))))

        return m


# ============================================================
# (the actual high level) USB MSC Engine
# ============================================================

class USBMSCHost(wiring.Component):
    """
    USB Mass Storage Class Host - read-only block device interface.

    Performs MSC-specific SCSI initialization (TEST UNIT READY, READ CAPACITY)
    before accepting block read commands.

    Usage:
    1. Wait for status.ready == 1
    2. Check status.block_count and status.block_size for device capacity
    3. Set cmd.lba to desired block address and strobe cmd.start
    4. If the read succeeds, status.block_size bytes are streamed out on rx_data
    5. Check resp.done and resp.error

    Eventually, this engine could be used to feed a pure-gateware DMA engine.

    TODO: exponential backoff instead of dumb retries?
    TODO: write support?
    TODO: cleaner way to do be/le conversion?
    """

    class Status(data.Struct):
        connected:   unsigned(1)     # device enumerated
        ready:       unsigned(1)     # device ready for block ops
        busy:        unsigned(1)     # scsi op in progress
        block_size:  unsigned(16)    # block size in bytes (typically 512)
        block_count: unsigned(32)    # total number of blocks

    class Command(data.Struct):
        start: unsigned(1)           # Strobe to begin transfer
        lba:   unsigned(32)          # block address to read/write
        write: unsigned(1)           # 0=read block, 1=write block (start+write=1)

    class Response(data.Struct):
        done:  unsigned(1)           # Transfer complete (strobed for 1 cycle)
        error: unsigned(1)           # CSW indicated failure

    _WATCHDOG_CYCLES = 10 * 60000000  # ~10 seconds at 60MHz
                                      # Some things (like SSDs) can take >5sec to start emitting blocks.
    _INIT_RETRY_MAX = 10
    _BLOCKS_PER_READ = 1

    status:  Out(Status)
    cmd:     In(Command)
    resp:    Out(Response)
    rx_data: Out(stream.Signature(Packet(unsigned(8))))
    tx_data: In(stream.Signature(unsigned(8)))   # block payload for write cmds
    csw:     Out(CSW)   # last CSW (pass-through from SCSIBulkHost, diagnostics)
    reject_response: Out(3)   # pass-through diagnostics (see SCSIBulkHost)
    reject_phase:    Out(3)
    reject_txdone:   Out(4)
    nyets:           Out(8)   # pass-through: NYETs this command
    phase_o:         Out(3)   # pass-through: live exchange phase
    csw_bad_o:       Out(1)   # pass-through: BOT §6.3.1 CSW validation failed
    rx_bytes_o:       Out(10)
    stream_mode_o:    Out(1)
    data_len_512_o:   Out(1)
    speed_o:         Out(2)   # negotiated link speed (USBHostSpeed)
    # Auto-REQUEST-SENSE result after a failed WRITE ([19:16]=key, [15:8]=ASC,
    # [7:0]=ASCQ). `sense_valid` set once captured, cleared on the next
    # cmd.start. Issuing REQUEST SENSE after CHECK CONDITION is required BOT
    # citizenship — some drives fail/STALL every later command until their
    # pending sense data is drained (observed hardware cascade, 2026-07-15).
    sense_o:       Out(20)
    sense_valid_o: Out(1)

    def __init__(self, *, bus=None, handle_clocking=True, device_address=0x12,
                 tx_chunk_bytes=_TX_CHUNK_BYTES, fullspeed_only=False):
        self.scsi = SCSIBulkHost(
            bus=bus,
            handle_clocking=handle_clocking,
            device_address=device_address,
            tx_chunk_bytes=tx_chunk_bytes,
            fullspeed_only=fullspeed_only,
        )
        super().__init__()

    @property
    def sie(self):
        """Expose internal SIE for bus forwarding / testing."""
        return self.scsi.enumerator.sie

    def elaborate(self, platform):
        m = Module()

        m.submodules.scsi = scsi = self.scsi
        enum = scsi.enumerator

        wiring.connect(m, scsi.rx_data, wiring.flipped(self.rx_data))
        wiring.connect(m, wiring.flipped(self.tx_data), scsi.tx_data)
        m.d.comb += [
            self.csw.eq(scsi.csw),
            self.reject_response.eq(scsi.reject_response),
            self.reject_phase.eq(scsi.reject_phase),
            self.reject_txdone.eq(scsi.reject_txdone),
            self.nyets.eq(scsi.nyets),
            self.phase_o.eq(scsi.phase_o),
            self.csw_bad_o.eq(scsi.csw_bad_o),
            self.rx_bytes_o.eq(scsi.rx_bytes_o),
            self.stream_mode_o.eq(scsi.stream_mode_o),
            self.data_len_512_o.eq(scsi.data_len_512_o),
        ]
        # Sim stubs (StubEnumerator) have no SIE; leave speed_o at 0 there.
        if hasattr(scsi.enumerator, "sie"):
            m.d.comb += self.speed_o.eq(
                scsi.enumerator.sie.ctrl.status.detected_speed)

        block_size = Signal(16, init=512)
        block_count = Signal(32)
        current_lba = Signal(32)
        is_write = Signal()
        init_retry = Signal(range(self._INIT_RETRY_MAX + 1))
        # Set entering TEST-UNIT-READY from RECOVERY-WAIT (post BOT §5.3.4
        # reset recovery): the drive's capacity hasn't changed, so a
        # successful revalidation goes straight to READY instead of
        # re-running READ CAPACITY (the normal WAIT-ENUMERATION path).
        post_recovery = Signal()
        # Snapshot of scsi.status.{error,rejected} taken on the status.done
        # cycle: scsi's own diagnostic registers consumed by need_rr below
        # (csw_bad_o, reject_phase, reject_response) are m.d.usb-updated on
        # that SAME edge inside SCSIBulkHost, so they lag one usb cycle
        # behind status.done itself — reading them combinationally in the
        # same state as status.done sees their PRE-update (stale) value.
        # *-DONE states below wait exactly one cycle so both these local
        # snapshots and scsi's registers have settled together. (Found via
        # test: a first-time bad-tag CSW read csw_bad_o as still 0 on the
        # status.done cycle and silently skipped recovery — masked in the
        # double-STALL case only because reject_phase/response were already
        # left at the needed values by the FIRST STALL, several cycles
        # earlier.)
        xfer_error_r = Signal()
        xfer_rejected_r = Signal()
        sense_r = Signal(20)
        sense_valid_r = Signal()
        m.d.comb += [
            self.sense_o.eq(sense_r),
            self.sense_valid_o.eq(sense_valid_r),
        ]

        watchdog = Signal(32)
        watchdog_expired = Signal()
        m.d.usb += watchdog.eq(watchdog + 1)
        m.d.comb += watchdog_expired.eq(watchdog >= (self._WATCHDOG_CYCLES - 1))

        # Handshake-fed watchdog (round seven, 2026-07-15). A drive answering
        # NAK is PRESENT and flow-controlling (e.g. FTL commit right after a
        # write); the completion-fed watchdog above used to hard-reset the
        # whole engine 10 s into any long NAK wait — round six proved the
        # post-write READ(10) dies exactly this way, wiping the diagnostics
        # with it. Hold the watchdog cleared while the SIE's last completed
        # transaction ended in a live handshake. Deliberately EXCLUDED so the
        # reset keeps its unplug/recovery role:
        #   TIMEOUT/NONE - silent bus is the only unplug signal there is
        #                  (the firmware keepalive supplies the probe traffic);
        #   STALL        - bounded by the clear-halt recovery paths; a drive
        #                  that STALLs everything SHOULD get re-enumerated;
        #   CRC/OVERFLOW - persistent garbage also wants the reset.
        # Registered (m.d.usb): cross-module-fanout lesson from round five;
        # one cycle of lag is nothing against a 600M-cycle budget.
        # .as_value() comparison: the production SIE's `response` is an
        # EnumView of guh's enum class (see ProductionElaborationTest).
        resp_live = Signal()
        resp_v = enum.ctrl.status.response.as_value()
        m.d.usb += resp_live.eq(
            enum.ctrl.status.idle
            & ((resp_v == TransferResponse.ACK.value)
               | (resp_v == TransferResponse.NAK.value)
               | (resp_v == TransferResponse.NYET.value)))
        with m.If(resp_live):
            m.d.usb += watchdog.eq(0)

        m.d.comb += [
            self.status.connected.eq(enum.status.enumerated),
            self.status.ready.eq(~self.status.busy),
            self.status.busy.eq(1),
            self.status.block_size.eq(block_size),
            self.status.block_count.eq(block_count),
        ]

        # Command setup helpers
        scsi_cmd = SCSIBulkHost.Command(scsi.cmd)
        cdb6 = CDB6(scsi_cmd.cdb.cdb6)
        cdb10 = CDB10(scsi_cmd.cdb.cdb10)

        with m.FSM(domain="usb"):

            with m.State("WAIT-ENUMERATION"):
                with m.If(scsi.status.idle):
                    m.d.usb += [watchdog.eq(0), init_retry.eq(0)]
                    m.next = "TEST-UNIT-READY"

            with m.State("TEST-UNIT-READY"):
                m.d.comb += [
                    cdb6.opcode.eq(SCSIOpCode.TEST_UNIT_READY),
                    scsi_cmd.data_len.eq(0),
                    scsi_cmd.stream_data.eq(0),
                    scsi_cmd.start.eq(1),
                ]
                m.next = "TEST-UNIT-READY-WAIT"

            with m.State("TEST-UNIT-READY-WAIT"):
                m.d.comb += cdb6.opcode.eq(SCSIOpCode.TEST_UNIT_READY)
                with m.If(scsi.status.done):
                    with m.If(~scsi.status.error & ~scsi.status.rejected):
                        m.d.usb += [watchdog.eq(0), init_retry.eq(0)]
                        with m.If(post_recovery):
                            m.d.usb += post_recovery.eq(0)
                            m.next = "READY"
                        with m.Else():
                            m.next = "READ-CAPACITY"
                    with m.Else():
                        m.d.usb += init_retry.eq(init_retry + 1)
                        with m.If(init_retry >= self._INIT_RETRY_MAX):
                            m.d.usb += post_recovery.eq(0)
                            m.next = "WAIT-ENUMERATION"
                        with m.Else():
                            m.next = "TEST-UNIT-READY"

            with m.State("READ-CAPACITY"):
                m.d.comb += [
                    cdb10.opcode.eq(SCSIOpCode.READ_CAPACITY_10),
                    scsi_cmd.data_len.eq(READ_CAPACITY_SIZE_BYTES),
                    scsi_cmd.stream_data.eq(0),
                    scsi_cmd.start.eq(1),
                ]
                m.next = "READ-CAPACITY-WAIT"

            with m.State("READ-CAPACITY-WAIT"):
                m.d.comb += [
                    cdb10.opcode.eq(SCSIOpCode.READ_CAPACITY_10),
                    scsi_cmd.data_len.eq(READ_CAPACITY_SIZE_BYTES),
                ]
                with m.If(scsi.status.done):
                    with m.If(~scsi.status.error & ~scsi.status.rejected):
                        m.d.usb += watchdog.eq(0)
                        # big-endian (scsi) to little-endian (amaranth)
                        last_lba_le = Cat(
                            scsi.captured.last_lba_be[24:32], scsi.captured.last_lba_be[16:24],
                            scsi.captured.last_lba_be[8:16], scsi.captured.last_lba_be[0:8])
                        blk_size_le = Cat(
                            scsi.captured.block_size_be[24:32], scsi.captured.block_size_be[16:24],
                            scsi.captured.block_size_be[8:16], scsi.captured.block_size_be[0:8])
                        m.d.usb += [
                            block_count.eq(last_lba_le + 1),
                            block_size.eq(blk_size_le[0:16]),
                        ]
                        m.next = "READY"
                    with m.Else():
                        m.next = "READ-CAPACITY"

            with m.State("READY"):
                m.d.comb += self.status.busy.eq(0)
                with m.If(self.cmd.start):
                    m.d.usb += [
                        current_lba.eq(self.cmd.lba),
                        is_write.eq(self.cmd.write),
                        sense_valid_r.eq(0),
                    ]
                    with m.If(self.cmd.write):
                        m.next = "WRITE"
                    with m.Else():
                        m.next = "READ"

            with m.State("READ"):
                m.d.comb += [
                    cdb10.opcode.eq(SCSIOpCode.READ_10),
                    # little-endian (amaranth) to big-endian (scsi)
                    cdb10.lba_be.eq(Cat(
                        current_lba[24:32], current_lba[16:24],
                        current_lba[8:16], current_lba[0:8])),
                    cdb10.xfer_len_be.eq(Cat(
                        Const(self._BLOCKS_PER_READ >> 8, 8),
                        Const(self._BLOCKS_PER_READ & 0xFF, 8))),
                    scsi_cmd.data_len.eq(block_size * self._BLOCKS_PER_READ),
                    scsi_cmd.stream_data.eq(1),
                    scsi_cmd.data_dir.eq(0),   # IN (device->host); explicit
                    scsi_cmd.start.eq(1),
                ]
                m.next = "READ-WAIT"

            with m.State("READ-WAIT"):
                m.d.comb += [
                    cdb10.opcode.eq(SCSIOpCode.READ_10),
                    cdb10.lba_be.eq(Cat(
                        current_lba[24:32], current_lba[16:24],
                        current_lba[8:16], current_lba[0:8])),
                    cdb10.xfer_len_be.eq(Cat(
                        Const(self._BLOCKS_PER_READ >> 8, 8),
                        Const(self._BLOCKS_PER_READ & 0xFF, 8))),
                    scsi_cmd.data_len.eq(block_size * self._BLOCKS_PER_READ),
                    scsi_cmd.stream_data.eq(1),
                    scsi_cmd.data_dir.eq(0),
                ]
                with m.If(scsi.status.done):
                    with m.If(~scsi.status.error & ~scsi.status.rejected):
                        m.d.usb += watchdog.eq(0)
                    m.d.comb += [
                        self.resp.done.eq(1),
                        self.resp.error.eq(scsi.status.error | scsi.status.rejected),
                    ]
                    m.d.usb += [
                        xfer_error_r.eq(scsi.status.error),
                        xfer_rejected_r.eq(scsi.status.rejected),
                    ]
                    m.next = "READ-DONE"

            with m.State("READ-DONE"):
                # See xfer_error_r's comment: wait one cycle for scsi's own
                # diagnostic registers to settle before deciding.
                # BOT §5.3.4: phase error, an invalid CSW, or a CSW that
                # STALLed twice all mean the transport is desynced —
                # Reset Recovery is REQUIRED before any next CBW (the
                # old behavior sent REQUEST SENSE into the desync).
                need_rr = (scsi.csw_bad_o
                           | (~xfer_rejected_r
                              & (scsi.csw.bCSWStatus == CSWStatus.PHASE_ERROR))
                           | (xfer_rejected_r
                              & (scsi.reject_phase == 3)
                              & (scsi.reject_response
                                 == TransferResponse.STALL.value)))
                with m.If(need_rr):
                    m.next = "RECOVERY"
                with m.Else():
                    m.next = "READY"

            def write_cdb():
                return [
                    cdb10.opcode.eq(SCSIOpCode.WRITE_10),
                    # little-endian (amaranth) to big-endian (scsi)
                    cdb10.lba_be.eq(Cat(
                        current_lba[24:32], current_lba[16:24],
                        current_lba[8:16], current_lba[0:8])),
                    cdb10.xfer_len_be.eq(Cat(
                        Const(self._BLOCKS_PER_READ >> 8, 8),
                        Const(self._BLOCKS_PER_READ & 0xFF, 8))),
                    scsi_cmd.data_len.eq(block_size * self._BLOCKS_PER_READ),
                    scsi_cmd.stream_data.eq(0),   # payload comes via tx_data
                    scsi_cmd.data_dir.eq(1),      # OUT (host->device)
                ]

            with m.State("WRITE"):
                m.d.comb += write_cdb() + [scsi_cmd.start.eq(1)]
                m.next = "WRITE-WAIT"

            with m.State("WRITE-WAIT"):
                m.d.comb += write_cdb()
                with m.If(scsi.status.done):
                    with m.If(~scsi.status.error & ~scsi.status.rejected):
                        m.d.usb += watchdog.eq(0)
                    m.d.comb += [
                        self.resp.done.eq(1),
                        self.resp.error.eq(scsi.status.error | scsi.status.rejected),
                    ]
                    m.d.usb += [
                        xfer_error_r.eq(scsi.status.error),
                        xfer_rejected_r.eq(scsi.status.rejected),
                    ]
                    m.next = "WRITE-DONE"

            with m.State("WRITE-DONE"):
                # See xfer_error_r's comment: wait one cycle for scsi's own
                # diagnostic registers to settle before deciding.
                # BOT §5.3.4: phase error, an invalid CSW, or a CSW that
                # STALLed twice all mean the transport is desynced —
                # Reset Recovery is REQUIRED before any next CBW (the
                # old behavior sent REQUEST SENSE into the desync).
                need_rr = (scsi.csw_bad_o
                           | (~xfer_rejected_r
                              & (scsi.csw.bCSWStatus == CSWStatus.PHASE_ERROR))
                           | (xfer_rejected_r
                              & (scsi.reject_phase == 3)
                              & (scsi.reject_response
                                 == TransferResponse.STALL.value)))
                with m.If(need_rr):
                    m.next = "RECOVERY"
                # A CSW FAILED (CHECK CONDITION) leaves pending sense data
                # on the drive; drain it with an auto REQUEST SENSE (and
                # capture key/ASC/ASCQ for firmware diagnostics). Only for
                # a clean CSW failure — after a rejected (bus-level)
                # exchange the transport may be desynced and another
                # command would just wedge again.
                with m.Elif(xfer_error_r & ~xfer_rejected_r):
                    m.next = "SENSE"
                with m.Else():
                    m.next = "READY"

            def sense_cdb():
                return [
                    cdb6.opcode.eq(SCSIOpCode.REQUEST_SENSE),
                    cdb6["_misc"].eq(18 << 24),   # allocation length (byte 4)
                    scsi_cmd.data_len.eq(18),
                    scsi_cmd.stream_data.eq(0),
                    scsi_cmd.data_dir.eq(0),
                ]

            with m.State("SENSE"):
                m.d.comb += sense_cdb() + [scsi_cmd.start.eq(1)]
                m.next = "SENSE-WAIT"

            with m.State("SENSE-WAIT"):
                m.d.comb += sense_cdb()
                with m.If(scsi.status.done):
                    with m.If(~scsi.status.error & ~scsi.status.rejected):
                        m.d.usb += [
                            watchdog.eq(0),
                            sense_r.eq(scsi.sense),
                            sense_valid_r.eq(1),
                        ]
                    m.next = "READY"

            with m.State("RECOVERY"):
                # scsi is back in IDLE on the cycle after status.done.
                m.d.comb += scsi.rr_start.eq(1)
                m.next = "RECOVERY-WAIT"

            with m.State("RECOVERY-WAIT"):
                with m.If(scsi.rr_done):
                    m.d.usb += [
                        watchdog.eq(0), init_retry.eq(0), post_recovery.eq(1),
                    ]
                    m.next = "TEST-UNIT-READY"   # revalidate before READY
                with m.Elif(scsi.status.done):
                    # Recovery's own control transfers failed: the control
                    # pipe is broken too — only re-enumeration can help.
                    m.next = "WAIT-ENUMERATION"

        return ResetInserter({"usb": watchdog_expired})(m)
