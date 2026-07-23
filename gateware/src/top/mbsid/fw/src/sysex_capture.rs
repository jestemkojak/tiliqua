//! Byte-at-a-time parser for MBSID SysEx *Patch Write* dumps.
//!
//! Mirrors the framing of the vendored engine's MbSidSysEx::cmdPatchWrite
//! (never edit that file — this is an independent Rust reimplementation used
//! ONLY to capture Bank Writes for flash persistence; the engine still parses
//! the same bytes itself and applies RAM Writes live).
//!
//! Accept-and-capture condition (M4_USER_PATCH_BANKS.md §6b): cmd 0x02,
//! type == 0x00 (Bank Write, sid 0), bank == 1 (User; bank 0 = factory ROM,
//! read-only), 1024 nibblized data bytes, checksum (-sum & 0x7F), terminated
//! by F7. Everything else is skipped silently.

const HEADER: [u8; 6] = [0xF0, 0x00, 0x00, 0x7E, 0x4B, 0x00];
const CMD_PATCH_WRITE: u8 = 0x02;
const TYPE_BANK_WRITE_SID0: u8 = 0x00;
const USER_BANK: u8 = 0x01;
const DATA_NIBBLES: u16 = 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Not in any message we care about. `hdr_ix` counts matched header bytes.
    Idle,
    Cmd,
    Type,
    Bank,
    Patch,
    Data,
    Checksum,
    /// Everything matched & checksum OK — waiting for the terminating F7.
    Term,
    /// Inside a SysEx message that is not for us; wait for it to end.
    Skip,
}

pub struct SysexCapture {
    state: State,
    hdr_ix: u8,
    buf: [u8; 512],
    data_ix: u16,  // nibble-byte counter, 0..=1024
    checksum: u8,  // running 7-bit sum (mirrors engine's u8 sysexChecksum)
    lnibble: bool, // true = low nibble already stored for buf[data_ix/2]
    bank: u8,
    patch: u8,
    file_mode: bool,
}

impl SysexCapture {
    pub fn new() -> Self {
        Self::with_mode(false)
    }

    /// File-import mode (M6 spec §6c): accept ANY cmd-0x02 patch dump —
    /// type/bank/patch bytes are ignored (a file explicitly chosen by the
    /// user carries its own intent) — while still enforcing header, nibble
    /// count, checksum, and the F7 terminator. The live MIDI path keeps
    /// `new()`'s strict Bank-Write/bank-1 rule.
    pub fn file_mode() -> Self {
        Self::with_mode(true)
    }

    fn with_mode(file_mode: bool) -> Self {
        Self {
            state: State::Idle,
            hdr_ix: 0,
            buf: [0u8; 512],
            data_ix: 0,
            checksum: 0,
            lnibble: false,
            bank: 0,
            patch: 0,
            file_mode,
        }
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.hdr_ix = 0;
    }

    pub fn slot(&self) -> u8 {
        self.patch & 0x7F
    }
    pub fn data(&self) -> &[u8; 512] {
        &self.buf
    }

    pub fn in_message(&self) -> bool {
        !(self.state == State::Idle && self.hdr_ix == 0)
    }

