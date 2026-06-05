import unittest
from amaranth import *
from amaranth.sim import Simulator

from top.sid_player.top import PlayTimerPeripheral


class PlayTimerTests(unittest.TestCase):
    def test_nmi_pulses_at_programmed_period(self):
        dut = PlayTimerPeripheral(clk_hz=1000)
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            # Enable + program period via the debug inputs exposed for test
            # (the control/period CSRs are write-only from a CSR master).
            ctx.set(dut.dbg_irq_enable, 1)
            ctx.set(dut.dbg_period, 20)
            ctx.set(dut.dbg_reset, 0)
            # period 20 → one NMI pulse every 20 cycles.
            pulses = 0
            for _ in range(45):
                await ctx.tick()
                if ctx.get(dut.nmi_pulse):
                    pulses += 1
            self.assertEqual(pulses, 2)  # two pulses in 45 cycles at period 20

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_no_pulse_when_disabled(self):
        dut = PlayTimerPeripheral(clk_hz=1000)
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            ctx.set(dut.dbg_irq_enable, 0)
            ctx.set(dut.dbg_period, 20)
            ctx.set(dut.dbg_reset, 0)
            for _ in range(45):
                await ctx.tick()
                self.assertEqual(ctx.get(dut.nmi_pulse), 0)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()


if __name__ == "__main__":
    unittest.main()
