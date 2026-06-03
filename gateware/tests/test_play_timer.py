import unittest
from amaranth import *
from amaranth.sim import Simulator

from top.sid_player.top import PlayTimerPeripheral


class PlayTimerTests(unittest.TestCase):
    def test_nmi_pulses_at_50hz_when_enabled(self):
        # Use a tiny divider override so the test runs fast.
        dut = PlayTimerPeripheral(clk_hz=1000, rate_hz_pal=50, rate_hz_ntsc=60)
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            # Enable, PAL rate, out of reset (drive internal control directly via
            # the debug inputs exposed for test).
            ctx.set(dut.dbg_irq_enable, 1)
            ctx.set(dut.dbg_play_rate, 0)
            ctx.set(dut.dbg_reset, 0)
            # 1000 Hz clk / 50 Hz = 20 cycles per NMI pulse.
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
        dut = PlayTimerPeripheral(clk_hz=1000, rate_hz_pal=50, rate_hz_ntsc=60)
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            ctx.set(dut.dbg_irq_enable, 0)
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
