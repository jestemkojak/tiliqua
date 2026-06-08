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

    /// Repro of the hardware bring-up path on the host: parse the bundled
    /// fallback tune, load it into the image, run INIT then a few PLAY frames.
    /// If `mos6502` lacks an opcode the tune uses, this either panics
    /// ("unimplemented or invalid instruction") or reports OVERRAN — the same
    /// failure that freezes the firmware (panic handler loops → dead UI).
    #[test]
    fn fallback_tune_init_and_play_complete() {
        use crate::psid::PsidHeader;
        static SID: &[u8] = include_bytes!("../Gyroscope_3.sid");

        let mem = boxed_mem();
        let hdr = PsidHeader::parse(SID).expect("parse");
        let payload_raw = &SID[hdr.data_offset as usize..];
        let load = hdr.effective_load_addr(payload_raw) as usize;
        let payload = if hdr.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
        mem[load..load + payload.len()].copy_from_slice(payload);

        let bus = PsidBus { mem, on_sid_write: |_r, _v| {} };
        let mut cpu = CPU::new(bus, Nmos6502);
        cpu.registers.stack_pointer.0 = 0xFD;

        let init_ok = init(&mut cpu, hdr.init_addr, hdr.start_song.saturating_sub(1) as u8, 2_000_000);
        assert!(init_ok, "INIT overran max_steps (PC stuck on an unsupported opcode?)");

        for frame in 0..3 {
            let ok = call(&mut cpu, hdr.play_addr, 2_000_000);
            assert!(ok, "PLAY frame {frame} overran max_steps");
        }
    }

    /// EVIDENCE for the dropped-note bug: count SID register writes issued per
    /// PLAY frame for Commando. The gateware transaction FIFO is depth-16 and
    /// drains exactly one write per phi2 (~1MHz). The emulated 6502 is NOT
    /// throttled to 1MHz, so any frame that bursts >16 writes faster than the
    /// FIFO drains will silently overflow it -> dropped writes -> dropped notes.
    #[test]
    fn commando_writes_per_frame() {
        use crate::psid::PsidHeader;
        static SID: &[u8] = include_bytes!("../../../../../../docs/Commando.sid");

        let mem = boxed_mem();
        let hdr = PsidHeader::parse(SID).expect("parse");
        let payload_raw = &SID[hdr.data_offset as usize..];
        let load = hdr.effective_load_addr(payload_raw) as usize;
        let payload = if hdr.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
        mem[load..load + payload.len()].copy_from_slice(payload);

        let count = std::cell::Cell::new(0usize);
        let bus = PsidBus { mem, on_sid_write: |_r, _v| count.set(count.get() + 1) };
        let mut cpu = CPU::new(bus, Nmos6502);
        cpu.registers.stack_pointer.0 = 0xFD;
        assert!(init(&mut cpu, hdr.init_addr, hdr.start_song.saturating_sub(1) as u8, 2_000_000));

        let mut per_frame = std::vec::Vec::new();
        for _ in 0..3000 {
            count.set(0);
            assert!(call(&mut cpu, hdr.play_addr, 2_000_000));
            per_frame.push(count.get());
        }
        let max = *per_frame.iter().max().unwrap();
        let mean = per_frame.iter().sum::<usize>() as f64 / per_frame.len() as f64;
        const FIFO_DEPTH: usize = 16; // SIDPeripheral._transactions (top/sid/top.py)
        let over = per_frame.iter().filter(|&&w| w > FIFO_DEPTH).count();
        eprintln!("Commando PLAY writes/frame: max={max} mean={mean:.1} | \
                   frames>{FIFO_DEPTH}={over}/{}", per_frame.len());
        // The gateware transaction FIFO is FIFO_DEPTH deep and drains 1 write per
        // phi2 (~1MHz). The emulated 6502 runs faster than real-time (a frame's
        // work completes in << the 20ms frame period), so a burst of writes is
        // issued far faster than the FIFO drains. Any frame bursting > FIFO_DEPTH
        // writes can overflow it -> writes silently dropped -> dropped notes.
        assert!(max > FIFO_DEPTH,
            "expected Commando to burst >{FIFO_DEPTH} writes/frame (overflow risk); got max={max}");
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
