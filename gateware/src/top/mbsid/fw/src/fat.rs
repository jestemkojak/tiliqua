//! fatfs storage adapter over a generic 512-byte block device.
//!
//! Ported from top/sid_player_sw/fw/src/fat.rs, with the concrete `UsbMsc`
//! dependency replaced by the `BlockIo` trait so the adapter (and, in M6b,
//! its write-back cache) is host-testable against an in-memory disk.
//! M6b: writes go through a single-sector read-modify-write cache with
//! write-back on flush/eviction (BlockIo::write_block).

pub use fatfs::{FileSystem, FsOptions};
use fatfs::{IoBase, IoError, Read, Seek, SeekFrom, Write};

/// Minimal 512-byte block device. Implemented by `&usb_msc::UsbMsc` on
/// target and by in-memory disks in host tests.
pub trait BlockIo {
    // Deferred: `Result<_, ()>` becomes a real error enum in the
    // error-handling refactor (review step 4). Both impls (UsbMsc on
    // target, MemDisk in tests) discard the cause today anyway.
    #[allow(clippy::result_unit_err)]
    fn read_block(&mut self, lba: u32, buf: &mut [u8; 512]) -> Result<(), ()>;
    #[allow(clippy::result_unit_err)]
    fn write_block(&mut self, lba: u32, buf: &[u8; 512]) -> Result<(), ()>;
}

#[derive(Debug)]
pub struct StorageError;

impl IoError for StorageError {
    fn is_interrupted(&self) -> bool {
        false
    }
    fn new_unexpected_eof_error() -> Self {
        StorageError
    }
    fn new_write_zero_error() -> Self {
        StorageError
    }
}

/// Block-cached storage adapter: presents the first FAT partition as a
/// byte stream starting at its BPB (fatfs mounts at stream offset 0).
pub struct MscStorage<B: BlockIo> {
    io: B,
    pos: u64,
    base_lba: u32,
    cache_lba: Option<u32>,
    cache: [u8; 512],
    dirty: bool,
}

impl<B: BlockIo> MscStorage<B> {
    pub fn new(mut io: B) -> Self {
        let base_lba = crate::partition::first_partition_lba(|lba, buf| io.read_block(lba, buf));
        Self {
            io,
            pos: 0,
            base_lba,
            cache_lba: None,
            cache: [0u8; 512],
            dirty: false,
        }
    }

    pub fn base_lba(&self) -> u32 {
        self.base_lba
    }

    fn flush_cache(&mut self) -> Result<(), StorageError> {
        if self.dirty {
            let lba = self.cache_lba.ok_or(StorageError)?;
            self.io
                .write_block(lba, &self.cache)
                .map_err(|_| StorageError)?;
            self.dirty = false;
        }
        Ok(())
    }

    fn ensure_block(&mut self, lba: u32) -> Result<(), StorageError> {
        if self.cache_lba != Some(lba) {
            self.flush_cache()?; // evict dirty sector first
            let mut buf = [0u8; 512];
            self.io
                .read_block(lba, &mut buf)
                .map_err(|_| StorageError)?;
            self.cache = buf;
            self.cache_lba = Some(lba);
        }
        Ok(())
    }
}

impl<B: BlockIo> IoBase for MscStorage<B> {
    type Error = StorageError;
}

impl<B: BlockIo> Read for MscStorage<B> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        let lba = self.base_lba + (self.pos / 512) as u32;
        let off = (self.pos % 512) as usize;
        self.ensure_block(lba)?;
        let n = (512 - off).min(buf.len());
        buf[..n].copy_from_slice(&self.cache[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<B: BlockIo> Write for MscStorage<B> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        // Loop across sector boundaries within a single call: a write may
        // span more than one cached sector (e.g. a short buffer straddling
        // a 512-byte boundary), and callers here invoke `write()` directly
        // rather than through fatfs's `write_all` retry wrapper.
        let mut written = 0usize;
        while written < buf.len() {
            let lba = self.base_lba + (self.pos / 512) as u32;
            let off = (self.pos % 512) as usize;
            // Don't propagate a mid-loop ensure_block error with `?`: the
            // `Write` contract requires an `Err` return to mean "no bytes
            // written this call". If earlier sectors in this same call
            // already landed in the cache (dirty=true, pos advanced), a
            // later sector's I/O error must not discard that progress —
            // return Ok(written) for what succeeded so far, and only
            // surface Err if the very first sector failed (written == 0).
            if let Err(e) = self.ensure_block(lba) {
                if written > 0 {
                    return Ok(written);
                }
                return Err(e);
            }
            let n = (512 - off).min(buf.len() - written);
            self.cache[off..off + n].copy_from_slice(&buf[written..written + n]);
            self.dirty = true;
            self.pos += n as u64;
            written += n;
        }
        Ok(written)
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.flush_cache()
    }
}

