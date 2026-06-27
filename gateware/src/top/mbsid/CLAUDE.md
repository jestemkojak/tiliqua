# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# MBSID-on-Tiliqua (`top/mbsid`)

**Status (2026-06-27): M1 software pipeline implemented; hardware bring-up pending.**
`DESIGN.md` is the approved M1 spec (authoritative for interfaces/milestones/acceptance).
`top.py`, `fw/` (incl. `build.rs`), and the `pdm mbsid build` script all exist on this branch
(`mbsid-port`). Verified green: freestanding compile, host oracle (shim == engine, 6/6
bit-exact), host `cargo test --lib`, full bitstream build, `sync` Fmax 67.25 MHz PASS. The one
thing NOT yet validated is **playback on real hardware** (DESIGN §7 milestones 2–3).

## Vendored engine (not in this repo)

The `mios32/` C++ engine tree is **GPL and gitignored** (kept out of the CERN-OHL-S repo).
A fresh clone has no `mios32/`, so `pdm mbsid build` fails in `fw/build.rs`. Run
**`./fetch-mios32.sh`** once after cloning — it blobless-clones `github.com/midibox/mios32`
(`--filter=blob:none`) and checks out the pinned commit
`44d8e6af401e41a8adf2319ce6a584cce154a14f` into `./mios32` (idempotent).
`fw/csrc/vendor_sources.txt` records the Lead-subset TUs (documentation only; build.rs globs
the whole `core/` tree and drops dead code with `--gc-sections`).

**Static ctors:** riscv-rt never calls `__libc_init_array`, so the engine's global
constructors don't auto-run on target. `fw/init_array.x` (wired in `build.rs`) exposes
`.init_array` bounds and `mbsid_run_static_ctors()` (in `mbsid_shim.cpp`, called from
`mbsid_init()`) walks them — do NOT remove either, or the engine boots uninitialised.

## What this is

Runs the **MBSID Lead** sound engine (mios32 `midibox_sid_v3` C++ core) on the VexiiRiscv
softcore, FFI'd from the firmware Rust, driving **one** gateware reSID (mono) played live
over MIDI. The C++ engine is the mandatory middle layer: zetaSID `.syx` patches are MBSID v2
voice descriptions with **zero** SID-register data — only the engine turns a patch into the
per-tick `sid_regs_t` register image. See `DESIGN.md §1–2`.

## Derives from `top/sid` — reuse it verbatim

The SoC is `top/sid`'s, unchanged: VexiiRiscv @ 60 MHz (`riscv32im`), `SIDPeripheral`,
`Phi2Divider` (φ2 = 1 MHz), MIDI-in CSR FIFO, Timer0. **No CSR changes expected for M1**, so
no PAC regen needed (if any CSR changes, `pdm sid build --pac-only` per the root CLAUDE.md).
Reference the base wiring before writing new code:
- `../sid/top.py` — `SIDPeripheral` (`transaction_data` = `(data<<5)|addr`, 16-bit, depth-16
  FIFO, backpressure via `TxnStatus`/`writable`); `Phi2Divider`; `midi_read` CSR FIFO (firmware
  drains by reading until 0); `Phi2Sel`.
- `../sid/fw/src/main.rs` — the Timer0 ISR + MIDI-in drain pattern to copy. **Replace** its
  per-note `midi_note_to_sid_freq` voice logic with: feed MIDI events to the MBSID engine,
  call `mbsid_tick`, diff `mbsid_regs_l()` against a 32-byte shadow, enqueue only changed
  registers to `SIDPeripheral`. The L image is used; the R image is computed but discarded
  (M1 mono-collapse decision, `DESIGN.md §2`).

## The register-write path (the whole point)

```
MIDI in (midi_read CSR FIFO) ─► mbsid_note_on/off / pitch_bend / cc   (engine state)
Timer0 ISR ─► mbsid_tick(speed_factor) ─► sid_regs_t L image
           ─► Rust diffs L vs 32-byte shadow ─► changed regs (data<<5)|addr ─► SIDPeripheral
           ─► reSID (φ2 = 1 MHz) ─► codec   [everything from the FIFO on is top/sid, reused]
```

## Non-obvious gotchas for this port

