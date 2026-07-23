use heapless::Vec;

pub type WriteList = Vec<(u8, u8), 32>;

pub struct RegDiff {
    shadow: [u8; 32],
}

impl RegDiff {
    pub fn new() -> Self {
        Self { shadow: [0u8; 32] }
    }

    pub fn reset(&mut self) {
        self.shadow = [0u8; 32];
    }

    /// Push (reg,val) for every register that differs from the shadow, then adopt the new image.
    pub fn update(&mut self, img: &[u8; 32], out: &mut WriteList) {
        for reg in 0..32u8 {
            let v = img[reg as usize];
            if v != self.shadow[reg as usize] {
                self.shadow[reg as usize] = v;
                let _ = out.push((reg, v)); // depth 32 == SID_REGS_NUM, never overflows
            }
        }
    }
}

impl Default for RegDiff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn emits_only_changed_registers() {
        let mut d = RegDiff::new(); // shadow all-zero
        let mut out = WriteList::new();
        let mut img = [0u8; 32];
        img[0x04] = 0x11;
        img[0x18] = 0x0f; // two regs changed
        d.update(&img, &mut out);
        assert_eq!(out.as_slice(), &[(0x04, 0x11), (0x18, 0x0f)]);
        out.clear();
        d.update(&img, &mut out); // no change second time
        assert!(out.is_empty());
    }

    #[test]
    fn two_independent_shadows() {
        let mut dl = RegDiff::new();
        let mut dr = RegDiff::new();
        let mut out = WriteList::new();

        let mut img_l = [0u8; 32];
        img_l[0x00] = 0xAA; // L changes reg 0
        let mut img_r = [0u8; 32];
        img_r[0x07] = 0xBB; // R changes reg 7

        dl.update(&img_l, &mut out);
        assert_eq!(out.as_slice(), &[(0x00, 0xAA)]);
        out.clear();

        dr.update(&img_r, &mut out);
        assert_eq!(out.as_slice(), &[(0x07, 0xBB)]); // independent shadow
        out.clear();

        dl.update(&img_l, &mut out); // L unchanged
        assert!(out.is_empty());
    }
}
