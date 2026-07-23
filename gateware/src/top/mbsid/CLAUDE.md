# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# MBSID-on-Tiliqua (`top/mbsid`)

**Status (2026-07-14): All four engines validated (Lead/Bassline/Drum/Multi); M2 dual-SID implemented; M3 factory patch bank done (MIDI PC → 128 patches); M4 writable user patch bank + on-device save UI + MIDI SysEx patch upload implemented (`M4_USER_PATCH_BANKS.md`); M5 menu/CV implemented; M6a (USB read/load) hardware-verified and working. M6b (USB write/export): first real-hardware exercise (2026-07-14) corrupted a test drive's GPT partition table and FAT32 boot sector; **root-caused the same day and fixed in gateware, sim-verified** (the CSR TX FIFO's flush-on-start_write erased the just-loaded payload, so every WRITE(10) went out payload-less and desynced the drive's bulk-only transport — fix: strobe-then-fill contract + engine start deferred until all 128 words are banked, `usb_msc_csr.py` + `fw/src/usb_msc.rs`, regression tests in `tests/test_usb_msc_csr.py`). The `Export` path is re-enabled in firmware and, as of 2026-07-16, hardware-retested — multiple real exports produced byte-correct `.SYX` files (header/slot/checksum verified against the source patch) with no drive damage, so **export is now permanently enabled, not disposable-media-only.** The write-leg stack-paint remeasure is now done (2026-07-22: combined import+export worst case measured 23312/25852 B, ~2540 B/9.8% headroom, no crash/corruption). A few `M6_USB_STORAGE.md` §7b checklist items remain outstanding (unplug-mid-write, 50x repeat, quirky-drive sweep, MIDI round-trip). See `M6_USB_STORAGE.md`'s incident writeup + root cause and §8 risk table.**
`DESIGN.md` is the approved spec (authoritative for interfaces/milestones/acceptance).
`docs/` holds the narrative documentation set (user guide, architecture, developer
guide, limitations, extending) — update the relevant page when a feature lands.
`top.py`, `fw/` (incl. `build.rs`), and the `pdm mbsid build` script all exist on this branch
(`mbsid-port`). Verified green: freestanding compile, host oracle (shim == engine, 28/28 OK +
Multi differential + 128-patch sweep + SysEx RAM-Write-equivalence + bad-checksum-rejection),
host `cargo test --lib` (155/155, incl. `patch_store`/`sysex_capture`/menu Save-row, frame diff/painter,
`usb_patch`/FAT-fixture/menu Usb-card coverage, `export_patch`/encode_syx round-trip, `uptime` wall-clock,
`bank_import` whole-bank replace),
full bitstream
build with **both** the M6a read path and the M6b write path (TX FIFO + WRITE(10) engine) included,
`sync` Fmax 65.41 MHz PASS (60 MHz target; round-five build, all five clocks PASS), 23122/24288 (95%) `TRELLIS_COMB`
(`build/mbsid-r5/top.tim`) — post-route Fmax swings several MHz build-to-build on
placement-seed noise alone (root `CLAUDE.md`), so treat this exact number as a snapshot, not
a promise; LUT climbed from M6a's 91% as expected with the write leg added, still comfortably
routable. M2 stereo playback, M3 factory-bank browsing, M4 SysEx/user-bank flows, and M5
menu/CV/patch-edit flows are all now **hardware-verified** (DESIGN §7 milestones 2–3;
`M4_USER_PATCH_BANKS.md §7`, `M5_MENU_CARDS_CV_MOD.md §8`). The one thing NOT yet validated
is **M6b's remaining USB write/export checklist items** — M6a (read) is hardware-verified
(see below); M6b's write path is fixed and re-tested for basic export, but several
`M6_USB_STORAGE.md` §7b checklist items are still outstanding (see status line above).
**Round seven (2026-07-15, landed after the M6b write path above) added a handshake-fed
engine watchdog + firmware wall-clock read/write timeouts** (`M6_USB_STORAGE.md`'s "Seventh
round" section) — simulation/compile-verified only, hardware validation still pending. The
most recent full build's `sync` post-route Fmax was **59.61 MHz, a FAIL** against the 60 MHz
target; traced to an unrelated VexiiRiscv CPU/wishbone critical path, not this round's
change, but not yet re-confirmed with a clean re-run.
**Round eight (2026-07-16) forced the MSC engine to Full Speed and landed the BOT-compliance
fixes (CSW validation, Reset Recovery, data-IN STALL parity, write-residue check,
SYNCHRONIZE CACHE)** (`M6_USB_STORAGE.md`'s "Eighth round" section) — sim-verified (host
`cargo test --lib` 121/121, `pytest tests/ -n auto` 179 passed/1 skipped, the four
`test_sid_periph.py` failures are a pre-existing `top/sid` test-fixture bug —
`_SidStub` missing `voice0_dca` — unrelated to this round's mbsid-only changes, not
introduced by it), full bitstream build PASS on all five clocks (`sync` 63.72 MHz PASS
against 60 MHz, up from round seven's 59.61 MHz FAIL — the prior FAIL's cause, an unrelated
CPU/wishbone path, was evidently seed noise rather than a persistent regression;
`TRELLIS_COMB` 23158/24288 = 95%), hardware validation still pending.
Post-round-eight follow-up coverage now also routes command-phase (CBW)
bulk-OUT STALLs through the same autonomous Reset Recovery sequence; the
focused integration regression is sim-verified, with hardware validation
still pending alongside the rest of round eight.
Whole-bank USB import (the `Import Bank` menu row, `/MBSID/BANK.SYX`,
replace semantics across all 128 user slots, pre-validate-then-wipe) is now
implemented (`fw/src/bank_import.rs`) and host-tested; hardware validation
is pending, same as the rest of M6.

## Vendored engine (not in this repo)

The `mios32/` C++ engine tree is licensed **"for personal non-commercial use only; all other
rights reserved"** (Thorsten Klose / midibox.org — verified per-file in the vendored source,
e.g. `MbSid.cpp`; NOT GPL, despite older notes in this repo claiming otherwise) and is
gitignored (kept out of the CERN-OHL-S repo).
A fresh clone has no `mios32/`, so `pdm mbsid build` fails in `fw/build.rs`. Run
**`./fetch-mios32.sh`** once after cloning — it blobless-clones `github.com/midibox/mios32`
(`--filter=blob:none`) and checks out the pinned commit
`44d8e6af401e41a8adf2319ce6a584cce154a14f` into `./mios32` (idempotent).
`fw/csrc/vendor_sources.txt` records the Lead-subset TUs (documentation only; build.rs globs
the whole `core/` tree and drops genuinely-unreferenced code with `--gc-sections`).

