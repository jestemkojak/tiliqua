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
}

#[derive(Debug, PartialEq)]
pub enum PsidError { TooShort, BadMagic, UnsupportedVersion }

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
        })
    }

    /// True if subtune `song_1based` plays at 60 Hz (NTSC).
    pub fn is_ntsc(&self, song_1based: u16) -> bool {
        let i = song_1based.saturating_sub(1).min(31);
        (self.speed >> i) & 1 == 1
    }

    /// Effective load address: header field, or first 2 bytes of payload if 0.
    pub fn effective_load_addr(&self, payload: &[u8]) -> u16 {
        if self.load_addr != 0 { self.load_addr }
        else { u16::from_le_bytes([payload[0], payload[1]]) }
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
    fn ntsc_speed_bit() {
        let h = PsidHeader::parse(&make_header()).unwrap();
        assert!(!h.is_ntsc(1));   // bit0 = 0
        assert!(h.is_ntsc(2));    // bit1 = 1
    }

    #[test]
    fn load_addr_from_payload_when_zero() {
        let mut hb = make_header();
        hb[0x08..0x0A].copy_from_slice(&be16(0));
        let h = PsidHeader::parse(&hb).unwrap();
        assert_eq!(h.effective_load_addr(&[0x00, 0x20]), 0x2000);
    }
}