- **Control rate is 1 kHz** (`TIMER0_ISR_PERIOD_MS = 1` in `fw/src/main.rs`, NOT base `sid`'s
  5 ms). The engine uses an internal `updateSpeedFactor = 2` (set by the `MbSidEnvironment`
  ctor); the `mbsid_tick` C arg is accepted but ignored. 1 ms matches the JUCE oracle so the
  bit-exact diff is apples-to-apples (`DESIGN.md §8`).
- **C++ is compiled freestanding for the target via `build.rs` + the `cc` crate** (the one piece
  `top/sid` lacks — `sid/fw` has no `build.rs`). Target is **`riscv32im`** (NOT the `imac`
  DESIGN §3 named — matched to the firmware to avoid an atomics/ABI link mismatch); compiler is
  **clang++** (no riscv-gcc on this box). Flags: `--target=riscv32-unknown-elf -march=rv32im
  -mabi=ilp32 -ffreestanding -fno-exceptions -fno-rtti -fno-threadsafe-statics
  -fno-use-cxa-atexit -nostdlib -DMIOS32_FAMILY_EMULATION`, no STL. The whole Lead subset
  compiled with **zero surgery** (the custom heap-free `array<T,N>` in `MbSidSe.h` + the
  `fw/csrc/mios32_shim/` facade absorbed every `new`/`std::`/`printf`).
- **The Lead subset does NOT self-link.** `MbSid` aggregates the Bassline/Drum/Multi SEs +
  `MbSidAsid` **by value**, so `build.rs` compiles the **whole** `core/` + `components/` tree
  (every `*.cpp` except `app.cpp`) + `sid.c`/`notestack.c`/`jsw_rand.c`, then `-ffunction/
  data-sections` + link `--gc-sections` drop the dead non-Lead code. `vendor_sources.txt` is
  now documentation only (build.rs globs, doesn't read it).
- **Static ctors don't auto-run** — see the "Static ctors" note above. `mbsid_run_static_ctors()`
  (walks `.init_array` via `fw/init_array.x`) is the reason the engine's speed-factor + RNG seed
  actually get applied on target. The host oracle CANNOT catch this (host libc runs ctors).
- **All engine state lives in `.bss`** via one `mbsid_shim.cpp` (the only C++ we author) owning
  a single `MbSid` + `MbSidClock` + two static `sid_regs_t`. No allocator. Shim ABI is in
  `DESIGN.md §4`; the 512-byte patch buffer is exactly what `zsid/zetasid_syx.py` emits (same
  `sid_patch_t` layout — no re-encoding).
- **Oracle validation is the keystone, runnable entirely on PC before any FPGA work.** Build
  the same `.cpp` + `mbsid_shim.cpp` for x86, run an identical `(patch, note-sequence)` through
  both it and the instrumented JUCE port, diff the **L** register streams — must be
  byte-identical on ≥3 Lead patches. Do this (`DESIGN.md §6`, milestone 1) before gateware.
- **`mainram_size` is bumped to `0x8000`** (`MBSIDSoc` subclasses `SIDSoc` in `top.py`; sid's
  default is `0x4000`). The by-value engine aggregation lands ~6.9 KB `.bss` + needs stack room;
  measured `.bss` 6884 B + stack 25880 B fits 0x8000. If you add firmware state, watch RAM.
- **GPL.** Linking the MBSID C++ into the firmware makes the distributed bitstream firmware
  GPL (fine for personal/open use). The zetaSID Cortex-M binary is proprietary — never touched
  or disassembled.

## Build & test

- `cd gateware && pdm mbsid build` — full bitstream (the `mbsid` script is registered in
  `[tool.pdm.scripts]`). `--fw-only` relinks firmware fast (reuses the bitstream; ends with an
  expected `missing top.bit` after the ELF is built). Flashable archive lands at
  `build/mbsid-r5/*.tar.gz`.
- Host firmware tests: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib` (the
  `riscv32` FFI is cfg-stubbed on host; `regdiff` is host-pure).
- **Oracle (the keystone):** `host_oracle/run_oracle.sh` — builds the engine + shim for x86 and
  diffs the L register stream of `oracle` vs `shim_driver` across 3 Lead presets × 2 sequences;
  must be 6/6 byte-identical. Re-run after any change to the shim, facade, or engine subset.
