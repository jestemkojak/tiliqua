//! USB-drive patch-file finder/loader over a mounted FAT volume (M6a §6b).
//!
//! Mirrors sid_player_sw's sid_scan.rs split: generic over ReadWriteSeek so
//! host tests drive the same code against an in-memory FAT image. Files live
//! in `/MBSID/` (preferred) or the root dir (fallback, so hand-copied files
//! Just Work). A patch file is either a standard MBSID v2 single-patch SysEx
//! dump (*.SYX, parsed by SysexCapture::file_mode) or a raw 512-byte
//! sid_patch_t (exact size match).

use fatfs::{Dir, DefaultTimeProvider, FileSystem, LossyOemCpConverter, Read, ReadWriteSeek};
use crate::sysex_capture::SysexCapture;

pub type FileName = heapless::String<16>;
pub const MAX_FILES: usize = 64;
pub type FileList = heapless::Vec<FileName, MAX_FILES>;

/// Upper bound on an accepted file: one 1036-byte single-patch dump with
/// slack for editors that pad; anything bigger is not a single patch.
pub const MAX_FILE_BYTES: usize = 2048;

fn is_syx_name(name: &[u8]) -> bool {
    name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(b".SYX")
}

fn candidate(name: &[u8], len: u64) -> bool {
    (is_syx_name(name) && len as usize <= MAX_FILE_BYTES) || len == 512
}

/// The directory patch files live in: `/MBSID/` if present, else the root.
fn patch_dir<IO: ReadWriteSeek>(
    fs: &FileSystem<IO>,
) -> Dir<'_, IO, DefaultTimeProvider, LossyOemCpConverter> {
    let root = fs.root_dir();
    match root.open_dir("MBSID") {
        Ok(d) => d,
        Err(_) => root,
    }
}

pub fn list_patch_files<IO: ReadWriteSeek>(
    fs: &FileSystem<IO>, out: &mut FileList) -> usize {
    let dir = patch_dir(fs);
    for entry in dir.iter() {
        let Ok(e) = entry else { break };
        if e.is_dir() { continue; }
        let name = e.short_file_name_as_bytes();
        if !candidate(name, e.len()) { continue; }
        let mut s = FileName::new();
        let _ = s.push_str(core::str::from_utf8(name).unwrap_or("?"));
        if out.push(s).is_err() { break; }
    }
    out.len()
}

/// Parse file bytes into a 512-byte sid_patch_t image. Raw 512-byte files
/// are taken verbatim; anything else must contain a valid patch dump.
pub fn parse_patch_file(bytes: &[u8], dst: &mut [u8; 512]) -> bool {
    if bytes.len() == 512 {
        dst.copy_from_slice(bytes);
        return true;
    }
    let mut cap = SysexCapture::file_mode();
    for &b in bytes {
        if cap.feed(b) {
            dst.copy_from_slice(cap.data());
            return true;
        }
    }
    false
}

