# SID φ2 runtime PAL/NTSC select — design spec

**Date:** 2026-06-12  **Target:** `sid_player_sw`  **Status:** approved design, pre-implementation

## Problem

The gateware clocks the reSID core at a hardcoded 1.000 MHz
(`gateware/src/top/sid/top.py` `DIVIDE_BY = 60`, sync 60 MHz / 60). PSID tunes
assume the real C64 φ2 — PAL 985 248 Hz or NTSC 1 022 727 Hz. SID pitch is
linear in φ2 (`f = Fn·φ2/2²⁴`), so every note and every φ2-clocked ADSR/filter
runs +1.497 % fast (+25.9 ¢, an audible quarter-semitone) for PAL tunes and
−2.2 % slow (−38.9 ¢) for NTSC tunes.

**Tempo is already correct and out of scope.** `psid::play_period_cycles`
(`fw/src/psid.rs`) computes the TIMER0 reload from the tune's declared standard
using the real C64 clocks, per tune. Only pitch (the SID's own clock) is wrong.

## Decision summary

- **Runtime select, not a build flag.** One bitstream carries both standards; a
  1-bit CSR (`phi2_sel`: 0=PAL, 1=NTSC, reset 0) switches them. Firmware
  auto-selects per tune from the PSID header, with a UI override row.
  (Supersedes the earlier `--phi2 {1mhz,pal,ntsc}` build-flag idea: runtime
  costs only ~+5 BRAM blocks and means mismatched-standard tunes play at
  correct pitch *and* tempo with zero user action.)
- **No 1 MHz back-compat path.** PAL/NTSC is enough (user decision). The old
  +1.497 %-sharp behavior disappears.
- **Near-exact "clean" φ2 targets, not bit-exact.** The decimator FIR tap count
  is `5·max(n_up, m_down)` with `n_up = 48000/gcd(48000, fs_in)`; bit-exact
  PAL (500/10263) needs a ~51 500-tap ROM ≈ 51 of the 22 free DP16KD blocks —
  doesn't fit — and bit-exact NTSC (16000/340909) is absurd (~1.7 M taps).
  The chosen targets are ≤0.5 ¢ off (audibility threshold ≈ 5 ¢; all voices
  shift together, so there is no inter-voice beating):

  | standard | nominal φ2 | chosen φ2 | pitch error | NCO `P+num/den` | decimator `n_up/m_down` | taps | tap-ROM BRAMs |
  |---|---|---|---|---|---|---|---|
  | PAL  | 985 248   | **985 500**   | +0.44 ¢ | 60 + 580/657 | 32/657 | 3 296 | 4 |
  | NTSC | 1 022 727 | **1 023 000** | +0.46 ¢ | 58 + 222/341 | 16/341 | 1 712 | 2 |

- **φ2 NCO = fractional-N divider** (chosen over a per-sync-cycle phase
  accumulator and over a PLL φ2 clock domain): keeps the existing
  counter/duty/edge structure, all new state updates at φ2 cadence (~1 MHz), so
  nothing is added to the 60 MHz critical path (the reSID filter muladd is the
  Fmax bottleneck — see `docs/sid_player_sw_perf_review.md`). φ2 edges land on
  the 60 MHz grid → ±1 sync cycle (16.7 ns) deterministic jitter; resulting
  sidebands < −80 dB in the audio band, further averaged by the ~103-MAC FIR.

## Gateware design

### 1. `SIDPeripheral` (`gateware/src/top/sid/top.py`)

- New constructor parameters: `phi2_hz=(sel0_hz, sel1_hz)` **defaulting to
  `(1_000_000, 1_000_000)`**, and `sync_hz` (retires the `TODO generate this
  constant`; passed from the SoC's clock settings). The default makes every
  non-opted-in SID target bit-identical to today regardless of `phi2_sel`
  (num=0 → constant /60), satisfying Non-goals. `sid_player_sw` passes
  `(985_500, 1_023_000)`.
- New 1-bit RW CSR register `phi2_sel` (next free offset in the existing reg
  map; 0=PAL, 1=NTSC, reset 0). Also exposed as an `Out(1)` port so the SoC
  top can mux decimator outputs.
- Replace the `DIVIDE_BY = 60` block with a fractional-N divider. At
  elaboration, `Fraction(sync_hz, phi2_hz)` gives `(P, num, den)` per standard
  (table above). At runtime:
  - `counter` counts `0 .. period−1`; at wrap: `acc += num`; if `acc ≥ den`
    → `period = P+1, acc −= den`, else `period = P`.
  - `phi2 = counter > (period >> 1)`, `phi2_edge = counter == period−1` —
    same duty/edge derivation as today; everything downstream (`startup`
    reset sequencing, transaction pop, `audio_strobe`) is untouched.
  - `(P, num, den)` are muxed from the two constant sets by `phi2_sel`.
    A mid-stream switch self-corrects within a few φ2 periods (the
    `acc ≥ den` step drains any stale accumulator value; `period` stays in
    `{58..61}`, within the existing 8-bit counter). Firmware switches only on
    tune load / UI override, so the transient is irrelevant.

### 2. Decimators (`gateware/src/top/sid_player_sw/top.py`)

- Replace the single `AudioDecimator(fs_in=1_000_000)` with two instances:
  `AudioDecimator(fs_in=985_500)` and `AudioDecimator(fs_in=1_023_000)`, both
  fed by the same `audio_strobe`/`last_audio_left` input. The inactive one
  receives samples at a 3.8 %-off `fs_in` and its output is ignored — harmless.
- A mux on `sid_periph.phi2_sel` picks which held output drives the codec
  (`pmod0.i_cal.payload[3]`) and the scope mix channel.
- `AudioDecimator`/`dsp.Resample`/`FIR` themselves need **no changes** — the
  existing gcd math derives 32/657 and 16/341 from `fs_in` automatically.
  MACs per output stay ~103 (polyphase), so FIFO depth 8 and backpressure
  behavior are unchanged. With φ2 = `fs_in` by construction, the selected
  output updates at exactly 48 kHz — no codec-rate drift.

### 3. Resource budget (current: DP16KD 34/56, MULT18X18D 11/28, COMB 20772/24288 = 85 %)

- BRAM: −(6/125 decimator) + PAL(4) + NTSC(2) ≈ **net +5 → ~39/56**.
- Multipliers: net **+1 → 12/28**.
- LUTs: one net-new FIR control FSM + FIFO (a few hundred LUTs) against ~3 500
  free. **This is the watched resource**; if the build no longer fits or Fmax
  degrades, fallback = share one FIR datapath between the two rate
  configurations (more design work, only if needed).

## Firmware design (`fw/src/main.rs`, `fw/src/psid.rs`)

- **Auto-select:** `reload_tune()` writes `phi2_sel` from `hdr.clock()`
  (PAL→0, NTSC→1) after every successful load. `psid::Clock` already exists
  and already drives tempo; pitch now follows the same source.
- **UI override:** new Player-card row `Clock: AUTO / PAL / NTSC` (default
  AUTO). AUTO applies the current tune's header value; an override writes the
  CSR immediately (pitch shifts live). Like the scope CSRs, `phi2_sel` is
  independent of the SID ISR state → **no critical section needed**; the
  worst race with `reload_tune()` is last-write-wins of two writes derived
  from the same header. Check/grow `HEADER_H` for the extra row.
- **Metadata row** gains the tune's declared standard ("PAL"/"NTSC") next to
  the declared SID model, so header info is visible at a glance.
- **No tempo-path changes.** `play_period_cycles` is untouched.
- PAC regen required after the CSR change (`pdm sid_player_sw build
  --pac-only`).
- `tools/host_render`: add a `PHI2` env var (default 985 500) so FPGA-faithful
  renders keep matching hardware after this change.

## Verification

1. **NCO sim test** (new, `gateware/tests/`): for each select, count φ2 edges
   over full fractional periods — exactly 657 edges per 40 000 sync cycles
   (PAL), 341 per 20 000 (NTSC); duty ≈ 50 %; plus a mid-stream-switch case
   (rate settles to the new standard, no stuck state).
2. **Decimator host test**: parametrize the existing `tests/test_sid_audio.py`
   harness over `fs_in ∈ {985_500, 1_023_000}` — 1 kHz tone passes, alias tone
   rejected.
3. **Firmware host tests**: `cd fw && cargo test --target
   x86_64-unknown-linux-gnu --lib` stays green (psid clock mapping is already
   host-tested).
4. **Build checks**: build completes; DP16KD ≈ 39, MULT 12; achieved Fmax (the
   *second* "Max frequency" line in `build/sid-player-sw-r5/top.tim`) not
   degraded vs. the current build.
5. **Hardware A/B** (the real proof): flash, play Commando (PAL), capture
   voice-0, compare against `docs/recordings/commando-c64ref-6581-v0.wav`
   (real-C64 PAL timing): time-stretch factor 1.000 ± 0.03 % (vs. today's
   1.497 %), envelope correlation ≥ 0.99 *without* time-warping, measured
   pitch offset ≤ 1 ¢. Spot-check one NTSC tune (tempo ≈ 59.83 Hz and pitch
   both correct on the same bitstream).

## Non-goals

- Bit-exact 985 248 / 1 022 727 Hz (doesn't fit BRAM; ≤0.5 ¢ is inaudible).
- A 1 MHz mode or any build-time φ2 flag.
- Changing other SID targets (`sid`, `sid_player`) — they keep their current
  behavior/defaults; only `sid_player_sw` opts into the new divider rates.
- Tempo-path or scope-branch changes (`VoiceSmoother` doesn't care about a
  3.7 % tap-rate range).
- Moving reSID off the 60 MHz domain (separate Fmax project).

## Key files

- `gateware/src/top/sid/top.py` — `SIDPeripheral`: divider (`DIVIDE_BY=60`
  block), CSR map, new `phi2_sel`.
- `gateware/src/top/sid_player_sw/top.py` — decimator instantiation (~line
  284), output mux, `sync_hz`/params threading.
- `gateware/src/top/sid/audio.py` — `AudioDecimator` (unchanged, reference).
- `gateware/src/tiliqua/dsp/filters.py` — `FIR` tap/cost model (unchanged,
  reference).
- `fw/src/main.rs` — `reload_tune()`, Player card rows, metadata line.
- `fw/src/psid.rs` — `Clock`/`clock()` (unchanged, reference).
- `gateware/src/top/sid_player_sw/tools/host_render/` — `PHI2` env var.
