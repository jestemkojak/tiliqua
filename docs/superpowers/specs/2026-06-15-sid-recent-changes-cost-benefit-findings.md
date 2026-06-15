# sid_player_sw — Recent Changes Cost/Benefit Findings

**Date:** 2026-06-15
**Question asked:** Which change ultimately fixed Commando playback, and did we add changes
that worsen FPGA timing without improving playback (candidates for removal)?

## TL;DR

The week-long hunt conflated **two different problems**. Commando's playback was fixed entirely
by **firmware + one tiny gateware FIFO fix** (no timing cost). The changes that *added* gateware
timing cost (the phi2 runtime PAL/NTSC select + dual decimator) were solving a *different* problem
(pitch accuracy) and do nothing for dropped notes. Two removal candidates fall out; both have specs.

## Thread A — Commando "playback issue" (dropped notes / stutter / start glitch)

Fixed by firmware + a 1-line gateware FIFO fix. **None of these cost gateware timing.**

| Commit | Layer | What it fixed |
|---|---|---|
| `708cb20` | gateware (1-line) | Swallowed-write race in the transaction FIFO (~1/60 writes lost). *Necessary*, recovered ~25 onsets. |
| `6fc28f8` | firmware | UI-repaint pacing — stop the menu re-blitting thousands×/s and starving the 6502's PSRAM fetches (HW A/B win). |
| `3f0af6f`+`6343cc9` | firmware | SID reset on tune (re)load ($00 to $D400–$D418 + TEST bit) — kills the start-glitch stale-register noise burst. |
| `706f456` | firmware | Compile out the per-instruction log check — eff 16→21%, more 6502 headroom. |
| `1f8f4a1` | config | 2 KB L1 caches — 2× emulation speed. Confirmed win, kept. |

The remembered "drop at 55 s" was the *old* build's bug (a swallowed gate-off sustaining a wrong
note); post-fix it matches websid. So Commando was a **firmware/FIFO** problem, not a timing one.
Full record: `sid-player-sw-dropped-notes-checkpoint` memory.

## Thread B — what added timing cost, and what it actually solved

Worsened sync Fmax **56.17 → 53.47 MHz** (congestion), did nothing for dropped notes:

- `e9072b4`,`dd2f9f5`,`24bd60b`,`5bf1b53`,`0cbb11f`,`e5cfa74` — runtime PAL/NTSC **phi2 select +
  dual decimator**. Purpose: **pitch accuracy** (flat 1.000 MHz phi2 was +1.5 % sharp; PAL
  985.5 kHz = +0.5 cents). A real audio-quality fix — but orthogonal to the dropped-notes hunt.

The expensive part is `top.py:303`: a **second full `AudioDecimator`** (`audio_decim_ntsc`) that
runs in parallel for every tune and is muxed out by `phi2_sel`. Its FIR is the *same* ~17.55 ns
single-MAC structure that is now the sync critical path — so it is an identical-cost twin sitting
next to the limiting path, adding congestion. For a PAL library it computes a result that is
thrown away.

## Do NOT revert: the 30 MHz sid-domain work (`b3b26c6`..`8e2d228`)

This session's domain move is a **net timing positive** — it *improved* sync 53.47 → 56.99 MHz and
fixed a real audible bug (the reSID low-pass was corrupted on HW, playing a raw pulse). It is not a
removal candidate. See `sid-player-sw-timing-critical-path` memory.

## Cost-without-clear-benefit candidates

| Item | Layer | Benefit | Verdict |
|---|---|---|---|
| **Paced-replay fixed anchor** (`play_tick`, `main.rs:176-208`) | firmware | **Unproven** — the envelope-jitter mechanism it guards against was tested and *did not occur* (0 delayed/weak notes of 2274 at ±1 ms jitter). Costs an ISR busy-wait of up to ~½ the play period. | **Remove** → spec `2026-06-15-remove-paced-replay-anchor-design.md`. The deferred §4 A/B, made permanent. |
| **NTSC decimator** (`top.py:303`) | gateware | Pitch for NTSC tunes only; dead weight for a PAL library. | **Build-time switch** → spec `2026-06-15-pal-ntsc-build-time-design.md`. |
| Voice scope plotter + smoother/upsampler | gateware | Real (on-screen visualization) — *but* the biggest PSRAM-contention cost (a master competing with the 6502). | **Has** a benefit; cut only if the scope isn't wanted. Not in the "no benefit" bucket. |
| Stress-tune gens, host_render, onset/probe tools | dev tooling | Diagnostic | Repo clutter only; zero runtime cost. Keep. |

Everything else recent is a confirmed win (2 KB caches, log-check removal, UI pacing) or already
reverted (opt-level=3, ZP/stack-in-BRAM).

## Important interaction with the FIR-pipelining spec

Removing the NTSC decimator reclaims *area/congestion* (helps the 6502 / the *A Drop of Blue*
200 Hz drift) but does **not** by itself drop the remaining single PAL FIR below 60 MHz — that FIR
is intrinsically ~17.55 ns. Closing that path still needs
`2026-06-15-fir-mac-pipeline-design.md`. The three changes are complementary:

- **Pipeline the FIR** → fixes the 17.55 ns path itself.
- **Drop NTSC decimator** → removes congestion + dead area.
- **Remove paced-replay anchor** → frees 6502 real-time headroom (firmware, helps 200 Hz drift).
