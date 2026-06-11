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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("infile")
    ap.add_argument("outfile")
    ap.add_argument("--format", choices=["s24be", "s16le"], required=True)
    ap.add_argument("--rate", type=int, default=48000)
    args = ap.parse_args()

    if args.format == "s24be":
        x = read_s24be(args.infile)
    else:
        x = read_s16le(args.infile)

    w = wave.open(args.outfile, "wb")
    w.setnchannels(1)
    w.setsampwidth(2)
    w.setframerate(args.rate)
    w.writeframes(x.astype("<i2").tobytes())
    w.close()
    print(f"{args.outfile}: {len(x)} samples, {len(x)/args.rate:.3f} s @ {args.rate} Hz")


if __name__ == "__main__":
    main()
