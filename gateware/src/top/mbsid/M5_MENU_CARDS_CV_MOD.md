# MBSID Menu Cards: CV Modulation + On-Device Patch Editing — Design (M5)

**Date:** 2026-07-03
**Branch:** `mbsid-port`
**Status:** IMPLEMENTED (2026-07-03, commits a645709..97d6b25 plus this docs
commit) — host tests + oracle green; hardware bring-up pending (§8 checklist).
**Scope:** builds on M1–M4 (`M4_USER_PATCH_BANKS.md`). Zero gateware changes,
zero CSR changes (no `--pac-only`), no upstream C++ edits. Everything is
firmware (menu/ISR/flash), five thin shim additions, and oracle/driver
extensions. Chosen-default details are marked **[DEFAULT]**.

**Implementation deviations from this design (confirmed intentional):**
- The pitch quantizer (`fw/src/cv.rs`'s `quantize_semitone`/`PITCH_SPAN`/
  `PITCH_HYST`) is a closed-form integer hysteresis routine, not the 61-entry
  lookup table this design originally sketched — same semitone-quantization
  semantics (V/oct tracking with boundary hysteresis to prevent flutter),
  satisfying the "no f32, integer only" constraint via an equivalent but more
  compact mechanism.
- `MENU_H` (`fw/src/menu.rs`) grew from 194 to 244 (during Task 8) to fit the
  Patch Edit card's scrolling parameter window on screen.

---

## 1. Goal & non-goals

**Goal.** Grow the single-card patch browser into a three-card menu:

1. **CV Mod card** — assign each of the 4 Eurorack CV inputs to a modulation
   target chosen from a list. Targets are **MBSID patch-layer parameters**
   (engine knobs and the parSet common block) plus Pitch/Gate note control —
   never raw SID registers (user decision 2026-07-03).
2. **Patch Edit card** — edit a curated set of Lead-engine patch parameters
   on-device, audible live, with a Save row that writes the edited patch into
   the flash User bank (reusing the M4 save path verbatim).
3. **Settings persistence** — the 4 CV assignments (+ MIDI Src) survive a
   power cycle, stored in the option-storage flash window `top.py` already
   reserves.

**Non-goals.**
- No editing UI for Bassline/Drum/Multi patches (card is disabled for them;
  the table-driven design leaves room to add per-engine tables later).
- No full patch editor (the Lead patch is ~500 significant bytes; we ship a
  curated ~28-row subset). SysEx from the MIDIbox SID Editor remains the
  full-surface editing route (M4).
