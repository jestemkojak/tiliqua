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


class Cpu6502BridgeTests(unittest.TestCase):
    def test_bram_write_then_read(self):
        dut = Cpu6502Bridge(psram_base_bytes=0x00800000)
        m = Module()
        m.submodules.bridge = dut

        async def testbench(ctx):
            # Drive the bridge's CPU-facing side directly (no real CPU).
            # Write 0x42 to $0200 (scratch BRAM).  Writes ack one cycle later
            # from WRITE-ACK (keeps cpu_RDY out of the combinational address
            # path); after one tick the FSM is in WRITE-ACK with RDY=1.
            ctx.set(dut.cpu_AB, 0x0200)
            ctx.set(dut.cpu_DO, 0x42)
            ctx.set(dut.cpu_WE, 1)
            await ctx.tick()
            self.assertEqual(ctx.get(dut.cpu_RDY), 1)
            # Read it back.  After the WRITE-ACK cycle the BRAM read stalls one
            # more cycle for the di_r pipeline (BRAM-WAIT) — poll RDY for both.
            ctx.set(dut.cpu_WE, 0)
            ctx.set(dut.cpu_AB, 0x0200)
            await ctx.tick()
            while not ctx.get(dut.cpu_RDY):
                await ctx.tick()
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
            # Write 0xAB to $D405 (SID register 5).
            ctx.set(dut.cpu_AB, 0xD405)
            ctx.set(dut.cpu_DO, 0xAB)
            ctx.set(dut.cpu_WE, 1)
            await ctx.tick()
            # SID write acks from WRITE-ACK after one tick; sid_w_en pulses for
            # that single cycle with the latched sid_w_data.
            self.assertEqual(ctx.get(dut.cpu_RDY), 1)
            self.assertEqual(ctx.get(dut.sid_w_en), 1)
            self.assertEqual(ctx.get(dut.sid_w_data), (0xAB << 5) | 0x05)
            ctx.set(dut.cpu_WE, 0)
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
            # Write 0x33 to byte $0002 (RMW into word 0).
            ctx.set(bridge.cpu_WE, 1)
            ctx.set(bridge.cpu_AB, 0x0002)
            ctx.set(bridge.cpu_DO, 0x33)
            # Wait for the write to complete (RDY returns high).
            for _ in range(64):
                await ctx.tick()
                if ctx.get(bridge.cpu_RDY) == 1:
                    break
            self.assertEqual(ctx.get(bridge.cpu_RDY), 1)
            # Write 0x77 to byte $0003 (RMW must preserve byte 2).
            ctx.set(bridge.cpu_AB, 0x0003)
            ctx.set(bridge.cpu_DO, 0x77)
            await ctx.tick()
            for _ in range(64):
                await ctx.tick()
                if ctx.get(bridge.cpu_RDY) == 1:
                    break
            # Read back byte $0002, expect preserved 0x33.
            ctx.set(bridge.cpu_WE, 0)
            ctx.set(bridge.cpu_AB, 0x0002)
            await ctx.tick()
            for _ in range(64):
                await ctx.tick()
                if ctx.get(bridge.cpu_RDY) == 1:
                    break
            self.assertEqual(ctx.get(bridge.cpu_DI), 0x33)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()


if __name__ == "__main__":
    unittest.main()
