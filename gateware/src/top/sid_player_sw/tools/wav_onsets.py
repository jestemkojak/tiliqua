#!/usr/bin/env python3
"""Onset-level comparison of two mono WAVs of the same SID voice (numpy-only).

Aligns globally, detects note onsets from the RMS envelope derivative, then for
a time window matches reference onsets to tiliqua onsets and reports missing /
extra / time-shifted notes. Built for the Commando fast-part investigation.

Usage: wav_onsets.py REF.wav TIL.wav T0 T1
"""
import sys, wave
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


def best_lag(a, b, max_lag):
    a = a - a.mean(); b = b - b.mean()
    n = min(len(a), len(b)); a, b = a[:n], b[:n]
    best, bl = -2.0, 0
    denom = np.linalg.norm(a) * np.linalg.norm(b) + 1e-12
    for L in range(-max_lag, max_lag + 1):
        c = (np.dot(a[L:], b[: n - L]) if L >= 0 else np.dot(a[: n + L], b[-L:])) / denom
        if c > best:
            best, bl = c, L
    return bl, best


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
            out.append(i); last = t
    return np.array(out)


def main():
    ref_p, til_p, t0, t1 = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4])
    ref, sr = read_wav(ref_p)
    til, sr2 = read_wav(til_p)
    assert sr == sr2
    hop_ms = 2.0
    hop_s = hop_ms / 1000.0
    er, et = envelope(ref, sr, hop_ms), envelope(til, sr, hop_ms)

    # global align on the 20ms envelope (cheap), refine on 2ms envelope
    e20r, e20t = envelope(ref, sr, 20.0), envelope(til, sr, 20.0)
    m = min(len(e20r), len(e20t))
    g, gc = best_lag(e20t[:m], e20r[:m], m // 4)
    coarse = g * 10  # 20ms hops -> 2ms hops
    f, fc = best_lag(et[max(0, coarse):coarse + 5000] if coarse >= 0 else et[:5000],
                     er[:5000], 100)
    lag = coarse + f
    print(f"global lag (til vs ref): {lag * hop_ms:.0f} ms  coarse-corr={gc:+.3f} fine-corr={fc:+.3f}")
    if lag > 0:
        et = et[lag:]
    elif lag < 0:
        er = er[-lag:]
    n = min(len(er), len(et)); er, et = er[:n], et[:n]

    # per-2s envelope lag drift across [t0, t1]
    blk = int(2.0 / hop_s); ml = int(0.100 / hop_s)
    print(f"\nper-2s envelope lag in [{t0:.0f},{t1:.0f}]s (±100ms search):")
    i0, i1 = int(t0 / hop_s), min(int(t1 / hop_s), n - blk)
    zr = (er - er.mean()) / (er.std() + 1e-9)
    zt = (et - et.mean()) / (et.std() + 1e-9)
    for s in range(i0, i1, blk):
        L, c = best_lag(zt[s:s + blk], zr[s:s + blk], ml)
        flag = "  <-- " if (abs(L * hop_ms) > 20 or c < 0.5) else ""
        print(f"  t={s * hop_s:6.1f}s  lag={L * hop_ms:+6.0f}ms  corr={c:+.3f}{flag}")

    # onset matching in [t0, t1]
    onr, ont = onsets(er, hop_s), onsets(et, hop_s)
    onr = onr[(onr * hop_s >= t0) & (onr * hop_s < t1)]
    ont_t = ont * hop_s
    tol = 0.030
    missing, matched, devs = [], 0, []
    used = np.zeros(len(ont), bool)
    for i in onr:
        t = i * hop_s
        d = np.abs(ont_t - t)
        j = int(np.argmin(d)) if len(d) else -1
        if j >= 0 and d[j] <= tol and not used[j]:
            used[j] = True; matched += 1; devs.append((ont_t[j] - t) * 1000)
        else:
            missing.append(t)
    ont_w = ont_t[(ont_t >= t0) & (ont_t < t1)]
    extra = len(ont_w) - matched
    devs = np.array(devs)
    print(f"\nonsets in window: ref={len(onr)} til={len(ont_w)} matched={matched} "
          f"missing(til)={len(missing)} extra(til)={extra}")
    if len(devs):
        print(f"matched-onset deviation: mean={devs.mean():+.1f}ms sd={devs.std():.1f}ms "
              f"p95={np.percentile(np.abs(devs), 95):.1f}ms max={np.abs(devs).max():.1f}ms")
    if missing:
        print("missing-in-tiliqua onset times (s): " +
              " ".join(f"{t:.2f}" for t in missing[:40]))
    # tiliqua onsets with no reference match (extra notes / regressions)
    extra_t = [t for j, t in enumerate(ont_t) if t0 <= t < t1 and not used[j]]
    if extra_t:
        print("tiliqua-only onset times (s): " +
              " ".join(f"{t:.2f}" for t in extra_t[:40]))


if __name__ == "__main__":
    main()
