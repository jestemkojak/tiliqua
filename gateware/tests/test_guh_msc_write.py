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

from guh.usbh.sie import USBSIEInterface, TransferType, TransferResponse, DataPID

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


def _passing_csw():
    b = [0x55, 0x53, 0x42, 0x53]          # dCSWSignature
    b += [0, 0, 0, 0]                      # dCSWTag
    b += [0, 0, 0, 0]                      # dCSWDataResidue
    b += [0x00]                            # bCSWStatus = PASSED
    return b


def _failing_csw():
    b = _passing_csw()
    b[-1] = 0x01                           # bCSWStatus = FAILED
    return b


class GuhMscWriteTests(unittest.TestCase):

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
        for csw, exp_err in ((_passing_csw(), 0), (_failing_csw(), 1)):
            with self.subTest(fail=exp_err):
                m, dut, stub = _build()
                result = {}

                async def tb(ctx, csw=csw, result=result):
                    sie = _Sie(dut, stub)
                    await sie.to_idle_and_start(
                        ctx, data_dir=1, opcode=SCSIOpCode.WRITE_10,
                        data_len=BLOCK_SIZE)
                    await sie.expect_out(ctx, TransferResponse.ACK, [])
                    for _ in range(8):
                        await sie.expect_out(ctx, TransferResponse.ACK, [])
                    done, error = await sie.do_in(
                        ctx, csw, TransferResponse.ACK)
                    result["done"] = done
                    result["error"] = error

                sim = Simulator(m)
                sim.add_clock(1 / 60e6, domain="usb")
                sim.add_testbench(tb)
                sim.run()

                self.assertEqual(result["done"], 1)
                self.assertEqual(result["error"], exp_err)

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


if __name__ == "__main__":
    unittest.main()
