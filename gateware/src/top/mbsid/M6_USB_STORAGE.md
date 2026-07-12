# MBSID USB Mass-Storage Patch Load/Export — Feasibility + Design (M6)

Status: **M6a implemented (hardware bring-up pending); M6b spec only** (2026-07-12).
Read-only USB patch load (browse a FAT drive from the menu, load = audition, Load→Slot =
audition + persist to a User bank slot) is built end-to-end: gateware (`usb_msc` CSR at
`0x1300`, dual USB engines behind a UTMI mux per §3's Option A, decided without needing the
Option B fallback), firmware (MSC block-read driver, FAT adapter, `usb_patch.rs` file finder/
loader, file-mode SysEx parser, settings persistence, `Card::Usb` menu card, main-loop
wiring). Host tests are green (110/110, `cd fw && cargo test --target x86_64-unknown-linux-gnu
--lib`) and a full bitstream build passes timing — see `CLAUDE.md`'s status line for the
current numbers. Not yet exercised on real hardware — see the hardware checklist below.
M6b (export/write) remains spec-only (§4b/§4c below); no vendored `guh` MSC write path or
`export_syx` firmware exists yet.

Original investigation below is kept for context. Loading patches *from* a USB drive turned
out cheap as predicted — the whole read-only MSC stack (gateware + FAT firmware) already
existed, proven, in `top/sid_player_sw`, and M6a is a straight port of it plus the UTMI mux.
Exporting patches *to* a USB drive still requires new gateware: the vendored `guh` MSC host
engine is **read-only by design** (its SCSI FSM implements READ(10) only, and its CBW builder
cannot even express a host→device data phase). Write support means forking/vendoring `guh`'s
MSC engine and adding a SCSI WRITE(10) + bulk-OUT data path — unchanged from the original
assessment, not attempted in M6a.

Export is nevertheless the strategically important half: **this device has no other patch
egress.** TRS MIDI is RX-only in hardware (`src/tiliqua/platform.py:185` — the `midi`
resource has only an `rx` subsignal on every board revision), the USB-C port is a host port
(can never appear as a MIDI device to a PC, `CLAUDE.md`), and M4 already documents "No MIDI
TX — ACK/DISACK is swallowed". A user-edited patch saved to the M4 flash bank currently can
never leave the module except via debug flash tooling. USB drive export closes that gap.

## 1. Goal & non-goals

Goals:
- **M6a (read):** browse `*.syx` patch files on a FAT-formatted USB drive from the menu,
  load one into the engine (audition) and optionally commit it to an M4 user-bank slot.
- **M6b (write):** export the currently-loaded/edited patch, or any user-bank slot, as a
  standard MBSID v2 SysEx dump file on the drive — interchangeable with MIOS Studio
  tooling and re-sendable over MIDI from a PC.
- Keep `top/sid` and `top/sid_player_sw` unaffected (same opt-in pattern as M4's
  `forward_sysex`).

Non-goals:
- USB hubs (the `guh` enumerator is single-device), multi-LUN devices, exFAT/NTFS
  (FAT12/16/32 only, whatever `fatfs` 0.4 mounts), or USB I/O anywhere near the audio path
  (all file access runs in the main loop, never the Timer0 ISR).
- Simultaneous USB MIDI + USB storage. One physical port, one plugged device — the modes are
  inherently exclusive at the connector (see §3).
- Whole-bank import/export in one file (single-patch files first; a 128-patch bank file
  format can layer on later without gateware changes).

## 2. Source-verified constraints (why the design is shaped this way)

