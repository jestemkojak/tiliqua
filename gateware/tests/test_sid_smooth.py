# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""Unit test for the sid_player_sw VoiceSmoother (cheap scope low-pass)."""

import unittest

from amaranth import *
from amaranth.sim import Simulator

from amaranth_future import fixed
from tiliqua.raster import PSQ

from top.sid_player_sw.smooth import VoiceSmoother, LinearUpsampler


class VoiceSmootherTests(unittest.TestCase):

    def test_passes_dc_and_attenuates_high_freq(self):
        """Low-pass core (dc_block off): tracks DC, attenuates a Nyquist swing."""
        dut = VoiceSmoother(n_channels=1, k=4, poles=2, dc_block=False)
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

    def test_dc_block_removes_bias_keeps_swing(self):
        """dc_block on: a DC-biased square wave comes out centred on zero."""
        # Small k_dc so the DC tracker settles within the sim; the audio swing
        # is fast relative to it and survives.
        dut = VoiceSmoother(n_channels=1, k=2, poles=1, dc_block=True, k_dc=8)
        m = Module()
        m.submodules.dut = dut

        async def tb(ctx):
            ctx.set(dut.strobe, 1)
            bias, amp, period = 4000, 400, 16
            # Let the DC tracker lock onto the bias first.
            for n in range(30000):
                ctx.set(dut.i[0], bias + (amp if (n // period) % 2 else -amp))
                await ctx.tick()
            mx, mn = -1 << 30, 1 << 30
            for n in range(4000):
                ctx.set(dut.i[0], bias + (amp if (n // period) % 2 else -amp))
                await ctx.tick()
                v = ctx.get(dut.o[0])
                mx, mn = max(mx, v), min(mn, v)
            mid = (mx + mn) // 2
            assert abs(mid) < 60, f"DC bias not removed: midpoint {mid}"
            assert (mx - mn) > amp, f"swing lost: {mx - mn}"

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(tb)
        sim.run()


class LinearUpsamplerTests(unittest.TestCase):

    def test_fills_a_step_with_a_ramp(self):
        """A step input is emitted as ~n_up intermediate points (no big gaps)."""
        n_up = 8
        dut = LinearUpsampler(n_channels=1, n_up=n_up)  # PSQ
        m = Module()
        m.submodules.dut = dut

        # Frame 0 then a big jump to 0.5: the jump is what must be filled with
        # intermediate points rather than emitted as a single leap.
        lo, hi = fixed.Const(0.0, shape=PSQ), fixed.Const(0.5, shape=PSQ)
        frames = [lo, lo, hi, hi, hi, hi]

        async def tb(ctx):
            ctx.set(dut.o.ready, 1)
            ctx.set(dut.i.valid, 1)
            outs = []
            fi = 0
            for _ in range(n_up * 16):
                ctx.set(dut.i.payload[0], frames[fi])
                accepted = ctx.get(dut.i.valid & dut.i.ready)
                if ctx.get(dut.o.valid & dut.o.ready):
                    outs.append(ctx.get(dut.o.payload[0]).as_float())
                await ctx.tick()
                if accepted:
                    fi = min(fi + 1, len(frames) - 1)
            # The big 0->0.5 transition must appear as several rising steps,
            # each far smaller than the 0.5 leap (gaps filled).
            max_step = max((abs(b - a) for a, b in zip(outs, outs[1:])), default=0)
            assert max_step <= 0.5 / (n_up - 1) + 0.02, f"gap too large: {max_step}"
            assert max(outs) > 0.4, f"never reached target: {max(outs)}"

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(tb)
        sim.run()


if __name__ == "__main__":
    unittest.main()
