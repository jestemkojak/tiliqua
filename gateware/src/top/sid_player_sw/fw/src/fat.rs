//! fatfs storage adapter over the USB-MSC block reader, and tune loader.
//!
//! Uses fatfs 0.4 (git) in no_std mode.
//! Only reading is supported; writes return an error.

use crate::usb_msc::UsbMsc;
use fatfs::{IoBase, IoError, Read, Seek, SeekFrom, Write};

// Re-export fatfs FileSystem so callers don't need to depend on fatfs directly.
pub use fatfs::{FileSystem, FsOptions};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Opaque I/O error for the MSC storage adapter.
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

// ---------------------------------------------------------------------------
// MscStorage — wraps UsbMsc as a fatfs ReadWriteSeek stream
// ---------------------------------------------------------------------------

/// A block-cached read-only storage adapter over `UsbMsc`.
pub struct MscStorage<'a> {
    msc: &'a UsbMsc,
    /// Current byte position within the FAT *volume* (partition-relative).
    pos: u64,
    /// Block size in bytes (from the MSC device).
    block_size: u32,
    /// LBA of the FAT volume's first sector (0 for an unpartitioned drive).
    base_lba: u32,
    /// LBA currently held in `cache`, or None if cache is cold.
    cache_lba: Option<u32>,
    /// Single-block read cache.
    cache: [u8; 512],
}

impl<'a> MscStorage<'a> {
    pub fn new(msc: &'a UsbMsc) -> Self {
        let block_size = msc.block_size() as u32;
        let block_size = if block_size == 0 { 512 } else { block_size };
        // The FAT BPB lives at the start of the partition, not at LBA 0 on a
        // partitioned (MBR/GPT) stick. Parse the partition table to find it.
        let base_lba = crate::partition::first_partition_lba(|lba, buf| {
            msc.read_block(lba, buf).map_err(|_| ())
        });
        Self {
            msc,
            pos: 0,
            block_size,
            base_lba,
            cache_lba: None,
            cache: [0u8; 512],
        }
    }

    /// The partition's starting LBA, for diagnostics.
    pub fn base_lba(&self) -> u32 {
        self.base_lba
    }

    /// Ensure `cache` holds the block at `lba`.
    fn ensure_block(&mut self, lba: u32) -> Result<(), StorageError> {
        if self.cache_lba != Some(lba) {
            let mut buf = [0u8; 512];
            self.msc
                .read_block(lba, &mut buf)
                .map_err(|_| StorageError)?;
            self.cache = buf;
            self.cache_lba = Some(lba);
        }
        Ok(())
    }
}

impl<'a> IoBase for MscStorage<'a> {
    type Error = StorageError;
}

impl<'a> Read for MscStorage<'a> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        // Cache and read_block are always 512-byte — use that regardless of
        // self.block_size (which may differ on exotic drives but read_block
        // always transfers exactly 512 bytes).
        let lba = self.base_lba + (self.pos / 512) as u32;
        let offset_in_block = (self.pos % 512) as usize;
        self.ensure_block(lba)?;
        let available = 512 - offset_in_block;
        let n = available.min(buf.len());
        buf[..n].copy_from_slice(&self.cache[offset_in_block..offset_in_block + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<'a> Write for MscStorage<'a> {
    fn write(&mut self, _buf: &[u8]) -> Result<usize, Self::Error> {
        // Read-only adapter — writes are not supported.
        Err(StorageError)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'a> Seek for MscStorage<'a> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let new_pos: i64 = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.pos as i64 + n,
            SeekFrom::End(_) => {
                // We don't know the disk size without reading all blocks;
                // fatfs does not call SeekFrom::End during normal read traversal.
                return Err(StorageError);
            }
        };
        if new_pos < 0 {
            return Err(StorageError);
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

// ---------------------------------------------------------------------------
// SID file loading and listing helpers
// ---------------------------------------------------------------------------

/// Mount the USB FAT volume and read the `idx`-th root `*.SID` into `dst`.
pub fn load_sid(msc: &UsbMsc, idx: usize, dst: &mut [u8]) -> Result<usize, StorageError> {
    let storage = MscStorage::new(msc);
    log::debug!("fat: partition base_lba={}", storage.base_lba());
    let fs = match FileSystem::new(storage, FsOptions::new()) {
        Ok(fs) => fs,
        Err(_) => {
            log::info!("fat: FileSystem::new failed (bad BPB at base_lba?)");
            return Err(StorageError);
        }
    };
    crate::sid_scan::load_sid_by_index(&fs, idx, dst).ok_or(StorageError)
}

/// Mount the USB FAT volume and enumerate root `*.SID` short names into `out`.
/// Returns the number found (0 on mount failure).
pub fn list_sids(msc: &UsbMsc, out: &mut crate::sid_scan::SidList) -> usize {
    let storage = MscStorage::new(msc);
    let fs = match FileSystem::new(storage, FsOptions::new()) {
        Ok(fs) => fs,
        Err(_) => {
            log::info!("fat: list_sids mount failed");
            return 0;
        }
    };
    crate::sid_scan::list_root_sids(&fs, out)
}

/// Back-compat shim: load the first root `*.SID`.
pub fn load_first_sid(msc: &UsbMsc, dst: &mut [u8]) -> Result<usize, StorageError> {
    load_sid(msc, 0, dst)
}
