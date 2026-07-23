//! CV-input modulation routing (M5_MENU_CARDS_CV_MOD.md §5b, §6b).
//!
//! Host-pure: all engine access goes through `CvSink`, so the mapping math,
//! hysteresis and note machine are fully unit-tested off-target. Integer only
//! (ISR path). Calibrated CV = 4096 counts/volt; usable span 0..5 V.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CvTarget {
    Off,
    Knob1,
    Knob2,
    Knob3,
    Knob4,
    Knob5, // engine knob matrix (patch-defined routing)
    Volume,
    Phase,
    Detune,
    Cutoff,
    Reso, // parSet common block 0x01..0x05
    Pitch,
    Gate, // CV note machine, MIDI ch 1
}

const TARGET_ORDER: [CvTarget; 13] = [
    CvTarget::Off,
    CvTarget::Knob1,
    CvTarget::Knob2,
    CvTarget::Knob3,
    CvTarget::Knob4,
    CvTarget::Knob5,
    CvTarget::Volume,
    CvTarget::Phase,
    CvTarget::Detune,
    CvTarget::Cutoff,
    CvTarget::Reso,
    CvTarget::Pitch,
    CvTarget::Gate,
];

impl CvTarget {
    pub fn to_u8(self) -> u8 {
        TARGET_ORDER.iter().position(|&t| t == self).unwrap_or(0) as u8
    }
    pub fn from_u8(b: u8) -> Self {
        // Unknown persisted bytes decode to Off (forward compatibility).
        TARGET_ORDER
            .get(b as usize)
            .copied()
            .unwrap_or(CvTarget::Off)
    }
    pub fn step(self, delta: i8) -> Self {
        let ix = self.to_u8() as i16 + delta as i16;
        let ix = ix.clamp(0, TARGET_ORDER.len() as i16 - 1);
        TARGET_ORDER[ix as usize]
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Knob1 => "Knob1",
            Self::Knob2 => "Knob2",
            Self::Knob3 => "Knob3",
            Self::Knob4 => "Knob4",
            Self::Knob5 => "Knob5",
            Self::Volume => "Volume",
            Self::Phase => "Phase",
            Self::Detune => "Detune",
            Self::Cutoff => "Cutoff",
            Self::Reso => "Reso",
            Self::Pitch => "Pitch",
            Self::Gate => "Gate",
        }
    }
    fn par_number(self) -> Option<u8> {
        match self {
            Self::Volume => Some(0x01),
            Self::Phase => Some(0x02),
            Self::Detune => Some(0x03),
            Self::Cutoff => Some(0x04),
            Self::Reso => Some(0x05),
            _ => None,
        }
    }
    fn knob_number(self) -> Option<u8> {
        match self {
            Self::Knob1 => Some(0),
            Self::Knob2 => Some(1),
            Self::Knob3 => Some(2),
            Self::Knob4 => Some(3),
            Self::Knob5 => Some(4),
            _ => None,
        }
    }
}

pub trait CvSink {
    fn knob(&mut self, knob: u8, value: u8);
    fn par(&mut self, par: u8, value16: u16);
    fn note_on(&mut self, note: u8); // MIDI ch 1, velocity 100 (implementer)
    fn note_off(&mut self, note: u8);
}

const COUNTS_PER_VOLT: i32 = 4096;
const FULL_SCALE: i32 = 5 * COUNTS_PER_VOLT; // 0..5 V unipolar
const GATE_ON: i32 = 2 * COUNTS_PER_VOLT; // > 2 V
const GATE_OFF: i32 = COUNTS_PER_VOLT; // < 1 V
const PITCH_BASE_NOTE: i32 = 36; // 0 V = C2
const PITCH_SPAN: i32 = 60; // semitone indices 0..=60
const PITCH_HYST: i32 = COUNTS_PER_VOLT / 12 / 4; // ±¼ semitone ≈ 85 counts
const FIXED_GATE_NOTE: u8 = 60; // Gate with no Pitch: C-4

fn to_u8_scale(x: i32) -> u8 {
    (x.clamp(0, FULL_SCALE) * 255 / FULL_SCALE) as u8
}

/// Semitone index with boundary hysteresis: leave `current` only when the CV
/// is more than PITCH_HYST past the boundary to the neighbouring semitone.
fn quantize_semitone(x: i32, current: u8) -> u8 {
    let x = x.clamp(0, FULL_SCALE);
    let cur = current as i32;
    let upper = (2 * cur + 1) * COUNTS_PER_VOLT / 24 + PITCH_HYST;
    let lower = (2 * cur - 1) * COUNTS_PER_VOLT / 24 - PITCH_HYST;
    if x > upper || x < lower {
        ((x * 12 + COUNTS_PER_VOLT / 2) / COUNTS_PER_VOLT).clamp(0, PITCH_SPAN) as u8
    } else {
        current
    }
}

