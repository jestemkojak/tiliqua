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

    pub fn block_size(&self) -> u16 {
        self.regs.block_size().read().value().bits()
    }

    pub fn block_count(&self) -> u32 {
        self.regs.block_count().read().value().bits()
    }

    pub fn wait_ready(&self) { while !self.ready() {} }

    /// Read one 512-byte block at `lba` into `buf`. Callers must have checked
    /// `block_size() == 512`: the fixed 128-word drain (and the gateware's
    /// non-backpressuring byte packer) silently corrupts any other sector size.
    pub fn read_block(&self, lba: u32, buf: &mut [u8; 512]) -> Result<(), MscError> {
        if !self.ready() { return Err(MscError::NotReady); }
        self.regs.lba().write(|w| unsafe { w.value().bits(lba) });
        self.regs.start().write(|w| w.strobe().set_bit());
        // Drain 128 words (512 bytes). Spin on rx_avail per word.
        for i in 0..128usize {
            loop {
                let st = self.regs.status().read();
                if st.rx_avail().bit_is_set() { break; }
                if self.regs.resp().read().error().bit_is_set() {
                    return Err(MscError::ReadError);
                }
            }
            let word = self.regs.rx_data().read().word().bits();
            buf[i*4..i*4+4].copy_from_slice(&word.to_le_bytes());
        }
        Ok(())
    }
}
