//! Whole-bank USB import orchestrator (spec §4): pre-validate BANK.SYX,
//! then replace the 128-slot user bank with its contents. Host-testable:
//! generic over the FAT IO and the NOR flash so the full
//! reject-leaves-flash-untouched / sparse-replace semantics are provable
//! off-target.

use fatfs::{FileSystem, ReadWriteSeek};
use tiliqua_hal::nor_flash::{NorFlash, ReadNorFlash};

use crate::patch_store::{UserPatchStore, N_SLOTS};
use crate::usb_patch::{for_each_bank_patch, validate_bank};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImportOutcome {
    /// Validation rejected BANK.SYX (missing/corrupt/truncated/duplicate
    /// slot) — flash untouched.
    BadFile,
    /// Import started but aborted (flash error or the drive re-read
    /// diverged) — bank partially replaced, every slot valid-or-empty.
    Failed,
    /// Replaced the bank with N patches from the file.
    Imported(u8),
}

/// Replace the whole user bank with BANK.SYX's contents (spec §4).
/// Pass 1 validates everything before any flash write; the erase of
/// file-absent slots and the per-message saves then interleave into one
/// sweep whose worst interruption leaves only valid-or-empty slots.
pub fn import_bank<IO: ReadWriteSeek, F: NorFlash + ReadNorFlash>(
    fs: &FileSystem<IO>,
    store: &mut UserPatchStore<F>,
) -> ImportOutcome {
    let Some(sum) = validate_bank(fs) else {
        return ImportOutcome::BadFile;
    };
    for slot in 0..N_SLOTS {
        if !sum.has(slot) && store.erase_slot(slot).is_err() {
            return ImportOutcome::Failed;
        }
    }
    if for_each_bank_patch(fs, |slot, patch| store.save(slot, patch).is_ok()) {
        ImportOutcome::Imported(sum.count)
    } else {
        ImportOutcome::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch_store::{UserPatchStore, SLOT_SIZE};
    use crate::usb_patch::testfs::*;
    use fatfs::{FileSystem, FsOptions};
    use std::vec::Vec;

    // 128-slot in-memory NOR: the shared 4-slot MockFlash is too small for
    // whole-bank replace semantics (import touches all 128 slot indices, and
    // an out-of-range erase would fail the import spuriously).
    struct BigFlash {
        mem: Vec<u8>,
    }
    impl BigFlash {
        fn new() -> Self {
            Self {
                mem: vec![0xFF; 128 * SLOT_SIZE as usize],
            }
        }
    }
    use tiliqua_hal::nor_flash::{
        ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    };
    #[derive(Debug)]
    struct BigErr;
    impl NorFlashError for BigErr {
        fn kind(&self) -> NorFlashErrorKind {
            NorFlashErrorKind::Other
        }
    }
    impl ErrorType for BigFlash {
        type Error = BigErr;
    }
    impl ReadNorFlash for BigFlash {
        const READ_SIZE: usize = 1;
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), BigErr> {
            let o = offset as usize;
            if o + bytes.len() > self.mem.len() {
                return Err(BigErr);
            }
            bytes.copy_from_slice(&self.mem[o..o + bytes.len()]);
            Ok(())
        }
        fn capacity(&self) -> usize {
            self.mem.len()
        }
    }
    impl NorFlash for BigFlash {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = 4096;
        fn erase(&mut self, from: u32, to: u32) -> Result<(), BigErr> {
            if to as usize > self.mem.len() || from > to {
                return Err(BigErr);
            }
            self.mem[from as usize..to as usize].fill(0xFF);
            Ok(())
        }
        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), BigErr> {
            let o = offset as usize;
            if o + bytes.len() > self.mem.len() {
                return Err(BigErr);
            }
            for (i, &b) in bytes.iter().enumerate() {
                self.mem[o + i] &= b;
            }
            Ok(())
        }
    }

    fn fs_of<'a>(img: &'a mut Vec<u8>) -> FileSystem<VecDisk<'a>> {
        let base = BASE_LBA as usize * SECTOR;
        FileSystem::new(VecDisk::new(&mut img[base..]), FsOptions::new()).unwrap()
    }

    #[test]
    fn full_bank_replaces_everything() {
        let patches: Vec<(u8, [u8; 512])> =
            (0..128).map(|i| (i as u8, test_patch(i as u8))).collect();
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bank_bytes(&patches))]);
        let mut store = UserPatchStore::new(BigFlash::new(), 0);
        store.save(60, &test_patch(200)).unwrap(); // pre-existing patch
        let fs = fs_of(&mut img);
        assert_eq!(import_bank(&fs, &mut store), ImportOutcome::Imported(128));
        let mut out = [0u8; 512];
        for i in 0..128u8 {
            assert!(store.load(i, &mut out), "slot {i} must load");
            assert_eq!(out, test_patch(i), "slot {i} must hold the file's patch");
        }
    }

    #[test]
    fn sparse_bank_empties_unlisted_slots() {
        let patches = vec![(3u8, test_patch(3)), (7u8, test_patch(7))];
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bank_bytes(&patches))]);
        let mut store = UserPatchStore::new(BigFlash::new(), 0);
        store.save(5, &test_patch(50)).unwrap(); // must be wiped
        store.save(100, &test_patch(90)).unwrap(); // must be wiped
        let fs = fs_of(&mut img);
        assert_eq!(import_bank(&fs, &mut store), ImportOutcome::Imported(2));
        let mut out = [0u8; 512];
        assert!(store.load(3, &mut out) && out == test_patch(3));
        assert!(store.load(7, &mut out) && out == test_patch(7));
        assert!(
            !store.load(5, &mut out),
            "replace semantics: unlisted slot wiped"
        );
        assert!(!store.load(100, &mut out));
    }

    #[test]
    fn bad_file_leaves_flash_byte_identical() {
        let mut bytes = bank_bytes(&[(0u8, test_patch(1)), (1u8, test_patch(2))]);
        bytes[1034] = (bytes[1034] + 1) & 0x7F; // corrupt message 0's checksum
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bytes)]);
        let mut store = UserPatchStore::new(BigFlash::new(), 0);
        store.save(10, &test_patch(11)).unwrap();
        let before = {
            let f = store.flash_mut();
            f.mem.clone()
        };
        let fs = fs_of(&mut img);
        assert_eq!(import_bank(&fs, &mut store), ImportOutcome::BadFile);
        assert_eq!(
            store.flash_mut().mem,
            before,
            "a rejected file must not touch flash"
        );
    }

    #[test]
    fn missing_file_is_bad_file() {
        let mut img = build_gpt_fat_image(&[("OTHER.TXT", b"x")]);
        let mut store = UserPatchStore::new(BigFlash::new(), 0);
        let fs = fs_of(&mut img);
        assert_eq!(import_bank(&fs, &mut store), ImportOutcome::BadFile);
    }
}