pub struct CvState {
    targets: [CvTarget; 4],
    last8: [Option<u8>; 4], // last emitted 8-bit value per input (knob/par deadband)
    gate: bool,
    held_note: u8,
    semitone: u8,
}

impl CvState {
    pub fn new() -> Self {
        Self {
            targets: [CvTarget::Off; 4],
            last8: [None; 4],
            gate: false,
            held_note: 0,
            semitone: 0,
        }
    }

    pub fn targets(&self) -> [CvTarget; 4] {
        self.targets
    }

    /// Apply a new assignment set; releases a held CV note if Gate went away.
    pub fn set_targets(&mut self, t: [CvTarget; 4], sink: &mut impl CvSink) {
        if self.gate && !t.contains(&CvTarget::Gate) {
            sink.note_off(self.held_note);
            self.gate = false;
        }
        self.last8 = [None; 4]; // re-emit values for retargeted inputs
        self.targets = t;
    }

    pub fn tick(&mut self, x: [i32; 4], sink: &mut impl CvSink) {
        // Continuous targets (knob/par), deadbanded on the 8-bit value.
        for ((&t, last), &xi) in self.targets.iter().zip(self.last8.iter_mut()).zip(x.iter()) {
            if t.knob_number().or(t.par_number()).is_none() {
                continue;
            }
            let v8 = to_u8_scale(xi);
            if *last == Some(v8) {
                continue;
            }
            *last = Some(v8);
            if let Some(k) = t.knob_number() {
                sink.knob(k, v8);
            } else if let Some(p) = t.par_number() {
                // 8-bit precision spread over the full 16-bit range.
                sink.par(p, ((v8 as u16) << 8) | v8 as u16);
            }
        }

        // Note machine: first Pitch input and first Gate input, if assigned.
        let pitch_in = self.targets.iter().position(|&t| t == CvTarget::Pitch);
        let gate_in = self.targets.iter().position(|&t| t == CvTarget::Gate);
        let Some(g) = gate_in else { return }; // Pitch without Gate: no effect
        let level = x[g];

        if !self.gate && level > GATE_ON {
            self.gate = true;
            self.held_note = match pitch_in {
                Some(p) => {
                    self.semitone = quantize_semitone(x[p], self.semitone);
                    (PITCH_BASE_NOTE + self.semitone as i32) as u8
                }
                None => FIXED_GATE_NOTE,
            };
            sink.note_on(self.held_note);
        } else if self.gate && level < GATE_OFF {
            self.gate = false;
            sink.note_off(self.held_note);
        } else if self.gate {
            if let Some(p) = pitch_in {
                let s = quantize_semitone(x[p], self.semitone);
                if s != self.semitone {
                    self.semitone = s;
                    let new = (PITCH_BASE_NOTE + s as i32) as u8;
                    sink.note_on(new); // on-then-off: legato in mono modes
                    sink.note_off(self.held_note);
                    self.held_note = new;
                }
            }
        }
    }
}