    /// Feed one raw SysEx-stream byte. Returns true exactly once per complete,
    /// valid Bank Write (type 0x00, bank 1). Never allocates, never panics.
    pub fn feed(&mut self, b: u8) -> bool {
        // Realtime bytes are transparent everywhere (MIDI spec).
        if b >= 0xF8 {
            return false;
        }

        // A status byte always (re)frames the stream.
        if b == 0xF0 {
            self.reset();
            self.hdr_ix = 1; // matched HEADER[0]
            return false;
        }
        if b >= 0x80 {
            // F7 or an interrupting status byte.
            let done = b == 0xF7 && self.state == State::Term;
            let (bank, complete) = (self.bank, done);
            self.reset();
            return complete && (self.file_mode || bank == USER_BANK);
        }

        // Data byte (< 0x80).
        match self.state {
            State::Idle => {
                if self.hdr_ix == 0 {
                    // Not inside any message — stray data byte, ignore.
                } else if b == HEADER[self.hdr_ix as usize] {
                    self.hdr_ix += 1;
                    if self.hdr_ix as usize == HEADER.len() {
                        self.state = State::Cmd;
                    }
                } else {
                    self.state = State::Skip;
                }
            }
            State::Cmd => {
                self.state = if b == CMD_PATCH_WRITE {
                    State::Type
                } else {
                    State::Skip
                };
            }
            State::Type => {
                // Strict: only Bank Write to sid 0. File mode: any type — the
                // body framing (bank+patch+1024 nibbles) is identical.
                if b == TYPE_BANK_WRITE_SID0 || self.file_mode {
                    self.state = State::Bank;
                    self.data_ix = 0;
                    self.checksum = 0;
                    self.lnibble = false;
                } else {
                    self.state = State::Skip;
                }
            }
            State::Bank => {
                self.bank = b;
                // Wrong bank could skip now, but running the full state machine
                // keeps in_message()/framing behavior uniform; the bank check
                // happens once at the F7.
                self.state = State::Patch;
            }
            State::Patch => {
                self.patch = b;
                self.state = State::Data;
            }
            State::Data => {
                self.checksum = self.checksum.wrapping_add(b);
                let byte_ix = (self.data_ix / 2) as usize;
                if !self.lnibble {
                    self.buf[byte_ix] = b & 0x0F;
                    self.lnibble = true;
                } else {
                    self.buf[byte_ix] |= (b & 0x0F) << 4;
                    self.lnibble = false;
                }
                self.data_ix += 1;
                if self.data_ix == DATA_NIBBLES {
                    self.state = State::Checksum;
                }
            }
            State::Checksum => {
                let expect = (self.checksum as i32).wrapping_neg() as u8 & 0x7F;
                self.state = if b == expect {
                    State::Term
                } else {
                    State::Skip
                };
            }
            State::Term => {
                // Extra data byte after checksum = malformed.
                self.state = State::Skip;
            }
            State::Skip => {}
        }
        false
    }
}

impl Default for SysexCapture {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a Patch Write dump exactly as MBSID tools do. 1036 bytes total.
    fn encode(ptype: u8, bank: u8, patch: u8, data: &[u8; 512]) -> [u8; 1036] {
        let mut out = [0u8; 1036];
        let mut k = 0;
        for &h in &HEADER {
            out[k] = h;
            k += 1;
        }
        out[k] = CMD_PATCH_WRITE;
        k += 1;
        out[k] = ptype;
        k += 1;
        out[k] = bank;
        k += 1;
        out[k] = patch;
        k += 1;
        let mut sum: u32 = 0;
        for &d in data.iter() {
            let lo = d & 0x0F;
            let hi = (d >> 4) & 0x0F;
            out[k] = lo;
            k += 1;
            out[k] = hi;
            k += 1;
            sum += (lo + hi) as u32;
        }
        out[k] = ((sum as i32).wrapping_neg() & 0x7F) as u8;
        k += 1;
        out[k] = 0xF7;
        k += 1;
        assert_eq!(k, 1036);
        out
    }

