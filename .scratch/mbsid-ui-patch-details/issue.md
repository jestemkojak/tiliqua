---
title: "feat(mbsid): display patch engine and key config on UI"
status: ready-for-human
created: 2026-06-29
---

## What to build

The current MBSID UI shows patch name, bank, and program number via the encoder menu. It does not surface any patch metadata that helps the player understand how to use the patch. Add a second line (or card) to the display that shows the engine type and the most useful per-patch config at a glance.

**Minimum useful fields:**

- **Engine**: Lead / Bassline / Drum / Multi (decoded from patch byte 0x10)
- **MIDI channel map**: depends on engine —
  - Lead/Drum: Ch 1
  - Bassline: Ch 1 (note < 60) / Ch 2 (note ≥ 60)
  - Multi: Ch 1–3 → L SID, Ch 4–6 → R SID
- **Voice mode** (Lead only): Mono / Poly / Legato (from `v_flags` at byte 0x50)

Optional / stretch:

- Filter cutoff/resonance summary
- WT active indicator (any voice has `wtAssign != 0`)

**Implementation notes:**

- Patch bytes are accessible via `mbsid_sys::bank_patch_name` already gives the name. A new shim function `mbsid_patch_engine(bank, patch) -> u8` (reads byte 0x10 from the raw patch buffer without loading it) would avoid touching engine state. Alternatively decode from the name-buf approach if the patch is already loaded.
- The display already uses `menu::draw` (`fw/src/menu.rs`). Extend `MenuState` or add a second draw call for the metadata line.
- Reference display pattern: `top/macro_osc` for the card/page layout (`tiliqua_lib::ui`/`draw`).
- Keep it read-only — no new CSRs needed, no PAC regen.

## Acceptance criteria

- [ ] After loading any patch, the display shows engine type (Lead/Bassline/Drum/Multi) without additional user input
- [ ] MIDI channel map hint is shown (at least engine-level, e.g. "Ch 1" for Lead)
- [ ] Display updates correctly when scrolling through patches with the encoder
- [ ] No regression in audio path — display rendering must not extend the critical section or add latency to the 1 kHz ISR
- [ ] Bitstream builds and sync Fmax ≥ 60 MHz

## Blocked by

None — can start immediately.

Done: patch-detail display implemented; build Fmax = 68.27 MHz (PASS)
