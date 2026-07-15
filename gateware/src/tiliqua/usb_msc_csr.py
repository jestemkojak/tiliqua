# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""Shared CSR peripheral wrapping guh USBMSCHost.

Hoisted verbatim from `top/sid_player_sw/top.py` so it can be reused by other
bitstreams (e.g. `top/mbsid`) that need to share a USB-C port between
USB-MIDI-host and mass-storage. `with_mode=False` (the default) reproduces
sid_player_sw's original CSR map and elaboration byte-for-byte; `with_mode=True`
adds one opt-in register (`mode` at offset 0x1C) plus a `mode_o` port used to
mux ownership of the underlying USB PHY.
"""

from amaranth import *
from amaranth.lib import data, stream, wiring
from amaranth.lib.fifo import SyncFIFOBuffered
from amaranth.lib.wiring import In, Out
from amaranth import ResetInserter

from luna.gateware.stream.future import Packet

from amaranth_soc import csr


USB_STATUS_LAYOUT = data.StructLayout({
    "connected": 1, "ready": 1, "busy": 1,
    "block_size": 16, "block_count": 32})
USB_RESP_LAYOUT = data.StructLayout({"done": 1, "error": 1})


class USBMSCPeripheral(wiring.Component):
    """CSR wrapper around guh USBMSCHost: status, LBA/start command, packed
    32-bit read-data FIFO drained by firmware."""

    class Status(csr.Register, access="r"):
        connected:   csr.Field(csr.action.R, unsigned(1))
        ready:       csr.Field(csr.action.R, unsigned(1))
        busy:        csr.Field(csr.action.R, unsigned(1))
        rx_avail:    csr.Field(csr.action.R, unsigned(1))
        # Negotiated link speed (guh USBHostSpeed = LUNA xcvr_select
        # encoding: 0=HIGH, 1=FULL, 2=LOW, 3=UNKNOWN/no device — NOT
        # 0=FULL/1=HIGH as previously mis-documented here, an inversion
        # that sent the 2026-07-15 M6b debug down a "link is FS" path
        # while the NYETing drive was actually at High Speed);
        # wired only when with_write (diagnostics), else reads 0.
        speed:       csr.Field(csr.action.R, unsigned(2))

    class BlockSize(csr.Register, access="r"):
        value: csr.Field(csr.action.R, unsigned(16))

    class BlockCount(csr.Register, access="r"):
        value: csr.Field(csr.action.R, unsigned(32))

    class Lba(csr.Register, access="w"):
        value: csr.Field(csr.action.W, unsigned(32))

    class Start(csr.Register, access="w"):
        strobe: csr.Field(csr.action.W, unsigned(1))

    class RxData(csr.Register, access="r"):
        word: csr.Field(csr.action.R, unsigned(32))

    class Resp(csr.Register, access="r"):
        error: csr.Field(csr.action.R, unsigned(1))

    class Mode(csr.Register, access="rw"):
        """USB port owner: 0 = USB-MIDI host engine (default), 1 = MSC (storage).
        Firmware mirrors the menu's `USB Mode` row here every redraw."""
        storage: csr.Field(csr.action.RW, unsigned(1))

    class TxData(csr.Register, access="w"):
        """One little-endian 32-bit word of write-payload. Strobe start_write
        FIRST, then push exactly 128 words (512 B); the engine is started
        automatically once the 128th word is banked."""
        word: csr.Field(csr.action.W, unsigned(32))

    class StartWrite(csr.Register, access="w"):
        """Arms a block write: flushes any leftover TX words, clears the
        sticky resp bits, and defers the actual engine start until the TX
        FIFO holds a full 128-word payload. The engine therefore never
        issues a WRITE(10) CBW without its complete data phase banked —
        a payload-less CBW leaves the device hanging mid-command and can
        desync the bulk-only transport (2026-07-14 drive-corruption
        incident, M6_USB_STORAGE.md)."""
        strobe: csr.Field(csr.action.W, unsigned(1))

    class RespW(csr.Register, access="r"):
        error: csr.Field(csr.action.R, unsigned(1))
        done:  csr.Field(csr.action.R, unsigned(1))

    class CswResidue(csr.Register, access="r"):
        """dCSWDataResidue of the last CSW (diagnostics: >0 on a failed write
        means the device did not accept the whole data phase)."""
        value: csr.Field(csr.action.R, unsigned(32))

    class CswStatus(csr.Register, access="r"):
        """bCSWStatus of the last CSW (0=passed, 1=failed, 2=phase error)."""
        value: csr.Field(csr.action.R, unsigned(8))

    class RejectInfo(csr.Register, access="r"):
        """Last rejected/STALLed transfer: SIE response (3=STALL, 4=TIMEOUT,
        5=CRC_ERROR) and exchange phase (1=CBW, 2=DATA-TX, 3=CSW, 4=DATA-RX,
        5=CTRL i.e. the clear-halt recovery itself). All fields in this
        register are LATCHED HERE in the peripheral (sync domain), outside
        the engine's 10 s watchdog reset — the 2026-07-15 64GB-stick hang
        came back rej=0/0/0 because the watchdog reset that ended the wedge
        also zeroed the engine-side diagnostics. Cleared by a start or
        start_write strobe."""
        response: csr.Field(csr.action.R, unsigned(3))
        phase:    csr.Field(csr.action.R, unsigned(3))
        # DATA-TX rejects only: 32-byte units already ACKed when it failed
        # (0 = the very first data packet never got a handshake).
        txdone:   csr.Field(csr.action.R, unsigned(4))
        # NYET handshakes seen during the command (saturating) — >0 means the
        # drive used HS flow control; relevant to the skipped PING protocol.
        nyets:    csr.Field(csr.action.R, unsigned(8))
        # Last nonzero live exchange phase seen (same encoding as `phase`).
        # On a wedged-then-watchdogged command this is the phase the engine
        # was STUCK in, even though `phase` (a reject latch) stayed 0.
        last_phase: csr.Field(csr.action.R, unsigned(3))

    class SenseInfo(csr.Register, access="r"):
        """Auto-REQUEST-SENSE result after a failed write. code[19:16]=sense
        key, code[15:8]=ASC, code[7:0]=ASCQ (e.g. key=7/ASC=0x27 = WRITE
        PROTECTED). valid=1 once captured; cleared on the next command."""
        code:  csr.Field(csr.action.R, unsigned(20))
        valid: csr.Field(csr.action.R, unsigned(1))

    # Ports — bus addr_width varies (5 bits normally, 6 once with_write adds
    # tx_data/start_write past 0x1F), so the whole signature is built
    # per-instance in __init__ rather than as class-level annotations
    # (amaranth.lib.wiring.Component disallows mixing the two).

    def __init__(self, *, word_fifo_depth=256, with_mode=False, with_write=False):
        self._with_mode = with_mode
        self._with_write = with_write
        self._word_fifo = SyncFIFOBuffered(width=32, depth=word_fifo_depth)
        if with_write:
            self._tx_fifo = SyncFIFOBuffered(width=32, depth=128)
        addr_width = 6 if with_write else 5
        regs = csr.Builder(addr_width=addr_width, data_width=8)
        self._status      = regs.add("status",      self.Status(),     offset=0x00)
        self._block_size  = regs.add("block_size",  self.BlockSize(),  offset=0x04)
        self._block_count = regs.add("block_count", self.BlockCount(), offset=0x08)
        self._lba         = regs.add("lba",         self.Lba(),        offset=0x0C)
        self._start       = regs.add("start",       self.Start(),      offset=0x10)
        self._rx_data_reg = regs.add("rx_data",     self.RxData(),     offset=0x14)
        if with_write:
            self._resp    = regs.add("resp",        self.RespW(),      offset=0x18)
        else:
            self._resp    = regs.add("resp",        self.Resp(),       offset=0x18)
        if with_mode:
            self._mode = regs.add("mode", self.Mode(), offset=0x1C)
        if with_write:
            self._tx_data     = regs.add("tx_data",     self.TxData(),     offset=0x20)
            self._start_write = regs.add("start_write", self.StartWrite(), offset=0x24)
            self._csw_residue = regs.add("csw_residue", self.CswResidue(), offset=0x28)
            self._csw_status  = regs.add("csw_status",  self.CswStatus(),  offset=0x2C)
            self._reject_info = regs.add("reject_info", self.RejectInfo(), offset=0x30)
            self._sense_info  = regs.add("sense_info",  self.SenseInfo(),  offset=0x34)
        self._bridge = csr.Bridge(regs.as_memory_map())
        super().__init__({
            "bus":           In(csr.Signature(addr_width=addr_width, data_width=8)),
            "rx_data":       In(stream.Signature(Packet(unsigned(8)))),
            "lba_o":         Out(32),
            "start_o":       Out(1),
            "status_i":      In(USB_STATUS_LAYOUT),
            "resp_i":        In(USB_RESP_LAYOUT),
            "mode_o":        Out(1),   # with_mode only: 0=MIDI owns PHY, 1=MSC
            "tx_data_o":     Out(stream.Signature(unsigned(8))),  # with_write only
            "start_write_o": Out(1),                              # with_write only
            "csw_status_i":  In(8),    # with_write only: last CSW status byte
            "csw_residue_i": In(32),   # with_write only: last CSW data residue
            "reject_response_i": In(3),  # with_write only: last rejected xfer
            "reject_phase_i":    In(3),  # with_write only: where it rejected
            "reject_txdone_i":   In(4),  # with_write only: ACKed 32B units
            "nyet_count_i":      In(8),  # with_write only: NYETs this command
            "phase_i":           In(3),  # with_write only: live engine phase
            "sense_i":           In(20), # with_write only: key/ASC/ASCQ
            "sense_valid_i":     In(1),
            "speed_i":           In(2),  # with_write only: link speed
        })
        self.bus.memory_map = self._bridge.bus.memory_map

    def elaborate(self, platform):
        m = Module()
        m.submodules.bridge = self._bridge
        wiring.connect(m, wiring.flipped(self.bus), self._bridge.bus)

        # start_strobe: high for one cycle when firmware writes start.strobe=1.
        start_strobe = self._start.f.strobe.w_stb & self._start.f.strobe.w_data

        # Wrap the word FIFO with ResetInserter so start_strobe flushes it in
        # one cycle (idiomatic Amaranth sync-domain flush).
        m.submodules.word_fifo = wf = ResetInserter(start_strobe)(self._word_fifo)

        # Pack incoming bytes (little-endian) into 32-bit words.
        byte_ix = Signal(2)
        acc = Signal(32)
        m.d.comb += [self.rx_data.ready.eq(1), wf.w_en.eq(0)]
        with m.If(self.rx_data.valid & self.rx_data.ready):
            b = self.rx_data.payload.data
            with m.Switch(byte_ix):
                for i in range(4):
                    with m.Case(i):
                        m.d.sync += acc[i*8:(i+1)*8].eq(b)
            m.d.sync += byte_ix.eq(byte_ix + 1)
            with m.If(byte_ix == 3):
                m.d.comb += [wf.w_data.eq(Cat(acc[0:24], b)), wf.w_en.eq(1)]

        # Status / capacity readback.
        m.d.comb += [
            self._status.f.connected.r_data.eq(self.status_i.connected),
            self._status.f.ready.r_data.eq(self.status_i.ready),
            self._status.f.busy.r_data.eq(self.status_i.busy),
            self._status.f.rx_avail.r_data.eq(wf.r_level != 0),
            self._block_size.f.value.r_data.eq(self.status_i.block_size),
            self._block_count.f.value.r_data.eq(self.status_i.block_count),
        ]

        # Command: latch LBA, pulse start.
        with m.If(self._lba.f.value.w_stb):
            m.d.sync += self.lba_o.eq(self._lba.f.value.w_data)
        m.d.comb += self.start_o.eq(start_strobe)

        # Drain word FIFO on rx_data CSR read.
        m.d.comb += wf.r_en.eq(self._rx_data_reg.f.word.r_stb)
        with m.If(wf.r_level != 0):
            m.d.comb += self._rx_data_reg.f.word.r_data.eq(wf.r_data)
        with m.Else():
            m.d.comb += self._rx_data_reg.f.word.r_data.eq(0)

        # Sticky response latch (set on resp_i.done).
        # Reset on start_strobe so a new command never sees a stale error.
        resp_error_r = Signal()
        with m.If(self.resp_i.done):
            m.d.sync += resp_error_r.eq(self.resp_i.error)
        # start_strobe wins: placed AFTER the done block so a simultaneous
        # start+done clears rather than latches (new command takes priority).
        with m.If(start_strobe):
            m.d.sync += [byte_ix.eq(0), acc.eq(0), resp_error_r.eq(0)]
        m.d.comb += self._resp.f.error.r_data.eq(resp_error_r)

        if self._with_mode:
            m.d.comb += self.mode_o.eq(self._mode.f.storage.data)

        if self._with_write:
            start_write = (self._start_write.f.strobe.w_stb
                           & self._start_write.f.strobe.w_data)
            # Wrap the TX word FIFO with ResetInserter so start_write flushes
            # any leftover words from a prior (partial/failed) write before
            # the new one begins — symmetric to the RX word FIFO's
            # ResetInserter(start_strobe) above. Because this flush fires on
            # the strobe itself, the payload MUST be pushed AFTER the strobe
            # (strobe-then-fill): the original fill-then-strobe contract had
            # this same flush erase the just-loaded payload on the start
            # edge, so every write issued a payload-less WRITE(10) CBW —
            # root cause of the 2026-07-14 drive-corruption incident.
            m.submodules.tx_fifo = txf = ResetInserter(start_write)(self._tx_fifo)
            # start_write only ARMS the write; the engine start is deferred
            # until the full 512-byte payload is banked, so a WRITE(10) can
            # never be issued with missing data (even if firmware dies
            # mid-fill, no bus traffic happens at all). A read start strobe
            # disarms, so stray tx words can't launch a stale write later.
            write_armed = Signal()
            fire_write = Signal()
            m.d.comb += fire_write.eq(write_armed & (txf.r_level == 128))
            with m.If(start_write):
                m.d.sync += write_armed.eq(1)
            with m.Elif(fire_write | start_strobe):
                m.d.sync += write_armed.eq(0)
            m.d.comb += [
                txf.w_en.eq(self._tx_data.f.word.w_stb),
                txf.w_data.eq(self._tx_data.f.word.w_data),
                self.start_write_o.eq(fire_write),
            ]
            # word -> byte unpacker (mirror of the RX byte->word packer)
            tx_ix = Signal(2)
            m.d.comb += [
                self.tx_data_o.valid.eq(txf.r_rdy),
                self.tx_data_o.payload.eq(
                    txf.r_data.word_select(tx_ix, 8)),
                txf.r_en.eq(0),
            ]
            with m.If(self.tx_data_o.valid & self.tx_data_o.ready):
                m.d.sync += tx_ix.eq(tx_ix + 1)
                with m.If(tx_ix == 3):
                    m.d.comb += txf.r_en.eq(1)
            with m.If(start_write):
                m.d.sync += tx_ix.eq(0)
            # sticky done (with_write resp variant); extends the existing
            # resp_error_r clear term rather than duplicating the register.
            # Cleared by EITHER start (read) or start_write, per spec.
            resp_done_r = Signal()
            with m.If(self.resp_i.done):
                m.d.sync += resp_done_r.eq(1)
            with m.If(start_strobe | start_write):
                m.d.sync += [resp_done_r.eq(0), resp_error_r.eq(0)]
            m.d.comb += self._resp.f.done.r_data.eq(resp_done_r)
            # Diagnostic latches. The engine-side reject/sense/nyet registers
            # live inside the MSC engine's 10 s watchdog reset domain, so a
            # wedged command's evidence is zeroed by the very reset that ends
            # the wedge (observed on hardware 2026-07-15: a hung write came
            # back rej=0/0/0). Latch every diagnostic HERE, outside that
            # reset, updating only from nonzero/valid engine values and
            # clearing on the next command strobe.
            rej_resp_r   = Signal(3)
            rej_phase_r  = Signal(3)
            rej_txdone_r = Signal(4)
            nyets_r      = Signal(8)
            last_phase_r = Signal(3)
            sense_code_r  = Signal(20)
            sense_valid_r = Signal()
            # Change-to-nonzero detection, NOT level: the engine's own diag
            # registers persist until its next cmd.start, which for a write
            # fires only after firmware banks all 128 payload words — a
            # level-based latch would re-capture the PREVIOUS command's
            # values in that window, right after our clear-on-strobe. The
            # engine zeroes each of these per command, so every real event
            # arrives as a 0 -> nonzero (or value-stepping) change; the
            # watchdog reset arrives as a change TO zero, which is ignored —
            # that's the evidence-preservation this latch exists for.
            rej_in = Cat(self.reject_response_i, self.reject_phase_i,
                         self.reject_txdone_i)
            rej_prev = Signal.like(rej_in)
            m.d.sync += rej_prev.eq(rej_in)
            with m.If((rej_in != rej_prev) & (self.reject_phase_i != 0)):
                m.d.sync += [
                    rej_resp_r.eq(self.reject_response_i),
                    rej_phase_r.eq(self.reject_phase_i),
                    rej_txdone_r.eq(self.reject_txdone_i),
                ]
            nyet_prev = Signal(8)
            m.d.sync += nyet_prev.eq(self.nyet_count_i)
            with m.If((self.nyet_count_i != nyet_prev)
                      & (self.nyet_count_i != 0)):
                m.d.sync += nyets_r.eq(self.nyet_count_i)
            phase_prev = Signal(3)
            m.d.sync += phase_prev.eq(self.phase_i)
            with m.If((self.phase_i != phase_prev) & (self.phase_i != 0)):
                m.d.sync += last_phase_r.eq(self.phase_i)
            sense_in = Cat(self.sense_i, self.sense_valid_i)
            sense_prev = Signal.like(sense_in)
            m.d.sync += sense_prev.eq(sense_in)
            with m.If((sense_in != sense_prev) & self.sense_valid_i):
                m.d.sync += [
                    sense_code_r.eq(self.sense_i),
                    sense_valid_r.eq(1),
                ]
            with m.If(start_strobe | start_write):
                m.d.sync += [
                    rej_resp_r.eq(0), rej_phase_r.eq(0), rej_txdone_r.eq(0),
                    nyets_r.eq(0), last_phase_r.eq(0),
                    sense_code_r.eq(0), sense_valid_r.eq(0),
                ]
            m.d.comb += [
                self._csw_residue.f.value.r_data.eq(self.csw_residue_i),
                self._csw_status.f.value.r_data.eq(self.csw_status_i),
                self._reject_info.f.response.r_data.eq(rej_resp_r),
                self._reject_info.f.phase.r_data.eq(rej_phase_r),
                self._reject_info.f.txdone.r_data.eq(rej_txdone_r),
                self._reject_info.f.nyets.r_data.eq(nyets_r),
                self._reject_info.f.last_phase.r_data.eq(last_phase_r),
                self._sense_info.f.code.r_data.eq(sense_code_r),
                self._sense_info.f.valid.r_data.eq(sense_valid_r),
                self._status.f.speed.r_data.eq(self.speed_i),
            ]

        return m
