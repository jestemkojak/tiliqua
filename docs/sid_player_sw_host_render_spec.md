# Spec: host-rendered SID reference WAVs ("virtual Tiliqua")

**Status:** IMPLEMENTED 2026-06-11 (commits `3a7b817` Stage 1, `3f313d7` Stages
2+3, fixes `2e5d7b1`/`8fdfdd4`, `4f73d03` note_peaks.py). See "Implementation
status" below for deltas vs this spec and what remains. **Date:** 2026-06-11.
**Goal:** render Commando (or any PSID) to WAV on the host using *the same
components the hardware uses* — our mos6502 write stream driving the verilated
reDIP-SID RTL — so recordings from the Tiliqua jacks can be diffed against a
bit-faithful digital reference, and websid differences can be attributed to
either the model or our player, not guessed at.

## Implementation status (2026-06-11)

All three stages landed; usage doc is
`gateware/src/top/sid_player_sw/tools/host_render/README.md` (3 commands
end-to-end). Deltas vs the spec below and remaining work:

- **Stage 1** = `dump_writes` ignored host test in `fw/src/player.rs`
  (env-parameterized: `DUMP_SID`, `DUMP_OUT`, `DUMP_FRAMES`, `DUMP_SUBTUNE`,
  `DUMP_C64`). The `--c64` 19656-cycle mode from open question 1 is in
  (`DUMP_C64=1`); default is hardware-truth 19950.616-cycle frames with
  running-remainder accumulation, as specced.
- **Stage 2 design change:** the reDIP-SID model select is a **compile-time**
  `` `define SID2 `` , not a runtime flag — so `render.sh -m {6581,8580}`
  builds and caches one verilated binary per model under
  `gateware/build/host_render/sim_<model>/`. Upstream's runtime `--sid-model`
  flag only masks filter-register bits and is unused. `harness.patch` adds
  `--phi2-hz` and `--tap {mix,v0,v1,v2}` exactly as specced; it is applied to
  a build-dir **copy** of `sid_api_sim.cpp` (deps/ never modified), with a
  `git apply` fallback when `patch(1)` is absent.
- **Stage 3:** `raw2wav.py` (s24be/s16le → 16-bit mono WAV); renders exist in
  `docs/recordings/` (`commando-host-6581-{mix,v1}.wav`). `note_peaks.py` was
  promoted into `tools/` as planned.
- **NOT done — mix tap is first-pass:** `-t mix` uses upstream's resampler
  (external RC filter on), not a port of the hardware `AudioDecimator`
  polyphase FIR (6/125, ~19 kHz). Treat mix WAVs as approximate; voice taps
  (`v0/v1/v2`) are the faithful jack comparison.
- **Validation:** V1–V4 all DONE (2026-06-11). V1: v1 tap byte-identical across
  two renders (commit `3f313d7`). V2: a 1 kHz tone renders at 999.985 Hz
  (−0.001 % vs the 1 MHz-phi2 expectation, +1.497 % sharp vs C64 PAL — matches
  the predicted ~1.5 %). V3: 24:1 (harness) vs 60:1 (hardware) clk:phi2 ratio →
  byte-identical voice taps. **V4: PASS — the host render reproduces the
  hardware `3f0af6f` capture's voice 1 better than websid does** (envelope corr
  0.984 vs websid 0.944; per-note loud/soft classes byte-identical to the
  capture, 32/32). See "V4 findings" below — two methodology fixes were needed
  to get there.

### V4 findings (two comparison hazards, both now fixed in the tooling)

1. **6581 voice taps carry a DC bias.** `voiceN_dca_o` on the 6581 sits at
   ~+0.38 FS (the model's `VOICE_DC` ≈ ½ dynamic range; the 8580's is 0). The
   Tiliqua voice path is in fact **DC-coupled** — `I2SCalibrator`'s `A·x+B`
   (`eurorack_pmod.py`) only nulls the codec's own zero, and the eurorack-pmod
   jack is DC-coupled by design — so the bias reaches the jack on hardware. It's
   stripped *downstream* by the AC-coupled input of whatever records the jack
   (and websid's voice export is AC-coupled too), which is why jack captures /
   websid exports are DC-free. Comparing a raw tap WAV (DC-laden) to a capture makes
   `abs()`/RMS analysis useless — the offset swamps the per-note dynamics
   (with DC: host per-note peaks span only 1.15×; without: the real ~4× split
   appears). **Fix:** `render.sh` now AC-couples voice taps via
   `raw2wav.py --dc-block` (one-pole HP, corner a few Hz). The mix tap is left
   alone (it already passes the external RC high-pass).
2. **Voice numbering is off by one between the taps and the captures.** The
   host_render taps `v0/v1/v2` are **0-indexed** (`voice0/1/2_dca_o`); the
   recordings named `*-1voice-*` / `*-voice1` are **1-indexed** "voice 1" =
   SID voice **index 0**. So the captured/soloed voice is host tap **`-t v0`**,
   not `v1`. Correlating the wrong voice gives ~0.2 and looks like a fidelity
   bug; the right voice gives 0.98. Confirmed by sweeping all three taps
   (v0 = 0.82→0.98 across windows, v1/v2 ≈ 0.2).

The note_peaks *class string* is fragile on the noisy 200 s captures (the
onset-based t0 fit latches onto ~1000 spurious onsets in a 6 s window); the
**lag-aligned RMS envelope correlation is the robust metric** and is what the
0.984-vs-0.944 result above uses.

## Why this isolates faults

| Comparison | A difference means |
|---|---|
| host render vs tiliqua capture | delivery/analog: FIFO, replay timing, codec, jack, recording chain |
| host render vs websid | model/timing: reDIP-SID vs websid's reSID, our frame quantum (19950.6 vs 19656 cycles), reset state |
| host render run-to-run | nothing — must be bit-identical (determinism canary) |

The existing investigation hit exactly this wall twice (soft/loud note classes,
start transient): recordings of two real renderers can't separate model
differences from player bugs.

## Existing parts (verified present)

- RTL: `gateware/deps/sid/gateware/sid_*.sv` (reDIP-SID, Dag Lem) — the same
  files `src/top/sid/top.py` instantiates on the FPGA (`sid_api`, with
  `voice0/1/2_dca_o` taps and `audio_o`). Model select = `` `define SID2 `` for
  8580 in `sid_defines.sv` (our build flag `--sid-model`).
