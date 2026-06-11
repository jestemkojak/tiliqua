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
- **Validation:** V1 done (v1 tap byte-identical across two renders of a
  29.9 s Commando dump; non-zero audio after 1 s — see commit `3f313d7`).
  **V2 (pitch tone), V3 (60:1 clk ratio), V4 (per-note peak table vs the
  `3f0af6f` capture) have NOT been run** — do these before drawing
  model-vs-player conclusions from renders.

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
- V2 ❌ TODO: 1 kHz test tone tune (reuse `tools/gen_stress_sid.py`) → expected
  pitch at 1 MHz phi2 (1.5 % sharp vs C64-targeted pitch).
- V3 ❌ TODO: one short render at 60:1 clk ratio (patch or harness param) vs
  default ratio → byte-identical voice taps expected; if not, match hardware's
  60:1.
- V4 ❌ TODO: per-note peak table (frames 3200–3500, `tools/note_peaks.py`) vs
  the `3f0af6f` capture — soft/loud classes must match the capture (delivery
  already proven good), and v1-tap render vs capture envelope corr should beat
  websid's.

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

1. Run validations **V2–V4** (see checklist above). V4 is the payoff: it
   directly tests whether the soft/loud note-class split is model or player.
2. Optional fidelity upgrade: port the `AudioDecimator` polyphase FIR
   coefficients into the `-t mix` path (currently upstream's resampler — see
   Implementation status).
