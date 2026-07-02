//! Flash-backed 128-slot user patch bank (M4_USER_PATCH_BANKS.md §6c).
//!
//! One 4 KiB erase sector per slot at USER_BANK_FLASH_BASE (unused flash tail
//! past the 8 bitstream slots; survives normal `pdm run flash` reflashes).
//! Torn-write safety: the 8-byte header (magic+version+payload checksum) is
//! programmed AFTER the payload, so an interrupted save reads back as empty —
//! never as a corrupt patch handed to the engine.

use tiliqua_hal::nor_flash::{NorFlash, ReadNorFlash};

pub const USER_BANK_FLASH_BASE: u32 = 0xF0_0000;
pub const SLOT_SIZE: u32 = 4096;
pub const N_SLOTS: u8 = 128;

const MAGIC: [u8; 4] = *b"MBUP";
const VERSION: u8 = 1;
const HEADER_LEN: u32 = 8;

pub fn payload_checksum(data: &[u8; 512]) -> u16 {
    data.iter().fold(0u16, |a, &b| a.wrapping_add(b as u16))
}

pub struct UserPatchStore<F> {
    flash: F,
    base: u32,
}

impl<F: NorFlash + ReadNorFlash> UserPatchStore<F> {
    pub fn new(flash: F, base: u32) -> Self { Self { flash, base } }
    /// Take the flash back out (tests / teardown).
    pub fn into_inner(self) -> F { self.flash }

    fn slot_addr(&self, slot: u8) -> u32 {
        self.base + (slot as u32) * SLOT_SIZE
    }

    /// Read + validate the 8-byte header. Returns the expected payload
    /// checksum on success.
    fn read_header(&mut self, slot: u8) -> Option<u16> {
        let mut hdr = [0u8; HEADER_LEN as usize];
        if slot >= N_SLOTS { return None; }
        if self.flash.read(self.slot_addr(slot), &mut hdr).is_err() { return None; }
        if hdr[0..4] != MAGIC || hdr[4] != VERSION { return None; }
        Some(u16::from_le_bytes([hdr[6], hdr[7]]))
    }

    pub fn load(&mut self, slot: u8, out: &mut [u8; 512]) -> bool {
        let Some(want) = self.read_header(slot) else { return false; };
        if self.flash.read(self.slot_addr(slot) + HEADER_LEN, out).is_err() {
            return false;
        }
        payload_checksum(out) == want
    }

    pub fn name(&mut self, slot: u8, out: &mut [u8; 16]) -> bool {
        if self.read_header(slot).is_none() { return false; }
        self.flash.read(self.slot_addr(slot) + HEADER_LEN, out).is_ok()
    }

