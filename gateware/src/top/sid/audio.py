# Copyright (c) 2026 Tiliqua contributors
#
# SPDX-License-Identifier: CERN-OHL-S-2.0

"""
Anti-alias decimation for the SID audio output path.

The reSID core (``deps/sid``) produces one audio sample per phi2 cycle (~1MHz).
The audio codec runs far slower (48kHz). Point-sampling the 1MHz stream at 48kHz
(as the original ``last_audio_*`` wiring did) is a zero-order-hold decimation
with no anti-alias filter, so all SID spectral content above 24kHz folds back
into the audible band as broadband 'grit' — the audible difference versus a
software-reSID reference, which resamples with a proper FIR.

``AudioDecimator`` reproduces that resampling: a polyphase FIR (``dsp.Resample``)
band-limits the ~1MHz sample stream and emits it at the codec rate. A small input
FIFO absorbs the FIR's MAC-burst backpressure (it holds ``i.ready`` low for ~N
cycles while computing an output, during which one or two 1MHz samples can
arrive).
"""

import math

from amaranth import *
from amaranth.lib import stream, wiring
from amaranth.lib.wiring import In, Out

from tiliqua import dsp
from tiliqua.dsp import ASQ


class AudioDecimator(wiring.Component):

    """
    Band-limit and decimate a high-rate SID audio stream to the codec rate.

    Members
    -------
    i : :py:`In(stream.Signature(shape))`
        Input samples at ``fs_in`` (one SID output sample per phi2 cycle).
    o : :py:`Out(shape)`
        Held, band-limited output sample (updated at ``fs_out``). Not a stream:
        the codec point-samples it whenever it likes, which is now harmless
        because the signal is already band-limited below the codec Nyquist.
    """

    def __init__(self, fs_in: int = 1_000_000, fs_out: int = 48_000,
                 fifo_depth: int = 8, shape=ASQ):
        gcd = math.gcd(fs_out, fs_in)
        self.fs_in      = fs_in
        self.fs_out     = fs_out
        self.n_up       = fs_out // gcd
        self.m_down     = fs_in // gcd
        self.fifo_depth = fifo_depth
        self.shape      = shape
        super().__init__({
            "i": In(stream.Signature(shape)),
            "o": Out(shape),
        })

    def elaborate(self, platform):
        m = Module()

        # Absorb FIR MAC-burst backpressure so no 1MHz input sample is dropped.
        m.submodules.fifo = fifo = dsp.SyncFIFOBuffered(
            shape=self.shape, depth=self.fifo_depth)
        m.submodules.resample = resample = dsp.Resample(
            fs_in=self.fs_in, n_up=self.n_up, m_down=self.m_down, shape=self.shape)

        wiring.connect(m, wiring.flipped(self.i), fifo.i)
        wiring.connect(m, fifo.o, resample.i)

        # Always accept resampler output; hold the latest band-limited sample.
        m.d.comb += resample.o.ready.eq(1)
        with m.If(resample.o.valid):
            m.d.sync += self.o.eq(resample.o.payload)

        return m
