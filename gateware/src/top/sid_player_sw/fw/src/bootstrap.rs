//! 6502 bootstrap stub + vector byte emitter (host-testable).

/// Fixed 6502 addresses for the player scaffold (in PSRAM region).
pub const INIT_STUB_ADDR: u16 = 0xFF00;
pub const NMI_STUB_ADDR:  u16 = 0xFF20;

/// Build the init stub: LDA #subtune_0based; JSR init; JMP *  (spin).
pub fn init_stub(subtune_0based: u8, init_addr: u16) -> [u8; 8] {
    let [il, ih] = init_addr.to_le_bytes();
    let [sl, sh] = INIT_STUB_ADDR.to_le_bytes();
    // 0xFF00: A9 nn      LDA #subtune
    // 0xFF02: 20 ll hh   JSR init
    // 0xFF05: 4C 05 FF   JMP $FF05 (spin)
    [0xA9, subtune_0based, 0x20, il, ih, 0x4C, sl.wrapping_add(5), sh]
}

/// Build the NMI play stub: JSR play; RTI.
pub fn nmi_stub(play_addr: u16) -> [u8; 4] {
    let [pl, ph] = play_addr.to_le_bytes();
    // 0xFF20: 20 ll hh  JSR play
    // 0xFF23: 40        RTI
    [0x20, pl, ph, 0x40]
}

/// The 6 vector bytes for $FFFA..$FFFF: NMI, RESET, IRQ.
pub fn vectors() -> [u8; 6] {
    let [nl, nh] = NMI_STUB_ADDR.to_le_bytes();
    let [rl, rh] = INIT_STUB_ADDR.to_le_bytes();
    // NMI -> NMI_STUB, RESET -> INIT_STUB, IRQ -> NMI_STUB
    [nl, nh, rl, rh, nl, nh]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_stub_encodes_subtune_and_jsr() {
        let s = init_stub(1, 0x1000);
        assert_eq!(s[0], 0xA9);            // LDA #
        assert_eq!(s[1], 1);               // subtune 0-based
        assert_eq!(s[2], 0x20);            // JSR
        assert_eq!([s[3], s[4]], [0x00, 0x10]);  // init $1000 little-endian
        assert_eq!(s[5], 0x4C);            // JMP
    }

    #[test]
    fn nmi_stub_calls_play_then_rti() {
        let s = nmi_stub(0x1003);
        assert_eq!(s, [0x20, 0x03, 0x10, 0x40]);
    }

    #[test]
    fn vectors_point_at_stubs() {
        let v = vectors();
        assert_eq!([v[2], v[3]], INIT_STUB_ADDR.to_le_bytes()); // RESET
        assert_eq!([v[0], v[1]], NMI_STUB_ADDR.to_le_bytes());  // NMI
    }
}
