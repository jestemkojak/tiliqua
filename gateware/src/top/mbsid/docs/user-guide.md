# MBSID-on-Tiliqua — User Guide

This guide is for musicians using the `mbsid` bitstream on a Tiliqua. No
knowledge of the codebase is needed. For flashing/building the bitstream in
the first place, see the [Developer Guide](developer-guide.md) or ask
whoever builds your firmware.

## What it is

A stereo **dual-SID synthesizer module**: the classic MIDIbox SID v2/v3
sound engine (the MBSID v2 engine family) running against two
emulated MOS 8580 SID chips. You play it over MIDI; patches describe
oscillators, filters, LFOs, envelopes, arpeggiators and wavetables — the
engine turns them into SID register writes 1000 times a second.

It is a **multi-timbral MIDI module**, not a single mono synth: which sound
engine is active (Lead / Bassline / Drum / Multi) is decided by the loaded
patch, and each engine has a fixed MIDI channel layout (below).

## Panel connections

From the bitstream's I/O legend:

| Jack | Function |
|---|---|
| In 0–3 (left side) | **CV1–CV4** modulation inputs (assignable, see [CV Mod card](#cv-mod-card)) |
| Out 0 | **L** audio out (SID #0, three voices through its filter) |
| Out 1 | **R** audio out (SID #1) |
| Out 2, Out 3 | **L+R mono mix** (averaged, clip-safe) |
| Encoder | Navigate the on-screen menu (turn = move/adjust, press = select/toggle) |
| USB-C | **MIDI host** port — plug a MIDI *device* (keyboard/controller) into it |
| TRS jack | **MIDI in** (TRS MIDI) |
| Video out | Menu display |

> **Important — the USB port is a host port.** Tiliqua powers and enumerates
> a MIDI device you plug *into* it, exactly like a computer would. It will
> **never** appear as a MIDI device on your computer, and a plain USB cable
> from a PC to Tiliqua carries no MIDI in either direction (two USB hosts
> cannot talk to each other). To drive Tiliqua from a computer, use the TRS
> jack via a USB-MIDI interface. See
> [Troubleshooting](#troubleshooting).

## Quick start

1. Plug a MIDI keyboard into the **TRS MIDI in** (or into the USB-C port,
   then set `MIDI Src` to `USB` on the Main menu card).
2. Connect **Out 0 / Out 1** to your mixer (or Out 2 for mono).
3. Play on **MIDI channel 1** — the default patch is a Lead patch, which is
   fully playable from a single keyboard on channel 1.
4. Browse patches with the encoder on the **Main** card (Bank / Program
   rows), or send **MIDI Program Change** messages.

## Sound engines & MIDI channels

Each patch selects one of four engines. The channel layout belongs to the
*engine*, not the patch:

| Engine | MIDI channels | How voices map | Playable from one keyboard? |
|---|:---:|---|---|
| **Lead** | 1 | engine allocates up to all 6 oscillators (3 per SID: unison, detune, stereo spread, wavetable chords) | ✅ yes |
| **Drum** | 1 | each MIDI note = a drum instrument, like a GM drum part | ✅ yes |
| **Bassline** | 1, 2 | two independent basslines (one per channel), each split at note 60 | ⚠️ ch 1 plays bassline 1 only |
| **Multi** | 1–6 | 6 independent mono parts: ch 1–3 → Left SID osc 1–3, ch 4–6 → Right SID osc 1–3 | ⚠️ one part per channel |

- **Lead / Drum** are the live-play engines: one keyboard or pad controller
  on channel 1 plays everything.
- **Bassline / Multi** want a multi-channel source — a DAW, hardware
  sequencer or groovebox sending on channels 1–6, like driving any
  multi-timbral module.
- Pitch bend, CCs and channel aftertouch are forwarded to the engine on
  their real channel.

## Patch banks

Two banks of 128 patches each:

| Bank | Contents | Writable? |
|---|---|---|
| **0 — Factory** | the MBSID "vintage" factory bank, baked into the bitstream | read-only |
| **1 — User** | 128 flash slots, initially empty | ✅ via Save or SysEx |

- **MIDI Program Change** selects patches in the **factory** bank
  (programs 0–127).
- Empty user slots show as empty in the browser; loading one does nothing
  harmful.

## The menu

The on-screen menu is three **cards**. The top row of every card is the
Card selector — turning the encoder on it cycles **Main → CV Mod → Edit →
Main**. Elsewhere: turn to move between rows, press to enter/adjust a row,
turn to change the value, press again to commit.

### Main card

| Row | Function |
|---|---|
| Bank | Factory (0) / User (1) |
| Program | 0–127; loading happens when you commit |
| MIDI Src | `TRS` or `USB` — which physical MIDI input is live |
| Save | write the currently loaded (possibly edited) patch into a User slot you pick — "save as"/duplicate semantics |
| USB Mode | `MIDI` (default) or `Storage` — see [USB Mode: loading patches from a drive](#usb-mode-loading-patches-from-a-drive) |

`MIDI Src`, `USB Mode`, and the CV assignments persist across power cycles
(written to flash a couple of seconds after you change them).

## USB Mode: loading patches from a drive

Tiliqua's USB-C port can host **either** a MIDI device **or** a USB thumb
drive — never both at once, because it's one physical connector with one
plugged device. The Main card's `USB Mode` row switches between them:

- **MIDI** (default) — the port behaves as documented above: plug in a MIDI
  keyboard/controller, it enumerates as a MIDI source.
- **Storage** — the port instead hosts a USB drive for browsing and loading
  `.syx` patch files. Switching to Storage mode:
  - forces the live MIDI source to **TRS**, regardless of the `MIDI Src`
    row's setting — TRS MIDI keeps working the whole time you're browsing,
    so you can still play while you load patches;
  - powers the port (VBUS) unconditionally, so a drive is recognized as
    soon as it's plugged in;
  - re-enumerates whatever's plugged in — switching back to MIDI mode later
    re-enumerates a keyboard the same way.

### Drive format requirements

- **FAT32, MBR-partitioned** (or a bare FAT filesystem with no partition
  table). Only the **first partition** is read; a drive with multiple
  partitions only exposes the first one. exFAT and NTFS are not supported.
  Use a drive fresh out of a normal "format as FAT32" from Windows/macOS/
  Linux — no special tooling needed.
- Patch files go in a **`/MBSID/`** directory at the drive's root — create
  it and drop `.syx` files in. If no `/MBSID/` directory exists, the root
  directory itself is scanned as a fallback, so a drive with `.syx` files
  just dropped at the top level also works.
- Any file whose SysEx body parses as an MBSID v2 single-patch dump is
  accepted (the same format MIOS Studio uses), plus bare 512-byte
  raw patch files (exact size match, no SysEx wrapper).

### Browsing and loading

1. Set `USB Mode` to `Storage` on the Main card, plug in a drive.
2. The Card selector gains a **Usb** card (only reachable in Storage mode)
   with:
   - **Drive** — status: `No drive` or `Ready (N files)`.
   - **File** — scroll through the found `.syx`/raw-patch files by name.
   - **Load>Slot** — same file, but also persists it into a User bank slot
     you pick (like the Main card's Save row).
   - **Export** — write the live edit buffer or a User slot back to the
     drive as a `.syx` file (see [Exporting patches to a
     drive](#exporting-patches-to-a-drive) below).
   - **Import** — replace the *entire* User bank in one shot from a
     `/MBSID/BANK.SYX` file on the drive (see [Importing a whole bank from a
     drive](#importing-a-whole-bank-from-a-drive) below).
3. Turn to the **File** row and press to enter it, scroll to the patch you
   want, press again to commit — this **loads the patch into the engine
   immediately** (audition only, same as an incoming SysEx RAM Write — it
   is *not* saved anywhere until you use Load>Slot or the Main card's Save).
4. To keep the patch, use **Load>Slot** instead: pick the file, then pick a
   User slot the same way the Save row works — this loads it *and* writes
   it into that flash slot, so it survives a power cycle and shows up in
   the User bank afterwards.

A patch loaded this way goes through the exact same engine entry point as
a MIDI SysEx upload of the same bytes, so it sounds identical either way —
there's no separate "USB patch" code path in the engine.

Unplugging the drive at any point (mid-browse, mid-load) is safe: the
`Drive` row falls back to `No drive`, the file list clears, and the menu
never hangs waiting on a lost drive. (A load itself briefly blocks the
menu — see the note on this in the export section below, which applies
equally here.)

### Exporting patches to a drive

The `Usb` card's **Export** row writes a patch *to* the drive as a
standard MBSID v2 single-patch SysEx dump — this is currently the module's
only way to get a patch off the device (MIDI is receive-only; see
[Uploading patches over SysEx](#uploading-patches-over-sysex) below).

1. With `USB Mode` = `Storage` and a drive plugged in, open the `Usb`
   card and turn to the **Export** row.
2. Press to enter it, then scroll to choose a source:
   - **EDIT → USB** — the currently loaded/edited patch (the same buffer
     the Main card's `Save` row would write), exported as `/MBSID/EDIT.SYX`.
   - **Unnn → USB** — a specific User bank slot, exported as
     `/MBSID/Pnnn.SYX` (e.g. slot 42 → `P042.SYX`).
3. Press again to commit. The status line reports `Exported <filename>` on
   success or `Export FAILED` if the write didn't go through (drive
   removed, filesystem full, or the peripheral reported a SCSI write
   error) — nothing is left half-written on a `FAILED` result. On success,
   the device also flushes the drive's write cache before reporting
   `Exported` — the file is durably on the drive, not just handed off to
   its firmware, at that point.

**Don't unplug the drive while an export is in progress.** There is no
separate `BUSY` indicator for this — the whole menu (encoder input,
redraw) is unresponsive for the short duration of the write, because the
write runs synchronously in the firmware's main loop. Treat "the screen
isn't responding to the encoder" as the busy signal and wait for the
status line to update before touching the drive. Audio playback is
unaffected either way (the real-time engine tick runs from a separate
interrupt, not the main loop).

Filenames are plain 8.3 (`EDIT.SYX`, `P042.SYX`) — there is no long
filename support, matching `sid_player_sw`'s drive-access conventions.
Exported files are byte-compatible with MIOS Studio `.syx`
patches: they can be re-imported on this device via the `File` row above,
sent to another MIDIbox SID over MIDI, or opened in the MIDIbox SID
Editor on a PC.

### Importing a whole bank from a drive

The `Usb` card's **Import** row replaces the *entire* User bank (all 128
slots) in one operation from a single bank-dump file, instead of loading
patches one at a time.

1. Copy a bank-dump `.syx` file (e.g. one exported from MIOS Studio, or
   another MIDIbox SID) onto the drive as **`/MBSID/BANK.SYX`** — this exact
   8.3 name, in the `MBSID` folder; rename the file on your PC if it's
   called something else.
2. With `USB Mode` = `Storage` and the drive plugged in, open the `Usb`
   card and turn to the **Import** row. Turning it shows `Import Cancel`;
   turn once more to arm it to `Import REPLACE bank!` (cancel-first, the
   same confirm dance as the Main card's `Save` row, to guard against an
   accidental press).
3. Press while armed to commit. The status line reports one of:
   - `Imported N patches` — success; `N` is how many of the 128 slots the
     file actually populated (a sparse dump that only defines some slots is
     fine — the rest are left empty).
   - `No/bad BANK.SYX` — the file is missing, or failed to parse/validate
     (bad checksum, truncated, wrong bank).
   - `Import FAILED` — the file validated but writing to flash failed.
   - `USB mount FAILED` — the drive itself couldn't be mounted (same
     failure mode as browsing/loading).
4. **Import fully replaces the User bank — every slot not named in the file
   is cleared, not left alone.** This is a *replace*, not a *merge*: any
   patches you'd saved individually before importing are gone from slots
   the import didn't rewrite. There's no undo; if you want to keep your
   current bank, export the slots you care about first.

Like Export, Import writes/reads run synchronously in the firmware's main
loop with no live `BUSY` indicator — the menu freezing (unresponsive to the
encoder) for the duration of the import *is* the busy signal, same as
described in the Export section above. Don't unplug the drive while the
menu is frozen. Unlike Export, Import only writes to the device's internal
flash, never back to the drive.

### CV Mod card

Assigns each of the four CV inputs (CV1–CV4) to a modulation target:

- **Off** — input ignored.
- **Knob1–Knob5** — the patch's knob matrix, the same destinations a MIDI
  CC would drive. What a knob does depends on the patch's mod-matrix
  programming.
- **Volume / Phase / Detune / Cutoff / Reso** — direct engine parameters
  (the "parSet" common block).
- **Pitch / Gate** — together they form a CV note machine on MIDI
  channel 1: Pitch tracks **1 V/octave, 0 V = C2**, quantized to semitones
  with hysteresis (no flutter at note boundaries); Gate opens above **2 V**
  and closes below **1 V**.
  - Gate alone (no Pitch assigned) retriggers a fixed **C‑4** on every
    gate-on.
  - **Pitch alone is inert** — without a Gate assignment it never produces
    a note. You need both.

Continuous targets are deadbanded on their 8-bit scaled value, so cable
noise doesn't spam the engine. Retargeting an input (or clearing a Gate
assignment while a note is held) releases any note it holds — no stuck
notes.

### Edit card

Live-edits the loaded **Lead** patch: ~32 curated parameters (master
Volume/Detune/Phase, filter Cutoff/Reso/Mode/Channel, per-oscillator
Wave/ADSR/Pulsewidth/Portamento for OSC1–3, LFO1/2 Rate & Depth). Each
change is audible immediately **and** written into the in-memory patch
body, so it's exactly what gets stored when you Save. Oscillator and filter
edits are mirrored to both SIDs, preserving the factory patches' L/R
symmetry (stereo width comes from Detune, not divergent voice settings).

Things to know:

- **Lead patches only.** For Bassline/Drum/Multi patches the card shows a
  placeholder — their parameters aren't exposed here. SysEx from the
  MIDIbox SID Editor remains the full-surface editing route for everything.
- **Unsaved edits are volatile.** A `*` after the patch name marks unsaved
  edits. There is **no confirmation prompt**: loading another patch — from
  the menu *or* via an incoming MIDI Program Change — discards edits
  immediately. Save to a User slot first if you want to keep them.
- **CV vs. Edit on the same parameter:** while a CV assigned to the same
  target is actively moving, the CV wins for what you hear (it's re-applied
  1000×/s); once the CV settles, your Edit-card value takes over live. The
  Edit value is always the one that gets Saved, regardless of CV.

## Uploading patches over SysEx

The firmware speaks the standard **MBSID SysEx protocol**, so the MIDIbox
SID Editor or any scripted tool (`amidi`, `sendmidi`) can send patches:

- **RAM Write** — auditioned live immediately, **never saved**. This
  matches MBSID editor semantics: "send" auditions, "store" persists.
- **Bank Write to bank 1 (User)** — persisted into the chosen User flash
  slot (torn-write safe: a power cut mid-write leaves the slot's old
  content or empty, never a corrupt patch). Bank-Writes addressed to
  bank 0 (Factory) are silently ignored — the factory bank is ROM.

Practical notes:

- **From a PC, use the TRS path**: class-compliant USB-MIDI interface into
  the PC, TRS/MIDI cable into Tiliqua's TRS in, menu `MIDI Src` = `TRS`.
  (The USB-C port only accepts a MIDI *device* plugged directly into it.)
- **There is no MIDI output**, so ACK/handshake replies are never sent.
  Editors that wait for a per-patch ACK before sending the next dump will
  stall — use fire-and-forget sends (e.g. `amidi -s bank.syx`), ideally
  with a small inter-message delay.
- A stalled/interrupted SysEx stream times out after **500 ms** and the
  receiver resets cleanly; just resend.

## Troubleshooting

| Symptom | Likely cause / fix |
|---|---|
| No sound at all | Wrong MIDI channel (Lead/Drum listen on **ch 1**); or `MIDI Src` set to the other input; check Out 0/1 vs Out 2/3 cabling |
| Notes on ch 2+ do nothing | Expected for Lead/Drum patches — only Multi (ch 1–6) and Bassline (ch 1–2) listen beyond ch 1 |
| Tiliqua doesn't show up on my computer's MIDI device list | By design — the USB-C port is a **host** port. Use TRS via a USB-MIDI interface |
| Multi patch: repeated notes bounce between L and R in groups of 3 | Upstream engine behavior, not a bug: with `voice_asg` = "all voices" the note round-robins all 6 oscillators. Fix in the patch (set voice assignment left-/right-only) |
| Drum patch freezes ~4 s after loading with no sequencer running | Known upstream engine bug (see [limitations.md](limitations.md)); keep an external MIDI clock running, or reload before 4 s |
| SysEx dump from an editor stalls after the first patch | The editor is waiting for an ACK that never comes (no MIDI TX). Use scripted fire-and-forget sends |
| My edits vanished | Loading any other patch (menu or MIDI Program Change) discards unsaved edits — the `*` was the warning. Save to a User slot first |
| CV wiggling does nothing | Target set to Off; or Pitch assigned without Gate (both are needed for notes); or the change is below the 8-bit deadband |
| `Drive` row stuck on `No drive` in Storage mode | Not FAT32/MBR-first-partition, or the drive needs more init time than a cheap flash stick — try a different drive; block size must be 512 bytes |
| My MIDI keyboard stopped responding after I plugged in a drive | `USB Mode` is `Storage` — a plugged drive and a plugged MIDI device are mutually exclusive on the one USB-C port. Switch `USB Mode` back to `MIDI`, or play over TRS in the meantime (TRS stays live in Storage mode) |
| `Export FAILED` on the Usb card | Drive removed mid-write, filesystem full, or the drive rejected the SCSI write — nothing is left half-written; try again with the drive freshly re-plugged |
| `No/bad BANK.SYX` on Import | The file isn't at `/MBSID/BANK.SYX` (check the exact 8.3 name/folder), or it failed to parse (bad checksum, truncated, wrong bank) — nothing was written to the User bank |
| `Import FAILED` on the Usb card | The bank file validated but the flash write itself failed partway through; retry — Import always fully re-validates before touching flash, so a retry starts clean |
| Menu froze for a moment during Load, Export, or Import | Expected — USB block reads/writes run synchronously in the main loop with no live `BUSY` indicator; it un-freezes when the operation finishes. Don't unplug the drive while it's frozen |