- **`guh` MSC engine is read-only.** `guh/engines/msc.py` (`USBMSCHost` docstring: "read-only
  block device interface … TODO: write support?"). Harder than the TODO suggests at the
  transport layer: `SCSIBulkHost` sets `bmCBWFlags = Mux(data_len > 0, DATA_IN, DATA_OUT)`
  (msc.py:192) — any command with a data phase is assumed device→host, and the FSM's only
  data state issues bulk-IN transfers. BUT the SIE primitive for bulk OUT already exists and
  is exercised on every command: the CBW itself is transmitted via `enum.ctrl.txs` + a bulk
  OUT transfer (`CBW-LOAD`/`CBW-XFER` states). Write support = a direction bit in
  `SCSIBulkHost.Command`, a `tx_data` stream input, and a `DATA-TX` state that streams 512
  payload bytes through the same `txs` path, plus WRITE(10)=0x2A in the opcode enum and a
  write leg in `USBMSCHost`'s FSM. Moderate, well-bounded gateware work.
- **`guh` is a pinned pip git dependency** (`pyproject.toml:15`, `guh @ git+…@d44315`,
  BSD-3-Clause). We cannot patch it in-place; write support means vendoring the touched
  modules (repo precedent: `src/vendor/vexiiriscv/`) or maintaining a fork and repinning.
  Vendoring `engines/msc.py` alone is enough — it only imports stable `guh.usbh.*` internals.
- **One ULPI PHY, currently owned by USB MIDI.** `SIDSoc.elaborate` requests
  `platform.default_usb_connection` and instantiates `USBMIDIHost` unconditionally on hw
  (`../sid/top.py:617-618`). A second `platform.request` of the same resource is impossible;
  MSC needs either a mux in front of two engines or a combined engine (§3).
- **Area headroom is the #1 risk.** mbsid is at **80% LUT** (19444/24288 `TRELLIS_COMB`,
  `build/mbsid-r5/top.tim`), sync Fmax 61.76 MHz vs 60 target. sid_player_sw carries one MSC
  host at 84%. Both class engines are thin FSMs (~130 lines MIDI, ~540 lines MSC+SCSI) over
  the shared-by-design heavy part (`USBHostEnumerator` + SIE + ULPI + descriptor parser,
  ~1.6k lines of Amaranth). Instantiating both engines duplicates the heavy part.
