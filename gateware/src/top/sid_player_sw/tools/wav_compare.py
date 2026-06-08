#!/usr/bin/env python3
"""Compare two mono WAV recordings of the same SID tune (reference vs tiliqua).

Goal: gather EVIDENCE for sid_player_sw audio root-cause debugging. Given a
"good" reference render (e.g. websid) and a tiliqua capture, quantify *how* they
differ so we can tell glitch classes apart:

  - skipped/dropped frames  -> local time-warp; cross-correlation lag drifts,
    short windows align at shifting offsets, envelope correlation < 1.
  - speed / play-rate error -> a single global resample factor aligns them.
  - aliasing / filter timbre -> envelopes align but high-freq spectra differ.
  - startup transient        -> first ~second diverges, rest matches.

Outputs numbers to stdout + a `wav_zoom.png` (waveform zoom + envelope + per-
window lag) next to the tiliqua file.

Usage: wav_compare.py REFERENCE.wav TILIQUA.wav [--out dir]
"""
import sys, os, argparse, wave
import numpy as np
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt


def read_wav(path):
    w = wave.open(path, "rb")
    sr = w.getframerate()
    n = w.getnframes()
    ch = w.getnchannels()
    raw = w.readframes(n)
    w.close()
    x = np.frombuffer(raw, dtype="<i2").astype(np.float64)
    if ch > 1:
        x = x.reshape(-1, ch).mean(axis=1)
    x /= 32768.0
    return x, sr


def envelope(x, sr, win_ms=20.0):
    """RMS envelope at win_ms hop — robust to phase, tracks musical dynamics."""
    w = max(1, int(sr * win_ms / 1000))
    n = len(x) // w
    e = np.sqrt((x[: n * w].reshape(n, w) ** 2).mean(axis=1) + 1e-12)
    t = np.arange(n) * win_ms / 1000.0
    return t, e


