//! Persisted menu settings (M5_MENU_CARDS_CV_MOD.md §6d): one 16-byte record
//! in the option-storage flash window (`manifest.get_option_storage_window()`).
//! Layout: "MBS5" | version | midi_src | cv_targets[4] | usb_mode | reserved[4] | chk.
//! chk = two's-complement byte making the whole record sum to 0 (mod 256) —
//! same family as patch_store's payload checksum. Any validation failure
//! loads defaults (TRS, all CV Off, MIDI). Saves are debounced by the caller and
//! skipped when the stored record is already identical (flash wear).

use tiliqua_hal::nor_flash::{NorFlash, ReadNorFlash};

const MAGIC: [u8; 4] = *b"MBS5";
const VERSION: u8 = 2;   // v2 adds usb_mode (M6); v1 records still decode.
pub const RECORD_LEN: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Settings {
    pub midi_src: u8,        // 0 = TRS, 1 = USB
    pub cv_targets: [u8; 4], // CvTarget::to_u8 encoding
    pub usb_mode: u8,        // 0 = MIDI, 1 = Storage (M6)
}

pub fn encode(s: &Settings) -> [u8; RECORD_LEN] {
    let mut r = [0u8; RECORD_LEN];
    r[0..4].copy_from_slice(&MAGIC);
    r[4] = VERSION;
    r[5] = s.midi_src;
    r[6..10].copy_from_slice(&s.cv_targets);
    r[10] = s.usb_mode;
    let sum: u8 = r[..15].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    r[15] = sum.wrapping_neg();
    r
}

pub fn decode(r: &[u8; RECORD_LEN]) -> Option<Settings> {
    if r[0..4] != MAGIC || !(r[4] == 1 || r[4] == 2) { return None; }
    if r.iter().fold(0u8, |a, &b| a.wrapping_add(b)) != 0 { return None; }
    let mut cv = [0u8; 4];
    cv.copy_from_slice(&r[6..10]);
    // v1 records carry reserved-zero at byte 10, so reading it as usb_mode
    // is exactly the intended "old records default to MIDI" behavior.
    Some(Settings { midi_src: r[5], cv_targets: cv, usb_mode: r[10] & 1 })
}

pub fn load<F: ReadNorFlash>(flash: &mut F, base: u32) -> Settings {
    let mut r = [0u8; RECORD_LEN];
    if flash.read(base, &mut r).is_err() { return Settings::default(); }
    decode(&r).unwrap_or_default()
}

pub fn save<F: NorFlash + ReadNorFlash>(flash: &mut F, base: u32,
                                        s: &Settings) -> Result<(), F::Error> {
    let rec = encode(s);
    let mut cur = [0u8; RECORD_LEN];
    if flash.read(base, &mut cur).is_ok() && cur == rec {
        return Ok(()); // identical: skip the erase (flash wear)
    }
    flash.erase(base, base + 4096)?;
    flash.write(base, &rec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiliqua_hal::nor_flash::{ErrorType, NorFlashErrorKind, NorFlashError};

    // ---- in-memory NOR mock: erase -> 0xFF, write -> AND (real NOR can only
    // clear bits), so a double-write-without-erase corrupts like hardware.
    // Adapted from patch_store.rs's test MockFlash: fixed 8192-byte backing
    // array (2 x 4096-byte erase sectors) instead of a Vec, plus an
    // erase_count counter to verify save()'s skip-if-identical debounce. ----
    const MOCK_SIZE: usize = 8192;

    #[derive(Debug)]
    struct MockErr;
    impl NorFlashError for MockErr {
        fn kind(&self) -> NorFlashErrorKind { NorFlashErrorKind::Other }
    }

    struct MockFlash {
        mem: [u8; MOCK_SIZE],
        erase_count: usize,
    }
    impl MockFlash {
        fn new() -> Self { Self { mem: [0xFF; MOCK_SIZE], erase_count: 0 } }
    }
    impl ErrorType for MockFlash { type Error = MockErr; }
    impl ReadNorFlash for MockFlash {
        const READ_SIZE: usize = 1;
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), MockErr> {
            let o = offset as usize;
            if o + bytes.len() > MOCK_SIZE { return Err(MockErr); }
            bytes.copy_from_slice(&self.mem[o..o + bytes.len()]);
            Ok(())
        }
        fn capacity(&self) -> usize { MOCK_SIZE }
    }
    impl NorFlash for MockFlash {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = 4096;
        fn erase(&mut self, from: u32, to: u32) -> Result<(), MockErr> {
            if to as usize > MOCK_SIZE || from > to { return Err(MockErr); }
            self.mem[from as usize..to as usize].fill(0xFF);
            self.erase_count += 1;
            Ok(())
        }
        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), MockErr> {
            let o = offset as usize;
            if o + bytes.len() > MOCK_SIZE { return Err(MockErr); }
            for (i, &b) in bytes.iter().enumerate() {
                self.mem[o + i] &= b; // NOR semantics
            }
            Ok(())
        }
    }

    #[test]
    fn roundtrip() {
        let s = Settings { midi_src: 1, cv_targets: [0, 3, 11, 12], usb_mode: 0 };
        assert_eq!(decode(&encode(&s)), Some(s));
    }

    #[test]
    fn corrupt_records_rejected() {
        let s = Settings { midi_src: 1, cv_targets: [1, 2, 3, 4], usb_mode: 0 };
        let good = encode(&s);
        let mut bad_magic = good; bad_magic[0] ^= 0xFF;
        assert_eq!(decode(&bad_magic), None);
        let mut bad_ver = good; bad_ver[4] = 99;
        assert_eq!(decode(&bad_ver), None);
        let mut bad_sum = good; bad_sum[6] ^= 0x01; // flip a target byte, keep chk
        assert_eq!(decode(&bad_sum), None);
    }

    #[test]
    fn load_defaults_on_blank_flash() {
        let mut f = MockFlash::new();
        let s = load(&mut f, 0);
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn save_then_load_and_identical_save_skips_erase() {
        let mut f = MockFlash::new();
        let s = Settings { midi_src: 1, cv_targets: [12, 11, 0, 0], usb_mode: 0 };
        save(&mut f, 0, &s).unwrap();
        assert_eq!(load(&mut f, 0), s);
        let erases_before = f.erase_count;
        save(&mut f, 0, &s).unwrap(); // identical: must not erase again
        assert_eq!(f.erase_count, erases_before);
    }

    #[test]
    fn v2_roundtrip_with_usb_mode() {
        let s = Settings { midi_src: 1, cv_targets: [0, 3, 11, 12], usb_mode: 1 };
        assert_eq!(decode(&encode(&s)), Some(s));
    }

    #[test]
    fn v1_record_decodes_with_usb_mode_default() {
        // A v1 record as M5 wrote it: version byte 1, byte 10 reserved-zero.
        let s = Settings { midi_src: 1, cv_targets: [1, 2, 3, 4], usb_mode: 0 };
        let mut r = encode(&s);
        r[4] = 1; // rewrite as v1
        let sum: u8 = r[..15].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        r[15] = sum.wrapping_neg();
        assert_eq!(decode(&r), Some(s)); // decodes, usb_mode defaults to 0
    }

    #[test]
    fn unknown_version_rejected() {
        let s = Settings::default();
        let mut r = encode(&s);
        r[4] = 3;
        let sum: u8 = r[..15].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        r[15] = sum.wrapping_neg();
        assert_eq!(decode(&r), None);
    }
}