/// Read the idx-th candidate file and parse it into `dst`.
pub fn load_patch_by_index<IO: ReadWriteSeek>(
    fs: &FileSystem<IO>, idx: usize, dst: &mut [u8; 512]) -> bool {
    let dir = patch_dir(fs);
    let mut count = 0usize;
    for entry in dir.iter() {
        let Ok(e) = entry else { return false };
        if e.is_dir() { continue; }
        let name = e.short_file_name_as_bytes();
        if !candidate(name, e.len()) { continue; }
        if count == idx {
            let mut file = e.to_file();
            let mut buf = [0u8; MAX_FILE_BYTES];
            let mut total = 0usize;
            while total < buf.len() {
                match file.read(&mut buf[total..]) {
                    Ok(0) => break,
                    Ok(n) => total += n,
                    Err(_) => return false,
                }
            }
            return parse_patch_file(&buf[..total], dst);
        }
        count += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use fatfs::{
        format_volume, FormatVolumeOptions, FsOptions, IoBase, IoError, Seek, SeekFrom, Write,
    };
    use std::vec::Vec;

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

    /// Encode a single-patch dump exactly as sysex_capture's tests do.
    fn syx_bytes(patch: &[u8; 512]) -> Vec<u8> {
        const HEADER: [u8; 6] = [0xF0, 0x00, 0x00, 0x7E, 0x4B, 0x00];
        let mut out = Vec::new();
        out.extend_from_slice(&HEADER);
        out.extend_from_slice(&[0x02, 0x00, 0x01, 0x00]);
        let mut sum: u32 = 0;
        for &d in patch.iter() {
            let (lo, hi) = (d & 0x0F, (d >> 4) & 0x0F);
            out.push(lo); out.push(hi);
            sum += (lo + hi) as u32;
        }
        out.push(((sum as i32).wrapping_neg() & 0x7F) as u8);
        out.push(0xF7);
        out
    }

    fn test_patch(seed: u8) -> [u8; 512] {
        let mut p = [0u8; 512];
        for (i, b) in p.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31).wrapping_add(seed);
        }
        p
    }

    #[test]
    fn lists_syx_and_raw512_skips_others() {
        let p = test_patch(1);
        let raw = test_patch(2);
        let mut img = build_gpt_fat_image(&[
            ("LEAD1.SYX", &syx_bytes(&p)),
            ("README.TXT", b"not a patch"),
            ("RAW.BIN", &raw),          // exactly 512 bytes -> candidate
            ("BIG.SYX", &[0u8; 4096]),  // too big -> skipped
        ]);
        let base = BASE_LBA as usize * SECTOR;
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut out = FileList::new();
        assert_eq!(list_patch_files(&fs, &mut out), 2);
        assert_eq!(out[0].as_str(), "LEAD1.SYX");
        assert_eq!(out[1].as_str(), "RAW.BIN");
    }

    #[test]
    fn loads_syx_by_index_and_parses_body() {
        let p = test_patch(3);
        let mut img = build_gpt_fat_image(&[("A.SYX", &syx_bytes(&p))]);
        let base = BASE_LBA as usize * SECTOR;
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut dst = [0u8; 512];
        assert!(load_patch_by_index(&fs, 0, &mut dst));
        assert_eq!(dst, p);
    }

    #[test]
    fn loads_raw_512_verbatim() {
        let p = test_patch(4);
        let mut img = build_gpt_fat_image(&[("R.BIN", &p)]);
        let base = BASE_LBA as usize * SECTOR;
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut dst = [0u8; 512];
        assert!(load_patch_by_index(&fs, 0, &mut dst));
        assert_eq!(dst, p);
    }

    #[test]
    fn mbsid_dir_preferred_over_root() {
        // Create /MBSID/IN.SYX plus a root ROOT.SYX; the /MBSID one must win.
        let p_dir = test_patch(5);
        let mut img = build_gpt_fat_image(&[("ROOT.SYX", &syx_bytes(&test_patch(6)))]);
        {
            let base = BASE_LBA as usize * SECTOR;
            let part = VecDisk::new(&mut img[base..]);
            let fs = FileSystem::new(part, FsOptions::new()).unwrap();
            let d = fs.root_dir().create_dir("MBSID").unwrap();
            let mut f = d.create_file("IN.SYX").unwrap();
            write_all(&mut f, &syx_bytes(&p_dir));
            f.flush().unwrap();
        }
        let base = BASE_LBA as usize * SECTOR;
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut out = FileList::new();
        assert_eq!(list_patch_files(&fs, &mut out), 1);
        assert_eq!(out[0].as_str(), "IN.SYX");
        let mut dst = [0u8; 512];
        assert!(load_patch_by_index(&fs, 0, &mut dst));
        assert_eq!(dst, p_dir);
    }

    #[test]
    fn corrupt_syx_load_fails() {
        let mut bytes = syx_bytes(&test_patch(7));
        let n = bytes.len();
        bytes[n - 2] = (bytes[n - 2] + 1) & 0x7F; // break checksum
        let mut img = build_gpt_fat_image(&[("BAD.SYX", &bytes)]);
        let base = BASE_LBA as usize * SECTOR;
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut dst = [0u8; 512];
        assert!(!load_patch_by_index(&fs, 0, &mut dst));
    }
}
