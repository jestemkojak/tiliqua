# MBSID SysEx Patch Upload → Flash User Bank — Design (M5, draft)

**Date:** 2026-07-02
**Branch:** `mbsid-port`
**Status:** DRAFT — written from source-verified analysis; decision points the user has not
yet confirmed are marked **[DECISION]**. Do not implement until those are resolved and the
doc is approved.
**Depends on:** M4 (`M4_USER_PATCH_BANKS.md`) — specifically `patch_store.rs` (flash user
bank, 128 × 4 KiB sectors at `0xF00000..0xF80000`, magic-prefixed slots). This spec is the
"separate follow-on spec" M4 §5 anticipated. **[DECISION]** whether M4's on-device save UI
still ships first, or M5 subsumes `patch_store.rs` and lands independently.

---

## 1. Goal & non-goals

**Goal.** Receive MBSID-protocol SysEx patch dumps (from MIDIbox SID Editor or any tool
speaking the protocol) over TRS or USB MIDI, and:
- **RAM Write** (`type & 0xf8 == 0x08`): apply the patch live via the upstream engine —
  works with zero new C++ once bytes reach the engine.
- **Bank Write** (`type & 0xf8 == 0x00`): persist the patch into a flash user-bank slot.

**Non-goals:**
- MIDI TX / ACK-DISACK replies (§7 — documented limitation, possible M6).
- Patch Read / dump-to-editor (`cmd 0x01`), Ensemble dumps (`type 0x70`, upstream TODO),
  ASID. Parameter Write (`cmd 0x06`) works for free via the engine but is not validated here.
- Any edit to vendored `mios32/` C++ (impossible to hook `bankSave` anyway, §2).

## 2. Source-verified constraints (why the design is shaped this way)

All refs are `mios32/apps/synthesizers/midibox_sid_v3/core/`.

1. **Upstream receive path is complete** (`MbSidSysEx.cpp`): header
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
   Forwarding Bank Writes to the engine is therefore harmless (state machine stays
   consistent; current sound is not changed — matching real MBSID semantics where a bank
   write does not re-patch the live voice).
4. **The engine sends ACK/DISACK after every dump** via `sendAck` →
   `MIOS32_MIDI_SendSysEx`, which our facade **swallows** (`fw/csrc/mios32_shim/mios32.h:163`,
   comment says exactly this). No TX path exists in this port. See §7.
5. **Gateware drops all SysEx today**: `MidiSysexFilter` (`tiliqua/midi/decode_serial.py:95`)
   drains `F0..F7`; `MidiDecodeUSB` routes `SYSEX_START/END_1/2/3` CIN packets to `DRAIN`
   (`decode_usb.py`). Both carry a `TODO: add a sideband stream for sysex messages`.
   The codebase's own template for an opt-in sideband is `MidiRTFilter(forward=True)`
   → `o_rt`.
6. **Interrupted-SysEx engine quirk**: `MbSidSysEx::parse` aborts on any status byte
   ≥ 0x80 mid-message (`cmdFinished`), consuming it — a subsequent `F0` that arrives as
   the aborting byte is *not* re-processed as a new header start, so the next message
   would be lost. The gateware sideband must therefore deliver **well-framed** messages
   (always terminated with `F7`, §3), and firmware should drive the upstream `timeOut()`
   hook on RX gaps (§5).

## 3. Gateware — SysEx sideband (shared module, opt-in)

**Approach: modify the existing shared decoders** (`tiliqua/midi/decode_serial.py`,
`decode_usb.py`) with a constructor flag, exactly mirroring `forward_rt`. A separate
decoder was considered and rejected: it would duplicate the serial RX, running-status FSM,
and USB CIN parsing — two divergent copies of subtle MIDI logic — for zero benefit. With
the flag defaulted off, the port doesn't exist and elaborated logic is unchanged, so
**no other top is affected** and no re-validation of other bitstreams is needed.

