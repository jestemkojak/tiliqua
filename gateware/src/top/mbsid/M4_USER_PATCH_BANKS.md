# MBSID Writable User Patch Bank + SysEx Upload — Design (M4)

**Date:** 2026-07-02 (merged; supersedes the 2026-07-01 draft and the short-lived
`M5_SYSEX_UPLOAD.md`, folded in per user decision)
**Branch:** `mbsid-port`
**Status:** IMPLEMENTED (2026-07-02, commits `f81da90`..`12c8eca`) — gateware,
firmware, shim/FFI, host unit tests, and oracle coverage all green (see §7). Hardware
bring-up is pending (§7 hardware acceptance checklist, unchecked). Single milestone;
bank 1 = User; RAM Write = audition only; on-device save UI kept as groundwork for
future on-device patch editing. Chosen-default details marked **[DEFAULT]** were
implemented as written.
**Scope:** builds on M1 (Lead mono), M2 (dual-SID stereo), M3 (read-only
factory ROM bank, `M3_PATCH_BANKS.md`).

---

## 1. Goal & non-goals

**Goal.** One milestone with three cooperating pieces:
1. **Flash user bank** — 128 writable patch slots persisted in SPI flash, browsable and
   loadable alongside the read-only Factory ROM bank.
2. **On-device save UI** — browse either bank via the existing encoder menu; save the
   currently-loaded patch into a chosen User slot ("save as"/duplicate). Kept (despite
   SysEx existing) as groundwork for future on-device patch editing.
3. **MIDI SysEx patch upload** — receive MBSID-protocol patch dumps (MIDIbox SID Editor
   or any tool speaking the protocol) over TRS or USB MIDI:
   - **RAM Write** (`type & 0xf8 == 0x08`): applied live by the upstream engine —
     audition only, never auto-persisted (matches MBSID editor semantics: "send"
     auditions, "store" persists). User-confirmed.
   - **Bank Write** (`type & 0xf8 == 0x00`, bank byte **1**): persisted into the flash
     user bank. Bank 0 is the Factory ROM (read-only) — writes to it are ignored.
     User-confirmed; matches the on-device convention `0 = Factory, 1 = User`.

**Non-goals:**
- On-device patch parameter editing (future milestone; this UI is its foundation).
- MIDI TX / ACK-DISACK replies (§8 — documented limitation, candidate follow-on).
- SysEx Patch Read / dump-to-editor (`cmd 0x01`), Ensemble dumps (`type 0x70`, an
  upstream TODO), ASID. Parameter Write (`cmd 0x06`) works for free via the engine but
  is not validated here.
- Multiple user banks (one 128-slot User bank only).
- Any edit to vendored `mios32/` C++ (impossible to hook `bankSave` anyway, §2).

## 2. Source-verified constraints (why the design is shaped this way)

All refs are `mios32/apps/synthesizers/midibox_sid_v3/core/`.