def best_lag(a, b, max_lag):
    """Integer lag (samples) maximising normalised cross-correlation of a,b."""
    a = a - a.mean()
    b = b - b.mean()
    n = min(len(a), len(b))
    a, b = a[:n], b[:n]
    lags = np.arange(-max_lag, max_lag + 1)
    best, bl = -2.0, 0
    denom = (np.linalg.norm(a) * np.linalg.norm(b)) + 1e-12
    for L in lags:
        if L >= 0:
            c = np.dot(a[L:], b[: n - L])
        else:
            c = np.dot(a[: n + L], b[-L:])
        c /= denom
        if c > best:
            best, bl = c, L
    return bl, best


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("reference")
    ap.add_argument("tiliqua")
    ap.add_argument("--out", default=None, help="output dir for wav_zoom.png")
    args = ap.parse_args()

    ref, sr_r = read_wav(args.reference)
    til, sr_t = read_wav(args.tiliqua)
    assert sr_r == sr_t, f"sample-rate mismatch {sr_r} vs {sr_t}"
    sr = sr_r
    print(f"reference: {len(ref)/sr:7.2f}s  {os.path.basename(args.reference)}")
    print(f"tiliqua  : {len(til)/sr:7.2f}s  {os.path.basename(args.tiliqua)}")

    # --- global alignment ---------------------------------------------------
    # 1) coarse on envelope (cheap, robust to phase), then 2) sample-accurate
    #    refine on the raw waveform near the coarse estimate.
    _, er = envelope(ref, sr)
    _, et = envelope(til, sr)
    m = min(len(er), len(et))
    g_env, g_corr = best_lag(et[:m], er[:m], max_lag=m // 4)
    coarse = g_env * int(sr * 0.020)  # env-hop -> samples
    print(f"\nGLOBAL envelope alignment: {g_env} env-hops "
          f"({g_env*20} ms)  corr={g_corr:+.3f}")
    # refine sample-accurate on a clean steady chunk around 10s
    s = sr * 10
    seg_t = til[s:s+sr]; seg_r = ref[s+ (-coarse if coarse<0 else 0): ]
    fine, fcorr = best_lag(til[s:s+sr], ref[s:s+sr], int(sr*0.030))
    g_lag = coarse + fine
    print(f"GLOBAL sample lag (tiliqua vs ref): {g_lag} samp "
          f"({g_lag/sr*1000:+.1f} ms)  refine-corr={fcorr:+.3f}")
    # apply the global shift so per-window analysis starts aligned.
    # positive g_lag => tiliqua trails reference => drop g_lag from front of til
    if g_lag > 0:
        til = til[g_lag:]; ref = ref[:len(til)]
    elif g_lag < 0:
        ref = ref[-g_lag:]; til = til[:len(ref)]
    n = min(len(ref), len(til)); ref = ref[:n]; til = til[:n]

    # --- per-block ENVELOPE lag tracking (detect drift / dropped frames) ----
    # Raw-waveform xcorr is useless on a single tonal voice (phase slides with
    # any pitch error). The RMS envelope is phase-robust: it tracks note onsets
    # and dynamics. Dropped play-frames make the tiliqua tune fall progressively
    # behind => the envelope lag DRIFTS monotonically across the tune.
    ehop_ms = 5.0
    _, ren = envelope(ref, sr, ehop_ms)
    _, ten = envelope(til, sr, ehop_ms)
    ren = (ren - ren.mean()) / (ren.std() + 1e-9)   # normalise away the 3x gain
    ten = (ten - ten.mean()) / (ten.std() + 1e-9)
    ehz = 1000.0 / ehop_ms                            # env samples / s
    blk = int(ehz * 4.0)                              # 4s blocks
    mlag = int(ehz * 0.250)                           # +-250ms search
    n_win = min(len(ren), len(ten)) // blk
    times, lags, corrs = [], [], []
    for i in range(n_win):
        s = i * blk
        L, c = best_lag(ten[s : s + blk], ren[s : s + blk], mlag)
        times.append(s / ehz)
        lags.append(L / ehz * 1000.0)                # ms
        corrs.append(c)
    times = np.array(times); lags = np.array(lags); corrs = np.array(corrs)
    print(f"\nper-4s envelope xcorr  (N={n_win}, ±250ms search):")
    print(f"  correlation  min={corrs.min():+.3f}  mean={corrs.mean():+.3f}  "
          f"median={np.median(corrs):+.3f}")
    print(f"  lag(ms)      first={lags[0]:+.0f}  last={lags[-1]:+.0f}  "
          f"min={lags.min():+.0f}  max={lags.max():+.0f}  "
          f"net drift={lags[-1]-lags[0]:+.0f}ms")
    # monotonic drift => steady frame loss; jumps => discrete dropouts
    dl = np.diff(lags)
    print(f"  lag steps>50ms (discrete dropouts): {int((np.abs(dl)>50).sum())}"
          + (f"  @ {times[1:][np.abs(dl)>50][:8].round(0)} s" if (np.abs(dl)>50).any() else ""))
    bad = np.where(corrs < 0.5)[0]
    print(f"  blocks with env-corr<0.5: {len(bad)}/{n_win}"
          + (f"  @ {times[bad][:8].round(0)} s" if len(bad) else ""))

    # --- startup divergence -------------------------------------------------
    head = sr * 2
    L0, c0 = best_lag(til[:head], ref[:head], mlag)
    Lq, cq = best_lag(til[sr*10:sr*12], ref[sr*10:sr*12], mlag)
    print(f"\nstartup  (0-2s):  corr={c0:+.3f} lag={L0/sr*1000:+.1f}ms")
    print(f"steady  (10-12s): corr={cq:+.3f} lag={Lq/sr*1000:+.1f}ms")

    # --- spectra (full-tune averaged magnitude) -----------------------------
    def avg_spec(x):
        N = 8192
        k = len(x) // N
        acc = np.zeros(N // 2 + 1)
        win = np.hanning(N)
        for i in range(k):
            seg = x[i*N:(i+1)*N] * win
            acc += np.abs(np.fft.rfft(seg))
        acc /= max(k, 1)
        f = np.fft.rfftfreq(N, 1/sr)
        return f, acc
    fr, sr_spec = avg_spec(ref)
    ft, st_spec = avg_spec(til)
    # normalise each spectrum to unit total energy so the (unmatched) recording
    # gain cancels and only spectral SHAPE differences remain.
    sr_n = sr_spec / (sr_spec.sum() + 1e-9)
    st_n = st_spec / (st_spec.sum() + 1e-9)
    def band(f, s, lo, hi):
        sel = (f >= lo) & (f < hi)
        return s[sel].sum()
    print("  (gain-normalised: fraction of total spectral energy per band)")
    for lo, hi in [(0,1000),(1000,5000),(5000,10000),(10000,20000)]:
        rr = band(fr, sr_n, lo, hi); tt = band(ft, st_n, lo, hi)
        print(f"  band {lo:5d}-{hi:5d}Hz  ref={rr:6.3f}  til={tt:6.3f}  "
              f"excess={tt/(rr+1e-9):5.2f}x")

    # --- plot ---------------------------------------------------------------
    outdir = args.out or os.path.dirname(os.path.abspath(args.tiliqua))
    fig, ax = plt.subplots(4, 1, figsize=(13, 12))
    # (1) waveform zoom near start
    z0, z1 = int(sr*0.0), int(sr*0.2)
    tt = np.arange(z0, z1) / sr
    ax[0].plot(tt, ref[z0:z1], label="reference", lw=0.8)
    ax[0].plot(tt, til[z0:z1], label="tiliqua", lw=0.8, alpha=0.8)
    ax[0].set_title("waveform zoom  0–200 ms (startup)"); ax[0].legend(); ax[0].set_xlabel("s")
    # (2) waveform zoom mid
    z0, z1 = int(sr*30.0), int(sr*30.2)
    tt = np.arange(z0, z1) / sr
    ax[1].plot(tt, ref[z0:z1], label="reference", lw=0.8)
    ax[1].plot(tt, til[z0:z1], label="tiliqua", lw=0.8, alpha=0.8)
    ax[1].set_title("waveform zoom  30.0–30.2 s (steady)"); ax[1].legend(); ax[1].set_xlabel("s")
    # (3) envelopes
    tre, ere = envelope(ref, sr); tte, ete = envelope(til, sr)
    ax[2].plot(tre, ere, label="reference env", lw=0.7)
    ax[2].plot(tte, ete, label="tiliqua env", lw=0.7, alpha=0.8)
    ax[2].set_title("RMS envelope (20ms)"); ax[2].legend(); ax[2].set_xlabel("s")
    # (4) per-window lag + corr
    axb = ax[3]; axb.plot(times, lags, ".-", color="tab:red", label="lag (ms)")
    axb.set_ylabel("lag ms", color="tab:red"); axb.set_xlabel("s")
    axc = axb.twinx(); axc.plot(times, corrs, ".-", color="tab:blue", alpha=0.6, label="corr")
    axc.set_ylabel("xcorr", color="tab:blue"); axc.set_ylim(-0.1, 1.05)
    ax[3].set_title("per-1s lag & correlation (drift => dropped samples)")
    fig.tight_layout()
    out = os.path.join(outdir, "wav_zoom.png")
    fig.savefig(out, dpi=110)
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
