# MBSID-on-Tiliqua — User Guide

This guide is for musicians using the `mbsid` bitstream on a Tiliqua. No
knowledge of the codebase is needed. For flashing/building the bitstream in
the first place, see the [Developer Guide](developer-guide.md) or ask
whoever builds your firmware.

## What it is

A stereo **dual-SID synthesizer module**: the classic MIDIbox SID v2/v3
sound engine (the same engine family behind zetaSID) running against two
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

`MIDI Src` and the CV assignments persist across power cycles (written to
flash a couple of seconds after you change them).

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