- **`usb` and `sync` are the same 60 MHz clock** (`src/tiliqua/pll.py:277` "sync, usb: 60 MHz
  (Main clock)"). New USB-side logic lands on the same timing budget that's passing by only
  1.76 MHz — keep added FSMs shallow, register CSR-crossing paths (root `CLAUDE.md` MULT
  lesson applies).
- **The read-side firmware stack is done and host-tested** in `sid_player_sw/fw/src/`:
  `usb_msc.rs` (57-line CSR block-read driver with spin caps), `partition.rs` (MBR/GPT →
  first-partition LBA), `fat.rs` (`fatfs` 0.4 no_std adapter, single-block cache, writes
  stubbed to error), `sid_scan.rs` (root-dir extension scan + load-by-index, host-tested
  against an in-memory FAT image). All directly reusable with the extension changed to
  `.SYX`.
- **A `.syx` patch file is exactly the byte stream `SysexCapture` already parses.** the reference hardware
  patch files are MBSID v2 SysEx dumps (`F0 00 00 7E 4B 00 …`, cmd 0x02, 1024 nibblized
  bytes = the same 512-byte `sid_patch_t` the whole port runs on). File import can feed file
  bytes through the existing parser (relaxed to accept any bank/type for file mode, §6c);
  file export is the trivial inverse (nibblize 512 bytes + 7-bit checksum). No new format.
- **RAM fits.** Measured peak stack 4016/25824 B post-M4 (~21.8 KB headroom,
  `M4_USER_PATCH_BANKS.md §6f`). sid_player_sw runs the identical fatfs stack in a *smaller*
  mainram (0x4000 vs mbsid's 0x8000). Budget adder here: `FileSystem` object + one 512 B
  sector cache + one 512 B patch buffer + dir-scan name list ≈ 2–3 KB. Re-measure with the
  stack-paint probe at implementation.
- **`fatfs` 0.4 (git, `default-features=false`) includes write support in no_std** — the
  write path is not feature-gated; sid_player_sw simply stubs `Write::write` in its adapter.
  M6b un-stubs it (read-modify-write on the sector cache + dirty write-back), no new crate.

## 3. USB port sharing: mode switch, not concurrency

Only one device can occupy the USB-C port, so "MIDI keyboard" vs "thumb drive" is already a
physical either/or. Model it as an explicit **USB Mode: MIDI / Storage** setting:

- New menu row (Main card) + persisted in the M5 settings record (bump `settings_store.rs`
  version; unknown/old records default to MIDI — same corrupt-record-→-defaults contract).
- Storage mode forces the effective MIDI source to TRS (TRS MIDI keeps working while a drive
  is plugged — you can still play while browsing patches). The existing `usb_midi_host` CSR
  semantics stay; a new mode bit gates which engine owns the PHY and asserts `usb_vbus_en`
  in *both* modes (today VBUS is only driven in USB-MIDI mode, `../sid/top.py:627` — a drive
  needs it too).
- Mode switch resets the newly-selected engine so it re-enumerates from scratch (both `guh`
  engines already self-reset via watchdog `ResetInserter`s; wire the mode bit into the same
  reset term).

Two gateware shapes, to be decided by a **probe build (Phase 0)**:

**Option A — two engines + ULPI mux (preferred if it fits).** Instantiate `USBMIDIHost` and
`USBMSCHost` side by side; a CSR-driven mux hands the ULPI signals to one and parks the
other in reset. Zero changes to either engine for M6a. Cost: a duplicated enumerator/SIE —
estimate +2.5–3.5k LUTs on a design at 80% → ~92–95%, plausibly routable on ECP5 but with
real Fmax risk. Cheap to try: one probe build answers it definitively.

**Option B — combined host, shared enumerator (fallback).** Vendor a `USBDualModeHost`:
one `USBHostEnumerator`/SIE, the descriptor parser's class/subclass/protocol match constants
made runtime-`Mux`ed from the mode bit (they are plain equality compares in
`guh/usbh/descriptor.py`), and both thin class FSMs on top with only one active. Adds
~1–1.5k LUTs. More surgery (touches `descriptor.py` matching, needs the enumerator's
`parser` handling audited for endpoint-filter differences: MIDI filters IN-only, MSC needs
IN_AND_OUT), but this is the shape upstream `guh` would likely accept as a PR.

Decision rule: run Phase 0 with Option A; accept it if post-route sync Fmax ≥ 60 MHz with
≥ 1 MHz margin across 2 seeds, else fall back to Option B.

**Outcome (M6a implementation): Option A shipped.** `../sid/top.py`'s `SIDSoc.elaborate`
instantiates `USBMIDIHost` and `USBMSCHost` with `bus=None` behind one shared
`UTMITranslator`, muxed by `usb_msc.mode_o`, each wrapped in a `ResetInserter` keyed off the
mode bit so the unselected engine sits in reset. Option B (shared enumerator) was never
needed — see `CLAUDE.md`'s status line for the build's post-route sync Fmax against the
60 MHz target.

## 4. Gateware

### 4a. M6a (read-only) — pure reuse

- Copy `USBMSCPeripheral` from `sid_player_sw/top.py:61-170` into a shared module (e.g.
  `src/tiliqua/usb_msc_csr.py`) or import it; register at CSR **`0x1300`** on the mbsid SoC
  (`0x1000` = SID_PERIPH, `0x1200` = SID_PERIPH_R). Add the mode bit + mux per §3 behind a
  `SIDSoc` opt-in flag (e.g. `with_usb_msc=False` default) so `top/sid` and existing tops
  elaborate byte-identically — the M4 `forward_sysex` pattern.
- **This is a CSR change:** `pdm mbsid build --pac-only` before `--fw-only` (root CLAUDE.md).

### 4b. M6b (write) — vendored MSC engine + TX path

Vendor `guh/engines/msc.py` → `src/vendor/guh_msc/msc.py` (BSD-3 header kept) and extend:

- `SCSIBulkHost.Command` gains `data_dir` (0=IN, 1=OUT); `bmCBWFlags` derives from it, not
  from `data_len > 0`. New `tx_data: In(stream.Signature(Packet(unsigned(8))))` + a
  `DATA-TX` state: per 512-byte chunk, stream bytes to `enum.ctrl.txs` (`CBW-LOAD` pattern),
  issue `start_bulk_out(endp_out)`, toggle `pid_out` on ACK, retry chunk on NAK. Then `CSW`
  as today.
- `USBMSCHost`: `SCSIOpCode.WRITE_10 = 0x2A`; `cmd` gains a `write` flag; `READY` dispatches
  to a `WRITE`/`WRITE-WAIT` leg mirroring `READ`/`READ-WAIT` (`_BLOCKS_PER_READ = 1` — one
  block per command keeps the FSM and FIFO sizing trivial; patch export is ~1.1 KB, write
  throughput is irrelevant).
- `USBMSCPeripheral` gains: `tx_data` CSR (W, 32-bit, fills a 128×32 word FIFO), a
  `start_write` strobe (legal only when the TX FIFO holds exactly 128 words; the peripheral
  unpacks words → bytes little-endian, symmetric to the RX packer), and `resp` reused as-is.
  Firmware contract: fill 128 words, strobe, poll `resp`/`busy`.
- Error handling stays retry-at-firmware-level: on `resp.error`, firmware re-issues the
  block write once, then fails the file operation visibly (no silent success). REQUEST_SENSE
  refinement can come later; the CSW status already distinguishes success/failure.
- Sim test: extend/copy whatever drives `USBMSCHost` today (check `guh` upstream tests) with
  a mock SIE asserting: CBW bytes carry flags=0x00 + opcode 0x2A + correct BE LBA, 512
  payload bytes emerge in order on bulk OUT, CSW consumed, `resp.done/error` correct, NAK
  mid-data retries the chunk with the same PID sequence.

## 5. On-disk format & directory layout

- Directory: `/MBSID/` on the first FAT partition (created on first export if absent; import
  also falls back to scanning the root dir so hand-copied files Just Work).
- File format: **standard MBSID v2 single-patch SysEx dump** (`F0 00 00 7E 4B <dev> 02 …
  F7`, 1024 nibblized data bytes + 7-bit checksum) — byte-compatible with reference patch
  `.syx` files, MIOS Studio, and our own `sysex_capture.rs` framing. A file exported by
  Tiliqua can be re-imported, sent to real MBSID hardware, or pushed back over TRS MIDI by
  any PC tool, unchanged.
- Export naming: `Pnnn_<name>.SYX` (8.3-safe: `Pnnn~1.SYX` via fatfs LFN off is fine too —
  decide at implementation; `nnn` = user-bank slot or `EDT` for the live edit buffer;
  `<name>` = patch body bytes 0..16, sanitized to FAT charset).
- Import accepts any file whose SysEx body parses (bank/type bytes ignored in file mode) —
  plus, as a convenience, raw 512-byte files (exact size match) treated as a bare
  `sid_patch_t`.

## 6. Firmware

### 6a. Reused wholesale (from `sid_player_sw/fw/src/`, adjusted paths only)

- `usb_msc.rs` — block-read driver (spin-capped). M6b adds `write_block(lba, &[u8;512])`.
- `partition.rs` — MBR/GPT first-partition LBA. Unchanged.
- `fat.rs` — `MscStorage` adapter. M6a unchanged (writes error). M6b: dirty flag on the
  512 B cache, `Write::write` mutates the cache via read-modify-write, `flush()` +
  sector-boundary crossings write back. Keep the single-sector cache — patch files are tiny.

### 6b. New: `fw/src/usb_patch.rs` (host-testable like `sid_scan.rs`)

- `list_syx(fs, out)` — scan `/MBSID/` (fallback root) for `.SYX`/512-byte files, bounded
  list (name + size), same shape as `list_root_sids`.
- `load_syx_by_index(fs, ix, &mut [u8;512]) -> Option<()>` — read file, run bytes through
  the file-mode SysEx parser (§6c) or accept raw 512.
- M6b: `export_syx(fs, name, &patch512)` — nibblize + checksum + write, `flush`, verify by
  reading back and re-parsing (cheap end-to-end check that the write path actually landed).

### 6c. `sysex_capture.rs` — file mode

Add a constructor/flag `SysexCapture::file_mode()` that relaxes the accept condition to any
cmd-0x02 patch dump (ignore bank/type/patch-number match; still enforce header, nibble
count, checksum, F7). The ISR/live path keeps today's strict bank-1 rule. Host tests: strict
mode rejects what it rejects today; file mode accepts a factory-bank dump and a reference file
fixture; both reject a corrupted checksum.

### 6d. Menu (`menu.rs`) — USB card

- Main card: `USB Mode: MIDI | Storage` row (persisted, §3).
- New `Card::Usb` (visible only in Storage mode, same collapse pattern as
  `lead_loaded`-gated rows — and remember the M5 lesson: derive visibility from live state
  every loop iteration, not from cached menu state): rows = drive status (`No drive` /
  `Ready N files`), file selector (name scroll), `Load` (audition — engine only), `Load→Slot
  nnn` (audition + M4 `UserPatchStore::save`), and M6b `Export: EDIT|Slot nnn → USB`.
- All USB/FAT I/O runs in the **main loop** on menu commands (Timer0 ISR keeps ticking the
  engine; a slow drive stalls only UI redraw — show a `BUSY` row state, don't redraw-spam per
  the DMAFramebuffer dirty-flag rule). The `read_block` 1M-iteration spin cap bounds the
  worst-case stall to well under a second per block.

### 6e. `main.rs` wiring

- Instantiate `UsbMsc` from the new PAC block; plumb into menu dispatch. Mount lazily on
  first USB-card entry (mount = partition scan + BPB read ≈ a handful of blocks), drop the
  `FileSystem` on drive-removed (`status.connected` low) or mode switch.
- Loaded-patch plumbing reuses the M4 path exactly: a file-sourced 512-byte image goes
  through the same `mbsid_bank_write`-equivalent entry the SysEx capture path uses today
  (patch → engine + optional flash slot), so engine-side behavior is provably identical to a
  MIDI upload of the same bytes.

### 6f. Footprint (re-verify at implementation)

- +`FileSystem` + caches + name list ≈ 2–3 KB (.bss/stack, main-loop only). Stack-paint
  re-measure per root CLAUDE.md; 21.8 KB headroom expected to absorb it easily.

## 7. Phasing & validation

**Phase 0 — probe build (½ day, no firmware).** mbsid + `USBMIDIHost` + `USBMSCHost` +
ULPI mux (Option A). Judge LUT% + post-route sync Fmax over 2 seeds (read the *second*
`Max frequency` line in `top.tim`). Decides Option A vs B before any real work.

**M6a — load (read-only).** Gateware §4a + firmware §6 minus export. Host tests: fat/scan/
parser suites (all runnable on PC, fixtures = MBSID v2 `.syx` + generated FAT images, same
harness as `sid_scan.rs`) — **110/110 green**, see `CLAUDE.md`. Gateware built and passing
timing — see `CLAUDE.md`'s status line for the current post-route sync Fmax. **Not yet run
on real hardware.**

### 7a. M6a hardware checklist (record results here once hardware is available)

Plain checklist, not something executable in this environment — no hardware is available
here. Walk this in order on a real Tiliqua r5 with a USB-C-to-A adapter and a FAT32 thumb
drive containing a few `.syx` files under `/MBSID/`:

- [ ] **Drive enumerates.** Switch `USB Mode` to `Storage`, plug in the drive, open the
  `Usb` card. `Drive` row goes `No drive` → (briefly `BUSY`) → `Ready (N files)` with `N`
  matching the number of `.syx`/512-byte files actually on the drive.
- [ ] **Files listed correctly.** Scroll the `File` row through all `N` entries; names match
  the files on the drive (spot-check a few, including one in `/MBSID/` and, if tested, one
  falling back to the root-dir scan).
- [ ] **Load auditions correctly.** Pick a file, commit on `File` (audition-only load).
  Compare by ear (and ideally by SID-register capture) against the *same* patch sent via TRS
  SysEx RAM Write — must sound/diff identical, since both paths land in the engine through
  the same entry point.
- [ ] **Load→Slot persists across power cycle.** Pick a file, commit on `Load>Slot` into a
  chosen User slot. Power-cycle the module, switch to Bank `User`, select that slot — the
  patch loads and sounds the same as it did on first load.
- [ ] **Unplug mid-browse degrades cleanly.** With the `Usb` card open and a file selected,
  physically unplug the drive. `Drive` row falls back to `No drive`, `File` shows `-`, no
  hang/freeze, encoder navigation keeps working, audio keeps playing throughout.
- [ ] **Mode switch back to MIDI re-enumerates a keyboard.** With a drive plugged and then
  removed (or still plugged), switch `USB Mode` back to `MIDI`, plug in a MIDI keyboard/
  controller — it enumerates and plays normally, same as before M6a existed.
- [ ] **TRS MIDI keeps playing in Storage mode.** With `USB Mode` = `Storage` and a drive
  plugged, play notes over the TRS MIDI input — audio responds normally the whole time,
  including while a load is in progress.
- [ ] **Stack-paint re-measure** (methodology: root `CLAUDE.md`'s RAM-budget-checks gotcha;
  prior measurement `M4_USER_PATCH_BANKS.md §6f`). Fill the stack region with a sentinel
  byte at boot, exercise the deepest realistic path — menu navigation into the `Usb` card,
  a directory listing, a `Load>Slot` — then scan for the high-water mark and log over UART0.
  Confirm actual peak stack usage stays comfortably inside the ~21.8 KB headroom measured
  post-M4 (the `FileSystem` + caches + name list adds an estimated 2–3 KB, per §6f above —
  this step turns that estimate into a real hardware number).

**M6b — export (write).** Gateware §4b (vendored engine + TX CSR path, sim-tested), fat
write-back, `export_syx` + menu rows. Hardware checklist: exported file mounts clean on a PC
(`fsck.vfat` clean), byte-identical round-trip (export → PC → send same file over TRS SysEx
→ engine state identical; and export → re-import on device → 512-byte compare equal),
unplug during export leaves a mountable filesystem with at worst a truncated/missing file
(export writes payload before directory-entry finalize where fatfs ordering allows; verify
empirically), repeated exports don't leak clusters (`fsck.vfat` after 50 exports).

**Stopgap export (zero gateware, available today):** the M4 user bank lives at flash
`0xF00000..0xF80000` in 4 KiB slots with an 8-byte `MBUP` header. PC-side flash tooling can
read that region over the debug/bootloader USB and a small host script can emit `.syx` files
(header parse + 512-byte payload + nibblize). Developer-grade UX (reboot to bootloader), but
worth having as a recovery/backup path regardless of M6 — and it de-risks "patches are
trapped in flash" immediately.

## 8. Risks

| Risk | Exposure | Mitigation |
|---|---|---|
| Area/Fmax: +USB engine on an 80%-full, 61.76 MHz design | High (project-gating) | Phase 0 probe; Option B fallback; last resort: `with_sysex` and MSC engine made build-time exclusive (two bitstream variants — ugly, avoid) — superseded: M6a measured 91% LUT, 66.41 MHz post-route sync Fmax PASS |
| FAT corruption on unplug during export | Medium | Writes only on explicit user action; flush eagerly; `BUSY` indicator; verify-by-readback; document "don't unplug while BUSY" |
| Quirky drives (slow spin-up, non-512 blocks) | Medium | `block_size()==512` guard already in driver (reject others visibly); `guh` 10 s init watchdog handles slow SSDs; test a cheap flash stick + an SSD enclosure |
| `guh` fork drift | Low | Vendor only `engines/msc.py`; `usbh/*` internals stay upstream-pinned; offer write support upstream as a PR |
| Mode-switch wedge (half-enumerated device) | Low | Mode bit resets the incoming engine; both engines already watchdog-reset on stall |
| Main-loop stall from a stalling drive | Low | Per-block spin cap already in `usb_msc.rs`; per-op block-count cap in `usb_patch.rs` |

## 9. Documentation follow-ups (with the implementation, not before)

- `docs/` user guide: USB Mode row, patch load/export walkthrough, drive format
  requirements (FAT32, MBR, ≤1 partition tested), unplug warning.
- `CLAUDE.md` (this dir): USB mode mux + `0x1300` CSR + PAC-regen note; update the "no
  export path" framing in the no-MIDI-TX gotcha once export lands.
- `DESIGN.md`: add M6 to the milestone table.

## 10. Reference pointers

- Read stack (proven): `../sid_player_sw/top.py:61-270` (CSR periph + wiring),
  `../sid_player_sw/fw/src/{usb_msc,fat,partition,sid_scan}.rs`.
- MSC engine to vendor/extend: `.venv/…/guh/engines/msc.py` (`SCSIBulkHost`, `USBMSCHost`).
- MIDI host being displaced/muxed: `.venv/…/guh/engines/midi.py`; instantiation
  `../sid/top.py:607-644` (incl. the VBUS gating to change).
- Patch/SysEx formats: `fw/src/sysex_capture.rs` (framing), `fw/src/patch_store.rs` (flash
  bank), `M4_USER_PATCH_BANKS.md §6b-c`.
- Area/Fmax baseline: `build/mbsid-r5/top.tim` (19444/24288 COMB, 61.76 MHz).