1. **Upstream SysEx receive path is complete** (`MbSidSysEx.cpp`): header
   `F0 00 00 7E 4B <devId>` (devId = 0, our facade's `MIOS32_MIDI_DeviceIDGet()` returns 0),
   command byte (`0x02` = Patch Write), then type / bank / patch bytes, 1024 nibblized data
   bytes (low nibble first → 512-byte `sid_patch_t`), checksum `(-sum & 0x7F)`, `F7`.
   Dispatch: `cmdPatchWrite` (`:229`) → `MbSidEnvironment::sysexSetPatch` (`:253`).
2. **`bankSave()` is a stub** returning `-2` (`MbSidEnvironment.cpp:96`) — no upstream
   persistence exists. It is a **non-virtual** method defined inside a vendored `.cpp`:
   it cannot be overridden, shadowed, or link-replaced without editing GPL vendored code.
   **Therefore Bank Write persistence must be captured outside the engine** — in firmware
   Rust, which owns the raw byte stream anyway.
3. **The engine silently "succeeds" on Bank Write**: `sysexSetPatch(toBank=true)` calls
   `bankSave`, *ignores* its return, and returns `true` (`MbSidEnvironment.cpp:258-260`).
   Forwarding Bank Writes to the engine is therefore harmless (its parser state stays
   consistent; the live sound is not changed — matching real MBSID semantics where a
   bank write does not re-patch the playing voice).
4. **The engine sends ACK/DISACK after every dump** via `sendAck` →
   `MIOS32_MIDI_SendSysEx`, which our facade **swallows** (`fw/csrc/mios32_shim/mios32.h:163`,
   comment says exactly this). No MIDI TX path exists in this port. See §8.
5. **Gateware drops all SysEx today**: `MidiSysexFilter` (`tiliqua/midi/decode_serial.py:95`)
   drains `F0..F7`; `MidiDecodeUSB` routes `SYSEX_START/END_1/2/3` CIN packets to `DRAIN`
   (`decode_usb.py`). Both carry a `TODO: add a sideband stream for sysex messages`.
   The codebase's own template for an opt-in sideband is `MidiRTFilter(forward=True)`
   → `o_rt`.
6. **Interrupted-SysEx engine quirk**: `MbSidSysEx::parse` aborts on any status byte
   ≥ 0x80 mid-message (`cmdFinished`), consuming it — a subsequent `F0` arriving as the
   aborting byte is *not* re-processed as a new header start, so the next message would
   be lost. The gateware sideband must therefore deliver **well-framed** messages
   (always `F0 … F7`, §4a), and firmware drives the upstream `timeOut()` hook on RX
   gaps (§6a).
7. **Getting the live patch out for on-device save is trivial**: `mbsid_bank_load`
   already raw-copies `env.mbSid[0].mbSidPatch.body` (`fw/csrc/mbsid_shim.cpp:79-90`);
   the save path just needs the same copy in the other direction. Reloading a saved
   patch is the existing `mbsid_load_patch()` — no new load-side C++.

## 3. Architecture & data flow

```
Menu (fw/src/menu.rs, extended)
  Bank row:    0 Factory (ROM, 128) | 1 User (flash, 128)
  Program row: 0-127; name from bankPatchNameGet (Factory) or flash name bytes (User)
  Save row:    destination User-slot cursor; commit = flash write (§6d)

Load path:
  Factory: mbsid_bank_load(0, program)             [existing shim fn]
  User:    patch_store.load(program)               [flash read 512B + header check]
             -> None (empty/bad header): render "Empty", do NOT touch the engine
             -> Some(bytes): mbsid_load_patch(bytes)  [existing shim fn]

Save paths (both end in the same flash write):
  On-device:  mbsid_current_patch_raw() ──────────────┐
  SysEx:      MIDI in ─► gateware sysex sideband ─► sysex_read CSR FIFO
                ─► ISR: mbsid_sysex_byte(b) [engine: RAM Write applies live]
                        + SysexCapture      [Rust: Bank Write bank 1 → 512B+slot] ─┤
                                                                                   ▼
                                             pending_save ─► main loop: patch_store.save
```

**Storage mechanism — flat sector-per-slot, not `sequential_storage`/KV.** The generic
option-persistence layer (`gateware/src/rs/opts/src/persistence.rs`) wraps
`sequential_storage::map` — built for many small frequently-rewritten keys with GC, welded
to a 32-byte buffer and the `Options` trait. Our pattern — 128 fixed 512-byte blobs,
individually addressable, written rarely — wants: **one 4 KiB erase sector per slot**
(128 × 4 KiB = 512 KiB). Save = erase sector + program header + 512 B. Load = read +
verify header before bytes ever reach the engine (an erased `0xFF` slot or torn write
must never reach `mbsid_load_patch`).

**Region placement.** `0xF00000..0xF80000` (512 KiB) carved from the unused flash tail:
bootloader `[0x0, 0x100000)`, 8 × 1 MiB bitstream slots `[0x100000, 0x900000)`
(`gateware/src/rs/manifest/src/lib.rs`, `spiflash_layout.py`). **Verified:** `pdm run
flash` (`gateware/src/tiliqua/flash/__init__.py:66`) only erases/programs the target
slot's own manifest regions — never full-chip; option-storage erase is opt-in. So user
patches survive normal reflashes. They do **not** survive a full chip erase (known
limitation, not solved here).

## 4. Gateware — SysEx sideband (shared module, opt-in)

**Approach: modify the existing shared decoders** (`tiliqua/midi/decode_serial.py`,
`decode_usb.py`) with a constructor flag, exactly mirroring `forward_rt`. A separate
decoder was considered and rejected: it would duplicate the serial RX, running-status
FSM, and USB CIN parsing — two divergent copies of subtle MIDI logic — for zero benefit.
With the flag defaulted off, the port doesn't exist and elaborated logic is unchanged,
so **no other top is affected** and no re-validation of other bitstreams is needed.

### 4a. `MidiSysexFilter(forward=False)` (serial path)
- When `forward=True`, expose `o_sysex: Out(stream.Signature(unsigned(8)))`.
- `PASS` state: on `F0`, emit it on `o_sysex` and enter `SYSEX`.
- `SYSEX` state: forward every byte to `o_sysex`. Termination:
  - `F7` → forward it, back to `PASS`.
  - any other status byte (interrupted message) → emit a **synthetic `F7`** on `o_sysex`
    first, then re-handle the status byte on the normal path (it must not be swallowed —
    today's filter consumes it; the forwarding variant must return it to the main
    stream). This keeps the sideband always `F0 … F7`-framed (see §2.6). RT bytes never
    reach this filter (stripped upstream by `MidiRTFilter`).
- **Backpressure is honored** (unlike `o_rt`'s fire-and-forget): when `o_sysex` is not
  ready, stall the input (`i.ready` low). SysEx is bulky (one patch dump ≈ 1.6 KB on the
  wire); dropping bytes silently corrupts patches. This stalls the whole MIDI stream
  behind a full sideband — acceptable because the CSR FIFO is drained every 1 ms tick
  (§6a) and serial MIDI is only ~3.1 KB/s.

### 4b. `MidiDecodeUSB(forward_sysex=False)`
- Same flag/port. The CIN cases `SYSEX_START` (3 bytes), `SYSEX_END_1/2/3` (1/2/3
  bytes) — today `DRAIN`ed — emit their payload bytes on `o_sysex`, with honored
  backpressure (hold `i.ready` low while emitting). USB can burst far faster than
  serial; backpressure propagates to the `USBMIDIHost` packet stream, the correct
  throttle.
- Same synthetic-`F7` framing rule if a dump is cut off (cable unplug mid-dump —
  firmware `timeOut()` also covers this, §6a).
- **Implemented deviation:** the USB decode path (`MidiDecodeUSB`) has no in-band
  cut-off signal at the packet level equivalent to a serial status byte, so a mid-dump
  USB unplug does not emit a synthetic `F7` there — recovery instead relies entirely
  on the firmware's 500 ms idle timeout (§6a, `mbsid_sysex_timeout()`), which resets
  both parsers on the next byte after the gap. Functionally equivalent (the wedged
  parser is cleared before the next dump), just via the firmware timeout path rather
  than a gateware-synthesized frame terminator.

### 4c. `SIDPeripheral` — new `sysex_read` CSR + FIFO (flag-gated)
- New param `with_sysex=False` on `SIDPeripheral`/`SIDSoc`; `MBSIDSoc`
  (`top/mbsid/top.py`) passes `True`. `top/sid` elaborates unchanged.
- New register `sysex_read` — implemented at offset **`0x24`** (next free slot after
  `build_model` @ `0x18`, `txn_status` @ `0x1C`, and M2's `phi2_sel` @ `0x20`, which
  landed between this design's draft and implementation), 16-bit read: **bit 8 =
  valid, bits 7:0 = data byte**. (The `midi_read` "read until 0"
  idiom does NOT work here — `0x00` is a legal SysEx data byte, so an explicit valid
  bit is required.) Backed by `SyncFIFOBuffered(width=8, depth=64)`; upstream
  backpressure means no silent overflow — when full, the decoder stalls.
- The existing `usb_midi_host` CSR mux selects which decoder's `o_sysex` feeds the FIFO
  (same `m.If/Else` `wiring.connect` pattern as `i_midi`); the unselected side is
  drained (`ready=1`) so a source switch mid-dump can't wedge the other decoder.
- **PAC regen required** (`pdm mbsid build --pac-only`) — first CSR change since M2.

### 4d. Gateware tests
Amaranth sim (in `tests/`, alongside existing MIDI decoder tests): per decoder,
(a) flag off → bit-identical behavior to today; (b) flag on → an `F0 … F7` sequence
appears verbatim on `o_sysex` while channel messages still parse on `o`;
(c) interrupted SysEx → synthetic `F7` emitted and the interrupting status byte still
parsed normally; (d) `o_sysex.ready=0` stalls input without byte loss.

## 5. Shim + FFI (no upstream edits)

`fw/csrc/mbsid_shim.cpp`, three new functions:
```c
// On-device save: copy the live patch out (mirror of the raw copy already done
// inside mbsid_bank_load, other direction).
void mbsid_current_patch_raw(uint8_t *buf512);   // copies env.mbSid[0].mbSidPatch.body
// Feed one raw SysEx byte to the engine's parsers (MbSidSysEx + MbSidAsid).
// Returns nonzero if consumed as SysEx.
int  mbsid_sysex_byte(uint8_t b);                // env.midiReceiveSysEx(DEFAULT, b)
// Abort a half-received message after an RX gap (upstream MIDI-timeout hook).
void mbsid_sysex_timeout(void);                  // env.midiTimeOut(DEFAULT)
```
Matching `fw/src/mbsid_sys.rs` FFI (riscv `extern "C"` + host stub, existing pattern).
`MbSidAsid` also sees the bytes; it stays inert unless an ASID stream is sent (out of
scope, harmless — already linked, per `CLAUDE.md`).

## 6. Firmware

### 6a. SysEx drain (Timer0 ISR, after the existing `midi_read` drain)
Engine access is single-threaded through the 1 kHz ISR (`mbsid_tick` vs.
`midiReceiveSysEx` must not race), so SysEx bytes are fed **in the ISR**:
- Each tick, pop up to **32 bytes** from `sysex_read` (valid-bit loop). 32 B/ms drain
  ≫ 3.1 KB/s serial; USB is backpressured by the FIFO, so the cap costs only latency
  (a full 1.6 KB dump ≈ 50 ms — irrelevant), never data. The cap keeps the ISR bounded.
- Each byte goes to **both** consumers, in order:
  1. `mbsid_sysex_byte(b)` — the engine applies RAM Writes live (whole upstream path,
     checksum included, for free).
  2. the Rust `SysexCapture` parser (§6b) — persists Bank Writes.
- **Timeout:** if either parser is mid-message and no SysEx byte arrived for ≥ 500 ms,
  call `mbsid_sysex_timeout()` and reset `SysexCapture`. **[DEFAULT]** 500 ms — long
  enough for editor inter-message pauses, short enough to recover a wedged parser.

### 6b. `fw/src/sysex_capture.rs` (new, host-testable, no FFI)
A byte-at-a-time state machine mirroring `cmdPatchWrite`'s framing exactly: header
`F0 00 00 7E 4B 00`, cmd `0x02`, type/bank/patch, nibble pairs assembled in place into a
`[u8; 512]` (low nibble first), running checksum, checksum byte, `F7`.

Accept-and-persist condition (all must hold; anything else = ignore silently — the
engine still sees the bytes and does its own thing):
- `type & 0xf8 == 0x00` (Bank Write) and `type & 0x07 == 0` (sid 0 — we present one
  logical MBSID; the stereo pair is one engine).
- bank byte == **1** (User). Bank 0 = Factory ROM, read-only → ignored, never
  persisted. Banks ≥ 2 don't exist → ignored. User slot = patch byte (0–127).
- checksum valid, exactly 1024 data bytes, terminated by `F7` (a synthetic `F7` from
  §4a arriving early = wrong length = reject).
- RAM Writes (`type & 0xf8 == 0x08`) are never captured — audition only (user-confirmed).

On accept: stash `(slot, [u8; 512])` into `App` as `pending_save` — **no flash I/O in
the ISR** (sector erase + program takes ms–tens of ms; the ISR budget is 1 ms).

### 6c. `fw/src/patch_store.rs` (new)
```rust
const USER_PATCH_FLASH_RANGE: Range<u32> = 0xF00000..0xF80000; // 512 KiB unused tail
const SLOT_SIZE: u32 = 4096;                                   // one erase sector/slot
// 8-byte slot header, written AFTER the payload within the same program sequence:
//   magic  [u8; 4] = *b"MBUP"
//   ver    u8      = 1
//   _pad   u8
//   cksum  u16     — additive checksum over the 512 payload bytes
// Torn-write safety: erase clears everything to 0xFF; load() verifies magic, version,
// AND payload checksum, so a save interrupted anywhere (post-erase, mid-program)
// reads back as empty/invalid — never as a corrupt patch fed to the engine.

pub struct UserPatchStore<F> { flash: F }
impl<F: NorFlash> UserPatchStore<F> {
    pub fn load(&mut self, slot: u8) -> Option<[u8; 512]>;  // None: empty/bad hdr/cksum
    pub fn save(&mut self, slot: u8, patch: &[u8; 512]) -> Result<(), Error>;
    pub fn name(&mut self, slot: u8) -> Option<[u8; 16]>;   // header + name bytes only
}
```
Generic over `F: NorFlash` so host tests inject an in-memory mock (flash can't run in
the host oracle) — this module is the save/load keystone test target.

### 6d. `menu.rs` — Bank/Program/Save rows
- `Row` gains `Save`; `bank` semantics extend to `0 = Factory, 1 = User`.
- Program row, User bank: `patch_store.load(program)` must be `Some` before
  `mbsid_load_patch` — on `None`, render "Empty" and skip the load entirely (empty-slot
  safety guard; same class of concern as M3's non-Lead-patch safety argument).
- Save row `on_turn` (Edit mode): scroll the destination cursor; **preview only**
  (query `patch_store.name()` for display; show "Empty" for unused slots). Never loads,
  never writes per-tick.
- **Commit/cancel [DEFAULT]:** the Save row's Edit-mode cursor is rendered as
  `Cancel, 0..127`, entering Edit at `Cancel`. Press (Edit → Nav) on a slot number =
  commit (the one write); press on `Cancel` = exit without writing. `on_press` returns
  `PressResult { None, Commit(slot), Cancel }` computed from `(focus, mode, cursor)`
  before toggling, so the write happens exactly once, only on deliberate confirmation.
- Saving is allowed regardless of which engine the live patch uses (Lead/Bassline/
  Drum/Multi) — the 512 bytes are engine-agnostic `sid_patch_t`, and reloading any of
  them is crash-safe (M3's argument).

### 6e. `main.rs` wiring
- Main loop checks `pending_save` under `critical_section` (both the SysEx path §6b and
  a menu commit §6d land here), takes it, calls `patch_store.save()`. Audio keeps
  running — the engine ticks from BRAM; flash writes don't touch the audio path.
- On success set the `dirty` UI flag and show `Saved U<slot>`; on error `Save failed`.
  This status line is the only feedback we can give without MIDI TX (§8).
- User-bank loads: `patch_store.load(program)` → `mbsid_sys::load_patch()` (existing
  M1-era fn). `mbsid_bank_load` stays Factory-only; upstream `SID_BANK_NUM` untouched.

### 6f. Footprint (re-verify at implementation)
New `.bss`: 512 B SysEx capture buffer + ~16 B parser state + 512 B `pending_save`
+ 512 B load/save scratch (share one buffer where possible). `CLAUDE.md` records
`.bss` 6884 B + stack 25880 B against the 0x8000 mainram — nearly exact; re-measure the
map after adding buffers and shrink the reserve consciously, don't discover it via
stack overflow.

**Note on `llvm-size`:** the default summary's "bss" column folds `.bss` +
`.heap` + `.stack` together (all NOLOAD sections) — `llvm-size -A` is needed to
separate them. The `.stack` section size the linker reports (25824 B) is just
the *leftover region* it assigns to the stack, not a measurement of what's
actually used; don't read it as a usage number.

**Peak stack, measured on hardware (2026-07-02):** a temporary stack-painting
probe (fill the stack region with `0xAA` at boot, scan for the high-water mark
from the main loop, log growth over UART0) settled at **4016 / 25824 bytes**
used after exercising menu navigation and an on-device save (the deepest
realistic call path: ISR → main-loop `UserPatchStore::save` → flash
erase/program). ~21.8 KB of headroom — the M4 additions (`SysexCapture`,
`pending_save`, `patch_buf`) fit comfortably; the whole-branch-review concern
about a near-exhausted budget was based on misreading the linker's leftover-
region size as a usage figure, not an actual measurement.

## 7. Validation

- **Host unit tests** (`cargo test --target x86_64-unknown-linux-gnu --lib`):
  - `patch_store` (mock flash): save/load round-trip, empty-slot detection, overwrite,
    torn-write simulation (truncate the mock mid-program → `load` returns `None`).
  - `sysex_capture`: golden round-trip (encode a known 512-byte patch as SysEx, feed
    byte-wise, assert exact buffer + slot), wrong checksum → reject, truncated
    (synthetic `F7`) → reject, RAM Write type → no capture, bank 0 / bank ≥ 2 / sid ≠ 0
    → no capture, garbage between messages tolerated.
  - `menu` Save row: cursor/preview, `Commit` exactly once on press-at-slot, `Cancel`
    writes nothing, "Empty" rendering.
- **Oracle extension** (`host_oracle/run_oracle.sh`): new sequence command `syx <hex>`
  feeding `mbsid_sysex_byte` in both drivers. Test: a RAM-Write dump of factory patch N
  must produce a register stream **byte-identical** to `pc N` — proves the engine-side
  SysEx path end-to-end with zero gateware.
- **Gateware sim**: §4d.

**Hardware acceptance checklist (manual, pending — not yet run on real hardware).**
SysEx items use `amidi`/`sendmidi` scripting, which sidesteps the ACK-wait problem
(§8):

- [ ] On-device save → power-cycle → reload identical; Cancel writes nothing; empty
      slots render "Empty" and don't load.
- [ ] Reflash a bitstream slot → user patches intact.
- [ ] SysEx RAM Write (e.g. `amidi -p hw:X -S "$(python3 -c 'print(dump_hex)')"`) →
      sound changes live, nothing persisted.
- [ ] SysEx Bank Write bank 1 slot k → `Saved U00k`, power-cycle, loads identically;
      bank 0 write → ignored.
- [ ] Same over USB MIDI; mid-dump unplug → recovers ≤ 500 ms idle, next dump OK.

## 8. Known limitation — no ACK/DISACK (MIDI TX)

Every dump is answered upstream by ACK/DISACK; our facade swallows it. Consequences:
- Fire-and-forget senders (scripted `amidi`/`sendmidi`, most single-patch "send"
  buttons) work fine.
- Editor workflows that **wait for ACK per patch** (typically full-bank uploads) will
  time out or stall. Workaround: scripted per-patch sends with a fixed inter-message gap.

Fixing this needs a MIDI TX path (gateware serial TX or USB routing toward the host,
plus facade `MIOS32_MIDI_SendSysEx` routed to it) — deliberately out of scope; candidate
next milestone. The facade comment already anticipates the routing. SysEx Patch Read
(`cmd 0x01`, dump-to-editor) becomes possible at the same time.

## 9. Documentation follow-ups (with the implementation, not before)

- `DESIGN.md §10` and `M3_PATCH_BANKS.md §8`: point at this doc.
- `CLAUDE.md` (this dir): SysEx path summary + the bank 0/1 convention + PAC-regen note.

## 10. Reference pointers

- Upstream protocol: `mios32/.../core/MbSidSysEx.cpp` (`parse` :74, `cmd` :150,
  `cmdPatchWrite` :229), `MbSidEnvironment.cpp` (`bankSave` :96, `midiReceiveSysEx`
  :199, `sysexSetPatch` :253).
- Facade stubs: `fw/csrc/mios32_shim/mios32.h:163` (`SendSysEx`), `:192` (`DeviceIDGet`).
- Sideband template: `MidiRTFilter` (`tiliqua/midi/decode_serial.py:46`); SysEx drop
  sites: `decode_serial.py:95`, `decode_usb.py` `DRAIN` state.
- MIDI mux + CSR wiring to copy: `top/sid/top.py:578-607` (decoders + `usb_midi_host`
  mux), `:296-438` (`midi_read` FIFO/CSR — see §4c for the valid-bit difference).
- ISR drain pattern to extend: `top/mbsid/fw/src/main.rs` `timer0_handler`.
- Raw-patch-copy-out precedent: `fw/csrc/mbsid_shim.cpp:79-90` (`mbsid_bank_load`).
- Flash HAL precedent (not reused, but the `NorFlash` usage pattern):
  `gateware/src/rs/opts/src/persistence.rs`.
- Flash layout: `gateware/src/rs/manifest/src/lib.rs`,
  `gateware/src/tiliqua/flash/spiflash_layout.py`,
  `gateware/src/tiliqua/flash/__init__.py`.
- Factory bank (read-only): `M3_PATCH_BANKS.md`. Menu to extend: `fw/src/menu.rs`.
