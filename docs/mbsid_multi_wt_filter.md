# MBSID Multi-engine wavetable → filter modulation (shim-side fix)

**Date:** 2026-06-29
**Status:** spec (approved approach) — implementation plan at
`docs/superpowers/plans/2026-06-29-mbsid-multi-wt-filter.md`
**Tracking issue:** `.scratch/mbsid-multi-wt-filter-broken/issue.md`
**Subsystem:** `gateware/src/top/mbsid` (MBSID-on-Tiliqua)

---

## 1. Problem

Multi-engine patches that drive a SID filter from the wavetable (WT) sequencer produce
**complete silence** on hardware. The headline case is the factory patch **A107 "Poly
Trancegate"** (bank 0, patch index 106): it parks both SIDs' low-pass cutoff at 0 (static
silence) and relies on the WT to sequence the cutoff open/closed rhythmically. With the WT→filter
path dead, cutoff stays 0 → no sound.

## 2. Root cause — upstream incompleteness, not a porting gap

This is **not** a Tiliqua porting omission. `fw/build.rs` compiles the *entire* mios32
`midibox_sid_v3` engine tree verbatim (including `components/MbSidWt.cpp`); the wavetable
sequencer is fully present and the **Lead** engine's WT path is oracle-validated bit-exact.
Nothing was dropped.

The gap is specific to upstream's **Multi** engine (`core/MbSidSeMulti.cpp`), which never finished
wiring the WT output to parameters:

1. **`wtAssignLeftRight` is never set.** In `sysexSetParameter`, the WT-speed byte handler
   (`case 0x2b`) extracts only `wtSpeed = data & 0x3f`. The Lead engine additionally does
   `wtAssignLeftRight = (data >> 6)` (which SID L/R the WT targets). Multi omits it, so
   `wtAssignLeftRight` stays 0 → `sidlr = 0` in the WT tick → neither filter is selected.

2. **`parSetWT` is a no-op.** `MbSidSeMulti` does not override `MbSidSe::parSetWT`, whose base
   definition is an empty `virtual {}`. The WT tick calls `parSetWT(w->wtAssign, wtValue, sidlr,
   ins)` every step (`MbSidSeMulti.cpp:187`) — and it does nothing.

Corroborating evidence of upstream incompleteness: `MbSidSeMulti::parSet`/`parGet` are littered
with `// TODO` for external CV, switches, and voice parameters. The V3 C++ rewrite left Multi's
modulation plumbing unfinished; it only manifests as a bug for patches that depend on it.

## 3. Decision — fix in our firmware/shim, not the vendored engine

The vendored `mios32/` tree is licensed "for personal non-commercial use only; all other
rights reserved" (not GPL — see the mbsid `CLAUDE.md`'s 2026-07-23 correction), gitignored,
and re-cloned at a pinned commit by `fetch-mios32.sh`. A raw edit there is invisible to a
fresh clone / CI and would require adding
a patch-apply mechanism. Per the project preference to avoid modifying the upstream C++ engine
(stated explicitly on the sibling Lead-retrigger issue), we fix this **entirely in code we own**.

This is feasible because every engine member involved is **public** and the shim already reaches
into engine internals (`env.mbSid[0].midiReceiveNote(...)`, etc.):

| Member | Location | Access |
|---|---|---|
| `MbSid::currentMbSidSePtr`, `MbSid::mbSidSeMulti` | `MbSid.h:67,64` | public |
| `MbSidSeMulti::mbSidWt[6]`, `mbSidFilter[2]` | `MbSidSeMulti.h:85,80` | public |
| `MbSidSeMulti::parSet(...)`, `parGet(...)` | `MbSidSeMulti.h:69,71` | public |
| `MbSidSe::mbSidPatchPtr` | `MbSidSe.h:95` | public |
| `MbSidWt::{wtOut(s16), wtAssign(u8), wtAssignLeftRight(u8)}` | `MbSidWt.h:49,42,43` | public |

We **cannot** override the no-op virtual on a by-value member, so instead we replicate Lead's
`parSetWT` math and call the engine's *own* public `parSet`/`parGet` — i.e. reuse the engine's
parameter routing rather than inventing new logic.

### Alternative considered & rejected

A 3-line edit to `MbSidSeMulti.cpp` (`+wtAssignLeftRight` extraction and a `parSetWT` override) is
*more* faithful (runs at the engine's own call site, no timing lag, oracle stays green
automatically). It was rejected only because it touches the vendored tree and needs a
patch-apply mechanism in `fetch-mios32.sh`. If the project later adopts a vendor-patch workflow,
this is the cleaner home for the fix.

## 4. Design

A single header-only helper, called **once immediately after `env.tick()`** by both the firmware
shim and the host oracle reference driver.

### 4.1 The helper

`fw/csrc/mbsid_multi_wt.h`:

```cpp
static inline void mbsid_multi_wt_fixup(MbSid &sid);
```

Behaviour (mirrors `MbSidSeMulti::tick()`'s WT block at lines 178–188, and
`MbSidSeLead::parSetWT` at 1067–1092):

