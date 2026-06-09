use mos6502::cpu::CPU;
use mos6502::instruction::Nmos6502;
use mos6502::memory::Bus;

/// Maximum SID writes buffered per PLAY frame (blockram budget: measured peak
/// ~160 writes/frame; 256 entries × 8 B ≤ 2 KB, fits in the 16 KB stack BRAM).
pub const SID_WRITE_CAP: usize = 256;

/// One SID register write with a frame-relative 6502-cycle stamp.
/// `cycle` is set by `call()` after each `single_step()`, not inside `set_byte`,
/// because the bus cannot see `cpu.cycles` mid-instruction.
pub struct SidWrite {
    pub cycle: u32,
    pub reg:   u8,
    pub val:   u8,
}

/// 6502 address space backed by the tune image.
/// $D400–$D41F writes are buffered as unstamped `SidWrite` entries;
/// `call()` stamps them after each instruction completes.
pub struct PsidBus {
    pub mem:     &'static mut [u8; 0x10000],
    pub writes:  heapless::Vec<SidWrite, SID_WRITE_CAP>,
    /// Cumulative count of writes lost because `writes` was full.
    pub dropped: u32,
}

impl Bus for PsidBus {
    fn get_byte(&mut self, a: u16) -> u8 { self.mem[a as usize] }
    fn set_byte(&mut self, a: u16, v: u8) {
        if a & 0xFFE0 == 0xD400 {
            if self.writes.push(SidWrite { cycle: 0, reg: (a & 0x1F) as u8, val: v }).is_err() {
                self.dropped += 1;
            }
        } else {
            self.mem[a as usize] = v;
        }
    }
}

/// SID transaction word for the SIDPeripheral CSR: (data << 5) | reg.
#[inline] pub fn sid_txn(reg: u8, val: u8) -> u16 { ((val as u16) << 5) | (reg as u16 & 0x1F) }

/// Run a 6502 subroutine to completion: push a sentinel return address, set PC,
/// then single-step until the routine's final RTS restores SP to its entry
/// value (bounded by `max_steps`). Returns false on overrun.
///
/// Clears `cpu.memory.writes` at entry and stamps newly-pushed entries after
/// each instruction, so callers see exactly this frame's writes with accurate
/// frame-relative cycle offsets.
pub fn call(cpu: &mut CPU<PsidBus, Nmos6502>, addr: u16, max_steps: u64) -> bool {
    const SENTINEL: u16 = 0xFFFF;
    let sp0 = cpu.registers.stack_pointer.0;
    let ret = SENTINEL.wrapping_sub(1);                // RTS adds 1 -> SENTINEL
    let sp = cpu.registers.stack_pointer.0;
    cpu.memory.set_byte(0x0100 + sp as u16, (ret >> 8) as u8);
    cpu.memory.set_byte(0x0100 + sp.wrapping_sub(1) as u16, ret as u8);
    cpu.registers.stack_pointer.0 = sp.wrapping_sub(2);
    cpu.registers.program_counter = addr;

    cpu.memory.writes.clear();
    let c0 = cpu.cycles;
    let mut stamped = 0usize;

    for _ in 0..max_steps {
        if cpu.registers.stack_pointer.0 == sp0 { return true; }
        cpu.single_step();
        // Stamp any entries pushed during this instruction.
        let now = (cpu.cycles - c0) as u32;
        let len = cpu.memory.writes.len();
        for w in &mut cpu.memory.writes[stamped..len] {
            w.cycle = now;
        }
        stamped = len;
    }
    false
}

