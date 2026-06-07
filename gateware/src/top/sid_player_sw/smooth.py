# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""Cheap per-voice low-pass smoother for the SID voice scope.

The three SID voice taps (`voiceN_dca`) are reSID outputs updated once per phi2
(~1MHz). The scope point-samples them at the 48kHz codec rate with no anti-alias
filter, so all voice content above 24kHz folds back as broadband scatter — the
voice traces render as dot-clouds instead of lines (see docs/SID_PLAYER.md).

The scope is a *visualisation*, not a measurement, so we don't need the accurate
polyphase FIR the audio path uses (top/sid/audio.py). A multi-pole leaky
integrator running at the ~1MHz strobe is enough to kill the >24kHz energy that
causes the scatter, and it costs only adders + shifts (no multipliers, no BRAM):

    acc += x - (acc >> k)      # per pole; output y = acc >> k -> x at DC

`k` sets the cutoff (~ f_strobe / (2*pi*2^k)); cascading `poles` of them steepens
the rolloff. This is applied ONLY on the scope branch — the audio outputs keep
reading the raw `voiceN_dca`, so sound is unaffected.
"""

from amaranth import *
from amaranth.lib import data, stream, wiring
from amaranth.lib.wiring import In, Out

from tiliqua.raster import PSQ


class VoiceSmoother(wiring.Component):
    """N-channel cascaded leaky-integrator low-pass, stepped on `strobe`.

    Pure adder/shift datapath: each channel is `poles` one-pole sections in
    series. Inputs/outputs are plain (combinational) values, not streams — the
    scope point-samples the held output whenever it likes, which is harmless now
    that the high-frequency content is gone.

    When `dc_block` is set, a final very-slow leaky integrator estimates the DC
    bias and subtracts it (AC-couple). The SID voice DCA taps carry a large
    constant offset (e.g. 6581 `VOICE_DC` = 1/2 the dynamic range), which would
    otherwise sit the trace at half-scale and wrap the AC peaks. `k_dc` sets the
    DC-tracking cutoff (~ f_strobe / (2*pi*2^k_dc)); it must be well below the
    audio band so only the constant bias is removed, not the waveform.
    """

    def __init__(self, *, n_channels=3, shape=signed(16), k=5, poles=2,
                 dc_block=True, k_dc=14):
        self.n_channels = n_channels
        self.shape      = shape
        self.k          = k
        self.poles      = poles
        self.dc_block   = dc_block
        self.k_dc       = k_dc
        super().__init__({
            "strobe": In(1),
            "i":      In(data.ArrayLayout(shape, n_channels)),
            "o":      Out(data.ArrayLayout(shape, n_channels)),
        })

    def elaborate(self, platform):
        m = Module()
        width = Shape.cast(self.shape).width
        for ch in range(self.n_channels):
            x = self.i[ch]
            for p in range(self.poles):
                # acc holds the value scaled by 2^k so the output (acc>>k) has no
                # steady-state dead zone: at DC acc settles to x<<k exactly.
                acc = Signal(signed(width + self.k), name=f"acc{ch}_{p}")
                y   = Signal(self.shape,             name=f"y{ch}_{p}")
                m.d.comb += y.eq(acc >> self.k)
                with m.If(self.strobe):
                    m.d.sync += acc.eq(acc + (x - y))
                x = y
            if self.dc_block:
                # Slow leaky integrator = DC estimate; subtract it to AC-couple.
                dc_acc = Signal(signed(width + self.k_dc), name=f"dcacc{ch}")
                dc     = Signal(self.shape,                 name=f"dc{ch}")
                m.d.comb += dc.eq(dc_acc >> self.k_dc)
                with m.If(self.strobe):
                    m.d.sync += dc_acc.eq(dc_acc + (x - dc))
                ac = Signal(self.shape, name=f"ac{ch}")
                m.d.comb += ac.eq(x - dc)
                x = ac
            m.d.comb += self.o[ch].eq(x)
        return m


class LinearUpsampler(wiring.Component):
    """Linearly interpolate `n_up` frames between consecutive input frames.

    The scope rasterizer plots one point per sample and does not connect
    consecutive points, so steep edges leave vertical gaps ("dotted on the
    vertical axis") when fed at the raw 48kHz frame rate. This fills those gaps
    by emitting `n_up` points stepping linearly from the previous frame to the
    new one — i.e. it draws the connecting line.

    Cheap on purpose (for a visualisation): with `n_up` a power of two the
    per-step increment is `(new - prev) >> log2(n_up)`, so the datapath is just
    subtract / shift / accumulate per channel — no multipliers, no BRAM. Output
    rate is `fs_in * n_up`, so the consumer's timebase (`scope_periph.fs`) must
    be scaled by `n_up` to match.
    """

    def __init__(self, *, n_channels=4, n_up=16, shape=PSQ):
        assert n_up >= 1 and (n_up & (n_up - 1)) == 0, "n_up must be a power of two"
        self.n_channels = n_channels
        self.n_up       = n_up
        self.shape      = shape
        self.k          = (n_up - 1).bit_length()  # log2(n_up)
        super().__init__({
            "i": In(stream.Signature(data.ArrayLayout(shape, n_channels))),
            "o": Out(stream.Signature(data.ArrayLayout(shape, n_channels))),
        })

    def elaborate(self, platform):
        m = Module()
        width = Shape.cast(self.shape).width
        target = [Signal(signed(width), name=f"target{c}") for c in range(self.n_channels)]
        delta  = [Signal(signed(width), name=f"delta{c}")  for c in range(self.n_channels)]
        acc    = [Signal(signed(width), name=f"acc{c}")    for c in range(self.n_channels)]
        cnt = Signal(range(self.n_up + 1))

        with m.If(cnt == 0):
            # Ready for a new frame: latch it, prime the interpolation from the
            # previous frame (held in `target`) toward the new one.
            m.d.comb += self.i.ready.eq(1)
            with m.If(self.i.valid):
                for c in range(self.n_channels):
                    nv = self.i.payload[c].as_value()
                    m.d.sync += [
                        delta[c].eq((nv - target[c]) >> self.k),
                        acc[c].eq(target[c]),
                        target[c].eq(nv),
                    ]
                m.d.sync += cnt.eq(self.n_up)
        with m.Else():
            # Emit n_up interpolated points (acc += delta each accepted step).
            m.d.comb += self.o.valid.eq(1)
            for c in range(self.n_channels):
                m.d.comb += self.o.payload[c].as_value().eq(acc[c])
            with m.If(self.o.ready):
                for c in range(self.n_channels):
                    m.d.sync += acc[c].eq(acc[c] + delta[c])
                m.d.sync += cnt.eq(cnt - 1)

        return m
