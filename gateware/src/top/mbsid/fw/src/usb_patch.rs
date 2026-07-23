//! USB-drive patch-file finder/loader over a mounted FAT volume (M6a §6b).
//!
//! Mirrors sid_player_sw's sid_scan.rs split: generic over ReadWriteSeek so
//! host tests drive the same code against an in-memory FAT image. Files live
//! in `/MBSID/` (preferred) or the root dir (fallback, so hand-copied files
//! Just Work). A patch file is either a standard MBSID v2 single-patch SysEx
//! dump (*.SYX, parsed by SysexCapture::file_mode) or a raw 512-byte
//! sid_patch_t (exact size match).

use crate::sysex_capture::SysexCapture;
use fatfs::{
    DefaultTimeProvider, Dir, File, FileSystem, LossyOemCpConverter, Read, ReadWriteSeek, Write,
};

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

pub fn list_patch_files<IO: ReadWriteSeek>(fs: &FileSystem<IO>, out: &mut FileList) -> usize {
    let dir = patch_dir(fs);
    for entry in dir.iter() {
        let Ok(e) = entry else { break };
        if e.is_dir() {
            continue;
        }
        let name = e.short_file_name_as_bytes();
        if !candidate(name, e.len()) {
            continue;
        }
        let mut s = FileName::new();
        let _ = s.push_str(core::str::from_utf8(name).unwrap_or("?"));
        if out.push(s).is_err() {
            break;
        }
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
    fs: &FileSystem<IO>,
    idx: usize,
    dst: &mut [u8; 512],
) -> bool {
    let dir = patch_dir(fs);
    let mut count = 0usize;
    for entry in dir.iter() {
        let Ok(e) = entry else { return false };
        if e.is_dir() {
            continue;
        }
        let name = e.short_file_name_as_bytes();
        if !candidate(name, e.len()) {
            continue;
        }
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

/// Encode `patch` as a standard MBSID v2 single-patch dump (bank 1 = User,
/// so the exported file re-sent over MIDI lands in the user bank).
pub fn encode_syx(patch: &[u8; 512], slot: u8, out: &mut [u8; 1036]) {
    const HEADER: [u8; 6] = [0xF0, 0x00, 0x00, 0x7E, 0x4B, 0x00];
    out[..6].copy_from_slice(&HEADER);
    out[6] = 0x02; // cmd: Patch Write
    out[7] = 0x00; // type: Bank Write, sid 0
    out[8] = 0x01; // bank: User
    out[9] = slot & 0x7F;
    let mut sum: u32 = 0;
    for (i, &d) in patch.iter().enumerate() {
        let (lo, hi) = (d & 0x0F, (d >> 4) & 0x0F);
        out[10 + 2 * i] = lo;
        out[11 + 2 * i] = hi;
        sum += (lo + hi) as u32;
    }
    out[1034] = ((sum as i32).wrapping_neg() & 0x7F) as u8;
    out[1035] = 0xF7;
}

/// Export `patch` as `/MBSID/<name>`; flush; verify by readback+reparse.
pub fn export_patch<IO: ReadWriteSeek>(
    fs: &FileSystem<IO>,
    name: &str,
    patch: &[u8; 512],
    slot: u8,
) -> bool {
    let root = fs.root_dir();
    let dir = match root.open_dir("MBSID") {
        Ok(d) => d,
        Err(_) => match root.create_dir("MBSID") {
            Ok(d) => d,
            Err(_) => root, // e.g. read-only quirk: fall back to root
        },
    };
    let mut syx = [0u8; 1036];
    encode_syx(patch, slot, &mut syx);
    {
        let Ok(mut f) = dir.create_file(name) else {
            return false;
        };
        if f.truncate().is_err() {
            return false;
        }
        let mut rest: &[u8] = &syx;
        while !rest.is_empty() {
            match f.write(rest) {
                Ok(0) | Err(_) => return false,
                Ok(n) => rest = &rest[n..],
            }
        }
        if f.flush().is_err() {
            return false;
        }
    }
    // Verify: re-open, re-read, re-parse, byte-compare (cheap end-to-end
    // check that the write path actually landed — spec §6b).
    let Ok(mut f) = dir.open_file(name) else {
        return false;
    };
    let mut back = [0u8; 1036];
    let mut total = 0usize;
    while total < back.len() {
        match f.read(&mut back[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => return false,
        }
    }
    let mut dst = [0u8; 512];
    total == 1036 && parse_patch_file(&back, &mut dst) && dst == *patch
}

/// Fixed bank-import source file (spec §1): /MBSID/BANK.SYX, root fallback.
pub const BANK_FILE: &str = "BANK.SYX";

/// Pass-1 result: how many valid messages, and which slots they target.
pub struct BankSummary {
    pub count: u8,
    pub slots: [u8; 16], // bitmap, bit n = slot n present
}

impl BankSummary {
    pub fn has(&self, slot: u8) -> bool {
        self.slots[(slot >> 3) as usize] & (1 << (slot & 7)) != 0
    }
}

fn open_bank_file<IO: ReadWriteSeek>(
    fs: &FileSystem<IO>,
) -> Option<File<'_, IO, DefaultTimeProvider, LossyOemCpConverter>> {
    let root = fs.root_dir();
    if let Ok(d) = root.open_dir("MBSID") {
        if let Ok(f) = d.open_file(BANK_FILE) {
            return Some(f);
        }
    }
    root.open_file(BANK_FILE).ok()
}

/// Stream BANK.SYX through SysexCapture::file_mode in 1 KB chunks, calling
/// `on_patch(slot, body)` per valid complete message. Returns
/// `(valid_messages, f0_message_starts)`; the two are equal iff every
/// F0-framed message in the file parsed valid and complete — SysexCapture
/// silently skips invalid messages, so callers MUST compare them (spec §1:
/// abort on any bad message, never shrink the bank). None on: file missing,
/// read error, >128 messages, EOF mid-message, zero messages, or the
/// callback returning false.
fn stream_bank<IO: ReadWriteSeek>(
    fs: &FileSystem<IO>,
    mut on_patch: impl FnMut(u8, &[u8; 512]) -> bool,
) -> Option<(u8, u16)> {
    let mut file = open_bank_file(fs)?;
    let mut cap = SysexCapture::file_mode();
    let (mut count, mut starts) = (0u16, 0u16);
    let mut buf = [0u8; 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == 0xF0 {
                starts += 1;
            }
            if cap.feed(b) {
                count += 1;
                if count > 128 {
                    return None;
                }
                if !on_patch(cap.slot(), cap.data()) {
                    return None;
                }
            }
        }
    }
    if cap.in_message() || count == 0 {
        return None;
    }
    Some((count as u8, starts))
}

/// Pass 1: validate the whole bank file without touching anything. None on
/// any structural failure, bad checksum, duplicate slot, or truncation.
pub fn validate_bank<IO: ReadWriteSeek>(fs: &FileSystem<IO>) -> Option<BankSummary> {
    let mut slots = [0u8; 16];
    let (count, starts) = stream_bank(fs, |slot, _| {
        let (ix, bit) = ((slot >> 3) as usize, 1u8 << (slot & 7));
        if slots[ix] & bit != 0 {
            return false;
        } // duplicate slot
        slots[ix] |= bit;
        true
    })?;
    if starts != count as u16 {
        return None;
    } // an invalid message was skipped
    Some(BankSummary { count, slots })
}

/// Pass 2: re-stream, handing each (slot, body) to `f`. Checksums are
/// inherently re-verified (same parser), so a drive returning different
/// bytes on the second read fails here instead of importing garbage.
pub fn for_each_bank_patch<IO: ReadWriteSeek>(
    fs: &FileSystem<IO>,
    f: impl FnMut(u8, &[u8; 512]) -> bool,
) -> bool {
    matches!(stream_bank(fs, f), Some((count, starts)) if starts == count as u16)
}

/// FAT-image test scaffolding shared by usb_patch and bank_import tests.
#[cfg(test)]
pub(crate) mod testfs {
    use fatfs::{
        format_volume, FileSystem, FormatVolumeOptions, FsOptions, IoBase, IoError, Read, Seek,
        SeekFrom, Write,
    };
    use std::vec::Vec;

    pub const SECTOR: usize = 512;
    pub const BASE_LBA: u32 = 34; // matches the user's GPT stick

    // --- A minimal in-memory fatfs storage over a mutable byte slice ---------

    #[derive(Debug)]
    pub struct DiskErr;
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

    pub struct VecDisk<'a> {
        data: &'a mut [u8],
        pos: usize,
    }
    impl<'a> VecDisk<'a> {
        pub fn new(data: &'a mut [u8]) -> Self {
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

    pub fn write_all<W>(file: &mut W, mut bytes: &[u8])
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
    pub fn build_gpt_fat_image(files: &[(&str, &[u8])]) -> Vec<u8> {
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
    pub fn syx_bytes(patch: &[u8; 512]) -> Vec<u8> {
        const HEADER: [u8; 6] = [0xF0, 0x00, 0x00, 0x7E, 0x4B, 0x00];
        let mut out = Vec::new();
        out.extend_from_slice(&HEADER);
        out.extend_from_slice(&[0x02, 0x00, 0x01, 0x00]);
        let mut sum: u32 = 0;
        for &d in patch.iter() {
            let (lo, hi) = (d & 0x0F, (d >> 4) & 0x0F);
            out.push(lo);
            out.push(hi);
            sum += (lo + hi) as u32;
        }
        out.push(((sum as i32).wrapping_neg() & 0x7F) as u8);
        out.push(0xF7);
        out
    }

    pub fn test_patch(seed: u8) -> [u8; 512] {
        let mut p = [0u8; 512];
        for (i, b) in p.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31).wrapping_add(seed);
        }
        p
    }

    /// Concatenate MBSID Bank Write dumps into one bank-file byte stream.
    pub fn bank_bytes(patches: &[(u8, [u8; 512])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (slot, p) in patches {
            let mut m = [0u8; 1036];
            crate::usb_patch::encode_syx(p, *slot, &mut m);
            out.extend_from_slice(&m);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testfs::*;
    use super::*;
    use fatfs::FsOptions;
    use std::vec::Vec;

    #[test]
    fn lists_syx_and_raw512_skips_others() {
        let p = test_patch(1);
        let raw = test_patch(2);
        let mut img = build_gpt_fat_image(&[
            ("LEAD1.SYX", &syx_bytes(&p)),
            ("README.TXT", b"not a patch"),
            ("RAW.BIN", &raw),         // exactly 512 bytes -> candidate
            ("BIG.SYX", &[0u8; 4096]), // too big -> skipped
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

    #[test]
    fn encode_syx_roundtrips_through_file_parser() {
        let p = test_patch(9);
        let mut syx = [0u8; 1036];
        encode_syx(&p, 5, &mut syx);
        let mut dst = [0u8; 512];
        assert!(parse_patch_file(&syx, &mut dst));
        assert_eq!(dst, p);
        assert_eq!(syx[8], 0x01); // bank byte 1 = User (re-sendable over MIDI)
        assert_eq!(syx[9], 5); // slot
    }

    #[test]
    fn export_then_reimport_is_byte_identical() {
        let p = test_patch(10);
        let mut img = build_gpt_fat_image(&[("SEED.TXT", b"x")]);
        let base = BASE_LBA as usize * SECTOR;
        {
            let part = VecDisk::new(&mut img[base..]);
            let fs = FileSystem::new(part, FsOptions::new()).unwrap();
            assert!(export_patch(&fs, "P007.SYX", &p, 7));
        }
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut out = FileList::new();
        assert_eq!(list_patch_files(&fs, &mut out), 1); // in /MBSID/, .SYX
        assert_eq!(out[0].as_str(), "P007.SYX");
        let mut dst = [0u8; 512];
        assert!(load_patch_by_index(&fs, 0, &mut dst));
        assert_eq!(dst, p);
    }

    #[test]
    fn export_overwrites_existing_file() {
        let (p1, p2) = (test_patch(11), test_patch(12));
        let mut img = build_gpt_fat_image(&[("SEED.TXT", b"x")]);
        let base = BASE_LBA as usize * SECTOR;
        for p in [&p1, &p2] {
            let part = VecDisk::new(&mut img[base..]);
            let fs = FileSystem::new(part, FsOptions::new()).unwrap();
            assert!(export_patch(&fs, "EDIT.SYX", p, 0));
        }
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut dst = [0u8; 512];
        assert!(load_patch_by_index(&fs, 0, &mut dst));
        assert_eq!(dst, p2); // second export won, file not duplicated
    }

    fn fs_with_root_file<'a>(img: &'a mut Vec<u8>) -> FileSystem<VecDisk<'a>> {
        let base = BASE_LBA as usize * SECTOR;
        FileSystem::new(VecDisk::new(&mut img[base..]), FsOptions::new()).unwrap()
    }

    #[test]
    fn validate_full_128_bank() {
        let patches: Vec<(u8, [u8; 512])> =
            (0..128).map(|i| (i as u8, test_patch(i as u8))).collect();
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bank_bytes(&patches))]);
        let fs = fs_with_root_file(&mut img);
        let sum = validate_bank(&fs).expect("full bank must validate");
        assert_eq!(sum.count, 128);
        assert!((0..128).all(|s| sum.has(s)));
    }

    #[test]
    fn validate_sparse_bank_sets_only_named_slots() {
        let patches = vec![(3u8, test_patch(3)), (7u8, test_patch(7))];
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bank_bytes(&patches))]);
        let fs = fs_with_root_file(&mut img);
        let sum = validate_bank(&fs).unwrap();
        assert_eq!(sum.count, 2);
        assert!(sum.has(3) && sum.has(7));
        assert!(!sum.has(0) && !sum.has(4) && !sum.has(127));
    }

    #[test]
    fn validate_accepts_bank_byte_zero() {
        // Real-world banks (e.g. bank1__v2_vintage_bank.syx) address bank 0.
        let mut bytes = bank_bytes(&[(5u8, test_patch(5))]);
        assert_eq!(bytes[8], 0x01);
        bytes[8] = 0x00; // encode_syx writes bank 1; patch it to Factory
                         // fix the checksum? No — bank/patch bytes are OUTSIDE the checksummed
                         // 1024-nibble body (checksum covers data nibbles only), so no fixup.
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bytes)]);
        let fs = fs_with_root_file(&mut img);
        let sum = validate_bank(&fs).expect("bank byte must be ignored");
        assert_eq!(sum.count, 1);
        assert!(sum.has(5));
    }

    #[test]
    fn validate_rejects_bad_checksum() {
        let mut bytes = bank_bytes(&[(0u8, test_patch(1)), (1u8, test_patch(2))]);
        bytes[1034] = (bytes[1034] + 1) & 0x7F; // corrupt first message's checksum
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bytes)]);
        let fs = fs_with_root_file(&mut img);
        assert!(
            validate_bank(&fs).is_none(),
            "a skipped-invalid message must reject the file, not shrink it"
        );
    }

    #[test]
    fn validate_rejects_duplicate_slot() {
        let bytes = bank_bytes(&[(9u8, test_patch(1)), (9u8, test_patch(2))]);
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bytes)]);
        let fs = fs_with_root_file(&mut img);
        assert!(validate_bank(&fs).is_none());
    }

    #[test]
    fn validate_rejects_truncated_tail() {
        let mut bytes = bank_bytes(&[(0u8, test_patch(1)), (1u8, test_patch(2))]);
        bytes.truncate(bytes.len() - 100); // EOF mid-message
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bytes)]);
        let fs = fs_with_root_file(&mut img);
        assert!(validate_bank(&fs).is_none());
    }

    #[test]
    fn validate_rejects_missing_or_empty() {
        let mut img = build_gpt_fat_image(&[("OTHER.TXT", b"x")]);
        let fs = fs_with_root_file(&mut img);
        assert!(validate_bank(&fs).is_none(), "missing BANK.SYX");
        drop(fs);
        let mut img = build_gpt_fat_image(&[("BANK.SYX", b"")]);
        let fs = fs_with_root_file(&mut img);
        assert!(validate_bank(&fs).is_none(), "zero messages");
    }

    #[test]
    fn mbsid_dir_bank_preferred_over_root() {
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bank_bytes(&[(0u8, test_patch(1))]))]);
        {
            let base = BASE_LBA as usize * SECTOR;
            let part = VecDisk::new(&mut img[base..]);
            let fs = FileSystem::new(part, FsOptions::new()).unwrap();
            let d = fs.root_dir().create_dir("MBSID").unwrap();
            let mut f = d.create_file("BANK.SYX").unwrap();
            write_all(&mut f, &bank_bytes(&[(42u8, test_patch(2))]));
            f.flush().unwrap();
        }
        let fs = fs_with_root_file(&mut img);
        let sum = validate_bank(&fs).unwrap();
        assert!(sum.has(42) && !sum.has(0), "/MBSID/BANK.SYX must win");
    }

    #[test]
    fn for_each_yields_slots_and_bodies() {
        let patches = vec![(2u8, test_patch(20)), (5u8, test_patch(50))];
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bank_bytes(&patches))]);
        let fs = fs_with_root_file(&mut img);
        let mut got: Vec<(u8, [u8; 512])> = Vec::new();
        assert!(for_each_bank_patch(&fs, |slot, body| {
            got.push((slot, *body));
            true
        }));
        assert_eq!(got, patches);
    }

    #[test]
    fn for_each_callback_false_aborts() {
        let patches = vec![(0u8, test_patch(1)), (1u8, test_patch(2))];
        let mut img = build_gpt_fat_image(&[("BANK.SYX", &bank_bytes(&patches))]);
        let fs = fs_with_root_file(&mut img);
        let mut calls = 0;
        assert!(!for_each_bank_patch(&fs, |_, _| {
            calls += 1;
            false
        }));
        assert_eq!(calls, 1);
    }
}
