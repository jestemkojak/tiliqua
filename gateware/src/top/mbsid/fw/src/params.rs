//! Curated Lead-engine patch parameter table (M5_MENU_CARDS_CV_MOD.md §5c).
//!
//! Offsets are the `sid_patch_t` `.L` view (MbSidStructs.h): globals 0x50–0x53,
//! filter[2][6] @ 0x54 (L) / 0x5A (R), voice[6][16] @ 0x60 (v0–2 = Left SID,
//! v3–5 = Right), lfo[6][5] @ 0xC0. OSC rows mirror voice n -> n+3 and filter
//! rows mirror +6 so the L/R SIDs stay identical (factory-Lead invariant;
//! stereo width comes from osc_detune, not divergent voice params).

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Enc {
    /// Sub-byte field: `(byte >> shift) & mask`.
    Byte { shift: u8, mask: u8 },
    /// 12-bit little-endian pair: full low byte + high-byte[3:0] (pulsewidth).
    Wide12,
    /// Filter cutoff: low-byte[6:0] + high-byte[3:0] << 7; low-byte bit 7 is
    /// the FIP interpolation flag and must be preserved.
    Cutoff11,
}

pub struct ParamDesc {
    pub label: &'static str,
    pub addr: u16,
    pub mirror: Option<u16>,
    pub enc: Enc,
    pub max: u16,
    pub step: u16,
}

const fn byte(
    label: &'static str,
    addr: u16,
    mirror: Option<u16>,
    shift: u8,
    mask: u8,
    max: u16,
) -> ParamDesc {
    ParamDesc {
        label,
        addr,
        mirror,
        enc: Enc::Byte { shift, mask },
        max,
        step: 1,
    }
}
const fn osc(label: &'static str, addr: u16, shift: u8, mask: u8, max: u16) -> ParamDesc {
    byte(label, addr, Some(addr + 0x30), shift, mask, max)
}

pub static LEAD_PARAMS: &[ParamDesc] = &[
    byte("Volume", 0x52, None, 0, 0x0F, 15),
    byte("Detune", 0x51, None, 0, 0xFF, 255),
    byte("Phase", 0x53, None, 0, 0xFF, 255),
    ParamDesc {
        label: "Cutoff",
        addr: 0x55,
        mirror: Some(0x5B),
        enc: Enc::Cutoff11,
        max: 2047,
        step: 16,
    },
    byte("Reso", 0x57, Some(0x5D), 4, 0x0F, 15),
    byte("FltMode", 0x54, Some(0x5A), 4, 0x0F, 15),
    byte("FltChn", 0x54, Some(0x5A), 0, 0x0F, 15),
    // OSC1 (voice0 @ 0x60, mirror voice3 @ 0x90)
    osc("O1 Wave", 0x61, 0, 0xFF, 255),
    osc("O1 Atk", 0x62, 4, 0x0F, 15),
    osc("O1 Dec", 0x62, 0, 0x0F, 15),
    osc("O1 Sus", 0x63, 4, 0x0F, 15),
    osc("O1 Rel", 0x63, 0, 0x0F, 15),
    ParamDesc {
        label: "O1 PW",
        addr: 0x64,
        mirror: Some(0x94),
        enc: Enc::Wide12,
        max: 4095,
        step: 16,
    },
    osc("O1 Porta", 0x6B, 0, 0xFF, 255),
    // OSC2 (voice1 @ 0x70, mirror voice4 @ 0xA0)
    osc("O2 Wave", 0x71, 0, 0xFF, 255),
    osc("O2 Atk", 0x72, 4, 0x0F, 15),
    osc("O2 Dec", 0x72, 0, 0x0F, 15),
    osc("O2 Sus", 0x73, 4, 0x0F, 15),
    osc("O2 Rel", 0x73, 0, 0x0F, 15),
    ParamDesc {
        label: "O2 PW",
        addr: 0x74,
        mirror: Some(0xA4),
        enc: Enc::Wide12,
        max: 4095,
        step: 16,
    },
    osc("O2 Porta", 0x7B, 0, 0xFF, 255),
    // OSC3 (voice2 @ 0x80, mirror voice5 @ 0xB0)
    osc("O3 Wave", 0x81, 0, 0xFF, 255),
    osc("O3 Atk", 0x82, 4, 0x0F, 15),
    osc("O3 Dec", 0x82, 0, 0x0F, 15),
    osc("O3 Sus", 0x83, 4, 0x0F, 15),
    osc("O3 Rel", 0x83, 0, 0x0F, 15),
    ParamDesc {
        label: "O3 PW",
        addr: 0x84,
        mirror: Some(0xB4),
        enc: Enc::Wide12,
        max: 4095,
        step: 16,
    },
    osc("O3 Porta", 0x8B, 0, 0xFF, 255),
    // LFO1 @ 0xC0, LFO2 @ 0xC5 (mode,depth,rate,delay,phase)
    byte("L1 Rate", 0xC2, None, 0, 0xFF, 255),
    byte("L1 Depth", 0xC1, None, 0, 0xFF, 255),
    byte("L2 Rate", 0xC7, None, 0, 0xFF, 255),
    byte("L2 Depth", 0xC6, None, 0, 0xFF, 255),
];

pub fn read_value(d: &ParamDesc, body: impl Fn(u16) -> u8) -> u16 {
    match d.enc {
        Enc::Byte { shift, mask } => ((body(d.addr) >> shift) & mask) as u16,
        Enc::Wide12 => (body(d.addr) as u16) | (((body(d.addr + 1) & 0x0F) as u16) << 8),
        Enc::Cutoff11 => ((body(d.addr) & 0x7F) as u16) | (((body(d.addr + 1) & 0x0F) as u16) << 7),
    }
}

