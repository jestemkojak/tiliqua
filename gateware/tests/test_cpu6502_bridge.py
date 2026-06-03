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
from amaranth.lib.fifo import SyncFIFO


class Cpu6502BridgeTests(unittest.TestCase):
    def test_bram_write_then_read_single_cycle(self):
        sid_fifo = SyncFIFO(width=16, depth=16)
        dut = Cpu6502Bridge(sid_fifo=sid_fifo, psram_base_bytes=0x00800000)
        m = Module()
        m.submodules.bridge = dut
        m.submodules.sid_fifo = sid_fifo

        async def testbench(ctx):
            # Drive the bridge's CPU-facing side directly (no real CPU).
            # Write 0x42 to $0200 (scratch BRAM).
            ctx.set(dut.cpu_AB, 0x0200)
            ctx.set(dut.cpu_DO, 0x42)
            ctx.set(dut.cpu_WE, 1)
            await ctx.tick()
            # BRAM access must not stall the CPU.
            self.assertEqual(ctx.get(dut.cpu_RDY), 1)
            # Read it back.
            ctx.set(dut.cpu_WE, 0)
            ctx.set(dut.cpu_AB, 0x0200)
            await ctx.tick()
            self.assertEqual(ctx.get(dut.cpu_RDY), 1)
            self.assertEqual(ctx.get(dut.cpu_DI), 0x42)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_sid_register_write_pushes_fifo(self):
        sid_fifo = SyncFIFO(width=16, depth=16)
        dut = Cpu6502Bridge(sid_fifo=sid_fifo, psram_base_bytes=0x00800000)
        m = Module()
        m.submodules.bridge = dut
        m.submodules.sid_fifo = sid_fifo

        async def testbench(ctx):
            # Write 0xAB to $D405 (SID register 5).
            ctx.set(dut.cpu_AB, 0xD405)
            ctx.set(dut.cpu_DO, 0xAB)
            ctx.set(dut.cpu_WE, 1)
            await ctx.tick()
            # SID writes never stall.
            self.assertEqual(ctx.get(dut.cpu_RDY), 1)
            ctx.set(dut.cpu_WE, 0)
            await ctx.tick()
            # FIFO holds {data[15:5], addr[4:0]} = (0xAB << 5) | 5.
            self.assertEqual(ctx.get(sid_fifo.r_level), 1)
            self.assertEqual(ctx.get(sid_fifo.r_data), (0xAB << 5) | 0x05)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()


if __name__ == "__main__":
    unittest.main()
