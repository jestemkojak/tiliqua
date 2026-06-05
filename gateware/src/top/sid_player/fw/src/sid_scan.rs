//! Storage-generic ".SID file finder" over a mounted FAT volume.
//!
//! Split out of `fat.rs` (which is tied to the USB-MSC hardware) so the actual
//! directory-scan + extension-match + read logic is exercised by host tests
//! against an in-memory disk image — the same code the firmware runs.

use fatfs::{FileSystem, Read, ReadWriteSeek};

/// Scan the root directory of `fs` for the first `*.SID` file and read it into
/// `dst`. Returns the number of bytes read, or `None` if no `.SID` file exists
/// (or a read error occurs mid-scan).
///
/// Uses `short_file_name_as_bytes()` (no `alloc` needed): FAT 8.3 short names
/// are uppercase with a dot, e.g. `b"0.SID"` or the LFN alias `b"GYROSC~1.SID"`.
pub fn find_first_sid<IO: ReadWriteSeek>(fs: &FileSystem<IO>, dst: &mut [u8]) -> Option<usize> {
    let root = fs.root_dir();
    for entry_result in root.iter() {
        let entry = entry_result.ok()?;
        if entry.is_dir() {
            continue;
        }
        let name_bytes = entry.short_file_name_as_bytes();
        log::info!(
            "fat: file '{}'",
            core::str::from_utf8(name_bytes).unwrap_or("<non-utf8>")
        );
        let is_sid = name_bytes.len() >= 4 && {
            let ext = &name_bytes[name_bytes.len() - 4..];
            ext.eq_ignore_ascii_case(b".SID")
        };
        if !is_sid {
            continue;
        }
        let mut file = entry.to_file();
        let mut total = 0usize;
        while total < dst.len() {
            let n = file.read(&mut dst[total..]).ok()?;
            if n == 0 {
                break;
            }
            total += n;
        }
        return Some(total);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::partition;
    use fatfs::{
        format_volume, FormatVolumeOptions, FsOptions, IoBase, IoError, Seek, SeekFrom, Write,
    };

    const SECTOR: usize = 512;
    const BASE_LBA: u32 = 34; // matches the user's GPT stick

    // --- A minimal in-memory fatfs storage over a mutable byte slice ---------

    #[derive(Debug)]
    struct DiskErr;
    impl IoError for DiskErr {
        fn is_interrupted(&self) -> bool {
            false
        }
        fn new_unexpected_eof_error() -> Self {
            DiskErr
        }
        fn new_write_zero_error() -> Self {
            DiskErr
        }
    }

    struct VecDisk<'a> {
        data: &'a mut [u8],
        pos: usize,
    }
    impl<'a> VecDisk<'a> {
        fn new(data: &'a mut [u8]) -> Self {
            Self { data, pos: 0 }
        }
    }
    impl<'a> IoBase for VecDisk<'a> {
        type Error = DiskErr;
    }
    impl<'a> Read for VecDisk<'a> {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, DiskErr> {
            let n = (self.data.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }
    impl<'a> Write for VecDisk<'a> {
        fn write(&mut self, buf: &[u8]) -> Result<usize, DiskErr> {
            let n = (self.data.len() - self.pos).min(buf.len());
            self.data[self.pos..self.pos + n].copy_from_slice(&buf[..n]);
            self.pos += n;
            Ok(n)
        }
        fn flush(&mut self) -> Result<(), DiskErr> {
            Ok(())
        }
    }
    impl<'a> Seek for VecDisk<'a> {
        fn seek(&mut self, pos: SeekFrom) -> Result<u64, DiskErr> {
            let p: i64 = match pos {
                SeekFrom::Start(n) => n as i64,
                SeekFrom::Current(n) => self.pos as i64 + n,
                SeekFrom::End(n) => self.data.len() as i64 + n,
            };
            if p < 0 || p as usize > self.data.len() {
                return Err(DiskErr);
            }
            self.pos = p as usize;
            Ok(self.pos as u64)
        }
    }

    fn write_all<W>(file: &mut W, mut bytes: &[u8])
    where
        W: Write,
        W::Error: core::fmt::Debug,
    {
        while !bytes.is_empty() {
            let n = file.write(bytes).unwrap();
            bytes = &bytes[n..];
        }
    }

    /// Build a GPT-partitioned disk image whose single FAT volume starts at
    /// `BASE_LBA` and contains the given `(name, contents)` files.
    fn build_gpt_fat_image(files: &[(&str, &[u8])]) -> Vec<u8> {
        // 1. Format a standalone FAT region and populate it.
        let fat_bytes = 4 * 1024 * 1024; // small enough for FAT12/16, plenty of room
        let mut fat_region = vec![0u8; fat_bytes];
        {
            let mut disk = VecDisk::new(&mut fat_region);
            format_volume(&mut disk, FormatVolumeOptions::new()).unwrap();
        }
        {
            let disk = VecDisk::new(&mut fat_region);
            let fs = FileSystem::new(disk, FsOptions::new()).unwrap();
            let root = fs.root_dir();
            for (name, contents) in files {
                let mut f = root.create_file(name).unwrap();
                write_all(&mut f, contents);
                f.flush().unwrap();
            }
        }

        // 2. Embed it at BASE_LBA and prepend a GPT (protective MBR + header +
        //    entry array) so first_partition_lba() must follow the real layout.
        let base = BASE_LBA as usize * SECTOR;
        let mut img = vec![0u8; base + fat_bytes];
        img[base..].copy_from_slice(&fat_region);

        // LBA 0: protective MBR — boot signature + partition type 0xEE.
        img[510] = 0x55;
        img[511] = 0xAA;
        img[446 + 4] = 0xEE;
        // LBA 1: GPT header — "EFI PART", partition entry array at LBA 2.
        img[512..520].copy_from_slice(b"EFI PART");
        img[512 + 72..512 + 76].copy_from_slice(&2u32.to_le_bytes());
        // LBA 2: first entry — non-zero type GUID, starting LBA = BASE_LBA.
        img[1024..1040].copy_from_slice(&[0x11u8; 16]);
        img[1024 + 32..1024 + 36].copy_from_slice(&BASE_LBA.to_le_bytes());
        img
    }

    #[test]
    fn finds_first_sid_through_gpt_partition_offset() {
        let sid0: &[u8] = b"PSID\x00\x02 first tune payload bytes";
        let music: &[u8] = b"PSID\x00\x02 second tune";
        let mut img = build_gpt_fat_image(&[
            ("0.SID", sid0),
            ("READ.ME", b"not a tune"),
            ("MUSIC.SID", music),
        ]);

        // The partition parser must locate the FAT volume at LBA 34.
        let base = partition::first_partition_lba(|lba, buf| {
            let o = lba as usize * SECTOR;
            buf.copy_from_slice(&img[o..o + SECTOR]);
            Ok(())
        });
        assert_eq!(base, BASE_LBA);

        // Mounting fatfs at that offset and scanning must return 0.SID's bytes.
        let part = VecDisk::new(&mut img[base as usize * SECTOR..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut dst = vec![0u8; 4096];
        let n = find_first_sid(&fs, &mut dst).expect("a .SID file should be found");
        assert_eq!(&dst[..n], sid0);
    }

    #[test]
    fn returns_none_when_no_sid_present() {
        let mut img = build_gpt_fat_image(&[("READ.ME", b"x"), ("DATA.BIN", b"y")]);
        let base = BASE_LBA as usize * SECTOR;
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut dst = vec![0u8; 4096];
        assert!(find_first_sid(&fs, &mut dst).is_none());
    }

    #[test]
    fn reading_lba0_directly_would_fail_to_mount() {
        // Regression guard for the original bug: mounting at LBA 0 (the
        // protective MBR) instead of the partition offset must NOT succeed.
        let img = build_gpt_fat_image(&[("0.SID", b"PSID payload")]);
        let mut lba0_view = img.clone();
        let part = VecDisk::new(&mut lba0_view); // starts at byte 0 = protective MBR
        assert!(
            FileSystem::new(part, FsOptions::new()).is_err(),
            "mounting at LBA 0 must fail — this is the bug we fixed"
        );
    }
}
