# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""
End-to-end integration sim for the M6b USB write path: USBMSCPeripheral (CSR)
+ USBMSCHost/SCSIBulkHost (vendored guh engine) + the exact command glue from
`top/sid/top.py`, driven with the firmware's exact CSR sequence against a
scripted "drive" (stub SIE played by the testbench).

Written during the 2026-07-15 hardware bring-up: the per-layer tests
(test_usb_msc_csr.py, test_guh_msc_write.py) each passed while the assembled
stack failed on hardware — nothing sim-tested the layers *together* (the
write_pending glue in top.py had zero sim coverage). These tests close that
gap and encode the realistic-drive behaviors found on hardware: NAKed CBWs,
NAKed CSWs, failing CSWs, and a STALLed CSW.
"""

import unittest

from amaranth import *
from amaranth.lib import wiring
from amaranth.sim import Simulator

# Vendored SIE types, NOT guh's — the stub harness's signatures use these,
# and enum members don't compare/ctx.set across the two otherwise-identical
# classes. Vendored TransferResponse adds NYET=7 (the 2026-07-15 HS
# write-failure fix; see vendor/guh_msc/sie.py).
from vendor.guh_msc.sie import TransferType, TransferResponse, DataPID

from tiliqua.usb_msc_csr import USBMSCPeripheral
from vendor.guh_msc.msc import USBMSCHost, SCSIBulkHost, CBW_SIZE_BYTES

from test_guh_msc_write import StubEnumerator

BLOCK = 512


def _csw(status=0x00, residue=0, tag=1):
    b = [0x55, 0x53, 0x42, 0x53]                     # dCSWSignature
    b += list(int(tag).to_bytes(4, "little"))        # dCSWTag (echo the CBW's)
    b += list(residue.to_bytes(4, "little"))         # dCSWDataResidue
    b += [status]                                    # bCSWStatus
    return b


def _capacity():
    # READ CAPACITY(10): last LBA + block size, both big-endian on the wire.
    return list((999).to_bytes(4, "big")) + list((512).to_bytes(4, "big"))


class _Drive:
    """Scripted device behind the stub SIE, played from a usb-domain
    testbench. Collects host OUT bytes (via ctrl.txs, which the engine loads
    into the SIE FIFO), answers transactions per script."""

    def __init__(self, stub):
        self.stub = stub
        self.out_log = []   # list of (bytes, response) per OUT transaction
        self.last_tag = 0

    async def tick(self, ctx, capture=None):
        c = self.stub.ctrl
        ctx.set(c.txs.ready, 1)
        if ctx.get(c.txs.valid) and ctx.get(c.txs.ready):
            if capture is not None:
                capture.append(ctx.get(c.txs.payload))
        await ctx.tick("usb")

    async def tick_noconsume(self, ctx):
        """Tick with txs.ready deasserted: used for a transaction's commit/
        response ticks so they can't eat the FIRST byte of whatever the
        engine starts loading next (e.g. the CLEAR_FEATURE setup packet
        pushed immediately after a STALL response is read)."""
        ctx.set(self.stub.ctrl.txs.ready, 0)
        await ctx.tick("usb")

    async def wait_start(self, ctx, capture=None, max_ticks=300000):
        c = self.stub.ctrl
        ctx.set(c.status.idle, 1)
        for _ in range(max_ticks):
            if ctx.get(c.xfer.start):
                return (ctx.get(c.xfer.type), ctx.get(c.xfer.data_pid))
            await self.tick(ctx, capture)
        raise AssertionError("no transfer started within bound")

    async def do_out(self, ctx, response, capture=None):
        """Serve one OUT transaction: bytes were already captured while
        waiting; commit start, then answer. Records the dCBWTag of any CBW
        seen so the test can echo it in the CSW (the engine validates tag
        echo per BOT §6.3.1 since round eight)."""
        buf = capture if capture is not None else []
        xtype, pid = await self.wait_start(ctx, buf)
        assert xtype == TransferType.OUT, f"expected OUT, got {xtype}"
        await self.tick_noconsume(ctx)              # commit -> engine *-WAIT
        ctx.set(self.stub.ctrl.status.response, response)
        await self.tick_noconsume(ctx)              # engine reads response
        if len(buf) >= 8 and bytes(buf[:4]) == b"USBC":
            self.last_tag = int.from_bytes(bytes(buf[4:8]), "little")
        return pid

    async def do_setup(self, ctx, capture=None,
                       response=TransferResponse.ACK):
        """Serve one SETUP transaction on ep0 (e.g. the engine's
        CLEAR_FEATURE(ENDPOINT_HALT) recovery); 8 bytes were already
        captured while waiting."""
        xtype, pid = await self.wait_start(ctx, capture)
        assert xtype == TransferType.SETUP, f"expected SETUP, got {xtype}"
        await self.tick_noconsume(ctx)              # commit -> engine wait
        ctx.set(self.stub.ctrl.status.response, response)
        await self.tick_noconsume(ctx)              # engine reads response
        return pid

    async def do_in(self, ctx, payload, response):
        """Serve one IN transaction with `payload` then `response`."""
        c = self.stub.ctrl
        xtype, pid = await self.wait_start(ctx)
        assert xtype == TransferType.IN, f"expected IN, got {xtype}"
        await self.tick(ctx)                        # commit
        ctx.set(c.status.idle, 0)
        for b in payload:
            ctx.set(c.rxs.valid, 1)
            ctx.set(c.rxs.payload, b)
            while not ctx.get(c.rxs.ready):
                await self.tick(ctx)
            await self.tick(ctx)
        ctx.set(c.rxs.valid, 0)
        ctx.set(c.status.response, response)
        ctx.set(c.status.idle, 1)
        await self.tick_noconsume(ctx)
        ctx.set(c.status.idle, 0)
        await self.tick_noconsume(ctx)
        ctx.set(c.status.idle, 1)
        return pid

    async def init_to_ready(self, ctx):
        """Play the engine's init: TEST UNIT READY, READ CAPACITY."""
        # TEST UNIT READY: CBW out, no data, CSW in.
        cbw = []
        await self.do_out(ctx, TransferResponse.ACK, cbw)
        assert len(cbw) == CBW_SIZE_BYTES
        await self.do_in(ctx, _csw(tag=self.last_tag), TransferResponse.ACK)
        # READ CAPACITY: CBW out, 8 bytes in, CSW in.
        cbw = []
        await self.do_out(ctx, TransferResponse.ACK, cbw)
        await self.do_in(ctx, _capacity(), TransferResponse.ACK)
        await self.do_in(ctx, _csw(tag=self.last_tag), TransferResponse.ACK)


