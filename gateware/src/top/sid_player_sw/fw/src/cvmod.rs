//! CV modulation of live SID playback (pure, host-testable).
//!
//! The three eurorack CV inputs offset/override SID registers each PLAY frame:
//!   CV1 -> filter cutoff offset (bipolar)
//!   CV2 -> pulse-width offset, all 3 voices (bipolar)
//!   CV3 -> progressive voice mute (unipolar, 4 zones, hysteresis)
//! `compute` has no hardware access; the ISR feeds it the SID shadow + raw CV
//! counts + jack-detect bits and drains the returned writes via sid_write_bp.

pub const SID_REGS: usize = 0x20;
pub type SidShadow = [u8; SID_REGS];

/// Max register writes one `compute` can emit: cutoff(2)+pw(6)+ctrl(3)=11; pad.
pub const MAX_WRITES: usize = 16;
pub type WriteList = heapless::Vec<(u8, u8), MAX_WRITES>;

// --- Tunables (all integer; no_std-friendly) -------------------------------
/// Calibrated input scale: 4000 counts == 1 volt (matches PMOD0 info CSR).
pub const COUNTS_PER_VOLT: i32 = 4000;
/// Cutoff offset depth: ~410 cutoff-counts/volt -> +/-5V ~= full 11-bit range.
pub const CUTOFF_CTS_PER_V: i32 = 410;
/// PW offset depth: ~400 PW-counts/volt -> +/-5V ~= +/-half of 12-bit range.
pub const PW_CTS_PER_V: i32 = 400;
/// 1-pole EMA shift for input slew (acc += (raw-acc) >> K). Lower = snappier.
pub const SLEW_K: u32 = 3;
/// CV3 zone width (1.25 V) and hysteresis deadband (0.12 V), in counts.
pub const ZONE_WIDTH: i32 = 5000; // 1.25 * 4000
pub const ZONE_HYST:  i32 = 480;  // 0.12 * 4000

// SID register layout.
const PW_REGS:   [(usize, usize); 3] = [(0x02, 0x03), (0x09, 0x0A), (0x10, 0x11)];
const CTRL_REGS: [usize; 3] = [0x04, 0x0B, 0x12];
const FC_LO: usize = 0x15;
const FC_HI: usize = 0x16;

/// Sentinel for "chip value unknown" in the change-detection cache.
const UNKNOWN: i16 = -1;

pub fn cutoff_base(shadow: &SidShadow) -> i32 {
    ((shadow[FC_LO] & 0x07) as i32) | ((shadow[FC_HI] as i32) << 3)
}

pub fn pw_base(shadow: &SidShadow, voice: usize) -> i32 {
    let (lo, hi) = PW_REGS[voice];
    (shadow[lo] as i32) | (((shadow[hi] & 0x0F) as i32) << 8)
}

/// CV3 mute zone (0..3) from slewed CV counts, with hysteresis around the
/// 5000/10000/15000-count boundaries so a noisy input doesn't chatter.
pub fn zone_with_hyst(v: i32, prev: u8) -> u8 {
    let mut z = prev as i32;
    while z < 3 && v >= (z + 1) * ZONE_WIDTH + ZONE_HYST { z += 1; }
    while z > 0 && v <  z * ZONE_WIDTH - ZONE_HYST { z -= 1; }
    z as u8
}

/// Per-feature modulation state, owned by the playback struct.
pub struct CvMod {
    /// EMA accumulators for the 3 CV inputs (raw counts).
    slew: [i32; 3],
    /// Per-CV jack-patched state from the previous frame (for edge detect).
    prev_patched: [bool; 3],
    /// CV3 mute zone carried across frames (hysteresis memory).
    prev_zone: u8,
    /// Last value this module emitted (or believes the chip holds) per register;
    /// UNKNOWN forces a write. Used for change-detection.
    last_emit: [i16; SID_REGS],
}

