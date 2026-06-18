//! PSID v1/v2 header parser (no_std, host-testable).

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PsidHeader {
    pub version: u16,
    pub data_offset: u16,
    pub load_addr: u16,
    pub init_addr: u16,
    pub play_addr: u16,
    pub songs: u16,
    pub start_song: u16,
    pub speed: u32,
    /// v2 `flags` field (offset $76); 0 for v1 (no such field).
    pub flags: u16,
}

#[derive(Debug, PartialEq)]
pub enum PsidError { TooShort, BadMagic, UnsupportedVersion, BadOffsets }

/// Video standard the tune was composed for (PSID `flags` bits 2-3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Clock { Pal, Ntsc }

/// SID chip model the tune declares for the first SID (PSID `flags` bits 4-5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SidModel { Unknown, Mos6581, Mos8580, Both }

impl SidModel {
    /// Short label for the UI metadata line.
    pub fn as_str(&self) -> &'static str {
        match self {
            SidModel::Mos6581 => "6581",
            SidModel::Mos8580 => "8580",
            SidModel::Both    => "6581/8580",
            SidModel::Unknown => "SID?",
        }
    }
}

/// C64 system (φ2) clock in Hz for each video standard.
const PHI2_PAL:  u64 = 985_248;
const PHI2_NTSC: u64 = 1_022_727;

/// φ2 cycles per video frame (VBlank period) for each standard.
/// PAL ≈ 50.12 Hz, NTSC ≈ 59.83 Hz.
const FRAME_CYCLES_PAL:  u64 = 19_656;
const FRAME_CYCLES_NTSC: u64 = 17_095;

/// Default CIA Timer A value when a CIA-timed tune leaves it unprogrammed
/// (≈ 60 Hz on PAL φ2, per the PSID convention).
const CIA_DEFAULT_TIMER: u16 = 0x4025;

/// Compute the `PlayTimerPeripheral` divider (in `clk_hz` sync cycles) that
/// reproduces the tune's intended play-call rate.
///
/// - VBlank tunes (`cia == false`): one call per video frame (50/60 Hz).
/// - CIA tunes (`cia == true`): rate = φ2 / (timer + 1), honouring multispeed.
///   `cia_timer == 0` means INIT never programmed it → fall back to the ~60 Hz
///   default.
pub fn play_period_cycles(clk_hz: u32, clock: Clock, cia: bool, cia_timer: u16) -> u32 {
    let phi2 = match clock { Clock::Ntsc => PHI2_NTSC, Clock::Pal => PHI2_PAL };
    let clk = clk_hz as u64;
    let period = if cia {
        let timer = if cia_timer == 0 { CIA_DEFAULT_TIMER } else { cia_timer } as u64;
        // rate = phi2 / (timer + 1)  →  period = clk / rate.
        clk * (timer + 1) / phi2
    } else {
        let frame = match clock { Clock::Ntsc => FRAME_CYCLES_NTSC, Clock::Pal => FRAME_CYCLES_PAL };
        // rate = phi2 / frame  →  period = clk / rate.
        clk * frame / phi2
    };
    period as u32
}

impl PsidHeader {
    pub fn parse(bytes: &[u8]) -> Result<PsidHeader, PsidError> {
        if bytes.len() < 0x76 { return Err(PsidError::TooShort); }
        if &bytes[0..4] != b"PSID" { return Err(PsidError::BadMagic); }
        let be16 = |o: usize| u16::from_be_bytes([bytes[o], bytes[o+1]]);
        let be32 = |o: usize| u32::from_be_bytes(
            [bytes[o], bytes[o+1], bytes[o+2], bytes[o+3]]);
        let version = be16(0x04);
        if version != 1 && version != 2 { return Err(PsidError::UnsupportedVersion); }
        Ok(PsidHeader {
            version,
            data_offset: be16(0x06),
            load_addr: be16(0x08),
            init_addr: be16(0x0A),
            play_addr: be16(0x0C),
            songs: be16(0x0E),
            start_song: be16(0x10),
            speed: be32(0x12),
            // `flags` only exists in v2 headers (offset $76); v1 has none.
            flags: if version >= 2 { be16(0x76) } else { 0 },
        })
    }

