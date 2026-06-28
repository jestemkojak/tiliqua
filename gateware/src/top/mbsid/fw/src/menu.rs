//! Minimal patch-browser menu: host-pure state machine + cstr helper + draw.

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
}
