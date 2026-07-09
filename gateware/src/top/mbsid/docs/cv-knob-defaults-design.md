# Design: default CV-to-knob assignment + knob-map display

Status: approved, not yet implemented.

## Problem

The CV Mod card lets the user route each of the 4 CV inputs to a patch knob
(Knob1-5), a common param (Volume/Phase/Detune/Cutoff/Reso), or Pitch/Gate.
Today all 4 CVs default to `Off` and stay there until the user manually
assigns them. There is also no way to see, at a glance, what a loaded
patch's Knob1-5 actually *do* — a patch author (or the MIDIbox SID v2 editor)
assigns each knob to an engine parameter via the patch body; a knob with no
assignment is inert.

Two related asks:
1. Auto-populate CV1-4 with the patch's exposed knobs by default, so a
   freshly-booted (never-configured) unit is already useful.
2. Show a "knob map" line on the Main card (e.g. "K1:Detun K2:Cutof") so the
   user can see what each knob does without opening an editor.

## Background: the knob-assign mechanism

Every patch body (`sid_patch_t`, shared header layout across all 4 engines
per `MbSidStructs.h`) has an 8x5 knob table at byte offset **0x18**:
`knob[8][5]` = `{assign1, assign2, value, min, max}` per slot, slots
0-4 = Knob1-5 (5-7 = Velocity/Pitchbender/Aftertouch, not in scope here).

`assign1`/`assign2` are parameter indices consumed by the engine's
`parSet(par, ...)`. **`par == 0` is a no-op** (confirmed: `MbSidSeLead.cpp`'s
`parSet` switch has no `case 0`, same pattern in Bassline/Drum/Multi) — so
`assign1 == 0 && assign2 == 0` means "this knob does nothing in this
patch," i.e. **not exposed**.

Calibration against the real factory bank (`sid_bank_preset_a.inc`, 128
Lead patches, the actual `sid_bank_preset_0` compiled into the firmware):
the large majority of factory patches leave Knob1-5 fully unassigned
(0/0) — the few that do assign something use Portamento, Mod-matrix
depth/op, and LFO depth. So auto-defaulting will frequently produce "all 4
CVs Off" for factory patches; it earns its keep once patches deliberately
route knobs (hand-authored via the MIDIbox SID v2 editor, or future
in-house patches). This is expected, not a bug.

## Scope decisions (from brainstorming)

- CV mapping: first 4 *exposed* knobs (scanning Knob1..Knob5 in order) map
  to CV1..CV4 in order. Fewer than 4 exposed -> remaining CVs stay `Off`.
  Never falls back to common params (Volume/Cutoff/etc) or Pitch/Gate.
- Applies uniformly to all 4 engines (Lead/Bassline/Drum/Multi) — they
  share the same `knob[8][5]` layout and `assign1==0` convention.
- Auto-default is active only while no CV config has ever been saved to
  flash. It recomputes on *every* patch load (menu nav or inbound MIDI
  Program Change) during that window. The instant the user manually edits
  CV Mod or MIDI Src (the existing `TurnResult::SettingsChanged` signal),
  auto-default turns off for the rest of the session — even before the
  2s-debounced flash write actually lands. (Edge case, accepted: if power
  is cut before that debounced write completes, the next boot sees no
  saved record and auto-default resumes. Not fixed here.)
- Knob-map display: names are provided for **Lead only** (the reference
  engine); Bassline/Drum/Multi show a generic `p0xNN` fallback. One
  compact abbreviated line, e.g. `K1:Detun K2:Cutof`, omitting unassigned
  knobs.

## Components

### `fw/src/knob_map.rs` (new, host-testable, no FFI)

```
pub const KNOB_TABLE_OFFSET: u16 = 0x18;

/// Read (assign1, assign2) for knob slot `idx` (0..=4 = Knob1..Knob5).
pub fn knob_assign(patch_byte: impl Fn(u16) -> u8, idx: u8) -> (u8, u8);

/// assign1 == 0 && assign2 == 0 => not exposed.
pub fn is_exposed(assign1: u8, assign2: u8) -> bool;

/// Short (<=8 char) label for a Lead assign pair. Prefers assign1, falls
/// back to assign2 if assign1 is 0. Unrecognized/non-Lead => "p0xNN".
pub fn label(assign1: u8, assign2: u8) -> heapless::String<8>;

/// First <=4 exposed knobs (Knob1..5 order) -> CvTarget::Knob1..Knob5,
/// mapped to CV1..CV4 in order. Remaining CVs = CvTarget::Off.
pub fn default_targets(patch_byte: impl Fn(u16) -> u8) -> [crate::cv::CvTarget; 4];
```