- No-op unless `sid.currentMbSidSePtr == &sid.mbSidSeMulti` (active engine is Multi).
- For each of the 6 WTs with `wtOut >= 0 && wtAssign`:
  - Recover the assign-LR bits upstream dropped, straight from the authoritative patch bytes:
    `sidlr = sid.mbSidSeMulti.mbSidPatchPtr->body.M.voice[wt][0x2b] >> 6`
    (the WT-speed byte; bit6 = `CHANNEL_TARGET_SIDL`, bit7 = `CHANNEL_TARGET_SIDR`, so `sidlr&1`
    = L filter, `sidlr&2` = R filter — exactly Lead's `data >> 6`).
  - `wtValue = …->body.M.wt_memory[wtOut & 0x7f]` (identical to `MbSidSeMulti.cpp:179`).
  - Apply Lead's relative/absolute formula, then `parSet(wtAssign, parValue, sidlr, /*ins*/wt,
    /*scaleFrom16bit*/true)`.

Reading `sidlr` from the patch each step (rather than fixing `wtAssignLeftRight` at load time)
keeps the entire fix in one post-tick function with no load-path hooks. The cost is negligible —
the body only runs on a WT step tick (`wtOut >= 0`).

This handles **every** Multi `wtAssign` target (cutoff, volume, resonance, channels, mode, detune,
knobs) via the engine's own `parSet`, not just filter cutoff.

### 4.2 Why "after tick" is correct (timing)

- `MbSidEnvironment::tick()` → `MbSid::tick()` → `currentMbSidSePtr->tick()` runs the SE — and
  thus each `MbSidWt::tick()` — **exactly once per `env.tick()`**. `updateSpeedFactor` (=2) is
  consumed *inside* the clock/LFO/env math, **not** as an outer iteration loop. Therefore `wtOut`
  is written once and read by the helper before any overwrite — **lossless** for both clocked
  patches (`wtOut < 0` between steps, A107's case) and key-control patches (`wtOut >= 0` every
  tick, where the engine itself would re-apply identically).
- The helper writes `filterCutoff` *after* the tick generated this tick's register image, so the
  change lands in the **next** tick's image — a **1 ms (one control tick) lag**. This is inaudible,
  and because both the shim and the oracle apply the same helper, the two remain bit-identical.
- `filterCutoff` is a persistent base value (not reset per tick — the WT holds a value across many
  ticks between steps), so the post-tick write sticks and is consumed next tick. This is the same
  mechanism Lead's `parSetWT` uses.

### 4.3 Call sites

- **Firmware:** `fw/csrc/mbsid_shim.cpp` — inside `mbsid_tick()`, after `env.tick()`.
- **Oracle reference:** `host_oracle/oracle.cpp` — inside `OracleBackend::tick()`, after
  `env.tick()`.

Both must call it, or the oracle's shim-vs-engine diff diverges on A107 (the shim would gate the
filter while the reference wouldn't). `host_oracle/run_oracle.sh` already puts `-I$SHIM`
(`fw/csrc`) on the oracle compile, so `#include "mbsid_multi_wt.h"` works unchanged.

## 5. Validation

The oracle's shim-vs-engine diff is **structurally blind** to this fix: both `oracle` and
`shim_driver` compile/run the same logic (now including the same helper), so they agree before and
after. The real validator is a **reference-free trace assertion**:

- Run `oracle` on `0 patch 106` + `sequences/seq_multi.txt` (A107 Poly Trancegate).
- Trace format is `<t_ms> <L|R> <reg> <hexval>`. The 11-bit cutoff is split across **reg 21**
  (`filter_l` = FC_LO, low 3 bits, byte 0x15) and **reg 22** (`filter_h` = FC_HI, high 8 bits,
  byte 0x16) in the 32-byte `sid_regs_t`. A sweep's dynamic range lives mostly in FC_HI, and how
  `MbSidFilter` splits `filterCutoff` is not assumed — so the assertion counts changes to reg 21
  **or** 22.
- **Assert:** reg 21/22 changes **≥ 2 times** over the A107 trace, on both the L and R SID streams
  (A107 uses `wtAssignLeftRight = 3` = both; if `seq_multi.txt` only drives one side, fall back to
  a combined L+R count — see the plan's confirmation step).
- **Pre-fix:** cutoff static at 0 → ≤ 1 change event on any cutoff register → assertion FAILS.
- **Post-fix:** WT sequences the cutoff → many change events → assertion PASSES.

The existing `run_oracle.sh` gate (28/28 + Multi differential + 128-patch sweep) must remain green
— it proves no regression and no new crash (unhandled `par` is a safe `parSet` no-op; indexing is
bounded).

## 6. Acceptance criteria

- [ ] `mbsid_multi_wt_fixup()` mirrors Lead's WT→param logic, delegating to Multi's own
      `parSet`/`parGet`; recovers `sidlr` from `body.M.voice[wt][0x2b] >> 6`.
- [ ] Called after `env.tick()` from both `mbsid_shim.cpp::mbsid_tick()` and
      `oracle.cpp::tick()`.
- [ ] New reference-free assertion FAILS on the pre-fix tree and PASSES post-fix (reg 21/22 move
      on A107, L and R).
- [ ] `host_oracle/run_oracle.sh` still reports 28/28 OK + differential + 128-patch sweep.
- [ ] `cd gateware && pdm mbsid build` passes; post-route `sync` Fmax ≥ 60 MHz.
- [ ] **Manual / hardware:** A107 produces audible rhythmic filter-gated sound instead of silence
      (the one check no automation covers).

## 7. Risks

- **Scope:** contained to one new header + two one-line call sites + one assertion. Lead /
  Bassline / Drum untouched.
- **Crash risk:** low — replicates code Lead already runs on target; unhandled `par` is a no-op;
  filter indexing is `[0]`/`[1]` gated by `sidlr` bits.
- **Drift:** duplicates Lead's `parSetWT` formula (stable, pinned engine commit). If the pin moves,
  re-check the formula. Documented in the header comment.
- **1 ms modulation lag** vs a hypothetical engine-side fix: inaudible; identical on both oracle
  sides so it does not break parity.
