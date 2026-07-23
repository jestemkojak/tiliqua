//! Minimal patch-browser menu: host-pure state machine + cstr helper + build_frame/Painter.

use core::fmt::Write;
use heapless::String;
use tiliqua_hal::embedded_graphics::{
    mono_font::{ascii::FONT_9X15, MonoTextStyle},
    prelude::*,
    text::Text,
};
use tiliqua_lib::color::HI8;

use crate::cv::CvTarget;
use crate::frame::Frame;
use crate::params;

pub const N_PARAMS: usize = params::LEAD_PARAMS.len();

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    Lead,
    Bassline,
    Drum,
    Multi,
}

impl Engine {
    pub fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::Bassline,
            2 => Self::Drum,
            3 => Self::Multi,
            _ => Self::Lead,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Lead => "Lead",
            Self::Bassline => "Bass",
            Self::Drum => "Drum",
            Self::Multi => "Multi",
        }
    }
    pub fn ch_map(self) -> &'static str {
        match self {
            Self::Lead => "Ch 1",
            Self::Bassline => "Ch 1/<60  2/>=60",
            Self::Drum => "Ch 1",
            Self::Multi => "Ch 1-3:L  4-6:R",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoiceMode {
    Mono,
    Poly,
    Legato,
}

impl VoiceMode {
    pub fn from_vflags(b: u8) -> Self {
        if b & 0x08 != 0 {
            Self::Poly
        } else if b & 0x01 != 0 {
            Self::Legato
        } else {
            Self::Mono
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Mono => "Mono",
            Self::Poly => "Poly",
            Self::Legato => "Legato",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Card {
    Main,
    CvMod,
    PatchEdit,
    Usb,
}

impl Card {
    pub fn label(self) -> &'static str {
        match self {
            Self::Main => "Main",
            Self::CvMod => "CV Mod",
            Self::PatchEdit => "Edit",
            Self::Usb => "USB",
        }
    }
    /// `usb_enabled` gates whether the Card row can reach `Usb` at all — the
    /// Usb card only exists in the selector when `usb_storage` is on. When
    /// `usb_enabled` flips false while `self == Usb` (main.rs's future
    /// resync of `usb_storage` from live state, mirroring the `lead_loaded`
    /// resync lesson), the very next call clamps back to `PatchEdit` (hi=2)
    /// since `step` re-derives the clamp bound from `usb_enabled` on every
    /// call rather than caching it.
    fn step(self, delta: i8, usb_enabled: bool) -> Self {
        let ix = match self {
            Self::Main => 0i16,
            Self::CvMod => 1,
            Self::PatchEdit => 2,
            Self::Usb => 3,
        };
        let hi = if usb_enabled { 3 } else { 2 };
        match (ix + delta as i16).clamp(0, hi) {
            0 => Self::Main,
            1 => Self::CvMod,
            2 => Self::PatchEdit,
            _ => Self::Usb,
        }
    }
}

pub const ROW_CARD: u8 = 0;
pub const MAIN_ROW_BANK: u8 = 1;
pub const MAIN_ROW_PROGRAM: u8 = 2;
pub const MAIN_ROW_SAVE: u8 = 3;
pub const MAIN_ROW_MIDISRC: u8 = 4;
pub const MAIN_ROW_USBMODE: u8 = 5;
pub const USB_ROW_FILE: u8 = 1;
pub const USB_ROW_LOADSLOT: u8 = 2;
pub const USB_ROW_EXPORT: u8 = 3;
pub const USB_ROW_IMPORT: u8 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TurnResult {
    None,
    Load,                         // (bank, program) load required
    Param { ix: u8, value: u16 }, // patch-edit row changed -> sysex_param writes
    SettingsChanged,              // cv target or midi_src changed -> persist
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MidiSource {
    Trs,
    Usb,
}

impl MidiSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Trs => "TRS",
            Self::Usb => "USB",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Nav,
    Edit,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PressResult {
    Toggled,
    Commit(u8),
    Cancel,
    UsbLoad(u8), // audition file idx (M6a; Task 9 wires I/O)
    UsbLoadToSlot { file: u8, slot: u8 },
    UsbExport { source: ExportSource },
    UsbImportBank, // replace user bank from /MBSID/BANK.SYX
}

/// Source patch for a USB export (M6b): the live EDIT buffer (current engine
/// state) or a saved user-bank slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportSource {
    Edit,
    Slot(u8),
}

pub struct MenuState {
    pub card: Card,
    pub focus: u8,
    pub mode: Mode,
    pub bank: u8,
    pub program: u8,
    pub save_cursor: i16,
    pub midi_src: MidiSource,
    pub cv_targets: [CvTarget; 4],
    pub edit_values: [u16; N_PARAMS],
    pub edit_scroll: u8,
    pub edited: bool,
    /// Whether the currently-loaded patch is a Lead-engine patch. Gates the
    /// PatchEdit card's param rows (Lead-layout byte offsets don't apply to
    /// Bassline/Drum/Multi patches) — see `row_count()`. Defaults to `true`
    /// to match `main.rs`'s initial assumption before the first load.
    pub lead_loaded: bool,
    /// USB Mode toggle (Main row 5). Gates whether `Card::Usb` is reachable
    /// via the Card row selector — see `Card::step`. Task 9 must resync this
    /// from live state every main-loop iteration (not just after a menu
    /// edit), mirroring the `lead_loaded` resync lesson (see this module's
    /// module doc / CLAUDE.md).
    pub usb_storage: bool,
    /// Selected file index on the Usb card's File row; -1 = none selected.
    pub usb_file: i16,
    /// File count on the current USB drive listing; set by main.rs after
    /// each directory refresh, not by the menu state machine itself.
    pub usb_file_count: u8,
    /// Load>Slot destination cursor; -1 = Cancel (Cancel-first, like Save).
    pub usb_slot: i16,
    /// Export source cursor: -1 = Cancel, 0 = EDIT buffer, 1..=128 = slot n-1
    /// (Cancel-first, like Save/Load>Slot).
    pub usb_export: i16,
    /// Import-bank confirm cursor: -1 = Cancel, 0 = REPLACE (Cancel-first,
    /// like Save/Load>Slot/Export — the arm-then-confirm guard of spec §4).
    pub usb_import: i16,
    bank_count: u8,
}

#[inline]
fn clamp_i16(v: i16, lo: i16, hi: i16) -> i16 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

impl MenuState {
    pub fn new(bank_count: u8, bank: u8, program: u8) -> Self {
        let bank_count = bank_count.max(1);
        Self {
            card: Card::Main,
            focus: ROW_CARD,
            mode: Mode::Nav,
            bank: bank.min(bank_count - 1),
            program: program.min(127),
            save_cursor: -1,
            midi_src: MidiSource::Trs,
            cv_targets: [CvTarget::Off; 4],
            edit_values: [0u16; N_PARAMS],
            edit_scroll: 0,
            edited: false,
            lead_loaded: true,
            usb_storage: false,
            usb_file: -1,
            usb_file_count: 0,
            usb_slot: -1,
            usb_export: -1,
            usb_import: -1,
            bank_count,
        }
    }

    pub fn row_count(&self) -> u8 {
        match self.card {
            Card::Main => 6,
            Card::CvMod => 5, // Card + CV1..CV4
            // Card + params + Save, but params are Lead-layout only: when the
            // loaded patch isn't Lead, collapse to Card + Save (matches what
            // `draw()` renders for `!lead_loaded`, and bounds `focus` so the
            // param branch in `on_turn`'s Edit-mode PatchEdit arm becomes
            // unreachable by construction).
            Card::PatchEdit => {
                if self.lead_loaded {
                    2 + N_PARAMS as u8
                } else {
                    2
                }
            }
            Card::Usb => 5, // Card + File + Load>Slot + Export + Import
        }
    }

    fn is_save_row(&self) -> bool {
        match self.card {
            Card::Main => self.focus == MAIN_ROW_SAVE,
            Card::PatchEdit => self.focus == self.row_count() - 1,
            Card::CvMod => false,
            Card::Usb => false,
        }
    }

    /// Handle an encoder rotation.
    pub fn on_turn(&mut self, delta: i8) -> TurnResult {
        match self.mode {
            Mode::Nav => {
                let hi = (self.row_count() - 1) as i16;
                self.focus = clamp_i16(self.focus as i16 + delta as i16, 0, hi) as u8;
                if self.card == Card::PatchEdit && self.focus >= 1 {
                    let ix = self.focus - 1; // 0-based row within the scrolling list
                    if ix < self.edit_scroll {
                        self.edit_scroll = ix;
                    }
                    if ix >= self.edit_scroll + PATCH_EDIT_WINDOW {
                        self.edit_scroll = ix - PATCH_EDIT_WINDOW + 1;
                    }
                }
                TurnResult::None
            }
            Mode::Edit => {
                if self.focus == ROW_CARD {
                    self.card = self.card.step(delta, self.usb_storage);
                    self.focus = ROW_CARD;
                    return TurnResult::None;
                }
                match self.card {
                    Card::Main => match self.focus {
                        MAIN_ROW_PROGRAM => {
                            let next = clamp_i16(self.program as i16 + delta as i16, 0, 127) as u8;
                            let changed = next != self.program;
                            self.program = next;
                            if changed {
                                TurnResult::Load
                            } else {
                                TurnResult::None
                            }
                        }
                        MAIN_ROW_BANK => {
                            let hi = (self.bank_count - 1) as i16;
                            let next = clamp_i16(self.bank as i16 + delta as i16, 0, hi) as u8;
                            let changed = next != self.bank;
                            self.bank = next;
                            // All banks hold 128 patches, so program needs no re-clamp;
                            // a load is required whenever the bank actually changed.
                            if changed {
                                TurnResult::Load
                            } else {
                                TurnResult::None
                            }
                        }
                        MAIN_ROW_SAVE => {
                            self.save_cursor = clamp_i16(self.save_cursor + delta as i16, -1, 127);
                            TurnResult::None // preview only; never a load, never a write
                        }
                        MAIN_ROW_MIDISRC => {
                            if delta != 0 {
                                self.midi_src = match self.midi_src {
                                    MidiSource::Trs => MidiSource::Usb,
                                    MidiSource::Usb => MidiSource::Trs,
                                };
                                TurnResult::SettingsChanged
                            } else {
                                TurnResult::None
                            }
                        }
                        MAIN_ROW_USBMODE => {
                            if delta != 0 {
                                self.usb_storage = !self.usb_storage;
                                TurnResult::SettingsChanged
                            } else {
                                TurnResult::None
                            }
                        }
                        _ => TurnResult::None,
                    },
                    Card::CvMod => {
                        let i = (self.focus - 1) as usize;
                        if i < 4 {
                            let next = self.cv_targets[i].step(delta);
                            let changed = next != self.cv_targets[i];
                            self.cv_targets[i] = next;
                            if changed {
                                TurnResult::SettingsChanged
                            } else {
                                TurnResult::None
                            }
                        } else {
                            TurnResult::None
                        }
                    }
                    Card::PatchEdit => {
                        if self.is_save_row() {
                            self.save_cursor = clamp_i16(self.save_cursor + delta as i16, -1, 127);
                            TurnResult::None
                        } else {
                            let ix = (self.focus - 1) as usize;
                            if ix < N_PARAMS {
                                let d = &params::LEAD_PARAMS[ix];
                                let cur = self.edit_values[ix] as i32;
                                let step = d.step as i32;
                                let next =
                                    (cur + delta as i32 * step).clamp(0, d.max as i32) as u16;
                                let changed = next != self.edit_values[ix];
                                self.edit_values[ix] = next;
                                if changed {
                                    self.edited = true;
                                    TurnResult::Param {
                                        ix: ix as u8,
                                        value: next,
                                    }
                                } else {
                                    TurnResult::None
                                }
                            } else {
                                TurnResult::None
                            }
                        }
                    }
                    Card::Usb => match self.focus {
                        USB_ROW_FILE => {
                            let hi = self.usb_file_count as i16 - 1;
                            self.usb_file = clamp_i16(self.usb_file + delta as i16, -1, hi.max(-1));
                            TurnResult::None
                        }
                        USB_ROW_LOADSLOT => {
                            self.usb_slot = clamp_i16(self.usb_slot + delta as i16, -1, 127);
                            TurnResult::None
                        }
                        USB_ROW_EXPORT => {
                            self.usb_export = clamp_i16(self.usb_export + delta as i16, -1, 128);
                            TurnResult::None
                        }
                        USB_ROW_IMPORT => {
                            self.usb_import = clamp_i16(self.usb_import + delta as i16, -1, 0);
                            TurnResult::None
                        }
                        _ => TurnResult::None,
                    },
                }
            }
        }
    }

    /// Button press. Commit/Cancel are decided BEFORE toggling the mode so a
    /// write can only ever happen on the deliberate Edit->Nav confirmation on
    /// the Save row (M4 spec §6d).
    pub fn on_press(&mut self) -> PressResult {
        let result = if self.card == Card::Usb && self.mode == Mode::Edit {
            match self.focus {
                USB_ROW_FILE => {
                    if self.usb_file >= 0 {
                        PressResult::UsbLoad(self.usb_file as u8)
                    } else {
                        PressResult::Cancel
                    }
                }
                USB_ROW_LOADSLOT => {
                    if self.usb_file >= 0 && self.usb_slot >= 0 {
                        PressResult::UsbLoadToSlot {
                            file: self.usb_file as u8,
                            slot: self.usb_slot as u8,
                        }
                    } else {
                        PressResult::Cancel
                    }
                }
                USB_ROW_EXPORT => match self.usb_export {
                    -1 => PressResult::Cancel,
                    0 => PressResult::UsbExport {
                        source: ExportSource::Edit,
                    },
                    n => PressResult::UsbExport {
                        source: ExportSource::Slot((n - 1) as u8),
                    },
                },
                USB_ROW_IMPORT => {
                    if self.usb_import == 0 {
                        PressResult::UsbImportBank
                    } else {
                        PressResult::Cancel
                    }
                }
                _ => PressResult::Toggled,
            }
        } else if self.is_save_row() && self.mode == Mode::Edit {
            if self.save_cursor < 0 {
                PressResult::Cancel
            } else {
                PressResult::Commit(self.save_cursor as u8)
            }
        } else {
            PressResult::Toggled
        };
        self.mode = match self.mode {
            Mode::Nav => {
                if self.is_save_row() {
                    self.save_cursor = -1; // always enter Edit at Cancel
                }
                if self.card == Card::Usb && self.focus == USB_ROW_LOADSLOT {
                    self.usb_slot = -1; // Cancel-first, same as Save
                }
                if self.card == Card::Usb && self.focus == USB_ROW_EXPORT {
                    self.usb_export = -1; // Cancel-first, same as Save
                }
                if self.card == Card::Usb && self.focus == USB_ROW_IMPORT {
                    self.usb_import = -1; // Cancel-first, same as Save
                }
                Mode::Edit
            }
            Mode::Edit => Mode::Nav,
        };
        result
    }

    pub fn is_user_bank(&self) -> bool {
        self.bank == self.bank_count - 1
    }

    pub fn refresh_params(&mut self, body: impl Fn(u16) -> u8) {
        for (i, d) in params::LEAD_PARAMS.iter().enumerate() {
            self.edit_values[i] = params::read_value(d, &body);
        }
    }
}

/// Interpret a 17-byte NUL-terminated engine name buffer as a &str (lossy-safe).
pub fn name_from_cstr(buf: &[u8; 17]) -> &str {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(16);
    core::str::from_utf8(&buf[..end]).unwrap_or("?")
}

/// USB drive status shown on the Usb card's Drive row. `NoDrive`/`Busy` are
/// UI-only in this task — Task 9 wires actual FAT/USB state into these.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriveState {
    NoDrive,
    Ready,
    Busy,
}

/// Everything `build_frame` needs to render the Usb card, supplied by
/// main.rs. `None` (any non-Usb card) renders as `DriveState::NoDrive` with
/// no file/slot names — but `build_frame` only reads `usb` when
/// `st.card == Card::Usb`, so a non-Usb caller passing `None` is the normal
/// case, not a degraded one.
pub struct UsbInfo<'a> {
    pub drive: DriveState,
    pub file_name: Option<&'a str>, // name of usb_file, if any
    pub file_count: u8,
    pub slot_name: Option<&'a str>, // name of the Load>Slot target slot
}

const ROW_DY: i32 = 24; // vertical spacing between rows

/// Visible window height (in rows) for the PatchEdit param list.
const PATCH_EDIT_WINDOW: u8 = 6;

/// Build the detail-row text. `detail` is `Some((engine, voice_mode))` for a
/// successfully-loaded patch, or `None` when patch info is unavailable (e.g. a
/// failed `bankLoad`) — in which case we show "---" rather than a stale/default
/// engine label.
pub fn detail_line(detail: Option<(Engine, Option<VoiceMode>)>) -> String<48> {
    let mut line: String<48> = String::new();
    match detail {
        Some((engine, Some(vm))) => {
            let _ = write!(
                line,
                "  {} {}  {}",
                engine.label(),
                vm.label(),
                engine.ch_map()
            );
        }
        Some((engine, None)) => {
            let _ = write!(line, "  {}  {}", engine.label(), engine.ch_map());
        }
        None => {
            let _ = write!(line, "  ---");
        }
    }
    line
}

/// Build the menu's frame description at (pos_x, pos_y). Pure: same inputs as
/// the old draw() (minus target/hue), returns the Item list that Painter
/// diffs+blits. `bright` on each item maps to intensity 15 vs 9 at paint time.
// Deferred: collapsed into a `FrameCtx<'_>` parameter struct by the
// menu-API refactor (review step 5). Not a new problem — the arg list
// has been 9-wide since the Usb card landed.
#[allow(clippy::too_many_arguments)]
pub fn build_frame(
    st: &MenuState,
    name: &str,
    detail: Option<(Engine, Option<VoiceMode>)>,
    save_name: Option<&str>,
    status: Option<&str>,
    lead_loaded: bool,
    usb: Option<&UsbInfo>,
    pos_x: i32,
    pos_y: i32,
) -> Frame {
    let mut f = Frame::default();

    // Title, with an edited-patch marker (" *") to the right of "MBSID".
    let mut title: String<32> = String::new();
    if st.edited {
        let _ = write!(title, "MBSID  {} *", name);
    } else {
        let _ = write!(title, "MBSID  {}", name);
    }
    f.push(pos_x, pos_y, true, &title);

    let mut line: String<48> = String::new();

    // Row 0 on every card: Card selector.
    let marker = row_marker(st, ROW_CARD);
    line.clear();
    let _ = write!(line, "{} Menu     {}", marker, st.card.label());
    f.push(pos_x, pos_y + ROW_DY, st.focus == ROW_CARD, &line);

    match st.card {
        Card::Main => {
            // Bank row.
            let marker = row_marker(st, MAIN_ROW_BANK);
            line.clear();
            let bank_char = if st.is_user_bank() {
                'U'
            } else {
                (b'A' + st.bank) as char
            };
            let _ = write!(line, "{} Bank     {}", marker, bank_char);
            f.push(pos_x, pos_y + 2 * ROW_DY, st.focus == MAIN_ROW_BANK, &line);

            // Program row (with the patch name).
            let marker = row_marker(st, MAIN_ROW_PROGRAM);
            line.clear();
            let _ = write!(line, "{} Program  {:03}  {}", marker, st.program, name);
            f.push(
                pos_x,
                pos_y + 3 * ROW_DY,
                st.focus == MAIN_ROW_PROGRAM,
                &line,
            );

            // Save row: destination cursor. Cancel-first (spec §6d [DEFAULT]).
            let marker = row_marker(st, MAIN_ROW_SAVE);
            line.clear();
            if st.save_cursor < 0 {
                let _ = write!(line, "{} Save     Cancel", marker);
            } else {
                let _ = write!(
                    line,
                    "{} Save     U{:03}  {}",
                    marker,
                    st.save_cursor,
                    save_name.unwrap_or("Empty")
                );
            }
            f.push(pos_x, pos_y + 4 * ROW_DY, st.focus == MAIN_ROW_SAVE, &line);

            // MIDI source row: which physical input feeds the engine.
            let marker = row_marker(st, MAIN_ROW_MIDISRC);
            line.clear();
            let _ = write!(line, "{} MIDI Src {}", marker, st.midi_src.label());
            f.push(
                pos_x,
                pos_y + 5 * ROW_DY,
                st.focus == MAIN_ROW_MIDISRC,
                &line,
            );

            // USB Mode row: MIDI (default) vs Storage; gates Card::Usb reachability.
            let marker = row_marker(st, MAIN_ROW_USBMODE);
            line.clear();
            let _ = write!(
                line,
                "{} USB Mode {}",
                marker,
                if st.usb_storage { "Storage" } else { "MIDI" }
            );
            f.push(
                pos_x,
                pos_y + 6 * ROW_DY,
                st.focus == MAIN_ROW_USBMODE,
                &line,
            );

            // Detail row: engine label + voice mode (Lead only) + channel map, or "---".
            f.push(pos_x, pos_y + 7 * ROW_DY, false, &detail_line(detail));

            if let Some(s) = status {
                f.push(pos_x, pos_y + 8 * ROW_DY, true, s);
            }
        }
        Card::CvMod => {
            for i in 0..4u8 {
                let row = i + 1;
                let marker = row_marker(st, row);
                line.clear();
                let _ = write!(
                    line,
                    "{} CV{}      {}",
                    marker,
                    i + 1,
                    st.cv_targets[i as usize].label()
                );
                f.push(
                    pos_x,
                    pos_y + (2 + i as i32) * ROW_DY,
                    st.focus == row,
                    &line,
                );
            }
            // Dim footer line in place of Main's detail line.
            f.push(pos_x, pos_y + 6 * ROW_DY, false, "mods engine knobs/params");

            if let Some(s) = status {
                f.push(pos_x, pos_y + 7 * ROW_DY, true, s);
            }
        }
        Card::PatchEdit => {
            if !lead_loaded {
                f.push(pos_x, pos_y + 2 * ROW_DY, false, "Lead patches only");

                // Save row (visual gate only — see draw()'s original NOTE:
                // row_count()/is_save_row() don't know about lead_loaded).
                let save_row = st.row_count() - 1;
                let marker = row_marker(st, save_row);
                line.clear();
                if st.save_cursor < 0 {
                    let _ = write!(line, "{} Save     Cancel", marker);
                } else {
                    let _ = write!(
                        line,
                        "{} Save     U{:03}  {}",
                        marker,
                        st.save_cursor,
                        save_name.unwrap_or("Empty")
                    );
                }
                f.push(pos_x, pos_y + 3 * ROW_DY, st.focus == save_row, &line);
            } else {
                let scroll = st.edit_scroll as usize;
                let end = (scroll + PATCH_EDIT_WINDOW as usize).min(N_PARAMS);

                for (slot, ix) in (scroll..end).enumerate() {
                    let row = (ix + 1) as u8;
                    let d_param = &params::LEAD_PARAMS[ix];
                    let marker = row_marker(st, row);
                    line.clear();
                    let _ = write!(
                        line,
                        "{} {:<8} {}",
                        marker, d_param.label, st.edit_values[ix]
                    );
                    f.push(
                        pos_x,
                        pos_y + (2 + slot as i32) * ROW_DY,
                        st.focus == row,
                        &line,
                    );
                }

                // Scroll indicators at the window edges.
                if scroll > 0 {
                    f.push(pos_x - 10, pos_y + 2 * ROW_DY, false, "^");
                }
                if end < N_PARAMS {
                    let last_slot = (end - scroll).max(1) as i32 - 1;
                    f.push(pos_x - 10, pos_y + (2 + last_slot) * ROW_DY, false, "v");
                }

                // Save row, immediately after the visible param window.
                let save_row = st.row_count() - 1;
                let marker = row_marker(st, save_row);
                line.clear();
                if st.save_cursor < 0 {
                    let _ = write!(line, "{} Save     Cancel", marker);
                } else {
                    let _ = write!(
                        line,
                        "{} Save     U{:03}  {}",
                        marker,
                        st.save_cursor,
                        save_name.unwrap_or("Empty")
                    );
                }
                let visible_rows = (end - scroll) as i32;
                f.push(
                    pos_x,
                    pos_y + (2 + visible_rows) * ROW_DY,
                    st.focus == save_row,
                    &line,
                );
            }
        }
        Card::Usb => {
            let (drive, fname, count, sname) = match usb {
                Some(u) => (u.drive, u.file_name, u.file_count, u.slot_name),
                None => (DriveState::NoDrive, None, 0, None),
            };
            line.clear();
            let _ = match drive {
                DriveState::NoDrive => write!(line, "  Drive    No drive"),
                DriveState::Busy => write!(line, "  Drive    BUSY"),
                DriveState::Ready => write!(line, "  Drive    Ready ({} files)", count),
            };
            f.push(pos_x, pos_y + 2 * ROW_DY, false, &line);

            let marker = row_marker(st, USB_ROW_FILE);
            line.clear();
            if st.usb_file < 0 {
                let _ = write!(line, "{} File     -", marker);
            } else {
                let _ = write!(
                    line,
                    "{} File     {:03}  {}",
                    marker,
                    st.usb_file,
                    fname.unwrap_or("?")
                );
            }
            f.push(pos_x, pos_y + 3 * ROW_DY, st.focus == USB_ROW_FILE, &line);

            let marker = row_marker(st, USB_ROW_LOADSLOT);
            line.clear();
            if st.usb_slot < 0 {
                let _ = write!(line, "{} Load>Slot Cancel", marker);
            } else {
                let _ = write!(
                    line,
                    "{} Load>Slot U{:03}  {}",
                    marker,
                    st.usb_slot,
                    sname.unwrap_or("Empty")
                );
            }
            f.push(
                pos_x,
                pos_y + 4 * ROW_DY,
                st.focus == USB_ROW_LOADSLOT,
                &line,
            );

            let marker = row_marker(st, USB_ROW_EXPORT);
            line.clear();
            match st.usb_export {
                -1 => {
                    let _ = write!(line, "{} Export   Cancel", marker);
                }
                0 => {
                    let _ = write!(line, "{} Export   EDIT -> USB", marker);
                }
                n => {
                    let _ = write!(line, "{} Export   U{:03} -> USB", marker, n - 1);
                }
            }
            f.push(pos_x, pos_y + 5 * ROW_DY, st.focus == USB_ROW_EXPORT, &line);

            let marker = row_marker(st, USB_ROW_IMPORT);
            line.clear();
            let _ = match st.usb_import {
                -1 => write!(line, "{} Import   Cancel", marker),
                _ => write!(line, "{} Import   REPLACE bank!", marker),
            };
            f.push(pos_x, pos_y + 6 * ROW_DY, st.focus == USB_ROW_IMPORT, &line);

            if let Some(s) = status {
                f.push(pos_x, pos_y + 7 * ROW_DY, true, s);
            }
        }
    }

    f
}

/// Diff-painting menu renderer. Owns the last-painted frame; each paint()
/// erases stale items by re-blitting their OLD text at intensity 0 (the
/// blitter's REPLACE mode writes zero-color pixels — a glyph-exact eraser)
/// and blits new/changed items. No rectangle fills: an encoder detent costs
/// ~2 rows of glyph blits instead of a ~93k-pixel clear that scanout films
/// as a top-to-bottom wipe. Erases run before draws (frame::diff guarantee)
/// and share the blitter FIFO with the draws, so ordering is exact.
pub struct Painter {
    prev: Frame,
}

impl Painter {
    pub fn new() -> Self {
        Self {
            prev: Frame::default(),
        }
    }

    pub fn paint<D>(&mut self, d: &mut D, frame: Frame, hue: u8) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = HI8>,
    {
        let erase = MonoTextStyle::new(&FONT_9X15, HI8::new(0, 0));
        let dim = MonoTextStyle::new(&FONT_9X15, HI8::new(hue, 9));
        let bright = MonoTextStyle::new(&FONT_9X15, HI8::new(hue, 15));
        for op in crate::frame::diff(&self.prev, &frame).iter() {
            match *op {
                crate::frame::PaintOp::Erase(i) => {
                    let it = &self.prev.items[i as usize];
                    Text::new(&it.text, Point::new(it.x, it.y), erase).draw(d)?;
                }
                crate::frame::PaintOp::Draw(i) => {
                    let it = &frame.items[i as usize];
                    let style = if it.bright { bright } else { dim };
                    Text::new(&it.text, Point::new(it.x, it.y), style).draw(d)?;
                }
            }
        }
        self.prev = frame;
        Ok(())
    }
}

impl Default for Painter {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn row_marker(st: &MenuState, row: u8) -> char {
    if st.focus != row {
        ' '
    } else if st.mode == Mode::Edit {
        '#' // editing this row
    } else {
        '>' // navigation cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_turn_moves_cursor_and_clamps_no_load() {
        let mut m = MenuState::new(1, 0, 10); // Nav, focus=Card
        assert_eq!(m.focus, ROW_CARD);
        assert!(matches!(m.on_turn(1), TurnResult::None)); // -> Bank, no load
        assert_eq!(m.focus, MAIN_ROW_BANK);
        assert!(matches!(m.on_turn(1), TurnResult::None)); // -> Program, no load
        assert_eq!(m.focus, MAIN_ROW_PROGRAM);
        assert!(matches!(m.on_turn(1), TurnResult::None)); // -> Save, no load
        assert_eq!(m.focus, MAIN_ROW_SAVE);
        assert!(matches!(m.on_turn(1), TurnResult::None)); // -> MidiSrc, no load
        assert_eq!(m.focus, MAIN_ROW_MIDISRC);
        assert!(matches!(m.on_turn(1), TurnResult::None)); // -> UsbMode, no load
        assert_eq!(m.focus, MAIN_ROW_USBMODE);
        assert!(matches!(m.on_turn(3), TurnResult::None)); // clamp: stays UsbMode
        assert_eq!(m.focus, MAIN_ROW_USBMODE);
        assert!(matches!(m.on_turn(-1), TurnResult::None)); // -> MidiSrc
        assert_eq!(m.focus, MAIN_ROW_MIDISRC);
        assert!(matches!(m.on_turn(-1), TurnResult::None)); // -> Save
        assert_eq!(m.focus, MAIN_ROW_SAVE);
        assert!(matches!(m.on_turn(-1), TurnResult::None)); // -> Program
        assert_eq!(m.focus, MAIN_ROW_PROGRAM);
        assert!(matches!(m.on_turn(-1), TurnResult::None)); // -> Bank
        assert_eq!(m.focus, MAIN_ROW_BANK);
        assert!(matches!(m.on_turn(-1), TurnResult::None)); // -> Card
        assert_eq!(m.focus, ROW_CARD);
        assert!(matches!(m.on_turn(-5), TurnResult::None)); // clamp: stays Card
        assert_eq!(m.focus, ROW_CARD);
    }

    #[test]
    fn press_toggles_mode_without_loading() {
        let mut m = MenuState::new(1, 0, 10);
        assert_eq!(m.mode, Mode::Nav);
        let _ = m.on_press();
        assert_eq!(m.mode, Mode::Edit);
        let _ = m.on_press();
        assert_eq!(m.mode, Mode::Nav);
    }

    #[test]
    fn edit_program_changes_value_clamps_and_loads_only_on_change() {
        let mut m = MenuState::new(1, 0, 0);
        m.focus = MAIN_ROW_PROGRAM;
        let _ = m.on_press(); // -> Edit
        assert!(matches!(m.on_turn(5), TurnResult::Load)); // 0 -> 5, load
        assert_eq!(m.program, 5);
        assert!(matches!(m.on_turn(-100), TurnResult::Load)); // clamp to 0, value changed -> load
        assert_eq!(m.program, 0);
        assert!(!matches!(m.on_turn(-1), TurnResult::Load)); // already 0, no change -> no load
        assert_eq!(m.program, 0);
        assert!(matches!(m.on_turn(127), TurnResult::Load)); // -> 127
        assert_eq!(m.program, 127);
        assert!(!matches!(m.on_turn(10), TurnResult::Load)); // clamp at 127, no change
    }

    #[test]
    fn edit_bank_is_inert_with_one_bank_but_live_with_many() {
        let mut one = MenuState::new(1, 0, 0);
        one.focus = MAIN_ROW_BANK;
        let _ = one.on_press();
        assert!(!matches!(one.on_turn(1), TurnResult::Load)); // clamp to [0,0]: inert
        assert_eq!(one.bank, 0);

        let mut many = MenuState::new(3, 0, 7);
        many.focus = MAIN_ROW_BANK;
        let _ = many.on_press();
        assert!(matches!(many.on_turn(1), TurnResult::Load)); // 0 -> 1, load at current program
        assert_eq!(many.bank, 1);
        assert_eq!(many.program, 7); // program preserved
        assert!(matches!(many.on_turn(5), TurnResult::Load)); // clamp to 2 (bank_count-1)
        assert_eq!(many.bank, 2);
        assert!(!matches!(many.on_turn(1), TurnResult::Load)); // already max, no change
    }

    #[test]
    fn name_from_cstr_trims_at_nul() {
        let mut buf = [b' '; 17];
        buf[..4].copy_from_slice(b"Lead");
        buf[4] = 0;
        assert_eq!(name_from_cstr(&buf), "Lead");
    }

    #[test]
    fn name_from_cstr_handles_full_16_chars() {
        let mut buf = [b'X'; 17];
        buf[16] = 0;
        assert_eq!(name_from_cstr(&buf).len(), 16);
    }

    #[test]
    fn engine_from_byte_coverage() {
        assert_eq!(Engine::from_byte(0), Engine::Lead);
        assert_eq!(Engine::from_byte(1), Engine::Bassline);
        assert_eq!(Engine::from_byte(2), Engine::Drum);
        assert_eq!(Engine::from_byte(3), Engine::Multi);
        assert_eq!(Engine::from_byte(99), Engine::Lead); // unknown → Lead
    }

    #[test]
    fn voice_mode_from_vflags_all_cases() {
        assert_eq!(VoiceMode::from_vflags(0x00), VoiceMode::Mono);
        assert_eq!(VoiceMode::from_vflags(0x01), VoiceMode::Legato); // bit 0
        assert_eq!(VoiceMode::from_vflags(0x08), VoiceMode::Poly); // bit 3
        assert_eq!(VoiceMode::from_vflags(0x09), VoiceMode::Poly); // POLY wins
    }

    #[test]
    fn detail_line_none_shows_error_indicator_not_lead() {
        // Failed bankLoad => None: must not silently show a Lead/Mono label.
        let l = detail_line(None);
        assert_eq!(l.as_str().trim(), "---");
        assert!(!l.contains("Lead"));
    }

    #[test]
    fn detail_line_lead_includes_voice_mode() {
        let l = detail_line(Some((Engine::Lead, Some(VoiceMode::Poly))));
        assert!(l.contains("Lead"));
        assert!(l.contains("Poly"));
        assert!(l.contains(Engine::Lead.ch_map()));
    }

    #[test]
    fn detail_line_non_lead_omits_voice_mode() {
        let l = detail_line(Some((Engine::Multi, None)));
        assert!(l.contains("Multi"));
        assert!(l.contains(Engine::Multi.ch_map()));
        assert!(!l.contains("Mono") && !l.contains("Poly") && !l.contains("Leg"));
    }

    #[test]
    fn engine_ch_map_and_label_non_empty() {
        for b in [0u8, 1, 2, 3, 99] {
            let e = Engine::from_byte(b);
            assert!(!e.label().is_empty());
            assert!(!e.ch_map().is_empty());
        }
    }

    #[test]
    fn nav_cycles_four_rows() {
        let mut m = MenuState::new(2, 0, 10);
        assert_eq!(m.focus, ROW_CARD);
        m.on_turn(1);
        assert_eq!(m.focus, MAIN_ROW_BANK);
        m.on_turn(1);
        assert_eq!(m.focus, MAIN_ROW_PROGRAM);
        m.on_turn(1);
        assert_eq!(m.focus, MAIN_ROW_SAVE);
        m.on_turn(1);
        assert_eq!(m.focus, MAIN_ROW_MIDISRC);
        m.on_turn(1);
        assert_eq!(m.focus, MAIN_ROW_USBMODE);
        m.on_turn(1); // clamp
        assert_eq!(m.focus, MAIN_ROW_USBMODE);
        m.on_turn(-1);
        assert_eq!(m.focus, MAIN_ROW_MIDISRC);
        m.on_turn(-1);
        assert_eq!(m.focus, MAIN_ROW_SAVE);
        m.on_turn(-1);
        assert_eq!(m.focus, MAIN_ROW_PROGRAM);
        m.on_turn(-1);
        assert_eq!(m.focus, MAIN_ROW_BANK);
        m.on_turn(-1);
        assert_eq!(m.focus, ROW_CARD);
        m.on_turn(-1); // clamp
        assert_eq!(m.focus, ROW_CARD);
    }

    #[test]
    fn user_bank_is_last() {
        let mut m = MenuState::new(2, 0, 0);
        assert!(!m.is_user_bank());
        m.focus = MAIN_ROW_BANK;
        let _ = m.on_press(); // -> Edit
        assert!(matches!(m.on_turn(1), TurnResult::Load)); // bank 0 -> 1 (User), load required
        assert!(m.is_user_bank());
    }

    #[test]
    fn save_row_edit_enters_at_cancel_and_scrolls() {
        let mut m = MenuState::new(2, 0, 0);
        m.focus = MAIN_ROW_SAVE;
        m.save_cursor = 42; // stale from a previous visit
        let _ = m.on_press(); // Nav -> Edit: cursor must reset to Cancel
        assert_eq!(m.mode, Mode::Edit);
        assert_eq!(m.save_cursor, -1);
        assert!(!matches!(m.on_turn(1), TurnResult::Load)); // Cancel -> slot 0; never a load
        assert_eq!(m.save_cursor, 0);
        assert!(!matches!(m.on_turn(-5), TurnResult::Load)); // clamp at Cancel
        assert_eq!(m.save_cursor, -1);
        assert!(!matches!(m.on_turn(127), TurnResult::Load)); // -1 + 127 = 126
        assert_eq!(m.save_cursor, 126);
        assert!(!matches!(m.on_turn(127), TurnResult::Load)); // clamp at 127
        assert_eq!(m.save_cursor, 127);
    }

    #[test]
    fn save_press_commits_at_slot_cancels_at_cancel() {
        let mut m = MenuState::new(2, 0, 0);
        m.focus = MAIN_ROW_SAVE;
        assert_eq!(m.on_press(), PressResult::Toggled); // Nav -> Edit
        m.on_turn(10);
        assert_eq!(m.on_press(), PressResult::Commit(9)); // Edit -> Nav (cursor -1+10=9)
        assert_eq!(m.mode, Mode::Nav);

        assert_eq!(m.on_press(), PressResult::Toggled); // back to Edit, cursor reset
        assert_eq!(m.save_cursor, -1);
        assert_eq!(m.on_press(), PressResult::Cancel); // press at Cancel
        assert_eq!(m.mode, Mode::Nav);
    }

    #[test]
    fn non_save_rows_never_commit() {
        let mut m = MenuState::new(2, 0, 0);
        m.focus = MAIN_ROW_PROGRAM;
        assert_eq!(m.on_press(), PressResult::Toggled);
        assert_eq!(m.on_press(), PressResult::Toggled);
    }

    #[test]
    fn midi_src_defaults_trs_and_toggles_on_edit_turn() {
        let mut m = MenuState::new(1, 0, 0);
        assert_eq!(m.midi_src, MidiSource::Trs);
        m.focus = MAIN_ROW_MIDISRC;
        let _ = m.on_press(); // Nav -> Edit
        assert!(matches!(m.on_turn(1), TurnResult::SettingsChanged)); // never a load
        assert_eq!(m.midi_src, MidiSource::Usb);
        assert!(matches!(m.on_turn(-1), TurnResult::SettingsChanged));
        assert_eq!(m.midi_src, MidiSource::Trs);
        assert!(matches!(m.on_turn(0), TurnResult::None)); // zero delta: no change
        assert_eq!(m.midi_src, MidiSource::Trs);
    }

    #[test]
    fn midi_src_press_never_commits_or_cancels() {
        let mut m = MenuState::new(1, 0, 0);
        m.focus = MAIN_ROW_MIDISRC;
        assert_eq!(m.on_press(), PressResult::Toggled); // Nav -> Edit
        assert_eq!(m.on_press(), PressResult::Toggled); // Edit -> Nav
    }

    #[test]
    fn card_row_cycles_cards_without_side_effects() {
        let mut m = MenuState::new(2, 0, 0);
        assert_eq!(m.card, Card::Main);
        assert_eq!(m.focus, ROW_CARD);
        let _ = m.on_press(); // Edit on Card row
        assert!(matches!(m.on_turn(1), TurnResult::None));
        assert_eq!(m.card, Card::CvMod);
        assert!(matches!(m.on_turn(1), TurnResult::None));
        assert_eq!(m.card, Card::PatchEdit);
        assert!(matches!(m.on_turn(1), TurnResult::None)); // clamp
        assert_eq!(m.card, Card::PatchEdit);
        assert!(matches!(m.on_turn(-2), TurnResult::None));
        assert_eq!(m.card, Card::Main);
        let _ = m.on_press(); // back to Nav
        assert_eq!(m.mode, Mode::Nav);
    }

    #[test]
    fn switching_cards_keeps_per_card_focus_in_range() {
        let mut m = MenuState::new(2, 0, 0);
        m.focus = MAIN_ROW_MIDISRC; // row 4 on Main
        m.card = Card::CvMod; // CvMod also has 5 rows: still valid
        assert!(m.focus < m.row_count());
        m.focus = 0;
        m.card = Card::PatchEdit;
        assert_eq!(m.row_count() as usize, 2 + N_PARAMS); // Card + params + Save
    }

    #[test]
    fn main_save_commit_still_works_at_new_index() {
        let mut m = MenuState::new(2, 0, 0);
        m.focus = MAIN_ROW_SAVE;
        assert_eq!(m.on_press(), PressResult::Toggled);
        m.on_turn(10);
        assert_eq!(m.on_press(), PressResult::Commit(9));
    }

    #[test]
    fn cvmod_row_edit_steps_targets_and_reports_settings_change() {
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::CvMod;
        m.focus = 1; // CV1
        let _ = m.on_press();
        assert_eq!(m.on_turn(1), TurnResult::SettingsChanged);
        assert_eq!(m.cv_targets[0], CvTarget::Knob1);
        assert_eq!(m.on_turn(-1), TurnResult::SettingsChanged);
        assert_eq!(m.cv_targets[0], CvTarget::Off);
        assert_eq!(m.on_turn(-1), TurnResult::None); // clamped at Off: no change
    }

    #[test]
    fn patch_edit_row_steps_value_and_emits_param() {
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::PatchEdit;
        m.edit_values[0] = 5; // Volume row (max 15, step 1)
        m.focus = 1;
        let _ = m.on_press();
        assert_eq!(m.on_turn(2), TurnResult::Param { ix: 0, value: 7 });
        assert_eq!(m.on_turn(100), TurnResult::Param { ix: 0, value: 15 }); // clamp
        assert_eq!(m.on_turn(1), TurnResult::None); // at max: no change
    }

    #[test]
    fn patch_edit_wide_row_uses_coarse_step() {
        let cutoff_ix = params::LEAD_PARAMS
            .iter()
            .position(|d| d.label == "Cutoff")
            .unwrap();
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::PatchEdit;
        m.focus = cutoff_ix as u8 + 1;
        let _ = m.on_press();
        assert_eq!(
            m.on_turn(1),
            TurnResult::Param {
                ix: cutoff_ix as u8,
                value: 16
            }
        );
    }

    #[test]
    fn patch_edit_scroll_follows_focus() {
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::PatchEdit;
        for _ in 0..(N_PARAMS + 1) {
            m.on_turn(1);
        } // Nav to the Save row
        assert_eq!(m.focus, m.row_count() - 1);
        let ix = m.focus - 1;
        assert!(
            ix >= m.edit_scroll && ix < m.edit_scroll + 6,
            "focus visible"
        );
        for _ in 0..(N_PARAMS + 1) {
            m.on_turn(-1);
        } // back to Card row
        assert_eq!(m.edit_scroll, 0);
    }

    #[test]
    fn patch_edit_save_row_commits_like_main() {
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::PatchEdit;
        m.focus = m.row_count() - 1;
        assert_eq!(m.on_press(), PressResult::Toggled);
        assert_eq!(m.save_cursor, -1); // Cancel-first
        m.on_turn(5);
        assert_eq!(m.on_press(), PressResult::Commit(4));
    }

    /// Finding 1: when the loaded patch isn't Lead, PatchEdit must collapse
    /// to Card + Save only (no param rows reachable), and `on_turn` must
    /// never emit `TurnResult::Param` — those are Lead-layout byte offsets
    /// and would corrupt a non-Lead patch body if applied live.
    #[test]
    fn patch_edit_gated_off_when_not_lead_loaded() {
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::PatchEdit;
        m.lead_loaded = false;
        assert_eq!(m.row_count(), 2); // Card + Save only, no params

        // Nav mode: turning through (and past) the full old param range must
        // clamp focus to the Card/Save bound (0..=1) — the param rows must
        // be unreachable, not just unrendered.
        for _ in 0..(N_PARAMS + 5) {
            m.on_turn(1);
            assert!(
                m.focus <= 1,
                "focus escaped the Card/Save bound: {}",
                m.focus
            );
        }
        assert_eq!(m.focus, m.row_count() - 1); // landed on Save, the only other row

        // Enter Edit mode on the Save row and confirm turning it through the
        // full old param range never emits Param (it must behave as the Save
        // row, adjusting save_cursor, since the param branch is unreachable
        // by construction once row_count() == 2).
        let _ = m.on_press();
        for _ in 0..(N_PARAMS + 5) {
            let r = m.on_turn(1);
            assert!(
                !matches!(r, TurnResult::Param { .. }),
                "on_turn emitted Param while lead_loaded == false"
            );
        }
        assert!(m.is_save_row());
    }

    /// Sanity check that `lead_loaded == true` (the `MenuState::new` default)
    /// keeps the Task 7 param-editing behavior fully intact — i.e. this fix
    /// doesn't change anything when a Lead patch actually is loaded.
    #[test]
    fn patch_edit_param_rows_unchanged_when_lead_loaded() {
        let m = MenuState::new(2, 0, 0);
        assert!(m.lead_loaded); // default
        let mut m = m;
        m.card = Card::PatchEdit;
        assert_eq!(m.row_count() as usize, 2 + N_PARAMS);
    }

    #[test]
    fn refresh_params_reads_lead_layout() {
        let mut m = MenuState::new(2, 0, 0);
        // Body: volume=0xC (0x52), OSC1 ad=0x84 (0x62: A=8, D=4).
        m.refresh_params(|a| match a {
            0x52 => 0x0C,
            0x62 => 0x84,
            _ => 0,
        });
        assert_eq!(m.edit_values[0], 12);
        let atk_ix = params::LEAD_PARAMS
            .iter()
            .position(|d| d.label == "O1 Atk")
            .unwrap();
        let dec_ix = params::LEAD_PARAMS
            .iter()
            .position(|d| d.label == "O1 Dec")
            .unwrap();
        assert_eq!(m.edit_values[atk_ix], 8);
        assert_eq!(m.edit_values[dec_ix], 4);
    }

    // --- build_frame / diff renderer tests (flicker fix) ---

    #[test]
    fn build_frame_main_card_rows_and_styles() {
        let m = MenuState::new(2, 0, 5);
        let fr = build_frame(
            &m,
            "TestPatch",
            Some((Engine::Lead, Some(VoiceMode::Mono))),
            None,
            None,
            true,
            None,
            60,
            80,
        );
        // title + Card + Bank + Program + Save + MidiSrc + UsbMode + detail (no status)
        assert_eq!(fr.items.len(), 8);
        assert!(fr.items[0].text.starts_with("MBSID"));
        assert!(fr.items[0].bright); // title always bright
        assert!(fr.items[1].bright); // Card row focused (default)
        assert!(fr.items[1].text.contains("> Menu")); // nav marker on focus
        assert!(!fr.items[2].bright); // Bank unfocused -> dim
        assert!(fr.items[3].text.contains("Program  005  TestPatch"));
        assert!(!fr.items[7].bright); // detail row always dim
                                      // Rows stack at ROW_DY spacing from pos_y.
        assert_eq!(fr.items[0].y, 80);
        assert_eq!(fr.items[1].y, 80 + 24);
        assert_eq!(fr.items[7].y, 80 + 7 * 24);
    }

    #[test]
    fn focus_move_changes_exactly_two_rows() {
        let mut m = MenuState::new(2, 0, 0);
        let f0 = build_frame(&m, "P", None, None, None, true, None, 60, 80);
        m.on_turn(1); // Card -> Bank
        let f1 = build_frame(&m, "P", None, None, None, true, None, 60, 80);
        let ops = crate::frame::diff(&f0, &f1);
        // Marker moved: both rows' text changed -> 2 erases + 2 draws, and
        // nothing else (title/program/save/midisrc/detail untouched). This is
        // the flicker-fix regression test: an encoder detent must NOT repaint
        // the whole menu.
        assert_eq!(ops.len(), 4);
        let card_y = 80 + 24;
        let bank_y = 80 + 2 * 24;
        for op in ops.iter() {
            let y = match *op {
                crate::frame::PaintOp::Erase(i) => f0.items[i as usize].y,
                crate::frame::PaintOp::Draw(i) => f1.items[i as usize].y,
            };
            assert!(
                y == card_y || y == bank_y,
                "op outside the two focus rows: y={}",
                y
            );
        }
    }

    #[test]
    fn status_disappearance_is_single_erase() {
        let m = MenuState::new(2, 0, 0);
        let f0 = build_frame(&m, "P", None, None, Some("Saved U000"), true, None, 60, 80);
        let f1 = build_frame(&m, "P", None, None, None, true, None, 60, 80);
        let ops = crate::frame::diff(&f0, &f1);
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], crate::frame::PaintOp::Erase(_)));
    }

    #[test]
    fn card_switch_replaces_body_rows() {
        let mut m = MenuState::new(2, 0, 0);
        let f0 = build_frame(&m, "P", None, None, None, true, None, 60, 80);
        let _ = m.on_press(); // Edit on Card row
        let _ = m.on_turn(1); // Main -> CvMod
        let f1 = build_frame(&m, "P", None, None, None, true, None, 60, 80);
        let ops = crate::frame::diff(&f0, &f1);
        // Title is unchanged; everything under it differs. No panic on
        // capacity, ops bounded by MAX_OPS.
        assert!(!ops.is_empty() && ops.len() <= crate::frame::MAX_OPS);
        assert!(ops.iter().all(|op| {
            let (fr, i) = match *op {
                crate::frame::PaintOp::Erase(i) => (&f0, i),
                crate::frame::PaintOp::Draw(i) => (&f1, i),
            };
            fr.items[i as usize].y > 80 // never touches the title line
        }));
    }

    #[test]
    fn patch_edit_frame_has_indicators_and_save() {
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::PatchEdit;
        m.edit_scroll = 1; // both indicators visible (1 above, more below)
        let fr = build_frame(&m, "P", None, Some("SlotName"), None, true, None, 60, 80);
        // title + Card + 6 params + "^" + "v" + Save = 11
        assert_eq!(fr.items.len(), 11);
        assert!(fr
            .items
            .iter()
            .any(|it| it.text.as_str() == "^" && it.x == 50));
        assert!(fr
            .items
            .iter()
            .any(|it| it.text.as_str() == "v" && it.x == 50));
        assert!(fr.items.last().unwrap().text.contains("Save"));
    }

    #[test]
    fn non_lead_patch_edit_frame_is_gated() {
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::PatchEdit;
        let fr = build_frame(&m, "P", None, None, None, false, None, 60, 80);
        // title + Card + "Lead patches only" + Save = 4
        assert_eq!(fr.items.len(), 4);
        assert!(fr.items[2].text.contains("Lead patches only"));
    }

    /// Minimal mock `DrawTarget`: records every pixel it's asked to draw.
    /// No hardware, no acceleration -- just enough for `Text::draw` to run
    /// its normal (non-blitter) glyph rendering against `draw_iter`.
    struct RecordingTarget {
        pixels: heapless::Vec<(Point, HI8), 16384>,
    }

    impl RecordingTarget {
        fn new() -> Self {
            Self {
                pixels: heapless::Vec::new(),
            }
        }
    }

    impl OriginDimensions for RecordingTarget {
        fn size(&self) -> Size {
            Size::new(400, 300)
        }
    }

    impl DrawTarget for RecordingTarget {
        type Color = HI8;
        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(point, color) in pixels.into_iter() {
                let _ = self.pixels.push((point, color));
            }
            Ok(())
        }
    }

    #[test]
    fn painter_erases_old_frame_and_draws_new_frame_not_transposed() {
        // Mirrors focus_move_changes_exactly_two_rows: Card -> Bank focus
        // move touches exactly the Card and Bank rows, each losing/gaining
        // the '>' marker glyph. If Painter::paint transposed which frame it
        // indexes for Erase vs Draw, the Card row's erase would blit f1's
        // markerless text (nothing to erase) and the Bank row's draw would
        // blit f0's markerless text (nothing new drawn) -- the opposite of
        // what's asserted below.
        let mut m = MenuState::new(2, 0, 0);
        let f0 = build_frame(&m, "P", None, None, None, true, None, 60, 80);
        m.on_turn(1); // Card -> Bank
        let f1 = build_frame(&m, "P", None, None, None, true, None, 60, 80);

        let card_x = 60;
        let card_y = 80 + ROW_DY;
        let bank_y = 80 + 2 * ROW_DY;

        let mut painter = Painter::new();
        let mut d = RecordingTarget::new();

        // First paint: prev is empty (Frame::default), so every op is a Draw
        // and none is an Erase -- no intensity-0 pixels should appear at all.
        painter.paint(&mut d, f0.clone(), 0).unwrap();
        assert!(!d.pixels.is_empty());
        assert!(
            d.pixels.iter().all(|&(_, c)| c.intensity() != 0),
            "first paint (empty prev) must contain no erase pixels"
        );
        assert!(
            d.pixels
                .iter()
                .any(|&(p, _)| p.x >= card_x && p.x < card_x + 9 && (p.y - card_y).abs() < ROW_DY),
            "expected the focused Card row's '>' marker glyph in the first paint"
        );

        // Second paint: focus moves off Card onto Bank.
        d.pixels.clear();
        painter.paint(&mut d, f1.clone(), 0).unwrap();

        // Erase output (intensity 0) must draw the OLD (f0) Card row, which
        // had the marker -- proving Erase indexes `self.prev`, not `frame`.
        assert!(
            d.pixels.iter().any(|&(p, c)| c.intensity() == 0
                && p.x >= card_x
                && p.x < card_x + 9
                && (p.y - card_y).abs() < ROW_DY),
            "erase must re-blit the OLD frame's Card marker glyph"
        );

        // Draw output (nonzero intensity) must draw the NEW (f1) Bank row,
        // which now has the marker -- proving Draw indexes the passed-in
        // `frame`, not `self.prev`.
        assert!(
            d.pixels.iter().any(|&(p, c)| c.intensity() != 0
                && p.x >= card_x
                && p.x < card_x + 9
                && (p.y - bank_y).abs() < ROW_DY),
            "draw must blit the NEW frame's Bank marker glyph"
        );

        // After painting, `prev` must become the just-painted frame so the
        // next diff() call compares against what's actually on screen.
        assert_eq!(painter.prev, f1);
    }

    // --- USB Mode row + Card::Usb (M6a) ---

    #[test]
    fn usb_mode_row_toggles_and_reports_settings_change() {
        let mut m = MenuState::new(1, 0, 0);
        assert!(!m.usb_storage);
        m.focus = MAIN_ROW_USBMODE;
        let _ = m.on_press();
        assert_eq!(m.on_turn(1), TurnResult::SettingsChanged);
        assert!(m.usb_storage);
        assert_eq!(m.on_turn(1), TurnResult::SettingsChanged); // toggles back
        assert!(!m.usb_storage);
    }

    #[test]
    fn usb_card_unreachable_unless_storage_mode() {
        let mut m = MenuState::new(1, 0, 0);
        let _ = m.on_press(); // Edit on Card row
        for _ in 0..5 {
            let _ = m.on_turn(1);
        }
        assert_eq!(m.card, Card::PatchEdit); // clamped, no Usb
        m.usb_storage = true;
        let _ = m.on_turn(1);
        assert_eq!(m.card, Card::Usb);
    }

    #[test]
    fn usb_file_row_scrolls_and_loads_on_press() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.usb_file_count = 3;
        m.focus = USB_ROW_FILE;
        assert_eq!(m.on_press(), PressResult::Toggled); // Nav -> Edit
        let _ = m.on_turn(2); // -1 -> 1
        assert_eq!(m.usb_file, 1);
        let _ = m.on_turn(10); // clamp at 2
        assert_eq!(m.usb_file, 2);
        assert_eq!(m.on_press(), PressResult::UsbLoad(2)); // Edit -> Nav commits
        assert_eq!(m.mode, Mode::Nav);
    }

    #[test]
    fn usb_file_row_with_no_files_cancels() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.usb_file_count = 0;
        m.focus = USB_ROW_FILE;
        let _ = m.on_press();
        let _ = m.on_turn(5); // nothing to select
        assert_eq!(m.usb_file, -1);
        assert_eq!(m.on_press(), PressResult::Cancel);
    }

    #[test]
    fn usb_loadslot_row_commits_file_and_slot() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.usb_file_count = 2;
        m.usb_file = 1;
        m.focus = USB_ROW_LOADSLOT;
        assert_eq!(m.on_press(), PressResult::Toggled);
        assert_eq!(m.usb_slot, -1); // Cancel-first
        let _ = m.on_turn(4);
        assert_eq!(
            m.on_press(),
            PressResult::UsbLoadToSlot { file: 1, slot: 3 }
        );
    }

    #[test]
    fn usb_loadslot_without_file_cancels() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.usb_file = -1;
        m.focus = USB_ROW_LOADSLOT;
        let _ = m.on_press();
        let _ = m.on_turn(4);
        assert_eq!(m.on_press(), PressResult::Cancel);
    }

    #[test]
    fn build_frame_usb_card_rows() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.usb_file_count = 2;
        m.usb_file = 0;
        let usb = UsbInfo {
            drive: DriveState::Ready,
            file_name: Some("LEAD1.SYX"),
            file_count: 2,
            slot_name: None,
        };
        let fr = build_frame(&m, "P", None, None, None, true, Some(&usb), 60, 80);
        // title + Card + Drive + File + Load>Slot + Export + Import = 7 (no status)
        assert_eq!(fr.items.len(), 7);
        assert!(fr.items[2].text.contains("Ready"));
        assert!(fr.items[3].text.contains("LEAD1.SYX"));
        assert!(fr.items[4].text.contains("Load>Slot"));
        assert!(fr.items[5].text.contains("Export"));
        assert!(fr.items[6].text.contains("Import"));
    }

    #[test]
    fn usb_export_row_selects_edit_or_slot() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.focus = USB_ROW_EXPORT;
        assert_eq!(m.on_press(), PressResult::Toggled);
        assert_eq!(m.usb_export, -1); // Cancel-first
        let _ = m.on_turn(1); // -> EDIT
        assert_eq!(
            m.on_press(),
            PressResult::UsbExport {
                source: ExportSource::Edit
            }
        );
        let _ = m.on_press(); // re-enter Edit
        let _ = m.on_turn(3); // -> slot 1 (0-based: 1+? )
        assert_eq!(
            m.on_press(),
            PressResult::UsbExport {
                source: ExportSource::Slot(1)
            }
        );
        let _ = m.on_press();
        assert_eq!(m.on_press(), PressResult::Cancel); // press at Cancel
    }

    #[test]
    fn usb_import_row_cancel_first_confirm() {
        let mut m = MenuState::new(2, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        assert_eq!(m.row_count(), 5); // Card + File + Load>Slot + Export + Import
        m.focus = USB_ROW_IMPORT;
        assert_eq!(m.on_press(), PressResult::Toggled); // Nav -> Edit
        assert_eq!(m.usb_import, -1, "must enter Edit at Cancel");
        assert_eq!(m.on_press(), PressResult::Cancel); // press at Cancel backs out
        assert_eq!(m.mode, Mode::Nav);

        assert_eq!(m.on_press(), PressResult::Toggled); // re-enter Edit
        assert!(matches!(m.on_turn(1), TurnResult::None));
        assert_eq!(m.usb_import, 0); // armed: REPLACE
        assert!(matches!(m.on_turn(5), TurnResult::None));
        assert_eq!(m.usb_import, 0, "cursor clamps at REPLACE");
        assert!(matches!(m.on_turn(-1), TurnResult::None));
        assert_eq!(m.usb_import, -1, "turn back disarms to Cancel");
        assert!(matches!(m.on_turn(1), TurnResult::None));
        assert_eq!(m.on_press(), PressResult::UsbImportBank); // armed press fires
        assert_eq!(m.mode, Mode::Nav);
    }

    #[test]
    fn usb_import_row_renders_cancel_and_replace() {
        let mut m = MenuState::new(2, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.focus = USB_ROW_IMPORT;
        let usb = UsbInfo {
            drive: DriveState::Ready,
            file_name: None,
            file_count: 0,
            slot_name: None,
        };
        let f = build_frame(&m, "P", None, None, None, true, Some(&usb), 60, 80);
        assert!(f.items.iter().any(|it| it.text.contains("Import   Cancel")));
        m.on_press(); // Edit at Cancel
        m.on_turn(1); // arm
        let f = build_frame(&m, "P", None, None, None, true, Some(&usb), 60, 80);
        assert!(f
            .items
            .iter()
            .any(|it| it.text.contains("Import   REPLACE bank!")));
    }

    #[test]
    fn build_frame_main_shows_usb_mode_row() {
        let m = MenuState::new(1, 0, 0);
        let fr = build_frame(&m, "P", None, None, None, true, None, 60, 80);
        // title + Card + Bank + Program + Save + MidiSrc + UsbMode + detail = 8
        assert_eq!(fr.items.len(), 8);
        assert!(fr.items[6].text.contains("USB Mode MIDI"));
    }
}
