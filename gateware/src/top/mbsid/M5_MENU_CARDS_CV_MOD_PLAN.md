# M5 Menu Cards + CV Modulation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `M5_MENU_CARDS_CV_MOD.md` — a three-card mbsid menu (Main / CV Mod / Patch Edit), CV1–4 assignable to MBSID patch-layer modulation targets, curated on-device Lead patch editing with save-to-user-bank, and persisted settings.

**Architecture:** Five thin `extern "C"` shim additions expose the upstream engine's `knobSet`/`parSet`/`sysexSetParameter` layers; three new host-pure firmware modules (`params.rs`, `cv.rs`, `settings_store.rs`) hold all tables and math; `menu.rs` grows a card layer; `main.rs` wires CV sampling into the 1 kHz ISR and edits/persistence into the main loop. Zero gateware changes, zero CSR changes, zero upstream C++ edits.

**Tech Stack:** Rust `no_std` firmware (`fw/`), C++14 shim (`fw/csrc/mbsid_shim.cpp`), host oracle (g++, `host_oracle/`), embedded-graphics menu drawing.

## Global Constraints

- Repo root for all paths below: `gateware/src/top/mbsid/` unless the path starts with `gateware/`.
- Never edit vendored code under `mios32/` (GPL, gitignored, pinned).
- Shim ABI rule: any change to `mbsid_shim.h` signatures updates the Rust FFI (`fw/src/mbsid_sys.rs`) **and both oracle drivers** in the same commit. (This milestone only *adds* entry points.)
- No f32 math in the Timer0 ISR path; all CV mapping is integer.
- No new 512-byte buffers in firmware (mainram budget); new tables are `const` (flash).
- Host firmware tests: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib`. Ignore rust-analyzer/LSP errors on `fw/` (no_std false positives — root CLAUDE.md).
- The fw crate is `#![no_std]` **including its test modules** (no `extern crate std` anywhere): test code uses arrays, slices, and `heapless` — never `std::vec::Vec` or `vec!`.
- Oracle gate: `host_oracle/run_oracle.sh` must end `exit 0` with every `OK:` line printed (28/28 + new checks). Requires `./fetch-mios32.sh` to have been run.
- Firmware relink: `cd gateware && pdm mbsid build --fw-only` (expected to end with `missing top.bit` **after** the ELF builds — that error is success for fw-only; a Rust compile error is failure).
- No `--pac-only` needed anywhere in this milestone (no CSR changes).
- Commit messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: Shim entry points + Rust FFI

**Files:**
- Modify: `fw/csrc/mbsid_shim.h` (after the M4 block)
- Modify: `fw/csrc/mbsid_shim.cpp` (after `mbsid_sysex_timeout`)
- Modify: `fw/src/mbsid_sys.rs`

**Interfaces:**
- Consumes: upstream `MbSid::sysexSetParameter(u16, u8)`, `MbSidSe::knobSet/parSet` (virtual, `MbSidSe.h:130/134`), `env.mbSid[0].mbSidPatch.body.ALL` (same access path as `mbsid_current_patch_raw`).
- Produces (used by Tasks 2, 8, 9):
  - C: `void mbsid_knob_set(uint8_t, uint8_t)`, `void mbsid_par_set(uint8_t, uint16_t)`, `int mbsid_sysex_param(uint16_t, uint8_t)`, `uint8_t mbsid_patch_byte(uint16_t)`, `uint8_t mbsid_current_engine(void)`
  - Rust: `mbsid_sys::{knob_set(u8,u8), par_set(u8,u16), sysex_param(u16,u8)->bool, patch_byte(u16)->u8, current_engine()->u8}` — host stubs return `false`/`0`.

- [ ] **Step 1: Declare the ABI in `mbsid_shim.h`**

Append before `#ifdef __cplusplus }`:

```c
/* M5: modulation + on-device patch editing (M5_MENU_CARDS_CV_MOD.md §4). */
void    mbsid_knob_set(uint8_t knob, uint8_t value);    /* engine knob 0..7 (K1..K5,V,P,A) */
void    mbsid_par_set(uint8_t par, uint16_t value16);   /* parSet common block, sidlr=3, 16-bit scaled */
int     mbsid_sysex_param(uint16_t addr, uint8_t data); /* patch body write + live update; 1 = ok */
uint8_t mbsid_patch_byte(uint16_t addr);                /* raw patch body read (display) */
uint8_t mbsid_current_engine(void);                     /* patch body[0x10]: 0=Lead,1=Bass,2=Drum,3=Multi */
```

- [ ] **Step 2: Implement in `mbsid_shim.cpp`**

Append after `mbsid_sysex_timeout`:

```cpp
// M5: modulation + on-device patch editing. knobSet/parSet dispatch through
// the current engine's virtual (safe empty default on MbSidSe); parSet's
// sidlr=3 targets both SIDs, scaleFrom16bit=true takes a full-range u16 —
// same call shape as the NRPN path. sysexSetParameter writes the patch BODY
// byte and live-updates the engine, so mbsid_current_patch_raw captures edits.
extern "C" void mbsid_knob_set(uint8_t knob, uint8_t value) {
    env.mbSid[0].currentMbSidSePtr->knobSet(knob & 7, value);
}
extern "C" void mbsid_par_set(uint8_t par, uint16_t value16) {
    env.mbSid[0].currentMbSidSePtr->parSet(par, value16, /*sidlr*/3, /*ins*/0,
                                           /*scaleFrom16bit*/true);
}
extern "C" int mbsid_sysex_param(uint16_t addr, uint8_t data) {
    return env.mbSid[0].sysexSetParameter(addr, data) ? 1 : 0;
}
extern "C" uint8_t mbsid_patch_byte(uint16_t addr) {
    return env.mbSid[0].mbSidPatch.body.ALL[addr & 0x1FF];
}
extern "C" uint8_t mbsid_current_engine(void) {
    return env.mbSid[0].mbSidPatch.body.ALL[0x10];
}
```

- [ ] **Step 3: Mirror in `fw/src/mbsid_sys.rs`**

Add to the `extern "C"` block (riscv32 section):

```rust
    fn mbsid_knob_set(knob: u8, value: u8);
    fn mbsid_par_set(par: u8, value16: u16);
    fn mbsid_sysex_param(addr: u16, data: u8) -> i32;
    fn mbsid_patch_byte(addr: u16) -> u8;
    fn mbsid_current_engine() -> u8;
```

Add safe wrappers (riscv32) after `sysex_timeout`:

```rust
#[cfg(target_arch = "riscv32")]
pub fn knob_set(knob: u8, value: u8) { unsafe { mbsid_knob_set(knob, value) } }

#[cfg(target_arch = "riscv32")]
pub fn par_set(par: u8, value16: u16) { unsafe { mbsid_par_set(par, value16) } }

#[cfg(target_arch = "riscv32")]
pub fn sysex_param(addr: u16, data: u8) -> bool { unsafe { mbsid_sysex_param(addr, data) != 0 } }

#[cfg(target_arch = "riscv32")]
pub fn patch_byte(addr: u16) -> u8 { unsafe { mbsid_patch_byte(addr) } }

#[cfg(target_arch = "riscv32")]
pub fn current_engine() -> u8 { unsafe { mbsid_current_engine() } }
```

Add host stubs after `sysex_timeout`'s host stub:

```rust
#[cfg(not(target_arch = "riscv32"))]
pub fn knob_set(_knob: u8, _value: u8) {}

#[cfg(not(target_arch = "riscv32"))]
pub fn par_set(_par: u8, _value16: u16) {}

#[cfg(not(target_arch = "riscv32"))]
pub fn sysex_param(_addr: u16, _data: u8) -> bool { false }

#[cfg(not(target_arch = "riscv32"))]
pub fn patch_byte(_addr: u16) -> u8 { 0 }

#[cfg(not(target_arch = "riscv32"))]
pub fn current_engine() -> u8 { 0 } // Lead, matches bank_patch_info stub
```

- [ ] **Step 4: Verify host tests still pass**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: all 41 tests PASS (no new tests yet; this proves the module still compiles both cfg arms).

- [ ] **Step 5: Verify shim compiles in the oracle build**

Run: `cd .. && host_oracle/run_oracle.sh 2>&1 | tail -5`
Expected: `OK: no-crash sweep`, exit 0 (the script compiles `mbsid_shim.cpp`; a C++ error fails here).

- [ ] **Step 6: Commit**

```bash
git add fw/csrc/mbsid_shim.h fw/csrc/mbsid_shim.cpp fw/src/mbsid_sys.rs
git commit -m "feat(mbsid): M5 shim entry points — knob/par modulation, patch-byte edit/read"
```

---

### Task 2: Oracle coverage for the new entry points

**Files:**
- Modify: `host_oracle/seq.h` (SeqEvent kinds + parser + dispatch)
- Modify: `host_oracle/shim_driver.cpp` (ShimBackend)
- Modify: `host_oracle/oracle.cpp` (OracleBackend, around lines 17–46)
- Create: `host_oracle/sequences/seq_lead_knobs.txt`
- Create: `host_oracle/param_check.cpp`
- Modify: `host_oracle/run_oracle.sh` (build + run the param check)

