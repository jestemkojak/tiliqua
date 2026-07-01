//! Minimal patch-browser menu: host-pure state machine + cstr helper + draw.

use tiliqua_hal::embedded_graphics::{
    mono_font::{ascii::FONT_9X15, MonoTextStyle},
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::Text,
    prelude::*,
};
use tiliqua_lib::color::HI8;
use heapless::String;
use core::fmt::Write;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine { Lead, Bassline, Drum, Multi }

impl Engine {
    pub fn from_byte(b: u8) -> Self {
        match b { 1 => Self::Bassline, 2 => Self::Drum, 3 => Self::Multi, _ => Self::Lead }
    }
    pub fn label(self) -> &'static str {
        match self { Self::Lead => "Lead", Self::Bassline => "Bass",
                     Self::Drum => "Drum", Self::Multi    => "Multi" }
    }
    pub fn ch_map(self) -> &'static str {
        match self {
            Self::Lead     => "Ch 1",
            Self::Bassline => "Ch 1/<60  2/>=60",
            Self::Drum     => "Ch 1",
            Self::Multi    => "Ch 1-3:L  4-6:R",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoiceMode { Mono, Poly, Legato }

impl VoiceMode {
    pub fn from_vflags(b: u8) -> Self {
        if b & 0x08 != 0 { Self::Poly }
        else if b & 0x01 != 0 { Self::Legato }
        else { Self::Mono }
    }
    pub fn label(self) -> &'static str {
        match self { Self::Mono => "Mono", Self::Poly => "Poly", Self::Legato => "Leg" }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row { Bank, Program }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode { Nav, Edit }

pub struct MenuState {
    pub focus: Row,
    pub mode: Mode,
    pub bank: u8,
    pub program: u8,
    bank_count: u8,
}

#[inline]
fn clamp_i16(v: i16, lo: i16, hi: i16) -> i16 {
    if v < lo { lo } else if v > hi { hi } else { v }
}

impl MenuState {
    pub fn new(bank_count: u8, bank: u8, program: u8) -> Self {
        let bank_count = bank_count.max(1);
        Self {
            focus: Row::Bank,
            mode: Mode::Nav,
            bank: bank.min(bank_count - 1),
            program: program.min(127),
            bank_count,
        }
    }

    /// Handle an encoder rotation. Returns true iff a (bank, program) load is required.
    pub fn on_turn(&mut self, delta: i8) -> bool {
        match self.mode {
            Mode::Nav => {
                // Two rows: positive -> Program, negative -> Bank (clamped, no wrap).
                self.focus = if delta > 0 { Row::Program } else { Row::Bank };
                false
            }
            Mode::Edit => match self.focus {
                Row::Program => {
                    let next = clamp_i16(self.program as i16 + delta as i16, 0, 127) as u8;
                    let changed = next != self.program;
                    self.program = next;
                    changed
                }
                Row::Bank => {
                    let hi = (self.bank_count - 1) as i16;
                    let next = clamp_i16(self.bank as i16 + delta as i16, 0, hi) as u8;
                    let changed = next != self.bank;
                    self.bank = next;
                    // All banks hold 128 patches, so program needs no re-clamp;
                    // a load is required whenever the bank actually changed.
                    changed
                }
            },
        }
    }

    /// Handle a button press: toggle Nav<->Edit. Never triggers a load.
    pub fn on_press(&mut self) {
        self.mode = match self.mode { Mode::Nav => Mode::Edit, Mode::Edit => Mode::Nav };
    }
}

/// Interpret a 17-byte NUL-terminated engine name buffer as a &str (lossy-safe).
pub fn name_from_cstr(buf: &[u8; 17]) -> &str {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(16);
    core::str::from_utf8(&buf[..end]).unwrap_or("?")
}

/// Width/height of the menu's opaque background box, in pixels.
const MENU_W: u32 = 380;
const MENU_H: u32 = 120;
const ROW_DY: i32 = 24; // vertical spacing between rows

/// Build the detail-row text. `detail` is `Some((engine, voice_mode))` for a
/// successfully-loaded patch, or `None` when patch info is unavailable (e.g. a
/// failed `bankLoad`) — in which case we show "---" rather than a stale/default
/// engine label.
pub fn detail_line(detail: Option<(Engine, Option<VoiceMode>)>) -> String<48> {
    let mut line: String<48> = String::new();
    match detail {
        Some((engine, Some(vm))) => {
            let _ = write!(line, "  {} {}  {}", engine.label(), vm.label(), engine.ch_map());
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

/// Draw the menu into its own opaque box at (pos_x, pos_y). `name` is the
/// 16-char patch name for the current (bank, program). `detail` is `None` when
/// patch info is unavailable (failed load), which renders the row as "---".
pub fn draw<D>(d: &mut D, st: &MenuState, name: &str,
               detail: Option<(Engine, Option<VoiceMode>)>,
               pos_x: i32, pos_y: i32, hue: u8) -> Result<(), D::Error>
where
    D: DrawTarget<Color = HI8>,
{
    // Opaque background so old text never bleeds through under high persistence.
    let bg = PrimitiveStyleBuilder::new().fill_color(HI8::new(0, 0)).build();
    Rectangle::new(Point::new(pos_x - 10, pos_y - 18), Size::new(MENU_W, MENU_H))
        .into_styled(bg)
        .draw(d)?;

    let dim    = MonoTextStyle::new(&FONT_9X15, HI8::new(hue, 9));
    let bright = MonoTextStyle::new(&FONT_9X15, HI8::new(hue, 15));

    // Title.
    Text::new("MBSID", Point::new(pos_x, pos_y), bright).draw(d)?;

    // Row helper: marker depends on focus/mode; value is bright when focused.
    let mut line: String<48> = String::new();

    // Bank row.
    let bank_focused = st.focus == Row::Bank;
    let marker = row_marker(st, Row::Bank);
    line.clear();
    let _ = write!(line, "{} Bank     {}", marker, (b'A' + st.bank) as char);
    let style = if bank_focused { bright } else { dim };
    Text::new(&line, Point::new(pos_x, pos_y + ROW_DY), style).draw(d)?;

    // Program row (with the patch name).
    let prog_focused = st.focus == Row::Program;
    let marker = row_marker(st, Row::Program);
    line.clear();
    let _ = write!(line, "{} Program  {:03}  {}", marker, st.program, name);
    let style = if prog_focused { bright } else { dim };
    Text::new(&line, Point::new(pos_x, pos_y + 2 * ROW_DY), style).draw(d)?;

    // Detail row: engine label + voice mode (Lead only) + channel map, or "---".
    let detail_line = detail_line(detail);
    Text::new(&detail_line, Point::new(pos_x, pos_y + 3 * ROW_DY), dim).draw(d)?;

    Ok(())
}

#[inline]
fn row_marker(st: &MenuState, row: Row) -> char {
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
        let mut m = MenuState::new(1, 0, 10); // Nav, focus=Bank
        assert_eq!(m.focus, Row::Bank);
        assert_eq!(m.on_turn(1), false);      // -> Program, no load
        assert_eq!(m.focus, Row::Program);
        assert_eq!(m.on_turn(3), false);      // clamp: stays Program
        assert_eq!(m.focus, Row::Program);
        assert_eq!(m.on_turn(-1), false);     // -> Bank
        assert_eq!(m.focus, Row::Bank);
        assert_eq!(m.on_turn(-5), false);     // clamp: stays Bank
        assert_eq!(m.focus, Row::Bank);
    }

    #[test]
    fn press_toggles_mode_without_loading() {
        let mut m = MenuState::new(1, 0, 10);
        assert_eq!(m.mode, Mode::Nav);
        m.on_press();
        assert_eq!(m.mode, Mode::Edit);
        m.on_press();
        assert_eq!(m.mode, Mode::Nav);
    }

    #[test]
    fn edit_program_changes_value_clamps_and_loads_only_on_change() {
        let mut m = MenuState::new(1, 0, 0);
        m.focus = Row::Program;
        m.on_press();                         // -> Edit
        assert_eq!(m.on_turn(5), true);       // 0 -> 5, load
        assert_eq!(m.program, 5);
        assert_eq!(m.on_turn(-100), true);    // clamp to 0, value changed -> load
        assert_eq!(m.program, 0);
        assert_eq!(m.on_turn(-1), false);     // already 0, no change -> no load
        assert_eq!(m.program, 0);
        assert_eq!(m.on_turn(127), true);     // -> 127
        assert_eq!(m.program, 127);
        assert_eq!(m.on_turn(10), false);     // clamp at 127, no change
    }

    #[test]
    fn edit_bank_is_inert_with_one_bank_but_live_with_many() {
        let mut one = MenuState::new(1, 0, 0);
        one.focus = Row::Bank;
        one.on_press();
        assert_eq!(one.on_turn(1), false);    // clamp to [0,0]: inert
        assert_eq!(one.bank, 0);

        let mut many = MenuState::new(3, 0, 7);
        many.focus = Row::Bank;
        many.on_press();
        assert_eq!(many.on_turn(1), true);    // 0 -> 1, load at current program
        assert_eq!(many.bank, 1);
        assert_eq!(many.program, 7);          // program preserved
        assert_eq!(many.on_turn(5), true);    // clamp to 2 (bank_count-1)
        assert_eq!(many.bank, 2);
        assert_eq!(many.on_turn(1), false);   // already max, no change
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
        assert_eq!(VoiceMode::from_vflags(0x01), VoiceMode::Legato);  // bit 0
        assert_eq!(VoiceMode::from_vflags(0x08), VoiceMode::Poly);    // bit 3
        assert_eq!(VoiceMode::from_vflags(0x09), VoiceMode::Poly);    // POLY wins
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
}
