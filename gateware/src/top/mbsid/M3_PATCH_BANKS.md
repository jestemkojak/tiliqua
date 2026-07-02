# MBSID Patch Banks — Design (M3, milestone 1: read-only factory bank)

**Date:** 2026-06-28
**Branch:** `mbsid-port`
**Status:** Implementation complete (MIDI Program Change → factory bank; boot A124; 12/12 oracle; 128-patch no-crash sweep).
**Scope of this doc:** design only — interfaces and acceptance tests, so implementation is
mechanical. Builds on the M1 (Lead, mono) and M2 (dual-SID stereo) work; see `DESIGN.md`
and `M2_DUAL_SID.md`.

---

## 1. Goal & non-goals

**Goal.** Make all 128 MBSID factory "vintage bank" patches selectable **live over MIDI
Program Change**, headless, by reusing the engine's native `bankLoad` path. This is the
deferred-roadmap "patch bank storage" item (`DESIGN.md §10`), scoped to its first slice: a
**read-only, ROM-baked default bank**.

**Why this shape.** The factory bank already exists verbatim in the vendored engine as
`sid_bank_preset_0[128][512]` (`sid_bank_preset_a.inc`, `#include`d by
`MbSidEnvironment.cpp`). The engine also already ships `MbSidEnvironment::bankLoad(sid, bank,
patch)` and wires MIDI Program Change → `bankLoad` in `MbSidEnvironment::midiReceive`. So
"porting patch banks" is mostly **plumbing the existing engine bank path through to the
firmware**, not new storage or engine work.

**Non-goals (deferred to later milestones, in roadmap order):**
- Runtime-**writable** user banks (flash erase/program, SysEx upload, on-device save/edit).
- Any **UI** (encoder / display / patch-name browse) — Program Change only this milestone.
- Multiple banks (`SID_BANK_NUM` stays `1`).
- Making non-Lead (Bassline / Drum / Multi) patches sound **correct** — see §4.

---

## 2. Architecture & data flow

The factory bank `sid_bank_preset_0` and `MbSidEnvironment::bankLoad` are **presently
`--gc-sections`-stripped** from the firmware ELF, because nothing in the firmware references
them (the firmware hand-parses MIDI and calls granular shim functions, never
`env.midiReceive`). Adding **one shim function that calls `bankLoad`** un-strips both, giving
us the 1:1 factory bank for free. No bank data is authored or duplicated.

```
MIDI in (CSR FIFO) ─► firmware parse ─► MidiMessage::ProgramChange(_ch, prog)
                                            └─► mbsid_sys::program_change(prog)
                                                  └─► [shim] env.bankLoad(0, 0, prog & 0x7F)
                                                        └─► copyToPatch + updatePatch()  (live re-patch)

note-on/off, CC, pitch-bend ─► granular shim fns (unchanged from M1/M2)
Timer0 1 kHz tick ─► mbsid_tick ─► sid_regs_t L/R ─► RegDiff L/R ─► two SIDs   (unchanged from M2)
```

Everything except the new Program Change arm + the new shim function is reused unchanged from
M2.

---

## 3. Components & interfaces

### 3a. Shim — `fw/csrc/mbsid_shim.cpp` / `mbsid_shim.h`
Add exactly one `extern "C"` function:

```c
void mbsid_program_change(uint8_t patch);   // env.bankLoad(0, 0, patch & 0x7F)
```

- `bankLoad` already clamps `bank >= SID_BANK_NUM` (returns −2) and `patch >= 128` (returns
  −3); we mask `patch & 0x7F` so every MIDI Program Change value (0–127) maps **1:1** onto a
  bank slot.
- Return value is ignored (headless; nothing to report).
- Referencing `bankLoad` is what pulls `sid_bank_preset_0` (64 KB) and the bank code back into
  the link.

### 3b. FFI — `fw/src/mbsid_sys.rs`
Mirror the existing pattern:
- `extern "C" { fn mbsid_program_change(patch: u8); }` (riscv-gated block).
- `pub fn program_change(patch: u8)` wrapper.
- `#[cfg(not(target_arch = "riscv32"))]` host **stub** (no-op), so host `cargo test --lib`
  links.

### 3c. Firmware MIDI parse — `fw/src/main.rs`
Add one match arm alongside the existing NoteOn / NoteOff / PitchBend / ControlChange arms
(`midi-types-0.1.7` exposes `ProgramChange(Channel, Program)`):

```rust
MidiMessage::ProgramChange(_ch, prog) => mbsid_sys::program_change(prog.into()),
```

Program Change is accepted on **any** MIDI channel (single-timbre, headless).

### 3d. Boot patch
Replace the M1/M2 boot load:

```rust
mbsid_sys::init();
mbsid_sys::load_patch(&PATCH);          // before
```
```rust
mbsid_sys::init();
mbsid_sys::program_change(BOOT_PATCH_INDEX);   // after; BOOT_PATCH_INDEX = 123 (A124 "Crazy Lead")
```

`BOOT_PATCH_INDEX` is the **0-based bank slot** (= MIDI Program Change value = patch number − 1).
**Constraint:** it must point at a **Lead** slot, or the synth boots with a wrong-sounding
non-Lead patch; document this on the constant. A124 "Crazy Lead" → index 123 is Lead.

**Load mechanism is identical to M2's boot.** The current boot's `load_patch` →
`MbSid::sysexSetPatch(p)` runs exactly `mbSidPatch.copyToPatch(p); updatePatch(false);` — the
*same two lines* `bankLoad` runs. The full engine init already happened in `MbSid::init` →
`updatePatch(true)` during `mbsid_init`, before either call. So boot timing, engine init, and
the first-tick register image come from identical code; **only the patch source changes**
(hand-copied array → bank slot). This unifies boot and Program Change on one path.