- Upstream sim harness: `deps/sid/gateware/sid_api_sim.cpp` + `make sim` —
  Verilator build that reads `cycles address value` lines (phi2-cycle delta
  before each register write) from stdin and writes raw 24-bit audio; options:
  `--sid-model {0,1}`, `--sample-rate`, `--filter`, `--bandpass` (external RC
  model), `--video-standard` (phi2 Hz).
- Verilator 5.049 (oss-cad-suite, on PATH).
- Write-stream generator: our host-tested `player.rs` (`init`/`call` with
  per-write cycle stamps) — already proven deterministic.

## Architecture (3 stages, all host)

### Stage 1 — write-stream dump (Rust, `fw/` host test or `--bin`)

New ignored test `dump_writes` (pattern: existing probes) that emits
the **exact sequence the firmware delivers to the FIFO**, as upstream-format
lines `cycles address value` (cycles = phi2 delta before the write):

1. **Prelude = `sid_reset()` semantics**: 3× CTRL=$08 (TEST), then $00 to regs
   $00–$18 ascending, 1 cycle apart (FIFO drains 1/phi2; backpressure ⇒
   back-to-back edges).
2. **INIT writes** (drained burst, 1 cycle apart — `drain_sid_writes`).
3. **PLAY frames**: frame n's write k at absolute phi2 cycle
   `n*FRAME + OFFSET + stamp_k` (the replay anchor scheme; `OFFSET` is a
   constant ⇒ emit `FRAME//2` once, it only shifts the timeline). Same-stamp
   writes serialize at ≥1-cycle spacing (FIFO drain order).
4. `FRAME` parameter (see Open Questions): hardware truth is
   `period_sync/60 = 19950.616…` — non-integer. Emit integer schedule with
   running remainder (accumulate `period_sync` in sync ticks, divide by 60 per
   write) so long-run tempo matches hardware exactly.

CLI via env (like `GATE_TRACE_FRAMES`): tune path, subtune, frame count.
Output: `commando_writes.sidw` (text, ~10600 frames ≈ 1.3 MB).

### Stage 2 — verilated render (upstream harness, wrapped not forked)

- Build: `make sim` in `deps/sid/gateware` (audit the Makefile first; it
  builds `sim_audio/Vsid_api` and a trace variant). Do **not** edit deps/ in
  place; any patch lives in our tree (see below).
- Run: `sim_audio/Vsid_api --sid-model {0|1} --sample-rate 48000 < dump.sidw`
  → `sid_api_audio.raw` (s24be mono) → WAV.
- **Required patch (small, kept as a git-apply'able diff under
  `src/top/sid_player_sw/tools/host_render/`):**
  1. phi2 = 1 000 000 Hz option (`--phi2-hz`): our gateware clocks the SID at
     exactly 1 MHz, not PAL 985248 — pitch/timeline must match Tiliqua, not a
     C64.
  2. Voice-tap dump mode (`--tap {mix,v0,v1,v2}`): write `voiceN_dca_o`
     instead of `audio_o`. For jack comparison the tap must be
     **point-sampled at 48 kHz with no anti-alias filter** — that is what
     `pmod0.i_cal.payload[n]` does on hardware (known aliasing and all). The
     mix tap instead mirrors `AudioDecimator` (polyphase 6/125, ~19 kHz
     cutoff) — port its coefficients, or first-pass: upstream's own resampler
     with `--filter`/`--bandpass` disabled and note the delta.
  3. Disable the external RC/output-stage filter for tap modes (jack taps are
     pre-codec digital values; the codec is ~flat in-band).