### 3a. `MidiSysexFilter(forward=False)` (serial path)
- When `forward=True`, expose `o_sysex: Out(stream.Signature(unsigned(8)))`.
- `PASS` state: on `F0`, emit it on `o_sysex` and enter `SYSEX`.
- `SYSEX` state: forward every byte to `o_sysex`. Termination:
  - `F7` → forward it, back to `PASS`.
  - any other status byte (interrupted message) → emit a **synthetic `F7`** on `o_sysex`
    first, then re-handle the status byte on the normal path (it must not be swallowed —
    today's filter consumes it; the forwarding variant must return it to the main stream).
    This guarantees the sideband is always `F0 … F7`-framed (see §2.6).
    RT bytes never reach this filter (already stripped upstream by `MidiRTFilter`).
- **Backpressure is honored** (unlike `o_rt`'s fire-and-forget): when `o_sysex` is not
  ready, stall the input (`i.ready` low). SysEx is bulky (one patch dump ≈ 1.6 KB on the
  wire; a full bank dump 128×) — dropping bytes silently corrupts patches. Note this
  stalls the *whole* MIDI stream behind a full sideband; acceptable because the CSR FIFO
  is drained every 1 ms tick (§5) and serial MIDI is only ~3.1 KB/s.

### 3b. `MidiDecodeUSB(forward_sysex=False)`
- Same flag/port. In `STATUS` (and subsequent byte states), the CIN cases
  `SYSEX_START` (3 bytes), `SYSEX_END_1/2/3` (1/2/3 bytes) — today `DRAIN`ed — emit their
  payload bytes on `o_sysex`, with honored backpressure (hold `i.ready` low while emitting).
  USB can burst far faster than serial; backpressure propagates to the `USBMIDIHost`
  packet stream, which is the correct throttle.
- Same synthetic-`F7` framing rule if a dump is cut off (cable unplug mid-dump →
  `timeOut()` in firmware also covers this, §5).

### 3c. `SIDPeripheral` — new `sysex_read` CSR + FIFO (flag-gated)
- New param `with_sysex=False` on `SIDPeripheral`/`SIDSoc`; `MBSIDSoc` (`top/mbsid/top.py`)
  passes `True`. `top/sid` elaborates unchanged (no CSR map change there).
- New register `sysex_read` (offset: next free after `usb_midi_endp` @ `0x14` → `0x18`),
  16-bit read: **bit 8 = valid, bits 7:0 = data byte**. (The `midi_read` "read until 0"
  idiom does NOT work here — `0x00` is a legal SysEx data byte, so an explicit valid bit
  is required.) Backed by a `SyncFIFOBuffered(width=8, depth=64)`; upstream backpressure
  means the FIFO never overflows silently — when full, the decoder stalls (§3a/§3b).
- The existing `usb_midi_host` CSR mux selects which decoder's `o_sysex` feeds the FIFO
  (same `m.If/Else` `wiring.connect` pattern as `i_midi`); the unselected side is drained
  (`ready=1`) so a source switch mid-dump can't wedge the other decoder.
- **PAC regen required** (`pdm mbsid build --pac-only`) — first CSR change since M2.

### 3d. Gateware tests
Amaranth sim (in `tests/`, alongside existing MIDI decoder tests): for each decoder,
(a) flag off → behavior bit-identical to today; (b) flag on → a `F0 … F7` byte sequence
appears verbatim on `o_sysex` while channel messages still parse on `o`; (c) interrupted
SysEx → synthetic `F7` emitted and the interrupting status byte still parsed normally;
(d) backpressure: `o_sysex.ready=0` stalls input without byte loss.

## 4. Shim — two one-line functions (`fw/csrc/mbsid_shim.cpp`)

```c
// Feed one raw SysEx byte to the engine's parsers (MbSidSysEx + MbSidAsid).
// Returns nonzero if the byte was consumed as SysEx.
int  mbsid_sysex_byte(uint8_t b);        // env.midiReceiveSysEx(DEFAULT, b)
// Abort a half-received message after an RX gap (upstream MIDI-timeout hook).
void mbsid_sysex_timeout(void);          // env.midiTimeOut(DEFAULT)
```
Plus matching `fw/src/mbsid_sys.rs` FFI (riscv extern + host stub, existing pattern).
No upstream edits. `MbSidAsid` also parses these bytes; it stays inert unless an ASID
stream is sent (out of scope, harmless — it's already linked, per `CLAUDE.md`).

## 5. Firmware — receive loop + Bank Write capture

### 5a. Drain (Timer0 ISR, after the existing `midi_read` drain)
Engine access is single-threaded through the 1 kHz ISR (`mbsid_tick` vs.
`midiReceiveSysEx` must not race), so SysEx bytes are fed **in the ISR**:
- Each tick, pop up to **32 bytes** from `sysex_read` (valid-bit loop). 32 B/ms = 32 KB/s
  drain ≫ 3.1 KB/s serial; USB is backpressured by the FIFO, so a cap costs only latency
  (a full 1.6 KB patch dump ≈ 50 ms — irrelevant), never data. Cap keeps the ISR bounded.
- Each byte goes to **both** consumers, in order:
  1. `mbsid_sysex_byte(b)` — engine applies RAM Writes live (the whole upstream path,
     checksum included, for free).
  2. the Rust `SysexCapture` parser (§5b) — persists Bank Writes.
- **Timeout**: if a message is in progress (either parser mid-message) and no SysEx byte
  arrived for **≥ 500 ms**, call `mbsid_sysex_timeout()` and reset `SysexCapture`.
  (Upstream calls `timeOut()` from its MIDI scheduler on RX gaps; 500 ms is our choice —
  long enough for editor inter-message pauses, short enough to recover a wedged parser.)

### 5b. `fw/src/sysex_capture.rs` (new, host-testable, no FFI)
A byte-at-a-time state machine mirroring `cmdPatchWrite`'s framing exactly:
header `F0 00 00 7E 4B 00`, cmd `0x02`, then type/bank/patch, nibble pairs assembled
in place into a `[u8; 512]` (low nibble first), running checksum, then checksum byte + `F7`.

Accept-and-persist condition (all must hold; anything else = ignore silently, engine
still sees the bytes and does its own thing):
- `type & 0xf8 == 0x00` (Bank Write) and `type & 0x07 == 0` (sid 0 — we present one
  logical MBSID; the stereo pair is one engine).
- checksum valid, exactly 1024 data bytes, terminated by `F7` (a synthetic `F7` from §3a
  arriving early = wrong length = reject).
- bank byte == **1** (the User bank, matching the M4 on-device convention
  `0 = Factory, 1 = User`) → user-bank slot = patch byte (0–127). Bank 0 is the
  read-only Factory ROM: a Bank Write addressed to it is ignored (never persisted).
  Banks ≥ 2 don't exist and are ignored. (User-confirmed 2026-07-02.)
- **[DECISION]** RAM Writes are applied live by the engine but NOT auto-persisted
  (matches MBSID semantics: editor "send" = audition, "store" = Bank Write). Alternative:
  a menu toggle to also capture RAM Writes into a "last received" slot.

On accept: stash `(slot, [u8;512])` into `App` as `pending_save` and set a flag —
**no flash I/O in the ISR** (sector erase + program takes ms–tens of ms; the ISR budget
is 1 ms).

### 5c. Main loop — the actual flash write
Main loop (currently menu/idle) checks `pending_save` under `critical_section`, takes it,
and calls `patch_store.save(slot, &bytes)` (M4 component). Audio keeps running during the
write — the engine ticks from BRAM/registers; flash writes don't touch the audio path.
On success, set a `dirty` UI flag to flash a "Saved U<slot>" status line (reuses M4's
menu redraw); on flash error, "Save failed" — this is the only user feedback we can give
without MIDI TX (§7).

### 5d. Footprint (must re-verify at implementation)
New `.bss`: 512 B patch buffer + ~16 B parser state + the `pending_save` option.
`CLAUDE.md` records `.bss` 6884 B + stack 25880 B against the 0x8000 mainram — the budget
is nearly exact; re-measure the map after adding the buffer and shrink the reserve
consciously, don't discover it via stack overflow.

## 6. Validation

- **Host unit tests** (`cargo test --target x86_64-unknown-linux-gnu --lib`):
  `sysex_capture` — golden encode/decode round-trip (build the SysEx encoding of a known
  512-byte patch, feed byte-wise, assert exact buffer + slot), wrong checksum → reject,
  truncated (synthetic-`F7`) → reject, RAM Write type → no capture, bank/sid filtering,
  interleaved garbage between messages.
- **Oracle extension** (`host_oracle/run_oracle.sh`): new sequence command `syx <hex>`
  feeding `mbsid_sysex_byte` in both drivers. Test: RAM-Write dump of factory patch N
  must produce a register stream **byte-identical** to `pc N` — proves the engine-side
  SysEx path end-to-end with zero gateware.
- **Gateware sim**: §3d.
- **Hardware acceptance**: from MIDIbox SID Editor (or a `amidi`/`sendmidi` script, which
  avoids the ACK-wait problem, §7): (1) RAM Write → sound changes live; (2) Bank Write
  (bank 1) to slot k → "Saved U k", power-cycle, browse User bank, slot k loads and
  sounds identical; (2b) Bank Write to bank 0 (Factory) → ignored, nothing persisted;
  (3) same over USB MIDI; (4) mid-dump unplug → recovers within timeout, next dump OK.

## 7. Known limitation — no ACK/DISACK (MIDI TX)

Every dump is answered upstream by ACK/DISACK; our facade swallows it. Consequences:
- Tools that fire-and-forget (scripted `amidi`/`sendmidi`, most single-patch "send"
  buttons) work fine.
- Editor workflows that **wait for ACK per patch** (typically full-bank uploads) will
  time out or stall. Workaround: scripted per-patch sends with a fixed inter-message gap.

Fixing this needs a MIDI TX path (gateware serial TX or USB MIDI IN endpoint toward the
host + facade `MIOS32_MIDI_SendSysEx` routing to it) — deliberately out of scope;
candidate M6. The facade comment already anticipates this routing.

## 8. Reference pointers

- Upstream protocol: `mios32/.../core/MbSidSysEx.cpp` (`parse` :74, `cmd` :150,
  `cmdPatchWrite` :229), `MbSidEnvironment.cpp` (`bankSave` :96, `midiReceiveSysEx` :199,
  `sysexSetPatch` :253).
- Facade stubs: `fw/csrc/mios32_shim/mios32.h:163` (`SendSysEx`), `:192` (`DeviceIDGet`).
- Sideband template: `MidiRTFilter` in `tiliqua/midi/decode_serial.py:46`; SysEx drop
  sites: `decode_serial.py:95`, `decode_usb.py` `DRAIN` state.
- MIDI mux + CSR wiring to copy: `top/sid/top.py:578-607` (decoders + `usb_midi_host`
  mux), `:296-438` (`midi_read` FIFO/CSR — but see §3c on the valid-bit difference).
- ISR drain pattern to extend: `top/mbsid/fw/src/main.rs` `timer0_handler`.
- Flash store (dependency): `M4_USER_PATCH_BANKS.md §3c` (`patch_store.rs`).