**Static ctors:** riscv-rt never calls `__libc_init_array`, so the engine's global
constructors don't auto-run on target. `fw/init_array.x` (wired in `build.rs`) exposes
`.init_array` bounds and `mbsid_run_static_ctors()` (in `mbsid_shim.cpp`, called from
`mbsid_init()`) walks them — do NOT remove either, or the engine boots uninitialised.

## What this is

Runs the **MBSID Lead** sound engine (mios32 `midibox_sid_v3` C++ core) on the VexiiRiscv
softcore, FFI'd from the firmware Rust, driving **two** gateware reSIDs (L/R stereo) played
live over MIDI. The C++ engine is the mandatory middle layer: MBSID v2 `.syx` patches are MBSID v2
voice descriptions with **zero** SID-register data — only the engine turns a patch into the
per-tick `sid_regs_t` register image. See `DESIGN.md §1–2`.

## Derives from `top/sid` — reuse it verbatim

The SoC is `top/sid`'s, unchanged: VexiiRiscv @ 60 MHz (`riscv32im`), `SIDPeripheral`,
`Phi2Divider` (φ2 = 1 MHz), MIDI-in CSR FIFO, Timer0. **No CSR changes expected for M1**, so
no PAC regen needed (if any CSR changes, `pdm sid build --pac-only` per the root CLAUDE.md). M2 added `SID_PERIPH_R` at `0x1200` — PAC already regenerated.
Reference the base wiring before writing new code:
- `../sid/top.py` — `SIDPeripheral` (`transaction_data` = `(data<<5)|addr`, 16-bit, depth-16
  FIFO, backpressure via `TxnStatus`/`writable`); `Phi2Divider`; `midi_read` CSR FIFO (firmware
  drains by reading until 0); `Phi2Sel`.
- `../sid/fw/src/main.rs` — the Timer0 ISR + MIDI-in drain pattern to copy. **Replace** its
  per-note `midi_note_to_sid_freq` voice logic with: feed MIDI events to the MBSID engine,
  call `mbsid_tick`, diff `mbsid_regs_l()` and `mbsid_regs_r()` against their 32-byte shadows,
  enqueue only changed registers to `SIDPeripheral` (L) and `SIDPeripheral_R` (R) respectively.

## The register-write path (the whole point)

```
MIDI in (midi_read CSR FIFO) ─► mbsid_note_on/off / pitch_bend / cc   (engine state)
Timer0 ISR ─► mbsid_tick(speed_factor) ─► sid_regs_t L image ──► RegDiff L ─► SIDPeripheral   ─► reSID0 ─► L output
                                        └─► sid_regs_t R image ──► RegDiff R ─► SIDPeripheral_R ─► reSID1 ─► R output
           [both changed as (data<<5)|addr; φ2 = 1 MHz each; everything from the FIFO on is top/sid, reused]
```

## Non-obvious gotchas for this port

- **Menu MIDI Src row (TRS/USB) is a pure firmware toggle, no gateware diff.** `MBSIDSoc`
  inherits `SIDSoc`'s USB/TRS source mux and `usb_midi_host` CSR (offset `0xC` on
  `SID_PERIPH`) unchanged — the menu's 4th row (`menu.rs`'s `Row::MidiSrc`) just writes that
  bit every redraw (`main.rs`). MidiSrc and the CV Mod card's four target assignments persist
  together in a single 16-byte record in the option-storage flash window
  (`fw/src/settings_store.rs`: magic `"MBS5"` + version + `midi_src` + `cv_targets[4]` +
  checksum), written debounced ~2 s after the last change (and skipped if identical, to spare
  flash wear). A corrupt or blank record (bad magic/version/checksum) decodes to defaults —
  TRS / all four CV targets Off — rather than failing to boot.
