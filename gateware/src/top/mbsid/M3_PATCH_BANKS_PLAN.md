# MBSID Patch Banks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make all 128 MBSID factory "vintage bank" patches selectable live over MIDI Program Change (headless), reusing the engine's native `bankLoad` path.

**Architecture:** Add one `extern "C"` shim function `mbsid_program_change(patch)` → `env.bankLoad(0,0,patch&0x7F)`; referencing it un-strips the 64 KB factory bank (`sid_bank_preset_0`) already in the vendored engine. The firmware MIDI parser gains a `ProgramChange` arm, and boot loads a default bank index instead of a hand-copied patch array. No gateware, no CSR, no PAC changes.

**Tech Stack:** C++ (freestanding, clang++ for target / g++ for host oracle), Rust (`no_std` riscv32im firmware + host-stubbed lib), Amaranth/pdm build, mios32 MBSIDv3 engine.

**Spec:** `gateware/src/top/mbsid/M3_PATCH_BANKS.md`.

## Global Constraints

- Target firmware arch: **riscv32im** (`riscv32-unknown-elf`, `-mabi=ilp32`); host arch for tests/oracle: **x86_64**. FFI wrappers are split by `#[cfg(target_arch = "riscv32")]` vs `#[cfg(not(target_arch = "riscv32"))]` (host stub).
- C++ shim crosses the FFI boundary with **`<stdint.h>` types only** — no mios32 types in `mbsid_shim.h`.
- The shim owns a single anonymous-namespace `MbSidEnvironment env;`; `bankLoad` is `env.bankLoad(u8 sid, u8 bank, u8 patch)` and is currently `--gc-sections`-stripped (un-stripped by referencing it).
- `BOOT_PATCH_INDEX = 123` (A124 "Crazy Lead"); it is a **0-based bank slot = MIDI Program Change value = patch number − 1**, and **must** be a Lead-engine slot.
- Program Change is accepted on **any** MIDI channel (single-timbre, headless).
- The host oracle (`host_oracle/run_oracle.sh`) is the keystone gate: shim traces must be **byte-identical** to the engine-reference traces (L **and** R) across every sequence × patch.
- Commit messages end with the project trailer `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- Run all `pdm`/`cargo`/oracle commands from the directories shown; `pdm` lives in `gateware/`, not the repo root.

---

### Task 1: C shim `mbsid_program_change` + host-oracle `pc` coverage

Adds the shim function and proves — via the existing oracle keystone — that it is byte-for-byte equivalent to the engine's own `bankLoad`. Mirrors the existing `patch <row>` harness command with a new `pc <row>` command.

**Files:**
- Modify: `host_oracle/seq.h` (add `PC` event kind: enum, parse, dispatch, doc comment)
- Modify: `host_oracle/oracle.cpp` (add `program_change` backend method → `env.bankLoad`)
- Modify: `host_oracle/shim_driver.cpp` (add `program_change` backend method → `mbsid_program_change`)
- Modify: `fw/csrc/mbsid_shim.h` (declare `mbsid_program_change`)
- Modify: `fw/csrc/mbsid_shim.cpp` (define `mbsid_program_change`)
- Modify: `host_oracle/run_oracle.sh` (run both a `patch` pass and a `pc` pass)
- Test: `host_oracle/run_oracle.sh`

**Interfaces:**
- Produces (C ABI): `void mbsid_program_change(uint8_t patch);` — loads factory bank slot `patch & 0x7F` via `env.bankLoad(0,0,...)`. Consumed by Task 2 (sweep) and Task 3 (firmware FFI).
- Produces (host backend method): `int program_change(int patch)` on both oracle and shim backends, dispatched by `seq.h`'s `PC` event.

- [ ] **Step 1: Add the `pc` event to the harness (the failing test scaffolding)**

In `host_oracle/seq.h`, extend the event kind enum (line 30):

```cpp
    enum Kind { PATCH, PC, ON, OFF, CC, BEND, END } kind;
