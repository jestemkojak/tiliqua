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


if __name__ == "__main__":
    unittest.main()