    fn test_patch() -> [u8; 512] {
        let mut p = [0u8; 512];
        for (i, b) in p.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37);
        }
        p[..4].copy_from_slice(b"NAME");
        p
    }

    fn feed_all(cap: &mut SysexCapture, bytes: &[u8]) -> usize {
        bytes.iter().filter(|&&b| cap.feed(b)).count()
    }

    #[test]
    fn bank_write_bank1_is_captured_exactly_once() {
        let p = test_patch();
        let msg = encode(0x00, 0x01, 42, &p);
        let mut cap = SysexCapture::new();
        assert_eq!(feed_all(&mut cap, &msg), 1);
        assert_eq!(cap.slot(), 42);
        assert_eq!(cap.data(), &p);
        assert!(!cap.in_message());
    }

    #[test]
    fn bank0_factory_is_read_only_never_captured() {
        let msg = encode(0x00, 0x00, 3, &test_patch());
        let mut cap = SysexCapture::new();
        assert_eq!(feed_all(&mut cap, &msg), 0);
    }

    #[test]
    fn bank_ge_2_ignored() {
        let msg = encode(0x00, 0x02, 3, &test_patch());
        let mut cap = SysexCapture::new();
        assert_eq!(feed_all(&mut cap, &msg), 0);
    }

    #[test]
    fn ram_write_is_audition_only_never_captured() {
        // type 0x08 = RAM Write, sid 0 (user decision: never auto-persist).
        let msg = encode(0x08, 0x01, 3, &test_patch());
        let mut cap = SysexCapture::new();
        assert_eq!(feed_all(&mut cap, &msg), 0);
    }

    #[test]
    fn nonzero_sid_ignored() {
        // type 0x01 = Bank Write sid 1 — we present exactly one logical MBSID.
        let msg = encode(0x01, 0x01, 3, &test_patch());
        let mut cap = SysexCapture::new();
        assert_eq!(feed_all(&mut cap, &msg), 0);
    }

    #[test]
    fn wrong_checksum_rejected() {
        let mut msg = encode(0x00, 0x01, 7, &test_patch());
        msg[1034] = (msg[1034] + 1) & 0x7F; // corrupt the checksum byte (layout: data ends at 1033)
        let mut cap = SysexCapture::new();
        assert_eq!(feed_all(&mut cap, &msg), 0);
        assert!(!cap.in_message()); // F7 closed the message
    }

    #[test]
    fn truncated_by_early_f7_rejected() {
        // A synthetic F7 (gateware-framed interrupted message) mid-data.
        let msg = encode(0x00, 0x01, 7, &test_patch());
        let mut cap = SysexCapture::new();
        assert_eq!(feed_all(&mut cap, &msg[..500]), 0);
        assert!(cap.in_message());
        assert!(!cap.feed(0xF7)); // early terminator = wrong length = reject
        assert!(!cap.in_message());
    }

    #[test]
    fn foreign_sysex_ignored_and_next_message_still_captured() {
        let mut cap = SysexCapture::new();
        // Some other manufacturer's message.
        for b in [0xF0u8, 0x43, 0x10, 0x4C, 0x00, 0x00, 0x7E, 0x00, 0xF7] {
            assert!(!cap.feed(b));
        }
        let p = test_patch();
        assert_eq!(feed_all(&mut cap, &encode(0x00, 0x01, 9, &p)), 1);
        assert_eq!(cap.slot(), 9);
    }

    #[test]
    fn restart_on_f0_mid_message() {
        let p = test_patch();
        let good = encode(0x00, 0x01, 5, &p);
        let mut cap = SysexCapture::new();
        // Half a message, then a fresh F0 restarts parsing from scratch.
        feed_all(&mut cap, &good[..300]);
        assert_eq!(feed_all(&mut cap, &good), 1);
        assert_eq!(cap.slot(), 5);
    }

    #[test]
    fn back_to_back_dumps_both_captured() {
        let p = test_patch();
        let mut cap = SysexCapture::new();
        assert_eq!(feed_all(&mut cap, &encode(0x00, 0x01, 0, &p)), 1);
        assert_eq!(cap.slot(), 0);
        assert_eq!(feed_all(&mut cap, &encode(0x00, 0x01, 127, &p)), 1);
        assert_eq!(cap.slot(), 127);
    }

    #[test]
    fn realtime_bytes_are_transparent() {
        // 0xF8..0xFF may be injected anywhere per the MIDI spec; the engine's
        // parser ignores them and so must we (defense in depth — the gateware
        // RT filter already strips them).
        let p = test_patch();
        let msg = encode(0x00, 0x01, 11, &p);
        let mut cap = SysexCapture::new();
        let mut captured = 0;
        for (i, &b) in msg.iter().enumerate() {
            if i == 100 {
                assert!(!cap.feed(0xF8));
            }
            if cap.feed(b) {
                captured += 1;
            }
        }
        assert_eq!(captured, 1);
        assert_eq!(cap.slot(), 11);
    }

    #[test]
    fn file_mode_accepts_factory_bank0_dump() {
        let p = test_patch();
        let msg = encode(0x00, 0x00, 3, &p); // bank 0: strict mode rejects this
        let mut cap = SysexCapture::file_mode();
        assert_eq!(feed_all(&mut cap, &msg), 1);
        assert_eq!(cap.data(), &p);
    }

    #[test]
    fn file_mode_accepts_ram_write_type() {
        let p = test_patch();
        let msg = encode(0x08, 0x00, 0, &p); // RAM Write framing, same body
        let mut cap = SysexCapture::file_mode();
        assert_eq!(feed_all(&mut cap, &msg), 1);
        assert_eq!(cap.data(), &p);
    }

    #[test]
    fn file_mode_still_rejects_bad_checksum() {
        let mut msg = encode(0x00, 0x00, 7, &test_patch());
        msg[1034] = (msg[1034] + 1) & 0x7F;
        let mut cap = SysexCapture::file_mode();
        assert_eq!(feed_all(&mut cap, &msg), 0);
    }

    #[test]
    fn strict_mode_unchanged_by_file_mode_addition() {
        let msg = encode(0x00, 0x00, 3, &test_patch());
        let mut cap = SysexCapture::new();
        assert_eq!(feed_all(&mut cap, &msg), 0); // bank 0 still read-only live
    }
}