```

Add the parse branch after the `patch` branch (after line 51):

```cpp
        if      (!strcmp(ev, "patch")) e.kind = SeqEvent::PATCH;
        else if (!strcmp(ev, "pc"))    e.kind = SeqEvent::PC;
```

Add the dispatch case after the `PATCH` case (after line 99):

```cpp
            case SeqEvent::PATCH: be.load_patch(e.a);        break;
            case SeqEvent::PC:    be.program_change(e.a);    break;
```

Update the format doc comment (after line 8) and the Backend-concept comment (after line 66):

```cpp
 *     <t_ms> patch <row>        select sid_bank_preset_0[row] via sysexSetPatch
 *     <t_ms> pc    <row>        select sid_bank_preset_0[row] via bankLoad (Program Change path)
```
```cpp
 *   int           program_change(int patch); // bankLoad(0,0,patch); 0 = ok
```

In `host_oracle/oracle.cpp`, add the backend method next to `load_patch` (after the `load_patch` method, ~line 41):

```cpp
    int program_change(int patch) { return env.bankLoad(/*sid*/0, /*bank*/0, (u8)patch); }
```

In `host_oracle/shim_driver.cpp`, add the backend method next to `load_patch`:

```cpp
    int  program_change(int patch)   { mbsid_program_change((uint8_t)patch); return 0; }
```

In `host_oracle/run_oracle.sh`, replace the inner test loop (the `for row in "${PATCHES[@]}"` block) with a version that runs both modes:

```bash
for seq in "$HERE"/sequences/*.txt; do
    seqname="$(basename "$seq" .txt)"
    for row in "${PATCHES[@]}"; do
        for mode in patch pc; do
            tmp="$BUILD/${seqname}_${mode}${row}.seq"
            { echo "0 $mode $row"; cat "$seq"; } > "$tmp"
            "$BUILD/oracle"      "$tmp" > "$BUILD/oracle.trace"
            "$BUILD/shim_driver" "$tmp" > "$BUILD/shim.trace"
            if diff -u "$BUILD/oracle.trace" "$BUILD/shim.trace"; then
                echo "OK: $seqname $mode=$row"
            else
                echo "DIFF: $seqname $mode=$row"
                fail=1
            fi
        done
    done
done
```

- [ ] **Step 2: Run the oracle to verify it fails (shim function missing)**

Run: `cd gateware/src/top/mbsid/host_oracle && ./run_oracle.sh`
Expected: **FAIL** at the `shim_driver` build/link step — `mbsid_program_change` is undeclared/undefined (the script uses `set -e`, so it aborts before any diff). The `oracle` binary builds fine (`env.bankLoad` already exists).

- [ ] **Step 3: Declare the shim function**

In `fw/csrc/mbsid_shim.h`, add after the `mbsid_load_patch` line (line 16):

```c
void           mbsid_program_change(uint8_t patch);     /* load factory bank slot (patch & 0x7F) via bankLoad */
```

- [ ] **Step 4: Define the shim function**

In `fw/csrc/mbsid_shim.cpp`, add after `mbsid_load_patch` (after line 55):

```cpp
extern "C" void mbsid_program_change(uint8_t patch) {
    // Load factory bank slot patch&0x7F (0..127) via the engine's native bank
    // path. Referencing bankLoad un-strips sid_bank_preset_0 (the 64 KB factory
    // bank) and the bank code. bankLoad internally clamps bank>=SID_BANK_NUM and
    // patch>=128; the mask makes every MIDI Program Change value map 1:1.
    env.bankLoad(/*sid*/0, /*bank*/0, patch & 0x7F);
}
```

- [ ] **Step 5: Run the oracle to verify it passes (byte-identical, both modes)**

Run: `cd gateware/src/top/mbsid/host_oracle && ./run_oracle.sh; echo "exit=$?"`
Expected: every line `OK: <seq> patch=<row>` and `OK: <seq> pc=<row>` (12 OK lines: 2 sequences × 3 patches × 2 modes), **no `DIFF:` lines**, `exit=0`. The `pc` rows prove `mbsid_program_change` wraps `env.bankLoad` with zero divergence from the engine reference.

- [ ] **Step 6: Commit**

```bash
cd /home/pawel/code/tiliqua && git add gateware/src/top/mbsid/fw/csrc/mbsid_shim.h gateware/src/top/mbsid/fw/csrc/mbsid_shim.cpp gateware/src/top/mbsid/host_oracle/seq.h gateware/src/top/mbsid/host_oracle/oracle.cpp gateware/src/top/mbsid/host_oracle/shim_driver.cpp gateware/src/top/mbsid/host_oracle/run_oracle.sh
git commit -m "feat(mbsid): add mbsid_program_change shim (bankLoad) + oracle pc coverage

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Host no-crash sweep over all 128 factory patches