### Stage 3 — WAV + comparison (existing tools)

raw → 16-bit 48 kHz WAV (numpy one-liner, lives in the same tools dir), named
`commando-host-<model>-<tap>.wav` in `docs/recordings/`. Compare with
`tools/wav_onsets.py` (note-level), `tools/wav_compare.py` (drift/spectra),
and the comb-matched per-note peak table from the 2026-06-11 session (promote
that throwaway script into `tools/note_peaks.py` while at it).

## Fidelity caveats (accepted, documented)

- **clk:phi2 ratio**: hardware = 60:1 (60 MHz), upstream sim uses its own
  ratio. The core spec needs only >20×; the phi2-edge pipeline completes
  either way. Validation item V3 checks this.
- The FIFO applies ≥1 write per phi2 edge in order; Stage 1 reproduces the
  schedule but not µs-level ISR-entry jitter — proven inaudible/irrelevant
  (envelope model, 2026-06-10).
- DAC tables: sid_dac.hex files are part of the RTL — included automatically.
- OSC3/ENV3 reads: none in Commando (verified); harness is write-only anyway.

## Validation checklist (before trusting renders)

- V1 ✅ DONE: render twice → byte-identical (verified on v1 tap, 29.9 s
  Commando dump, commit `3f313d7`).
- V2 ✅ DONE: a fixed-frequency tone (`Fn=0x4189`) renders at 999.985 Hz —
  −0.001 % vs the 1 MHz-phi2 expectation and +1.497 % sharp vs the C64 PAL
  pitch (matches the predicted ~1.5 %). The `make_sid` validator asserts the
  voice-0 freq changes, so the tone was emitted as a direct `.sidw` write-stream
  (reset → set ADSR/freq/CTRL → hold) rather than via `gen_stress_sid.py`.
- V3 ✅ DONE: built a 30-clk-per-half-cycle variant (60:1, hardware) and diffed
  its voice tap against the default 24:1 harness build on the same dump →
  **byte-identical**. The reDIP-SID pipeline settles within either half-cycle,
  so the harness ratio faithfully matches hardware; no change needed.
- V4 ✅ DONE: see "V4 findings" above. On the correctly-mapped voice (host
  `-t v0` = the captured "voice 1"), with voice taps DC-blocked: envelope corr
  host-vs-capture = **0.984** > websid-vs-capture 0.944, and the per-note
  loud/soft class string is **byte-identical to the capture (32/32)**. The host
  render matches the hardware better than websid does — delivery/model/player
  all confirmed faithful for this tune.

## Deliverables / layout

```
gateware/src/top/sid_player_sw/tools/host_render/
  README.md            usage: 3 commands end-to-end
  harness.patch        upstream sid_api_sim.cpp additions (phi2-hz, taps)
  render.sh            dump (cargo test) -> make sim -> render -> wav
  raw2wav.py           s24be raw -> 16-bit wav (numpy)
fw/src/player.rs       + dump_writes (ignored test; env-paramed)
```

Estimated effort: Stage 1 ~1 h; Stage 2 patch ~1–2 h (validation included);
Stage 3 trivial. No gateware/firmware changes; nothing on the real-time path.

## Open questions — all RESOLVED in implementation

1. **Frame quantum**: hardware-truth 19950.616-cycle frames is the default;
   `DUMP_C64=1` switches to 19656-cycle frames (mod-9 phase-lock testing vs
   websid). Both implemented and property-tested in `player.rs`.
2. **Default model**: 6581 (`render.sh -m 6581` is the default), matching the
   Commando builds. Note the model is a compile-time Verilog define, not the
   upstream `--sid-model` runtime flag — render.sh caches one binary per model.
3. **Patch, not fork**: `harness.patch` applied at build time to a copy of
   `sid_api_sim.cpp` in the build dir; deps/ untouched; `git apply` fallback
   when `patch(1)` is missing.

## Remaining work (pick up from here)

V2–V4 are all done (2026-06-11); the payoff (V4) confirmed the host render
matches the hardware capture better than websid. What's left is optional:

1. Optional fidelity upgrade: port the `AudioDecimator` polyphase FIR
   coefficients into the `-t mix` path (currently upstream's resampler — see
   Implementation status).
2. Optional robustness: `note_peaks.py`'s onset-based t0 fit is fragile on
   noisy multi-voice captures (it found ~1000 spurious onsets in a 6 s window).
   A "known gate times + single best lag" alignment (we know the dump's gate
   frames) would make the per-note class string trustworthy without the
   envelope-correlation fallback. Until then, prefer the lag-aligned RMS
   envelope correlation as the headline metric.
3. The V4 conclusion was validated only on Commando voice 0. Re-running V4 on
   another tune (and the other two voices, with fresh co-built captures) would
   broaden the "model + player faithful" claim beyond one voice of one tune.
