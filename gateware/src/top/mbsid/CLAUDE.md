# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# MBSID-on-Tiliqua (`top/mbsid`)

**Status (2026-07-12): All four engines validated (Lead/Bassline/Drum/Multi); M2 dual-SID implemented; M3 factory patch bank done (MIDI PC → 128 patches); M4 writable user patch bank + on-device save UI + MIDI SysEx patch upload implemented (`M4_USER_PATCH_BANKS.md`); M5 menu/CV implemented; M6 USB mass-storage patch load/export implemented end-to-end — dual USB engines (MIDI host + MSC host) behind a UTMI mux; M6a (read) browses/loads `.syx` files from a FAT USB drive from the menu; M6b (write) exports the live EDIT buffer or any User-bank slot as a standard MBSID SysEx `.syx` file back to the drive (`M6_USB_STORAGE.md`). This closes out the whole M6 plan — code-complete and host/sim/timing-verified. Hardware bring-up pending for all of the above (M1–M6 alike).**
`DESIGN.md` is the approved spec (authoritative for interfaces/milestones/acceptance).
`docs/` holds the narrative documentation set (user guide, architecture, developer
guide, limitations, extending) — update the relevant page when a feature lands.
`top.py`, `fw/` (incl. `build.rs`), and the `pdm mbsid build` script all exist on this branch
(`mbsid-port`). Verified green: freestanding compile, host oracle (shim == engine, 28/28 OK +
Multi differential + 128-patch sweep + SysEx RAM-Write-equivalence + bad-checksum-rejection),
host `cargo test --lib` (118/118, incl. `patch_store`/`sysex_capture`/menu Save-row, frame diff/painter,
`usb_patch`/FAT-fixture/menu Usb-card coverage, `export_patch`/encode_syx round-trip), full bitstream
build with **both** the M6a read path and the M6b write path (TX FIFO + WRITE(10) engine) included,
`sync` Fmax 64.29 MHz PASS (60 MHz target), 22872/24288 (94%) `TRELLIS_COMB`
(`build/mbsid-r5/top.tim`) — post-route Fmax swings several MHz build-to-build on
placement-seed noise alone (root `CLAUDE.md`), so treat this exact number as a snapshot, not
a promise; LUT climbed from M6a's 91% as expected with the write leg added, still comfortably
routable. The one thing NOT yet validated is **playback and the M4 SysEx/user-bank and M6
USB-drive load/export flows on real hardware** (DESIGN §7 milestones 2–3;
`M4_USER_PATCH_BANKS.md §7`, `M6_USB_STORAGE.md`'s hardware checklists for both M6a and M6b).

## Vendored engine (not in this repo)

The `mios32/` C++ engine tree is **GPL and gitignored** (kept out of the CERN-OHL-S repo).
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
live over MIDI. The C++ engine is the mandatory middle layer: zetaSID `.syx` patches are MBSID v2
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
  `DESIGN.md §4`; the 512-byte patch buffer is exactly what `zsid/zetasid_syx.py` emits (same
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
- **GPL.** Linking the MBSID C++ into the firmware makes the distributed bitstream firmware
  GPL (fine for personal/open use). The zetaSID Cortex-M binary is proprietary — never touched
  or disassembled.
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
- **M6b: USB patch export (write) — `M6_USB_STORAGE.md §4b/§6`.** Adds a SCSI
  WRITE(10) + bulk-OUT data path to the `guh` MSC host, a TX CSR block on
  `USBMSCPeripheral`, and the fat write-back cache + `export_syx` firmware flow. Landed
  in five commits (vendor engine, CSR write path, FAT write-back, patch-store partial-write
  fix, `export_syx`+menu row); host tests green throughout, full bitstream PASS with the
  write leg included (see status line above).
  - **Vendored engine lives at `src/vendor/guh_msc/msc.py`** (BSD-3 header kept), NOT the
    pip-installed `guh` under `.venv/` — the pinned upstream `guh/engines/msc.py` is
    read-only by design (its CBW builder can't express a host→device data phase; see
    `M6_USB_STORAGE.md §2`). Never edit the `.venv` copy — changes there are silently lost
    on the next `pdm install`/lockfile resolve and don't reach the build. The vendored
    file's own header notes it as an **upstream-PR candidate**: the diff from stock `guh`
    is self-contained (`SCSIBulkHost.Command.data_dir`, a `DATA-TX` state, `WRITE_10 =
    0x2A`) and could be offered back rather than carried as a permanent fork.
  - **Write CSR contract** (`src/tiliqua/usb_msc_csr.py`, `USBMSCPeripheral(with_write=True)`,
    instantiated at `../sid/top.py:539` alongside `with_mode=True`): push exactly 128
    little-endian 32-bit words to `tx_data` (offset `0x20`) — one full 512-byte block, no
    partial fills accepted — then strobe `start_write` (offset `0x24`, `0x18` is the shared
    `resp` register reused from the read path). Firmware then polls the **sticky**
    `resp.done` bit (cleared by either a new `start`/read strobe or a new `start_write`
    strobe, not by the poll itself) and only then checks `resp.error`. `fw/src/usb_msc.rs`'s
    `write_block` is the reference implementation: fill loop, one `start_write` strobe, then
    a capped poll loop (`MAX_SPIN = 10_000_000`, looser than `read_block`'s 1,000,000 — a
    block write is expected to take longer than a read). Getting the done/error read order
    wrong (checking `error` before `done` is set, or polling a non-sticky bit) was exactly
    the class of bug the CSR-side review round caught — the fix is what made `resp.done`
    sticky in gateware rather than a single-cycle pulse firmware could race past.
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
- Host firmware tests: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib` (the
  `riscv32` FFI is cfg-stubbed on host; `regdiff` is host-pure).
- **Oracle (the keystone):** `host_oracle/run_oracle.sh` — builds the engine + shim for x86 and
  diffs register streams of `oracle` vs `shim_driver` across all four engines (Lead × 3 presets × 2
  sequences, plus Multi × 3, Bassline × 2, Drum × 4, multi-channel differential, and a 128-patch
  no-crash sweep); must be 28/28 OK + differential + sweep. Re-run after any change to the shim,
  facade, or engine subset.
