# MBSID Writable User Patch Banks — Design (M4, in progress)

**Date:** 2026-07-01
**Branch:** `mbsid-port`
**Status:** DRAFT — architecture and components agreed with user; edge cases, validation,
and footprint sections not yet specced (see "Deferred / not yet specced" below). Do not
implement from this doc until it is complete and re-reviewed.
**Scope of this doc:** design only. Builds on M1 (Lead mono), M2 (dual-SID stereo), M3
(read-only factory ROM bank, `M3_PATCH_BANKS.md`).

---

## 1. Goal & non-goals

**Goal.** Add a writable **User** patch bank (128 slots, flash-backed) alongside the
existing read-only **Factory** ROM bank, plus an on-device browse/save UI: browse either
bank via the existing encoder menu, and save the currently-loaded patch into a chosen User
slot ("save as" / duplicate).

**Why this shape.** M3 shipped a read-only factory bank only (`M3_PATCH_BANKS.md §8`
flagged "user banks (writable)" and "UI" as explicitly deferred). There is no on-device
patch editor, so "save" means **duplicating an already-loaded patch** (factory or another
user slot) into a user slot — not editing parameters on-device.

**How the save source was scoped (important context for anyone picking this up).** The
original ask included "MIDI SysEx patch upload from an editor" as a possible save source.
Investigation found:
- MBSID's SysEx protocol (`MbSidSysEx::cmdPatchWrite` → `MbSidEnvironment::sysexSetPatch`)
  already fully supports receiving a patch dump and applying it live
  (`toBank=false` branch — this is exactly what `mbsid_load_patch()` already calls).
- But persisting a SysEx-received patch (`toBank=true` → `MbSidEnvironment::bankSave()`) is
  an upstream **stub that returns `-2` "not supported yet"** — it does nothing. There is no
  upstream persistence to reuse.
- Worse, the gateware MIDI decoders (`tiliqua/midi/decode_serial.py`,
  `tiliqua/midi/decode_usb.py`, both shared modules used by other tops) **currently drop all
  SysEx bytes** (0xF0..0xF7) — there's an existing `# TODO: add a sideband stream for sysex
  messages` comment; nobody built the pass-through path yet.
- So "SysEx upload" requires new **shared gateware** (a sideband raw-byte stream out of both
  MIDI decoders + a new CSR FIFO), not just firmware work. That's a materially bigger,
  cross-cutting change.

**Decision (user-confirmed):** this spec covers **factory-duplicate only**. SysEx upload is
explicitly deferred to a **separate follow-on spec**, gated on the gateware sideband-stream
work existing first. See §8.

**Non-goals (this milestone):**
- MIDI SysEx patch upload (deferred — see above).
- On-device patch parameter editing.
- Multiple user banks (one 128-slot User bank only).
- Any change to the factory ROM bank or upstream `mios32` C++ (no upstream edits at all —
  the only new C++ is one shim function, §3a).

---

## 2. Architecture & data flow

```
Menu (fw/src/menu.rs, extended)
  Bank row:    A (Factory ROM, 128) | U (User, 128)
  Program row: 0-127, name from bankPatchNameGet (A) or flash-read name bytes (U)
  Save row:    destination user-slot cursor 0-127, shows existing name or "Empty"
               entering Edit = preview only; committing (see §3d) = flash write;
               a distinct cancel path = exit without writing

Load path (unchanged shape for either bank source):
  A: mbsid_bank_load(0, program)              [existing shim fn, upstream bankLoad]
  U: flash read 512B @ slot(program)
       -> if empty (magic/version check fails): render "Empty", do NOT load
       -> if populated: mbsid_load_patch(bytes)   [existing shim fn, sysexSetPatch toBank=false]

Save path (new):
  mbsid_current_patch_raw()   [ONE new shim fn: copies out env.mbSid[0].mbSidPatch.body,
                                same field mbsid_bank_load already reads out]
  -> flash write 512B @ slot(save_slot)   [new firmware-side flash module, §3c]