- No new modulation engine in firmware: all modulation routing/scaling is the
  upstream engine's (`knobSet`/`parSet`/`sysexSetParameter`). Firmware only
  samples CV and calls the right entry point (per the porting rule: reuse
  upstream, don't reimplement).
- No opts/ui framework migration. The hand-rolled `menu.rs` state machine is
  extended (user decision; keeps the 41 host tests and the bespoke
  bank/program/save semantics).

---

## 2. Source-verified constraints (why the design is shaped this way)

All verified against the pinned mios32 checkout
(`44d8e6af…`, paths relative to `mios32/apps/synthesizers/midibox_sid_v3/core/`):

- **`MbSid::sysexSetParameter(u16 addr, u8 data)`** (`MbSid.cpp`) writes
  `mbSidPatch.body.ALL[addr] = data` **and** forwards to
  `currentMbSidSePtr->sysexSetParameter` for a live engine update. This is the
  patch-edit primitive: edits are instantly audible *and* land in the patch
  body, so the existing M4 save path (`mbsid_current_patch_raw` →
  `UserPatchStore::save`) captures them with no extra plumbing. Returns false
  on invalid address (≥512 or engine-rejected).
- **`knobSet` / `parSet` / `parGet` are `virtual` on `MbSidSe`**
  (`MbSidSe.h:130–137`, non-pure with empty default bodies) — dispatching via
  `currentMbSidSePtr` is engine-generic and crash-safe on all four engines.
- **Knobs are the patch's own modulation matrix**: `mbsid_knob_t {assign1,
  assign2, value, min, max}` × 8 at patch offset `0x18` (`knob[8][5]`,
  `MbSidStructs.h:672`). `knobSet(k, v)` stores the value and applies it
  through the patch's assign/min/max scaling via `parSet` — the exact path CC1
  (mod wheel) already takes (`MbSidSeLead.cpp:348`). CV→knob therefore
  modulates *what the patch says the knob modulates*, at the depth the patch
  says.
- **`parSet` common block is engine-shared and non-destructive**: par
  `0x01`=Volume, `0x02`=OSC Phase, `0x03`=OSC Detune, `0x04`=Filter CutOff,
  `0x05`=Filter Resonance (verified in `MbSidSeLead.cpp::parSet`; same low
  block in Bassline). With `scaleFrom16bit=true` it takes a full-range u16.
  Filter/volume pars write **runtime** objects (`mbSidFilter[..]`), NOT the
  patch body — correct for modulation (never dirties the patch), which is why
  the Patch Edit card must use `sysexSetParameter` instead.
- **Lead patch layout** (`sid_se_patch_t` union, `MbSidStructs.h:659+`,
  offsets confirmed against the `.L` struct):
  - `0x50` v_flags, `0x51` osc_detune, `0x52` volume (4 bits used),
    `0x53` osc_phase
  - `0x54..0x5F` `filter[2][6]` L then R: `+0` chn_mode (channels lo-nibble,
    mode hi-nibble), `+1` cutoff_l, `+2` cutoff_h (12-bit total; cutoff_l bit 7
    = FIP interpolation flag), `+3` resonance (hi nibble used), `+4` keytrack
  - `0x60..0xBF` `voice[6][16]`, voices 0–2 = Left SID, 3–5 = Right:
    `+0` flags, `+1` waveform, `+2` ad (A hi-nibble, D lo), `+3` sr (S hi, R
    lo), `+4/+5` pulsewidth 12-bit, `+8` transpose, `+9` finetune,
    `+11` portamento, `+12` arp_mode
  - `0xC0..0xDD` `lfo[6][5]`: `+0` mode, `+1` depth, `+2` rate, `+3` delay,
    `+4` phase
- **Engine byte** is patch body `0x10` (header is engine-common) — gates the
  edit card and is already what `bank_patch_info` reports.
- **CV inputs are already reachable with zero gateware work**: `SIDSoc`
  instantiates `PMOD0_PERIPH`; `top/sid`'s firmware reads calibrated inputs
  via `EurorackPmod0::sample_i()` (i16, 4096 counts/volt) and loads
  calibration constants over I2C1 at boot (`top/sid/fw/src/main.rs:343-344`).
  mbsid's `bitstream_help` marks inputs 0–3 unused — they become the 4 CV mod
  inputs.
- **Option-storage window exists but is unused**: mbsid's `top.py` archiver
  already calls `with_option_storage()`; `bootinfo.manifest
  .get_option_storage_window()` returns the flash window. mbsid doesn't use
  the opts framework, so we hand-roll a tiny record there (§6d).

---

## 3. Architecture & data flow

```
                         main loop (UI, flash)                1 kHz Timer0 ISR (audio-rate)
                ┌────────────────────────────────┐   ┌─────────────────────────────────────┐
 encoder ──────►│ menu.rs: Card{Main,CvMod,Edit} │   │ midi_read drain ──► engine (M1–M4)  │
                │  CvMod row edit ─► cv_targets ─┼──►│ sysex drain     ──► engine + capture│
                │  Edit row edit ──► (cs) ───────┼──►│ CV: pmod.sample_i() ─► per-input:   │
                │     mbsid_sysex_param(addr,val)│   │   Knob1..5 ─► mbsid_knob_set        │
                │  Save row ─► current_patch_raw │   │   Vol/Phase/Det/Cut/Res ─►          │
                │      ─► UserPatchStore (M4)    │   │                    mbsid_par_set    │
                │  settings dirty ─► debounce ──►│   │   Pitch/Gate ─► note_on/note_off    │
                │      settings_store (flash)    │   │ mbsid_tick ─► RegDiff L/R ─► SIDs   │
                └────────────────────────────────┘   └─────────────────────────────────────┘