**Interfaces:**
- Consumes: Task 1's C ABI.
- Produces: sequence commands `kn <knob> <val8>` and `pr <par> <val16>`; a `param_check` binary asserting the save-captures-edits invariant.

- [ ] **Step 1: Add `kn` / `pr` to `seq.h`**

In `SeqEvent`, extend the enum: `enum Kind { PATCH, PC, ON, OFF, CC, BEND, AT, CH, SYX, SYXPC, KN, PR, END };` and document both commands in the header comment (`<t_ms> kn <knob 0..7> <val 0..255>`, `<t_ms> pr <par> <val16 0..65535>`).

In `seq_parse`, add before the `end` line:

```cpp
        else if (!strcmp(ev, "kn"))    e.kind = SeqEvent::KN;
        else if (!strcmp(ev, "pr"))    e.kind = SeqEvent::PR;
```

In `run_sequence`'s switch, add:

```cpp
            case SeqEvent::KN:    be.knob(e.a, e.b);             break;
            case SeqEvent::PR:    be.par(e.a, e.b);              break;
```

Extend the Backend concept comment with `void knob(int k, int v);` and `void par(int par, int val16);`.

- [ ] **Step 2: Implement in both backends**

`shim_driver.cpp`, inside `ShimBackend`:

```cpp
    void knob(int k, int v)   { mbsid_knob_set((uint8_t)k, (uint8_t)v); }
    void par(int p, int v16)  { mbsid_par_set((uint8_t)p, (uint16_t)v16); }
```

