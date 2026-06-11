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

    /// TEMP ANALYSIS (not a regression test; run with -- --ignored --nocapture):
    /// characterise Commando's note-trigger structure and per-frame workload
    /// variance, to test the inter-frame replay-jitter hypothesis.
    #[test]
    #[ignore]
    fn commando_trigger_structure() {
        use crate::psid::PsidHeader;
        static SID: &[u8] = include_bytes!("../../../../../../docs/Commando.sid");

        let mem = boxed_mem();
        let hdr = PsidHeader::parse(SID).expect("parse");
        let payload_raw = &SID[hdr.data_offset as usize..];
        let load = hdr.effective_load_addr(payload_raw) as usize;
        let payload = if hdr.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
        mem[load..load + payload.len()].copy_from_slice(payload);

        // OSC3/ENV3 ($D41B/$D41C) absolute-operand scan: does the player READ
        // live SID state (which PsidBus can't provide)?
        let mut osc3_refs = 0;
        for i in load..load + payload.len() - 1 {
            if mem[i + 1] == 0xD4 && (mem[i] == 0x1B || mem[i] == 0x1C) { osc3_refs += 1; }
        }

        let mut cpu = CPU::new(new_bus(mem), Nmos6502);
        cpu.registers.stack_pointer.0 = 0xFD;
        assert!(init(&mut cpu, hdr.init_addr, hdr.start_song.saturating_sub(1) as u8, 2_000_000));

        const CTRL: [u8; 3] = [0x04, 0x0B, 0x12];
        let mut gate = [false; 3];
        let mut last_off_frame: [Option<usize>; 3] = [None; 3];
        let mut same_frame = [0usize; 3];
        let mut cross_frame: [std::collections::BTreeMap<usize, usize>; 3] = Default::default();
        let mut triggers = [0usize; 3];
        let mut testbit = [0usize; 3];

        let mut cyc_per_frame = std::vec::Vec::new();
        for frame in 0..3000usize {
            let c0 = cpu.cycles;
            assert!(call(&mut cpu, hdr.play_addr, 2_000_000));
            cyc_per_frame.push((cpu.cycles - c0) as u32);
            for w in cpu.memory.writes.iter() {
                for v in 0..3 {
                    if w.reg == CTRL[v] {
                        let g = w.val & 1 != 0;
                        if w.val & 8 != 0 { testbit[v] += 1; }
                        if g && !gate[v] {
                            triggers[v] += 1;
                            match last_off_frame[v] {
                                Some(f) if f == frame => same_frame[v] += 1,
                                Some(f) => *cross_frame[v].entry(frame - f).or_insert(0) += 1,
                                None => {}
                            }
                        }
                        if !g && gate[v] { last_off_frame[v] = Some(frame); }
                        gate[v] = g;
                    }
                }
            }
        }
        let maxc = *cyc_per_frame.iter().max().unwrap();
        let minc = *cyc_per_frame.iter().min().unwrap();
        let max_adj = cyc_per_frame.windows(2).map(|w| w[0].abs_diff(w[1])).max().unwrap();
        let mean = cyc_per_frame.iter().map(|&c| c as u64).sum::<u64>() / cyc_per_frame.len() as u64;
        eprintln!("cpu_cyc/frame: min={minc} max={maxc} mean={mean} max_adjacent_delta={max_adj}");
        eprintln!("OSC3/ENV3 absolute-operand refs in image: {osc3_refs}");
        for v in 0..3 {
            eprintln!("voice{v}: triggers={} same_frame_off_on={} testbit_writes={} cross_frame_gaps={:?}",
                      triggers[v], same_frame[v], testbit[v], cross_frame[v]);
        }
    }

    /// TEMP ANALYSIS (not a regression test; run with -- --ignored --nocapture):
    /// full-tune scan of Commando (212 s ≈ 10600 frames at 50 Hz), focused on
    /// the fast part (>50 s = frame 2500+): per-bucket workload, write-stamp
    /// tails (replay length), trigger structure, and hard-restart (test-bit)
    /// usage — the timing-sensitive pattern that inter-frame replay jitter
    /// would break first.
    #[test]
    #[ignore]
    fn commando_full_tune_scan() {
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

        const FRAMES: usize = 10600;
        const BUCKET: usize = 500; // 10 s at 50 Hz
        const CTRL: [u8; 3] = [0x04, 0x0B, 0x12];
        const AD:   [u8; 3] = [0x05, 0x0C, 0x13];
        const SR:   [u8; 3] = [0x06, 0x0D, 0x14];

        #[derive(Default, Clone)]
        struct Bucket {
            cyc_max: u32, cyc_sum: u64, wr_max: usize, wr_sum: u64,
            stamp_max: u32, triggers: usize, testbit: usize, frames: usize,
        }
        let mut buckets = vec![Bucket::default(); FRAMES / BUCKET + 1];
        let mut gate = [false; 3];
        let mut test = [false; 3];
        let mut cur_ad = [0u8; 3];
        let mut cur_sr = [0u8; 3];
        // frame index of the most recent test-bit-set per voice (hard restart)
        let mut last_test_frame: [Option<usize>; 3] = [None; 3];
        let mut last_off_frame:  [Option<usize>; 3] = [None; 3];
        // (frame, voice, gap_from_gate_off, hard_restart, ad, sr) for fast-part triggers
        let mut fast_triggers: std::vec::Vec<(usize, usize, Option<usize>, bool, u8, u8)> =
            std::vec::Vec::new();
        let mut top_cyc: std::vec::Vec<(u32, usize)> = std::vec::Vec::new();

        for frame in 0..FRAMES {
            let c0 = cpu.cycles;
            assert!(call(&mut cpu, hdr.play_addr, 2_000_000), "frame {frame} overran");
            let cyc = (cpu.cycles - c0) as u32;
            let b = &mut buckets[frame / BUCKET];
            b.frames += 1;
            b.cyc_max = b.cyc_max.max(cyc);
            b.cyc_sum += cyc as u64;
            b.wr_max = b.wr_max.max(cpu.memory.writes.len());
            b.wr_sum += cpu.memory.writes.len() as u64;
            if let Some(w) = cpu.memory.writes.last() { b.stamp_max = b.stamp_max.max(w.cycle); }
            top_cyc.push((cyc, frame));
            assert_eq!(cpu.memory.dropped, 0);

            for w in cpu.memory.writes.iter() {
                for v in 0..3 {
                    if w.reg == AD[v] { cur_ad[v] = w.val; }
                    if w.reg == SR[v] { cur_sr[v] = w.val; }
                    if w.reg == CTRL[v] {
                        let g = w.val & 1 != 0;
                        let t = w.val & 8 != 0;
                        if t && !test[v] { b.testbit += 1; last_test_frame[v] = Some(frame); }
                        if g && !gate[v] {
                            b.triggers += 1;
                            let hard = last_test_frame[v].is_some_and(|f| frame - f <= 3);
                            if frame >= 2500 {
                                let gap = last_off_frame[v].map(|f| frame - f);
                                fast_triggers.push((frame, v, gap, hard, cur_ad[v], cur_sr[v]));
                            }
                        }
                        if !g && gate[v] { last_off_frame[v] = Some(frame); }
                        gate[v] = g; test[v] = t;
                    }
                }
            }
        }

        eprintln!("bucket(10s)  t(s)   cyc_max cyc_mean  wr_max wr_mean  stamp_max  trig  testbit");
        for (i, b) in buckets.iter().enumerate() {
            if b.frames == 0 { continue; }
            eprintln!("{:>10}  {:>4}   {:>7} {:>8}  {:>6} {:>7.1}  {:>9}  {:>4}  {:>7}",
                i, i * 10, b.cyc_max, b.cyc_sum / b.frames as u64,
                b.wr_max, b.wr_sum as f64 / b.frames as f64, b.stamp_max,
                b.triggers, b.testbit);
        }
        top_cyc.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        eprintln!("\ntop-10 cpu_cyc frames (frame -> t):");
        for &(c, f) in top_cyc.iter().take(10) {
            eprintln!("  cyc={c:>5} frame={f:>5} t={:.1}s", f as f64 / 50.0);
        }
        let hard_n = fast_triggers.iter().filter(|t| t.3).count();
        let gaps: std::collections::BTreeMap<Option<usize>, usize> = fast_triggers.iter()
            .fold(Default::default(), |mut m, t| { *m.entry(t.2.map(|g| g.min(5))).or_insert(0) += 1; m });
        eprintln!("\nfast-part (frame>=2500) triggers: {} total, hard_restart={} \
                   gate_off->on gap histogram (frames, 5=5+): {gaps:?}",
                  fast_triggers.len(), hard_n);
        let decays: std::collections::BTreeMap<u8, usize> = fast_triggers.iter()
            .fold(Default::default(), |mut m, t| { *m.entry(t.4 & 0xF).or_insert(0) += 1; m });
        eprintln!("fast-part decay-nibble histogram: {decays:?}");
        // attack nibble 0 = 2ms attack: the most jitter-sensitive envelope
        let fast_attacks: std::collections::BTreeMap<u8, usize> = fast_triggers.iter()
            .fold(Default::default(), |mut m, t| { *m.entry(t.4 >> 4).or_insert(0) += 1; m });
        eprintln!("fast-part attack-nibble histogram (0=2ms .. 15=8s): {fast_attacks:?}");
        let sr0 = fast_triggers.iter().filter(|t| t.5 == 0).count();
        eprintln!("fast-part triggers with SR=0 at gate-on (hard-restart style): {sr0}");
    }

    /// TEMP ANALYSIS (run with --release -- --ignored --nocapture):
    /// reSID-faithful envelope-generator model driven by Commando's captured
    /// write stream, to test the ADSR-delay-bug jitter hypothesis: the fast
    /// part is all attack=0/SR=0 percussion with no hard restart, so whether a
    /// note's attack fires immediately or is swallowed by a ~33 ms rate-counter
    /// wrap depends on the envelope rate-counter phase (mod the previous rate
    /// period) at the exact gate-write cycle. Frame-anchor jitter re-rolls that
    /// phase. Counts delayed/weak notes for several anchor-jitter magnitudes.
    #[test]
    #[ignore]
    fn commando_envelope_jitter_model() {
        use crate::psid::PsidHeader;
        static SID: &[u8] = include_bytes!("../../../../../../docs/Commando.sid");

        // --- reSID-style envelope generator (envelope.cc semantics) ---------
        const RATE: [u16; 16] = [9, 32, 63, 95, 149, 220, 267, 313, 392, 977,
                                 1954, 3126, 3907, 11720, 19532, 31251];
        #[derive(Clone, Copy, PartialEq)]
        enum St { Attack, DecaySustain, Release }
        #[derive(Clone, Copy)]
        struct Env {
            rate_counter: u16, rate_period: u16,
            exp_counter: u8, exp_period: u8,
            env: u8, state: St, hold_zero: bool, gate: bool,
            ad: u8, sr: u8,
        }
        impl Env {
            fn new() -> Self {
                Env { rate_counter: 0, rate_period: RATE[0], exp_counter: 0,
                      exp_period: 1, env: 0, state: St::Release,
                      hold_zero: true, gate: false, ad: 0, sr: 0 }
            }
            fn write_control(&mut self, val: u8) {
                let gate_next = val & 1 != 0;
                if !self.gate && gate_next {
                    self.state = St::Attack;
                    self.rate_period = RATE[(self.ad >> 4) as usize];
                    self.hold_zero = false;
                } else if self.gate && !gate_next {
                    self.state = St::Release;
                    self.rate_period = RATE[(self.sr & 0xF) as usize];
                }
                self.gate = gate_next;
            }
            fn write_ad(&mut self, val: u8) {
                self.ad = val;
                match self.state {
                    St::Attack       => self.rate_period = RATE[(val >> 4) as usize],
                    St::DecaySustain => self.rate_period = RATE[(val & 0xF) as usize],
                    _ => {}
                }
            }
            fn write_sr(&mut self, val: u8) {
                self.sr = val;
                if self.state == St::Release { self.rate_period = RATE[(val & 0xF) as usize]; }
            }
            #[inline]
            fn clock(&mut self) {
                // 15-bit counter with equality compare = the delay bug.
                self.rate_counter = (self.rate_counter + 1) & 0x7FFF;
                if self.rate_counter != self.rate_period { return; }
                self.rate_counter = 0;
                self.exp_counter += 1;
                if self.state == St::Attack || self.exp_counter == self.exp_period {
                    self.exp_counter = 0;
                    if self.hold_zero { return; }
                    match self.state {
                        St::Attack => {
                            self.env = self.env.wrapping_add(1);
                            if self.env == 0xFF {
                                self.state = St::DecaySustain;
                                self.rate_period = RATE[(self.ad & 0xF) as usize];
                            }
                        }
                        St::DecaySustain => {
                            let sus = (self.sr >> 4) * 0x11;
                            if self.env != sus { self.env = self.env.wrapping_sub(1); }
                        }
                        St::Release => { self.env = self.env.wrapping_sub(1); }
                    }
                    match self.env {
                        0xFF => self.exp_period = 1,
                        0x5D => self.exp_period = 2,
                        0x36 => self.exp_period = 4,
                        0x1A => self.exp_period = 8,
                        0x0E => self.exp_period = 16,
                        0x06 => self.exp_period = 30,
                        0x00 => { self.exp_period = 1; self.hold_zero = true; }
                        _ => {}
                    }
                }
            }
        }

        // --- capture the write stream (frame, stamp, reg, val) --------------
        let mem = boxed_mem();
        let hdr = PsidHeader::parse(SID).expect("parse");
        let payload_raw = &SID[hdr.data_offset as usize..];
        let load = hdr.effective_load_addr(payload_raw) as usize;
        let payload = if hdr.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
        mem[load..load + payload.len()].copy_from_slice(payload);
        let mut cpu = CPU::new(new_bus(mem), Nmos6502);
        cpu.registers.stack_pointer.0 = 0xFD;
        assert!(init(&mut cpu, hdr.init_addr, hdr.start_song.saturating_sub(1) as u8, 2_000_000));
        let init_writes: std::vec::Vec<(u8, u8)> =
            cpu.memory.writes.iter().map(|w| (w.reg, w.val)).collect();

        const FRAMES: usize = 10600;
        let mut frames: std::vec::Vec<std::vec::Vec<(u32, u8, u8)>> =
            std::vec::Vec::with_capacity(FRAMES);
        for _ in 0..FRAMES {
            assert!(call(&mut cpu, hdr.play_addr, 2_000_000));
            frames.push(cpu.memory.writes.iter()
                .map(|w| (w.cycle, w.reg, w.val)).collect());
        }

        // --- replay into the model under a given per-frame anchor jitter ----
        // Returns (delayed>2ms, delayed>10ms, weak[peak<0x40], total) for
        // fast-part (frame>=2500) gate-on notes.
        let run = |period: u64, jitter_cyc: u64, seed: u64| -> (usize, usize, usize, usize) {
            let mut envs = [Env::new(); 3];
            for &(r, v) in &init_writes {
                let v_i = (r / 7) as usize;
                if v_i < 3 {
                    match r % 7 {
                        4 => envs[v_i].write_control(v),
                        5 => envs[v_i].write_ad(v),
                        6 => envs[v_i].write_sr(v),
                        _ => {}
                    }
                }
            }
            let mut rng = seed | 1;
            let mut next_jit = move || {
                rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
                if jitter_cyc == 0 { 0 } else { rng % (2 * jitter_cyc + 1) }
            };
            // note state per voice: (gate_on_cycle, attack_started, peak, frame)
            let mut note: [Option<(u64, bool, u8, usize)>; 3] = [None; 3];
            let (mut d2, mut d10, mut weak, mut total) = (0usize, 0, 0, 0);
            let mut t: u64 = 0;
            let close = |n: &mut Option<(u64, bool, u8, usize)>, t: u64,
                             d2: &mut usize, d10: &mut usize, weak: &mut usize,
                             total: &mut usize| {
                if let Some((t_on, started, peak, fr)) = n.take() {
                    if fr >= 2500 {
                        *total += 1;
                        let dly = if started { 0 } else { t - t_on };
                        if !started || peak < 0x40 { *weak += 1; }
                        if dly > 2_000 { *d2 += 1; }
                        if dly > 10_000 { *d10 += 1; }
                    }
                }
            };
            for (fi, fw) in frames.iter().enumerate() {
                let base = fi as u64 * period + period / 2 + next_jit();
                for &(stamp, r, v) in fw {
                    let target = base + stamp as u64;
                    while t < target {
                        for e in envs.iter_mut() { e.clock(); }
                        for v_i in 0..3 {
                            if let Some(n) = note[v_i].as_mut() {
                                if !n.1 && envs[v_i].state == St::Attack && envs[v_i].env > 0 {
                                    n.1 = true;
                                    n.0 = t - n.0; // repurpose: delay
                                    let d = n.0;
                                    if n.3 >= 2500 {
                                        if d > 2_000 { d2 += 1; }
                                        if d > 10_000 { d10 += 1; }
                                    }
                                    n.0 = 0;
                                }
                                if envs[v_i].env > n.2 { n.2 = envs[v_i].env; }
                            }
                        }
                        t += 1;
                    }
                    let v_i = (r / 7) as usize;
                    if v_i < 3 {
                        match r % 7 {
                            4 => {
                                let was = envs[v_i].gate;
                                envs[v_i].write_control(v);
                                if !was && envs[v_i].gate {
                                    // close the previous note, open a new one
                                    let mut prev = note[v_i].take();
                                    close(&mut prev, t, &mut d2, &mut d10, &mut weak, &mut total);
                                    note[v_i] = Some((t, false, 0, fi));
                                }
                            }
                            5 => envs[v_i].write_ad(v),
                            6 => envs[v_i].write_sr(v),
                            _ => {}
                        }
                    }
                }
            }
            for v_i in 0..3 {
                let mut n = note[v_i].take();
                close(&mut n, t, &mut d2, &mut d10, &mut weak, &mut total);
            }
            (d2, d10, weak, total)
        };

        eprintln!("scenario                          delayed>2ms  >10ms  weak(peak<0x40)  /total fast-part notes");
        let (d2, d10, w, n) = run(20_000, 0, 12345);
        eprintln!("player ideal (20000cyc, jit=0)    {d2:>11}  {d10:>5}  {w:>15}  /{n}");
        let (d2, d10, w, n) = run(19_656, 0, 12345);
        eprintln!("C64 PAL frame (19656cyc, jit=0)   {d2:>11}  {d10:>5}  {w:>15}  /{n}");
        for j in [3u64, 10, 50, 200, 1000] {
            let (d2, d10, w, n) = run(20_000, j, 12345);
            eprintln!("player + anchor jitter ±{j:<5}µs   {d2:>11}  {d10:>5}  {w:>15}  /{n}");
        }
    }

    /// TEMP ANALYSIS (run with -- --ignored --nocapture): startup-glitch probe.
    /// Dumps Commando's INIT write sequence and the first PLAY frames' voice
    /// writes (the tiliqua capture has a loud noise burst at ~14 ms that websid
    /// lacks), and flags any 6502 reads from addresses that were neither loaded
    /// from the .sid payload nor previously written — those return zeros on the
    /// host (and in websid) but PSRAM garbage on hardware, because
    /// `load_psid_to_mem` never zeroes the image.
    #[test]
    #[ignore]
    fn commando_init_trace() {
        use crate::psid::PsidHeader;
        static SID: &[u8] = include_bytes!("../../../../../../docs/Commando.sid");

        let mem = boxed_mem();
        let hdr = PsidHeader::parse(SID).expect("parse");
        let payload_raw = &SID[hdr.data_offset as usize..];
        let load = hdr.effective_load_addr(payload_raw) as usize;
        let payload = if hdr.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
        mem[load..load + payload.len()].copy_from_slice(payload);
        eprintln!("payload ${load:04X}-${:04X}  init=${:04X} play=${:04X}",
                  load + payload.len() - 1, hdr.init_addr, hdr.play_addr);

        // Wrap the bus to track known (loaded/written) addresses and log
        // unknown reads. PsidBus is concrete, so shadow-track via a bitmap
        // consulted around call() instead: simplest is a parallel scan —
        // mos6502 gives no read hook, so instrument by diffing isn't possible;
        // instead mark the loaded range + IO and rely on a second pass below.
        let mut known = vec![false; 0x10000];
        for a in load..load + payload.len() { known[a] = true; }
        for a in 0xD400..0xD420 { known[a] = true; }  // SID write-only here
        known[0xDC04] = true; known[0xDC05] = true;   // zeroed by loader

        let mut cpu = CPU::new(new_bus(mem), Nmos6502);
        cpu.registers.stack_pointer.0 = 0xFD;
        assert!(init(&mut cpu, hdr.init_addr, hdr.start_song.saturating_sub(1) as u8, 2_000_000));

        eprintln!("\nINIT writes ({}):", cpu.memory.writes.len());
        for w in cpu.memory.writes.iter() {
            let v = w.reg / 7;
            let kind = match w.reg {
                r if r % 7 == 4 && r < 21 => "CTRL",
                r if r % 7 == 5 && r < 21 => "AD",
                r if r % 7 == 6 && r < 21 => "SR",
                0x17 => "RESFILT", 0x18 => "MODEVOL",
                0x15 | 0x16 => "FILT_FC", _ => "",
            };
            eprintln!("  cyc={:>5} ${:02X} <= ${:02X}  v{} {}",
                      w.cycle, w.reg, w.val, if w.reg < 21 { v.to_string() } else { "-".into() }, kind);
        }

        // First 12 PLAY frames: voice-1 (and ctrl of all voices) writes.
        for frame in 0..12 {
            assert!(call(&mut cpu, hdr.play_addr, 2_000_000));
            let mut line = std::string::String::new();
            for w in cpu.memory.writes.iter() {
                use std::fmt::Write;
                let interesting = w.reg % 7 == 4 && w.reg < 21      // any CTRL
                    || (0x07..0x0E).contains(&w.reg)                 // all voice1 regs
                    || w.reg == 0x18 || w.reg == 0x17;
                if interesting {
                    write!(line, " ${:02X}<=${:02X}@{}", w.reg, w.val, w.cycle).ok();
                }
            }
            eprintln!("frame {frame:>2} (t={:>3}ms) writes={:>2}:{}",
                      frame * 20, cpu.memory.writes.len(), line);
        }

        // Second pass: static scan for reads of never-initialised memory is
        // not possible without a bus hook; approximate by running INIT+frames
        // again on a canary-filled image and diffing the write streams.
        let mem2 = boxed_mem();
        for b in mem2.iter_mut() { *b = 0xAA; } // simulate PSRAM garbage
        mem2[load..load + payload.len()].copy_from_slice(payload);
        mem2[0xDC04] = 0; mem2[0xDC05] = 0;
        let mut cpu2 = CPU::new(new_bus(mem2), Nmos6502);
        cpu2.registers.stack_pointer.0 = 0xFD;
        let init2_ok = init(&mut cpu2, hdr.init_addr, hdr.start_song.saturating_sub(1) as u8, 2_000_000);
        eprintln!("\ncanary-image INIT ok={init2_ok}");

        // Re-run both from scratch and diff the full write streams frame by frame.
        let mk = |fill: u8| {
            let m = boxed_mem();
            for b in m.iter_mut() { *b = fill; }
            m[load..load + payload.len()].copy_from_slice(payload);
            m[0xDC04] = 0; m[0xDC05] = 0;
            let mut c = CPU::new(new_bus(m), Nmos6502);
            c.registers.stack_pointer.0 = 0xFD;
            assert!(init(&mut c, hdr.init_addr, hdr.start_song.saturating_sub(1) as u8, 2_000_000));
            c
        };
        let mut a = mk(0x00);
        let mut b = mk(0xAA);
        let ia: std::vec::Vec<_> = a.memory.writes.iter().map(|w| (w.cycle, w.reg, w.val)).collect();
        let ib: std::vec::Vec<_> = b.memory.writes.iter().map(|w| (w.cycle, w.reg, w.val)).collect();
        eprintln!("INIT streams zero-fill vs 0xAA-fill: {}", if ia == ib { "IDENTICAL" } else { "DIFFER!" });
        let mut first_diff = None;
        for frame in 0..500 {
            assert!(call(&mut a, hdr.play_addr, 2_000_000), "zero frame {frame}");
            assert!(call(&mut b, hdr.play_addr, 2_000_000), "canary frame {frame}");
            let wa: std::vec::Vec<_> = a.memory.writes.iter().map(|w| (w.cycle, w.reg, w.val)).collect();
            let wb: std::vec::Vec<_> = b.memory.writes.iter().map(|w| (w.cycle, w.reg, w.val)).collect();
            if wa != wb && first_diff.is_none() {
                first_diff = Some(frame);
                eprintln!("first PLAY-stream divergence at frame {frame} (t={}ms):", frame * 20);
                eprintln!("  zero : {wa:?}");
                eprintln!("  0xAA : {wb:?}");
            }
        }
        match first_diff {
            Some(_) => eprintln!("=> tune IS sensitive to uninitialised memory (HW garbage diverges from host zeros)"),
            None => eprintln!("=> 500 frames identical on zero vs 0xAA fill: tune does NOT read uninitialised memory"),
        }
        let _ = known;
    }

    /// TEMP ANALYSIS (run with -- --ignored --nocapture): per-voice gate/note
    /// trace for a frame window (default 2690..2820 ≈ 53.8–56.4 s, around the
    /// user-confirmed audible drop at ~55 s). Shows, for each voice: gate
    /// transitions, waveform, freq, AD/SR — i.e. exactly what the SID was told
    /// to play, to separate "note never commanded" from "note commanded but
    /// rendered differently".
    #[test]
    #[ignore]
    fn commando_gate_trace_55s() {
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

        let (w0, w1) = (2690usize, 2820usize);
        // shadow SID register file to know freq/AD/SR at each gate event
        let mut regs = [0u8; 25];
        for frame in 0..w1 {
            assert!(call(&mut cpu, hdr.play_addr, 2_000_000));
            for w in cpu.memory.writes.iter() {
                let r = w.reg as usize;
                if r < 25 {
                    if frame >= w0 && r % 7 == 4 && r < 21 {
                        let v = r / 7;
                        let old = regs[r];
                        let (g0, g1) = (old & 1, w.val & 1);
                        let freq = u16::from_le_bytes([regs[v * 7], regs[v * 7 + 1]]);
                        let wave = w.val & 0xF0;
                        let edge = match (g0, g1) {
                            (0, 1) => "ON ",
                            (1, 0) => "off",
                            _      => "...",
                        };
                        eprintln!(
                            "frame {frame} t={:>7.2}s v{v} {edge} ctrl={:02X} wave={wave:02X} \
                             freq={freq:04X} ad={:02X} sr={:02X}",
                            frame as f64 * 0.02, w.val,
                            regs[v * 7 + 5], regs[v * 7 + 6]);
                    }
                    regs[r] = w.val;
                }
            }
        }
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