```

- The ISR reads a copy of the 4 CV target assignments from the shared `App`
  (already `Mutex<RefCell<_>>`-guarded); the main loop updates them on menu
  edit. All engine calls stay inside the existing `critical_section` blocks —
  same concurrency pattern as M4's `load_patch`/`current_patch_raw`.
- **Precedence rule (documented, by design):** if a CV input modulates the
  same runtime parameter the user is editing on the Patch Edit card, the CV
  wins audibly *while it's actively changing* (it rewrites the runtime value
  at up to 1 kHz) — but CV targets are deadbanded on their 8-bit value, so
  once the CV settles a subsequent menu edit takes effect live until the CV
  next moves past its resolution step. Either way, the menu edit is what's in
  the patch **body** and therefore what Save persists. Likewise CV→Knob1 and
  the MIDI mod wheel (CC1) share a knob: last-writer-wins, exactly as two
  MIDI controllers would.

---

## 4. Shim + FFI (no upstream edits)

Five additions to `fw/csrc/mbsid_shim.cpp` / `mbsid_shim.h`, mirrored in
`fw/src/mbsid_sys.rs` (cfg-stubbed on host, as all existing entries are):

```c
void mbsid_knob_set(uint8_t knob, uint8_t value);
    // env.mbSid[0].currentMbSidSePtr->knobSet(knob & 7, value)
void mbsid_par_set(uint8_t par, uint16_t value16);
    // env.mbSid[0].currentMbSidSePtr->parSet(par, value16,
    //                                        /*sidlr=*/3, /*ins=*/0,
    //                                        /*scaleFrom16bit=*/true)
int  mbsid_sysex_param(uint16_t addr, uint8_t data);
    // env.mbSid[0].sysexSetParameter(addr, data); returns 0/1
uint8_t mbsid_patch_byte(uint16_t addr);
    // env.mbSid[0].mbSidPatch.body.ALL[addr & 0x1FF] (value display/read-back)
uint8_t mbsid_current_engine(void);
    // env.mbSid[0].mbSidPatch.body.ALL[0x10] (gates the edit card)