`oracle.cpp`, inside `OracleBackend` (reference side — calls the engine the way Task 1's shim claims to):

```cpp
    void knob(int k, int v)   { env.mbSid[0].currentMbSidSePtr->knobSet((u8)k, (u8)v); }
    void par(int p, int v16)  { env.mbSid[0].currentMbSidSePtr->parSet((u8)p, (u16)v16, 3, 0, true); }
```

`sweep_driver.cpp` does not use `run_sequence` — no change (verify with `grep -c run_sequence host_oracle/sweep_driver.cpp` → 0; if nonzero, add the same two methods to its backend).

- [ ] **Step 3: Write the differential sequence**

Create `host_oracle/sequences/seq_lead_knobs.txt` (the `seq_lead_*` glob in run_oracle.sh auto-runs it across 3 Lead patches × patch/pc modes — the patch-select line is prepended by the script):

```
# seq_lead_knobs.txt — M5: knob + parSet modulation while notes play.
# Exercises mbsid_knob_set / mbsid_par_set (kn/pr) against the engine reference.
10   on   60 100
50   kn   0 64
100  kn   0 192
150  kn   1 32
200  pr   4 8192
250  pr   4 49152
300  pr   5 61440
350  pr   1 32768
400  kn   0 255
500  on   67 100
550  pr   4 4096
700  off  67
900  off  60
1200 end
```

- [ ] **Step 4: Run the oracle — expect a real, non-trivial diff-clean run**

Run: `host_oracle/run_oracle.sh 2>&1 | grep -E "seq_lead_knobs|FAIL|DIFF"`
Expected: 6 lines `OK: seq_lead_knobs (patch|pc)=(0|51|94)`, no DIFF/FAIL. If all six traces are near-empty, the knob/par calls aren't reaching the engine — fix the drivers, never the trace.

- [ ] **Step 5: Write `param_check.cpp` (save-captures-edits invariant)**

```cpp
/* param_check.cpp — M5: assert mbsid_sysex_param edits land in the patch body
 * that mbsid_current_patch_raw captures (the on-device-save invariant), and
 * that out-of-range addresses are rejected. Shim-side only, no reference. */
#include <mios32.h>
#include "mbsid_shim.h"
#include <cstdio>
#include <cstring>
#include "sid_bank_preset_a.inc"

int main() {
    mbsid_init();
    if (mbsid_load_patch(sid_bank_preset_0[0]) != 0) { puts("FAIL: load"); return 1; }
    // Edit: volume (0x52), filter cutoff_l L (0x55), OSC1 waveform (0x61).
    struct { uint16_t addr; uint8_t val; } edits[] =
        {{0x52, 0x0A}, {0x55, 0x33}, {0x61, 0x04}};
    for (auto &e : edits)
        if (!mbsid_sysex_param(e.addr, e.val)) { printf("FAIL: write %03x\n", e.addr); return 1; }
    for (int i = 0; i < 5; ++i) mbsid_tick(2); // engine must survive live update
    uint8_t buf[512];
    mbsid_current_patch_raw(buf);
    for (auto &e : edits) {
        if (buf[e.addr] != e.val) { printf("FAIL: body[%03x]=%02x want %02x\n", e.addr, buf[e.addr], e.val); return 1; }
        if (mbsid_patch_byte(e.addr) != e.val) { printf("FAIL: patch_byte %03x\n", e.addr); return 1; }
    }
    if (mbsid_current_engine() != 0) { puts("FAIL: engine byte"); return 1; }
    if (mbsid_sysex_param(512, 0)) { puts("FAIL: addr 512 accepted"); return 1; }
    puts("OK: sysex_param edits captured by current_patch_raw");
    return 0;
}
```

- [ ] **Step 6: Hook into `run_oracle.sh`**

After the sweep-driver build lines (~line 80), add the build:

```bash
# --- M5 param-edit body-capture check (shim side only) ---
"$CXX" $CXXFLAGS -c "$HERE/param_check.cpp" -o "$BUILD/param_check.o"
"$CXX" $CXXFLAGS "$BUILD/param_check.o" "$BUILD/mbsid_shim.o" "${ENGINE_OBJS[@]}" "${C_OBJS[@]}" "$BUILD/host_stubs.o" -o "$BUILD/param_check"
```

Before the final `exit $fail`, add the run:

```bash
echo "=== M5 sysex_param save-capture invariant ==="
if "$BUILD/param_check"; then :; else echo "FAIL: param_check"; fail=1; fi
```

- [ ] **Step 7: Full oracle run**

Run: `host_oracle/run_oracle.sh 2>&1 | grep -cE "^OK:"` then `echo $?` of the script.
Expected: OK count ≥ 36 (28 original + 6 knobs + param_check + sweep), script exit 0.

- [ ] **Step 8: Commit**

```bash
git add host_oracle/seq.h host_oracle/shim_driver.cpp host_oracle/oracle.cpp \
        host_oracle/sequences/seq_lead_knobs.txt host_oracle/param_check.cpp host_oracle/run_oracle.sh
git commit -m "test(mbsid): oracle kn/pr commands + knob/par differential + save-capture check"
```

---

### Task 3: `params.rs` — curated Lead parameter table

**Files:**
- Create: `fw/src/params.rs`
- Modify: `fw/src/lib.rs` (add `pub mod params;`)

**Interfaces:**
- Consumes: nothing (host-pure).
- Produces (used by Tasks 8, 9):
  - `pub struct ParamDesc { pub label: &'static str, pub addr: u16, pub mirror: Option<u16>, pub enc: Enc, pub max: u16, pub step: u16 }`
  - `pub enum Enc { Byte { shift: u8, mask: u8 }, Wide12, Cutoff11 }`
  - `pub static LEAD_PARAMS: &[ParamDesc]` (32 entries)
  - `pub fn read_value(d: &ParamDesc, body: impl Fn(u16) -> u8) -> u16`
  - `pub fn write_ops(d: &ParamDesc, value: u16, body: impl Fn(u16) -> u8) -> heapless::Vec<(u16, u8), 4>`

- [ ] **Step 1: Write failing tests** (bottom of the new `fw/src/params.rs`, module skeleton + tests first)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Fake 512-byte patch body for read/write tests.
    fn body_from(pairs: &[(u16, u8)]) -> impl Fn(u16) -> u8 + '_ {
        move |a| pairs.iter().find(|(pa, _)| *pa == a).map(|(_, v)| *v).unwrap_or(0)
    }

    #[test]
    fn table_addresses_inside_lead_regions() {
        for d in LEAD_PARAMS {
            for a in core::iter::once(d.addr).chain(d.mirror) {
                let hi = if matches!(d.enc, Enc::Wide12 | Enc::Cutoff11) { a + 1 } else { a };
                assert!(hi < 512, "{}: addr out of patch", d.label);
                // Lead regions only: globals 0x50..0x54, filter 0x54..0x60,
                // voices 0x60..0xC0, LFOs 0xC0..0xDE.
                assert!((0x50..0xDE).contains(&a), "{}: {a:#x} outside Lead regions", d.label);
            }
            assert!(d.step >= 1 && d.max >= 1, "{}", d.label);
        }
        assert_eq!(LEAD_PARAMS.len(), 32);
    }

    #[test]
    fn osc_rows_mirror_right_sid_voice() {
        // Every voice-region row must mirror addr+0x30 (voice n -> voice n+3).
        for d in LEAD_PARAMS.iter().filter(|d| (0x60..0xC0).contains(&d.addr)) {
            assert_eq!(d.mirror, Some(d.addr + 0x30), "{}", d.label);
        }
        // Filter rows mirror the R block at +6.
        for d in LEAD_PARAMS.iter().filter(|d| (0x54..0x60).contains(&d.addr)) {
            assert_eq!(d.mirror, Some(d.addr + 6), "{}", d.label);
        }
    }

    #[test]
    fn byte_nibble_read_write_roundtrip() {
        let d = ParamDesc { label: "T", addr: 0x62, mirror: None,
                            enc: Enc::Byte { shift: 4, mask: 0x0F }, max: 15, step: 1 };
        assert_eq!(read_value(&d, body_from(&[(0x62, 0xA5)])), 0xA);
        // write attack=3 into ad=0xA5: keep decay nibble 5.
        let ops = write_ops(&d, 3, body_from(&[(0x62, 0xA5)]));
        assert_eq!(ops.as_slice(), &[(0x62, 0x35)]);
    }

    #[test]
    fn wide12_write_preserves_high_nibble_and_mirrors() {
        let d = ParamDesc { label: "PW1", addr: 0x64, mirror: Some(0x94),
                            enc: Enc::Wide12, max: 4095, step: 16 };
        let b = body_from(&[(0x64, 0x00), (0x65, 0xF0), (0x94, 0x00), (0x95, 0xF0)]);
        assert_eq!(read_value(&d, &b), 0x000); // only [11:0] visible
        let ops = write_ops(&d, 0xABC, &b);
        assert_eq!(ops.as_slice(), &[(0x64, 0xBC), (0x65, 0xFA), (0x94, 0xBC), (0x95, 0xFA)]);
    }

    #[test]
    fn cutoff11_preserves_fip_bit() {
        let d = ParamDesc { label: "Cut", addr: 0x55, mirror: Some(0x5B),
                            enc: Enc::Cutoff11, max: 2047, step: 16 };
        // cutoff_l bit 7 = FIP flag, must survive; value = l[6:0] | h[3:0]<<7.
        let b = body_from(&[(0x55, 0x80 | 0x7F), (0x56, 0x0F), (0x5B, 0x80), (0x5C, 0x00)]);
        assert_eq!(read_value(&d, &b), 0x7FF);
        let ops = write_ops(&d, 0x155, &b);
        // 0x155 = l7 0x55, h4 0x02; FIP (0x80) kept on both blocks.
        assert_eq!(ops.as_slice(), &[(0x55, 0x80 | 0x55), (0x56, 0x02), (0x5B, 0x80 | 0x55), (0x5C, 0x02)]);
    }

    #[test]
    fn clamp_to_max() {
        let d = &LEAD_PARAMS[0]; // Volume, max 15
        let ops = write_ops(d, 999, body_from(&[]));
        assert!(ops.iter().all(|(_, v)| (*v & 0x0F) <= 15));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib params`
Expected: compile FAIL (`ParamDesc` not defined).

- [ ] **Step 3: Implement**

Top of `fw/src/params.rs`:

```rust
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

const fn byte(label: &'static str, addr: u16, mirror: Option<u16>,
              shift: u8, mask: u8, max: u16) -> ParamDesc {
    ParamDesc { label, addr, mirror, enc: Enc::Byte { shift, mask }, max, step: 1 }
}
const fn osc(label: &'static str, addr: u16, shift: u8, mask: u8, max: u16) -> ParamDesc {
    byte(label, addr, Some(addr + 0x30), shift, mask, max)
}

pub static LEAD_PARAMS: &[ParamDesc] = &[
    byte("Volume",   0x52, None, 0, 0x0F, 15),
    byte("Detune",   0x51, None, 0, 0xFF, 255),
    byte("Phase",    0x53, None, 0, 0xFF, 255),
    ParamDesc { label: "Cutoff", addr: 0x55, mirror: Some(0x5B),
                enc: Enc::Cutoff11, max: 2047, step: 16 },
    byte("Reso",     0x57, Some(0x5D), 4, 0x0F, 15),
    byte("FltMode",  0x54, Some(0x5A), 4, 0x0F, 15),
    byte("FltChn",   0x54, Some(0x5A), 0, 0x0F, 15),
    // OSC1 (voice0 @ 0x60, mirror voice3 @ 0x90)
    osc("O1 Wave",  0x61, 0, 0xFF, 255),
    osc("O1 Atk",   0x62, 4, 0x0F, 15),
    osc("O1 Dec",   0x62, 0, 0x0F, 15),
    osc("O1 Sus",   0x63, 4, 0x0F, 15),
    osc("O1 Rel",   0x63, 0, 0x0F, 15),
    ParamDesc { label: "O1 PW", addr: 0x64, mirror: Some(0x94),
                enc: Enc::Wide12, max: 4095, step: 16 },
    osc("O1 Porta", 0x6B, 0, 0xFF, 255),
    // OSC2 (voice1 @ 0x70, mirror voice4 @ 0xA0)
    osc("O2 Wave",  0x71, 0, 0xFF, 255),
    osc("O2 Atk",   0x72, 4, 0x0F, 15),
    osc("O2 Dec",   0x72, 0, 0x0F, 15),
    osc("O2 Sus",   0x73, 4, 0x0F, 15),
    osc("O2 Rel",   0x73, 0, 0x0F, 15),
    ParamDesc { label: "O2 PW", addr: 0x74, mirror: Some(0xA4),
                enc: Enc::Wide12, max: 4095, step: 16 },
    osc("O2 Porta", 0x7B, 0, 0xFF, 255),
    // OSC3 (voice2 @ 0x80, mirror voice5 @ 0xB0)
    osc("O3 Wave",  0x81, 0, 0xFF, 255),
    osc("O3 Atk",   0x82, 4, 0x0F, 15),
    osc("O3 Dec",   0x82, 0, 0x0F, 15),
    osc("O3 Sus",   0x83, 4, 0x0F, 15),
    osc("O3 Rel",   0x83, 0, 0x0F, 15),
    ParamDesc { label: "O3 PW", addr: 0x84, mirror: Some(0xB4),
                enc: Enc::Wide12, max: 4095, step: 16 },
    osc("O3 Porta", 0x8B, 0, 0xFF, 255),
    // LFO1 @ 0xC0, LFO2 @ 0xC5 (mode,depth,rate,delay,phase)
    byte("L1 Rate",  0xC2, None, 0, 0xFF, 255),
    byte("L1 Depth", 0xC1, None, 0, 0xFF, 255),
    byte("L2 Rate",  0xC7, None, 0, 0xFF, 255),
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
pub fn write_ops(d: &ParamDesc, value: u16,
                 body: impl Fn(u16) -> u8) -> heapless::Vec<(u16, u8), 4> {
    let v = value.min(d.max);
    let mut ops = heapless::Vec::new();
    let mut one = |a: u16| {
        match d.enc {
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
        }
    };
    one(d.addr);
    if let Some(m) = d.mirror { one(m); }
    ops
}
```

Add `pub mod params;` to `fw/src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib params`
Expected: 6 PASS.

- [ ] **Step 5: Commit**

```bash
git add fw/src/params.rs fw/src/lib.rs
git commit -m "feat(mbsid): curated Lead patch parameter table (params.rs)"
```

---

### Task 4: `cv.rs` — CV targets, mapping math, note machine

**Files:**
- Create: `fw/src/cv.rs`
- Modify: `fw/src/lib.rs` (add `pub mod cv;`)

**Interfaces:**
- Consumes: nothing (host-pure; the sink trait decouples it from FFI).
- Produces (used by Tasks 7, 9):
  - `pub enum CvTarget { Off, Knob1..Knob5, Volume, Phase, Detune, Cutoff, Reso, Pitch, Gate }` with `pub fn to_u8/from_u8`, `pub fn step(self, delta: i8) -> CvTarget` (clamped list), `pub fn label(self) -> &'static str`
  - `pub trait CvSink { fn knob(&mut self, knob: u8, value: u8); fn par(&mut self, par: u8, value16: u16); fn note_on(&mut self, note: u8); fn note_off(&mut self, note: u8); }`
  - `pub struct CvState` with `pub fn new() -> Self`, `pub fn set_targets(&mut self, t: [CvTarget; 4], sink: &mut impl CvSink)`, `pub fn tick(&mut self, x: [i32; 4], sink: &mut impl CvSink)`, `pub fn targets(&self) -> [CvTarget; 4]`

- [ ] **Step 1: Write failing tests** (in `fw/src/cv.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec { knobs: heapless::Vec<(u8, u8), 16>, pars: heapless::Vec<(u8, u16), 16>,
                 ons: heapless::Vec<u8, 16>, offs: heapless::Vec<u8, 16> }
    impl CvSink for Rec {
        fn knob(&mut self, k: u8, v: u8)   { self.knobs.push((k, v)).unwrap(); }
        fn par(&mut self, p: u8, v: u16)   { self.pars.push((p, v)).unwrap(); }
        fn note_on(&mut self, n: u8)       { self.ons.push(n).unwrap(); }
        fn note_off(&mut self, n: u8)      { self.offs.push(n).unwrap(); }
    }
    const V: i32 = 4096; // counts per volt

    #[test]
    fn target_persistence_roundtrip_and_unknown_is_off() {
        for t in [CvTarget::Off, CvTarget::Knob3, CvTarget::Cutoff, CvTarget::Gate] {
            assert_eq!(CvTarget::from_u8(t.to_u8()), t);
        }
        assert_eq!(CvTarget::from_u8(0xEE), CvTarget::Off);
    }

    #[test]
    fn knob_target_scales_0_to_5v_and_deadbands() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets([CvTarget::Knob1, CvTarget::Off, CvTarget::Off, CvTarget::Off], &mut s);
        cv.tick([0, 0, 0, 0], &mut s);
        cv.tick([5 * V, 0, 0, 0], &mut s);
        cv.tick([5 * V, 0, 0, 0], &mut s); // unchanged: no re-emit
        cv.tick([6 * V, 0, 0, 0], &mut s); // clamped: still 255, no re-emit
        assert_eq!(s.knobs.as_slice(), &[(0, 0), (0, 255)]);
    }

    #[test]
    fn par_target_uses_common_block_numbers() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets([CvTarget::Cutoff, CvTarget::Reso, CvTarget::Volume, CvTarget::Detune], &mut s);
        cv.tick([5 * V, 5 * V, 5 * V, 5 * V], &mut s);
        let pars: heapless::Vec<u8, 16> = s.pars.iter().map(|(p, _)| *p).collect();
        assert_eq!(pars.as_slice(), &[0x04, 0x05, 0x01, 0x03]);
        assert!(s.pars.iter().all(|(_, v)| *v == 0xFFFF));
    }

    #[test]
    fn gate_hysteresis_and_fixed_note_without_pitch() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets([CvTarget::Gate, CvTarget::Off, CvTarget::Off, CvTarget::Off], &mut s);
        cv.tick([8193, 0, 0, 0], &mut s);        // > 2V: on
        assert_eq!(s.ons.as_slice(), &[60]);
        cv.tick([5000, 0, 0, 0], &mut s);        // between thresholds: hold
        assert!(s.offs.is_empty());
        cv.tick([4095, 0, 0, 0], &mut s);        // < 1V: off
        assert_eq!(s.offs.as_slice(), &[60]);
    }

    #[test]
    fn pitch_tracks_voct_with_legato_retrigger() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets([CvTarget::Pitch, CvTarget::Gate, CvTarget::Off, CvTarget::Off], &mut s);
        cv.tick([0, 3 * V, 0, 0], &mut s);       // gate on at 0V -> note 36
        assert_eq!(s.ons.as_slice(), &[36]);
        cv.tick([1 * V, 3 * V, 0, 0], &mut s);   // 1V -> note 48: on(48) then off(36)
        assert_eq!(s.ons.as_slice(), &[36, 48]);
        assert_eq!(s.offs.as_slice(), &[36]);
        cv.tick([1 * V, 0, 0, 0], &mut s);       // gate off -> off(48)
        assert_eq!(s.offs.as_slice(), &[36, 48]);
    }

    #[test]
    fn pitch_quantizer_hysteresis_no_flutter_on_boundary() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets([CvTarget::Pitch, CvTarget::Gate, CvTarget::Off, CvTarget::Off], &mut s);
        // Boundary between semitone 0 and 1 is at 4096/24 ≈ 171 counts.
        cv.tick([100, 3 * V, 0, 0], &mut s);     // note 36
        cv.tick([180, 3 * V, 0, 0], &mut s);     // just past boundary but < hysteresis: hold
        cv.tick([100, 3 * V, 0, 0], &mut s);
        assert_eq!(s.ons.as_slice(), &[36], "boundary jitter must not retrigger");
        cv.tick([300, 3 * V, 0, 0], &mut s);     // decisively past: note 37
        assert_eq!(s.ons.as_slice(), &[36, 37]);
    }

    #[test]
    fn pitch_without_gate_does_nothing() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets([CvTarget::Pitch, CvTarget::Off, CvTarget::Off, CvTarget::Off], &mut s);
        cv.tick([2 * V, 0, 0, 0], &mut s);
        assert!(s.ons.is_empty() && s.offs.is_empty() && s.knobs.is_empty() && s.pars.is_empty());
    }

    #[test]
    fn clearing_gate_target_releases_held_note() {
        let mut cv = CvState::new();
        let mut s = Rec::default();
        cv.set_targets([CvTarget::Gate, CvTarget::Off, CvTarget::Off, CvTarget::Off], &mut s);
        cv.tick([3 * V, 0, 0, 0], &mut s);
        assert_eq!(s.ons.as_slice(), &[60]);
        cv.set_targets([CvTarget::Off; 4], &mut s); // reassignment: no stuck note
        assert_eq!(s.offs.as_slice(), &[60]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib cv`
Expected: compile FAIL (`CvTarget` not defined).

- [ ] **Step 3: Implement**

```rust
//! CV-input modulation routing (M5_MENU_CARDS_CV_MOD.md §5b, §6b).
//!
//! Host-pure: all engine access goes through `CvSink`, so the mapping math,
//! hysteresis and note machine are fully unit-tested off-target. Integer only
//! (ISR path). Calibrated CV = 4096 counts/volt; usable span 0..5 V.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CvTarget {
    Off,
    Knob1, Knob2, Knob3, Knob4, Knob5,   // engine knob matrix (patch-defined routing)
    Volume, Phase, Detune, Cutoff, Reso, // parSet common block 0x01..0x05
    Pitch, Gate,                         // CV note machine, MIDI ch 1
}

const TARGET_ORDER: [CvTarget; 13] = [
    CvTarget::Off,
    CvTarget::Knob1, CvTarget::Knob2, CvTarget::Knob3, CvTarget::Knob4, CvTarget::Knob5,
    CvTarget::Volume, CvTarget::Phase, CvTarget::Detune, CvTarget::Cutoff, CvTarget::Reso,
    CvTarget::Pitch, CvTarget::Gate,
];

impl CvTarget {
    pub fn to_u8(self) -> u8 {
        TARGET_ORDER.iter().position(|&t| t == self).unwrap_or(0) as u8
    }
    pub fn from_u8(b: u8) -> Self {
        // Unknown persisted bytes decode to Off (forward compatibility).
        TARGET_ORDER.get(b as usize).copied().unwrap_or(CvTarget::Off)
    }
    pub fn step(self, delta: i8) -> Self {
        let ix = self.to_u8() as i16 + delta as i16;
        let ix = ix.clamp(0, TARGET_ORDER.len() as i16 - 1);
        TARGET_ORDER[ix as usize]
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Knob1 => "Knob1", Self::Knob2 => "Knob2", Self::Knob3 => "Knob3",
            Self::Knob4 => "Knob4", Self::Knob5 => "Knob5",
            Self::Volume => "Volume", Self::Phase => "Phase", Self::Detune => "Detune",
            Self::Cutoff => "Cutoff", Self::Reso => "Reso",
            Self::Pitch => "Pitch", Self::Gate => "Gate",
        }
    }
    fn par_number(self) -> Option<u8> {
        match self {
            Self::Volume => Some(0x01), Self::Phase => Some(0x02),
            Self::Detune => Some(0x03), Self::Cutoff => Some(0x04),
            Self::Reso => Some(0x05), _ => None,
        }
    }
    fn knob_number(self) -> Option<u8> {
        match self {
            Self::Knob1 => Some(0), Self::Knob2 => Some(1), Self::Knob3 => Some(2),
            Self::Knob4 => Some(3), Self::Knob5 => Some(4), _ => None,
        }
    }
}

pub trait CvSink {
    fn knob(&mut self, knob: u8, value: u8);
    fn par(&mut self, par: u8, value16: u16);
    fn note_on(&mut self, note: u8);   // MIDI ch 1, velocity 100 (implementer)
    fn note_off(&mut self, note: u8);
}

const COUNTS_PER_VOLT: i32 = 4096;
const FULL_SCALE: i32 = 5 * COUNTS_PER_VOLT;      // 0..5 V unipolar
const GATE_ON: i32 = 2 * COUNTS_PER_VOLT;         // > 2 V
const GATE_OFF: i32 = COUNTS_PER_VOLT;            // < 1 V
const PITCH_BASE_NOTE: i32 = 36;                  // 0 V = C2
const PITCH_SPAN: i32 = 60;                       // semitone indices 0..=60
const PITCH_HYST: i32 = COUNTS_PER_VOLT / 12 / 4; // ±¼ semitone ≈ 85 counts
const FIXED_GATE_NOTE: u8 = 60;                   // Gate with no Pitch: C-4

fn to_u8_scale(x: i32) -> u8 {
    (x.clamp(0, FULL_SCALE) * 255 / FULL_SCALE) as u8
}

/// Semitone index with boundary hysteresis: leave `current` only when the CV
/// is more than PITCH_HYST past the boundary to the neighbouring semitone.
fn quantize_semitone(x: i32, current: u8) -> u8 {
    let x = x.clamp(0, FULL_SCALE);
    let cur = current as i32;
    let upper = (2 * cur + 1) * COUNTS_PER_VOLT / 24 + PITCH_HYST;
    let lower = (2 * cur - 1) * COUNTS_PER_VOLT / 24 - PITCH_HYST;
    if x > upper || x < lower {
        ((x * 12 + COUNTS_PER_VOLT / 2) / COUNTS_PER_VOLT).clamp(0, PITCH_SPAN) as u8
    } else {
        current
    }
}

pub struct CvState {
    targets: [CvTarget; 4],
    last8: [Option<u8>; 4], // last emitted 8-bit value per input (knob/par deadband)
    gate: bool,
    held_note: u8,
    semitone: u8,
}

impl CvState {
    pub fn new() -> Self {
        Self { targets: [CvTarget::Off; 4], last8: [None; 4],
               gate: false, held_note: 0, semitone: 0 }
    }

    pub fn targets(&self) -> [CvTarget; 4] { self.targets }

    /// Apply a new assignment set; releases a held CV note if Gate went away.
    pub fn set_targets(&mut self, t: [CvTarget; 4], sink: &mut impl CvSink) {
        if self.gate && !t.contains(&CvTarget::Gate) {
            sink.note_off(self.held_note);
            self.gate = false;
        }
        self.last8 = [None; 4]; // re-emit values for retargeted inputs
        self.targets = t;
    }

    pub fn tick(&mut self, x: [i32; 4], sink: &mut impl CvSink) {
        // Continuous targets (knob/par), deadbanded on the 8-bit value.
        for i in 0..4 {
            let t = self.targets[i];
            let (Some(_), v8) = (t.knob_number().or(t.par_number()), to_u8_scale(x[i]))
                else { continue };
            if self.last8[i] == Some(v8) { continue; }
            self.last8[i] = Some(v8);
            if let Some(k) = t.knob_number() {
                sink.knob(k, v8);
            } else if let Some(p) = t.par_number() {
                // 8-bit precision spread over the full 16-bit range.
                sink.par(p, ((v8 as u16) << 8) | v8 as u16);
            }
        }

        // Note machine: first Pitch input and first Gate input, if assigned.
        let pitch_in = self.targets.iter().position(|&t| t == CvTarget::Pitch);
        let gate_in = self.targets.iter().position(|&t| t == CvTarget::Gate);
        let Some(g) = gate_in else { return }; // Pitch without Gate: no effect
        let level = x[g];

        if !self.gate && level > GATE_ON {
            self.gate = true;
            self.held_note = match pitch_in {
                Some(p) => {
                    self.semitone = quantize_semitone(x[p], self.semitone);
                    (PITCH_BASE_NOTE + self.semitone as i32) as u8
                }
                None => FIXED_GATE_NOTE,
            };
            sink.note_on(self.held_note);
        } else if self.gate && level < GATE_OFF {
            self.gate = false;
            sink.note_off(self.held_note);
        } else if self.gate {
            if let Some(p) = pitch_in {
                let s = quantize_semitone(x[p], self.semitone);
                if s != self.semitone {
                    self.semitone = s;
                    let new = (PITCH_BASE_NOTE + s as i32) as u8;
                    sink.note_on(new);       // on-then-off: legato in mono modes
                    sink.note_off(self.held_note);
                    self.held_note = new;
                }
            }
        }
    }
}
```

Note: on the very first gated tick `semitone` starts at 0 — `quantize_semitone` immediately jumps to the true value because a fresh CV is decisively outside note 0's hysteresis band (and 0 is correct when the CV really is near 0 V). Add `pub mod cv;` to `fw/src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib cv`
Expected: 8 PASS.

- [ ] **Step 5: Commit**

```bash
git add fw/src/cv.rs fw/src/lib.rs
git commit -m "feat(mbsid): CV target routing, mapping math, note machine (cv.rs)"
```

---

### Task 5: `settings_store.rs` + `UserPatchStore::flash_mut`

**Files:**
- Create: `fw/src/settings_store.rs`
- Modify: `fw/src/patch_store.rs` (one accessor)
- Modify: `fw/src/lib.rs` (add `pub mod settings_store;`)

**Interfaces:**
- Consumes: `tiliqua_hal::nor_flash::{NorFlash, ReadNorFlash}` (same bound as `UserPatchStore`); `cv::CvTarget::{to_u8, from_u8}` is applied by the *caller* (this module stores raw bytes).
- Produces (used by Task 9):
  - `pub struct Settings { pub midi_src: u8, pub cv_targets: [u8; 4] }` (+ `Default`: all 0 = TRS, Off×4)
  - `pub fn encode(s: &Settings) -> [u8; 16]`, `pub fn decode(rec: &[u8; 16]) -> Option<Settings>`
  - `pub fn load<F: ReadNorFlash>(flash: &mut F, base: u32) -> Settings`
  - `pub fn save<F: NorFlash + ReadNorFlash>(flash: &mut F, base: u32, s: &Settings) -> Result<(), F::Error>` (no-op if the stored record already matches)
  - `UserPatchStore::flash_mut(&mut self) -> &mut F` (lets main.rs reuse the single SPIFlash instance)

- [ ] **Step 1: Add the accessor to `patch_store.rs`**

After `into_inner`:

```rust
    /// Borrow the flash driver (shared with the M5 settings record — one
    /// SPIFlash instance serves both stores; never called from the ISR).
    pub fn flash_mut(&mut self) -> &mut F { &mut self.flash }
```

- [ ] **Step 2: Write failing tests** (in `fw/src/settings_store.rs`; reuse the mock-flash pattern from `patch_store.rs`'s test module — copy its `MockFlash` implementing `ErrorType + ReadNorFlash + NorFlash` over a fixed `[u8; 8192]` backing array with 4096-byte erase granularity, plus an `erase_count: usize` incremented in `erase`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // MockFlash: copy of patch_store.rs's test mock (Vec-backed, 4KiB sectors).

    #[test]
    fn roundtrip() {
        let s = Settings { midi_src: 1, cv_targets: [0, 3, 11, 12] };
        assert_eq!(decode(&encode(&s)), Some(s));
    }

    #[test]
    fn corrupt_records_rejected() {
        let s = Settings { midi_src: 1, cv_targets: [1, 2, 3, 4] };
        let good = encode(&s);
        let mut bad_magic = good; bad_magic[0] ^= 0xFF;
        assert_eq!(decode(&bad_magic), None);
        let mut bad_ver = good; bad_ver[4] = 99;
        assert_eq!(decode(&bad_ver), None);
        let mut bad_sum = good; bad_sum[6] ^= 0x01; // flip a target byte, keep chk
        assert_eq!(decode(&bad_sum), None);
    }

    #[test]
    fn load_defaults_on_blank_flash() {
        let mut f = MockFlash::new();
        let s = load(&mut f, 0);
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn save_then_load_and_identical_save_skips_erase() {
        let mut f = MockFlash::new();
        let s = Settings { midi_src: 1, cv_targets: [12, 11, 0, 0] };
        save(&mut f, 0, &s).unwrap();
        assert_eq!(load(&mut f, 0), s);
        let erases_before = f.erase_count;
        save(&mut f, 0, &s).unwrap(); // identical: must not erase again
        assert_eq!(f.erase_count, erases_before);
    }
}
```

(`Settings` needs `#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]`; give the mock an `erase_count: usize` it increments in `erase`.)

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib settings`
Expected: compile FAIL.

- [ ] **Step 4: Implement**

```rust
//! Persisted menu settings (M5_MENU_CARDS_CV_MOD.md §6d): one 16-byte record
//! in the option-storage flash window (`manifest.get_option_storage_window()`).
//! Layout: "MBS5" | version | midi_src | cv_targets[4] | reserved[5] | chk.
//! chk = two's-complement byte making the whole record sum to 0 (mod 256) —
//! same family as patch_store's payload checksum. Any validation failure
//! loads defaults (TRS, all CV Off). Saves are debounced by the caller and
//! skipped when the stored record is already identical (flash wear).

use tiliqua_hal::nor_flash::{NorFlash, ReadNorFlash};

const MAGIC: [u8; 4] = *b"MBS5";
const VERSION: u8 = 1;
pub const RECORD_LEN: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Settings {
    pub midi_src: u8,        // 0 = TRS, 1 = USB
    pub cv_targets: [u8; 4], // CvTarget::to_u8 encoding
}

pub fn encode(s: &Settings) -> [u8; RECORD_LEN] {
    let mut r = [0u8; RECORD_LEN];
    r[0..4].copy_from_slice(&MAGIC);
    r[4] = VERSION;
    r[5] = s.midi_src;
    r[6..10].copy_from_slice(&s.cv_targets);
    let sum: u8 = r[..15].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    r[15] = sum.wrapping_neg();
    r
}

pub fn decode(r: &[u8; RECORD_LEN]) -> Option<Settings> {
    if r[0..4] != MAGIC || r[4] != VERSION { return None; }
    if r.iter().fold(0u8, |a, &b| a.wrapping_add(b)) != 0 { return None; }
    let mut cv = [0u8; 4];
    cv.copy_from_slice(&r[6..10]);
    Some(Settings { midi_src: r[5], cv_targets: cv })
}

pub fn load<F: ReadNorFlash>(flash: &mut F, base: u32) -> Settings {
    let mut r = [0u8; RECORD_LEN];
    if flash.read(base, &mut r).is_err() { return Settings::default(); }
    decode(&r).unwrap_or_default()
}

pub fn save<F: NorFlash + ReadNorFlash>(flash: &mut F, base: u32,
                                        s: &Settings) -> Result<(), F::Error> {
    let rec = encode(s);
    let mut cur = [0u8; RECORD_LEN];
    if flash.read(base, &mut cur).is_ok() && cur == rec {
        return Ok(()); // identical: skip the erase (flash wear)
    }
    flash.erase(base, base + 4096)?;
    flash.write(base, &rec)
}
```

Add `pub mod settings_store;` to `fw/src/lib.rs`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib` — settings tests PASS, patch_store tests still PASS.

- [ ] **Step 6: Commit**

```bash
git add fw/src/settings_store.rs fw/src/patch_store.rs fw/src/lib.rs
git commit -m "feat(mbsid): persisted settings record in the option-storage window"
```

---

### Task 6: `menu.rs` card layer (state machine only)

**Files:**
- Modify: `fw/src/menu.rs` (state machine + existing tests; drawing changes come in Task 8)

**Interfaces:**
- Consumes: `cv::CvTarget`, `params::LEAD_PARAMS`.
- Produces (used by Tasks 7, 8, 9):
  - `pub enum Card { Main, CvMod, PatchEdit }`
  - `MenuState` fields: `pub card: Card`, `pub focus: u8` (row index in the current card), `pub cv_targets: [CvTarget; 4]`, `pub edit_values: [u16; N_PARAMS]`, `pub edit_scroll: u8`, `pub edited: bool` (plus all existing fields)
  - `pub const N_PARAMS: usize = params::LEAD_PARAMS.len();`
  - Row index constants: `ROW_CARD: u8 = 0`; Main: `MAIN_ROW_BANK=1, MAIN_ROW_PROGRAM=2, MAIN_ROW_SAVE=3, MAIN_ROW_MIDISRC=4`
  - `pub enum TurnResult { None, Load, Param { ix: u8, value: u16 }, SettingsChanged }` — `on_turn(&mut self, delta: i8) -> TurnResult` replaces the old `-> bool` (`Load` ⇔ old `true`)
  - `on_press(&mut self) -> PressResult` unchanged in shape; `PressResult::Commit(slot)` now fires from the Save row of **either** Main or PatchEdit
  - `pub fn refresh_params(&mut self, body: impl Fn(u16) -> u8)` — fills `edit_values` from a patch-body reader

- [ ] **Step 1: Update existing tests + add card-layer tests**

Mechanical updates to the 15 existing menu tests: `focus` comparisons become row indices (e.g. `m.focus == MAIN_ROW_BANK`), `on_turn(..) == true/false` become `matches!(.., TurnResult::Load)` / `!matches!(.., TurnResult::Load)`, and Nav clamp bounds grow by one (row 0 = Card). Keep every behavioral assertion.

New tests:

```rust
    #[test]
    fn card_row_cycles_cards_without_side_effects() {
        let mut m = MenuState::new(2, 0, 0);
        assert_eq!(m.card, Card::Main);
        assert_eq!(m.focus, ROW_CARD);
        let _ = m.on_press(); // Edit on Card row
        assert!(matches!(m.on_turn(1), TurnResult::None));
        assert_eq!(m.card, Card::CvMod);
        assert!(matches!(m.on_turn(1), TurnResult::None));
        assert_eq!(m.card, Card::PatchEdit);
        assert!(matches!(m.on_turn(1), TurnResult::None)); // clamp
        assert_eq!(m.card, Card::PatchEdit);
        assert!(matches!(m.on_turn(-2), TurnResult::None));
        assert_eq!(m.card, Card::Main);
        let _ = m.on_press(); // back to Nav
        assert_eq!(m.mode, Mode::Nav);
    }

    #[test]
    fn switching_cards_keeps_per_card_focus_in_range() {
        let mut m = MenuState::new(2, 0, 0);
        m.focus = MAIN_ROW_MIDISRC; // row 4 on Main
        m.card = Card::CvMod;       // CvMod also has 5 rows: still valid
        assert!(m.focus < m.row_count());
        m.focus = 0;
        m.card = Card::PatchEdit;
        assert_eq!(m.row_count() as usize, 2 + N_PARAMS); // Card + params + Save
    }

    #[test]
    fn main_save_commit_still_works_at_new_index() {
        let mut m = MenuState::new(2, 0, 0);
        m.focus = MAIN_ROW_SAVE;
        assert_eq!(m.on_press(), PressResult::Toggled);
        m.on_turn(10);
        assert_eq!(m.on_press(), PressResult::Commit(9));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib menu`
Expected: compile FAIL (`Card`, `TurnResult` undefined).

- [ ] **Step 3: Implement the card layer**

Key changes in `menu.rs` (drawing untouched until Task 8; keep `draw` compiling by mapping `focus` indices back to the old `Row` enum internally, or convert `draw`'s row checks to indices now — implementer's choice, tests are on the state machine):

```rust
use crate::cv::CvTarget;
use crate::params;

pub const N_PARAMS: usize = params::LEAD_PARAMS.len();

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Card { Main, CvMod, PatchEdit }

impl Card {
    pub fn label(self) -> &'static str {
        match self { Self::Main => "Main", Self::CvMod => "CV Mod", Self::PatchEdit => "Edit" }
    }
    fn step(self, delta: i8) -> Self {
        let ix = match self { Self::Main => 0i16, Self::CvMod => 1, Self::PatchEdit => 2 };
        match (ix + delta as i16).clamp(0, 2) {
            0 => Self::Main, 1 => Self::CvMod, _ => Self::PatchEdit,
        }
    }
}

pub const ROW_CARD: u8 = 0;
pub const MAIN_ROW_BANK: u8 = 1;
pub const MAIN_ROW_PROGRAM: u8 = 2;
pub const MAIN_ROW_SAVE: u8 = 3;
pub const MAIN_ROW_MIDISRC: u8 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TurnResult {
    None,
    Load,                             // (bank, program) load required
    Param { ix: u8, value: u16 },     // patch-edit row changed -> sysex_param writes
    SettingsChanged,                  // cv target or midi_src changed -> persist
}
```

`MenuState` gains `card`, `cv_targets: [CvTarget; 4]`, `edit_values: [u16; N_PARAMS]`, `edit_scroll: u8`, `edited: bool`; `focus: Row` becomes `focus: u8`. `new()` starts `card: Card::Main, focus: ROW_CARD`.

```rust
    pub fn row_count(&self) -> u8 {
        match self.card {
            Card::Main => 5,
            Card::CvMod => 5,                       // Card + CV1..CV4
            Card::PatchEdit => 2 + N_PARAMS as u8,  // Card + params + Save
        }
    }

    fn is_save_row(&self) -> bool {
        match self.card {
            Card::Main => self.focus == MAIN_ROW_SAVE,
            Card::PatchEdit => self.focus == self.row_count() - 1,
            Card::CvMod => false,
        }
    }
```

`on_turn` Nav arm: `self.focus = clamp(focus + delta, 0, row_count-1)`; on PatchEdit also pull `edit_scroll` so the focused row stays inside the 6-row window:

```rust
            Mode::Nav => {
                let hi = (self.row_count() - 1) as i16;
                self.focus = clamp_i16(self.focus as i16 + delta as i16, 0, hi) as u8;
                if self.card == Card::PatchEdit && self.focus >= 1 {
                    let ix = self.focus - 1; // 0-based row within the scrolling list
                    const WINDOW: u8 = 6;
                    if ix < self.edit_scroll { self.edit_scroll = ix; }
                    if ix >= self.edit_scroll + WINDOW { self.edit_scroll = ix - WINDOW + 1; }
                }
                TurnResult::None
            }
```

`on_turn` Edit arm dispatches on `(card, focus)`:
- `ROW_CARD` (any card): `self.card = self.card.step(delta); self.focus = ROW_CARD; TurnResult::None`
- Main rows: existing bank/program/save/midi_src logic; bank/program change → `TurnResult::Load`; midi_src toggle → `TurnResult::SettingsChanged`
- CvMod rows 1–4: `self.cv_targets[i] = self.cv_targets[i].step(delta);` changed → `TurnResult::SettingsChanged`
- PatchEdit param row `ix = focus-1` (< `N_PARAMS`): step `edit_values[ix]` by `delta * step`, clamp to `0..=max`, changed → `TurnResult::Param { ix, value }`
- PatchEdit Save row: existing `save_cursor` logic (returns `TurnResult::None`)

`on_press`: replace `self.focus == Row::Save` with `self.is_save_row()` in both the commit check and the enter-Edit `save_cursor = -1` reset. Everything else unchanged.

```rust
    pub fn refresh_params(&mut self, body: impl Fn(u16) -> u8) {
        for (i, d) in params::LEAD_PARAMS.iter().enumerate() {
            self.edit_values[i] = params::read_value(d, &body);
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib menu`
Expected: all old (adjusted) + 3 new tests PASS.

- [ ] **Step 5: Commit**

```bash
git add fw/src/menu.rs
git commit -m "feat(mbsid): menu card layer — Main/CvMod/PatchEdit state machine"
```

---

### Task 7: CV Mod + Patch Edit row behavior tests

**Files:**
- Modify: `fw/src/menu.rs` (tests + any behavior gaps they expose)

**Interfaces:**
- Consumes: Task 6's `TurnResult`, `CvTarget::step`, `params::LEAD_PARAMS`.
- Produces: verified row semantics Tasks 8–9 build on.

- [ ] **Step 1: Write the tests**

```rust
    #[test]
    fn cvmod_row_edit_steps_targets_and_reports_settings_change() {
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::CvMod;
        m.focus = 1; // CV1
        let _ = m.on_press();
        assert_eq!(m.on_turn(1), TurnResult::SettingsChanged);
        assert_eq!(m.cv_targets[0], CvTarget::Knob1);
        assert_eq!(m.on_turn(-1), TurnResult::SettingsChanged);
        assert_eq!(m.cv_targets[0], CvTarget::Off);
        assert_eq!(m.on_turn(-1), TurnResult::None); // clamped at Off: no change
    }

    #[test]
    fn patch_edit_row_steps_value_and_emits_param() {
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::PatchEdit;
        m.edit_values[0] = 5; // Volume row (max 15, step 1)
        m.focus = 1;
        let _ = m.on_press();
        assert_eq!(m.on_turn(2), TurnResult::Param { ix: 0, value: 7 });
        assert_eq!(m.on_turn(100), TurnResult::Param { ix: 0, value: 15 }); // clamp
        assert_eq!(m.on_turn(1), TurnResult::None); // at max: no change
    }

    #[test]
    fn patch_edit_wide_row_uses_coarse_step() {
        let cutoff_ix = params::LEAD_PARAMS.iter()
            .position(|d| d.label == "Cutoff").unwrap();
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::PatchEdit;
        m.focus = cutoff_ix as u8 + 1;
        let _ = m.on_press();
        assert_eq!(m.on_turn(1),
                   TurnResult::Param { ix: cutoff_ix as u8, value: 16 });
    }

    #[test]
    fn patch_edit_scroll_follows_focus() {
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::PatchEdit;
        for _ in 0..(N_PARAMS + 1) { m.on_turn(1); } // Nav to the Save row
        assert_eq!(m.focus, m.row_count() - 1);
        let ix = m.focus - 1;
        assert!(ix >= m.edit_scroll && ix < m.edit_scroll + 6, "focus visible");
        for _ in 0..(N_PARAMS + 1) { m.on_turn(-1); } // back to Card row
        assert_eq!(m.edit_scroll, 0);
    }

    #[test]
    fn patch_edit_save_row_commits_like_main() {
        let mut m = MenuState::new(2, 0, 0);
        m.card = Card::PatchEdit;
        m.focus = m.row_count() - 1;
        assert_eq!(m.on_press(), PressResult::Toggled);
        assert_eq!(m.save_cursor, -1); // Cancel-first
        m.on_turn(5);
        assert_eq!(m.on_press(), PressResult::Commit(4));
    }

    #[test]
    fn refresh_params_reads_lead_layout() {
        let mut m = MenuState::new(2, 0, 0);
        // Body: volume=0xC (0x52), OSC1 ad=0x84 (0x62: A=8, D=4).
        m.refresh_params(|a| match a { 0x52 => 0x0C, 0x62 => 0x84, _ => 0 });
        assert_eq!(m.edit_values[0], 12);
        let atk_ix = params::LEAD_PARAMS.iter().position(|d| d.label == "O1 Atk").unwrap();
        let dec_ix = params::LEAD_PARAMS.iter().position(|d| d.label == "O1 Dec").unwrap();
        assert_eq!(m.edit_values[atk_ix], 8);
        assert_eq!(m.edit_values[dec_ix], 4);
    }
```

- [ ] **Step 2: Run tests; fix any gaps they expose**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib menu`
Expected: PASS (Task 6 implemented these paths; this task pins them). Fix `on_turn` edge cases if any test fails — the tests are the contract, don't weaken them.

- [ ] **Step 3: Commit**

```bash
git add fw/src/menu.rs
git commit -m "test(mbsid): pin CvMod and PatchEdit row semantics"
```

---

### Task 8: Menu drawing for all three cards

**Files:**
- Modify: `fw/src/menu.rs` (`draw`, `MENU_H`, row rendering)

**Interfaces:**
- Consumes: Task 6/7 state, `params::LEAD_PARAMS`, `CvTarget::label`, `Card::label`.
- Produces (used by Task 9): `pub fn draw<D>(d, st: &MenuState, name: &str, detail: Option<(Engine, Option<VoiceMode>)>, save_name: Option<&str>, status: Option<&str>, lead_loaded: bool, pos_x, pos_y, hue) -> Result<(), D::Error>` — one new `lead_loaded: bool` argument (gates Patch Edit rows).

- [ ] **Step 1: Implement drawing** (visual code — no host test; verified on hardware and by compile)

- `MENU_H` 194 → **244** (PatchEdit worst case: title + Card + 6 param rows + Save + status = 10 lines × 24 px; spec allows growing when needed).
- Row 0 on every card: `{marker} Card     {card.label()}` — marker via the existing `row_marker` (now index-based).
- `Card::Main`: existing five rows at indices 1–4 (Bank/Program/Save/MidiSrc), plus the detail line and status line as today.
- `Card::CvMod`: rows `CV1..CV4` as `{marker} CV{n}      {target.label()}`; a dim footer line `mods engine knobs/params` in place of Main's detail line.
- `Card::PatchEdit`:
  - `lead_loaded == false`: draw the Card row, one dim line `Lead patches only`, then the Save row; skip all param rows.
  - `lead_loaded == true`: draw param rows `edit_scroll .. min(edit_scroll+6, N_PARAMS)` as `{marker} {label:<8} {value}` (values from `st.edit_values`), then the Save row (same rendering as Main's Save: `Cancel` / `U{slot} {name}`).
  - When `st.edited`, append ` *` after `name` in the title area (all cards: change the title line to `MBSID  {name}{star}` — Main's Program row already shows the name; keep the star on the Program row on Main and in the title on other cards — simplest: star is drawn right of the title on every card).
- Scroll indicators: draw `↑`/`↓` (or `^`/`v` — FONT_9X15 is ASCII; use `^`/`v`) at the window edges when `edit_scroll > 0` / more rows below.

- [ ] **Step 2: Verify the crate compiles for host and all tests pass**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: all tests PASS (draw isn't unit-tested; this catches signature drift).

- [ ] **Step 3: Commit**

```bash
git add fw/src/menu.rs
git commit -m "feat(mbsid): draw all three menu cards with patch-edit scroll window"
```

---

### Task 9: `main.rs` wiring — CV in the ISR, edits + persistence in the main loop

**Files:**
- Modify: `fw/src/main.rs`

**Interfaces:**
- Consumes: everything above — `cv::{CvState, CvSink, CvTarget}`, `params`, `settings_store`, `menu::{TurnResult, Card}`, `mbsid_sys::{knob_set, par_set, sysex_param, patch_byte, current_engine}`, `UserPatchStore::flash_mut`, `EurorackPmod0` (`tiliqua_hal::pmod::EurorackPmod`, `sample_i() -> [i32; 4]`), `calibration::CalibrationConstants`.
- Produces: the complete M5 firmware.

- [ ] **Step 1: App state + boot wiring**

Imports to add: `use tiliqua_hal::pmod::EurorackPmod;`, `use tiliqua_fw::cv::{self, CvSink};`, `use tiliqua_fw::{params, settings_store};`, `use tiliqua_fw::menu::{TurnResult, Card};`, `use tiliqua_lib::calibration;`.

`App` gains:

```rust
struct App {
    // ... existing fields ...
    cv: cv::CvState,
    pmod: EurorackPmod0,
    uptime_ms: u32,   // ISR tick counter: main-loop debounce timebase
}
```

`App::new(pmod: EurorackPmod0)` takes the pmod (constructed + calibrated in `main`). In `main()`, before `App::new`:

```rust
    let mut i2cdev1 = I2c1::new(peripherals.I2C1);
    let mut pmod = EurorackPmod0::new(peripherals.PMOD0_PERIPH);
    calibration::CalibrationConstants::load_or_default(&mut i2cdev1, &mut pmod);
```

Settings load (after `store` is created; `bootinfo` is already in scope):

```rust
    let opt_window = bootinfo.manifest.get_option_storage_window();
    let settings = match opt_window {
        Some(ref w) => settings_store::load(store.flash_mut(), w.start),
        None => settings_store::Settings::default(),
    };
```

Apply to `MenuState` after construction: `state.midi_src` from `settings.midi_src` (0=Trs, 1=Usb), `state.cv_targets = settings.cv_targets.map(cv::CvTarget::from_u8)`. Seed the ISR side once before `irq::scope`: `app.cv.set_targets(state.cv_targets, &mut EngineSink)` (no note can be held yet, sink is inert). Add `let mut settings_dirty_at: Option<u32> = None;` and `let mut last_saved = settings;`.

- [ ] **Step 2: The engine sink**

```rust
/// CvSink implementation over the engine FFI. Only used inside
/// critical_section (ISR body, or main-loop blocks under `cs`).
struct EngineSink;
impl CvSink for EngineSink {
    fn knob(&mut self, knob: u8, value: u8) { mbsid_sys::knob_set(knob, value); }
    fn par(&mut self, par: u8, value16: u16) { mbsid_sys::par_set(par, value16); }
    fn note_on(&mut self, note: u8) { mbsid_sys::note_on(0, note, 100); } // MIDI ch 1
    fn note_off(&mut self, note: u8) { mbsid_sys::note_off(0, note); }
}
```

- [ ] **Step 3: ISR additions** (in `timer0_handler`, inside the existing `critical_section`, after the SysEx drain and **before** the `mbsid_sys::tick()` block)

```rust
        // (a3) CV modulation: sample the calibrated inputs and route per the
        // menu's target assignments (M5 §6b). Integer-only; engine calls are
        // the same knob/par paths MIDI CC takes.
        app.uptime_ms = app.uptime_ms.wrapping_add(1);
        let x = app.pmod.sample_i();
        let App { cv, .. } = &mut *app;
        cv.tick(x, &mut EngineSink);
```

- [ ] **Step 4: Main-loop additions**

Replace the `on_turn` handling:

```rust
            let mut need_load = false;
            if ticks != 0 {
                match state.on_turn(ticks) {
                    TurnResult::Load => { need_load = true; }
                    TurnResult::Param { ix, value } => {
                        let d = &params::LEAD_PARAMS[ix as usize];
                        let ops = params::write_ops(d, value, |a| mbsid_sys::patch_byte(a));
                        critical_section::with(|_cs| {
                            for (a, v) in ops.iter() {
                                mbsid_sys::sysex_param(*a, *v);
                            }
                        });
                        state.edited = true;
                    }
                    TurnResult::SettingsChanged => {
                        critical_section::with(|cs| {
                            let mut a = app.borrow_ref_mut(cs);
                            let App { cv, .. } = &mut *a;
                            cv.set_targets(state.cv_targets, &mut EngineSink);
                        });
                        settings_dirty_at = Some(now_ms(&app));
                    }
                    TurnResult::None => {}
                }
                dirty = true;
            }
```

with the helper (above `main`):

```rust
fn now_ms(app: &Mutex<RefCell<App>>) -> u32 {
    critical_section::with(|cs| app.borrow_ref(cs).uptime_ms)
}
```

Debounced settings persist (in the loop, after the pending-save block):

```rust
            // Persist settings ~2s after the last change (flash wear; §6d).
            if let (Some(t0), Some(ref w)) = (settings_dirty_at, opt_window.as_ref().map(|w| w.clone())) {
                if now_ms(&app).wrapping_sub(t0) >= 2000 {
                    let s = settings_store::Settings {
                        midi_src: (state.midi_src == menu::MidiSource::Usb) as u8,
                        cv_targets: state.cv_targets.map(|t| t.to_u8()),
                    };
                    if s != last_saved {
                        let _ = settings_store::save(store.flash_mut(), w.start, &s);
                        last_saved = s;
                    }
                    settings_dirty_at = None;
                }
            }
```

(`MidiSrc` toggling already returns `TurnResult::SettingsChanged` from Task 6, so it feeds the same debounce.)

Patch-load / save integration:
- After a successful load (`need_load` block, both user-bank and ROM paths): `state.refresh_params(|a| mbsid_sys::patch_byte(a)); state.edited = false;` and compute `lead_loaded = mbsid_sys::current_engine() == 0;` (keep a `let mut lead_loaded = true;` initialized after the boot `program_change(BOOT_PATCH_INDEX)` with the same call, plus an initial `state.refresh_params(..)`).
- In the `PressResult::Commit(slot)` arm (now reachable from both cards — code unchanged): after a successful save add `state.edited = false;`.
- Pass `lead_loaded` into `menu::draw(...)` (new argument from Task 8).

`patch_byte`/`current_engine` reads are point-reads of engine `.bss` the ISR only mutates through the same engine; they're wrapped here in the drawing path without a critical section exactly like `bank_patch_info` already is — acceptable staleness, `dirty` redraw picks up changes.

- [ ] **Step 5: Host tests + firmware relink**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: all PASS.

Run: `cd ../../../.. && pdm mbsid build --fw-only` (from `gateware/`)
Expected: Rust/C++ compile clean; ends with the expected `missing top.bit` error **after** `tiliqua-fw` links. Any compile error = failure.

- [ ] **Step 6: Verify RAM budget**

Run: `llvm-size -A gateware/src/top/mbsid/fw/target/riscv32im-unknown-none-elf/release/tiliqua-fw | grep -E "\.bss|\.data|\.stack"`
Expected: `.bss` within ~200 B of the pre-M5 value (new state is tens of bytes); `.stack` region still ≥ 20 KB. (Remember: `.stack` section size is the linker leftover, not usage — root CLAUDE.md.)

- [ ] **Step 7: Commit**

```bash
git add fw/src/main.rs
git commit -m "feat(mbsid): wire CV modulation, patch edits, and settings persistence"
```

---

### Task 10: Docs, labels, full verification

**Files:**
- Modify: `top.py` (`bitstream_help.io_left`)
- Modify: `CLAUDE.md` (this dir)
- Modify: `README.md` (this dir)
- Modify: `M5_MENU_CARDS_CV_MOD.md` (status header)

**Interfaces:** none — documentation + final gates.

- [ ] **Step 1: Update `top.py` help**

```python
        io_left=['CV1 mod', 'CV2 mod', 'CV3 mod', 'CV4 mod', 'L out', 'R out', 'L+R mix', 'L+R mix'],
```

- [ ] **Step 2: Update `CLAUDE.md`**

- Replace the MidiSrc note's "Resets to TRS on every boot … not persisted" with: MidiSrc + CV targets persist in the option-storage window via `fw/src/settings_store.rs` (16-byte record, debounced ~2 s, defaults TRS/Off on a corrupt or blank record).
- Add a short M5 bullet: three menu cards; CV targets (Knob1–5 = patch knob matrix via `mbsid_knob_set`, Volume/Phase/Detune/Cutoff/Reso via `mbsid_par_set` 0x01–0x05, Pitch/Gate note machine on ch 1); Patch Edit card uses `mbsid_sysex_param` (body + live) so saves capture edits; precedence rule (CV overwrites runtime at 1 kHz, body keeps the menu edit); the status line "All four engines validated…" gains "M5 menu/CV implemented, hardware bring-up pending".

- [ ] **Step 3: Update `README.md`**

Add a "Menu cards" section: card row navigation, CV jack mapping (inputs 0–3), the target list, Pitch/Gate behavior (0 V = C2, gate >2 V on / <1 V off, Pitch alone inert), edit-card volatility contract (unsaved edits discarded by any patch load; `*` = unsaved edits), Lead-only editing.

- [ ] **Step 4: Update `M5_MENU_CARDS_CV_MOD.md` status**

Header `Status:` → `IMPLEMENTED (<date>, commits <first>..<last>) — host tests + oracle green; hardware bring-up pending (§8 checklist).` Also note the two implementation deviations if they stand: pitch quantizer is a closed-form integer routine rather than a 61-entry table (same semantics, satisfies the no-f32 intent), and `MENU_H` grew 194→244.

- [ ] **Step 5: Full verification gates**

```bash
cd gateware/src/top/mbsid && host_oracle/run_oracle.sh; echo "oracle exit: $?"
cd fw && cargo test --target x86_64-unknown-linux-gnu --lib; cd ..
cd ../../../.. # gateware/
pdm mbsid build          # full bitstream
```

Expected: oracle exit 0 (all OK lines, incl. `seq_lead_knobs` × 6 and `param_check`); all cargo tests PASS; full build produces `build/mbsid-r5/*.tar.gz`; check post-route Fmax in `build/mbsid-r5/top.tim` — the **second** `Max frequency for clock '$glbnet$clk'` line must PASS ≥ 60 MHz (expected unchanged: zero gateware delta).

- [ ] **Step 6: Commit**

```bash
git add top.py CLAUDE.md README.md M5_MENU_CARDS_CV_MOD.md
git commit -m "docs(mbsid): M5 implemented — cards, CV modulation, patch edit; labels + persistence notes"
```

---

## Hardware bring-up (out of plan scope, tracked in the spec)

The M5 spec's §8 hardware checklist (CV→knob, CV→cutoff, V/oct tracking, edit→save→reload, settings persistence, dirty star) runs with the M2–M4 pending bring-up; flash via `pdm run flash archive build/mbsid-r5/<hash>.tar.gz`.
