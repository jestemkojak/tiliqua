# Copyright (c) 2024 S. Holzapfel <me@sebholzapfel.com>
#
# SPDX-License-Identifier: BSD-3-Clause
"""
Sim tests for the vendored guh MSC engine's SCSI WRITE(10) / bulk-OUT data path
(M6b, src/top/mbsid/M6_USB_STORAGE.md §4b).

We drive `SCSIBulkHost` directly with a stub enumerator: a component exposing the
same `.ctrl` (an `USBSIEInterface`) + `.status` + `.parser.o` surface the real
`USBHostEnumerator` presents, but with no SIE behind it — the testbench itself
plays the SIE, capturing bytes the DUT pushes on `ctrl.txs`, feeding `ctrl.rxs`,
and answering ACK/NAK on `ctrl.status.response`.
"""

import unittest

from amaranth import *
from amaranth.lib import wiring
from amaranth.lib.wiring import In, Out
from amaranth.sim import Simulator

# Vendored SIE types (not guh's): the stub's ctrl signature and the response
# codes the drive scripts set must be the SAME enum class the vendored engine
# compares against — guh's TransferResponse lacks NYET=7 and mixing the two
# classes makes ctx.set() reject members of the other.
from vendor.guh_msc.sie import (
    USBSIEInterface, TransferType, TransferResponse, DataPID,
)

from vendor.guh_msc.msc import (
    SCSIBulkHost, SCSIOpCode, CBWFlags, CBW_SIZE_BYTES, CSW_SIZE_BYTES,
)


class StubEnumerator(wiring.Component):
    """Just enough surface for SCSIBulkHost: a `ctrl` SIE interface driven from
    the testbench + always-enumerated status + fixed BULK endpoint numbers."""

    ctrl: Out(USBSIEInterface())

    def __init__(self):
        super().__init__()
        # Plain (non-port) attributes: SCSIBulkHost reads these structurally.
        class _Bag:
            pass
        self.status = _Bag()
        self.status.enumerated = Signal(init=1)
        self.status.dev_addr = Signal(7, init=0x12)
        self.parser = _Bag()
        self.parser.o = _Bag()
        self.parser.o.valid = Signal(init=1)
        self.parser.o.i_endp = _Bag()
        self.parser.o.i_endp.number = Signal(4, init=1)   # IN  endpoint
        self.parser.o.o_endp = _Bag()
        self.parser.o.o_endp.number = Signal(4, init=2)   # OUT endpoint

    def elaborate(self, platform):
        return Module()


# Big-endian LBA 0x11 0x22 0x33 0x44 on the wire == field value 0x44332211
# (the field serialises LSB-first, same as USBMSCHost's be/le Cat swizzle).
LBA_WIRE = (0x11, 0x22, 0x33, 0x44)
LBA_FIELD = 0x44332211
BLOCK_SIZE = 512


