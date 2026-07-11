//! USBMSCPeripheral CSR driver: block reads from a USB mass-storage device.
use tiliqua_pac as pac;

pub struct UsbMsc { regs: pac::USB_MSC }

#[derive(Debug)]
pub enum MscError { NotReady, ReadError }

impl UsbMsc {
    pub fn new(regs: pac::USB_MSC) -> Self { Self { regs } }

    pub fn ready(&self) -> bool {
        self.regs.status().read().ready().bit_is_set()
    }

    pub fn connected(&self) -> bool {
        self.regs.status().read().connected().bit_is_set()
    }

    /// Mirror of the menu's USB Mode row: 1 = MSC owns the PHY (Storage).
    pub fn set_mode(&self, storage: bool) {
        self.regs.mode().write(|w| w.storage().bit(storage));
    }

    pub fn block_size(&self) -> u16 {
        self.regs.block_size().read().value().bits()
    }

    /// Read one 512-byte block at `lba` into `buf`. Callers must have checked
    /// `block_size() == 512`: the fixed 128-word drain (and the gateware's
    /// non-backpressuring byte packer) silently corrupts any other sector size.
    pub fn read_block(&self, lba: u32, buf: &mut [u8; 512]) -> Result<(), MscError> {
        if !self.ready() { return Err(MscError::NotReady); }
        self.regs.lba().write(|w| unsafe { w.value().bits(lba) });
        self.regs.start().write(|w| w.strobe().set_bit());
        // Drain 128 words (512 bytes). Spin on rx_avail per word.
        for i in 0..128usize {
            // Cap spin iterations to prevent an infinite hang if the device
            // stalls (no word, no error). The limit is generous enough for a
            // healthy but slow USB device; on-bench HW confirmation is needed.
            // Consistent with `sid_write_bp`'s 100_000-iteration cap pattern.
            #[cfg(not(test))]
            const MAX_SPIN: u32 = 1_000_000;
            #[cfg(not(test))]
            let mut spins: u32 = 0;
            loop {
                let st = self.regs.status().read();
                if st.rx_avail().bit_is_set() { break; }
                if self.regs.resp().read().error().bit_is_set() {
                    return Err(MscError::ReadError);
                }
                #[cfg(not(test))]
                {
                    spins += 1;
                    if spins >= MAX_SPIN {
                        return Err(MscError::ReadError);
                    }
                }
            }
            let word = self.regs.rx_data().read().word().bits();
            buf[i*4..i*4+4].copy_from_slice(&word.to_le_bytes());
        }
        Ok(())
    }
}

impl crate::fat::BlockIo for &UsbMsc {
    fn read_block(&mut self, lba: u32, buf: &mut [u8; 512]) -> Result<(), ()> {
        UsbMsc::read_block(self, lba, buf).map_err(|_| ())
    }
}