impl CvMod {
    pub const fn new() -> Self {
        CvMod {
            slew: [0; 3],
            prev_patched: [false; 3],
            prev_zone: 0,
            last_emit: [UNKNOWN; SID_REGS],
        }
    }

    /// Reset all state (call on tune (re)load, alongside zeroing the shadow).
    pub fn reset(&mut self) {
        self.slew = [0; 3];
        self.prev_patched = [false; 3];
        self.prev_zone = 0;
        self.last_emit = [UNKNOWN; SID_REGS];
    }

    fn slew_update(&mut self, i: usize, raw: i32, rising: bool) -> i32 {
        if rising {
            self.slew[i] = raw; // snap for instant response on patch-in
        } else {
            self.slew[i] += (raw - self.slew[i]) >> SLEW_K;
        }
        self.slew[i]
    }

    fn emit(&mut self, out: &mut WriteList, shadow: &SidShadow,
            dirty: u32, reg: usize, val: u8) {
        // The chip currently holds: the tune's base (shadow) if the tune wrote
        // this reg this frame, else whatever we last emitted.
        let current = if dirty & (1 << reg) != 0 {
            shadow[reg] as i16
        } else {
            self.last_emit[reg]
        };
        if current != val as i16 {
            let _ = out.push((reg as u8, val)); // capacity proven by MAX_WRITES
        }
        self.last_emit[reg] = val as i16;
    }

