//! fatfs storage adapter over a generic 512-byte block device.
//!
//! Ported from top/sid_player_sw/fw/src/fat.rs, with the concrete `UsbMsc`
//! dependency replaced by the `BlockIo` trait so the adapter (and, in M6b,
//! its write-back cache) is host-testable against an in-memory disk.
//! M6a: read-only (writes error, like upstream).

use fatfs::{IoBase, IoError, Read, Seek, SeekFrom, Write};
pub use fatfs::{FileSystem, FsOptions};

/// Minimal 512-byte block device. Implemented by `&usb_msc::UsbMsc` on
/// target and by in-memory disks in host tests.
pub trait BlockIo {
    fn read_block(&mut self, lba: u32, buf: &mut [u8; 512]) -> Result<(), ()>;
}

#[derive(Debug)]
pub struct StorageError;

impl IoError for StorageError {
    fn is_interrupted(&self) -> bool { false }
    fn new_unexpected_eof_error() -> Self { StorageError }
    fn new_write_zero_error() -> Self { StorageError }
}

/// Block-cached storage adapter: presents the first FAT partition as a
/// byte stream starting at its BPB (fatfs mounts at stream offset 0).
pub struct MscStorage<B: BlockIo> {
    io: B,
    pos: u64,
    base_lba: u32,
    cache_lba: Option<u32>,
    cache: [u8; 512],
}

impl<B: BlockIo> MscStorage<B> {
    pub fn new(mut io: B) -> Self {
        let base_lba = crate::partition::first_partition_lba(
            |lba, buf| io.read_block(lba, buf));
        Self { io, pos: 0, base_lba, cache_lba: None, cache: [0u8; 512] }
    }

    pub fn base_lba(&self) -> u32 { self.base_lba }

    fn ensure_block(&mut self, lba: u32) -> Result<(), StorageError> {
        if self.cache_lba != Some(lba) {
            let mut buf = [0u8; 512];
            self.io.read_block(lba, &mut buf).map_err(|_| StorageError)?;
            self.cache = buf;
            self.cache_lba = Some(lba);
        }
        Ok(())
    }
}

impl<B: BlockIo> IoBase for MscStorage<B> { type Error = StorageError; }

impl<B: BlockIo> Read for MscStorage<B> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() { return Ok(0); }
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
    fn write(&mut self, _buf: &[u8]) -> Result<usize, Self::Error> {
        Err(StorageError) // M6a: read-only (M6b un-stubs this)
    }
    fn flush(&mut self) -> Result<(), Self::Error> { Ok(()) }
}

impl<B: BlockIo> Seek for MscStorage<B> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let new_pos: i64 = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.pos as i64 + n,
            SeekFrom::End(_) => return Err(StorageError),
        };
        if new_pos < 0 { return Err(StorageError); }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}