def _build(watchdog_cycles=None):
    """Assemble peripheral + engine + the top.py command glue, verbatim.
    `watchdog_cycles` shrinks the engine watchdog for tests that must
    provoke (or must NOT provoke) it inside a simulable horizon; None
    keeps the production 10 s value."""
    periph = USBMSCPeripheral(with_mode=True, with_write=True)
    if watchdog_cycles is None:
        host = USBMSCHost(bus=None)
    else:
        class _ShortWatchdogHost(USBMSCHost):
            _WATCHDOG_CYCLES = watchdog_cycles
        host = _ShortWatchdogHost(bus=None)
    host.scsi = SCSIBulkHost(enumerator=StubEnumerator())  # inject stub SIE

    m = Module()
    m.submodules.periph = periph
    m.submodules.msc = host

    # --- glue copied from top/sid/top.py (keep in sync!) --------------------
    wiring.connect(m, host.rx_data, periph.rx_data)
    wiring.connect(m, periph.tx_data_o, host.tx_data)
    write_pending = Signal()
    with m.If(periph.start_write_o):
        m.d.sync += write_pending.eq(1)
    with m.If(periph.start_o | periph.start_flush_o):
        m.d.sync += write_pending.eq(0)
    flush_pending = Signal()
    with m.If(periph.start_flush_o):
        m.d.sync += flush_pending.eq(1)
    with m.If(periph.start_o | periph.start_write_o):
        m.d.sync += flush_pending.eq(0)
    m.d.comb += [
        periph.status_i.connected.eq(host.status.connected),
        periph.status_i.ready.eq(host.status.ready),
        periph.status_i.busy.eq(host.status.busy),
        periph.status_i.block_size.eq(host.status.block_size),
        periph.status_i.block_count.eq(host.status.block_count),
        host.cmd.lba.eq(periph.lba_o),
        host.cmd.start.eq(periph.start_o
                           | periph.start_write_o
                           | periph.start_flush_o),
        host.cmd.write.eq(periph.start_write_o |
                          (write_pending & ~periph.start_o
                           & ~periph.start_flush_o)),
        host.cmd.flush.eq(periph.start_flush_o |
                          (flush_pending & ~periph.start_o
                           & ~periph.start_write_o)),
        periph.resp_i.done.eq(host.resp.done),
        periph.resp_i.error.eq(host.resp.error),
        periph.csw_status_i.eq(host.csw.bCSWStatus),
        periph.csw_residue_i.eq(host.csw.dCSWDataResidue),
        periph.reject_response_i.eq(host.reject_response),
        periph.reject_phase_i.eq(host.reject_phase),
        periph.sense_i.eq(host.sense_o),
        periph.sense_valid_i.eq(host.sense_valid_o),
        periph.reject_txdone_i.eq(host.reject_txdone),
        periph.nyet_count_i.eq(host.nyets),
        periph.phase_i.eq(host.phase_o),
        periph.csw_bad_i.eq(host.csw_bad_o),
        periph.engine_rx_bytes_i.eq(host.rx_bytes_o),
        periph.engine_stream_mode_i.eq(host.stream_mode_o),
        periph.engine_data_len_512_i.eq(host.data_len_512_o),
        periph.speed_i.eq(host.speed_o),
    ]
    # ------------------------------------------------------------------------
    return m, periph, host


class _Fw:
    """usb_msc.rs's CSR access sequences, byte-for-byte."""

    def __init__(self, periph):
        self.p = periph

    async def csr_write(self, ctx, offset, value):
        ctx.set(self.p.bus.addr, offset)
        ctx.set(self.p.bus.w_data, value)
        ctx.set(self.p.bus.w_stb, 1)
        await ctx.tick()
        ctx.set(self.p.bus.w_stb, 0)
        await ctx.tick()

    async def csr_read(self, ctx, offset):
        ctx.set(self.p.bus.addr, offset)
        ctx.set(self.p.bus.r_stb, 1)
        await ctx.tick()
        ctx.set(self.p.bus.r_stb, 0)
        return ctx.get(self.p.bus.r_data)

    async def csr_write32(self, ctx, offset, value):
        for i in range(4):
            await self.csr_write(ctx, offset + i, (value >> (8 * i)) & 0xFF)

    async def csr_read32(self, ctx, offset):
        v = 0
        for i in range(4):
            v |= (await self.csr_read(ctx, offset + i)) << (8 * i)
        return v

    async def wait_ready(self, ctx, max_polls=200000):
        for _ in range(max_polls):
            st = await self.csr_read(ctx, 0x00)     # status
            if st & 0b10:                            # ready bit
                bs = await self.csr_read32(ctx, 0x04)
                if (bs & 0xFFFF) == 512:
                    return
        raise AssertionError("device never became ready")

    async def write_block(self, ctx, lba, data512, max_polls=400000):
        """Mirror of fw write_block: lba -> arm -> 128 words -> poll done."""
        await self.csr_write32(ctx, 0x0C, lba)       # lba
        await self.csr_write(ctx, 0x24, 1)           # start_write (arm)
        for i in range(128):
            w = int.from_bytes(bytes(data512[i*4:i*4+4]), "little")
            await self.csr_write32(ctx, 0x20, w)
        for _ in range(max_polls):
            r = await self.csr_read(ctx, 0x18)       # resp: bit0 err, bit1 done
            if r & 0b10:
                return (r & 0b01) != 0               # error flag
        raise AssertionError("write never completed (resp.done stayed 0)")


