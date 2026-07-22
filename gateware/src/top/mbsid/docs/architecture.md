# MBSID-on-Tiliqua — Architecture

How the project works, layer by layer. Interfaces and acceptance criteria
are owned by the design specs ([`../DESIGN.md`](../DESIGN.md) and the
`M2`–`M5` docs); this document is the explanatory map.

## The core idea

MBSID `.syx` patches are **voice descriptions** — 6 oscillators,
2 filters, 6 LFOs, multi-stage envelopes, a mod matrix, wavetable
sequencers — and contain **zero SID register data**. A bare SID (or reSID)
emulation is just the dumb chip. The mandatory middle layer is the MBSID
firmware engine, which interprets a patch and writes the SID registers
every control tick.

Rather than reimplement ~8k lines of synthesis logic, this project
cross-compiles the **original mios32 C++ engine**
(`midibox_sid_v3/core`, the portable rewrite that contains the v2 engine)
for the RISC-V softcore and drives Tiliqua's gateware reSIDs with its
output. The porting rule throughout: **reuse upstream, never rewrite** —
if the engine already has a feature (patch banks, SysEx, knobs), plumb it
through; don't duplicate it in Rust.

## Data flow (the whole point)

USB-C is one host port, muxed one-at-a-time between a MIDI engine and a USB
mass-storage engine (`USB Mode` on the Main menu card, M6); TRS MIDI stays
live either way. The note/CC path below is what's live in `USB Mode=MIDI`;
`USB Mode=Storage` instead routes the port to the `usb_msc` CSR for `.syx`
patch import/export (see [Layer 1](#layer-1--gateware-toppy) below) and is
omitted from this diagram for clarity.

```
MIDI in (TRS / USB host)
      │  gateware decode → midi_read CSR FIFO
      ▼
┌────────────────────────────────────────────────────────────┐
│ VexiiRiscv SoC @ 60 MHz (riscv32im)                        │
│                                                            │
│  main loop: menu/display, patch load/save, settings flash  │
│                                                            │
│  TIMER0 ISR @ 1 kHz:                                       │
│    drain midi_read ──► mbsid_note_on/off/cc/pitch_bend/…   │
│    drain sysex_read ──► engine (audition) + SysexCapture   │
│    CV tick ──► mbsid_knob_set / mbsid_par_set / CV notes   │
│    mbsid_tick() ──► sid_regs_t L image ─ RegDiff ─► SID L  │
│                 └─► sid_regs_t R image ─ RegDiff ─► SID R  │
└──────────────┬─────────────────────────┬───────────────────┘
     (data<<5)|addr writes      (data<<5)|addr writes
               ▼                         ▼
     SIDPeripheral (0x1000)     SIDPeripheral_R (0x1200)
     depth-16 FIFO, CDC          depth-16 FIFO, CDC
               ▼                         ▼
     reSID #0 (8580)             reSID #1 (8580)
     30 MHz domain, φ2 = 1 MHz   30 MHz domain, φ2 = 1 MHz
               │ 3-voice filtered mix    │
               ▼                         ▼
            out0 (L)                  out1 (R)
                    └── (L+R)>>1 ──► out2 / out3
```

Every control tick (1 ms) the engine produces two full 32-byte SID register
images. The firmware diffs each image against a shadow copy and enqueues
**only changed registers** to the corresponding peripheral FIFO — the same
strategy the upstream JUCE desktop port uses against a software reSID.

## Layer 1 — Gateware (`top.py`)

`MBSIDSoc` subclasses `top/sid`'s `SIDSoc` and changes **almost nothing**:

```python
kwargs.setdefault("mainram_size", 0x8000)          # engine .bss + stack need 32 KB
kwargs.setdefault("with_scope", False)             # LUT budget for the 2nd SID
kwargs.setdefault("n_sids", 2)                     # M2: stereo dual-SID
kwargs.setdefault("with_sysex", True)              # M4: SysEx sideband CSR
kwargs.setdefault("with_usb_msc", True)            # M6: USB mass-storage host
kwargs.setdefault("usb_msc_fullspeed_only", True)  # M6 round 8: avoid HS wedges
```

Everything of substance lives in `top/sid/top.py` and is reused verbatim:

- **`SIDPeripheral`** — a CSR-driven write port to one reSID. Firmware
  writes 16-bit transactions encoded `(data << 5) | addr` into a depth-16
  FIFO; `TxnStatus`/`writable` provides backpressure. M2 instantiates a
  second one (`SID_PERIPH_R` at offset `0x1200`) for the right SID.
- **Clocking** — the reSID cores and their φ2 dividers run in a dedicated
  **30 MHz `sid` clock domain** (the reSID filter's combinational muladd
  fails timing at 60 MHz). Writes cross `sync → sid` via AsyncFIFO; the
  audio-sample strobe is pulse-synchronized back. φ2 (the emulated chip
  clock) is 1 MHz.
- **MIDI in** — TRS and USB-host MIDI are decoded in gateware and muxed
  (selected by the `usb_midi_host` CSR bit, which the menu's `MIDI Src`
  row writes) into the `midi_read` CSR FIFO. Firmware drains it by reading
  until it returns 0.
- **SysEx sideband (M4)** — normal MIDI decode drops SysEx, so an opt-in
  `forward_sysex` path taps raw SysEx bytes into a separate `sysex_read`
  CSR (offset `0x24`). Because `0x00` is a legal SysEx data byte, this CSR
  uses a **valid-bit read** (bit 8 = valid, bits 7:0 = data) — *not* the
  `midi_read` "read until 0" idiom.
- **Timer0** — the 1 kHz ISR heartbeat (and the only usable time source:
  VexiiRiscv here has no `mcycle` CSR; reading it traps).
- **USB mass-storage (M6)** — one `UTMITranslator` owns the physical ULPI
  PHY; a `USBMIDIHost` and a `USBMSCHost` are both instantiated against it
  and each held in reset by a `ResetInserter({"usb": storage_mode})` term
  (one is always in reset, the other live) — a mode flip re-enumerates
  whichever engine just came out of reset. Storage mode forces VBUS on
  unconditionally (a thumb drive needs bus power even with no MIDI host
  running) and firmware only ever writes the `usb_midi_host` CSR bit when
  **not** in storage mode, so entering Storage silently falls back to TRS
  as the live MIDI source. The storage engine exposes a `usb_msc` CSR block
  at offset `0x1300` (read path + `0x20`/`0x24`/`0x3C` write/flush path,
  `M6_USB_STORAGE.md`) that firmware drains through `fw/src/usb_msc.rs`.

## Layer 2 — The vendored C++ engine (`mios32/`, not in the repo)

- **GPL and gitignored.** `./fetch-mios32.sh` blobless-clones
  `github.com/midibox/mios32` and pins commit `44d8e6af…`. A fresh clone
  without this step fails in `fw/build.rs`.
- `fw/build.rs` compiles the **whole** `core/` + `components/` tree (every
  `.cpp` except `app.cpp`) plus `sid.c`/`notestack.c`/`jsw_rand.c`
  freestanding for `riscv32im` with **clang++**
  (`--target=riscv32-unknown-elf -march=rv32im -mabi=ilp32 -ffreestanding
  -fno-exceptions -fno-rtti -fno-threadsafe-statics -fno-use-cxa-atexit
  -nostdlib -DMIOS32_FAMILY_EMULATION`), no STL. Link-time `--gc-sections`
  drops genuinely unreferenced code.
- All four sound engines (Lead/Bassline/Drum/Multi) stay linked, because
  `MbSid` aggregates them **by value** and dispatches virtually — which is
  also why loading any patch type is crash-safe.
- **`fw/csrc/mios32_shim/`** is a small facade of mios32 platform headers
  (MIDI device ID, timing, printf-stubs) that absorbed every platform
  dependency — the engine compiled with **zero source surgery**. Vendored
  C++ is never edited.
- **Static constructors don't auto-run** under riscv-rt. `fw/init_array.x`
  exposes `.init_array` bounds and `mbsid_run_static_ctors()` (called from
  `mbsid_init()`) walks them. Without this the engine boots uninitialised
  (wrong speed factor, unseeded RNG) — and the host oracle *cannot* catch
  it, because host libc runs ctors natively.

## Layer 3 — The `extern "C"` shim (`fw/csrc/mbsid_shim.cpp`)

The only C++ this project authors. It owns a single static
`MbSid` + `MbSidClock` + two `sid_regs_t` — all engine state in `.bss`, no
allocator. The ABI (spec: `DESIGN.md §4`, extended in M3–M5):

- lifecycle: `mbsid_init`, `mbsid_run_static_ctors`
- MIDI: `mbsid_note_on/off`, `mbsid_pitch_bend`, `mbsid_cc`,
  `mbsid_aftertouch` (all take the real MIDI channel as first arg),
  `mbsid_sysex_byte`
- patches: `mbsid_load_patch` (512-byte `sid_patch_t`, same layout
  `the reference `.syx` encoder script` emits), `mbsid_program_change` (engine `bankLoad`),
  patch-body peek/poke + `mbsid_sysex_param` for the Edit card
- control: `mbsid_knob_set`, `mbsid_par_set` (CV modulation entry points)
- output: `mbsid_tick`, `mbsid_regs_l()`, `mbsid_regs_r()` (32-byte images)

Changing any signature requires updating **all** callers together (Rust
FFI in `fw/src/mbsid_sys.rs` + both oracle drivers) — it's `extern "C"`,
so there is no mangling guard.

## Layer 4 — Firmware (`fw/src/`)

| Module | Role |
|---|---|
| `main.rs` | boot, main loop (menu/display/persistence), TIMER0 ISR (MIDI drain → engine, SysEx drain, CV tick, `mbsid_tick` + RegDiff enqueue) |
| `mbsid_sys.rs` | FFI declarations for the shim (cfg-stubbed on host so unit tests run on x86) |
| `regdiff.rs` | 32-byte shadow diff → changed-register list (host-pure, unit-tested) |
| `menu.rs` | menu state machine, 4 cards — Main, CV Mod, Edit, and Usb (M6, only joins the Card row while `USB Mode`=`Storage`) — (`Card`, `MenuState`, `on_turn`/`on_press`) + drawing |
| `params.rs` | curated Lead parameter table for the Edit card: 32 rows with sub-byte encodings and L/R mirror addresses |
| `cv.rs` | CV sampling, 8-bit deadband, semitone quantizer (integer hysteresis, 1 V/oct, 0 V = C2), gate thresholds, note release on retarget |
| `patch_store.rs` | User bank in SPI flash `0xF00000..0xF80000`, 128 × 4 KiB slots; header written *after* payload (torn-write safe) |
| `sysex_capture.rs` | Rust-side parser of MBSID Bank-Write dumps (bank 1 only) feeding `patch_store` |
| `settings_store.rs` | 16-byte persisted record (magic `"MBS5"`, MIDI Src + USB Mode + 4 CV targets + checksum) in the option-storage flash window; debounced ~2 s; corrupt record → defaults |
| `usb_msc.rs` | `usb_msc` CSR driver (M6): `read_block`/`write_block`/`flush`, wall-clock read/write timeout polling (`uptime.rs`) |
| `usb_patch.rs` | `export_patch`/`load_patch` — `.syx` file I/O for the Usb menu card (8.3 filenames: `EDIT.SYX`, `P{:03}.SYX`); export self-verifies by re-reading and byte-comparing the write |
| `bank_import.rs` | whole-bank import from `/MBSID/BANK.SYX`: pre-validate-then-wipe replace across all 128 User slots |
| `fat.rs`, `partition.rs` | FAT/GPT parsing glue for `usb_msc.rs`'s block device (mount-per-op, no held `FileSystem` across loop iterations) |

Key ISR/main-loop rules:

- **Control rate is 1 kHz** (`TIMER0_ISR_PERIOD_MS = 1`), matching the
  upstream JUCE port so oracle diffs are apples-to-apples. The engine's
  internal `updateSpeedFactor = 2` is set by its ctor; `mbsid_tick`'s
  argument is ignored.
- SysEx bytes are fed to **both** consumers: the engine
  (`mbsid_sysex_byte` — RAM Writes audition live) and `SysexCapture`
  (persistence). `SYSEX_TIMEOUT_MS = 500` resets both parsers on an RX gap.
- ISR/main-loop shared state uses `critical_section::Mutex<RefCell<T>>` —
  `riscv32im` has no atomic RMW instructions, so `AtomicUsize::fetch_add`
  et al. don't even compile.
- `MenuState.lead_loaded` is resynced from the engine **every** main-loop
  iteration (not just after menu-driven loads), because an inbound MIDI
  Program Change changes the loaded patch behind the menu's back.

### Memory budget

`mainram_size = 0x8000` (32 KB BRAM): measured `.bss` ≈ 6.9 KB (the
by-value engine aggregate) + measured **peak stack 4016 bytes** of the
~25.8 KB stack region (hardware stack-painting probe, deepest realistic
path). Roughly 21.8 KB of true headroom — but watch it when adding
firmware state, and don't trust `llvm-size`'s default summary (see
[limitations.md](limitations.md#resource-budgets)).

## Layer 5 — Validation: the host oracle (`host_oracle/`)

The keystone of the project. "Did ~8k lines of C++ port correctly?" is
converted into a mechanical regression test that runs entirely on a PC:

1. Build the *same* engine sources + `mbsid_shim.cpp` for x86
   (`shim_driver`).
2. Build the upstream engine in its reference harness, instrumented to
   emit a timestamped register trace (`oracle`).
3. Run identical `(patch, note-sequence)` inputs through both and diff the
   **L and R register streams — they must be byte-identical.**

`run_oracle.sh` covers all four engines (Lead ×3 presets ×2 sequences,
Multi ×3, Bassline ×2, Drum ×4), a multi-channel differential test, a
128-patch no-crash sweep, and SysEx equivalence/rejection tests: **28/28
OK** is the green bar. Additionally `fw/` has 41 host-side `cargo test`
unit tests (regdiff, patch store, SysEx capture, menu, params, CV).

What the oracle *cannot* catch: anything target-specific — the static-ctor
walk, ISR timing, CSR plumbing, flash behavior. That is what the hardware
checklists in the M-specs exist for.

## Design decisions worth knowing (and where they're argued)

| Decision | Why | Where |
|---|---|---|
| C++ engine FFI'd, not rewritten in Rust | patches are meaningless without the exact engine; bit-exactness is testable | `DESIGN.md §1` |
| 1 kHz control rate | match the JUCE oracle exactly | `DESIGN.md §8` |
| `riscv32im` target (not the `imac` the spec first named) | must match the firmware target to avoid atomics/ABI link mismatch | `../CLAUDE.md` |
| Diff-and-enqueue instead of full-image writes | 16-deep FIFO + 1 MHz φ2 can't absorb 2×29 writes/ms | `DESIGN.md §2` |
| Bank 0 = Factory ROM, bank 1 = User flash; RAM-Write = audition only | matches MBSID editor semantics | `M4 §1` |
| CV targets are engine parameters, never raw SID registers | the engine owns the register image; raw pokes would fight the diff loop | `M5 §1` |
| Edit card mirrors voice/filter writes L↔R | preserve factory Lead stereo invariant | `fw/src/params.rs` header |
| Scope gateware stripped (`with_scope=False`) | LUT budget for the second reSID on the 25F | `M2 §6` |
| MSC engine forced to Full Speed (`usb_msc_fullspeed_only`) | HS bulk-OUT mandates 512 B packets + PING, which the SIE didn't implement — both were root causes of real-drive write wedges; FS's ~1 MB/s is ample for 512 B patch files | `M6_USB_STORAGE.md` "Eighth round" |
| USB-C forced to MIDI ↔ storage mutual exclusion, never simultaneous | one physical port, one `UTMITranslator`; simplest correct mux is "exactly one engine out of reset" | `M6_USB_STORAGE.md §2` |