- **M5 (menu cards + CV modulation + on-device patch edit):** the menu is three cards —
  Main, CV Mod, Edit (`fw/src/menu.rs`'s `Card` enum) — navigated via a Card selector row.
  CV Mod assigns each of the 4 CV inputs to a target: Knob1–5 (patch knob matrix, via
  `mbsid_knob_set`), Volume/Phase/Detune/Cutoff/Reso (parSet common block, `mbsid_par_set`
  addresses `0x01`–`0x05`), or Pitch/Gate (a CV note machine on MIDI channel 1; see
  `fw/src/cv.rs`). The Edit card edits the loaded Lead patch's params via `mbsid_sysex_param`,
  which writes straight into the patch body (so it also lands in whatever gets saved) as well
  as applying live. **Precedence:** the CV Mod ISR tick runs every 1 ms and overwrites the same
  live knob/par values the Edit card just set, so CV modulation wins for what you hear
  right now *while the CV input is actively changing* — CV targets are deadbanded on their
  8-bit value (`CvState::tick`'s `last8` dedup in `fw/src/cv.rs`), so once a CV settles an
  Edit-card write to the same target takes effect live until the CV next crosses an LSB
  boundary — but the Edit card's write already landed in the patch body, so that's the value
  that gets persisted on Save, independent of whatever CV is currently doing to the live sound.
- **`MenuState.lead_loaded` must be resynced unconditionally every main-loop iteration, not just
  after a menu-driven load.** The Edit card's param rows are only meaningful for a Lead patch;
  `row_count()` collapses them away when `!lead_loaded` so `on_turn` can't emit stray
  `TurnResult::Param` writes into a non-Lead patch body. It's tempting to set `state.lead_loaded`
  only inside the `need_load` block (menu bank/program change) — that's incomplete, because
  inbound MIDI Program Change (`fw/src/main.rs`'s ISR) changes the engine's loaded patch
  independently of `MenuState` and will leave `lead_loaded` stale if it's not re-read from
  `mbsid_sys::current_engine()` at the top of every loop iteration, before `on_turn` is
  dispatched (found via a whole-branch review that per-task review missed; took two fix rounds).
- **Tiliqua's USB-C MIDI port is a host port, not a device port** (`guh.engines.midi.USBMIDIHost`,
  a genuine `USBHostEnumerator` — drives VBUS, enumerates whatever's plugged in). Tiliqua will
  **never** show up in a PC's own `lsusb`/`amidi -l`, and a plain PC-to-Tiliqua USB-C cable
  carries no MIDI either direction (two hosts can't enumerate each other) — this is by design,
  not a bug. For PC-scripted SysEx (`amidi`/`sendmidi`), go over **TRS**: a class-compliant
  USB-MIDI interface plugged into the PC (that's what shows up as `hw:X`) with a TRS/MIDI
  cable into Tiliqua's TRS MIDI-in jack. To exercise the USB path, plug a MIDI device that can
  transmit SysEx *into* Tiliqua's USB-C port and set the menu's `MIDI Src` row to `USB`.
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
  data-sections` + link `--gc-sections` drop genuinely-unreferenced
  code (`app.cpp`, the SysEx-ACK/`sprintf` paths, `MbSidAsid`). NOTE: the Bassline/Drum/
  Multi SEs are **not** dropped — `MbSid::updatePatch` references them via `&mbSidSe*`
  + virtual dispatch, so they stay linked (verified: 24–26 symbols each in the ELF).
  This is why loading a non-Lead patch is crash-safe (it dispatches to a real engine).
  `vendor_sources.txt` is now documentation only (build.rs globs, doesn't read it).
- **Static ctors don't auto-run** — see the "Static ctors" note above. `mbsid_run_static_ctors()`
  (walks `.init_array` via `fw/init_array.x`) is the reason the engine's speed-factor + RNG seed
  actually get applied on target. The host oracle CANNOT catch this (host libc runs ctors).
- **All engine state lives in `.bss`** via one `mbsid_shim.cpp` (the only C++ we author) owning
  a single `MbSid` + `MbSidClock` + two static `sid_regs_t`. No allocator. Shim ABI is in
  `DESIGN.md §4`; the 512-byte patch buffer is exactly what `the reference `.syx` encoder script` emits (same
  `sid_patch_t` layout — no re-encoding).
- **Oracle validation is the keystone, runnable entirely on PC before any FPGA work.** Build
  the same `.cpp` + `mbsid_shim.cpp` for x86, run an identical `(patch, note-sequence)` through
  both it and the instrumented JUCE port, diff the **L and R** register streams — must be
  byte-identical on ≥3 Lead patches. Do this (`DESIGN.md §6`, milestone 1) before gateware.
- **`mainram_size` is bumped to `0x8000`** (`MBSIDSoc` subclasses `SIDSoc` in `top.py`; sid's
  default is `0x4000`). The by-value engine aggregation lands ~6.9 KB `.bss` + needs stack room;
  measured `.bss` 6884 B + stack 25880 B fits 0x8000. If you add firmware state, watch RAM.
  **Caution:** `llvm-size`'s default summary folds `.bss`+`.heap`+`.stack` into one "bss" number,
  and the `.stack` *section* size is just the linker's leftover-region allocation, not actual
  usage — use `llvm-size -A` for the real per-section breakdown, and don't mistake "stack section
  is nearly full" for "stack is nearly overflowing." Real **peak stack usage was measured on
  hardware post-M4** via a stack-painting probe (fill with `0xAA` at boot, scan for the
  high-water mark, log growth over UART0): **4016 / 25824 bytes** after menu navigation + an
  on-device save (the deepest realistic path) — ~21.8 KB of actual headroom (`M4_USER_PATCH_BANKS.md §6f`).
- **All four engines are validated** (oracle bit-exact, all 9 non-Lead factory
  patches + Lead). The firmware forwards the **real MIDI channel** (not hardcoded
  0): each engine routes notes per its fixed `updatePatch` channel map — Lead/Drum
  on ch 1, Bassline on ch 1–2 (split @ note 60), Multi on ch 1–6 (ch 1–3 → Left
  SID, ch 4–6 → Right SID). Channel aftertouch is forwarded via `mbsid_aftertouch`.
  The shim MIDI ABI takes `chn` as its first arg — change it and ALL callers
  (Rust FFI + both oracle drivers) together (extern "C", no mangling guard).
- **Drum engine SIGSEGV at t≈4182ms (MASTER clock mode):** `MbSidWtDrum::tick()` dereferences a sentinel pointer `(MbSidDrum*)1` roughly 4.18s after loading a Drum patch with no external MIDI clock. The oracle sequences end before this window; on hardware, use an external MIDI clock or trigger reload before 4s. See `.scratch/mbsid-drum-sigsegv/issue.md`.
- **`MbSidClock` AUTO mode stays in MIDI-slave mode (WT frozen) until ~4095ms**, then falls back to its internal BPM master clock — same threshold as the Drum SIGSEGV above, but it affects *any* oracle test that needs the WT to actually step (e.g. Multi WT→filter modulation). Stock sequences like `seq_multi.txt` end at ~1200ms and never reach this window, so WT-dependent asserts silently no-op — extend the sequence past ~4.1s locally in the test (don't edit the shared `.txt` file; see `run_oracle.sh`'s A107 block for the pattern) and use a discriminating check (helper disabled → must still FAIL) to rule out false positives from the clock switch itself.
- **Multi engine: repeating one note on a fixed MIDI channel can alternate L/R in blocks of 3 — by upstream design, not a Tiliqua bug.** Root cause is `MbSidVoiceQueue`/`MbSidSeMulti::voiceGet` (`MbSidSeMulti.cpp:476-491`): when an instrument's `voice_asg` patch param is 0 ("all voices"), every note-on round-robins through all 6 physical oscillators via a least-recently-used queue (voices 0-2 = Left SID, 3-5 = Right, same `physVoice = voice % 3` split as Lead). Retriggering the same note repeatedly therefore cycles 0→1→2→3→4→5→0→…, i.e. 3 notes on L then 3 on R, forever. Reproduced bit-exact on the host oracle (no gateware/shim involved) — confirmed with `pc 60 / ch 0 / on 60 100 / off 60` repeated 6× in a `host_oracle` sequence, gate-on lands on L(v0,v1,v2) then R(v0,v1,v2). Lead is unaffected — it always gates all 6 voices (both SIDs) simultaneously (`MbSidSeLead.cpp:391,428`), no `VoiceQueue` involved. Fix, if ever wanted, is patch-side (`voice_asg` = left-only/right-only instead of all), not firmware.
- **Licensing correction (2026-07-23): NOT GPL.** Prior notes in this file and in
  `DESIGN.md`/`README.md`/the M2/M4/M5/M6 docs claimed the vendored MBSID engine was GPL —
  that was wrong. Every file under `mios32/apps/synthesizers/midibox_sid_v3/core/` and the
  `mios32/modules/{sid,notestack}` sources it pulls in carries "Licensed for personal
  non-commercial use only. All other rights reserved." (Copyright Thorsten Klose /
  midibox.org); `jsw_rand.c` is Public Domain. No file in the tree we compile references the
  GPL. The implications for redistributing the linked bitstream firmware under this license
  text have not been reviewed — treat as unresolved, not as "fine for personal/open use."
- **M4 SysEx path (full design in `M4_USER_PATCH_BANKS.md`):** gateware sideband
  (`forward_sysex` on `MidiDecodeUSB`/`MidiSysexFilter`, opt-in, `top/sid` unaffected)
  → `sysex_read` CSR at offset **`0x24`** on `SIDPeripheral`, a **valid-bit read**
  (bit 8 = valid, bits 7:0 = data) — this is NOT the `midi_read` "read until 0" idiom,
  because `0x00` is a legal SysEx data byte. The Timer0 ISR drains it (`SYSEX_BYTES_PER_TICK
  = 32` per 1 ms tick, `fw/src/main.rs`) and feeds every byte to **both** consumers: the
  engine (`mbsid_sysex_byte` — RAM Writes apply live, audition only) and Rust
  `SysexCapture` (`fw/src/sysex_capture.rs` — Bank Write bank **1** only; bank 0 = Factory
  ROM, ignored/read-only). A captured Bank Write lands in `UserPatchStore`
  (`fw/src/patch_store.rs`) at flash `0xF00000..0xF80000`, 128 × 4 KiB slots, header
  written *after* the payload (torn-write safe). `SYSEX_TIMEOUT_MS = 500` resets both
  parsers on an RX gap.
- **No MIDI TX — ACK/DISACK is swallowed** (the facade has no route back to any MIDI
  output). Editors/workflows that wait for a per-patch ACK before sending the next dump
  will stall or time out; use scripted fire-and-forget sends (`amidi`/`sendmidi`) instead.
  **This used to mean the device had no patch egress at all — that's no longer true.**
  M6b's USB export (below) writes the live EDIT buffer or any User-bank slot to a
  standard MBSID SysEx `.syx` file on a plugged drive, re-sendable from a PC over TRS or
  reloadable via M6a. MIDI itself is still TX-less; USB storage is the way a patch leaves
  the device.
- **`sysex_read` was the first CSR change since M2's `phi2_sel`** — touching
  `SIDPeripheral` registers again needs `pdm mbsid build --pac-only` before `--fw-only`,
  same as any other CSR change (root `CLAUDE.md`).
- **M6a: USB mass-storage patch load (`M6_USB_STORAGE.md`).** New `usb_msc` CSR block
  at **`0x1300`** (`0x1000`/`0x1200` are the two `SIDPeripheral`s), built with
  `USBMSCPeripheral(with_mode=True)` (`src/tiliqua/usb_msc_csr.py`) — this is another
  CSR change, needs `pdm mbsid build --pac-only` before `--fw-only`. The mode bit lives
  at register offset **`0x1C`** (`mode`, bit 0 = storage), read in gateware as
  `usb_msc.mode_o` and driven by firmware's `usb_msc.set_mode(state.usb_storage)`
  (`fw/src/main.rs:610`).
  - **Option A shape, confirmed at 25F scale (`../sid/top.py:629-690`):** one
    `UTMITranslator` owns the physical ULPI PHY; `USBMIDIHost` and `USBMSCHost` are both
    instantiated with `bus=None` (raw `UTMIInterface` records) and each wrapped in its
    own `ResetInserter({"usb": ...})` keyed off `storage_mode` (one term per engine, so
    exactly one is out of reset at a time — a mode flip re-enumerates the newly-selected
    engine from scratch, composing with each engine's own watchdog reset). PHY→engine
    signals fan out to both (harmless — the parked engine is in reset); engine→PHY
    signals are `Mux(storage_mode, msc_utmi.x, midi_utmi.x)` per wire. This is the
    "two engines + mux" option the plan called Option A, not the shared-enumerator
    Option B fallback — it fit without an Fmax fallback being needed.
  - **Storage mode forces TRS MIDI + VBUS always on.** `with_usb_msc=True` makes
    `vbus_o` combinationally `1` unconditionally (`../sid/top.py:696-699` — a thumb
    drive needs bus power even though `usb_midi_host=0`); the M5-inherited USB/TRS
    source mux (`sid_periph.usb_midi_host` CSR bit) is untouched in gateware, but
    firmware only ever sets it when **not** in storage mode
    (`sid.usb_midi_host().write(... && !state.usb_storage)`, `main.rs:606-608`) — so
    entering Storage mode silently falls back to TRS as the live MIDI source, and TRS
    keeps working the whole time (the ISR/register-write path is untouched by USB mode).
  - **Mount-per-op, no held `FileSystem` lifetime** (`fw/src/main.rs`'s `with_fat`
    helper, the `sid_player_sw` idiom): every USB menu action (list, load, load→slot)
    calls `with_fat(&usb_msc, |fs| ...)`, which constructs a fresh `MscStorage` +
    `FileSystem::new` and drops it when the closure returns. Nothing holds a `FileSystem`
    across loop iterations, so a drive yanked mid-browse can't leave a dangling mount —
    the next op's `FileSystem::new` just fails and returns `None`. Costs a few extra
    512-byte block reads (BPB + root dir) per action; patch files are tiny so this is
    cheap.
  - **M5-lesson-shaped per-iteration resync, load-bearing here too**
    (`fw/src/main.rs`'s main loop, ~line 463 on): `drive_ready` is recomputed every
    iteration from live state (`state.usb_storage && usb_msc.ready() && block_size() ==
    512`), and both directions are handled unconditionally, not just on a menu event —
    losing the drive (or leaving Storage mode) collapses `Card::Usb` back to `Card::Main`
    and clears the cached file list in the same iteration it's detected, and a
    newly-ready drive triggers the directory scan the same way. Same shape as the M5
    `lead_loaded` bug: deriving `Card::Usb`'s validity only from menu navigation events
    (instead of every iteration) would let a stale file list survive an unplug until the
    user happened to turn the encoder again.
  - **An idle-but-ready drive needs a firmware keepalive — don't remove it.** Since
    round seven the MSC engine's 10 s watchdog (`src/vendor/guh_msc/msc.py`) is
    handshake-fed — any ACK/NAK/NYET holds it cleared, so a busy-NAKing drive is
    never reset mid-command — but an IDLE bus produces no handshakes (SOFs don't
    touch the SIE response), so a quiet READY drive still needs the 2 s LBA-0
    probe in `main.rs`. The keepalive is also the unplug detector's trigger: a
    yanked drive meets the probe with silence (TIMEOUT — deliberately NOT in the
    watchdog's alive-set, along with STALL and CRC noise), the watchdog runs out,
    `ready` drops. Note `sid_player_sw` shares the stock engine and the old
    idle-reset behavior; only mbsid carries the vendored fix.
  - **Peak stack usage is far tighter than the §6f estimate predicted — 22736/25856 B
    (~3.1 KB / 12% headroom), measured on hardware 2026-07-14 via a temporary UART0
    probe** (mbsid has no logger/UI wiring at all, unlike `sid_player_sw`'s
    `handlers::logger_init` — the probe talked to `Serial0`/UART0 directly: paint the
    stack region with `0xAA` at boot, scan for the high-water mark every 64 main-loop
    iterations, log new peaks at 115200 baud; now behind the `stack-probe` cargo
    feature (default off) rather than unconditionally compiled in; enable it per the
    Build & test section to re-measure — see `M6_USB_STORAGE.md §7a`).
    Triggered by a `Usb` card `Load→Slot` — nearly 6x the
    +2–3 KB (over M4's 4016 B baseline) the plan estimated. **M6b's export path (the
    `tx_data` fill loop + FAT write-back cache) has not yet been measured and stacks on
    top of this** — treat it as a real overflow risk, not a formality, before trusting
    M6b on hardware (`M6_USB_STORAGE.md §7b`, §8 risk table).
- **M6b: USB patch export (write) — `M6_USB_STORAGE.md §4b/§6`. Corrupted a real drive on
  first hardware exercise (2026-07-14); root-caused the same day and fixed in gateware
  simulation — see `M6_USB_STORAGE.md`'s incident writeup + root cause.** The bug: the
  final-review commit `a232efb` wrapped the CSR TX word FIFO in
  `ResetInserter(start_write)` while the contract was fill-then-strobe, so the strobe that
  started every write flushed the just-loaded payload — each WRITE(10) CBW went out with no
  data phase, hanging the drive mid-command until the 10 s watchdog reset, and the resulting
  bulk-only-transport desync committed mostly-zero sectors at arbitrary LBAs (the LBA math
  itself was never wrong). `PressResult::UsbExport` (`fw/src/main.rs`) was re-enabled after
  the fix and, as of 2026-07-16, hardware-retested: multiple real exports produced files with
  correct header/slot/checksum matching the source patch and no drive damage — **export is
  now permanently enabled, no longer disposable-media-only.** §7b's stack-paint remeasure for
  the write leg and a few other checklist items (unplug-mid-write, 50x repeat, quirky-drive
  sweep, MIDI round-trip) are still outstanding — see `M6_USB_STORAGE.md` §7b/§8.
  **Second bring-up round (2026-07-15) found three more engine bugs** (see
  `M6_USB_STORAGE.md`'s round-two writeup): CBW NAK treated as rejection (upstream `guh`
  bug, latent on reads — now retried with same PID), CSW-RX/DATA-RX missing Default arms
  (a STALLed CSW wedged the engine until the watchdog, whose reset also zeroed the diag
  CSRs — beware last-failure-only diagnostics destroying their own evidence), and no
  REQUEST SENSE after CHECK CONDITION (now auto-issued after failed writes; key/ASC/ASCQ
  in the `sense_info` CSR at `0x34`; key=7/asc=0x27 = drive is write-protected).
  **Rounds three/four (2026-07-15) found the final root cause: undecoded NYET**
  (`M6_USB_STORAGE.md` round four). The drive runs at High Speed — `status.speed` CSR
  encoding is LUNA xcvr_select: **0=HIGH**, 1=FULL, 2=LOW, 3=no-device (it was
  mis-documented inverted, which misdirected round three toward "link is FS") — and HS
  drives answer bulk-OUT data with NYET ("accepted, busy") as routine write flow
  control. Stock `guh`'s SIE decodes only ACK/NAK/STALL, so NYET fell into the TIMEOUT
  arm → engine rejected every write as `rej=4/2/0` regardless of chunk size (64/32/31)
  or toggle. Fix: `guh/usbh/sie.py` vendored to `src/vendor/guh_msc/sie.py`
  (`TransferResponse.NYET=7` + `detected.nyet` decode; swapped into the stock
  enumerator in `SCSIBulkHost.__init__`), engine treats NYET as ACK in `CBW-WAIT`/
  `DATA-TX-WAIT` (PING protocol deliberately skipped — the NAK-replay path covers the
  busy case). Tests: UTMI-level handshake injection in `tests/test_guh_sie_tx_packets.py`
  + a NYETing-drive write in `tests/test_usb_msc_integration.py`. All sim tests now
  import `TransferType`/`TransferResponse` from `vendor.guh_msc.sie`, NOT `guh.usbh.sie`
  — the classes are numerically identical but `ctx.set()`/asserts reject members of the
  other class. **A related trap bites the engine RTL itself**: the *production* SIE's
  `status.response` is an EnumView of **guh's** enum class, so `response ==
  TransferResponse.NYET` (vendored class) raises TypeError at elaboration — but only in
  the full build, because the sim stub's signature uses the vendored class. Compare via
  `.as_value() == Member.value` in any new `m.If`; `m.Case` arms are immune (compare by
  value). Sim green + build crash = check for this.
  **Round five (2026-07-15, post-NYET-fix hardware test): no BOT error recovery, and the
  watchdog destroys wedge evidence** (full writeup: `M6_USB_STORAGE.md` round five). An
  8GB stick STALLed the bulk-OUT endpoint mid-data-phase (`rej=3/2/4` = STALL/DATA-TX/
  128B ACKed) and, with no CLEAR_FEATURE(ENDPOINT_HALT), every later CBW bounced off the
  halted endpoint (`rej=3/1/4`), sense unreadable, watchdog loop; a 64GB stick wedged
  outright and logged `rej=0/0/0` — the watchdog reset zeroed the diag CSRs before
  firmware read them. Fixes: (1) clear-halt recovery in the engine (DATA-TX STALL →
  clear OUT halt via ep0 control transfer → toggle reset → read CSW → auto-sense; CSW
  STALL → clear IN halt → exactly one CSW retry; reject phase **5** = the recovery
  itself failed), (2) ALL reject/sense/NYET diags latched in `usb_msc_csr.py` on
  **change-to-nonzero**, outside the watchdog reset domain — the engine now zeroes its
  reject regs on each `cmd.start`, which the change-detect contract requires (don't
  remove either half) — plus two new `reject_info` fields: `nyets` (NYET count; `ny=0`
  on a STALL rules out the skipped-PING hypothesis) and `last_phase` (live-phase
  breadcrumb: where a wedged engine was stuck when the watchdog wiped it). UART diag
  prints `rej=r/p/t ny=N lph=P`. RejectInfo gained fields = CSR change = `--pac-only`
  regen. Hotplug re-detection is separately broken (bitstream restart needed) —
  `.scratch/mbsid-usb-hotplug-redetect-broken/`. **Timing lesson:** the live-phase
  decode as a comb path (whole FSM state reg → Mux chain → cross-module →
  peripheral change-detect) cost ~5–7 MHz of `sync` Fmax at 94% LUT — three seeds all
  FAILED 55–57 MHz with critical paths in *unrelated stock logic* (CPU fabric, audio
  calibrator DSP), the signature of diffuse congestion rather than a hot path;
  registering the decode in the engine (`m.d.usb`, one cycle of lag, irrelevant for a
  breadcrumb) restored 64.46 MHz PASS at the default seed. Cross-module diagnostic
  fanout wants a register at the source, same spirit as the root CLAUDE.md's
  register-the-MULT rule.
  - **Round-six read-path `pth` diagnostic:** `pth` is the raw `read_path_info`
    CSR: engine bytes `[9:0]`, peripheral bytes `[19:10]`, packed words
    `[27:20]`, sampled stream mode `[28]`, sampled-length-is-512 `[29]`.
    Diagnostic-only. **The "sampled before the watchdog" assumption is disproven on
    hardware (2026-07-15 run):** `read_block`'s 10M-spin timeout outlasts the engine's
    10 s watchdog (2 byte-serialized CSR reads per spin), so the engine-side fields
    (`[9:0]`, `[28]`, `[29]`) read back zeroed and `lph` shows the post-reset recovery
    TEST-UNIT-READY, not the failing read. Only `periph_bytes`/`periph_words`
    (`[19:10]`/`[27:20]`, sync-domain, reset only on start strobes) survive — and they
    proved zero bytes ever crossed for the post-write READ(10), exonerating the whole
    Tiliqua datapath (see `M6_USB_STORAGE.md`'s round-six results).
  - **Round seven (2026-07-15): handshake-fed watchdog + wall-clock firmware
    timeouts** (`M6_USB_STORAGE.md` round seven). The engine watchdog is held
    cleared by any live handshake (ACK/NAK/NYET) so a drive busy-NAKing for
    >10 s (the round-six read-after-write killer) is waited out instead of
    reset; TIMEOUT/STALL/CRC still count so unplug detection and re-enumeration
    survive (guarded by `test_watchdog_still_fires_on_silent_drive`). Firmware
    read/write polls budget 30 s of Timer0 wall-clock (`fw/src/uptime.rs`;
    spin caps proved uncalibrated), abort early with read reason 4 if
    `connected` drops, and print `ms=`/`wms=` beside `sp=`. No CSR changes —
    no `--pac-only` needed. Consequence for diagnostics: the `pth` engine
    fields are live-and-trustworthy at a rsn=3 snapshot now, since no watchdog
    reset precedes it.
  **Per-layer sim tests all passed while the assembled stack failed on hardware** — the
  CSR peripheral + engine + `top.py` glue combination is now covered by
  `tests/test_usb_msc_integration.py` (firmware-exact CSR sequences vs a scripted
  disagreeable drive); keep its copy of the glue in sync with `../sid/top.py`.
  Adds a SCSI
  WRITE(10) + bulk-OUT data path to the `guh` MSC host, a TX CSR block on
  `USBMSCPeripheral`, and the fat write-back cache + `export_syx` firmware flow. Landed
  in five commits (vendor engine, CSR write path, FAT write-back, patch-store partial-write
  fix, `export_syx`+menu row); host tests green throughout, full bitstream PASS with the
  write leg included (see status line above) — none of that caught the real-hardware bug,
  since host tests and sim both drive a mock/in-memory storage backend, not a real MSC
  device.
  - **Vendored engine lives at `src/vendor/guh_msc/msc.py`** (BSD-3 header kept), NOT the
    pip-installed `guh` under `.venv/` — the pinned upstream `guh/engines/msc.py` is
    read-only by design (its CBW builder can't express a host→device data phase; see
    `M6_USB_STORAGE.md §2`). Never edit the `.venv` copy — changes there are silently lost
    on the next `pdm install`/lockfile resolve and don't reach the build. The vendored
    file's own header notes it as an **upstream-PR candidate**: the diff from stock `guh`
    is self-contained (`SCSIBulkHost.Command.data_dir`, a `DATA-TX` state, `WRITE_10 =
    0x2A`) and could be offered back rather than carried as a permanent fork.
  - **Write CSR contract** (`src/tiliqua/usb_msc_csr.py`, `USBMSCPeripheral(with_write=True)`,
    instantiated at `../sid/top.py:539` alongside `with_mode=True`): strobe `start_write`
    FIRST (offset `0x24` — arms the write: flushes leftover TX words, clears the sticky
    resp bits), then push exactly 128 little-endian 32-bit words to `tx_data` (offset
    `0x20`); the peripheral defers the engine start until the 128th word is banked, so a
    WRITE(10) CBW can never be issued without its full 512-byte data phase (`0x18` is the
    shared `resp` register reused from the read path). **Do NOT revert to fill-then-strobe:
    that order + the flush-on-strobe was the 2026-07-14 drive-corruption root cause.**
    Firmware then polls the **sticky**
    `resp.done` bit (cleared by either a new `start`/read strobe or a new `start_write`
    strobe, not by the poll itself) and only then checks `resp.error`. `fw/src/usb_msc.rs`'s
    `write_block` is the reference implementation: `lba`, one `start_write` strobe, fill
    loop, then a capped poll loop (`MAX_SPIN = 10_000_000`, looser than `read_block`'s
    1,000,000 — a block write is expected to take longer than a read). The CSR-side review round (task 12)
    caught a different read/write interaction bug: `write_pending`'s `m.d.sync` clear on
    `start_o` lands one cycle late, so a plain read issued right after a write saw a stale
    `write_pending=1` and got misrouted into `msc`'s WRITE state instead of READ, silently
    corrupting the read — fixed by masking it combinationally on the read-start cycle,
    `msc.cmd.write.eq(start_write_o | (write_pending & ~start_o))` (`../sid/top.py`,
    commit `1c330fa`).
  - **8.3 filenames, no long-filename support** — `fw/src/usb_patch.rs`'s `export_patch`
    writes `EDIT.SYX` (live buffer) or `P{:03}.SYX` (User slot `n`, e.g. `P042.SYX`),
    chosen because `Cargo.toml` builds `fatfs` with `default-features = false` (no `lfn`
    feature), matching `sid_player_sw`'s existing fatfs config — anything past 8.3 would
    silently truncate/mangle rather than round-trip. `export_patch` verifies its own write
    by re-opening, re-reading, and re-parsing the file, byte-comparing the 512-byte payload
    against the source patch before reporting success — a write that lands in the
    filesystem but garbles in transit (or truncates) is caught immediately, not on next
    load.
  - **No live "BUSY" indicator during a write** — `menu.rs`'s `DriveState::Busy` variant
    exists and renders, but nothing in `main.rs` ever constructs it (grep confirms
    `DriveState::Busy` is written in exactly one place: the match arm that renders it).
    `write_block`/`export_patch` run synchronously in the main loop with no yield, so the
    **screen freezing** (unresponsive to encoder turns) for the write's duration *is* the
    only "busy" signal a user gets — don't write doc copy implying a `BUSY` row appears
    during export; it doesn't. The safe-unplug rule is "don't unplug while the menu is
    unresponsive," not "don't unplug while it says BUSY."
  - **Round eight (2026-07-16): FS forcing + BOT compliance** (`M6_USB_STORAGE.md`
    "Eighth round"). The MSC engine is now **FS-only by design** — a real-hardware
    run found HS writes chunked to 64 B send 8 legally-short packets (HS bulk
    `wMaxPacketSize` is mandatorily 512), and the SIE has no PING protocol
    (mandatory for HS hosts), together explaining the round-five/six wedges;
    `usb_msc_fullspeed_only` defaults on for mbsid (`top/mbsid/top.py:71`
    → `SIDSoc.__init__`, `top/sid/top.py:500,513,653`). **Do NOT "fix" the speed
    back to HS** without also implementing 512-byte SIE TX packets + PING — that
    reintroduces both wedge mechanisms. Also landed this round: BOT §6.3.1 CSW
    validation (`csw_bad_o`, `reject_info` bit 21), autonomous BOT §5.3.4 Reset
    Recovery (MSC reset 0xFF + clear-both-halts + toggle reset,
    `vendor/guh_msc/msc.py`'s `need_rr` in `READ-DONE`/`WRITE-DONE`/`FLUSH-DONE`),
    data-IN STALL clear-halt+CSW-read parity with the write side, a write-residue
    check (`fw/src/usb_msc.rs`), and SYNCHRONIZE CACHE(10) via a new `start_flush`
    CSR strobe at offset **`0x3C`**, one flush per export
    (`usb_msc_csr.py`/`fw/src/usb_msc.rs::flush`). Two testing/scope notes: (1)
    scripted CSWs in `tests/test_usb_msc_integration.py` and
    `tests/test_guh_msc_write.py` must now echo the real captured CBW tag
    (`_Drive.last_tag`/`_Sie.last_tag`) or the now-stricter CSW-tag check rejects
    them; (2) a follow-up fix extended all three identical `need_rr` predicates
    to command-phase (CBW, `reject_phase == 1`) STALL as well as CSW-phase
    (`reject_phase == 3`) STALL. `test_cbw_stall_escalates_to_reset_recovery`
    pins the full reset + clear-IN + clear-OUT + DATA0 TEST UNIT READY sequence
    and the preserved phase-1 diagnostic.
- **Menu rendering is a blit-diff, never a background fill.** Rectangle fills
  are NOT hardware-accelerated (per-pixel via the pixel_plot FIFO, ~93k CSR
  writes for the old full-box clear — scanout films it as a top-to-bottom
  wipe); text IS (blitter, REPLACE mode, 0-bits transparent). So `menu.rs`
  builds a `frame::Frame` (list of positioned strings) and `Painter::paint`
  diffs it against the last-painted frame: stale text is erased by re-blitting
  the OLD string at intensity 0 (glyph-exact eraser; REPLACE mode has no
  zero-color skip), changed rows are re-blitted. Don't reintroduce
  `Rectangle`/`fill` into the redraw path, and don't draw menu text outside
  `build_frame` — anything the Painter doesn't know about never gets erased.
  The gateware side of the contract is `persist_freeze_rows=320` in `top.py`
  (menu band exempt from phosphor decay; without it the input-only redraw
  lets the idle menu slowly fade — persistence 80 still decays ~1 step per
  256 passes).
- **Doc test-counts can drift within a single plan, not just across plans.** A
  final-review fix round that adds a test (e.g. new `Painter`/`build_frame`
  coverage) lands in a commit *after* the task that already wrote the count
  into `CLAUDE.md`/`docs/developer-guide.md` — the count goes stale the
  moment that fix commit lands. Before closing out a plan, grep both files
  for `cargo test --lib` / `N tests:` and reconcile against a fresh `cargo
  test --target x86_64-unknown-linux-gnu --lib` run.
- **Host-testing a `DrawTarget` consumer** (e.g. `Painter`): mock it with a
  minimal struct implementing `OriginDimensions` + `DrawTarget<Color = HI8>`
  that records every `(Point, HI8)` from `draw_iter` into a `heapless::Vec`
  (see `menu.rs`'s `RecordingTarget`). Don't assert exact pixel columns —
  glyph rendering spans the whole character cell, not just the pen-x column;
  use a range like `[x, x+9)` for one `FONT_9X15` cell.

## Build & test

- `cd gateware && pdm mbsid build` — full bitstream (the `mbsid` script is registered in
  `[tool.pdm.scripts]`). `--fw-only` relinks firmware fast (reuses the bitstream; ends with an
  expected `missing top.bit` after the ELF is built). Flashable archive lands at
  `build/mbsid-r5/*.tar.gz`.
- **Bring-up diagnostics are cargo features, both default-OFF** (`fw/Cargo.toml`
  `[features]`): `usb-diag` (UART0 trace of MSC status transitions, per-stage
  export progress, and the latched first/last read+write failure snapshots —
  `M6_USB_STORAGE.md` §7b) and `stack-probe` (boot-time 0xAA stack painting +
  a high-water scan every 64 main-loop iterations — §7a). The build driver
  (`src/tiliqua/tiliqua_soc.py:533`) runs a bare `cargo build --release` with
  no `--features` flag and is **shared by all 17 bitstreams — do not modify
  it**. To turn one on for a session, edit `default = []` in `fw/Cargo.toml`
  to `default = ["usb-diag"]`, run `pdm mbsid build --fw-only`, and revert
  before committing. Host-only checks can pass `--features` directly.
  When `usb-diag` is off, `MscDiag` is a zero-field struct and the failure
  sites' CSR reads are compiled out entirely — not merely skipped at runtime.
- Host firmware tests: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib` (the
  `riscv32` FFI is cfg-stubbed on host; `regdiff` is host-pure).
- **Oracle (the keystone):** `host_oracle/run_oracle.sh` — builds the engine + shim for x86 and
  diffs register streams of `oracle` vs `shim_driver` across all four engines (Lead × 3 presets × 2
  sequences, plus Multi × 3, Bassline × 2, Drum × 4, multi-channel differential, and a 128-patch
  no-crash sweep); must be 28/28 OK + differential + sweep. Re-run after any change to the shim,
  facade, or engine subset.