class ProductionElaborationTest(unittest.TestCase):

    def test_production_engine_elaborates(self):
        """Elaborate USBMSCHost through its REAL constructor path: stock guh
        USBHostEnumerator + vendored SIE swapped in (exactly what top.py
        builds). The sim tests above structurally CANNOT catch cross-package
        enum mismatches — their stub's interface is shaped by the vendored
        enum classes, so vendored-vs-upstream EnumViews never meet. A plain
        `==` between an upstream-shaped EnumView and a vendored enum member
        raises TypeError only here (found 2026-07-15: sim suite green, full
        build crashed at elaboration; use `.as_value() == Member.value`)."""
        from amaranth.back import rtlil
        rtlil.convert(USBMSCHost(bus=None))

    def test_fullspeed_only_plumbs_to_reset_controller(self):
        """Round eight: mbsid forces the MSC engine to Full Speed — at FS
        the 64-byte TX chunking is exactly wMaxPacketSize (legal) and the
        PING protocol does not exist, removing both critical HS violations
        (M6_USB_STORAGE.md round eight). This asserts the flag actually
        reaches the reset controller through the real constructor path,
        and that the FS engine still elaborates."""
        from amaranth.back import rtlil
        host = USBMSCHost(bus=None, fullspeed_only=True)
        self.assertTrue(host.scsi.enumerator.sie.reset_ctrl.fullspeed_only)
        rtlil.convert(host)