```

**Storage mechanism — flat sector-per-slot, not `sequential_storage`/KV.** The existing
generic option-persistence layer (`gateware/src/rs/opts/src/persistence.rs`,
`FlashOptionsPersistence<F>`) wraps `sequential_storage::map`, which is built for many
small, frequently-rewritten keys needing garbage collection. Our access pattern — 128 fixed
512-byte blobs, individually addressable, written rarely — doesn't fit that model well and
that module is welded to a 32-byte `DATA_BUFFER_SZ` and the `Options` trait anyway. Instead:
one 4 KiB erase sector per patch slot (128 × 4 KiB = 512 KiB total). Save = erase sector +
program 512 B. Load = read 512 B, check a magic/version prefix we write ourselves (upstream
`sid_patch_t` has no "is this slot used" concept) before ever handing bytes to the engine —
an unwritten (erased, `0xFF`-filled) slot must never reach `mbsid_load_patch`.

**Region placement.** `0x900000`–`0x1000000` (~7 MiB) is unused by the existing flash
layout: bootloader occupies `[0x0, 0x100000)`, up to 8 bitstream slots occupy
`[0x100000, 0x900000)` at 1 MiB each (`gateware/src/rs/manifest/src/lib.rs`,
`spiflash_layout.py`). We carve `0xF00000..0xF80000` (512 KiB) from that tail for the User
bank. **Verified:** `pdm run flash` (`gateware/src/tiliqua/flash/__init__.py:66`,
`compute_concrete_regions_to_flash`) only erases/programs the target slot's own manifest
regions — never a full-chip erase, and option-storage erasure is opt-in
(`--erase-option-storage`). So a fixed address in the unused tail persists across normal
reflashes. It does **not** survive a full chip erase (out of scope to solve here — worth
flagging as a known limitation, not a bug).

---

## 3. Components & interfaces

### 3a. Shim — `fw/csrc/mbsid_shim.cpp` (one new function)
```c
void mbsid_current_patch_raw(uint8_t *buf512);  // copies env.mbSid[0].mbSidPatch.body out
```
Mirrors the existing raw-copy already done inside `mbsid_bank_load`
(`fw/csrc/mbsid_shim.cpp:82`). No upstream `.cpp`/`.h` edits.

### 3b. FFI — `fw/src/mbsid_sys.rs`
Add `current_patch_raw() -> [u8; 512]`, riscv `extern "C"` block + host stub, matching the
existing pattern for every other function in this file.

### 3c. New flash module — `fw/src/patch_store.rs` (new file)
```rust
const USER_PATCH_FLASH_RANGE: Range<u32> = 0xF00000..0xF80000; // 512 KiB, unused tail
const SLOT_SIZE: u32 = 4096;      // one erase sector per patch
const MAGIC: [u8; 4] = *b"MBUP";  // marks a slot as populated (vs erased 0xFF)