```

Shim MIDI-ABI rule from `CLAUDE.md` applies: any signature change must update
Rust FFI **and both oracle drivers** together. These are additions, so the
existing 28/28 suite is unaffected; new sequence commands (§8) exercise them.

---

## 5. Firmware — menu (`fw/src/menu.rs`)

### 5a. Card layer

```rust
pub enum Card { Main, CvMod, PatchEdit }
```

- Row 0 of **every** card is a `Card` selector row: focus it, press to Edit,
  turn to cycle `Main ↔ CvMod ↔ PatchEdit`, press to confirm — the same
  Nav/Edit idiom as every other row, no new gestures **[DEFAULT]** (the
  encoder HAL exposes no long-press).
- `MenuState` gains `card: Card`, per-card focus (`focus` becomes a row index
  scoped to the current card), `edit_scroll: u8` (Patch Edit scroll offset),
  `cv_targets: [CvTarget; 4]`, and `edited: bool` (dirty star).
- Existing Main rows shift down one: `Card, Bank, Program, Save, MidiSrc`.
  Existing tests keep their semantics with focus indices adjusted.
- Switching cards never triggers a patch load, save, or engine call.

### 5b. CV Mod card

Rows: `Card, CV1, CV2, CV3, CV4`. Each CV row edits one enum value:

```rust
pub enum CvTarget {
    Off,                                    // default
    Knob1, Knob2, Knob3, Knob4, Knob5,      // -> mbsid_knob_set (patch matrix)
    Volume, Phase, Detune, Cutoff, Reso,    // -> mbsid_par_set 0x01..0x05
    Pitch, Gate,                            // -> engine note on/off, MIDI ch 1
}
```

Turning while in Edit steps through the list (clamped ends **[DEFAULT]**, like
the Bank row). Any change marks settings dirty (§6d) and is picked up by the
ISR on its next tick.

### 5c. Patch Edit card

Rows: `Card`, then the parameter table (scrollable), then `Save`.

- **Gating:** rows are active only when `mbsid_current_engine() == 0` (Lead).
  Otherwise the card draws a single dim line `Lead patches only` between the
  Card and Save rows, and Save still works (saving a non-Lead patch unedited
  is harmless and matches M4's save-anything behavior) **[DEFAULT]**.
- **Scroll window:** the box shows the Card row, up to 6 parameter rows, and
  a footer line. `edit_scroll` follows focus (focus moves past the window edge
  → window shifts by one). `MENU_H` grows only if needed; prefer keeping the
  current 380×194 box **[DEFAULT]**.
- **Parameter table** — `const`, in flash, one entry per row:

```rust
struct ParamDesc {
    label: &'static str,
    addr: u16,          // Lead patch body offset (primary / Left)
    mirror: Option<u16>, // Right-SID twin written with the same value
    shift: u8, mask: u8, // sub-byte fields (nibbles); 0xFF = whole byte
    max: u8,             // displayed range 0..=max
    wide: bool,          // 12-bit little-endian pair at addr/addr+1 (PW, cutoff)
}
```

  Curated Lead set (~28 rows) **[DEFAULT]**:

  | Rows | addr (mirror) | Notes |
  |---|---|---|
  | Volume | `0x52` | 0–15 |
  | OSC Detune | `0x51` | 0–255 |
  | OSC Phase | `0x53` | 0–255 |
  | Flt Cutoff | `0x55` (`0x5B`), wide | 12-bit, coarse encoder step 16; preserve cutoff_l bit 7 (FIP) |
  | Flt Reso | `0x57` (`0x5D`), hi nibble | shown 0–15 |
  | Flt Mode | `0x54` (`0x5A`), hi nibble | 0–15 bitmask LP/BP/HP/EXT |
  | Flt Chn | `0x54` (`0x5A`), lo nibble | voice-routing bitmask |
  | OSC1–3 Wave | `0x61/0x71/0x81` (`0x91/0xA1/0xB1`) | waveform byte |
  | OSC1–3 Atk/Dec | `0x62/0x72/0x82` (+0x30), hi/lo nibbles | 2 rows per OSC |
  | OSC1–3 Sus/Rel | `0x63/0x73/0x83` (+0x30), hi/lo nibbles | 2 rows per OSC |
  | OSC1–3 PW | `0x64/0x74/0x84` (+0x30), wide | 12-bit, coarse step 16 |
  | OSC1–3 Porta | `0x6B/0x7B/0x8B` (+0x30) | 0–255 |
  | LFO1/2 Rate | `0xC2` / `0xC7` | 0–255 |
  | LFO1/2 Depth | `0xC1` / `0xC6` | 0–255 |

  OSC rows write voice *n* and voice *n+3* so the L/R SIDs stay mirrored, the
  same invariant factory Lead patches hold (stereo detune comes from
  `osc_detune`, not divergent voice params) **[DEFAULT]**.

- **Edit path:** on value change, main loop calls (under `critical_section`)
  `mbsid_sysex_param(addr, byte)` — for `wide` rows two calls, low byte first;
  for nibble rows read-modify-write via `mbsid_patch_byte`. Sets
  `edited = true` (drawn as `*` after the patch name on all cards; cleared on
  patch load and successful save).
- **Read-back:** row values render from `mbsid_patch_byte(addr)` on redraw —
  the body is the single source of truth, so SysEx RAM Writes arriving
  mid-edit display correctly.
- **Save row:** identical semantics and shared code with the Main card's Save
  row (Cancel-first cursor, `PressResult::Commit(slot)`,
  `current_patch_raw` → `UserPatchStore::save`, status line). Volatility is
  the existing M4 contract: unsaved edits are discarded by any patch load.

---

## 6. Firmware — main loop & ISR (`fw/src/main.rs`)

### 6a. Boot additions

- Bind `EurorackPmod0::new(peripherals.PMOD0_PERIPH)` and load calibration
  constants via I2C1 (`calibration::CalibrationConstants::load_or_default`,
  copied from `top/sid`) — required for accurate V/oct.
- Load the settings record (§6d) before constructing `MenuState`; apply
  `midi_src` and `cv_targets` from it.

### 6b. ISR CV sampling (in `timer0_handler`, before `mbsid_tick`)

Per tick, read `sample_i()` once → `x: [i16; 4]`; for each input with a
non-`Off` target:

- **Knob1–5:** `v8 = clamp(x, 0, 20480) * 255 / 20480` (0–5 V unipolar →
  0–255 **[DEFAULT]**); call `mbsid_knob_set` only when `v8` changed since the
  last call (8-bit quantization is the deadband).
- **Volume/Phase/Detune/Cutoff/Reso:** `v16 = clamp(x, 0, 20480) * 65535 /
  20480`; call `mbsid_par_set(0x01..0x05, v16)` only when the top 8 bits
  changed (deadband against ADC noise) **[DEFAULT]**.
- **Pitch:** `note = clamp(36 + round(12 * volts), 0, 127)` (0 V = C2 = MIDI
  36 **[DEFAULT]**), hysteresis of ±¼ semitone on the quantizer boundary to
  prevent flutter.
- **Gate:** hysteresis on >2 V (8192 counts), off <1 V (4096) — same
  thresholds as `top/sid`. Rise → `note_on(ch0, note, 100)`; fall →
  `note_off(ch0, note)`. Pitch change while gated → `note_on(new)` then
  `note_off(old)` (legato-friendly in the engine's mono modes). Gate with no
  Pitch assigned plays fixed C-4 (MIDI 60); Pitch with no Gate assigned does
  nothing **[DEFAULT]**. Reassigning/clearing Gate while a CV note is held
  sends the matching `note_off` (no stuck notes).

Budget: 4 compares + at most a handful of engine calls per tick; engine-side
`knobSet`/`parSet` are the same code MIDI CC already runs at wire rate.
No new f32 math in the ISR: the volts→value maps are integer, and the pitch
quantizer is a 61-entry count-threshold table in flash **[DEFAULT]**.

### 6c. Main-loop deltas

- Menu edit handling extends the existing `on_turn`/`on_press` results: patch
  edits call `mbsid_sysex_param` under `critical_section`; CV target changes
  copy into the ISR-shared `App` and mark settings dirty.
- Save handling is unchanged (both Save rows funnel into the same
  `PressResult::Commit` arm).

### 6d. `fw/src/settings_store.rs` (new, host-testable)

One 16-byte record in the option-storage window:

```
magic  "MBS5" (4 B) | version u8 | midi_src u8 | cv_targets [u8;4] |
reserved [u8;5] | crc8 u8
```

- `load()` at boot: bad magic/version/crc → defaults (Off×4, TRS). Unknown
  `CvTarget` byte values decode to `Off` (forward compatibility).
- `save()` from the main loop, debounced: written ~2 s after the last change
  **[DEFAULT]**, skipped if identical to the last-written record (flash wear).
  Reuses the `SPIFlash0` driver; 4 KiB sector erase + program, exactly the
  `UserPatchStore` pattern. Never called from the ISR.
- **Behavior change, deliberate:** `midi_src` now persists across boots
  (previously reset to TRS; `CLAUDE.md` note updated with the implementation).

### 6e. Footprint

New state is bytes (targets, scroll, dirty flags); the parameter and pitch
tables are `const` in flash. No new 512-B buffers (`patch_buf` is reused).
Re-verify with `llvm-size -A` per the root `CLAUDE.md` caveats; the measured
M4 headroom (~21.8 KB stack) absorbs this comfortably.

---

## 7. Gateware

**None.** No new CSRs, no PAC regen, no timing exposure. `PMOD0_PERIPH`
(sample_i), I2C1, the option-storage window, and both SID peripherals are all
already in the M2/M4 SoC.

---

## 8. Validation

**Host `cargo test --lib` (extend the 41):**
- Card selector cycles all three cards; per-card focus clamps; card switch
  never returns a load/commit.
- CV row edit steps/clamps the target list; unknown persisted byte → `Off`.
- Mapping math: volts→knob8, volts→par16, gate hysteresis (rise/fall/deadband),
  pitch quantizer incl. boundary hysteresis and clamps.
- Param table static checks: every `addr`/`mirror`/`wide` pair < 512 and
  inside the documented Lead regions; nibble masks consistent.
- Edit-card state machine: scroll follows focus; nibble RMW composes
  correctly; `edited` flag set/clear lifecycle.
- `settings_store` record roundtrip + corrupt-record rejection.

**Oracle (`host_oracle/run_oracle.sh`):**
- Re-run the full 28/28 + differential + sweep (shim recompiles).
- Add `kn <knob> <val>` and `pr <par> <val16>` sequence commands to **both**
  drivers (`shim_driver` and the JUCE `oracle`); add one Lead sequence mixing
  notes with knob/par moves — register streams must stay byte-identical.
- Add a shim-only check: `mbsid_sysex_param` on a Lead patch, then
  `mbsid_current_patch_raw` — the edited bytes appear at the expected offsets
  (save-captures-edits invariant).

**Hardware checklist (pending, with M2–M4 bring-up):**
- [ ] CV1→Knob1 audibly modulates a patch with a K1 assignment; does nothing
      audible on a patch without one (correct — document in README).
- [ ] CV→Cutoff sweeps the filter on any Lead patch; releasing the CV leaves
      the patch's own cutoff (next patch load restores).
- [ ] Pitch+Gate from a sequencer tracks V/oct over ≥4 octaves, no stuck
      notes on cable pull.
- [ ] Edit Cutoff/ADSR on-device → audible immediately → Save → power cycle →
      load User slot → edit persisted.
- [ ] Settings persist: assign CVs, set MIDI Src=USB, power cycle, verify.
- [ ] `*` dirty star appears on first edit, clears on save and on load.
- [ ] Edit card on a non-Lead patch — confirm the encoder cannot corrupt or
      dirty the loaded patch (host-test-guarded per the M5 review fix; still
      worth a physical confirmation of encoder/display behavior), including
      when the non-Lead patch was selected via inbound MIDI Program Change,
      not just menu navigation.

---

## 9. Documentation follow-ups (with the implementation)

- `CLAUDE.md` (this dir): replace the "resets to TRS on every boot" MidiSrc
  note with the persisted-settings behavior; add the CV target list and the
  precedence rule (§3).
- `README.md`: CV input mapping (jacks 0–3), card navigation, edit-card
  volatility contract.
- `top.py` `bitstream_help.io_left[0..3]`: `'CV1'..'CV4'` (or the assigned
  default labels).

---

## 10. Reference pointers

- Engine entry points: `MbSid.cpp::sysexSetParameter`,
  `MbSidSe.h:130-137` (virtual knob/par API),
  `MbSidSeLead.cpp::parSet/knobSet` (common block 0x01–0x05, knob scaling).
- Patch layout: `MbSidStructs.h` `sid_patch_t` `.L` view (offsets in §2).
- CV read + calibration pattern: `top/sid/fw/src/main.rs` (`sample_i`,
  `CalibrationConstants::load_or_default`).
- Save path being reused: `M4_USER_PATCH_BANKS.md` §6c–6e.