    /// Video standard the tune targets (PSID `flags` bits 2-3):
    /// 01 = PAL, 10 = NTSC; 00 (unknown) and 11 (both) default to PAL.
    pub fn clock(&self) -> Clock {
        match (self.flags >> 2) & 0b11 {
            0b10 => Clock::Ntsc,
            _    => Clock::Pal,
        }
    }

    /// SID chip model declared for the first SID (PSID `flags` bits 4-5):
    /// 00 = unknown, 01 = 6581, 10 = 8580, 11 = both. v1 tunes (flags == 0)
    /// report `Unknown`.
    pub fn model(&self) -> SidModel {
        match (self.flags >> 4) & 0b11 {
            0b01 => SidModel::Mos6581,
            0b10 => SidModel::Mos8580,
            0b11 => SidModel::Both,
            _    => SidModel::Unknown,
        }
    }

    /// True if subtune `song_1based` is CIA-timed (PSID `speed` bit set);
    /// false means VBlank timing. CIA tunes are often multispeed.
    pub fn is_cia(&self, song_1based: u16) -> bool {
        let i = song_1based.saturating_sub(1).min(31);
        (self.speed >> i) & 1 == 1
    }

    /// Effective load address: header field, or first 2 bytes of payload if 0.
    pub fn effective_load_addr(&self, payload: &[u8]) -> u16 {
        if self.load_addr != 0 { self.load_addr }
        else { u16::from_le_bytes([payload[0], payload[1]]) }
    }

