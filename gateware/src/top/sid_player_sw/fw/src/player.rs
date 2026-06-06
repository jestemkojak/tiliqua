use mos6502::cpu::CPU;
use mos6502::instruction::Nmos6502;
use mos6502::memory::Bus;

/// 6502 address space backed by the tune image; $D400-$D41F writes are SID
/// register writes (captured in tests; sent to the SID CSR on hardware).
pub struct PsidBus<F: FnMut(u8, u8)> {
    pub mem: &'static mut [u8; 0x10000],
    pub on_sid_write: F,
}

impl<F: FnMut(u8, u8)> Bus for PsidBus<F> {
    fn get_byte(&mut self, a: u16) -> u8 { self.mem[a as usize] }
    fn set_byte(&mut self, a: u16, v: u8) {
        if a & 0xFFE0 == 0xD400 { (self.on_sid_write)((a & 0x1F) as u8, v); }
        else { self.mem[a as usize] = v; }
    }
}

/// SID transaction word for the SIDPeripheral CSR: (data << 5) | reg.
#[inline] pub fn sid_txn(reg: u8, val: u8) -> u16 { ((val as u16) << 5) | (reg as u16 & 0x1F) }

/// Run a 6502 subroutine to completion: push a sentinel return address, set PC,
/// then single-step until the routine's final RTS restores SP to its entry
/// value (bounded by `max_steps`). Returns false on overrun.
pub fn call<F: FnMut(u8, u8)>(cpu: &mut CPU<PsidBus<F>, Nmos6502>, addr: u16, max_steps: u64) -> bool {
    const SENTINEL: u16 = 0xFFFF;
    let sp0 = cpu.registers.stack_pointer.0;
    let ret = SENTINEL.wrapping_sub(1);                // RTS adds 1 -> SENTINEL
    let sp = cpu.registers.stack_pointer.0;
    cpu.memory.set_byte(0x0100 + sp as u16, (ret >> 8) as u8);
    cpu.memory.set_byte(0x0100 + sp.wrapping_sub(1) as u16, ret as u8);
    cpu.registers.stack_pointer.0 = sp.wrapping_sub(2);
    cpu.registers.program_counter = addr;
    for _ in 0..max_steps {
        if cpu.registers.stack_pointer.0 == sp0 { return true; }
        cpu.single_step();
    }
    false
}

/// Run INIT(subtune): A=subtune, then call init_addr to completion.
pub fn init<F: FnMut(u8, u8)>(cpu: &mut CPU<PsidBus<F>, Nmos6502>,
                              init_addr: u16, subtune: u8, max_steps: u64) -> bool {
    cpu.registers.accumulator = subtune;
    cpu.registers.index_x = 0; cpu.registers.index_y = 0;
    call(cpu, init_addr, max_steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed_mem() -> &'static mut [u8; 0x10000] {
        Box::leak(Box::new([0u8; 0x10000]))
    }

    #[test]
    fn init_writes_sid_via_hook() {
        let mem = boxed_mem();
        // $1000: LDA #$0F ; STA $D418 ; RTS
        mem[0x1000..0x1006].copy_from_slice(&[0xA9,0x0F, 0x8D,0x18,0xD4, 0x60]);
        let mut writes: std::vec::Vec<(u8,u8)> = std::vec::Vec::new();
        let bus = PsidBus { mem, on_sid_write: |r,v| writes.push((r,v)) };
        let mut cpu = CPU::new(bus, Nmos6502);
        cpu.registers.stack_pointer.0 = 0xFD;
        assert!(init(&mut cpu, 0x1000, 0, 100_000));
        assert_eq!(writes, vec![(0x18, 0x0F)]);
        assert_eq!(sid_txn(0x18, 0x0F), (0x0F << 5) | 0x18);
    }
}
