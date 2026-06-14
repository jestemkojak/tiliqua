#!/usr/bin/env python3
"""Convert a host_render raw audio dump to a 16-bit 48 kHz mono WAV (numpy-only).

The verilated harness (sid_api_sim.cpp) emits one of two raw formats:
  - mix tap : signed 24-bit big-endian  (audio_o, upstream path)
  - vN  tap : signed 16-bit little-endian (voiceN_dca_o, point-sampled)

Usage: raw2wav.py IN.raw OUT.wav --format {s24be,s16le} [--rate 48000]

Run with the repo venv python:
  /home/pawel/code/tiliqua/gateware/.venv/bin/python raw2wav.py ...
"""
import argparse
import wave

import numpy as np


def read_s24be(path):
    b = np.fromfile(path, dtype=np.uint8)
    n = len(b) // 3
    b = b[: n * 3].reshape(n, 3).astype(np.int32)
    # big-endian 24-bit -> signed int32
    x = (b[:, 0] << 16) | (b[:, 1] << 8) | b[:, 2]
    x = np.where(x & 0x800000, x - (1 << 24), x)
    # scale 24-bit -> 16-bit
    return (x >> 8).astype(np.int16)


def read_s16le(path):
    return np.fromfile(path, dtype="<i2")


def dc_block(x, a=0.9995):
    """One-pole high-pass (AC-couple): y[n] = x[n] - x[n-1] + a*y[n-1].

    The 6581 voice DCA taps (voiceN_dca_o) carry a model DC bias of ~half the
    dynamic range (VOICE_DC); the 8580's is 0. The hardware voice path is
    AC-coupled (codec), so the jacks/captures are DC-free. Removing the DC here
    makes a tap WAV directly comparable to a jack capture or a websid voice
    export — without it, abs()/RMS analysis is swamped by the +0.38-FS offset
    and per-note dynamics vanish (see the host_render spec, V4). a=0.9995 puts
    the corner at a few Hz @ 48 kHz (inaudible, preserves the envelope).
    """
    out = np.empty(len(x), dtype=np.float64)
    py = 0.0
    # Pre-charge the filter to the first sample: the 6581 VOICE_DC is already
    # present at sim start, and on hardware it is a standing offset, not a
    # step. Starting from px=0 turns it into a step whose high-pass response
    # is an audible ~30 ms thump at t=0 (a fake "glitch" on the voice tap).
    px = float(x[0]) if len(x) else 0.0
    for i in range(len(x)):
        py = x[i] - px + a * py
        px = x[i]
        out[i] = py
    return np.clip(np.rint(out), -32768, 32767).astype(np.int16)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("infile")
    ap.add_argument("outfile")
    ap.add_argument("--format", choices=["s24be", "s16le"], required=True)
    ap.add_argument("--rate", type=int, default=48000)
    ap.add_argument("--dc-block", action="store_true",
                    help="AC-couple the output (remove 6581 VOICE_DC bias) so a "
                         "voice tap is comparable to an AC-coupled jack capture.")
    args = ap.parse_args()

    if args.format == "s24be":
        x = read_s24be(args.infile)
    else:
        x = read_s16le(args.infile)

    if args.dc_block:
        x = dc_block(x.astype(np.float64))

    w = wave.open(args.outfile, "wb")
    w.setnchannels(1)
    w.setsampwidth(2)
    w.setframerate(args.rate)
    w.writeframes(x.astype("<i2").tobytes())
    w.close()
    print(f"{args.outfile}: {len(x)} samples, {len(x)/args.rate:.3f} s @ {args.rate} Hz")


if __name__ == "__main__":
    main()
