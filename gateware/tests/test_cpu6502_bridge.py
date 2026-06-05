import unittest
from amaranth import *
from amaranth.sim import Simulator

from top.sid_player.top import Cpu6502


class Cpu6502Tests(unittest.TestCase):
    def test_cpu_fetches_reset_vector(self):
        dut = Cpu6502()
        m = Module()
        m.submodules.cpu = dut

        async def testbench(ctx):
            # Hold reset for a few cycles, supply 0xEA (NOP) on DI throughout.
            ctx.set(dut.RDY, 1)
            ctx.set(dut.DI, 0xEA)
            ctx.set(dut.reset, 1)
            for _ in range(4):
                await ctx.tick()
            ctx.set(dut.reset, 0)
            # After reset deasserts the CPU must drive AB and request the
            # reset vector ($FFFC/$FFFD) within a bounded number of cycles.
            saw_fffc = False
            for _ in range(16):
                await ctx.tick()
                if ctx.get(dut.AB) == 0xFFFC:
                    saw_fffc = True
                    break
            self.assertTrue(saw_fffc, "CPU never fetched reset vector low byte")

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()


from top.sid_player.top import Cpu6502Bridge


async def _to_rdy(ctx, dut, n):
    """Tick until the n-th cpu_RDY (advance) pulse, leaving the simulation
    positioned ON that pulse so window-stable outputs can be sampled.

    The windowed bridge freezes the CPU bus for a 64-cycle window and pulses
    cpu_RDY once per window on the advance cycle.  An access presented on the
    bus is *latched* at the first advance pulse and its effect — a committed
    BRAM/PSRAM write, a sid_w_en pulse, or settled read data on cpu_DI —
    appears at the *next* advance pulse (the one-window pipeline the arlet core
    expects).  So callers wait for the 2nd pulse to observe a write's effect.
    """
    seen = 0
    while True:
        if ctx.get(dut.cpu_RDY):
            seen += 1
            if seen == n:
                return
        await ctx.tick()


class Cpu6502BridgeTests(unittest.TestCase):
    def test_bram_write_then_read(self):
        dut = Cpu6502Bridge(psram_base_bytes=0x00800000)
        m = Module()
        m.submodules.bridge = dut

        async def testbench(ctx):
            # Write 0x42 to $0200 (scratch BRAM).  Hold the bus stable: the
            # write is latched at the 1st advance and commits at the 2nd.
            ctx.set(dut.cpu_AB, 0x0200)
            ctx.set(dut.cpu_DO, 0x42)
            ctx.set(dut.cpu_WE, 1)
            await _to_rdy(ctx, dut, 2)
            # Read back $0200.  cpu_DI is combinational from the window's
            # latched address, so it is settled by the 2nd read advance.
            ctx.set(dut.cpu_WE, 0)
            ctx.set(dut.cpu_AB, 0x0200)
            await ctx.tick()
            await _to_rdy(ctx, dut, 2)
            self.assertEqual(ctx.get(dut.cpu_DI), 0x42)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_sid_register_write(self):
        dut = Cpu6502Bridge(psram_base_bytes=0x00800000)
        m = Module()
        m.submodules.bridge = dut

        async def testbench(ctx):
            # Write 0xAB to $D405 (SID register 5).  sid_w_en pulses on the
            # advance that commits the latched write (the 2nd pulse).
            ctx.set(dut.cpu_AB, 0xD405)
            ctx.set(dut.cpu_DO, 0xAB)
            ctx.set(dut.cpu_WE, 1)
            await _to_rdy(ctx, dut, 2)
            self.assertEqual(ctx.get(dut.sid_w_en), 1)
            self.assertEqual(ctx.get(dut.sid_w_data), (0xAB << 5) | 0x05)
            # The pulse is one cycle wide.
            await ctx.tick()
            self.assertEqual(ctx.get(dut.sid_w_en), 0)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()


from amaranth.lib import wiring as _wiring
from tiliqua.test import psram as test_psram


class Cpu6502BridgePsramTests(unittest.TestCase):
    def _build(self):
        # psram_base_bytes=0 keeps word math simple
        bridge = Cpu6502Bridge(psram_base_bytes=0x0)
        fake = test_psram.FakePSRAM(addr_width=22, data_width=32,
                                    storage_words=4096, latency_cycles=4)
        m = Module()
        m.submodules.bridge = bridge
        m.submodules.fake = fake
        _wiring.connect(m, bridge.psram_bus, fake.bus)
        return m, bridge, fake

    def test_psram_rmw_preserves_adjacent_byte(self):
        m, bridge, fake = self._build()

        async def testbench(ctx):
            # Write 0x33 to byte $0002 (RMW into PSRAM word 0); latched at the
            # 1st advance, committed (read-modify-write completes) by the 2nd.
            ctx.set(bridge.cpu_WE, 1)
            ctx.set(bridge.cpu_AB, 0x0002)
            ctx.set(bridge.cpu_DO, 0x33)
            await _to_rdy(ctx, bridge, 2)
            # Write 0x77 to byte $0003 (RMW must preserve byte 2).
            ctx.set(bridge.cpu_AB, 0x0003)
            ctx.set(bridge.cpu_DO, 0x77)
            await ctx.tick()
            await _to_rdy(ctx, bridge, 2)
            # Read back byte $0002, expect preserved 0x33 (settled by 2nd pulse).
            ctx.set(bridge.cpu_WE, 0)
            ctx.set(bridge.cpu_AB, 0x0002)
            await ctx.tick()
            await _to_rdy(ctx, bridge, 2)
            self.assertEqual(ctx.get(bridge.cpu_DI), 0x33)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()


if __name__ == "__main__":
    unittest.main()