impl Default for CvState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec {
        knobs: heapless::Vec<(u8, u8), 16>,
        pars: heapless::Vec<(u8, u16), 16>,
        ons: heapless::Vec<u8, 16>,
        offs: heapless::Vec<u8, 16>,
    }
    impl CvSink for Rec {
        fn knob(&mut self, k: u8, v: u8) {
            self.knobs.push((k, v)).unwrap();
        }
        fn par(&mut self, p: u8, v: u16) {
            self.pars.push((p, v)).unwrap();
        }
        fn note_on(&mut self, n: u8) {
            self.ons.push(n).unwrap();
        }
        fn note_off(&mut self, n: u8) {
            self.offs.push(n).unwrap();
        }
    }
    const V: i32 = 4096; // counts per volt

    #[test]
    fn target_persistence_roundtrip_and_unknown_is_off() {
        for t in [
            CvTarget::Off,
            CvTarget::Knob3,
            CvTarget::Cutoff,
            CvTarget::Gate,
        ] {
            assert_eq!(CvTarget::from_u8(t.to_u8()), t);
        }
        assert_eq!(CvTarget::from_u8(0xEE), CvTarget::Off);
    }

    #[test]
    fn knob_target_scales_0_to_5v_and_deadbands() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets(
            [CvTarget::Knob1, CvTarget::Off, CvTarget::Off, CvTarget::Off],
            &mut s,
        );
        cv.tick([0, 0, 0, 0], &mut s);
        cv.tick([5 * V, 0, 0, 0], &mut s);
        cv.tick([5 * V, 0, 0, 0], &mut s); // unchanged: no re-emit
        cv.tick([6 * V, 0, 0, 0], &mut s); // clamped: still 255, no re-emit
        assert_eq!(s.knobs.as_slice(), &[(0, 0), (0, 255)]);
    }

    #[test]
    fn par_target_uses_common_block_numbers() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets(
            [
                CvTarget::Cutoff,
                CvTarget::Reso,
                CvTarget::Volume,
                CvTarget::Detune,
            ],
            &mut s,
        );
        cv.tick([5 * V, 5 * V, 5 * V, 5 * V], &mut s);
        let pars: heapless::Vec<u8, 16> = s.pars.iter().map(|(p, _)| *p).collect();
        assert_eq!(pars.as_slice(), &[0x04, 0x05, 0x01, 0x03]);
        assert!(s.pars.iter().all(|(_, v)| *v == 0xFFFF));
    }

    #[test]
    fn gate_hysteresis_and_fixed_note_without_pitch() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets(
            [CvTarget::Gate, CvTarget::Off, CvTarget::Off, CvTarget::Off],
            &mut s,
        );
        cv.tick([8193, 0, 0, 0], &mut s); // > 2V: on
        assert_eq!(s.ons.as_slice(), &[60]);
        cv.tick([5000, 0, 0, 0], &mut s); // between thresholds: hold
        assert!(s.offs.is_empty());
        cv.tick([4095, 0, 0, 0], &mut s); // < 1V: off
        assert_eq!(s.offs.as_slice(), &[60]);
    }

    #[test]
    fn pitch_tracks_voct_with_legato_retrigger() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets(
            [
                CvTarget::Pitch,
                CvTarget::Gate,
                CvTarget::Off,
                CvTarget::Off,
            ],
            &mut s,
        );
        cv.tick([0, 3 * V, 0, 0], &mut s); // gate on at 0V -> note 36
        assert_eq!(s.ons.as_slice(), &[36]);
        cv.tick([1 * V, 3 * V, 0, 0], &mut s); // 1V -> note 48: on(48) then off(36)
        assert_eq!(s.ons.as_slice(), &[36, 48]);
        assert_eq!(s.offs.as_slice(), &[36]);
        cv.tick([1 * V, 0, 0, 0], &mut s); // gate off -> off(48)
        assert_eq!(s.offs.as_slice(), &[36, 48]);
    }

    #[test]
    fn pitch_quantizer_hysteresis_no_flutter_on_boundary() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets(
            [
                CvTarget::Pitch,
                CvTarget::Gate,
                CvTarget::Off,
                CvTarget::Off,
            ],
            &mut s,
        );
        // Boundary between semitone 0 and 1 is at 4096/24 ≈ 171 counts.
        cv.tick([100, 3 * V, 0, 0], &mut s); // note 36
        cv.tick([180, 3 * V, 0, 0], &mut s); // just past boundary but < hysteresis: hold
        cv.tick([100, 3 * V, 0, 0], &mut s);
        assert_eq!(
            s.ons.as_slice(),
            &[36],
            "boundary jitter must not retrigger"
        );
        cv.tick([300, 3 * V, 0, 0], &mut s); // decisively past: note 37
        assert_eq!(s.ons.as_slice(), &[36, 37]);
    }

    #[test]
    fn pitch_without_gate_does_nothing() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets(
            [CvTarget::Pitch, CvTarget::Off, CvTarget::Off, CvTarget::Off],
            &mut s,
        );
        cv.tick([2 * V, 0, 0, 0], &mut s);
        assert!(s.ons.is_empty() && s.offs.is_empty() && s.knobs.is_empty() && s.pars.is_empty());
    }

    #[test]
    fn clearing_gate_target_releases_held_note() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets(
            [CvTarget::Gate, CvTarget::Off, CvTarget::Off, CvTarget::Off],
            &mut s,
        );
        cv.tick([3 * V, 0, 0, 0], &mut s);
        assert_eq!(s.ons.as_slice(), &[60]);
        cv.set_targets([CvTarget::Off; 4], &mut s); // reassignment: no stuck note
        assert_eq!(s.offs.as_slice(), &[60]);
    }
}