impl<B: BlockIo> Seek for MscStorage<B> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let new_pos: i64 = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.pos as i64 + n,
            SeekFrom::End(_) => return Err(StorageError),
        };
        if new_pos < 0 {
            return Err(StorageError);
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory BlockIo over a byte vec; counts writes for flush assertions.
    /// `fail_read_lba`: if set, the next `read_block` for that LBA errors
    /// once (then clears itself) — used to inject a mid-multi-sector I/O
    /// failure without a larger test-double restructure.
    struct MemDisk {
        data: std::vec::Vec<u8>,
        writes: usize,
        fail_read_lba: Option<u32>,
    }
    impl crate::fat::BlockIo for &mut MemDisk {
        fn read_block(&mut self, lba: u32, buf: &mut [u8; 512]) -> Result<(), ()> {
            if self.fail_read_lba == Some(lba) {
                self.fail_read_lba = None;
                return Err(());
            }
            let o = lba as usize * 512;
            if o + 512 > self.data.len() {
                return Err(());
            }
            buf.copy_from_slice(&self.data[o..o + 512]);
            Ok(())
        }
        fn write_block(&mut self, lba: u32, buf: &[u8; 512]) -> Result<(), ()> {
            let o = lba as usize * 512;
            if o + 512 > self.data.len() {
                return Err(());
            }
            self.data[o..o + 512].copy_from_slice(buf);
            self.writes += 1;
            Ok(())
        }
    }

    #[test]
    fn write_rmw_lands_after_flush() {
        let mut disk = MemDisk {
            data: std::vec![0xAAu8; 8 * 512],
            writes: 0,
            fail_read_lba: None,
        };
        // superfloppy layout (no partition table) -> base_lba 0 fallback is
        // fine: we drive MscStorage directly, not through fatfs here.
        {
            let mut s = MscStorage::new(&mut disk);
            use fatfs::{Seek, SeekFrom, Write};
            s.seek(SeekFrom::Start(512 + 5)).unwrap();
            s.write(&[1, 2, 3]).unwrap();
            s.flush().unwrap();
        }
        assert_eq!(disk.writes, 1);
        assert_eq!(&disk.data[512 + 5..512 + 8], &[1, 2, 3]);
        assert_eq!(disk.data[512 + 4], 0xAA); // RMW preserved neighbors
    }

    #[test]
    fn crossing_sector_boundary_writes_back_dirty_sector() {
        let mut disk = MemDisk {
            data: std::vec![0u8; 8 * 512],
            writes: 0,
            fail_read_lba: None,
        };
        {
            let mut s = MscStorage::new(&mut disk);
            use fatfs::{Seek, SeekFrom, Write};
            s.seek(SeekFrom::Start(510)).unwrap();
            s.write(&[9; 4]).unwrap(); // spans sectors 0 and 1
            s.flush().unwrap();
        }
        assert_eq!(&disk.data[510..514], &[9, 9, 9, 9]);
        assert!(disk.writes >= 2);
    }

    #[test]
    fn read_of_other_sector_evicts_dirty_cache_first() {
        let mut disk = MemDisk {
            data: std::vec![0u8; 8 * 512],
            writes: 0,
            fail_read_lba: None,
        };
        {
            let mut s = MscStorage::new(&mut disk);
            use fatfs::{Read, Seek, SeekFrom, Write};
            s.seek(SeekFrom::Start(0)).unwrap();
            s.write(&[7; 8]).unwrap();
            s.seek(SeekFrom::Start(3 * 512)).unwrap();
            let mut b = [0u8; 4];
            s.read(&mut b).unwrap(); // must not lose sector 0's dirty data
        }
        assert_eq!(&disk.data[0..8], &[7; 8]);
    }

    #[test]
    fn write_spanning_sectors_returns_partial_count_on_second_sector_error() {
        // Write straddles sector 0 and sector 1; sector 1's underlying
        // read_block (via ensure_block) fails once. The call must return
        // Ok(n) for only the first sector's bytes, not propagate Err and
        // discard the already-cached, already-dirty first-sector progress
        // (the bug: the old `ensure_block(lba)?` in the loop threw away
        // `written` on any later-sector failure).
        let mut disk = MemDisk {
            data: std::vec![0u8; 8 * 512],
            writes: 0,
            fail_read_lba: Some(1),
        };
        let n = {
            let mut s = MscStorage::new(&mut disk);
            use fatfs::{Seek, SeekFrom, Write};
            s.seek(SeekFrom::Start(510)).unwrap();
            // 4 bytes: 2 land in sector 0 (offsets 510,511), 2 would land
            // in sector 1 (offsets 0,1) but sector 1's read fails.
            let n = s
                .write(&[9; 4])
                .expect("partial write must succeed, not error out");
            s.flush().unwrap(); // flush only touches the still-cached sector 0
            n
        };
        assert_eq!(n, 2, "only sector 0's 2 bytes were written this call");
        assert_eq!(
            &disk.data[510..512],
            &[9, 9],
            "sector 0's dirty data was not lost/orphaned"
        );
        assert_eq!(disk.writes, 1, "only sector 0 was ever flushed to the disk");
    }
}
