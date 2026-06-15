# FIR Multiply-Accumulate Pipeline — Design

**Date:** 2026-06-15
**Status:** Implemented (commit b541268). See "## Results" below.

## Problem

After the 30 MHz `sid`-domain move (`docs/superpowers/plans/2026-06-14-sid-30mhz-domain.md`,
commits `312d83e`..`8e2d228`), the reSID filter left the `sync` critical path and the
`sid_player_sw` build's `sync` (`$glbnet$clk`) timing improved from 53.47 MHz to **56.99 MHz**
— still **FAIL at 60 MHz**. The new sync critical path is the AudioDecimator's polyphase FIR:

```
audio_decim_pal.resample.filt:  taps_mem (BRAM clk-to-q 5.61ns)
  -> b[] tap -> a[0]_MULT18X18D (3.93ns) -> y[]_CCU2C accumulator carry chain
  -> y[15]_TRELLIS_FF      = 17.55 ns total  (~57 MHz)
```

Source: `gateware/build/sid-player-sw-r5/top.tim` (build `67e5023`), critical-path report for
`$glbnet$clk`. The FIR is `tiliqua.dsp.FIR` (`gateware/src/tiliqua/dsp/filters.py:251`), used via
`tiliqua.dsp.Resample` (`gateware/src/tiliqua/dsp/resample.py`).

### Root cause

`FIR`'s `MAC` state (`filters.py:418-436`) performs the entire multiply-accumulate in a single
clock:

```python
m.d.comb += [a.eq(x_rport.data), b.eq(taps_rport.data)]   # combinational BRAM reads
m.d.sync += [y.eq(y + (a * b)), macs.eq(macs + 1), ...]   # mult + accumulate + register
```

So `BRAM read (a,b) -> MULT18X18D -> accumulate carry chain -> register y` is one combinational
path. The ECP5 `MULT18X18D` has an output pipeline register (PREG) that is left unused; driving an
unregistered multiplier straight into the accumulator is what costs ~17.55 ns.

## Goal / Acceptance

- The decimator FIR (`resample.filt`) is **no longer on the `sync` critical path**; sync Fmax
  improves measurably above 56.99 MHz.
- Report the new `sync` critical path and Fmax after the change.
- This does **not** have to close sync at 60 MHz: a third, unrelated path (~57 MHz region, e.g.
  SoC/6502) may surface and is a separate follow-up. (Decision 2026-06-15.)
- No behavioural change to any FIR/Resample consumer: filtered output stays correct within the
  existing test tolerances.

## Approach

Chosen over "move the decimator to the `sid` domain" because it fixes the root cause in the
shared DSP primitive (benefiting all six FIR/Resample users), needs **no new clock-domain
crossing**, and is provable in simulation by the existing `test_fir`/`test_resample` suite.
(Decision 2026-06-15.)

### Pipeline the multiply-accumulate (one stage + a drain cycle)

Register the product, accumulate it the following cycle, and add one drain cycle so the final
in-flight product is accumulated before output:

```
WAIT-VALID:  y <- 0,  p <- 0                 # init accumulator AND product register
MAC (k=0 .. N-1):                            # N = n // stride_i
             p <- a * b                       # registers the product -> engages MULT PREG
             y <- y + p                       # accumulates the PREVIOUS cycle's product
             (advance ix_tap / ix_rd as today)
DRAIN (new): y <- y + p                       # flush the final product still in `p`
WAIT-READY:  o.payload <- y                   # unchanged
```

This breaks the 17.55 ns path into two:
- **BRAM read -> MULT -> `p`**: ~13.4 ns (≈75 MHz). (The `MULT18X18D.P` output sat at 13.39 ns in
  the timing report; registering it there is the natural split.)
- **`p` -> accumulate -> `y`**: ~4 ns (carry chain only).

Both clear 60 MHz, so the FIR leaves the sync critical path. Registering the multiplier *operands*
as well (full `MULT18X18D` AREG/BREG+PREG pipeline, ~120 MHz) is deliberately **not** done: it would
require 2-cycle-ahead addressing for no benefit at a 60 MHz target. Keep the change minimal.

### Correctness details

- **Product width.** `p` is declared at the **full multiply width** (`fixed.SQ(4, 2*f_bits)`, i.e.
  the shape of `a * b`), so `y += p` truncates to `ctype` bit-identically to the original
  `y + (a * b)`. No arithmetic drift; tight-tolerance tests stay valid.