class _Sie:
    """Tick-level SIE emulator shared by the assertions below."""

    def __init__(self, dut, stub):
        self.dut = dut
        self.stub = stub
        self.tx_counter = 0   # counting pattern fed into dut.tx_data
        self.last_tag = 0

    async def tick(self, ctx, capture=None):
        c = self.stub.ctrl
        ctx.set(c.txs.ready, 1)
        ctx.set(self.dut.tx_data.valid, 1)
        ctx.set(self.dut.tx_data.payload, self.tx_counter & 0xFF)
        # Byte pushed onto the SIE tx FIFO this cycle (LOAD or RELOAD source).
        if ctx.get(c.txs.valid) and ctx.get(c.txs.ready):
            if capture is not None:
                capture.append(ctx.get(c.txs.payload))
        # Fresh payload byte consumed from tx_data (does NOT advance on replay).
        if ctx.get(self.dut.tx_data.valid) and ctx.get(self.dut.tx_data.ready):
            self.tx_counter += 1
        await ctx.tick("usb")

    async def to_idle_and_start(self, ctx, *, data_dir, opcode, data_len):
        """Advance WAIT-ENUMERATION -> IDLE and strobe a command."""
        c = self.stub.ctrl
        ctx.set(c.status.idle, 1)
        await self.tick(ctx)   # WAIT-ENUMERATION -> IDLE
        cmd = self.dut.cmd
        ctx.set(cmd.start, 1)
        ctx.set(cmd.data_len, data_len)
        ctx.set(cmd.data_dir, data_dir)
        ctx.set(cmd.stream_data, 0)
        ctx.set(cmd.cdb.cdb10.opcode, opcode)
        ctx.set(cmd.cdb.cdb10.lba_be, LBA_FIELD)
        await self.tick(ctx)   # IDLE latches -> CBW-LOAD
        ctx.set(cmd.start, 0)

    async def expect_out(self, ctx, response, capture):
        """Play one bulk-OUT transaction: capture bytes on ctrl.txs until the DUT
        asserts xfer.start, then answer `response`. Returns (pid, type, ep)."""
        c = self.stub.ctrl
        ctx.set(c.status.idle, 1)
        while not ctx.get(c.xfer.start):
            await self.tick(ctx, capture)
        pid = ctx.get(c.xfer.data_pid)
        xtype = ctx.get(c.xfer.type)
        ep = ctx.get(c.xfer.ep_addr)
        await self.tick(ctx)                       # commit XFER -> *-WAIT (idle=1)
        ctx.set(c.status.response, response)
        await self.tick(ctx)                       # DUT reads response
        if (capture is not None and len(capture) >= 8
                and bytes(capture[:4]) == b"USBC"):
            self.last_tag = int.from_bytes(bytes(capture[4:8]), "little")
        return pid, xtype, ep

    async def do_in(self, ctx, payload, response):
        """Play one bulk-IN transaction (e.g. CSW): feed `payload` on ctrl.rxs,
        then answer `response`. Returns (done, error) sampled on completion."""
        c = self.stub.ctrl
        ctx.set(c.status.idle, 1)
        while not ctx.get(c.xfer.start):
            await self.tick(ctx)
        await self.tick(ctx)                       # commit -> *-RX (idle=1)
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
        done = ctx.get(self.dut.status.done)
        error = ctx.get(self.dut.status.error)
        await self.tick(ctx)
        ctx.set(c.status.idle, 0)
        return done, error


def _build():
    stub = StubEnumerator()
    dut = SCSIBulkHost(enumerator=stub)
    m = Module()
    m.submodules.dut = dut
    return m, dut, stub


def _passing_csw(tag=1):
    b = [0x55, 0x53, 0x42, 0x53]          # dCSWSignature
    b += list(int(tag).to_bytes(4, "little"))  # dCSWTag (must echo the CBW's)
    b += [0, 0, 0, 0]                      # dCSWDataResidue
    b += [0x00]                            # bCSWStatus = passed
    return b


def _failing_csw(tag=1):
    b = _passing_csw(tag)
    b[-1] = 0x01                           # bCSWStatus = failed
    return b


