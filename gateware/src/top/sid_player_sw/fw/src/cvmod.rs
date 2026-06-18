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

    /// Produce this frame's override writes. Stub — filled in by later tasks.
    pub fn compute(&mut self, _shadow: &SidShadow, _dirty: u32,
                   _cv_raw: [i32; 3], _jacks: u8) -> WriteList {
        WriteList::new()
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
}