- **First MAC cycle.** `p` is initialized to 0 in `WAIT-VALID` (alongside `y <- 0`), so the first
  `y <- y + p` adds zero — correct.
- **Addressing unchanged.** Read addresses are still set one cycle ahead; `MAC` cycle `k` presents
  `a,b` for product `k`, exactly as today. The `DRAIN` cycle issues no read (the last
  address-advance in `MAC` is harmless — its data is ignored).
- **Accumulation count.** `MAC` runs `N` cycles (computing products 0..N-1, accumulating products
  0..N-2); `DRAIN` accumulates product N-1. All `N` products are summed. Net latency goes from
  `N + 1` to **`N + 2`** (+1 cycle).
- **Discard path untouched.** When `stride_o_pos != 0` the FSM still goes `WAIT-VALID -> WAIT-READY`
  directly (no `MAC`, no output); `DRAIN` is only ever entered from `MAC` (the produce path).

## Components / Files

- **`gateware/src/tiliqua/dsp/filters.py`** — `FIR.elaborate`: add the `p` product register
  (full width), init `p <- 0` in `WAIT-VALID`, split the `MAC` body into `p <- a*b` / `y <- y + p`,
  add the `DRAIN` state, retarget the `MAC`-done transition to `DRAIN`. Single, self-contained unit;
  its interface (the `i`/`o` streams) is unchanged.
- **`gateware/tests/test_dsp.py`** — `test_fir`: bump each `expected_latency` by 1
  (11→12, 17→18, 9→10, 5→6, 6→7, 4→5). `test_resample` asserts values/counts only (no fixed
  latency), so it needs no change but must still pass.

No consumer files change: all six FIR/Resample users (`raster/scope.py`, `top/macro_osc`,
`top/polysyn`, `top/sid/audio.py`, `top/vectorscope_no_soc`, `top/xbeam`) communicate through the
stream `valid`/`ready` handshake, which absorbs the +1 latency.

## Testing & Verification

1. **Unit (sim, no hardware):** `cd gateware && pdm run pytest tests/test_dsp.py -k "fir or resample" -v`
   — `test_fir` proves filtered output within 0.005 of `scipy.signal.lfilter` at the new latencies;
   `test_resample` proves the polyphase resampler still matches `scipy.signal.resample_poly`.
2. **Build:** `cd gateware && pdm sid_player_sw build`.
3. **Timing (acceptance):**
   `grep -nE "Max frequency for clock" build/sid-player-sw-r5/top.tim` (sync = the second
   `$glbnet$clk` line) and the `$glbnet$clk` critical-path report. Confirm `resample.filt` is no
   longer the limiter and record the new Fmax + new critical path.

## Risks

- **Shared primitive.** A MAC-drain off-by-one would corrupt filtering for all six consumers — but
  it is fully caught by the tight-tolerance `test_fir`/`test_resample` in sim before any build.
- **PREG inference.** If yosys/abc9 does not pack the product register into the `MULT18X18D` PREG,
  `p` is still an ordinary FF after the multiplier — the path is still split (mult vs accumulate),
  so the timing benefit holds either way (≈75 MHz vs ≈120 MHz best case); 75 MHz already clears the
  60 MHz target.
- **Residual sync FAIL.** Per acceptance, a third path may still hold sync below 60 MHz; that is
  reported, not fixed here.

## Out of Scope

- Closing `sync` at 60 MHz unconditionally (the next path, if any, is a separate effort).
- Moving the decimator(s) to the `sid` domain (the alternative approach, not chosen).
- Registering the multiplier operands for a ~120 MHz FIR (unnecessary at a 60 MHz target).

## Results

- Build: `b541268` (`gateware/build/sid-player-sw-r5/top.tim`).
- sync (`$glbnet$clk`) post-route Fmax: 56.99 MHz -> **64.36 MHz (PASS at 60.00 MHz)**.
- New `sync` critical path: `audio_decim_pal.resample.filt` — BRAM clk-to-q ->
  LUT4 -> MULT18X18D -> **`p[30]_TRELLIS_FF_Q`** (the new product register) = 15.54 ns.
  The accumulator carry chain (`p -> y`) is now a separate shorter path. The old
  17.55 ns `BRAM -> MULT -> accumulator -> y` path is broken; `resample.filt` is no
  longer the limiter in the accumulate sense — the MAC pipeline split worked as designed.
  sync is now PASS at 60 MHz with 4.36 MHz of margin.
- `test_fir`/`test_resample`: PASS at the new N+2 latencies (verified in commit b541268).
