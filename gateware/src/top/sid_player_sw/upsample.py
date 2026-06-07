# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""N-channel polyphase upsampler for the SID voice scope.

Direct port of the macro_osc scope-upsample stage (macro_osc/top.py:270-284),
generalized from 2 to N channels. Splits an N-channel frame stream into N
independent streams, runs each through a dsp.Resample interpolator (n_up:1),
and re-merges into an N-channel frame stream at fs_in * n_up. Used so the
sid_player_sw scope renders continuous traces instead of dotted ones.
"""

from amaranth import *
from amaranth.lib import data, stream, wiring
from amaranth.lib.wiring import In, Out

from tiliqua import dsp
from tiliqua.raster import PSQ


class VoiceUpsampler(wiring.Component):
    """Upsample an N-channel frame stream by `n_up` (polyphase, per channel)."""

    def __init__(self, *, n_channels=4, n_up=16, fs_in, shape=PSQ):
        self.n_channels = n_channels
        self.n_up       = n_up
        self.fs_in      = fs_in
        self.shape      = shape
        super().__init__({
            "i": In(stream.Signature(data.ArrayLayout(shape, n_channels))),
            "o": Out(stream.Signature(data.ArrayLayout(shape, n_channels))),
        })

    def elaborate(self, platform):
        m = Module()

        m.submodules.split = split = dsp.Split(
            n_channels=self.n_channels, shape=self.shape)
        m.submodules.merge = merge = dsp.Merge(
            n_channels=self.n_channels, shape=self.shape)

        # Boundary streams: self.i/self.o are this component's ports, so flip
        # them to connect into the child submodules.
        wiring.connect(m, wiring.flipped(self.i), split.i)
        wiring.connect(m, merge.o, wiring.flipped(self.o))

        for ch in range(self.n_channels):
            r = dsp.Resample(
                fs_in=self.fs_in, n_up=self.n_up, m_down=1, shape=self.shape)
            setattr(m.submodules, f"resample{ch}", r)
            wiring.connect(m, split.o[ch], r.i)
            wiring.connect(m, r.o, merge.i[ch])

        return m
