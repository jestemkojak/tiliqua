#!/usr/bin/env python3
"""Per-note peak-level table for a mono SID-voice WAV (numpy-only).

Given a recording, the tune's gate-on frame numbers, and the recording's frame
rate, this: detects onsets (envelope-derivative method, like wav_onsets.py),
fits the recording's time offset t0 so model frame anchors line up with real
onsets, then measures each note's peak |x| in a fixed window after its gate-on
and classifies it loud/soft against the (log) midpoint of the per-note peaks.

The final "class string" (e.g. LSLLSL...) is meant for cheap diffing between
two recordings of the same tune (hardware vs websid vs host-render).

Usage:
  note_peaks.py REC.wav --frames 3200:3500 --fps 50.0 --gate-frames FILE

FILE has one gate-on frame number per line (e.g. from commando_gate_trace_55s).
--frames bounds which gate frames are used (inclusive a, exclusive b).
"""
import argparse
import wave

import numpy as np


def read_wav(path):
    w = wave.open(path, "rb")
    sr = w.getframerate()
    x = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").astype(np.float64)
    if w.getnchannels() > 1:
        x = x.reshape(-1, w.getnchannels()).mean(axis=1)
    w.close()
    return x / 32768.0, sr


def envelope(x, sr, hop_ms=2.0):
    w = max(1, int(sr * hop_ms / 1000))
    n = len(x) // w
    return np.sqrt((x[: n * w].reshape(n, w) ** 2).mean(axis=1) + 1e-12)


def onsets(env, hop_s, thresh_db=8.0, floor=3e-3, refractory_s=0.040):
    """Indices (in env hops) where the envelope jumps by >thresh_db within 8ms."""
    db = 20 * np.log10(env + 1e-9)
    look = max(1, int(0.008 / hop_s))
    rise = db[look:] - db[:-look]
    cand = np.where((rise > thresh_db) & (env[look:] > floor))[0] + look
    out, last = [], -1e9
    for i in cand:
        t = i * hop_s
        if t - last >= refractory_s:
            out.append(i)
            last = t
    return np.array(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("wav")
    ap.add_argument("--frames", required=True, help="a:b window of gate frames (inclusive a, exclusive b)")
    ap.add_argument("--fps", type=float, required=True, help="recording frame rate (e.g. 50.0 or 50.125)")
    ap.add_argument("--gate-frames", required=True, help="file with one gate-on frame number per line")
    args = ap.parse_args()

    a_str, b_str = args.frames.split(":")
    fa, fb = int(a_str), int(b_str)

    gate_frames = []
    with open(args.gate_frames) as fh:
        for line in fh:
            line = line.strip()
            if line:
                gate_frames.append(int(line))
    gate_frames = [f for f in gate_frames if fa <= f < fb]
    gate_frames.sort()
    if not gate_frames:
        print("no gate frames in window")
        return

    x, sr = read_wav(args.wav)
    hop_ms = 2.0
    hop_s = hop_ms / 1000.0
    env = envelope(x, sr, hop_ms)
    on_idx = onsets(env, hop_s)
    on_t = on_idx * hop_s
    if len(on_t) == 0:
        print("no onsets detected — t0 fit impossible")
        return

    # --- fit recording time offset t0 ---------------------------------------
    # Model places gate frame f at wall-clock f/fps + t0. Estimate t0 as the
    # median over gate frames of (nearest detected onset time - f/fps).
    deltas = []
    for f in gate_frames:
        t_nominal = f / args.fps
        # nearest onset (search around a generous range so a bad initial guess
        # of t0 doesn't bias the fit — onsets are sparse so nearest is robust)
        j = int(np.argmin(np.abs(on_t - (t_nominal))))
        deltas.append(on_t[j] - t_nominal)
    # robust: take median, then refit nearest-onset using that t0 to discard
    # gate frames whose nearest onset is an outlier (>50ms), then re-median.
    t0 = float(np.median(deltas))
    refined = []
    for f in gate_frames:
        t_model = f / args.fps + t0
        j = int(np.argmin(np.abs(on_t - t_model)))
        d = on_t[j] - t_model
        if abs(d) <= 0.050:
            refined.append(on_t[j] - f / args.fps)
    if refined:
        t0 = float(np.median(refined))
    print(f"# {args.wav}")
    print(f"# fps={args.fps}  frames={fa}:{fb}  gate_notes={len(gate_frames)}  "
          f"onsets={len(on_t)}  t0={t0 * 1000:.1f}ms  (matched={len(refined)})")

    # --- measure per-note peak in [on+5ms, on+70ms] -------------------------
    rows = []
    for f in gate_frames:
        t_model = f / args.fps + t0
        i0 = int((t_model + 0.005) * sr)
        i1 = int((t_model + 0.070) * sr)
        i0 = max(0, i0)
        i1 = min(len(x), i1)
        if i1 <= i0:
            peak = 0.0
        else:
            peak = float(np.abs(x[i0:i1]).max())
        rows.append((f, t_model, peak))

    peaks = np.array([r[2] for r in rows])
    pos = peaks[peaks > 0]
    if len(pos) == 0:
        print("all peaks zero — degenerate")
        return
    # log midpoint between max and min peak
    lo = np.log(pos.min())
    hi = np.log(peaks.max())
    mid = (lo + hi) / 2.0
    thresh = np.exp(mid)

    print(f"# loud/soft threshold (log-midpoint) = {thresh:.4f}  "
          f"[min={pos.min():.4f} max={peaks.max():.4f}]")
    print(f"{'frame':>6} {'t_model(s)':>11} {'peak':>9}  class")
    cls = []
    for f, t_model, peak in rows:
        c = "L" if peak >= thresh else "S"
        cls.append(c)
        print(f"{f:>6} {t_model:>11.3f} {peak:>9.4f}  {c}")

    class_str = "".join(cls)
    print(f"\nclass string: {class_str}")


if __name__ == "__main__":
    main()