class GuhMscWriteTests(unittest.TestCase):

    def test_read_byte_diagnostic_saturates_at_1023(self):
        """The 10-bit live diagnostic clamps instead of wrapping when the
        functional transport counter accepts a 1024th byte."""
        m, dut, stub = _build()

        async def tb(ctx):
            sie = _Sie(dut, stub)
            await sie.to_idle_and_start(
                ctx, data_dir=0, opcode=SCSIOpCode.READ_10, data_len=1024)
            await sie.expect_out(ctx, TransferResponse.ACK, [])
            for _ in range(16):
                await sie.do_in(ctx, [0xA5] * 64, TransferResponse.ACK)
            self.assertEqual(ctx.get(dut.rx_bytes_o), 0x3FF)

        sim = Simulator(m)
        sim.add_clock(1 / 60e6, domain="usb")
        sim.add_testbench(tb)
        sim.run()

    def test_read_byte_diagnostic_resets_on_command_start(self):
        """A new command clears the prior byte count before its CBW is ACKed."""
        m, dut, stub = _build()

        async def tb(ctx):
            sie = _Sie(dut, stub)
            await sie.to_idle_and_start(
                ctx, data_dir=0, opcode=SCSIOpCode.READ_10, data_len=1)
            await sie.expect_out(ctx, TransferResponse.ACK, [])
            await sie.do_in(ctx, [0xA5], TransferResponse.ACK)
            await sie.do_in(ctx, _passing_csw(sie.last_tag), TransferResponse.ACK)
            self.assertEqual(ctx.get(dut.rx_bytes_o), 1)

            ctx.set(dut.cmd.start, 1)
            ctx.set(dut.cmd.data_len, 1)
            ctx.set(dut.cmd.data_dir, 0)
            ctx.set(dut.cmd.stream_data, 0)
            ctx.set(dut.cmd.cdb.cdb10.opcode, SCSIOpCode.READ_10)
            await sie.tick(ctx)
            self.assertEqual(ctx.get(dut.rx_bytes_o), 0)

        sim = Simulator(m)
        sim.add_clock(1 / 60e6, domain="usb")
        sim.add_testbench(tb)
        sim.run()

    def test_write_cbw_fields(self):
        """(1) A write command emits a 31-byte CBW: flags=0x00 (DATA_OUT),
        opcode=0x2A (WRITE_10), LBA big-endian 11 22 33 44."""
        m, dut, stub = _build()
        cbw = []

        async def tb(ctx):
            sie = _Sie(dut, stub)
            await sie.to_idle_and_start(
                ctx, data_dir=1, opcode=SCSIOpCode.WRITE_10, data_len=BLOCK_SIZE)
            await sie.expect_out(ctx, TransferResponse.ACK, cbw)

        sim = Simulator(m)
        sim.add_clock(1 / 60e6, domain="usb")
        sim.add_testbench(tb)
        sim.run()

        self.assertEqual(len(cbw), CBW_SIZE_BYTES)
        self.assertEqual(cbw[12], CBWFlags.DATA_OUT.value)   # bmCBWFlags
        self.assertEqual(cbw[15], SCSIOpCode.WRITE_10.value)  # CDB opcode
        self.assertEqual(tuple(cbw[17:21]), LBA_WIRE)         # LBA big-endian

    def test_write_data_phase_chunks(self):
        """(2) A 512-byte payload streams as 8 x 64-byte OUT transactions on ep2
        with the data PID toggling per chunk."""
        m, dut, stub = _build()
        cbw = []
        chunks = []
        meta = []

        async def tb(ctx):
            sie = _Sie(dut, stub)
            await sie.to_idle_and_start(
                ctx, data_dir=1, opcode=SCSIOpCode.WRITE_10, data_len=BLOCK_SIZE)
            await sie.expect_out(ctx, TransferResponse.ACK, cbw)
            for _ in range(8):
                buf = []
                pid, xtype, ep = await sie.expect_out(
                    ctx, TransferResponse.ACK, buf)
                chunks.append(buf)
                meta.append((pid, xtype, ep))

        sim = Simulator(m)
        sim.add_clock(1 / 60e6, domain="usb")
        sim.add_testbench(tb)
        sim.run()

        payload = [b for c in chunks for b in c]
        self.assertEqual(len(payload), BLOCK_SIZE)
        self.assertEqual(payload, [i & 0xFF for i in range(BLOCK_SIZE)])
        self.assertTrue(all(len(c) == 64 for c in chunks))
        # All OUT transactions target the OUT endpoint (2).
        self.assertTrue(all(xtype == TransferType.OUT for _, xtype, _ in meta))
        self.assertTrue(all(ep == 2 for _, _, ep in meta))
        # PID toggles every chunk; first data chunk is DATA1 (CBW ACK toggled it).
        pids = [p for p, _, _ in meta]
        self.assertEqual(pids[0], DataPID.DATA1)
        for a, b in zip(pids, pids[1:]):
            self.assertNotEqual(a, b)

    def test_write_data_phase_chunks_31_bytes(self):
        """(2b) With tx_chunk_bytes=31 (2026-07-15 diagnostic build: data
        packets byte-identical in length to a hardware-proven CBW), a 512-byte
        payload streams as 16 x 31-byte chunks + one 16-byte tail, PID toggling
        throughout, payload intact."""
        stub = StubEnumerator()
        dut = SCSIBulkHost(enumerator=stub, tx_chunk_bytes=31)
        m = Module()
        m.submodules.dut = dut
        cbw = []
        chunks = []
        meta = []

        async def tb(ctx):
            sie = _Sie(dut, stub)
            await sie.to_idle_and_start(
                ctx, data_dir=1, opcode=SCSIOpCode.WRITE_10, data_len=BLOCK_SIZE)
            await sie.expect_out(ctx, TransferResponse.ACK, cbw)
            for _ in range(17):
                buf = []
                pid, xtype, ep = await sie.expect_out(
                    ctx, TransferResponse.ACK, buf)
                chunks.append(buf)
                meta.append((pid, xtype, ep))
            # Command completes with a passing CSW.
            done, error = await sie.do_in(
                ctx, _passing_csw(sie.last_tag), TransferResponse.ACK)
            self.assertTrue(done)
            self.assertFalse(error)

        sim = Simulator(m)
        sim.add_clock(1 / 60e6, domain="usb")
        sim.add_testbench(tb)
        sim.run()

        payload = [b for c in chunks for b in c]
        self.assertEqual(len(payload), BLOCK_SIZE)
        self.assertEqual(payload, [i & 0xFF for i in range(BLOCK_SIZE)])
        self.assertEqual([len(c) for c in chunks], [31] * 16 + [16])
        self.assertTrue(all(ep == 2 for _, _, ep in meta))
        pids = [p for p, _, _ in meta]
        self.assertEqual(pids[0], DataPID.DATA1)
        for a, b in zip(pids, pids[1:]):
            self.assertNotEqual(a, b)

    def test_write_nak_replays_same_chunk(self):
        """(3) A NAK on a data OUT transaction re-issues the same chunk with the
        same PID (bytes come from the local replay buffer, not tx_data)."""
        m, dut, stub = _build()
        cbw = []
        first = []
        replay = []
        pids = []

        async def tb(ctx):
            sie = _Sie(dut, stub)
            await sie.to_idle_and_start(
                ctx, data_dir=1, opcode=SCSIOpCode.WRITE_10, data_len=BLOCK_SIZE)
            await sie.expect_out(ctx, TransferResponse.ACK, cbw)
            # First data chunk: NAK it.
            p0, _, _ = await sie.expect_out(ctx, TransferResponse.NAK, first)
            pids.append(p0)
            # DUT replays the same chunk; ACK this time.
            p1, _, _ = await sie.expect_out(ctx, TransferResponse.ACK, replay)
            pids.append(p1)
            # Drain remaining 7 chunks so the FSM completes cleanly.
            for _ in range(7):
                await sie.expect_out(ctx, TransferResponse.ACK, [])

        sim = Simulator(m)
        sim.add_clock(1 / 60e6, domain="usb")
        sim.add_testbench(tb)
        sim.run()

        self.assertEqual(len(first), 64)
        self.assertEqual(replay, first)          # identical payload replayed
        self.assertEqual(pids[0], pids[1])       # same PID (not toggled on NAK)

    def test_write_csw_pass_and_fail(self):
        """(4) CSW status is reflected in status.done / status.error."""
        for build_csw, exp_err in ((_passing_csw, 0), (_failing_csw, 1)):
            with self.subTest(fail=exp_err):
                m, dut, stub = _build()
                result = {}

                async def tb(ctx, build_csw=build_csw, result=result):
                    sie = _Sie(dut, stub)
                    await sie.to_idle_and_start(
                        ctx, data_dir=1, opcode=SCSIOpCode.WRITE_10,
                        data_len=BLOCK_SIZE)
                    cbw = []
                    await sie.expect_out(ctx, TransferResponse.ACK, cbw)
                    for _ in range(8):
                        await sie.expect_out(ctx, TransferResponse.ACK, [])
                    done, error = await sie.do_in(
                        ctx, build_csw(sie.last_tag), TransferResponse.ACK)
                    result["done"] = done
                    result["error"] = error

                sim = Simulator(m)
                sim.add_clock(1 / 60e6, domain="usb")
                sim.add_testbench(tb)
                sim.run()

                self.assertEqual(result["done"], 1)
                self.assertEqual(result["error"], exp_err)

    def test_cbw_nak_retries_same_cbw(self):
        """(6) Regression (2026-07-15 hardware bring-up): a NAK on the CBW OUT
        transaction is flow control, not rejection — the engine must re-send
        the identical 31-byte CBW with the same PID and then proceed normally.
        A real drive NAKs CBWs while doing flash housekeeping; the old code's
        Default arm failed the whole command (reject resp=2 phase=1)."""
        m, dut, stub = _build()
        first = []
        retry = []
        pids = []
        rejected = {}

        async def tb(ctx):
            sie = _Sie(dut, stub)
            await sie.to_idle_and_start(
                ctx, data_dir=1, opcode=SCSIOpCode.WRITE_10, data_len=BLOCK_SIZE)
            # NAK the CBW.
            p0, _, _ = await sie.expect_out(ctx, TransferResponse.NAK, first)
            pids.append(p0)
            rejected["after_nak"] = ctx.get(dut.status.rejected)
            # Engine must re-send the same CBW. Bounded wait: the broken
            # engine goes IDLE after the NAK and never retries — fail the
            # test instead of hanging the simulation.
            c = stub.ctrl
            ctx.set(c.status.idle, 1)
            for _ in range(200):
                if ctx.get(c.xfer.start):
                    break
                await sie.tick(ctx, retry)
            else:
                self.fail("engine never re-sent the CBW after a NAK "
                          "(treated flow control as rejection)")
            p1 = ctx.get(c.xfer.data_pid)
            await sie.tick(ctx)                    # commit XFER -> CBW-WAIT
            ctx.set(c.status.response, TransferResponse.ACK)
            await sie.tick(ctx)                    # DUT reads response
            pids.append(p1)
            # Command proceeds into the data phase as usual.
            data = []
            await sie.expect_out(ctx, TransferResponse.ACK, data)
            rejected["data_len"] = len(data)

        sim = Simulator(m)
        sim.add_clock(1 / 60e6, domain="usb")
        sim.add_testbench(tb)
        sim.run()

        self.assertEqual(rejected["after_nak"], 0)   # not treated as fatal
        self.assertEqual(len(first), CBW_SIZE_BYTES)
        self.assertEqual(retry, first)               # identical CBW re-sent
        self.assertEqual(pids[0], pids[1])           # same PID (no toggle on NAK)
        self.assertEqual(rejected["data_len"], 64)   # first data chunk followed

    def test_read_cbw_flags_unchanged(self):
        """(5) Regression: a READ command (data_dir=0) still emits flags 0x80."""
        m, dut, stub = _build()
        cbw = []

        async def tb(ctx):
            sie = _Sie(dut, stub)
            await sie.to_idle_and_start(
                ctx, data_dir=0, opcode=SCSIOpCode.READ_10, data_len=BLOCK_SIZE)
            await sie.expect_out(ctx, TransferResponse.ACK, cbw)

        sim = Simulator(m)
        sim.add_clock(1 / 60e6, domain="usb")
        sim.add_testbench(tb)
        sim.run()

        self.assertEqual(len(cbw), CBW_SIZE_BYTES)
        self.assertEqual(cbw[12], CBWFlags.DATA_IN.value)     # 0x80
        self.assertEqual(cbw[15], SCSIOpCode.READ_10.value)   # 0x28

    def test_csw_bad_tag_rejected(self):
        """BOT §6.3.1: a CSW whose dCSWTag doesn't echo the CBW's dCBWTag is
        not a valid CSW — the command must be rejected (and csw_bad_o set),
        never reported as success."""
        m, dut, stub = _build()
        sie = _Sie(dut, stub)

        async def tb(ctx):
            await sie.to_idle_and_start(
                ctx, data_dir=0, opcode=0x00, data_len=0)  # TEST UNIT READY
            cbw = []
            await sie.expect_out(ctx, TransferResponse.ACK, cbw)
            done, error = await sie.do_in(
                ctx, _passing_csw(tag=sie.last_tag + 1), TransferResponse.ACK)
            self.assertEqual(done, 1)
            self.assertEqual(ctx.get(dut.csw_bad_o), 1)
            self.assertEqual(ctx.get(dut.status.rejected), 0)  # strobe passed
            # csw_bad_o persists until the next cmd.start
            await sie.tick(ctx)
            self.assertEqual(ctx.get(dut.csw_bad_o), 1)

        sim = Simulator(m)
        sim.add_clock(1e-6, domain="usb")
        sim.add_testbench(tb)
        sim.run()

    def test_csw_bad_signature_rejected(self):
        """BOT §6.3.1: bad dCSWSignature = not a CSW at all."""
        m, dut, stub = _build()
        sie = _Sie(dut, stub)

        async def tb(ctx):
            await sie.to_idle_and_start(
                ctx, data_dir=0, opcode=0x00, data_len=0)
            cbw = []
            await sie.expect_out(ctx, TransferResponse.ACK, cbw)
            bad = _passing_csw(tag=sie.last_tag)
            bad[0] = 0x00                     # corrupt the signature
            done, _ = await sie.do_in(ctx, bad, TransferResponse.ACK)
            self.assertEqual(done, 1)
            self.assertEqual(ctx.get(dut.csw_bad_o), 1)

        sim = Simulator(m)
        sim.add_clock(1e-6, domain="usb")
        sim.add_testbench(tb)
        sim.run()


if __name__ == "__main__":
    unittest.main()
