# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# MBSID-on-Tiliqua (`top/mbsid`)

**Status (2026-06-27): design-only.** The only file here is `DESIGN.md` — the approved M1
spec (read it first; it is authoritative for interfaces, milestones, and acceptance tests).
No `top.py` / `fw/` / `build.rs` exist yet. This branch (`mbsid-port`) implements that spec.

## Vendored engine (not in this repo)

The `mios32/` C++ engine tree is **GPL and gitignored** (kept out of the CERN-OHL-S repo).
A fresh clone has no `mios32/`, so `pdm mbsid build` fails in `fw/build.rs`. Run
**`./fetch-mios32.sh`** once after cloning — it shallow-clones `github.com/midibox/mios32`
at the pinned commit `44d8e6af401e41a8adf2319ce6a584cce154a14f` into `./mios32` (idempotent).
`fw/csrc/vendor_sources.txt` records the Lead-subset TUs (documentation only; build.rs globs
the whole `core/` tree and drops dead code with `--gc-sections`).

**Static ctors:** riscv-rt never calls `__libc_init_array`, so the engine's global
constructors don't auto-run on target. `fw/init_array.x` (wired in `build.rs`) exposes
`.init_array` bounds and `mbsid_run_static_ctors()` (in `mbsid_shim.cpp`, called from
`mbsid_init()`) walks them — do NOT remove either, or the engine boots uninitialised.

## What this is

Runs the **MBSID Lead** sound engine (mios32 `midibox_sid_v3` C++ core) on the VexiiRiscv
softcore, FFI'd from the firmware Rust, driving **one** gateware reSID (mono) played live
over MIDI. The C++ engine is the mandatory middle layer: MBSID v2 `.syx` patches are MBSID v2
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

- **Control rate must be 1 kHz, but base `sid` ticks at 5 ms.** `sid/fw/src/main.rs` sets
  `TIMER0_ISR_PERIOD_MS = 5`. The oracle (JUCE port) runs the engine at **1 kHz**, so the
  bit-exact diff is only apples-to-apples if the ISR calls `mbsid_tick` at 1 ms. Set the
  mbsid ISR period to 1 ms and confirm the `speed_factor` value the JUCE port passes to
  `tick()`, then mirror it (`DESIGN.md §8`).
- **C++ is compiled freestanding for the target via `build.rs` + the `cc` crate** (this is the
  one piece `top/sid` does *not* have — `sid/fw` has no `build.rs`). Flags: target
  `riscv32imac`, `-ffreestanding -fno-exceptions -fno-rtti -fno-threadsafe-statics`, no STL.
  The engine's custom `array<T,N>` (in `MbSidSe.h`) is heap-free; any stray `new`/`std::` in a
  `.cpp` must be triaged to static storage (Spike 0, `DESIGN.md §3,§7`). A single resistant
  component is a candidate for a Rust rewrite — not the whole engine.
- **All engine state lives in `.bss`** via one `mbsid_shim.cpp` (the only C++ we author) owning
  a single `MbSid` + `MbSidClock` + two static `sid_regs_t`. No allocator. Shim ABI is in
  `DESIGN.md §4`; the 512-byte patch buffer is exactly what `the reference `.syx` encoder script` emits (same
  `sid_patch_t` layout — no re-encoding).
- **Oracle validation is the keystone, runnable entirely on PC before any FPGA work.** Build
  the same `.cpp` + `mbsid_shim.cpp` for x86, run an identical `(patch, note-sequence)` through
  both it and the instrumented JUCE port, diff the **L** register streams — must be
  byte-identical on ≥3 Lead patches. Do this (`DESIGN.md §6`, milestone 1) before gateware.
- **`mainram_size`** may need bumping from `top/sid`'s value if engine state + framebuffer
  working set overflows BRAM (~49 KB free on the 25F). Decide empirically during bring-up.
- **GPL.** Linking the MBSID C++ into the firmware makes the distributed bitstream firmware
  GPL (fine for personal/open use).

## Build (once `top.py` + the pdm script exist)

`cd gateware && pdm mbsid build` — mirrors `pdm sid build`. The script
`mbsid = "src/top/mbsid/top.py"` must be **added** to `[tool.pdm.scripts]` in
`gateware/pyproject.toml` (it is not registered yet). Firmware host tests follow the repo
pattern: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib`.