    /// Produce this frame's override writes.
    pub fn compute(&mut self, shadow: &SidShadow, dirty: u32,
                   cv_raw: [i32; 3], jacks: u8) -> WriteList {
        let mut out = WriteList::new();
        let patched = [jacks & 1 != 0, jacks & 2 != 0, jacks & 4 != 0];

        // --- CV1: filter cutoff offset (bipolar) ---
        if patched[0] {
            let rising = !self.prev_patched[0];
            let cv = self.slew_update(0, cv_raw[0], rising);
            let off = cv * CUTOFF_CTS_PER_V / COUNTS_PER_VOLT;
            let fin = (cutoff_base(shadow) + off).clamp(0, 2047);
            self.emit(&mut out, shadow, dirty, FC_LO, (fin & 7) as u8);
            self.emit(&mut out, shadow, dirty, FC_HI, (fin >> 3) as u8);
        } else if self.prev_patched[0] {
            // falling edge: restore base (handled fully in Task 5)
        }

        // --- CV2: pulse-width offset (bipolar), all 3 voices ---
        if patched[1] {
            let rising = !self.prev_patched[1];
            let cv = self.slew_update(1, cv_raw[1], rising);
            let off = cv * PW_CTS_PER_V / COUNTS_PER_VOLT;
            for v in 0..3 {
                let (lo, hi) = PW_REGS[v];
                let fin = (pw_base(shadow, v) + off).clamp(0, 4095);
                self.emit(&mut out, shadow, dirty, lo, (fin & 0xFF) as u8);
                self.emit(&mut out, shadow, dirty, hi, ((fin >> 8) & 0x0F) as u8);
            }
        } else if self.prev_patched[1] {
            // falling edge: restore (Task 5)
        }

        // --- CV3: progressive voice mute (unipolar, 4 zones, hysteresis) ---
        if patched[2] {
            let rising = !self.prev_patched[2];
            let cv = self.slew_update(2, cv_raw[2], rising);
            let zone = zone_with_hyst(cv, self.prev_zone);
            self.prev_zone = zone;
            for v in 0..3 {
                let ctrl = CTRL_REGS[v];
                let muted = v >= (3 - zone as usize); // zone N mutes the N highest voices
                let val = if muted { shadow[ctrl] & 0x0F } else { shadow[ctrl] };
                self.emit(&mut out, shadow, dirty, ctrl, val);
            }
        } else if self.prev_patched[2] {
            // falling edge: restore (Task 5)
            self.prev_zone = 0;
        }

        self.prev_patched[0] = patched[0];
        self.prev_patched[1] = patched[1];
        self.prev_patched[2] = patched[2];
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_extractors() {
        let mut s: SidShadow = [0; SID_REGS];
        // cutoff = FC_LO[2:0] | FC_HI<<3
        s[0x15] = 0b101;       // low 3 bits
        s[0x16] = 0xFF;        // high 8 bits
        assert_eq!(cutoff_base(&s), (0xFF << 3) | 0b101); // 2045

        // PW voice 0 = lo | (hi&0xF)<<8
        s[0x02] = 0x34;
        s[0x03] = 0x12;        // only low nibble counts
        assert_eq!(pw_base(&s, 0), 0x234);
        // voices 1 and 2 use their own reg pairs
        s[0x09] = 0xFF; s[0x0A] = 0x0F;
        assert_eq!(pw_base(&s, 1), 0xFFF);
        s[0x10] = 0x00; s[0x11] = 0x00;
        assert_eq!(pw_base(&s, 2), 0x000);
    }

    #[test]
    fn new_is_idle() {
        let cv = CvMod::new();
        let s: SidShadow = [0; SID_REGS];
        // nothing patched -> no writes
        let out = { let mut cv = cv; cv.compute(&s, 0, [0, 0, 0], 0) };
        assert!(out.is_empty());
    }

    #[test]
    fn cv1_cutoff_offset_and_clamp() {
        let mut cv = CvMod::new();
        let mut s: SidShadow = [0; SID_REGS];
        // tune base cutoff = 1000
        let base = 1000;
        s[FC_LO] = (base & 7) as u8;
        s[FC_HI] = (base >> 3) as u8;

        // CV1 patched (jack bit 0), +5V => +~2050 -> clamps at 2047.
        // First frame snaps the slew to the raw value (rising edge).
        let out = cv.compute(&s, 0, [5 * COUNTS_PER_VOLT, 0, 0], 0b001);
        // Expect two writes for cutoff regs reconstructing the clamped value 2047.
        let mut got = 0i32;
        for &(r, v) in out.iter() {
            if r as usize == FC_LO { got |= (v as i32) & 7; }
            if r as usize == FC_HI { got |= (v as i32) << 3; }
        }
        assert_eq!(got, 2047, "cutoff should clamp to 11-bit max");

        // Negative CV pushes below base; -5V => 1000-2050 -> clamp 0.
        let mut cv2 = CvMod::new();
        let out = cv2.compute(&s, 0, [-5 * COUNTS_PER_VOLT, 0, 0], 0b001);
        let mut got = 0i32;
        for &(r, v) in out.iter() {
            if r as usize == FC_LO { got |= (v as i32) & 7; }
            if r as usize == FC_HI { got |= (v as i32) << 3; }
        }
        assert_eq!(got, 0, "cutoff should clamp to 0");
    }

    #[test]
    fn cv1_zero_volts_is_passthrough() {
        let mut cv = CvMod::new();
        let mut s: SidShadow = [0; SID_REGS];
        let base = 1234;
        s[FC_LO] = (base & 7) as u8;
        s[FC_HI] = (base >> 3) as u8;
        // 0V, patched: offset 0 -> final == base. The tune already wrote these regs
        // this frame (dirty), so the chip holds base -> compute emits nothing.
        let dirty = (1 << FC_LO) | (1 << FC_HI);
        let out = cv.compute(&s, dirty, [0, 0, 0], 0b001);
        assert!(out.is_empty(), "0V on a dirty cutoff = no extra writes");
    }

    #[test]
    fn cv1_unpatched_emits_nothing() {
        let mut cv = CvMod::new();
        let s: SidShadow = [0; SID_REGS];
        let out = cv.compute(&s, 0, [5 * COUNTS_PER_VOLT, 0, 0], 0b000);
        assert!(out.is_empty());
    }

    #[test]
    fn cv2_pw_offset_all_voices() {
        let mut cv = CvMod::new();
        let mut s: SidShadow = [0; SID_REGS];
        // distinct PW bases per voice
        for (v, &(lo, hi)) in PW_REGS.iter().enumerate() {
            let base = 1000 + (v as i32) * 100; // 1000,1100,1200
            s[lo] = (base & 0xFF) as u8;
            s[hi] = ((base >> 8) & 0x0F) as u8;
        }
        // CV2 patched (jack bit 1), +2.5V => +1000 PW (2.5*400).
        let out = cv.compute(&s, 0, [0, 2500 * COUNTS_PER_VOLT / 1000, 0], 0b010);

        for (v, &(lo, hi)) in PW_REGS.iter().enumerate() {
            let mut got = 0i32;
            for &(r, val) in out.iter() {
                if r as usize == lo { got |= val as i32; }
                if r as usize == hi { got |= ((val as i32) & 0x0F) << 8; }
            }
            let want = (1000 + (v as i32) * 100 + 1000).min(4095);
            assert_eq!(got, want, "voice {v} PW offset");
        }
    }

    #[test]
    fn cv2_clamps_to_12bit() {
        let mut cv = CvMod::new();
        let mut s: SidShadow = [0; SID_REGS];
        for &(lo, hi) in PW_REGS.iter() { s[lo] = 0xF0; s[hi] = 0x0F; } // base 4080
        let out = cv.compute(&s, 0, [0, 5 * COUNTS_PER_VOLT, 0], 0b010); // +2000 -> clamp 4095
        for &(lo, hi) in PW_REGS.iter() {
            let mut got = 0i32;
            for &(r, val) in out.iter() {
                if r as usize == lo { got |= val as i32; }
                if r as usize == hi { got |= ((val as i32) & 0x0F) << 8; }
            }
            assert_eq!(got, 4095);
        }
    }

    #[test]
    fn cv3_zone_hysteresis() {
        // boundaries at 5000/10000/15000 counts, deadband +/-480.
        // rising past a boundary needs +HYST; falling needs -HYST.
        assert_eq!(zone_with_hyst(0, 0), 0);
        assert_eq!(zone_with_hyst(ZONE_WIDTH - 1, 0), 0);          // just below b1
        assert_eq!(zone_with_hyst(ZONE_WIDTH + ZONE_HYST, 0), 1);  // crosses up
        assert_eq!(zone_with_hyst(ZONE_WIDTH + ZONE_HYST - 1, 0), 0); // inside deadband, stays
        // already in zone 1, drop only when below b1 - HYST
        assert_eq!(zone_with_hyst(ZONE_WIDTH - ZONE_HYST + 1, 1), 1); // inside deadband, stays
        assert_eq!(zone_with_hyst(ZONE_WIDTH - ZONE_HYST - 1, 1), 0); // drops
        // clamps and negatives
        assert_eq!(zone_with_hyst(-9999, 2), 0);
        assert_eq!(zone_with_hyst(3 * ZONE_WIDTH + ZONE_HYST + 1, 0), 3);
        assert_eq!(zone_with_hyst(99 * ZONE_WIDTH, 0), 3);
    }

    #[test]
    fn cv3_mutes_high_voices_first() {
        // zone N mutes the N highest voice indices (V3=idx2 first).
        let mut cv = CvMod::new();
        let mut s: SidShadow = [0; SID_REGS];
        // each voice ctrl: pulse waveform (0x40) + gate (0x01) = 0x41
        for &c in CTRL_REGS.iter() { s[c] = 0x41; }

        // ~1.25*1 + margin volts -> zone 1 -> mute idx2 only.
        let v = ZONE_WIDTH + ZONE_HYST + 1;
        let out = cv.compute(&s, 0, [0, 0, v], 0b100);
        // idx2 ctrl forced to low nibble (0x01); idx0/idx1 untouched (== shadow,
        // and dirty=0 with last_emit unknown -> emitted as shadow, harmless).
        let mut muted2 = None;
        for &(r, val) in out.iter() {
            if r as usize == CTRL_REGS[2] { muted2 = Some(val); }
        }
        assert_eq!(muted2, Some(0x01), "voice 3 (idx2) waveform bits cleared, gate kept");
    }
}
