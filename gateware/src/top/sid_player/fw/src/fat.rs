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
    /// Current byte position within the virtual disk image.
    pos: u64,
    /// LBA currently held in `cache`, or None if cache is cold.
    cache_lba: Option<u32>,
    /// Single-block read cache.
    cache: [u8; 512],
}

impl<'a> MscStorage<'a> {
    pub fn new(msc: &'a UsbMsc) -> Self {
        Self {
            msc,
            pos: 0,
            cache_lba: None,
            cache: [0u8; 512],
        }
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
        let lba = (self.pos / 512) as u32;
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
// load_first_sid — find the first *.SID file in the FAT root dir
// ---------------------------------------------------------------------------

/// Find the first `*.SID` file in the root directory of the FAT32 volume and
/// read its contents into `dst`.  Returns the number of bytes read.
///
/// Uses `short_file_name_as_bytes()` (always available without `alloc`) to
/// detect `.SID` extension; the short name is stored uppercase in FAT 8.3
/// format with a dot separator, e.g. `b"MUSIC.SID"`.
pub fn load_first_sid(msc: &UsbMsc, dst: &mut [u8]) -> Result<usize, StorageError> {
    let storage = MscStorage::new(msc);
    let fs = FileSystem::new(storage, FsOptions::new()).map_err(|_| StorageError)?;
    let root = fs.root_dir();

    for entry_result in root.iter() {
        let entry = entry_result.map_err(|_| StorageError)?;
        if entry.is_dir() {
            continue;
        }
        let name_bytes = entry.short_file_name_as_bytes();
        // short_file_name_as_bytes returns e.g. b"MUSIC.SID" (uppercase, with dot).
        // Check for ".SID" suffix, case-insensitive (FAT short names are uppercase
        // so a plain suffix match suffices, but eq_ignore_ascii_case is defensive).
        let is_sid = name_bytes.len() >= 4 && {
            let ext = &name_bytes[name_bytes.len() - 4..];
            ext.eq_ignore_ascii_case(b".SID")
        };
        if !is_sid {
            continue;
        }
        // Found — read the file.
        let mut file = entry.to_file();
        let mut total = 0usize;
        loop {
            if total >= dst.len() {
                break;
            }
            let n = file.read(&mut dst[total..]).map_err(|_| StorageError)?;
            if n == 0 {
                break;
            }
            total += n;
        }
        return Ok(total);
    }

    Err(StorageError) // no .SID file found
}
