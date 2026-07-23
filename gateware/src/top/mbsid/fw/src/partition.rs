//! Find the starting LBA of the first FAT volume on a USB block device.
//!
//! `fatfs::FileSystem::new` expects to read the FAT boot sector (BPB) at byte
//! offset 0 of the stream it is given. Real USB sticks, however, usually place
//! a partition table at LBA 0 and the FAT volume further in:
//!
//! * GPT: LBA 0 is a *protective MBR* (partition type `0xEE`), LBA 1 is the GPT
//!   header, and the partition entry array (default LBA 2) gives the first
//!   partition's starting LBA (commonly 34).
//! * Classic MBR: LBA 0 holds up to four partition entries at offset `0x1BE`;
//!   the FAT volume typically starts at LBA 2048.
//! * "Superfloppy": no partition table — LBA 0 *is* the FAT BPB (start 0).
//!
//! Reading LBA 0 directly (as the old code did) only works for the superfloppy
//! case; on a partitioned stick it parses garbage as a BPB and fails. This
//! module inspects LBA 0 and returns the correct base LBA for all three layouts.
//!
//! Pure byte-parsing with an injected block-reader closure → host-testable
//! (no `tiliqua_pac` dependency).

const SIG_OFF: usize = 510; // 0x55AA boot signature
const MBR_PART0: usize = 446; // 0x1BE — first MBR partition entry
const PART_TYPE: usize = 4; // entry-relative offset of the type byte
const PART_LBA: usize = 8; // entry-relative offset of the start-LBA (LE u32)
const GPT_PROTECTIVE: u8 = 0xEE;

fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Heuristic: does this 512-byte sector look like a FAT BPB (superfloppy),
/// rather than a partition table? FAT boot sectors begin with a jump
/// instruction and declare a sane bytes-per-sector. A partition-table MBR does
/// neither, so this cleanly distinguishes the two even though both carry the
/// `0x55AA` signature.
fn looks_like_fat_bpb(blk: &[u8; 512]) -> bool {
    let jump = blk[0] == 0xEB || blk[0] == 0xE9;
    let bps = u16::from_le_bytes([blk[11], blk[12]]);
    jump && matches!(bps, 512 | 1024 | 2048 | 4096)
}

/// Return the starting LBA of the first FAT volume.
///
/// `read_block(lba, &mut buf)` must fill `buf` with the 512-byte sector at
/// `lba`. Any read failure or unrecognised layout falls back to LBA 0 (the
/// superfloppy assumption), preserving the old behaviour as a safe default.
pub fn first_partition_lba<F, E>(mut read_block: F) -> u32
where
    F: FnMut(u32, &mut [u8; 512]) -> Result<(), E>,
{
    let mut blk = [0u8; 512];
    if read_block(0, &mut blk).is_err() {
        return 0;
    }
    // Superfloppy: LBA 0 is already the FAT BPB.
    if looks_like_fat_bpb(&blk) {
        return 0;
    }
    // From here on we need a valid MBR boot signature to trust the table.
    if blk[SIG_OFF] != 0x55 || blk[SIG_OFF + 1] != 0xAA {
        return 0;
    }

    // GPT protective MBR → follow the GPT header to the partition entry array.
    if blk[MBR_PART0 + PART_TYPE] == GPT_PROTECTIVE {
        if read_block(1, &mut blk).is_err() {
            return 0;
        }
        if &blk[0..8] != b"EFI PART" {
            return 0;
        }
        // GPT header: partition-entry-array starting LBA at offset 72 (8 bytes
        // LE; high word is always 0 on these disks, so the low u32 suffices).
        let entries_lba = le_u32(&blk[72..76]);
        if read_block(entries_lba, &mut blk).is_err() {
            return 0;
        }
        // First entry: 16-byte all-zero type GUID means "unused"; starting LBA
        // is at entry offset 32 (8 bytes LE).
        if blk[0..16].iter().all(|&b| b == 0) {
            return 0;
        }
        return le_u32(&blk[32..36]);
    }

    // Classic MBR: take the first entry with a non-zero partition type.
    for i in 0..4 {
        let e = MBR_PART0 + i * 16;
        if blk[e + PART_TYPE] != 0 {
            return le_u32(&blk[e + PART_LBA..e + PART_LBA + 4]);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank() -> [u8; 512] {
        [0u8; 512]
    }

    /// A reader backed by up to three canned sectors.
    fn reader(
        sectors: alloc::vec::Vec<(u32, [u8; 512])>,
    ) -> impl FnMut(u32, &mut [u8; 512]) -> Result<(), ()> {
        move |lba, buf| {
            for (l, s) in &sectors {
                if *l == lba {
                    *buf = *s;
                    return Ok(());
                }
            }
            *buf = [0u8; 512];
            Ok(())
        }
    }

    extern crate alloc;

    #[test]
    fn gpt_returns_partition_start() {
        // Reproduces the user's stick: GPT, FAT volume at LBA 34.
        let mut lba0 = blank();
        lba0[SIG_OFF] = 0x55;
        lba0[SIG_OFF + 1] = 0xAA;
        lba0[MBR_PART0 + PART_TYPE] = GPT_PROTECTIVE;

        let mut lba1 = blank();
        lba1[0..8].copy_from_slice(b"EFI PART");
        lba1[72..76].copy_from_slice(&2u32.to_le_bytes()); // entries at LBA 2

        let mut lba2 = blank();
        lba2[0] = 0x01; // non-zero type GUID → entry is used
        lba2[32..36].copy_from_slice(&34u32.to_le_bytes()); // starting LBA 34

        let f = reader(alloc::vec![(0, lba0), (1, lba1), (2, lba2)]);
        assert_eq!(first_partition_lba(f), 34);
    }

    #[test]
    fn classic_mbr_returns_partition_start() {
        let mut lba0 = blank();
        lba0[SIG_OFF] = 0x55;
        lba0[SIG_OFF + 1] = 0xAA;
        lba0[MBR_PART0 + PART_TYPE] = 0x0C; // FAT32 LBA
        lba0[MBR_PART0 + PART_LBA..MBR_PART0 + PART_LBA + 4]
            .copy_from_slice(&2048u32.to_le_bytes());

        let f = reader(alloc::vec![(0, lba0)]);
        assert_eq!(first_partition_lba(f), 2048);
    }

    #[test]
    fn superfloppy_returns_zero() {
        // FAT BPB directly at LBA 0: jump instruction + 512 bytes/sector.
        let mut lba0 = blank();
        lba0[0] = 0xEB;
        lba0[1] = 0x58;
        lba0[2] = 0x90;
        lba0[11..13].copy_from_slice(&512u16.to_le_bytes());
        lba0[SIG_OFF] = 0x55;
        lba0[SIG_OFF + 1] = 0xAA;

        let f = reader(alloc::vec![(0, lba0)]);
        assert_eq!(first_partition_lba(f), 0);
    }

    #[test]
    fn garbage_falls_back_to_zero() {
        // No FAT jump, no valid signature → safe default of LBA 0.
        let f = reader(alloc::vec![(0, blank())]);
        assert_eq!(first_partition_lba(f), 0);
    }
}
