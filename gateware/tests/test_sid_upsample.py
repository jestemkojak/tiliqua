# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""Unit test for the sid_player_sw VoiceUpsampler (macro_osc scope-upsample port)."""

import sys
import unittest

from amaranth import *
from amaranth.sim import Simulator

from amaranth_future import fixed
from tiliqua.raster import PSQ

from top.sid_player_sw.upsample import VoiceUpsampler


class VoiceUpsamplerTests(unittest.TestCase):

    def test_upsamples_all_channels(self):
        """Feeding N input frames yields ~N*n_up output frames, channels kept distinct."""
        n_channels = 4
        n_up       = 4          # small factor keeps the sim fast; hardware uses 16
        n_in       = 40

        dut = VoiceUpsampler(n_channels=n_channels, n_up=n_up, fs_in=48000)
        m = Module()
        m.submodules.dut = dut

        # Distinct constant per channel so we can confirm channel separation.
        consts = [fixed.Const(0.1 * (c + 1), shape=PSQ) for c in range(n_channels)]

        async def stimulus_i(ctx):
            sent = 0
            ctx.set(dut.i.valid, 1)
            for c in range(n_channels):
                ctx.set(dut.i.payload[c], consts[c])
            while sent < n_in:
                if ctx.get(dut.i.valid & dut.i.ready):
                    sent += 1
                await ctx.tick()
            ctx.set(dut.i.valid, 0)

        async def testbench(ctx):
            ctx.set(dut.o.ready, 1)
            n_out = 0
            # Skip the first n_up * filter_order / n_up outputs while the
            # polyphase filter warms up from its zero initial state.
            n_warmup = n_up * 5   # ~filter_order / n_up * n_up = filter_order
            # Run long enough for the polyphase pipeline to flush.
            for _ in range(n_in * n_up * 64):
                if ctx.get(dut.o.valid & dut.o.ready):
                    n_out += 1
                    if n_out > n_warmup:
                        # Channel separation: each lane carries its own constant
                        # (allow filter ripple / transient with a loose tolerance).
                        for c in range(n_channels):
                            got = ctx.get(dut.o.payload[c]).as_float()
                            assert abs(got - consts[c].as_float()) < 0.15, \
                                f"ch{c} got {got}, want ~{consts[c].as_float()}"
                await ctx.tick()
            # ~n_up output frames per input frame (loose: ignores edge transients).
            assert n_out > n_in * n_up * 0.5, f"too few outputs: {n_out}"

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(stimulus_i)
        sim.add_testbench(testbench)
        sim.run()


if __name__ == "__main__":
    unittest.main()