/// Run INIT(subtune): A=subtune, then call init_addr to completion.
pub fn init(cpu: &mut CPU<PsidBus, Nmos6502>,
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

    fn new_bus(mem: &'static mut [u8; 0x10000]) -> PsidBus {
        PsidBus { mem, writes: heapless::Vec::new(), dropped: 0 }
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

        let mut cpu = CPU::new(new_bus(mem), Nmos6502);
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

        let mut cpu = CPU::new(new_bus(mem), Nmos6502);
        cpu.registers.stack_pointer.0 = 0xFD;
        assert!(init(&mut cpu, hdr.init_addr, hdr.start_song.saturating_sub(1) as u8, 2_000_000));

        let mut per_frame = std::vec::Vec::new();
        for _ in 0..3000 {
            assert!(call(&mut cpu, hdr.play_addr, 2_000_000));
            per_frame.push(cpu.memory.writes.len());
        }
        let max = *per_frame.iter().max().unwrap();
        let mean = per_frame.iter().sum::<usize>() as f64 / per_frame.len() as f64;
        const FIFO_DEPTH: usize = 16; // SIDPeripheral._transactions (top/sid/top.py)
        let over = per_frame.iter().filter(|&&w| w > FIFO_DEPTH).count();
        eprintln!("Commando PLAY writes/frame: max={max} mean={mean:.1} | \
                   frames>{FIFO_DEPTH}={over}/{}", per_frame.len());
        assert!(max > FIFO_DEPTH,
            "expected Commando to burst >{FIFO_DEPTH} writes/frame (overflow risk); got max={max}");
    }

    /// Overflow policy: a full buffer drops the write and counts it, never
    /// panics or reallocates (the buffer is a fixed Vec in blockram).
    #[test]
    fn overflow_drops_and_counts() {
        let mem = boxed_mem();
        let mut bus = new_bus(mem);
        for _ in 0..SID_WRITE_CAP + 3 {
            bus.set_byte(0xD400, 0x55);
        }
        assert_eq!(bus.writes.len(), SID_WRITE_CAP);
        assert_eq!(bus.dropped, 3);
    }

    /// Verify that `call()` captures SID writes and they appear in `cpu.memory.writes`.
    #[test]
    fn init_writes_sid_captured() {
        let mem = boxed_mem();
        // $1000: LDA #$0F ; STA $D418 ; RTS
        mem[0x1000..0x1006].copy_from_slice(&[0xA9,0x0F, 0x8D,0x18,0xD4, 0x60]);
        let mut cpu = CPU::new(new_bus(mem), Nmos6502);
        cpu.registers.stack_pointer.0 = 0xFD;
        assert!(init(&mut cpu, 0x1000, 0, 100_000));
        let writes = &cpu.memory.writes;
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].reg, 0x18);
        assert_eq!(writes[0].val, 0x0F);
        assert_eq!(sid_txn(0x18, 0x0F), (0x0F << 5) | 0x18);
    }

    /// The load-bearing property: stamps reflect inter-write 6502 cycle spacing.
    /// Hand-assembled routine at $1000:
    ///   LDA #$0F   (2 cy)  STA $D404 (4 cy)  NOP (2 cy)  NOP (2 cy)  STA $D404 (4 cy)  RTS
    /// write[0] at cycle 6 (LDA2 + STA4), write[1] at cycle 14 (+ NOP2 + NOP2 + STA4 = +8).
    #[test]
    fn stamp_delta() {
        let mem = boxed_mem();
        // A9 0F  8D 04 D4  EA  EA  8D 04 D4  60
        mem[0x1000..0x100B].copy_from_slice(&[0xA9,0x0F, 0x8D,0x04,0xD4, 0xEA, 0xEA, 0x8D,0x04,0xD4, 0x60]);
        let mut cpu = CPU::new(new_bus(mem), Nmos6502);
        cpu.registers.stack_pointer.0 = 0xFD;
        assert!(call(&mut cpu, 0x1000, 100_000));
        let writes = &cpu.memory.writes;
        assert_eq!(writes.len(), 2, "expected exactly 2 SID writes");
        let delta = writes[1].cycle - writes[0].cycle;
        assert_eq!(delta, 8, "NOP+NOP+STA = 2+2+4 = 8 cycles between the two writes; got {delta}");
        // LDA #$0F (2) + STA $D404 (4) = 6 cycles for first write.
        assert_eq!(writes[0].cycle, 6);
    }

    /// Stamps are monotonically non-decreasing across a full PLAY frame for a real tune.
    /// Also checks no write overflows the cap during normal playback.
    #[test]
    fn commando_stamps_monotonic() {
        use crate::psid::PsidHeader;
        static SID: &[u8] = include_bytes!("../../../../../../docs/Commando.sid");

        let mem = boxed_mem();
        let hdr = PsidHeader::parse(SID).expect("parse");
        let payload_raw = &SID[hdr.data_offset as usize..];
        let load = hdr.effective_load_addr(payload_raw) as usize;
        let payload = if hdr.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
        mem[load..load + payload.len()].copy_from_slice(payload);

        let mut cpu = CPU::new(new_bus(mem), Nmos6502);
        cpu.registers.stack_pointer.0 = 0xFD;
        assert!(init(&mut cpu, hdr.init_addr, hdr.start_song.saturating_sub(1) as u8, 2_000_000));

        for frame in 0..50 {
            let c_before = cpu.cycles;
            assert!(call(&mut cpu, hdr.play_addr, 2_000_000), "frame {frame} overran");
            let c_after = cpu.cycles;
            let frame_cycles = (c_after - c_before) as u32;
            let writes = &cpu.memory.writes;
            // Stamps must be non-decreasing within the frame.
            for i in 1..writes.len() {
                assert!(writes[i].cycle >= writes[i-1].cycle,
                    "frame {frame}: stamp not monotone at write {i}: {} < {}",
                    writes[i].cycle, writes[i-1].cycle);
            }
            // Last stamp must not exceed the frame's total cycle count.
            if let Some(last) = writes.last() {
                assert!(last.cycle <= frame_cycles,
                    "frame {frame}: last stamp {} > frame cycles {}",
                    last.cycle, frame_cycles);
            }
            assert_eq!(cpu.memory.dropped, 0, "frame {frame}: unexpected write drop");
        }
    }

    /// Guard the CAP=256 choice: INIT bursts must not overflow the buffer for
    /// our bundled tunes. Also prints counts for future reference.
    #[test]
    fn init_write_count_cap_coverage() {
        use crate::psid::PsidHeader;

        struct Tune { name: &'static str, data: &'static [u8] }
        let tunes = [
            Tune { name: "Gyroscope_3", data: include_bytes!("../Gyroscope_3.sid") },
            Tune { name: "Commando",    data: include_bytes!("../../../../../../docs/Commando.sid") },
        ];

        for tune in &tunes {
            let mem = boxed_mem();
            let hdr = PsidHeader::parse(tune.data).expect("parse");
            let payload_raw = &tune.data[hdr.data_offset as usize..];
            let load = hdr.effective_load_addr(payload_raw) as usize;
            let payload = if hdr.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
            mem[load..load + payload.len()].copy_from_slice(payload);

            let mut cpu = CPU::new(new_bus(mem), Nmos6502);
            cpu.registers.stack_pointer.0 = 0xFD;
            assert!(init(&mut cpu, hdr.init_addr, hdr.start_song.saturating_sub(1) as u8, 2_000_000),
                    "{}: INIT overran", tune.name);
            let n = cpu.memory.writes.len();
            eprintln!("{}: INIT writes={n} dropped={}", tune.name, cpu.memory.dropped);
            assert_eq!(cpu.memory.dropped, 0, "{}: INIT dropped writes", tune.name);
            assert!(n <= SID_WRITE_CAP, "{}: INIT writes {n} > CAP {SID_WRITE_CAP}", tune.name);
        }
    }
}
