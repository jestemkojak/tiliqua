# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""Unit test for the sid_player_sw VoiceSmoother (cheap scope low-pass)."""

import unittest

from amaranth import *
from amaranth.sim import Simulator

from top.sid_player_sw.smooth import VoiceSmoother


class VoiceSmootherTests(unittest.TestCase):

    def test_passes_dc_and_attenuates_high_freq(self):
        """Output tracks a DC input but heavily attenuates a Nyquist-rate swing."""
        dut = VoiceSmoother(n_channels=1, k=4, poles=2)
        m = Module()
        m.submodules.dut = dut

        async def tb(ctx):
            ctx.set(dut.strobe, 1)  # one filter step per clock

            # DC: output converges to the input level.
            ctx.set(dut.i[0], 1000)
            for _ in range(4000):
                await ctx.tick()
            dc = ctx.get(dut.o[0])
            assert abs(dc - 1000) < 30, f"DC not tracked: {dc}"

            # High frequency: input flips sign every step (max rate). After the
            # cascade settles, the surviving swing must be a small fraction.
            amp = 1000
            mx, mn = -1 << 30, 1 << 30
            for n in range(4000):
                ctx.set(dut.i[0], amp if (n & 1) else -amp)
                await ctx.tick()
                if n > 2000:
                    v = ctx.get(dut.o[0])
                    mx = max(mx, v)
                    mn = min(mn, v)
            swing = mx - mn
            assert swing < amp // 2, f"high-freq not attenuated: swing {swing}"

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(tb)
        sim.run()


if __name__ == "__main__":
    unittest.main()