Proves the spec's safety claim entirely on PC: loading any factory patch — including the 9 non-Lead (Bassline/Drum/Multi) patches, which dispatch into the linked non-Lead SEs — runs without crashing or hanging. A segfault fails the process; a hang is caught by a `timeout` wrapper.

**Files:**
- Create: `host_oracle/sweep_driver.cpp`
- Modify: `host_oracle/run_oracle.sh` (build + run the sweep under `timeout`)
- Test: `host_oracle/run_oracle.sh`

**Interfaces:**
- Consumes: `mbsid_program_change` (Task 1), plus existing `mbsid_init`/`mbsid_note_on`/`mbsid_note_off`/`mbsid_tick`.

- [ ] **Step 1: Create the sweep driver**

Create `host_oracle/sweep_driver.cpp`:

```cpp
/* sweep_driver.cpp — no-crash sweep over ALL 128 factory patches via the shim.
 *
 * Loads each sid_bank_preset_0[row] through mbsid_program_change (the bankLoad
 * path), plays a short note, and ticks the engine. Asserts only that the engine
 * RUNS to completion for every patch — including the 9 non-Lead patches, which
 * dispatch into the linked Bassline/Drum/Multi SEs (verified present in the ELF,
 * 24-26 symbols each). A segfault fails the process exit code; a hang is caught
 * by the `timeout` wrapper in run_oracle.sh. Proves "non-Lead patches don't
 * freeze the SoC" entirely on PC.
 */
#include <cstdint>
#include <cstdio>
#include "mbsid_shim.h"

int main() {
    mbsid_init();
    for (int row = 0; row < 128; ++row) {
        mbsid_program_change((uint8_t)row);
        mbsid_note_on(60, 100);
        for (int t = 0; t < 16; ++t) mbsid_tick(2);
        mbsid_note_off(60);
        for (int t = 0; t < 4; ++t)  mbsid_tick(2);
    }
    printf("SWEEP OK: 128 patches\n");
    return 0;
}
```

- [ ] **Step 2: Wire the sweep into run_oracle.sh**

In `host_oracle/run_oracle.sh`, after the `mbsid_shim.o` compile line (`"$CXX" $CXXFLAGS -c "$SHIM/mbsid_shim.cpp" -o "$BUILD/mbsid_shim.o"`), add the sweep build:

```bash
# --- no-crash sweep driver (shim side only) ---
"$CXX" $CXXFLAGS -c "$HERE/sweep_driver.cpp" -o "$BUILD/sweep_driver.o"
"$CXX" $CXXFLAGS "$BUILD/sweep_driver.o" "$BUILD/mbsid_shim.o" "${ENGINE_OBJS[@]}" "${C_OBJS[@]}" "$BUILD/host_stubs.o" -o "$BUILD/sweep_driver"
```

Then, immediately before the final `exit $fail` line, add the run:

```bash
echo "=== no-crash sweep (all 128 factory patches, incl. non-Lead) ==="
if timeout 30 "$BUILD/sweep_driver"; then
    echo "OK: no-crash sweep"
else
    echo "FAIL: no-crash sweep (crash or hang, exit $?)"
    fail=1
fi
```

- [ ] **Step 3: Run the oracle to verify the sweep passes**

Run: `cd gateware/src/top/mbsid/host_oracle && ./run_oracle.sh; echo "exit=$?"`
Expected: the bit-exact `OK:` lines from Task 1, then `SWEEP OK: 128 patches`, then `OK: no-crash sweep`, and `exit=0`. (If a non-Lead patch did jump into stripped code, the process would segfault and print `FAIL: no-crash sweep`.)

- [ ] **Step 4: Commit**

```bash
cd /home/pawel/code/tiliqua && git add gateware/src/top/mbsid/host_oracle/sweep_driver.cpp gateware/src/top/mbsid/host_oracle/run_oracle.sh
git commit -m "test(mbsid): host no-crash sweep over all 128 factory patches

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Firmware Program Change routing + boot default A124; retire patch.rs

Adds the Rust FFI wrapper, the `ProgramChange` MIDI parse arm, switches boot to the bank path with `BOOT_PATCH_INDEX = 123` (A124 "Crazy Lead"), and deletes the now-redundant hand-copied boot patch.

**Files:**
- Modify: `fw/src/mbsid_sys.rs` (FFI decl + riscv wrapper + host stub for `program_change`)
- Modify: `fw/src/main.rs` (ProgramChange arm; `BOOT_PATCH_INDEX`; boot via `program_change`; drop `PATCH` import)
- Modify: `fw/src/lib.rs` (remove `pub mod patch;`)
- Delete: `fw/src/patch.rs`
- Test: host `cargo test --lib`; `pdm mbsid build --fw-only`; ELF symbol check

**Interfaces:**
- Consumes (C ABI): `mbsid_program_change(patch: u8)` from Task 1.
- Produces (Rust): `tiliqua_fw::mbsid_sys::program_change(patch: u8)` — riscv calls the shim, host is a no-op stub.

- [ ] **Step 1: Add the Rust FFI declaration + wrappers**

In `fw/src/mbsid_sys.rs`, add to the `#[cfg(target_arch = "riscv32")] extern "C"` block (after line 7, `fn mbsid_load_patch`):

```rust
    fn mbsid_program_change(patch: u8);
```

Add the riscv wrapper after the `load_patch` wrapper (after line 45):

```rust
#[cfg(target_arch = "riscv32")]
pub fn program_change(patch: u8) {
    unsafe { mbsid_program_change(patch) }
}
```

Add the host stub after the `load_patch` host stub (after line 85):

```rust
#[cfg(not(target_arch = "riscv32"))]
pub fn program_change(_patch: u8) {}
```

- [ ] **Step 2: Verify host lib still compiles + existing tests pass**

Run: `cd gateware/src/top/mbsid/fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: PASS (the regdiff tests still pass; the new `program_change` host stub compiles). This is the host-side guard that the FFI surface change didn't break the host build.

- [ ] **Step 3: Add the ProgramChange MIDI arm + boot default; drop patch.rs usage**

In `fw/src/main.rs`, remove the import (line 34):

```rust
use tiliqua_fw::patch::PATCH;   // DELETE THIS LINE
```

Add a boot-index constant near the top of the file (next to the other `const`/config items, e.g. just below the `TIMER0_ISR_PERIOD_MS` definition):

```rust
// Boot patch = factory bank slot loaded at power-on. 0-based slot index =
// MIDI Program Change value = (patch number - 1). 123 = A124 "Crazy Lead".
// MUST be a Lead-engine slot, or the synth boots with a wrong-sounding
// non-Lead patch (the 9 non-Lead slots are 15, 32-35, 60, 98, 99, 106).
const BOOT_PATCH_INDEX: u8 = 123;
```

Add the Program Change arm inside the `match msg` block, after the `ControlChange` arm (after line 139, before `_ => {}`):

```rust
                    // Program Change -> load factory bank patch N (0..127) via
                    // the engine bankLoad path. Accepted on any MIDI channel.
                    MidiMessage::ProgramChange(_ch, prog) => {
                        mbsid_sys::program_change(u8::from(prog));
                    }