**Retire `fw/src/patch.rs`** — its hand-copied 512-byte image (A052 "Nice Lead") is just a row
of the bank we now link; delete the array, its `pub mod patch`, and the `use …::PATCH` import.
`mbsid_load_patch` / `mbsid_sys::load_patch` lose their firmware caller but are **kept**: the
host oracle's bit-exact harness still loads raw 512-byte patches through that primitive.

---

## 4. Edge cases & behavior

- **Non-Lead patch selected (Bassline / Drum / Multi).** `MbSid::updatePatch` switches
  `currentMbSidSePtr` to the corresponding SE and re-inits MIDI voices. **Verified safe:** all
  four SEs are linked into the ELF (24–26 symbols each; referenced via `&mbSidSe*` +
  virtual dispatch), so this routes to a real, constructed engine and **does not freeze the
  SoC**. It may sound wrong or silent because the firmware feeds only Lead-style channel-0 note
  events. **Accepted and documented** for this milestone.
- **Engine switch leaving stuck notes.** Selecting a non-Lead patch then returning to a Lead
  patch re-runs `updatePatch`'s voice re-init; a held note may hang. Acceptable this milestone;
  note in user-facing docs.
- **Live re-patch while playing.** `updatePatch` wraps the engine swap in
  `MIOS32_IRQ_Disable`/`Enable` (atomic). Program Change is handled in the same firmware
  MIDI-drain context as note events, so no new concurrency is introduced.
- **Boot sound.** The new boot path is identical engine code to M2's (`copyToPatch` +
  `updatePatch(false)`; see §3d), so the only change is the chosen patch (A124 "Crazy Lead").
  Validation (§6) confirms the boot patch sounds correctly.

---

## 5. Footprint

- **~64 KB added `.rodata`** (128 × 512). It lands in the **flash / `.text` region (~2.1 MB)**,
  **not** the 32 KB `.bss` / mainram (`mainram_size = 0x8000`). No `mainram_size` change needed.
- **No new CSRs** → no PAC regeneration.
- **No gateware change** → `sync` Fmax unaffected (expect unchanged ~67 MHz).

---

## 6. Validation (acceptance tests)

1. **Host oracle — no-crash sweep (new).** Load each of the 128 factory patches via
   `program_change`, tick N frames, assert the shim never hangs or faults. De-risks the
   "non-Lead won't freeze" claim entirely on PC.
2. **Host oracle — bit-exact (extend the existing keystone).** For ≥3 **Lead** factory indices,
   drive `program_change(i)` + a note sequence through both the host shim and the JUCE oracle
   (oracle doing the same `bankLoad(0, 0, i)`); the **L and R** register streams must be
   byte-identical.
3. **Host `cargo test --lib`.** The `program_change` host stub and the `ProgramChange` parse
   arm compile and route.
4. **Build.** `pdm mbsid build` green; report post-route `sync` Fmax (expect unchanged — no
   gateware delta).
5. **Hardware.** Boot plays A124 "Crazy Lead" (the new `BOOT_PATCH_INDEX = 123`); send Program
   Change across several indices and confirm at least one *other* Lead index audibly changes
   timbre.

---

## 7. Documentation corrections (part of this work)

- **Correct the "dead-stripped" claim.** Both `gateware/src/top/mbsid/CLAUDE.md` and `DESIGN.md`
  state the non-Lead SEs are "dead-stripped by `--gc-sections` (the engine aggregates them by
  value)." This is **false**: they are referenced via `&mbSidSeBassline/Drum/Multi` +
  virtual dispatch in `MbSid::updatePatch`, and appear in the linked ELF (24–26 symbols each).
  The patch-bank safety argument (§4) depends on this being correct, so fix both docs.
- **`DESIGN.md §10`.** Mark "Patch bank storage" as having its read-only factory-bank slice done;
  note that writable user banks and UI remain deferred.

---

## 8. Forward-compatibility (later milestones — NOT built now)

**Implemented in M4** — see `M4_USER_PATCH_BANKS.md`: writable user bank 1 (flat
sector-per-slot flash store, not the `SID_BANK_NUM`/manifest-region sketch below),
the on-device browse/save UI, and MIDI SysEx patch upload. Left below for history.

- **User banks (writable).** Raise `SID_BANK_NUM`; back bank `1..` with a flash **manifest
  region** (Tiliqua already has the region system + `SpiFlash` erase/program HAL). Read 512 B
  from flash → `copyToPatch`. The shim grows `mbsid_select_bank` + write entry points; the
  `program_change` selection path is unchanged.
- **UI.** `MbSidEnvironment::bankPatchNameGet` (already in the engine) yields 16-char patch
  names when the UI milestone lands. This design deliberately does **not** reference it, to
  avoid pulling `sprintf` into the link.

---

## 9. Reference pointers

- Engine bank API: `mios32/apps/synthesizers/midibox_sid_v3/core/MbSidEnvironment.cpp`
  (`bankLoad`, `sysexSetPatch`, `midiReceive` Program Change path), `sid_bank_preset_a.inc`
  (`sid_bank_preset_0[128][512]`).
- Engine dispatch (non-Lead SE linkage): `MbSid.cpp` `updatePatch` (lines ~147–231).
- Shim: `fw/csrc/mbsid_shim.cpp` / `.h`. FFI: `fw/src/mbsid_sys.rs`. MIDI parse + boot:
  `fw/src/main.rs`. Current boot patch: `fw/src/patch.rs` (to be retired).
- Host oracle keystone: `host_oracle/run_oracle.sh`.