    /// Resolve the tune payload's load address + byte slice, validating the
    /// untrusted `data_offset`/`load_addr` fields against the in-buffer length
    /// `len` and the target image size `mem_len`. Never indexes out of bounds.
    pub fn resolve_payload<'a>(&self, tune: &'a [u8], len: usize, mem_len: usize)
        -> Result<(usize, &'a [u8]), PsidError>
    {
        let off = self.data_offset as usize;
        if len > tune.len() || off > len { return Err(PsidError::BadOffsets); }
        let payload_raw = &tune[off..len];
        if self.load_addr == 0 && payload_raw.len() < 2 {
            return Err(PsidError::BadOffsets);
        }
        let load_addr = self.effective_load_addr(payload_raw) as usize;
        let payload = if self.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
        if load_addr + payload.len() > mem_len { return Err(PsidError::BadOffsets); }
        Ok((load_addr, payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn be16(v: u16) -> [u8;2] { v.to_be_bytes() }

    fn make_header() -> [u8; 0x7C] {
        let mut h = [0u8; 0x7C];
        h[0..4].copy_from_slice(b"PSID");
        h[0x04..0x06].copy_from_slice(&be16(2));
        h[0x06..0x08].copy_from_slice(&be16(0x7C));
        h[0x08..0x0A].copy_from_slice(&be16(0x1000));
        h[0x0A..0x0C].copy_from_slice(&be16(0x1000));
        h[0x0C..0x0E].copy_from_slice(&be16(0x1003));
        h[0x0E..0x10].copy_from_slice(&be16(3));
        h[0x10..0x12].copy_from_slice(&be16(1));
        h[0x12..0x16].copy_from_slice(&2u32.to_be_bytes()); // song 2 = 60Hz
        h
    }

    #[test]
    fn parses_v2_header() {
        let h = PsidHeader::parse(&make_header()).unwrap();
        assert_eq!(h.version, 2);
        assert_eq!(h.init_addr, 0x1000);
        assert_eq!(h.play_addr, 0x1003);
        assert_eq!(h.songs, 3);
        assert_eq!(h.start_song, 1);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut h = make_header(); h[0] = b'X';
        assert_eq!(PsidHeader::parse(&h), Err(PsidError::BadMagic));
    }

    #[test]
    fn parses_flags_field() {
        let mut h = make_header();
        h[0x76..0x78].copy_from_slice(&be16(0x08)); // clock=NTSC (10 << 2)
        assert_eq!(PsidHeader::parse(&h).unwrap().flags, 0x08);
    }

    #[test]
    fn clock_from_flags() {
        let mut h = make_header();
        h[0x76..0x78].copy_from_slice(&be16(0x04)); // 01 << 2 = PAL
        assert_eq!(PsidHeader::parse(&h).unwrap().clock(), Clock::Pal);
        h[0x76..0x78].copy_from_slice(&be16(0x08)); // 10 << 2 = NTSC
        assert_eq!(PsidHeader::parse(&h).unwrap().clock(), Clock::Ntsc);
    }

    #[test]
    fn model_from_flags() {
        let mut h = make_header();
        h[0x76..0x78].copy_from_slice(&be16(0x10)); // 01 << 4 = 6581
        assert_eq!(PsidHeader::parse(&h).unwrap().model(), SidModel::Mos6581);
        h[0x76..0x78].copy_from_slice(&be16(0x20)); // 10 << 4 = 8580
        assert_eq!(PsidHeader::parse(&h).unwrap().model(), SidModel::Mos8580);
        h[0x76..0x78].copy_from_slice(&be16(0x30)); // 11 << 4 = both
        assert_eq!(PsidHeader::parse(&h).unwrap().model(), SidModel::Both);
        h[0x76..0x78].copy_from_slice(&be16(0x00)); // 00 = unknown
        assert_eq!(PsidHeader::parse(&h).unwrap().model(), SidModel::Unknown);
    }

    #[test]
    fn clock_defaults_to_pal_when_unknown_or_both() {
        let mut h = make_header();
        h[0x76..0x78].copy_from_slice(&be16(0x00)); // 00 = unknown
        assert_eq!(PsidHeader::parse(&h).unwrap().clock(), Clock::Pal);
        h[0x76..0x78].copy_from_slice(&be16(0x0C)); // 11 = both
        assert_eq!(PsidHeader::parse(&h).unwrap().clock(), Clock::Pal);
    }

    #[test]
    fn is_cia_reads_speed_bit() {
        // make_header sets speed=2 → bit0=0 (VBI), bit1=1 (CIA).
        let h = PsidHeader::parse(&make_header()).unwrap();
        assert!(!h.is_cia(1));
        assert!(h.is_cia(2));
    }

    fn rate(period: u32) -> u32 { 60_000_000 / period }

    #[test]
    fn vbi_pal_rate_is_50hz() {
        let p = play_period_cycles(60_000_000, Clock::Pal, false, 0);
        assert!((49..=51).contains(&rate(p)), "got {} Hz", rate(p));
    }

    #[test]
    fn vbi_ntsc_rate_is_60hz() {
        let p = play_period_cycles(60_000_000, Clock::Ntsc, false, 0);
        assert!((59..=61).contains(&rate(p)), "got {} Hz", rate(p));
    }

    #[test]
    fn cia_unprogrammed_timer_defaults_to_60hz() {
        let p = play_period_cycles(60_000_000, Clock::Pal, true, 0);
        assert!((59..=61).contains(&rate(p)), "got {} Hz", rate(p));
    }

    #[test]
    fn cia_multispeed_timer_gives_150hz() {
        // PAL φ2 = 985248; rate = φ2/(T+1); 150 Hz → T+1 ≈ 6568 → T = 6567.
        let p = play_period_cycles(60_000_000, Clock::Pal, true, 6567);
        assert!((148..=152).contains(&rate(p)), "got {} Hz", rate(p));
    }

    #[test]
    fn load_addr_from_payload_when_zero() {
        let mut hb = make_header();
        hb[0x08..0x0A].copy_from_slice(&be16(0));
        let h = PsidHeader::parse(&hb).unwrap();
        assert_eq!(h.effective_load_addr(&[0x00, 0x20]), 0x2000);
    }

    // --- resolve_payload tests ---

    #[test]
    fn resolve_payload_rejects_data_offset_beyond_len() {
        // data_offset (0x7C) > len (0x7B) → BadOffsets
        let h = PsidHeader::parse(&make_header()).unwrap(); // data_offset = 0x7C
        let tune = make_header();
        let mem_len = 0x10000;
        assert_eq!(h.resolve_payload(&tune, 0x7B, mem_len), Err(PsidError::BadOffsets));
    }

    #[test]
    fn resolve_payload_rejects_tiny_payload_when_load_addr_zero() {
        // load_addr == 0, payload_raw.len() < 2 → BadOffsets
        let mut hb = make_header();
        hb[0x08..0x0A].copy_from_slice(&be16(0)); // load_addr = 0
        // data_offset = 0x7C; len = 0x7D → payload_raw.len() = 1 < 2
        let h = PsidHeader::parse(&hb).unwrap();
        let mut tune = [0u8; 0x7D];
        tune[..0x7C].copy_from_slice(&hb[..0x7C]);
        tune[0x7C] = 0xAA; // only 1 byte of payload
        let mem_len = 0x10000;
        assert_eq!(h.resolve_payload(&tune, 0x7D, mem_len), Err(PsidError::BadOffsets));
    }

    #[test]
    fn resolve_payload_rejects_payload_exceeding_mem() {
        // load_addr = 0xFF00, large payload → load_addr + payload.len() > 0x10000
        let mut hb = make_header();
        hb[0x08..0x0A].copy_from_slice(&be16(0xFF00)); // explicit load_addr
        let h = PsidHeader::parse(&hb).unwrap();
        // data_offset = 0x7C; make tune large enough: 0x7C + 0x200 = 0x27C bytes
        let tune_len = 0x7C + 0x200;
        let mut tune = vec![0u8; tune_len];
        tune[..0x7C].copy_from_slice(&hb[..0x7C]);
        let mem_len = 0x10000;
        // load_addr (0xFF00) + payload.len (0x200) = 0x10100 > 0x10000
        assert_eq!(h.resolve_payload(&tune, tune_len, mem_len), Err(PsidError::BadOffsets));
    }

    #[test]
    fn resolve_payload_ok_explicit_load_addr() {
        // load_addr = 0x1000 (explicit, from header), payload at data_offset..len
        let h = PsidHeader::parse(&make_header()).unwrap();
        // data_offset = 0x7C; make a tune of 0x7C + 4 bytes
        let tune_len = 0x7C + 4;
        let mut tune = [0u8; 0x80];
        tune[..0x7C].copy_from_slice(&make_header());
        tune[0x7C] = 0x01; tune[0x7D] = 0x02; tune[0x7E] = 0x03; tune[0x7F] = 0x04;
        let mem_len = 0x10000;
        let result = h.resolve_payload(&tune, tune_len, mem_len);
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let (load_addr, payload) = result.unwrap();
        assert_eq!(load_addr, 0x1000);
        assert_eq!(payload, &[0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn resolve_payload_ok_embedded_load_addr() {
        // load_addr == 0 → first 2 bytes of payload_raw give the address
        let mut hb = make_header();
        hb[0x08..0x0A].copy_from_slice(&be16(0)); // load_addr = 0
        let h = PsidHeader::parse(&hb).unwrap();
        // data_offset = 0x7C; payload_raw = [0x00, 0x20, 0xAA, 0xBB]
        let tune_len = 0x7C + 4;
        let mut tune = [0u8; 0x80];
        tune[..0x7C].copy_from_slice(&hb[..0x7C]);
        tune[0x7C] = 0x00; tune[0x7D] = 0x20; // LE load addr = 0x2000
        tune[0x7E] = 0xAA; tune[0x7F] = 0xBB;
        let mem_len = 0x10000;
        let result = h.resolve_payload(&tune, tune_len, mem_len);
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let (load_addr, payload) = result.unwrap();
        assert_eq!(load_addr, 0x2000);
        assert_eq!(payload, &[0xAA, 0xBB]); // leading 2 bytes stripped
    }
}