`label`'s ranges are taken directly from `MbSidSeLead.cpp`'s `parSet`
switch: common block (0x01 Vol, 0x02 Phase, 0x03 Detun, 0x04 Cutof, 0x05
Reso, 0x06 Chan, 0x07 FMode), voice block (0x20 Wave, 0x24 Transp, 0x28
Fine, 0x2c Porta, 0x30 PW, 0x34 Dly, 0x38 Atk, 0x3c Dec, 0x40 Sus, 0x44
Rel, 0x48 ArpSpd, 0x4c ArpGat, 0x50 PBend), mod matrix (0x60-0x67 depth,
0x68-0x6f op, index = `par & 7`, e.g. "Mod2"), LFO (0x80/0x88/0x90/0x98/0xa0
x Wave/Depth/Rate/Dly/Phase, index = `par & 7`, e.g. "L2Dep"), WT
(0xe0-0xf3, Speed/Begin/End/Loop/Pos, index = `par & 3`, e.g. "W2Spd"),
Note (0xfc, "Note"). Anything else -> `p0xNN` (hex of assign1, or assign2
if assign1 was 0).

The ENV range (0xa8-0xdf, `MbSidSeLead.cpp:750-771`) is deliberately
**excluded** from the named table: its sub-param dispatch is
`switch(par & 0xf0) { case 0x0: ... case 0xf: ... }` — `par & 0xf0` only
ever produces 0 or a multiple of 16, which can equal a `case 0x0`..`0xf`
label solely when the result is 0 (i.e. `par < 0x10`), which is
unreachable given the block's own `par <= 0xdf` / preceding `par <= 0xa7`
guard. So this switch cannot select any envelope sub-param for the values
that actually route through it in practice — it looks like a pre-existing
upstream dead-code path (confirmed: none of the 128 factory patches assign
a value in 0xa8-0xdf to any knob). Not our vendored code to fix; simply
route this range to the generic `p0xNN` fallback like any other
unrecognized value.

Unit tests (host, `cargo test --lib`): `is_exposed` truth table, `label`
for each named range plus an unmapped value, `default_targets` for 0/1/3/5
exposed knobs in various slot positions (confirms "first N in order" and
"unfilled -> Off").

### `App.cv_auto_default: bool` (new field, `fw/src/main.rs`)

Boot: `settings_store::load_checked(...)` (new function, returns
`Option<Settings>`; existing `load()` becomes
`load_checked(...).unwrap_or_default()`) — `cv_auto_default = decoded.is_none()`.

Applied after every completed patch load while true:
- Menu-driven ROM bank load (`bank_load` call site) and user-bank load
  (`store.load` + `load_patch` call site), both already in the main loop:
  immediately after, if `cv_auto_default`, compute
  `knob_map::default_targets(|a| mbsid_sys::patch_byte(a))`, apply via
  `critical_section::with(|cs| { cv.set_targets(new, &mut EngineSink); })`,
  and update `state.cv_targets` (menu display copy). Do **not** touch
  `settings_dirty_at`.
- Inbound MIDI Program Change: handled inside `timer0_handler`'s existing
  `critical_section::with` block, which already has `app.cv` in scope.
  Right after `mbsid_sys::program_change(...)`, if `app.cv_auto_default`,
  do the same recompute + `cv.set_targets` call in place (no need to leave
  the ISR — `patch_byte` reads are safe here, same as elsewhere in this
  file).

Turned off: in the main loop, when `on_turn` returns
`TurnResult::SettingsChanged` (already the sole signal for a manual CV
Mod / MIDI Src edit), set `cv_auto_default = false` immediately, before
the existing debounce/save logic runs.

### Main card display (`fw/src/menu.rs`)

New row after the existing detail row (`Card::Main` only): built from
`mbsid_sys::patch_byte` reads (live patch, not bank metadata — same
staleness-tolerant idiom as `state.refresh_params`), one entry per exposed
Knob1-5 via `knob_map::label`, joined with spaces, e.g.
`"K1:Detun K2:Cutof"`. Omits unassigned knobs entirely; empty string (no
row content, row still present but blank) if none exposed. Status row
(when present) shifts down by one `ROW_DY`. `row_count()` for `Card::Main`
increases by 1 to account for the new row existing (even when blank) —
matches the existing pattern where the detail row is always present.

## Testing

- `fw/src/knob_map.rs`: new unit tests as described above (host-only,
  `cargo test --target x86_64-unknown-linux-gnu --lib`).
- `fw/src/settings_store.rs`: unit test for `load_checked` returning `None`
  on blank/corrupt flash and `Some` on a valid record (extends existing
  MockFlash tests).
- `fw/src/menu.rs`: extend `build_frame_main_card_rows_and_styles`-style
  tests to cover the new knob-map row's presence/position and content for
  a few `(assign1, assign2)` fixtures.
- No gateware/CSR changes, no oracle re-run needed (this is pure
  firmware/menu logic reading existing patch bytes via the existing
  `patch_byte` FFI).