```

Replace the boot load (line 175, `mbsid_sys::load_patch(&PATCH);`) with:

```rust
    mbsid_sys::program_change(BOOT_PATCH_INDEX);
```

- [ ] **Step 4: Remove the patch module and delete patch.rs**

In `fw/src/lib.rs`, remove the line:

```rust
pub mod patch;   // DELETE THIS LINE
```

Delete the file:

```bash
git rm gateware/src/top/mbsid/fw/src/patch.rs
```

- [ ] **Step 5: Build the firmware and verify the bank is now linked**

Run: `cd gateware && pdm mbsid build --fw-only`
Expected: the Rust crate + C shim compile; the firmware ELF `src/top/mbsid/fw/target/riscv32im-unknown-none-elf/release/tiliqua-fw` is produced; the script then ends with the **expected** `missing top.bit` error (per CLAUDE.md, `--fw-only` reuses the bitstream — the ELF is still built). If the build fails *before* `missing top.bit`, that is a real error (grep stdout for the cause).

Then verify the factory bank path is now in the ELF (it was absent before this milestone):

```bash
cd gateware/src/top/mbsid/fw/target/riscv32im-unknown-none-elf/release && \
  llvm-nm tiliqua-fw | grep -c bankLoad
```
Expected: **≥ 1** (was `0` pre-milestone — referencing `bankLoad` un-stripped it and the 64 KB `sid_bank_preset_0`). Sanity-check `.text` grew ~64 KB vs the pre-milestone size via `llvm-size tiliqua-fw`.

- [ ] **Step 6: Commit**

```bash
cd /home/pawel/code/tiliqua && git add gateware/src/top/mbsid/fw/src/mbsid_sys.rs gateware/src/top/mbsid/fw/src/main.rs gateware/src/top/mbsid/fw/src/lib.rs
git commit -m "feat(mbsid): MIDI Program Change -> factory bank; boot A124; retire patch.rs

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Full bitstream build + hardware validation

Integration gate: build the complete bitstream, confirm no timing regression, flash, and verify playback on real hardware. (Manual hardware step — no automated assertion.)

**Files:** none modified (build/flash/verify only).

**Interfaces:** Consumes the firmware from Task 3.

- [ ] **Step 1: Full bitstream build**

Run: `cd gateware && pdm mbsid build`
Expected: build completes; a flashable archive lands at `build/mbsid-r5/*.tar.gz`.

- [ ] **Step 2: Confirm no `sync` timing regression**

Read the **post-route** Fmax — the **second** occurrence of `Max frequency for clock '$glbnet$clk'` in `gateware/build/mbsid-r5/top.tim` (~line 1390+, NOT the first ~line 345 pre-route estimate):

```bash
grep -n "Max frequency for clock '\$glbnet\$clk'" gateware/build/mbsid-r5/top.tim
```
Expected: `sync` PASS at ~67 MHz (unchanged — this milestone adds no gateware and no `sync`-domain logic; only ~64 KB flash `.rodata`).

- [ ] **Step 3: Flash the bitstream**

Run: `cd gateware && pdm run flash archive build/mbsid-r5/*.tar.gz`
Expected: flash completes without error.

- [ ] **Step 4: Hardware playback check (manual)**