pub struct UserPatchStore<F> { flash: F }
impl<F: NorFlash> UserPatchStore<F> {
    pub fn load(&mut self, slot: u8) -> Option<[u8; 512]>;   // None if empty/bad magic
    pub fn save(&mut self, slot: u8, patch: &[u8; 512]) -> Result<(), Error>;  // erase+program
    pub fn name(&mut self, slot: u8) -> Option<[u8; 16]>;    // reads name bytes only, no full load
}
```
Generic over `F` so a host test can inject an in-memory mock — this is the save/load
keystone test (flash can't run in the host oracle).

### 3d. `menu.rs` changes
- `Row` gains `Save`; `MenuState` gains `save_slot: u8`; `bank` semantics extend to
  `0 = Factory, 1 = User`.
- `on_turn` on the Save row behaves like Program (Edit-mode scroll 0-127, clamped) but
  never triggers a patch **load** — it triggers a **preview** (query
  `patch_store.name(save_slot)` for display) instead.
- `on_press` is special-cased for the Save row: the **Edit → Nav transition specifically on
  `Row::Save`** is the commit point (do the flash write). A **Nav → Edit** transition, or a
  distinct cancel path, does not write. Concretely, `on_press` returns
  `PressResult { None, Commit, Cancel }` computed from `(focus, mode)` before toggling, so
  the write happens exactly once, only on deliberate confirmation — never on every encoder
  tick while previewing a destination slot.
- Bank/Program `on_turn` for the User bank must check `patch_store.load(program)` is `Some`
  before calling `mbsid_load_patch` — on `None`, render "Empty" and skip the load entirely
  (the empty-slot safety guard; same class of concern as M3's non-Lead-patch safety
  argument in `M3_PATCH_BANKS.md §4`).

### 3e. `main.rs` wiring
- Save-row commit: `mbsid_sys::current_patch_raw()` → `patch_store.save(slot, &bytes)`.
- Bank=User load: `patch_store.load(program)` → `mbsid_sys::load_patch(&bytes)` (existing
  M1-era function) instead of `mbsid_bank_load` (which is ROM-only; `SID_BANK_NUM` stays `1`
  upstream and is never touched).

---

## 4. Deferred / not yet specced (do not implement yet)

The following sections still need to be worked through with the user before this doc is
implementation-ready:

- **Edge cases & error handling.** At minimum, still needs explicit treatment of: flash
  write failure mid-save (torn write — erase succeeded, program failed/partial), what
  happens if Save is entered while the currently-loaded patch is itself from an empty/never-
  loaded state, whether saving is allowed while a non-Lead engine patch is active, and the
  Save-row cancel UX in more detail (the doc above states the *rule* — commit only on
  Edit→Nav — but the concrete cancel gesture, e.g. long-press vs. re-entering Nav via the
  Bank/Program rows, isn't chosen yet).
- **Validation / acceptance tests.** Not yet written: host-side tests for `patch_store`
  (mock flash — save/load round-trip, empty-slot detection, overwrite), extended `menu.rs`
  host tests for the Save row state machine (slot pick, commit, empty display, cancel),
  and a hardware acceptance checklist (save a factory patch to a user slot, power-cycle,
  confirm it reloads; confirm reflashing a bitstream slot doesn't clobber user patches).
- **Footprint.** New flash region size is chosen (512 KiB) but the actual `.bss`/stack
  impact of `patch_store.rs` + any buffering hasn't been checked against the `0x8000`
  mainram budget noted in `CLAUDE.md`.
- **Documentation follow-ups.** `DESIGN.md §10` and `M3_PATCH_BANKS.md §8` should be updated
  to point at this doc once it's final.

---

## 5. Forward-compatibility (later milestones — NOT built now)

- **MIDI SysEx patch upload.** Blocked on new shared gateware: a sideband raw-byte SysEx
  stream out of `MidiDecodeSerial`/`MidiDecodeUSB` (both currently drop SysEx bytes
  entirely) plus a new CSR FIFO to carry it to firmware. Once that exists, the firmware
  side is small: one new shim function forwarding bytes to `env.midiReceiveSysEx()`
  (already reachable — `env` in `mbsid_shim.cpp` is already an `MbSidEnvironment`), feeding
  a byte-level receive loop that lands received patches into a User slot via the same
  `patch_store.save()` built here.

---

## 6. Reference pointers

- Factory bank (read-only): `M3_PATCH_BANKS.md`.
- Upstream stub confirming no persistence to reuse:
  `mios32/apps/synthesizers/midibox_sid_v3/core/MbSidEnvironment.cpp:96-105` (`bankSave`),
  `:253-266` (`sysexSetPatch`).
- Upstream SysEx receive protocol (usable once gateware sideband exists):
  `mios32/apps/synthesizers/midibox_sid_v3/core/MbSidSysEx.cpp` (`cmdPatchWrite`).
- SysEx currently dropped in gateware: `gateware/src/tiliqua/midi/decode_serial.py:98`,
  `gateware/src/tiliqua/midi/decode_usb.py:46`.
- Existing raw-patch-copy-out precedent: `fw/csrc/mbsid_shim.cpp:79-90` (`mbsid_bank_load`).
- Existing generic flash-KV pattern (not reused, but the precedent for
  `NorFlash`/`MultiwriteNorFlash` HAL usage): `gateware/src/rs/opts/src/persistence.rs`.
- Flash layout / manifest / slot regions: `gateware/src/rs/manifest/src/lib.rs`,
  `gateware/src/tiliqua/flash/spiflash_layout.py`, `gateware/src/tiliqua/flash/__init__.py`.
- Menu to extend: `fw/src/menu.rs`.
