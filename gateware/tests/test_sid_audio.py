# Copyright (c) 2026 Tiliqua contributors
#
# SPDX-License-Identifier: CERN-OHL-S-2.0

"""
Anti-alias decimation for the SID audio output.

The SID core produces one audio sample per phi2 cycle (~1MHz). The audio codec
samples at 48kHz. Without an anti-alias filter, that ~21x downsample folds all
SID spectral content above 24kHz into the audible band (broadband 'grit', the
audible difference vs a reSID reference). ``AudioDecimator`` band-limits the
1MHz stream with a polyphase FIR before producing the 48kHz output.
"""

import math
import sys
import unittest

from amaranth import *
from amaranth.sim import Simulator

from amaranth_future import fixed
from tiliqua.dsp import ASQ
from tiliqua.test import stream

from top.sid.audio import AudioDecimator


def _measure_tone_rms(freq_hz, fs_in=1_000_000, amp=0.4, n_in=4000, warmup=1200):
    """Drive AudioDecimator with a sine at ``freq_hz`` and return the RMS of its
    (held) output over the post-warmup window."""
    m = Module()
    m.submodules.dut = dut = AudioDecimator(fs_in=fs_in, fs_out=48_000)

    collected = []

    async def stimulus_i(ctx):
        for n in range(n_in):
            v = amp * math.sin(2 * math.pi * freq_hz * n / fs_in)
            await stream.put(ctx, dut.i, fixed.Const(v, shape=ASQ))

    async def testbench(ctx):
        # Sample the held output once per input-sample period (~fs_in cadence).
        ticks_per_in = 0
        for n in range(n_in):
            for _ in range(int(60e6 // fs_in)):  # ~sync cycles per input sample
                await ctx.tick()
            if n >= warmup:
                collected.append(ctx.get(dut.o).as_float())

    sim = Simulator(m)
    sim.add_clock(1 / 60e6)
    sim.add_process(stimulus_i)
    sim.add_testbench(testbench)
    sim.run()

    if not collected:
        return 0.0
    return math.sqrt(sum(v * v for v in collected) / len(collected))


class SidAudioTests(unittest.TestCase):

    def test_passband_tone_survives(self):
        """A 1kHz tone (well within the audio band) passes ~unattenuated."""
        rms = _measure_tone_rms(1_000)
        # input amp 0.4 -> expected RMS ~0.28; allow filter/quantization margin.
        assert rms > 0.15, f"1kHz tone unexpectedly attenuated: rms={rms:.4f}"

    def test_aliasing_tone_rejected(self):
        """A 100kHz tone would alias to 4kHz under naive 48kHz sampling; the
        anti-alias FIR must reject it so it never reaches the output."""
        rms_pass = _measure_tone_rms(1_000)
        rms_alias = _measure_tone_rms(100_000)
        assert rms_alias < 0.25 * rms_pass, (
            f"100kHz tone not rejected: alias_rms={rms_alias:.4f} "
            f"passband_rms={rms_pass:.4f}")


if __name__ == "__main__":
    unittest.main()