- Power-on / boot: the synth should sound as **A124 "Crazy Lead"** when you play MIDI notes (the boot default).
- Send a **MIDI Program Change** to a different **Lead** index (e.g. PC 9 = A010 "WT Flute", PC 51 = A052 "Nice Lead"): the timbre must audibly change.
- Note: the 9 non-Lead slots (PC 15, 32-35, 60, 98, 99, 106) may sound wrong/silent — expected this milestone, and confirmed crash-safe by Task 2.

- [ ] **Step 5: Commit (only if any tracked files changed)**

This task changes no source files (build artifacts under `gateware/build/` are not committed). If nothing tracked changed, skip the commit. Record the hardware result and measured Fmax in the PR/notes.

---

### Task 5: Documentation corrections

Corrects the inaccurate "dead-stripped" claim that the safety argument depends on, and marks the spec's milestone slice done.

**Files:**
- Modify: `gateware/src/top/mbsid/CLAUDE.md`
- Modify: `gateware/src/top/mbsid/DESIGN.md`

**Interfaces:** none (docs only).

- [ ] **Step 1: Correct the CLAUDE.md "dead-stripped" claim**

In `gateware/src/top/mbsid/CLAUDE.md`, find the sentence under the gotcha "**The Lead subset does NOT self-link.**" ending with `then -ffunction/data-sections + link --gc-sections drop the dead non-Lead code.` Replace that trailing clause with:

```
then `-ffunction/data-sections` + link `--gc-sections` drop genuinely-unreferenced
code (`app.cpp`, the SysEx-ACK/`sprintf` paths, `MbSidAsid`). NOTE: the Bassline/Drum/
Multi SEs are **not** dropped — `MbSid::updatePatch` references them via `&mbSidSe*`
+ virtual dispatch, so they stay linked (verified: 24–26 symbols each in the ELF).
This is why loading a non-Lead patch is crash-safe (it dispatches to a real engine).
```

Also fix the "## Build & test" / status area if it repeats the "dead-stripped" phrasing: grep the file for `dead-strip` / `dead non-Lead` and correct any remaining occurrence to match the above.

Run to find all occurrences first:
```bash
grep -rn "dead-strip\|dead non-Lead\|gc-sections" gateware/src/top/mbsid/CLAUDE.md
```

- [ ] **Step 2: Correct DESIGN.md §10 and mark the milestone slice done**

In `gateware/src/top/mbsid/DESIGN.md` §10 "Further deferred", replace the Bassline/Drum/Multi bullet:

```
- **Bassline / Drum / Multi engines.** Already compiled into `libmbsid.a` and **linked**
  (not dead-stripped — `MbSid::updatePatch` references them via `&mbSidSe*` + virtual
  dispatch; 24–26 symbols each in the ELF). Enabling is firmware UI/routing + RAM budget,
  not new vendoring or freestanding-port work.
```

And update the patch-bank bullet:

```
- **Patch bank storage.** Read-only ROM-baked factory bank **done** — see
  `M3_PATCH_BANKS.md` (all 128 factory patches selectable over MIDI Program Change).
  Writable user banks (flash) and a browse UI remain deferred.
```

- [ ] **Step 3: Commit**

```bash
cd /home/pawel/code/tiliqua && git add gateware/src/top/mbsid/CLAUDE.md gateware/src/top/mbsid/DESIGN.md
git commit -m "docs(mbsid): correct non-Lead 'dead-stripped' claim; mark factory-bank done

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the implementer

- **Do not 'fix the trace.'** If the oracle's `pc` pass shows a `DIFF:`, the shim mis-wraps `bankLoad` — fix the shim, never the expected trace (`host_oracle/run_oracle.sh` header says the same).
- **`mbsid_load_patch` stays.** It loses its firmware caller (boot now uses `program_change`) but the host oracle's `patch <row>` command still exercises it; do not delete it.
- **`llvm-nm` / `llvm-size`** are the LLVM binutils; if absent, `riscv64-unknown-elf-nm`/`-size` or `nm`/`size` on the ELF work equivalently for the symbol/size checks.