    pub fn save(&mut self, slot: u8, patch: &[u8; 512]) -> Result<(), F::Error> {
        debug_assert!(slot < N_SLOTS);
        let a = self.slot_addr(slot);
        self.flash.erase(a, a + SLOT_SIZE)?;
        // Payload first; header LAST — the header is the commit point, so an
        // interrupted save can never validate.
        self.flash.write(a + HEADER_LEN, patch)?;
        let mut hdr = [0u8; HEADER_LEN as usize];
        hdr[0..4].copy_from_slice(&MAGIC);
        hdr[4] = VERSION;
        hdr[5] = 0;
        hdr[6..8].copy_from_slice(&payload_checksum(patch).to_le_bytes());
        self.flash.write(a, &hdr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiliqua_hal::nor_flash::{ErrorType, NorFlashErrorKind, NorFlashError};

    // ---- in-memory NOR mock: erase -> 0xFF, write -> AND (real NOR can only
    // clear bits), so a double-write-without-erase corrupts like hardware ----
    const MOCK_SLOTS: usize = 4;
    const MOCK_SIZE: usize = MOCK_SLOTS * SLOT_SIZE as usize;

    #[derive(Debug)]
    struct MockErr;
    impl NorFlashError for MockErr {
        fn kind(&self) -> NorFlashErrorKind { NorFlashErrorKind::Other }
    }

    struct MockFlash {
        mem: [u8; MOCK_SIZE],
    }
    impl MockFlash {
        fn new() -> Self { Self { mem: [0xFF; MOCK_SIZE] } }
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

    fn test_patch(seed: u8) -> [u8; 512] {
        let mut p = [0u8; 512];
        for (i, b) in p.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31).wrapping_add(seed);
        }
        p[..16].copy_from_slice(b"USER PATCH NAME "); // sid_patch_t.name
        p
    }

    #[test]
    fn empty_slots_load_false() {
        let mut s = UserPatchStore::new(MockFlash::new(), 0);
        let mut out = [0u8; 512];
        for slot in 0..MOCK_SLOTS as u8 {
            assert!(!s.load(slot, &mut out), "erased slot must be empty");
            let mut n = [0u8; 16];
            assert!(!s.name(slot, &mut n));
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let mut s = UserPatchStore::new(MockFlash::new(), 0);
        let p = test_patch(7);
        s.save(2, &p).unwrap();
        let mut out = [0u8; 512];
        assert!(s.load(2, &mut out));
        assert_eq!(out, p);
        // neighbors untouched
        assert!(!s.load(1, &mut out));
        assert!(!s.load(3, &mut out));
    }

    #[test]
    fn name_reads_payload_head() {
        let mut s = UserPatchStore::new(MockFlash::new(), 0);
        s.save(0, &test_patch(1)).unwrap();
        let mut n = [0u8; 16];
        assert!(s.name(0, &mut n));
        assert_eq!(&n, b"USER PATCH NAME ");
    }

    #[test]
    fn overwrite_slot() {
        let mut s = UserPatchStore::new(MockFlash::new(), 0);
        s.save(1, &test_patch(1)).unwrap();
        s.save(1, &test_patch(2)).unwrap();
        let mut out = [0u8; 512];
        assert!(s.load(1, &mut out));
        assert_eq!(out, test_patch(2));
    }

    #[test]
    fn corrupted_payload_fails_checksum() {
        let flash = MockFlash::new();
        let mut s = UserPatchStore::new(flash, 0);
        s.save(0, &test_patch(3)).unwrap();
        // simulate bit-rot / torn program inside the payload
        let mut f = s.into_inner();
        f.mem[HEADER_LEN as usize + 100] &= 0x00;
        let mut s = UserPatchStore::new(f, 0);
        let mut out = [0u8; 512];
        assert!(!s.load(0, &mut out), "bad checksum must read as empty");
    }

    #[test]
    fn torn_write_header_missing_reads_empty() {
        // Interrupted save: erase + payload done, header never written.
        let mut flash = MockFlash::new();
        let p = test_patch(4);
        flash.erase(0, SLOT_SIZE).unwrap();
        flash.write(HEADER_LEN, &p).unwrap();
        let mut s = UserPatchStore::new(flash, 0);
        let mut out = [0u8; 512];
        assert!(!s.load(0, &mut out));
    }

    #[test]
    fn wrong_version_reads_empty() {
        let mut s = UserPatchStore::new(MockFlash::new(), 0);
        s.save(0, &test_patch(5)).unwrap();
        let mut f = s.into_inner();
        f.mem[4] &= 0x02; // clobber version byte
        let mut s = UserPatchStore::new(f, 0);
        let mut out = [0u8; 512];
        assert!(!s.load(0, &mut out));
    }

    #[test]
    fn nonzero_base_addressing() {
        let mut s = UserPatchStore::new(MockFlash::new(), SLOT_SIZE);
        s.save(0, &test_patch(6)).unwrap(); // lands at flash offset 4096
        let mut out = [0u8; 512];
        assert!(s.load(0, &mut out));
        let f = s.into_inner();
        assert_eq!(&f.mem[SLOT_SIZE as usize..SLOT_SIZE as usize + 4], b"MBUP");
        assert_eq!(&f.mem[..8], &[0xFF; 8]); // slot before base untouched
    }
}