/// The (addr, new_byte) writes an edit needs — primary block then mirror.
pub fn write_ops(
    d: &ParamDesc,
    value: u16,
    body: impl Fn(u16) -> u8,
) -> heapless::Vec<(u16, u8), 4> {
    let v = value.min(d.max);
    let mut ops = heapless::Vec::new();
    let mut one = |a: u16| match d.enc {
        Enc::Byte { shift, mask } => {
            let old = body(a);
            let b = (old & !(mask << shift)) | (((v as u8) & mask) << shift);
            let _ = ops.push((a, b));
        }
        Enc::Wide12 => {
            let _ = ops.push((a, (v & 0xFF) as u8));
            let old_h = body(a + 1);
            let _ = ops.push((a + 1, (old_h & 0xF0) | ((v >> 8) as u8 & 0x0F)));
        }
        Enc::Cutoff11 => {
            let old_l = body(a);
            let _ = ops.push((a, (old_l & 0x80) | (v as u8 & 0x7F)));
            let old_h = body(a + 1);
            let _ = ops.push((a + 1, (old_h & 0xF0) | ((v >> 7) as u8 & 0x0F)));
        }
    };
    one(d.addr);
    if let Some(m) = d.mirror {
        one(m);
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake 512-byte patch body for read/write tests.
    fn body_from(pairs: &[(u16, u8)]) -> impl Fn(u16) -> u8 + '_ {
        move |a| {
            pairs
                .iter()
                .find(|(pa, _)| *pa == a)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
    }

    #[test]
    fn table_addresses_inside_lead_regions() {
        for d in LEAD_PARAMS {
            for a in core::iter::once(d.addr).chain(d.mirror) {
                let hi = if matches!(d.enc, Enc::Wide12 | Enc::Cutoff11) {
                    a + 1
                } else {
                    a
                };
                assert!(hi < 512, "{}: addr out of patch", d.label);
                // Lead regions only: globals 0x50..0x54, filter 0x54..0x60,
                // voices 0x60..0xC0, LFOs 0xC0..0xDE.
                assert!(
                    (0x50..0xDE).contains(&a),
                    "{}: {a:#x} outside Lead regions",
                    d.label
                );
            }
            assert!(d.step >= 1 && d.max >= 1, "{}", d.label);
        }
        assert_eq!(LEAD_PARAMS.len(), 32);
    }

    #[test]
    fn osc_rows_mirror_right_sid_voice() {
        // Every voice-region row must mirror addr+0x30 (voice n -> voice n+3).
        for d in LEAD_PARAMS
            .iter()
            .filter(|d| (0x60..0xC0).contains(&d.addr))
        {
            assert_eq!(d.mirror, Some(d.addr + 0x30), "{}", d.label);
        }
        // Filter rows mirror the R block at +6.
        for d in LEAD_PARAMS
            .iter()
            .filter(|d| (0x54..0x60).contains(&d.addr))
        {
            assert_eq!(d.mirror, Some(d.addr + 6), "{}", d.label);
        }
    }

    #[test]
    fn byte_nibble_read_write_roundtrip() {
        let d = ParamDesc {
            label: "T",
            addr: 0x62,
            mirror: None,
            enc: Enc::Byte {
                shift: 4,
                mask: 0x0F,
            },
            max: 15,
            step: 1,
        };
        assert_eq!(read_value(&d, body_from(&[(0x62, 0xA5)])), 0xA);
        // write attack=3 into ad=0xA5: keep decay nibble 5.
        let ops = write_ops(&d, 3, body_from(&[(0x62, 0xA5)]));
        assert_eq!(ops.as_slice(), &[(0x62, 0x35)]);
    }

    #[test]
    fn wide12_write_preserves_high_nibble_and_mirrors() {
        let d = ParamDesc {
            label: "PW1",
            addr: 0x64,
            mirror: Some(0x94),
            enc: Enc::Wide12,
            max: 4095,
            step: 16,
        };
        let b = body_from(&[(0x64, 0x00), (0x65, 0xF0), (0x94, 0x00), (0x95, 0xF0)]);
        assert_eq!(read_value(&d, &b), 0x000); // only [11:0] visible
        let ops = write_ops(&d, 0xABC, &b);
        assert_eq!(
            ops.as_slice(),
            &[(0x64, 0xBC), (0x65, 0xFA), (0x94, 0xBC), (0x95, 0xFA)]
        );
    }

    #[test]
    fn cutoff11_preserves_fip_bit() {
        let d = ParamDesc {
            label: "Cut",
            addr: 0x55,
            mirror: Some(0x5B),
            enc: Enc::Cutoff11,
            max: 2047,
            step: 16,
        };
        // cutoff_l bit 7 = FIP flag, must survive; value = l[6:0] | h[3:0]<<7.
        let b = body_from(&[
            (0x55, 0x80 | 0x7F),
            (0x56, 0x0F),
            (0x5B, 0x80),
            (0x5C, 0x00),
        ]);
        assert_eq!(read_value(&d, &b), 0x7FF);
        let ops = write_ops(&d, 0x155, &b);
        // 0x155 = l7 0x55, h4 0x02; FIP (0x80) kept on both blocks.
        assert_eq!(
            ops.as_slice(),
            &[
                (0x55, 0x80 | 0x55),
                (0x56, 0x02),
                (0x5B, 0x80 | 0x55),
                (0x5C, 0x02)
            ]
        );
    }

    #[test]
    fn clamp_to_max() {
        let d = &LEAD_PARAMS[0]; // Volume, max 15
        let ops = write_ops(d, 999, body_from(&[]));
        assert!(ops.iter().all(|(_, v)| (*v & 0x0F) <= 15));
    }
}
