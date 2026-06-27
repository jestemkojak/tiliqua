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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn emits_only_changed_registers() {
        let mut d = RegDiff::new();          // shadow all-zero
        let mut out = WriteList::new();
        let mut img = [0u8; 32];
        img[0x04] = 0x11; img[0x18] = 0x0f;  // two regs changed
        d.update(&img, &mut out);
        assert_eq!(out.as_slice(), &[(0x04, 0x11), (0x18, 0x0f)]);
        out.clear();
        d.update(&img, &mut out);            // no change second time
        assert!(out.is_empty());
    }
}