class UsbMscIntegrationTests(unittest.TestCase):

    def _run(self, fw_tb, drive_tb, deadline_us=40000, watchdog_cycles=None):
        m, periph, host = _build(watchdog_cycles=watchdog_cycles)
        sim = Simulator(m)
        sim.add_clock(1e-6, domain="sync")
        sim.add_clock(1e-6, domain="usb")
        sim.add_testbench(fw_tb(periph, host))
        sim.add_testbench(drive_tb(host))
        sim.run_until(deadline_us * 1e-6)

    def test_write_succeeds_end_to_end_with_naks(self):
        """Firmware-exact write against a drive that NAKs the CBW twice and
        the CSW once: must complete with error=0 and the drive must receive
        the exact 512 payload bytes. (The assembled-stack regression that
        would have caught the original TX-FIFO-flush incident.)"""
        payload = [(i * 7 + 3) & 0xFF for i in range(BLOCK)]
        result = {}

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                result["error"] = await fw.write_block(ctx, 0x1234, payload)
                result["csw_stat"] = await fw.csr_read(ctx, 0x2C)
            return tb

        def drive_tb(host):
            async def tb(ctx):
                drive = _Drive(host.scsi.enumerator)
                await drive.init_to_ready(ctx)
                cbw1, cbw2, cbw3 = [], [], []
                await drive.do_out(ctx, TransferResponse.NAK, cbw1)   # CBW: NAK
                await drive.do_out(ctx, TransferResponse.NAK, cbw2)   # retry: NAK
                p = await drive.do_out(ctx, TransferResponse.ACK, cbw3)
                result["cbw_retries_identical"] = (cbw1 == cbw2 == cbw3)
                data = []
                for _ in range(8):
                    await drive.do_out(ctx, TransferResponse.ACK, data)
                result["data"] = data
                await drive.do_in(ctx, [], TransferResponse.NAK)      # CSW: NAK
                await drive.do_in(ctx, _csw(0x00, tag=drive.last_tag), TransferResponse.ACK)
            return tb

        self._run(fw_tb, drive_tb)
        self.assertIn("error", result, "firmware never saw resp.done")
        self.assertFalse(result["error"])
        self.assertTrue(result["cbw_retries_identical"])
        self.assertEqual(result["data"], payload)
        self.assertEqual(result["csw_stat"], 0)

    def test_write_succeeds_when_drive_nyets_data_phase(self):
        """The 2026-07-15 real-hardware failure mode, end to end: a High-Speed
        drive answers every write-data OUT with NYET (data accepted, endpoint
        busy — routine flash-drive flow control) and the CBW with a NYET too.
        The write must complete with error=0 and the drive must receive the
        exact payload. Against the pre-fix engine this rejected the command
        with reject_response=TIMEOUT (rej=4/2/0 on hardware)."""
        payload = [(i * 11 + 5) & 0xFF for i in range(BLOCK)]
        result = {}

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                result["error"] = await fw.write_block(ctx, 0x2000, payload)
                result["csw_stat"] = await fw.csr_read(ctx, 0x2C)
                ri = await fw.csr_read32(ctx, 0x30)
                result["nyets"] = (ri >> 10) & 0xFF
            return tb

        def drive_tb(host):
            async def tb(ctx):
                drive = _Drive(host.scsi.enumerator)
                await drive.init_to_ready(ctx)
                await drive.do_out(ctx, TransferResponse.NYET, [])    # CBW
                data = []
                for _ in range(8):                                    # data
                    await drive.do_out(ctx, TransferResponse.NYET, data)
                result["data"] = data
                await drive.do_in(ctx, _csw(0x00, tag=drive.last_tag), TransferResponse.ACK)
            return tb

        self._run(fw_tb, drive_tb)
        self.assertIn("error", result, "firmware never saw resp.done")
        self.assertFalse(result["error"])
        self.assertEqual(result["data"], payload)
        self.assertEqual(result["csw_stat"], 0)
        # NYET counter diag: 1 (CBW) + 8 (data packets), latched CSR-side.
        self.assertEqual(result["nyets"], 9)

    def test_write_passed_csw_with_nonzero_residue_visible_to_firmware(self):
        """BOT: PASSED (status 0) + dCSWDataResidue != 0 on a write means the
        device did NOT accept the whole data phase. The residue is exposed
        live at CSR 0x28; firmware's write_block (fw/src/usb_msc.rs) treats
        this as WriteError — this test pins the CSR-visible contract."""
        result = {}

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                err = await fw.write_block(ctx, 100, [0x11] * BLOCK)
                residue = await fw.csr_read32(ctx, 0x28)
                result["err"] = err
                result["residue"] = residue
            return tb

        def drive_tb(host):
            async def tb(ctx):
                stub = host.scsi.enumerator
                drive = _Drive(stub)
                await drive.init_to_ready(ctx)
                cbw = []
                await drive.do_out(ctx, TransferResponse.ACK, cbw)
                for _ in range(8):
                    await drive.do_out(ctx, TransferResponse.ACK)
                await drive.do_in(
                    ctx, _csw(status=0x00, residue=64, tag=drive.last_tag),
                    TransferResponse.ACK)
            return tb

        self._run(fw_tb, drive_tb)
        self.assertFalse(result["err"])       # engine-level resp.error stays 0
        self.assertEqual(result["residue"], 64)  # firmware maps this to failure

    def test_read_immediately_after_successful_write(self):
        """Round six (2026-07-15): on hardware, the first device READ after
        the first successful WRITE failed deterministically (d_wrok=1 then
        d_rderr=2, identical counts across runs, drive healthy afterwards).
        This drives the firmware-exact write_block -> read_block sequence
        through the full glue to rule the gateware in or out: the read's CBW
        must go out as a READ(10) (not misrouted by a stale write_pending),
        and the returned block must reach the rx FIFO intact."""
        payload = [(i * 13 + 1) & 0xFF for i in range(BLOCK)]
        sector = [(i * 5 + 2) & 0xFF for i in range(BLOCK)]
        result = {}

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                result["werr"] = await fw.write_block(ctx, 0x40, payload)
                # firmware read_block: lba -> start -> drain 128 words
                await fw.csr_write32(ctx, 0x0C, 0x41)
                await fw.csr_write(ctx, 0x10, 1)          # start (read)
                data = []
                for _ in range(128):
                    for _ in range(200000):
                        st = await fw.csr_read(ctx, 0x00)
                        if st & 0b1000:                    # rx_avail
                            break
                        r = await fw.csr_read(ctx, 0x18)
                        if r & 0b01:
                            result["rerr"] = True
                            return
                    w = await fw.csr_read32(ctx, 0x14)
                    data += list(w.to_bytes(4, "little"))
                for _ in range(200000):
                    if result.get("read_csw_nak_seen"):
                        break
                    await ctx.tick("sync")
                else:
                    raise AssertionError("drive never NAKed the read CSW")
                result["read_path"] = await fw.csr_read32(ctx, 0x38)
                result["rerr"] = False
                result["rdata"] = data
            return tb

        def drive_tb(host):
            async def tb(ctx):
                drive = _Drive(host.scsi.enumerator)
                await drive.init_to_ready(ctx)
                # WRITE: CBW + 8 data OUTs + CSW
                await drive.do_out(ctx, TransferResponse.ACK, [])
                wdata = []
                for _ in range(8):
                    await drive.do_out(ctx, TransferResponse.ACK, wdata)
                result["wdata"] = wdata
                await drive.do_in(ctx, _csw(0x00, tag=drive.last_tag), TransferResponse.ACK)
                # READ: CBW must be a READ(10) opcode, then serve the block
                rcbw = []
                await drive.do_out(ctx, TransferResponse.ACK, rcbw)
                result["read_opcode"] = rcbw[15]
                for i in range(8):
                    await drive.do_in(ctx, sector[i*64:(i+1)*64],
                                      TransferResponse.ACK)
                await drive.do_in(ctx, [], TransferResponse.NAK)
                result["read_csw_nak_seen"] = True
                await drive.do_in(ctx, _csw(0x00, tag=drive.last_tag), TransferResponse.ACK)
            return tb

        self._run(fw_tb, drive_tb)
        self.assertFalse(result["werr"])
        self.assertEqual(result["wdata"], payload)
        self.assertEqual(result.get("read_opcode"), 0x28)   # READ(10)
        self.assertFalse(result.get("rerr"),
                         "read after write errored (glue bug)")
        self.assertEqual(result.get("rdata"), sector)
        read_path = result.get("read_path", 0)
        self.assertEqual(read_path & 0x3FF, 512)          # engine_bytes
        self.assertEqual((read_path >> 10) & 0x3FF, 512) # periph_bytes
        self.assertEqual((read_path >> 20) & 0xFF, 128)  # periph_words
        self.assertEqual((read_path >> 28) & 1, 1)       # stream_mode
        self.assertEqual((read_path >> 29) & 1, 1)       # data_len_512
        self.assertEqual(read_path >> 30, 0)

    def test_read_survives_nak_wait_longer_than_watchdog(self):
        """Round seven: a drive that busy-NAKs the data-IN phase for longer
        than the engine watchdog budget (the round-six post-write failure:
        FTL commit right after a WRITE) must NOT get the engine reset out
        from under it — NAK is a live handshake, the drive is present. With
        the old completion-fed watchdog this test fails: the engine resets
        mid-NAK-loop, re-runs init, and the read never completes."""
        payload = [(i * 3 + 9) & 0xFF for i in range(BLOCK)]
        sector = [(i * 17 + 4) & 0xFF for i in range(BLOCK)]
        result = {}
        WD = 2500  # usb cycles; the NAK loop below spans well past this
        NAKS = 1000  # empirically ~5 usb cycles/NAK round trip in this stub
                     # harness (measured via VCD), so >> WD/5=500 is required
                     # to guarantee the watchdog expires mid-loop; 250 (a
                     # naive "15-25 cycles/NAK" estimate) was not enough and
                     # let the read finish before the watchdog ever fired.

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                result["werr"] = await fw.write_block(ctx, 0x40, payload)
                await fw.csr_write32(ctx, 0x0C, 0x41)
                await fw.csr_write(ctx, 0x10, 1)          # start (read)
                data = []
                for _ in range(128):
                    for _ in range(400000):
                        st = await fw.csr_read(ctx, 0x00)
                        if st & 0b1000:                    # rx_avail
                            break
                        r = await fw.csr_read(ctx, 0x18)
                        if r & 0b01:
                            result["rerr"] = True
                            return
                    w = await fw.csr_read32(ctx, 0x14)
                    data += list(w.to_bytes(4, "little"))
                result["rerr"] = False
                result["rdata"] = data
            return tb

        def drive_tb(host):
            async def tb(ctx):
                drive = _Drive(host.scsi.enumerator)
                await drive.init_to_ready(ctx)
                # WRITE: CBW + 8 data OUTs + CSW (all clean)
                await drive.do_out(ctx, TransferResponse.ACK, [])
                for _ in range(8):
                    await drive.do_out(ctx, TransferResponse.ACK, [])
                await drive.do_in(ctx, _csw(0x00, tag=drive.last_tag), TransferResponse.ACK)
                # READ: accept the CBW, then busy-NAK the data phase for
                # far longer than WD cycles (see NAKS comment above).
                rcbw = []
                await drive.do_out(ctx, TransferResponse.ACK, rcbw)
                result["read_opcode"] = rcbw[15]
                for _ in range(NAKS):
                    await drive.do_in(ctx, [], TransferResponse.NAK)
                # Drive finally frees up: serve the block + a clean CSW.
                for i in range(8):
                    await drive.do_in(ctx, sector[i*64:(i+1)*64],
                                      TransferResponse.ACK)
                await drive.do_in(ctx, _csw(0x00, tag=drive.last_tag), TransferResponse.ACK)
                result["drive_done"] = True
            return tb

        self._run(fw_tb, drive_tb, deadline_us=120000, watchdog_cycles=WD)
        self.assertFalse(result.get("werr"), "write leg failed")
        self.assertEqual(result.get("read_opcode"), 0x28)   # READ(10)
        self.assertTrue(result.get("drive_done"),
                        "drive script never finished (engine was reset "
                        "mid-NAK-wait and abandoned the exchange)")
        self.assertFalse(result.get("rerr"),
                         "read errored instead of riding out the NAKs")
        self.assertEqual(result.get("rdata"), sector)

    def test_watchdog_still_fires_on_silent_drive(self):
        """Guard for the handshake-fed watchdog: TIMEOUT (silent bus — the
        yanked-drive signature) must NOT feed it. After a read whose CBW
        times out, the engine holds response=TIMEOUT; the watchdog must
        still expire and restart init (a fresh TUR CBW appears without any
        firmware command). This is the only unplug detection there is."""
        result = {}
        WD = 2500

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                await fw.csr_write32(ctx, 0x0C, 0x10)
                await fw.csr_write(ctx, 0x10, 1)          # start (read)
                for _ in range(400000):
                    r = await fw.csr_read(ctx, 0x18)
                    if r & 0b10:                           # resp.done
                        result["rerr"] = (r & 0b01) != 0
                        break
                # Now go quiet, exactly like firmware after a failed read;
                # wait for the drive script to observe the watchdog reinit.
                for _ in range(400000):
                    if result.get("reinit_seen"):
                        return
                    await ctx.tick()
            return tb

        def drive_tb(host):
            async def tb(ctx):
                drive = _Drive(host.scsi.enumerator)
                await drive.init_to_ready(ctx)
                # The read's CBW: answer TIMEOUT (device gone / no handshake).
                await drive.do_out(ctx, TransferResponse.TIMEOUT, [])
                # Engine rejects the command. With no further live handshake
                # the watchdog must fire and re-run init: the next transfer
                # the "drive" sees is a fresh TUR CBW it never asked for.
                cbw = []
                xtype, _pid = await drive.wait_start(ctx, cbw,
                                                     max_ticks=20 * WD)
                result["reinit_xfer_is_out"] = (xtype == TransferType.OUT)
                result["reinit_seen"] = True
            return tb

        self._run(fw_tb, drive_tb, deadline_us=120000, watchdog_cycles=WD)
        self.assertTrue(result.get("rerr"),
                        "TIMEOUT CBW should surface as resp.error")
        self.assertTrue(result.get("reinit_seen"),
                        "watchdog never fired on a silent drive")
        self.assertTrue(result.get("reinit_xfer_is_out"))

    def test_csw_failed_reaches_firmware_diag_and_autosenses(self):
        """A CSW with bCSWStatus=1 must surface as resp.error=1, be readable
        in the csw_status/csw_residue diag CSRs (proves the diag plumbing end
        to end), AND trigger an automatic REQUEST SENSE whose key/ASC/ASCQ
        land in the sense_info CSR — key=7/ASC=0x27 is the drive literally
        saying WRITE PROTECTED."""
        payload = [0xA5] * BLOCK
        result = {}

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                result["error"] = await fw.write_block(ctx, 7, payload)
                result["csw_stat"] = await fw.csr_read(ctx, 0x2C)
                result["csw_resid"] = await fw.csr_read32(ctx, 0x28)
                # Sense runs after resp.done — poll until valid.
                for _ in range(20000):
                    v = await fw.csr_read32(ctx, 0x34)
                    if v & (1 << 20):
                        result["sense"] = v & 0xFFFFF
                        break
            return tb

        def drive_tb(host):
            async def tb(ctx):
                drive = _Drive(host.scsi.enumerator)
                await drive.init_to_ready(ctx)
                await drive.do_out(ctx, TransferResponse.ACK, [])     # CBW
                for _ in range(8):
                    await drive.do_out(ctx, TransferResponse.ACK, [])
                await drive.do_in(ctx, _csw(0x01, residue=512, tag=drive.last_tag),
                                  TransferResponse.ACK)
                # Engine auto-issues REQUEST SENSE: CBW out, 18 bytes in, CSW.
                sense_cbw = []
                await drive.do_out(ctx, TransferResponse.ACK, sense_cbw)
                result["sense_opcode"] = sense_cbw[15]
                result["sense_alloc"] = sense_cbw[19]
                sense = [0x70, 0, 0x07] + [0] * 9 + [0x27, 0x00] + [0] * 4
                await drive.do_in(ctx, sense, TransferResponse.ACK)
                await drive.do_in(ctx, _csw(0x00, tag=drive.last_tag), TransferResponse.ACK)
            return tb

        self._run(fw_tb, drive_tb)
        self.assertTrue(result["error"])
        self.assertEqual(result["csw_stat"], 1)
        self.assertEqual(result["csw_resid"], 512)
        self.assertEqual(result["sense_opcode"], 0x03)   # REQUEST SENSE
        self.assertEqual(result["sense_alloc"], 18)
        self.assertEqual(result.get("sense"),
                         (0x7 << 16) | (0x27 << 8) | 0x00)

    def test_data_stall_recovers_via_clear_halt_and_reads_csw(self):
        """Round five (2026-07-15): the 8GB stick STALLed the bulk-OUT
        endpoint after 2 accepted data packets (rej=3/2/4 on hardware). Per
        BOT §6.7.3 the host must CLEAR_FEATURE(ENDPOINT_HALT) the endpoint,
        then read the CSW to learn why. Asserts the exact recovery sequence:
        SETUP bytes byte-exact on ep0, status stage, CSW read (CHECK
        CONDITION), auto REQUEST SENSE whose CBW goes out with the data
        toggle reset to DATA0."""
        payload = [0x33] * BLOCK
        result = {}

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                result["error"] = await fw.write_block(ctx, 7, payload)
                result["csw_stat"] = await fw.csr_read(ctx, 0x2C)
                ri = await fw.csr_read32(ctx, 0x30)
                result["rej_resp"] = ri & 0b111
                result["rej_phase"] = (ri >> 3) & 0b111
                result["rej_txdone"] = (ri >> 6) & 0b1111
                result["nyets"] = (ri >> 10) & 0xFF
                for _ in range(20000):
                    v = await fw.csr_read32(ctx, 0x34)
                    if v & (1 << 20):
                        result["sense"] = v & 0xFFFFF
                        break
            return tb

        def drive_tb(host):
            async def tb(ctx):
                drive = _Drive(host.scsi.enumerator)
                await drive.init_to_ready(ctx)
                await drive.do_out(ctx, TransferResponse.ACK, [])     # CBW
                await drive.do_out(ctx, TransferResponse.ACK, [])     # 64 B
                await drive.do_out(ctx, TransferResponse.ACK, [])     # 128 B
                await drive.do_out(ctx, TransferResponse.STALL)       # halt!
                setup = []
                await drive.do_setup(ctx, setup)      # CLEAR_FEATURE(HALT)
                result["setup"] = setup
                await drive.do_in(ctx, [], TransferResponse.ACK)      # status
                await drive.do_in(ctx, _csw(0x01, residue=384, tag=drive.last_tag),
                                  TransferResponse.ACK)               # CSW
                # Auto REQUEST SENSE; its CBW must restart at DATA0.
                sense_cbw = []
                result["sense_cbw_pid"] = await drive.do_out(
                    ctx, TransferResponse.ACK, sense_cbw)
                result["sense_opcode"] = sense_cbw[15]
                sense = [0x70, 0, 0x07] + [0] * 9 + [0x27, 0x00] + [0] * 4
                await drive.do_in(ctx, sense, TransferResponse.ACK)
                await drive.do_in(ctx, _csw(0x00, tag=drive.last_tag), TransferResponse.ACK)
            return tb

        self._run(fw_tb, drive_tb)
        self.assertIn("error", result, "firmware never saw resp.done")
        self.assertTrue(result["error"])
        self.assertEqual(result["csw_stat"], 1)
        # bmRequestType=0x02 (endpoint), bRequest=1 (CLEAR_FEATURE),
        # wValue=0 (ENDPOINT_HALT), wIndex=0x0002 (the stub's OUT ep).
        self.assertEqual(result["setup"],
                         [0x02, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00])
        # Breadcrumb latched even though the command recovered to a CSW.
        self.assertEqual(result["rej_resp"], TransferResponse.STALL.value)
        self.assertEqual(result["rej_phase"], 2)
        self.assertEqual(result["rej_txdone"], 4)    # 128 B = 4x32B ACKed
        self.assertEqual(result["nyets"], 0)
        self.assertEqual(result["sense_cbw_pid"], DataPID.DATA0)
        self.assertEqual(result["sense_opcode"], 0x03)
        self.assertEqual(result.get("sense"),
                         (0x7 << 16) | (0x27 << 8) | 0x00)

    def test_csw_stall_clears_halt_and_retries_once(self):
        """A STALLed CSW: per BOT §6.7.2 clear the IN endpoint's halt and
        retry the CSW read once — a drive that then delivers a PASSED CSW
        completes the write with error=0 (previously this failed the
        command outright). The retried CSW IN must restart at DATA0."""
        payload = [0x5A] * BLOCK
        result = {}

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                result["error"] = await fw.write_block(ctx, 7, payload)
                ri = await fw.csr_read32(ctx, 0x30)
                result["rej_resp"] = ri & 0b111
                result["rej_phase"] = (ri >> 3) & 0b111
            return tb

        def drive_tb(host):
            async def tb(ctx):
                drive = _Drive(host.scsi.enumerator)
                await drive.init_to_ready(ctx)
                await drive.do_out(ctx, TransferResponse.ACK, [])     # CBW
                for _ in range(8):
                    await drive.do_out(ctx, TransferResponse.ACK, [])
                await drive.do_in(ctx, [], TransferResponse.STALL)    # CSW!
                setup = []
                await drive.do_setup(ctx, setup)      # CLEAR_FEATURE(HALT)
                result["setup"] = setup
                await drive.do_in(ctx, [], TransferResponse.ACK)      # status
                result["csw_pid"] = await drive.do_in(
                    ctx, _csw(0x00, tag=drive.last_tag), TransferResponse.ACK)            # retry
            return tb

        self._run(fw_tb, drive_tb)
        self.assertIn("error", result, "firmware never saw resp.done")
        self.assertFalse(result["error"])
        # wIndex=0x0081: IN direction bit + the stub's IN ep number.
        self.assertEqual(result["setup"],
                         [0x02, 0x01, 0x00, 0x00, 0x81, 0x00, 0x00, 0x00])
        self.assertEqual(result["csw_pid"], DataPID.DATA0)
        # Breadcrumb of the recovered STALL survives the successful retry.
        self.assertEqual(result["rej_resp"], TransferResponse.STALL.value)
        self.assertEqual(result["rej_phase"], 3)

    def test_csw_double_stall_escalates_to_reset_recovery(self):
        """If the CSW STALLs again after a clear-halt, reset recovery is
        needed — before round eight the engine failed the command promptly
        and parked; now it runs the full BOT §5.3.4 recovery sequence
        autonomously and returns to ready."""
        payload = [0xC3] * BLOCK
        result = {}
        setups = []

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                result["error"] = await fw.write_block(ctx, 7, payload,
                                                       max_polls=60000)
                ri = await fw.csr_read32(ctx, 0x30)
                result["rej_resp"] = ri & 0b111
                result["rej_phase"] = (ri >> 3) & 0b111
                result["last_phase"] = (ri >> 18) & 0b111
                await fw.wait_ready(ctx)      # ready again post-recovery
                result["recovered"] = True
            return tb

        def drive_tb(host):
            async def tb(ctx):
                drive = _Drive(host.scsi.enumerator)
                await drive.init_to_ready(ctx)
                await drive.do_out(ctx, TransferResponse.ACK, [])     # CBW
                for _ in range(8):
                    await drive.do_out(ctx, TransferResponse.ACK, [])
                await drive.do_in(ctx, [], TransferResponse.STALL)    # CSW!
                await drive.do_setup(ctx, [])         # CLEAR_FEATURE(HALT)
                await drive.do_in(ctx, [], TransferResponse.ACK)      # status
                await drive.do_in(ctx, [], TransferResponse.STALL)    # again!
                # --- Reset Recovery ---
                s = []
                await drive.do_setup(ctx, s)          # MSC reset (0x21, 0xFF)
                setups.append(list(s))
                await drive.do_in(ctx, [], TransferResponse.ACK)  # status ZLP
                s = []
                await drive.do_setup(ctx, s)          # clear halt IN (0x81)
                setups.append(list(s))
                await drive.do_in(ctx, [], TransferResponse.ACK)
                s = []
                await drive.do_setup(ctx, s)          # clear halt OUT (0x02)
                setups.append(list(s))
                await drive.do_in(ctx, [], TransferResponse.ACK)
                # --- post-recovery revalidation: TEST UNIT READY ---
                cbw = []
                await drive.do_out(ctx, TransferResponse.ACK, cbw)
                await drive.do_in(
                    ctx, _csw(tag=drive.last_tag), TransferResponse.ACK)
            return tb

        self._run(fw_tb, drive_tb, deadline_us=80000)
        self.assertIn("error", result,
                      "engine hung on a double-STALLed CSW (no resp.done)")
        self.assertTrue(result["error"])
        self.assertEqual(result["rej_resp"], TransferResponse.STALL.value)
        self.assertEqual(result["rej_phase"], 3)
        # The live-phase breadcrumb saw the CSW phase last.
        self.assertEqual(result["last_phase"], 3)
        self.assertTrue(result.get("recovered"))
        self.assertEqual(setups[0], [0x21, 0xFF, 0, 0, 0, 0, 0, 0])
        self.assertEqual(setups[1], [0x02, 0x01, 0, 0, 0x81, 0, 0, 0])
        self.assertEqual(setups[2], [0x02, 0x01, 0, 0, 0x02, 0, 0, 0])

    def test_csw_with_wrong_tag_is_write_failure(self):
        """A stale/garbage CSW (wrong tag) after a write must surface as
        resp.error with reject_info.csw_bad set — before round eight it
        read as a clean success if its status byte happened to be 0. Since
        this task, an invalid CSW also triggers BOT §5.3.4 Reset Recovery,
        so the drive script must play out the recovery sequence too."""
        result = {}
        setups = []

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                err = await fw.write_block(ctx, 100, [0xAB] * BLOCK)
                # reject_info is a 32-bit read at 0x30; csw_bad is the field
                # above last_phase (bit position per the generated layout —
                # read via the packed register and check nonzero phase=3
                # plus the csw_bad bit; simplest robust check: the write
                # errored and rej phase == 3).
                rej = await fw.csr_read32(ctx, 0x30)
                result["err"] = err
                result["rej_phase"] = (rej >> 3) & 0b111
                result["csw_bad"] = (rej >> 21) & 1
                await fw.wait_ready(ctx)      # ready again post-recovery
                result["recovered"] = True
            return tb

        def drive_tb(host):
            async def tb(ctx):
                drive = _Drive(host.scsi.enumerator)
                await drive.init_to_ready(ctx)
                cbw = []
                await drive.do_out(ctx, TransferResponse.ACK, cbw)  # CBW
                for _ in range(8):                                   # 8x64B data
                    await drive.do_out(ctx, TransferResponse.ACK)
                await drive.do_in(
                    ctx, _csw(tag=drive.last_tag + 7), TransferResponse.ACK)
                # --- Reset Recovery ---
                s = []
                await drive.do_setup(ctx, s)          # MSC reset (0x21, 0xFF)
                setups.append(list(s))
                await drive.do_in(ctx, [], TransferResponse.ACK)  # status ZLP
                s = []
                await drive.do_setup(ctx, s)          # clear halt IN (0x81)
                setups.append(list(s))
                await drive.do_in(ctx, [], TransferResponse.ACK)
                s = []
                await drive.do_setup(ctx, s)          # clear halt OUT (0x02)
                setups.append(list(s))
                await drive.do_in(ctx, [], TransferResponse.ACK)
                # --- post-recovery revalidation: TEST UNIT READY ---
                cbw = []
                await drive.do_out(ctx, TransferResponse.ACK, cbw)
                await drive.do_in(
                    ctx, _csw(tag=drive.last_tag), TransferResponse.ACK)
            return tb

        self._run(fw_tb, drive_tb, deadline_us=80000)
        self.assertTrue(result["err"])
        self.assertEqual(result["rej_phase"], 3)
        self.assertEqual(result["csw_bad"], 1)
        self.assertTrue(result.get("recovered"))
        self.assertEqual(setups[0], [0x21, 0xFF, 0, 0, 0, 0, 0, 0])
        self.assertEqual(setups[1], [0x02, 0x01, 0, 0, 0x81, 0, 0, 0])
        self.assertEqual(setups[2], [0x02, 0x01, 0, 0, 0x02, 0, 0, 0])

    def test_phase_error_csw_triggers_reset_recovery(self):
        """BOT §5.3.4: CSW status 0x02 (phase error) requires Reset Recovery
        (MSC reset + clear both halts) before the next CBW - issuing any
        other command (the old auto-REQUEST-SENSE) is a spec violation.
        The engine must: fail the write to firmware, run the recovery
        sequence autonomously, re-run TEST UNIT READY, and return to ready
        with both toggles back at DATA0."""
        result = {}
        setups = []

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                result["err"] = await fw.write_block(ctx, 100, [0x5A] * BLOCK)
                await fw.wait_ready(ctx)      # ready again post-recovery
                result["recovered"] = True
            return tb

        def drive_tb(host):
            async def tb(ctx):
                stub = host.scsi.enumerator
                drive = _Drive(stub)
                await drive.init_to_ready(ctx)
                cbw = []
                await drive.do_out(ctx, TransferResponse.ACK, cbw)   # CBW
                for _ in range(8):
                    await drive.do_out(ctx, TransferResponse.ACK)    # data
                await drive.do_in(
                    ctx, _csw(status=0x02, tag=drive.last_tag),
                    TransferResponse.ACK)                            # PHASE ERROR
                # --- Reset Recovery ---
                s = []
                await drive.do_setup(ctx, s)          # MSC reset (0x21, 0xFF)
                setups.append(list(s))
                await drive.do_in(ctx, [], TransferResponse.ACK)  # status ZLP
                s = []
                await drive.do_setup(ctx, s)          # clear halt IN (0x81)
                setups.append(list(s))
                await drive.do_in(ctx, [], TransferResponse.ACK)
                s = []
                await drive.do_setup(ctx, s)          # clear halt OUT (0x02)
                setups.append(list(s))
                await drive.do_in(ctx, [], TransferResponse.ACK)
                # --- post-recovery revalidation: TEST UNIT READY ---
                cbw = []
                pid = await drive.do_out(ctx, TransferResponse.ACK, cbw)
                result["tur_pid"] = pid               # toggle reset -> DATA0
                result["tur_opcode"] = cbw[15]
                await drive.do_in(
                    ctx, _csw(tag=drive.last_tag), TransferResponse.ACK)
            return tb

        self._run(fw_tb, drive_tb, deadline_us=80000)
        self.assertTrue(result["err"])                # firmware saw failure
        self.assertTrue(result.get("recovered"))
        self.assertEqual(setups[0], [0x21, 0xFF, 0, 0, 0, 0, 0, 0])
        self.assertEqual(setups[1], [0x02, 0x01, 0, 0, 0x81, 0, 0, 0])
        self.assertEqual(setups[2], [0x02, 0x01, 0, 0, 0x02, 0, 0, 0])
        self.assertEqual(result["tur_opcode"], 0x00)  # TEST UNIT READY
        self.assertEqual(result["tur_pid"], DataPID.DATA0)

    def test_read_data_stall_recovers_via_clear_halt_and_reads_csw(self):
        """BOT §6.7.2 / case 4: a device that halts the IN endpoint mid
        data phase (e.g. unrecoverable read error) still owes a CSW. The
        engine must clear the halt, read the CSW, and — on CSW FAILED —
        drain sense, exactly like the write side has since round five.
        Before round eight the Default arm rejected and left both the halt
        and the queued CSW in place, desyncing the next command."""
        result = {}

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                # Reuse the read sequence from
                # test_read_immediately_after_successful_write: lba+start,
                # then poll resp; expect error.
                await fw.csr_write32(ctx, 0x0C, 40)   # lba
                await fw.csr_write(ctx, 0x10, 1)      # start (read)
                for _ in range(400000):
                    r = await fw.csr_read(ctx, 0x18)
                    if r & 0b10:
                        result["err"] = (r & 0b01) != 0
                        break
                # sense captured after the auto-REQUEST-SENSE, which runs
                # in the background after resp.done fires — poll for it
                # (same pattern as test_data_stall_recovers_via_clear_halt_
                # and_reads_csw on the write side).
                for _ in range(20000):
                    si = await fw.csr_read32(ctx, 0x34)
                    if (si >> 20) & 1:
                        result["sense_valid"] = (si >> 20) & 1
                        result["sense_key"] = (si >> 16) & 0xF
                        break
            return tb

        def drive_tb(host):
            async def tb(ctx):
                stub = host.scsi.enumerator
                drive = _Drive(stub)
                await drive.init_to_ready(ctx)
                cbw = []
                await drive.do_out(ctx, TransferResponse.ACK, cbw)  # READ(10) CBW
                # STALL the first data-IN transaction (no payload).
                await drive.do_in(ctx, [], TransferResponse.STALL)
                s = []
                await drive.do_setup(ctx, s)                    # clear halt IN
                assert s == [0x02, 0x01, 0, 0, 0x81, 0, 0, 0], s
                await drive.do_in(ctx, [], TransferResponse.ACK)  # status ZLP
                await drive.do_in(                                 # CSW FAILED
                    ctx, _csw(status=0x01, residue=512, tag=drive.last_tag),
                    TransferResponse.ACK)
                # auto-REQUEST-SENSE: CBW, 18 bytes, CSW
                cbw = []
                await drive.do_out(ctx, TransferResponse.ACK, cbw)
                assert cbw[15] == 0x03, cbw                        # REQUEST SENSE
                sense = [0x70, 0, 0x03, 0, 0, 0, 0, 10] + [0] * 10  # key=3 MEDIUM ERROR
                await drive.do_in(ctx, sense, TransferResponse.ACK)
                await drive.do_in(
                    ctx, _csw(tag=drive.last_tag), TransferResponse.ACK)
            return tb

        self._run(fw_tb, drive_tb, deadline_us=80000)
        self.assertTrue(result["err"])
        self.assertEqual(result["sense_valid"], 1)
        self.assertEqual(result["sense_key"], 0x3)

    def test_flush_issues_synchronize_cache_10(self):
        """Round eight durability: strobing start_flush (0x3C) must emit a
        SYNCHRONIZE CACHE(10) CBW — opcode 0x35, zero data length — and
        complete via the sticky resp bits like a write."""
        result = {}

        def fw_tb(periph, host):
            async def tb(ctx):
                fw = _Fw(periph)
                await fw.wait_ready(ctx)
                await fw.csr_write(ctx, 0x3C, 1)      # start_flush
                for _ in range(400000):
                    r = await fw.csr_read(ctx, 0x18)  # resp
                    if r & 0b10:
                        result["err"] = (r & 0b01) != 0
                        return
                raise AssertionError("flush never completed")
            return tb

        def drive_tb(host):
            async def tb(ctx):
                stub = host.scsi.enumerator
                drive = _Drive(stub)
                await drive.init_to_ready(ctx)
                cbw = []
                await drive.do_out(ctx, TransferResponse.ACK, cbw)
                result["opcode"] = cbw[15]
                result["dlen"] = int.from_bytes(bytes(cbw[8:12]), "little")
                await drive.do_in(
                    ctx, _csw(tag=drive.last_tag), TransferResponse.ACK)
            return tb

        self._run(fw_tb, drive_tb)
        self.assertFalse(result["err"])
        self.assertEqual(result["opcode"], 0x35)
        self.assertEqual(result["dlen"], 0)


if __name__ == "__main__":
    unittest.main()
