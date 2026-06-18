# CV Modulation of SID Parameters — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the three eurorack CV inputs modulate live SID playback (CV1 = filter cutoff offset, CV2 = pulse-width offset on all voices, CV3 = progressive voice mute), entirely in `sid_player_sw` firmware.

**Architecture:** A new pure module `fw/src/cvmod.rs` exposes `CvMod::compute(&shadow, dirty, cv_raw, jacks) -> Vec<(reg,val)>` — no hardware access, fully host-testable. `main.rs` keeps a `sid_shadow: [u8; 0x20]` of the tune's per-register base intent, builds a per-frame `dirty` mask of registers the tune wrote, reads the 3 CV samples + the `jack` CSR in the TIMER0 `play_tick` ISR (after the tune's writes drain), calls `compute`, and drains the resulting override writes through the existing backpressured `sid_write_bp`. `reload_tune` zeroes the shadow and resets `CvMod`.

**Tech Stack:** Rust `no_std` / `riscv32im`, `heapless::Vec`, `mos6502` crate, the generated `pac` (eurorack_pmod CSRs already present), `critical_section`.

## Global Constraints

- `no_std` firmware, default target `riscv32im`; **host tests run with** `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib`.
- **No heap.** Use `heapless::Vec` and fixed-size arrays only.
- **Every SID write issued from the ISR must go through `sid_write_bp`** (polls the depth-16 FIFO's `writable`). Never issue a raw burst — overflow = dropped notes.
- **Audio/SID timing always wins over visuals.** Keep per-frame work minimal: patch-gating + change-detection so an idle/unpatched feature costs ~0 writes/frame (protects the fast-CIA-tune real-time budget).
- The CV→parameter logic lives in the **pure** `compute` (no `pac`/hardware) so it is host-unit-testable.
- **No gateware, no PAC regen, no menu/UI changes.**
- CV read scale: a `sample_iN` read cast `as i16` is signed counts; **4000 counts = 1 volt** (matches `macro_osc`/`PMOD0_PERIPH.info.counts_per_mv`).
- SID register map (offsets into `[u8; 0x20]`): cutoff `0x15` (FC_LO, low 3 bits) + `0x16` (FC_HI, 8 bits); PW pairs V1 `(0x02,0x03)`, V2 `(0x09,0x0A)`, V3 `(0x10,0x11)` (lo + hi nibble = 12-bit); control regs V1 `0x04`, V2 `0x0B`, V3 `0x12` (bits 4–7 = waveform select, bits 0–3 = gate/sync/ring/test).

---

### Task 1: `cvmod` module scaffold — constants, state, base extractors

**Files:**
- Create: `gateware/src/top/sid_player_sw/fw/src/cvmod.rs`
- Modify: `gateware/src/top/sid_player_sw/fw/src/lib.rs` (add `pub mod cvmod;` **without** `#[cfg(not(test))]` so it is host-tested)
- Test: in-module `#[cfg(test)] mod tests` in `cvmod.rs`

**Interfaces:**
- Produces:
  - `pub const SID_REGS: usize = 0x20;`
  - `pub type SidShadow = [u8; SID_REGS];`
  - `pub const MAX_WRITES: usize = 16;`
  - `pub type WriteList = heapless::Vec<(u8, u8), MAX_WRITES>;`
  - `pub struct CvMod { … }` with `pub const fn new() -> Self` and `pub fn reset(&mut self)`
  - `pub fn cutoff_base(shadow: &SidShadow) -> i32` (0–2047)
  - `pub fn pw_base(shadow: &SidShadow, voice: usize) -> i32` (0–4095)
  - tunable constants `COUNTS_PER_VOLT`, `CUTOFF_CTS_PER_V`, `PW_CTS_PER_V`, `SLEW_K`, `ZONE_WIDTH`, `ZONE_HYST`
  - private reg tables `PW_REGS: [(usize,usize);3]`, `CTRL_REGS: [usize;3]`
- Consumes: nothing (first task).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_extractors() {
        let mut s: SidShadow = [0; SID_REGS];
        // cutoff = FC_LO[2:0] | FC_HI<<3
        s[0x15] = 0b101;       // low 3 bits
        s[0x16] = 0xFF;        // high 8 bits
        assert_eq!(cutoff_base(&s), (0xFF << 3) | 0b101); // 2045

        // PW voice 0 = lo | (hi&0xF)<<8
        s[0x02] = 0x34;
        s[0x03] = 0x12;        // only low nibble counts
        assert_eq!(pw_base(&s, 0), 0x234);
        // voices 1 and 2 use their own reg pairs
        s[0x09] = 0xFF; s[0x0A] = 0x0F;
        assert_eq!(pw_base(&s, 1), 0xFFF);
        s[0x10] = 0x00; s[0x11] = 0x00;
        assert_eq!(pw_base(&s, 2), 0x000);
    }

    #[test]
    fn new_is_idle() {
        let cv = CvMod::new();
        let s: SidShadow = [0; SID_REGS];
        // nothing patched -> no writes
        let out = { let mut cv = cv; cv.compute(&s, 0, [0, 0, 0], 0) };
        assert!(out.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib cvmod`
Expected: FAIL to compile — `cvmod` / `cutoff_base` / `pw_base` / `CvMod` not found.

- [ ] **Step 3: Write minimal implementation**

In `lib.rs`, add alongside the other host-testable modules (e.g. near `pub mod player;`):

```rust
pub mod cvmod;
```

Create `cvmod.rs`:

```rust
//! CV modulation of live SID playback (pure, host-testable).
//!
//! The three eurorack CV inputs offset/override SID registers each PLAY frame:
//!   CV1 -> filter cutoff offset (bipolar)
//!   CV2 -> pulse-width offset, all 3 voices (bipolar)
//!   CV3 -> progressive voice mute (unipolar, 4 zones, hysteresis)
//! `compute` has no hardware access; the ISR feeds it the SID shadow + raw CV
//! counts + jack-detect bits and drains the returned writes via sid_write_bp.

pub const SID_REGS: usize = 0x20;
pub type SidShadow = [u8; SID_REGS];

/// Max register writes one `compute` can emit: cutoff(2)+pw(6)+ctrl(3)=11; pad.
pub const MAX_WRITES: usize = 16;
pub type WriteList = heapless::Vec<(u8, u8), MAX_WRITES>;

// --- Tunables (all integer; no_std-friendly) -------------------------------
/// Calibrated input scale: 4000 counts == 1 volt (matches PMOD0 info CSR).
pub const COUNTS_PER_VOLT: i32 = 4000;
/// Cutoff offset depth: ~410 cutoff-counts/volt -> +/-5V ~= full 11-bit range.
pub const CUTOFF_CTS_PER_V: i32 = 410;
/// PW offset depth: ~400 PW-counts/volt -> +/-5V ~= +/-half of 12-bit range.
pub const PW_CTS_PER_V: i32 = 400;
/// 1-pole EMA shift for input slew (acc += (raw-acc) >> K). Lower = snappier.
pub const SLEW_K: u32 = 3;
/// CV3 zone width (1.25 V) and hysteresis deadband (0.12 V), in counts.
pub const ZONE_WIDTH: i32 = 5000; // 1.25 * 4000
pub const ZONE_HYST:  i32 = 480;  // 0.12 * 4000

// SID register layout.
const PW_REGS:   [(usize, usize); 3] = [(0x02, 0x03), (0x09, 0x0A), (0x10, 0x11)];
const CTRL_REGS: [usize; 3] = [0x04, 0x0B, 0x12];
const FC_LO: usize = 0x15;
const FC_HI: usize = 0x16;

/// Sentinel for "chip value unknown" in the change-detection cache.
const UNKNOWN: i16 = -1;

pub fn cutoff_base(shadow: &SidShadow) -> i32 {
    ((shadow[FC_LO] & 0x07) as i32) | ((shadow[FC_HI] as i32) << 3)
}

pub fn pw_base(shadow: &SidShadow, voice: usize) -> i32 {
    let (lo, hi) = PW_REGS[voice];
    (shadow[lo] as i32) | (((shadow[hi] & 0x0F) as i32) << 8)
}

/// Per-feature modulation state, owned by the playback struct.
pub struct CvMod {
    /// EMA accumulators for the 3 CV inputs (raw counts).
    slew: [i32; 3],
    /// Per-CV jack-patched state from the previous frame (for edge detect).
    prev_patched: [bool; 3],
    /// CV3 mute zone carried across frames (hysteresis memory).
    prev_zone: u8,
    /// Last value this module emitted (or believes the chip holds) per register;
    /// UNKNOWN forces a write. Used for change-detection.
    last_emit: [i16; SID_REGS],
}

impl CvMod {
    pub const fn new() -> Self {
        CvMod {
            slew: [0; 3],
            prev_patched: [false; 3],
            prev_zone: 0,
            last_emit: [UNKNOWN; SID_REGS],
        }
    }

    /// Reset all state (call on tune (re)load, alongside zeroing the shadow).
    pub fn reset(&mut self) {
        self.slew = [0; 3];
        self.prev_patched = [false; 3];
        self.prev_zone = 0;
        self.last_emit = [UNKNOWN; SID_REGS];
    }

    /// Produce this frame's override writes. Stub — filled in by later tasks.
    pub fn compute(&mut self, _shadow: &SidShadow, _dirty: u32,
                   _cv_raw: [i32; 3], _jacks: u8) -> WriteList {
        WriteList::new()
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib cvmod`
Expected: PASS (`base_extractors`, `new_is_idle`).

- [ ] **Step 5: Commit**

```bash
git add gateware/src/top/sid_player_sw/fw/src/cvmod.rs gateware/src/top/sid_player_sw/fw/src/lib.rs
git commit -m "feat(sid_player_sw): cvmod module scaffold + base extractors"
```

---

### Task 2: CV1 — filter cutoff offset (bipolar, slew, clamp, change-detect)

**Files:**
- Modify: `gateware/src/top/sid_player_sw/fw/src/cvmod.rs`
- Test: in-module tests

**Interfaces:**
- Consumes: `CvMod`, `cutoff_base`, constants from Task 1.
- Produces: a private `emit` helper and the cutoff branch of `compute`.
  - `fn emit(&mut self, out: &mut WriteList, shadow: &SidShadow, dirty: u32, reg: usize, val: u8)` — pushes `(reg as u8, val)` iff the chip's current value (`shadow[reg]` when `dirty & 1<<reg`, else `last_emit[reg]`) differs from `val`; always updates `last_emit[reg]`.
  - `fn slew_update(&mut self, i: usize, raw: i32, rising: bool)` — snaps on a rising patch edge, else 1-pole EMA.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn cv1_cutoff_offset_and_clamp() {
    let mut cv = CvMod::new();
    let mut s: SidShadow = [0; SID_REGS];
    // tune base cutoff = 1000
    let base = 1000;
    s[FC_LO] = (base & 7) as u8;
    s[FC_HI] = (base >> 3) as u8;

    // CV1 patched (jack bit 0), +5V => +~2050 -> clamps at 2047.
    // First frame snaps the slew to the raw value (rising edge).
    let out = cv.compute(&s, 0, [5 * COUNTS_PER_VOLT, 0, 0], 0b001);
    // Expect two writes for cutoff regs reconstructing the clamped value 2047.
    let mut got = 0i32;
    for &(r, v) in out.iter() {
        if r as usize == FC_LO { got |= (v as i32) & 7; }
        if r as usize == FC_HI { got |= (v as i32) << 3; }
    }
    assert_eq!(got, 2047, "cutoff should clamp to 11-bit max");

    // Negative CV pushes below base; -5V => 1000-2050 -> clamp 0.
    let mut cv2 = CvMod::new();
    let out = cv2.compute(&s, 0, [-5 * COUNTS_PER_VOLT, 0, 0], 0b001);
    let mut got = 0i32;
    for &(r, v) in out.iter() {
        if r as usize == FC_LO { got |= (v as i32) & 7; }
        if r as usize == FC_HI { got |= (v as i32) << 3; }
    }
    assert_eq!(got, 0, "cutoff should clamp to 0");
}

#[test]
fn cv1_zero_volts_is_passthrough() {
    let mut cv = CvMod::new();
    let mut s: SidShadow = [0; SID_REGS];
    let base = 1234;
    s[FC_LO] = (base & 7) as u8;
    s[FC_HI] = (base >> 3) as u8;
    // 0V, patched: offset 0 -> final == base. The tune already wrote these regs
    // this frame (dirty), so the chip holds base -> compute emits nothing.
    let dirty = (1 << FC_LO) | (1 << FC_HI);
    let out = cv.compute(&s, dirty, [0, 0, 0], 0b001);
    assert!(out.is_empty(), "0V on a dirty cutoff = no extra writes");
}

#[test]
fn cv1_unpatched_emits_nothing() {
    let mut cv = CvMod::new();
    let s: SidShadow = [0; SID_REGS];
    let out = cv.compute(&s, 0, [5 * COUNTS_PER_VOLT, 0, 0], 0b000);
    assert!(out.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib cvmod`
Expected: FAIL — `cv1_cutoff_offset_and_clamp` etc. fail (compute is a stub returning empty).

- [ ] **Step 3: Write minimal implementation**

Add the helpers and replace the cutoff portion of `compute`:

```rust
impl CvMod {
    fn slew_update(&mut self, i: usize, raw: i32, rising: bool) -> i32 {
        if rising {
            self.slew[i] = raw; // snap for instant response on patch-in
        } else {
            self.slew[i] += (raw - self.slew[i]) >> SLEW_K;
        }
        self.slew[i]
    }

    fn emit(&mut self, out: &mut WriteList, shadow: &SidShadow,
            dirty: u32, reg: usize, val: u8) {
        // The chip currently holds: the tune's base (shadow) if the tune wrote
        // this reg this frame, else whatever we last emitted.
        let current = if dirty & (1 << reg) != 0 {
            shadow[reg] as i16
        } else {
            self.last_emit[reg]
        };
        if current != val as i16 {
            let _ = out.push((reg as u8, val)); // capacity proven by MAX_WRITES
        }
        self.last_emit[reg] = val as i16;
    }
}
```

Now flesh out `compute` (cutoff branch only for this task; PW/mute come next):

```rust
pub fn compute(&mut self, shadow: &SidShadow, dirty: u32,
               cv_raw: [i32; 3], jacks: u8) -> WriteList {
    let mut out = WriteList::new();
    let patched = [jacks & 1 != 0, jacks & 2 != 0, jacks & 4 != 0];

    // --- CV1: filter cutoff offset (bipolar) ---
    if patched[0] {
        let rising = !self.prev_patched[0];
        let cv = self.slew_update(0, cv_raw[0], rising);
        let off = cv * CUTOFF_CTS_PER_V / COUNTS_PER_VOLT;
        let fin = (cutoff_base(shadow) + off).clamp(0, 2047);
        self.emit(&mut out, shadow, dirty, FC_LO, (fin & 7) as u8);
        self.emit(&mut out, shadow, dirty, FC_HI, (fin >> 3) as u8);
    } else if self.prev_patched[0] {
        // falling edge: restore base (handled fully in Task 5)
    }

    self.prev_patched[0] = patched[0];
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib cvmod`
Expected: PASS (Task 1 + Task 2 tests).

- [ ] **Step 5: Commit**

```bash
git add gateware/src/top/sid_player_sw/fw/src/cvmod.rs
git commit -m "feat(sid_player_sw): CV1 filter-cutoff offset"
```

---

### Task 3: CV2 — pulse-width offset, all 3 voices

**Files:**
- Modify: `gateware/src/top/sid_player_sw/fw/src/cvmod.rs`
- Test: in-module tests

**Interfaces:**
- Consumes: `CvMod`, `emit`, `slew_update`, `pw_base`, `PW_REGS`, `PW_CTS_PER_V` from earlier tasks.
- Produces: the CV2 branch of `compute` (writes the 6 PW registers).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn cv2_pw_offset_all_voices() {
    let mut cv = CvMod::new();
    let mut s: SidShadow = [0; SID_REGS];
    // distinct PW bases per voice
    for (v, &(lo, hi)) in PW_REGS.iter().enumerate() {
        let base = 1000 + (v as i32) * 100; // 1000,1100,1200
        s[lo] = (base & 0xFF) as u8;
        s[hi] = ((base >> 8) & 0x0F) as u8;
    }
    // CV2 patched (jack bit 1), +2.5V => +1000 PW (2.5*400).
    let out = cv.compute(&s, 0, [0, 2500 * COUNTS_PER_VOLT / 1000, 0], 0b010);

    for (v, &(lo, hi)) in PW_REGS.iter().enumerate() {
        let mut got = 0i32;
        for &(r, val) in out.iter() {
            if r as usize == lo { got |= val as i32; }
            if r as usize == hi { got |= ((val as i32) & 0x0F) << 8; }
        }
        let want = (1000 + (v as i32) * 100 + 1000).min(4095);
        assert_eq!(got, want, "voice {v} PW offset");
    }
}

#[test]
fn cv2_clamps_to_12bit() {
    let mut cv = CvMod::new();
    let mut s: SidShadow = [0; SID_REGS];
    for &(lo, hi) in PW_REGS.iter() { s[lo] = 0xF0; s[hi] = 0x0F; } // base 4080
    let out = cv.compute(&s, 0, [0, 5 * COUNTS_PER_VOLT, 0], 0b010); // +2000 -> clamp 4095
    for &(lo, hi) in PW_REGS.iter() {
        let mut got = 0i32;
        for &(r, val) in out.iter() {
            if r as usize == lo { got |= val as i32; }
            if r as usize == hi { got |= ((val as i32) & 0x0F) << 8; }
        }
        assert_eq!(got, 4095);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib cvmod`
Expected: FAIL — `cv2_pw_offset_all_voices`, `cv2_clamps_to_12bit` (no CV2 branch yet).

- [ ] **Step 3: Write minimal implementation**

Insert the CV2 branch into `compute`, after the CV1 block and before `self.prev_patched[0] = …` is updated (move the `prev_patched` updates to the end for all three). Replace the tail of `compute`:

```rust
    // --- CV2: pulse-width offset (bipolar), all 3 voices ---
    if patched[1] {
        let rising = !self.prev_patched[1];
        let cv = self.slew_update(1, cv_raw[1], rising);
        let off = cv * PW_CTS_PER_V / COUNTS_PER_VOLT;
        for v in 0..3 {
            let (lo, hi) = PW_REGS[v];
            let fin = (pw_base(shadow, v) + off).clamp(0, 4095);
            self.emit(&mut out, shadow, dirty, lo, (fin & 0xFF) as u8);
            self.emit(&mut out, shadow, dirty, hi, ((fin >> 8) & 0x0F) as u8);
        }
    } else if self.prev_patched[1] {
        // falling edge: restore (Task 5)
    }

    self.prev_patched[0] = patched[0];
    self.prev_patched[1] = patched[1];
    out
}
```

(Remove the now-duplicated `self.prev_patched[0] = patched[0];` from the CV1 tail.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib cvmod`
Expected: PASS (all tests so far).

- [ ] **Step 5: Commit**

```bash
git add gateware/src/top/sid_player_sw/fw/src/cvmod.rs
git commit -m "feat(sid_player_sw): CV2 pulse-width offset (all voices)"
```

---

### Task 4: CV3 — progressive voice mute with hysteresis

**Files:**
- Modify: `gateware/src/top/sid_player_sw/fw/src/cvmod.rs`
- Test: in-module tests

**Interfaces:**
- Consumes: `CvMod`, `emit`, `slew_update`, `CTRL_REGS`, `ZONE_WIDTH`, `ZONE_HYST` from earlier tasks.
- Produces: `fn zone_with_hyst(v: i32, prev: u8) -> u8` (free function) and the CV3 branch of `compute`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn cv3_zone_hysteresis() {
    // boundaries at 5000/10000/15000 counts, deadband +/-480.
    // rising past a boundary needs +HYST; falling needs -HYST.
    assert_eq!(zone_with_hyst(0, 0), 0);
    assert_eq!(zone_with_hyst(ZONE_WIDTH - 1, 0), 0);          // just below b1
    assert_eq!(zone_with_hyst(ZONE_WIDTH + ZONE_HYST, 0), 1);  // crosses up
    assert_eq!(zone_with_hyst(ZONE_WIDTH + ZONE_HYST - 1, 0), 0); // inside deadband, stays
    // already in zone 1, drop only when below b1 - HYST
    assert_eq!(zone_with_hyst(ZONE_WIDTH - ZONE_HYST + 1, 1), 1); // inside deadband, stays
    assert_eq!(zone_with_hyst(ZONE_WIDTH - ZONE_HYST - 1, 1), 0); // drops
    // clamps and negatives
    assert_eq!(zone_with_hyst(-9999, 2), 0);
    assert_eq!(zone_with_hyst(3 * ZONE_WIDTH + ZONE_HYST + 1, 0), 3);
    assert_eq!(zone_with_hyst(99 * ZONE_WIDTH, 0), 3);
}

#[test]
fn cv3_mutes_high_voices_first() {
    // zone N mutes the N highest voice indices (V3=idx2 first).
    let mut cv = CvMod::new();
    let mut s: SidShadow = [0; SID_REGS];
    // each voice ctrl: pulse waveform (0x40) + gate (0x01) = 0x41
    for &c in CTRL_REGS.iter() { s[c] = 0x41; }

    // ~1.25*1 + margin volts -> zone 1 -> mute idx2 only.
    let v = ZONE_WIDTH + ZONE_HYST + 1;
    let out = cv.compute(&s, 0, [0, 0, v], 0b100);
    // idx2 ctrl forced to low nibble (0x01); idx0/idx1 untouched (== shadow,
    // and dirty=0 with last_emit unknown -> emitted as shadow, harmless).
    let mut muted2 = None;
    for &(r, val) in out.iter() {
        if r as usize == CTRL_REGS[2] { muted2 = Some(val); }
    }
    assert_eq!(muted2, Some(0x01), "voice 3 (idx2) waveform bits cleared, gate kept");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib cvmod`
Expected: FAIL — `zone_with_hyst` not defined, CV3 branch missing.

- [ ] **Step 3: Write minimal implementation**

Add the free function near the base extractors:

```rust
/// CV3 mute zone (0..3) from slewed CV counts, with hysteresis around the
/// 5000/10000/15000-count boundaries so a noisy input doesn't chatter.
pub fn zone_with_hyst(v: i32, prev: u8) -> u8 {
    let mut z = prev as i32;
    while z < 3 && v >= (z + 1) * ZONE_WIDTH + ZONE_HYST { z += 1; }
    while z > 0 && v <  z * ZONE_WIDTH - ZONE_HYST { z -= 1; }
    z as u8
}
```

Add the CV3 branch to `compute`, before the `prev_patched` tail, and add the zone/patched bookkeeping to the tail:

```rust
    // --- CV3: progressive voice mute (unipolar, 4 zones, hysteresis) ---
    if patched[2] {
        let rising = !self.prev_patched[2];
        let cv = self.slew_update(2, cv_raw[2], rising);
        let zone = zone_with_hyst(cv, self.prev_zone);
        self.prev_zone = zone;
        for v in 0..3 {
            let ctrl = CTRL_REGS[v];
            let muted = v >= (3 - zone as usize); // zone N mutes the N highest voices
            let val = if muted { shadow[ctrl] & 0x0F } else { shadow[ctrl] };
            self.emit(&mut out, shadow, dirty, ctrl, val);
        }
    } else if self.prev_patched[2] {
        // falling edge: restore (Task 5)
        self.prev_zone = 0;
    }

    self.prev_patched[0] = patched[0];
    self.prev_patched[1] = patched[1];
    self.prev_patched[2] = patched[2];
    out
}
```

(Remove the duplicate `prev_patched[0]/[1]` assignments left from Task 3's tail; keep only this final block.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib cvmod`
Expected: PASS (all tests).

- [ ] **Step 5: Commit**

```bash
git add gateware/src/top/sid_player_sw/fw/src/cvmod.rs
git commit -m "feat(sid_player_sw): CV3 progressive voice mute"
```

---

### Task 5: Unpatch restore + change-detection across frames

**Files:**
- Modify: `gateware/src/top/sid_player_sw/fw/src/cvmod.rs`
- Test: in-module tests

**Interfaces:**
- Consumes: `CvMod`, `emit`, reg tables from earlier tasks.
- Produces: a `fn restore(&mut self, out, shadow, dirty, regs: &[usize])` helper and wired-up falling-edge restores in all three CV branches.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn unpatch_restores_base_once() {
    let mut cv = CvMod::new();
    let mut s: SidShadow = [0; SID_REGS];
    s[FC_LO] = 2; s[FC_HI] = 100; // base cutoff
    // frame 1: patched, +5V -> cutoff overridden.
    let _ = cv.compute(&s, 0, [5 * COUNTS_PER_VOLT, 0, 0], 0b001);
    // frame 2: unpatched -> one-shot restore of FC_LO/FC_HI to base.
    let out = cv.compute(&s, 0, [5 * COUNTS_PER_VOLT, 0, 0], 0b000);
    let mut lo = None; let mut hi = None;
    for &(r, v) in out.iter() {
        if r as usize == FC_LO { lo = Some(v); }
        if r as usize == FC_HI { hi = Some(v); }
    }
    assert_eq!(lo, Some(2)); assert_eq!(hi, Some(100));
    // frame 3: still unpatched -> nothing.
    let out = cv.compute(&s, 0, [5 * COUNTS_PER_VOLT, 0, 0], 0b000);
    assert!(out.is_empty(), "restore is one-shot");
}

#[test]
fn unpatch_unmutes_voices() {
    let mut cv = CvMod::new();
    let mut s: SidShadow = [0; SID_REGS];
    for &c in CTRL_REGS.iter() { s[c] = 0x41; }
    // patched, zone 3 -> all muted.
    let _ = cv.compute(&s, 0, [0, 0, 3 * ZONE_WIDTH + ZONE_HYST + 1], 0b100);
    // unpatch -> all ctrl restored to 0x41.
    let out = cv.compute(&s, 0, [0, 0, 0], 0b000);
    for &c in CTRL_REGS.iter() {
        let v = out.iter().find(|&&(r, _)| r as usize == c).map(|&(_, v)| v);
        assert_eq!(v, Some(0x41), "voice ctrl {c:#x} restored");
    }
}

#[test]
fn static_cv_costs_no_writes_after_first() {
    let mut cv = CvMod::new();
    let mut s: SidShadow = [0; SID_REGS];
    s[FC_LO] = 0; s[FC_HI] = 50;
    let cvv = 2 * COUNTS_PER_VOLT;
    // frame 1: emits the override.
    let out1 = cv.compute(&s, 0, [cvv, 0, 0], 0b001);
    assert!(!out1.is_empty());
    // frames 2+: same CV, tune did NOT rewrite cutoff (dirty=0) -> 0 writes.
    let out2 = cv.compute(&s, 0, [cvv, 0, 0], 0b001);
    assert!(out2.is_empty(), "steady CV + clean regs = no rewrites");
    // but if the tune rewrites cutoff this frame (dirty), re-assert.
    let dirty = (1 << FC_LO) | (1 << FC_HI);
    let out3 = cv.compute(&s, dirty, [cvv, 0, 0], 0b001);
    assert!(!out3.is_empty(), "tune clobbered cutoff -> re-assert override");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib cvmod`
Expected: FAIL — `unpatch_restores_base_once`, `unpatch_unmutes_voices` (falling-edge branches are empty); `static_cv_costs_no_writes_after_first` may already pass.

- [ ] **Step 3: Write minimal implementation**

Add the restore helper:

```rust
impl CvMod {
    /// One-shot restore of `regs` to their tune base (shadow). After restoring,
    /// mark each as UNKNOWN so a future re-patch re-forces the override.
    fn restore(&mut self, out: &mut WriteList, shadow: &SidShadow,
               dirty: u32, regs: &[usize]) {
        for &r in regs {
            self.emit(out, shadow, dirty, r, shadow[r]);
            self.last_emit[r] = UNKNOWN;
        }
    }
}
```

Fill in the three falling-edge branches in `compute`:

```rust
    // CV1 else-branch:
    } else if self.prev_patched[0] {
        self.restore(&mut out, shadow, dirty, &[FC_LO, FC_HI]);
    }

    // CV2 else-branch:
    } else if self.prev_patched[1] {
        let regs = [PW_REGS[0].0, PW_REGS[0].1, PW_REGS[1].0, PW_REGS[1].1,
                    PW_REGS[2].0, PW_REGS[2].1];
        self.restore(&mut out, shadow, dirty, &regs);
    }

    // CV3 else-branch:
    } else if self.prev_patched[2] {
        self.prev_zone = 0;
        self.restore(&mut out, shadow, dirty, &CTRL_REGS);
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: PASS — all `cvmod` tests plus the pre-existing `player` tests.

- [ ] **Step 5: Commit**

```bash
git add gateware/src/top/sid_player_sw/fw/src/cvmod.rs
git commit -m "feat(sid_player_sw): unpatch restore + cross-frame change-detection"
```

---

### Task 6: Wire `cvmod` into the playback ISR

**Files:**
- Modify: `gateware/src/top/sid_player_sw/fw/src/main.rs`
  - `struct Playback` (~line 149): add `shadow: cvmod::SidShadow` and `cv: cvmod::CvMod`.
  - the two `Playback { … }` constructions (search `Playback {`): init `shadow: [0; cvmod::SID_REGS]`, `cv: cvmod::CvMod::new()`.
  - `fn play_tick` (~line 170): build dirty mask + mirror shadow while draining, then read CSRs + call `compute` + drain overrides.
  - `fn reload_tune` (~line 218): after `sid_reset()`, zero the shadow and `cv.reset()`.

**Interfaces:**
- Consumes: `cvmod::{CvMod, SidShadow, SID_REGS}`, `cvmod::CvMod::compute`, `sid_write_bp`, `PMOD0_PERIPH` PAC accessors (`sample_i0/1/2().read().bits()`, `jack().read().bits()`).
- Produces: nothing (top-level wiring; verified by build + on-hardware).

- [ ] **Step 1: Add the module use and Playback fields**

At the top of `main.rs` where other `use tiliqua_fw::…` / crate modules are imported, ensure `cvmod` is reachable (it's a sibling module of `player`; use the same path the code uses for `player`, e.g. `use crate::cvmod;` or refer to it as `cvmod::`). Then:

```rust
struct Playback {
    cpu: PlayerCpu,
    play_addr: u16,
    paused: bool,
    shadow: cvmod::SidShadow,
    cv: cvmod::CvMod,
}
```

Update every `Playback { … }` literal to add:

```rust
        shadow: [0; cvmod::SID_REGS],
        cv: cvmod::CvMod::new(),
```

- [ ] **Step 2: Verify it builds (fields added, not yet used)**

Run: `cd gateware && pdm sid_player_sw build --fw-only`
Expected: builds OK (a `field is never read` warning for `shadow`/`cv` is fine at this step).

- [ ] **Step 3: Mirror shadow + build dirty mask + call compute in `play_tick`**

Replace the drain loop inside `play_tick` (the `for w in pb.cpu.memory.writes.iter() { sid_write_bp(w.reg, w.val); }` block) with:

```rust
                // Drain this frame's tune writes to the SID (backpressured),
                // mirroring each into the shadow and recording which registers
                // the tune touched this frame (dirty mask) for CV change-detect.
                let mut dirty: u32 = 0;
                for w in pb.cpu.memory.writes.iter() {
                    sid_write_bp(w.reg, w.val);
                    pb.shadow[(w.reg & 0x1F) as usize] = w.val;
                    dirty |= 1 << (w.reg & 0x1F);
                }
                // Read the 3 CV inputs + jack-detect, then apply CV modulation
                // on top of the tune's writes (override wins until the next
                // tune write to the same register).
                let p = unsafe { pac::Peripherals::steal() };
                let cv_raw = [
                    p.PMOD0_PERIPH.sample_i0().read().bits() as i16 as i32,
                    p.PMOD0_PERIPH.sample_i1().read().bits() as i16 as i32,
                    p.PMOD0_PERIPH.sample_i2().read().bits() as i16 as i32,
                ];
                let jacks = p.PMOD0_PERIPH.jack().read().bits();
                let writes = pb.cv.compute(&pb.shadow, dirty, cv_raw, jacks);
                for (reg, val) in writes.iter() {
                    sid_write_bp(*reg, *val);
                }
```

- [ ] **Step 4: Reset shadow + CvMod on tune (re)load**

In `reload_tune`, immediately after `sid_reset();` (which already zeroes the chip), add:

```rust
        pb.shadow = [0; cvmod::SID_REGS];
        pb.cv.reset();
```

- [ ] **Step 5: Build firmware and run host tests**

Run:
```bash
cd gateware && pdm sid_player_sw build --fw-only
cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib
```
Expected: firmware builds with no warnings about unused `shadow`/`cv`; all host tests pass.

- [ ] **Step 6: Commit**

```bash
git add gateware/src/top/sid_player_sw/fw/src/main.rs
git commit -m "feat(sid_player_sw): apply CV modulation in the play_tick ISR"
```

---

### Task 7: Hardware verification + 6581/8580 mute check

**Files:** none (verification only). This task gates the open implementation item from the spec (zero-waveform mute may leak on 6581).

**Interfaces:** Consumes the full firmware from Task 6.

- [ ] **Step 1: Flash and bench-test on hardware**

Build a full bitstream (or reuse one) and flash, per the project conventions:
```bash
cd gateware && pdm sid_player_sw build           # full build (~4-5 min)
pdm run flash archive build/sid-player-sw-r5/*.tar.gz
```
Then, with a tune playing:
- Patch an LFO → input jack 0 (CV1): confirm the filter cutoff sweeps; unplug → cutoff returns to the tune's value.
- Patch an LFO → input jack 1 (CV2): confirm audible PWM on pulse-waveform voices; unplug → restores.
- Patch a slow ramp/envelope → input jack 2 (CV3): confirm voices drop V3 → V2 → V1 as voltage rises, no chatter at the zone edges; unplug → all voices return.

- [ ] **Step 2: 6581 mute-leak check**

On a **6581** build, with CV3 muting a voice, listen for residual oscillator output (the 6581's floating waveform DAC can leak when no waveform bit is set). If the muted voice is not silent:
- In `cvmod.rs`, change the mute value from clearing the waveform bits to **forcing the TEST bit** (bit 3) on the muted voice: replace `shadow[ctrl] & 0x0F` with `(shadow[ctrl] & 0x0F) | 0x08` in the CV3 branch (TEST holds the oscillator at DC = silence). Re-run the `cvmod` tests (update `cv3_mutes_high_voices_first` to expect `0x09`) and re-flash.
- If 8580 is silent with the plain mask and only 6581 leaks, gate the `| 0x08` on the build model (read `SID_PERIPH.build_model`) — but prefer the unconditional TEST-bit form if it sounds clean on both, for simplicity.

- [ ] **Step 3: Fast-tune real-time guard**

Play a fast CIA tune (e.g. *A Drop of Blue*) with all three CVs patched and modulating. Confirm there are no **new** dropped/stuttered notes vs the same tune unpatched (audio-priority invariant). If patched playback regresses, the per-frame write count is the suspect — confirm change-detection is holding (static CVs should add ~0 writes).

- [ ] **Step 4: Commit any mute-mechanism change**

```bash
git add gateware/src/top/sid_player_sw/fw/src/cvmod.rs
git commit -m "fix(sid_player_sw): CV3 mute via TEST bit (6581 leak)"   # only if Step 2 required it
```

---

## Self-Review

**Spec coverage:**
- CV1 cutoff offset → Task 2. ✓
- CV2 PW offset, all voices → Task 3. ✓
- CV3 progressive mute + hysteresis → Task 4. ✓
- Offset/modulate semantics + clamping → Tasks 2–3. ✓
- Auto on patch-detect (jack CSR) + unpatched passthrough → Tasks 2–5 (patch-gating) + Task 6 (read `jack`). ✓
- Bipolar CV1/CV2, unipolar CV3 → Tasks 2/3 (signed offset) / Task 4 (zones from 0V). ✓
- Slew/EMA de-zipper → `slew_update` (Task 2). ✓
- Change-detection + dirty mask → `emit` (Task 2) + Task 6 mask build; cross-frame test Task 5. ✓
- Unpatch one-shot restore → Task 5. ✓
- Tune-reload reset of shadow + CvMod → Task 6 Step 4. ✓
- `sid_shadow` mirror via the drain loop → Task 6 Step 3. ✓
- ISR injection after the tune-write drain, via `sid_write_bp` → Task 6 Step 3. ✓
- Host unit tests for all mappings → Tasks 1–5. ✓
- 6581/8580 mute validation + TEST-bit fallback → Task 7. ✓
- HW verification + fast-tune guard → Task 7. ✓
- No gateware/PAC/UI → respected (only `cvmod.rs`, `lib.rs`, `main.rs`). ✓

**Placeholder scan:** No TBD/TODO; every code step shows full code. The CV3 branch and `prev_patched` tail are progressively edited across Tasks 2–5 — each task states exactly what to add/remove. ✓

**Type consistency:** `compute(&SidShadow, u32, [i32;3], u8) -> WriteList` is used identically in every task and in Task 6's call site. `emit`, `slew_update`, `restore`, `zone_with_hyst`, `cutoff_base`, `pw_base` signatures match across tasks. `WriteList = heapless::Vec<(u8,u8),16>`; the ISR iterates `(reg, val)` as `&(u8,u8)` and calls `sid_write_bp(*reg, *val)`. ✓
