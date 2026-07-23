# MBSID

Run the **MIDIbox SID v3** sound engine on the Tiliqua, driven by MIDI. Two
cycle-accurate reSID cores (MOS 8580) produce true stereo output — the MBSID
engine distributes voices and timbres across both chips. All four MBSID engines
(**Lead, Bassline, Drum, Multi**) are present; the engine of a patch is selected
by the patch itself. Patches are the standard `.syx` format; a factory
bank ships in the bitstream.

---

## Quick start

1. **Fetch the engine** (personal non-commercial use only; not in the repo — one-time step
   after cloning):

   ```sh
   cd gateware/src/top/mbsid
   ./fetch-mios32.sh
   ```

2. **Build & flash:**

   ```sh
   cd gateware
   pdm mbsid build
   pdm run flash archive build/mbsid-r5/<name>.tar.gz
   ```

3. **Connect MIDI** (TRS jack or USB host) and play. The patch menu lets you
   browse the factory bank with the encoder. See the channel map below for which
   MIDI channels each engine listens on.

   > **The USB-C MIDI port is a *host* port, not a device port.** Tiliqua powers
   > and enumerates devices plugged *into it* (keyboards, controllers). It will
   > never show up in a PC's `lsusb`/`amidi -l`, and a plain PC↔Tiliqua USB cable
   > carries no MIDI in either direction (two hosts can't enumerate each other) —
   > this is by design. To send MIDI/SysEx from a PC, use **TRS**: a
   > class-compliant USB-MIDI interface on the PC with a TRS/MIDI cable into
   > Tiliqua's MIDI-in jack. (If your PC does detect *something* when cabled to
   > Tiliqua, that's the separate debug-USB serial console, not MIDI.) See
   > `docs/limitations.md` for details.

   > **The same USB-C port can host a USB drive instead, for patch storage.**
   > Flip `USB Mode` on the Main menu card from `MIDI` to `Storage` and plug in
   > a thumb drive to browse/load `.syx` patches, export a patch to a file, or
   > import a whole 128-slot User bank — a MIDI device and a drive are mutually
   > exclusive on this one port, and TRS keeps working as the live MIDI source
   > the whole time you're in Storage mode. See
   > [`docs/user-guide.md`](docs/user-guide.md#usb-mode-loading-patches-from-a-drive)
   > for the full walkthrough.

---

## Sound engines & MIDI channels

MBSID is a **multi-timbral MIDI module**, not a single-channel synth. Each engine
maps its internal MIDI "voices" onto the two SIDs and onto fixed MIDI channels.
The mapping is the engine's, not the patch's — every patch of a given engine uses
the same channel layout:

| Engine | MIDI channels | Voices → SID | Plays from one keyboard? |
|--------|:---:|---|---|
| **Lead** | 1 | engine allocates up to all 6 oscillators (unison / detune / wavetable chord) | ✅ fully |
| **Drum** | 1 | MIDI note → drum instrument, like a GM drum part | ✅ fully |
| **Bassline** | 1, 2 | two independent basslines (one per channel), each split at note 60 | ⚠️ ch 1 = bassline 1 only |
| **Multi** | 1–6 | 6 independent mono parts: ch 1–3 → **Left** SID osc 1–3, ch 4–6 → **Right** SID osc 1–3 | ⚠️ one part per channel |

**How to play:**

- **Lead / Drum** — a single MIDI keyboard or pad on **channel 1** plays the whole
  engine. This is the live-play case.
- **Multi / Bassline** — these want a **multi-channel source** (a DAW, hardware
  sequencer, or groovebox) sending on channels 1–6, exactly like driving any
  multi-timbral GM module. A keyboard that can split its keys into per-channel
  zones can play several parts by hand; a plain single-channel keyboard plays only
  the one part assigned to its channel.

> **Status:** the firmware forwards the real MIDI channel, so **all four engines are
> reachable** per the channel map above (Lead/Drum on ch 1, Bassline split across
> ch 1–2, Multi across ch 1–6). All four are validated bit-exact against the host
> oracle, and live hardware playback is confirmed.

---

## Audio outputs

Each reSID core mixes its three voices internally through the SID filter. The
two chip outputs map directly to the stereo jack pair, plus a summed mono mix
on the remaining outputs:

```
SID0 (sid_periph)          SID1 (sid_periph_r)
┌──────────────────┐       ┌──────────────────┐
│ last_audio_left  │       │ last_audio_left  │
│ last_audio_right │ [x]   │ last_audio_right │ [x]
│ voice0_dca_o     │ [x]   │ voice0_dca_o     │ [x]
│ voice1_dca_o     │ [x]   │ voice1_dca_o     │ [x]
│ voice2_dca_o     │ [x]   │ voice2_dca_o     │ [x]
└────────┬─────────┘       └─────────┬────────┘
         │                           │
         │  (3-voice post-filter)    │  (3-voice post-filter)
         │                           │
         ├───────────────────────────┼──────────► out0  (L out)
         │                           ├──────────► out1  (R out)
         └─────────── (+)>>1 ────────┼──────────► out2  (L+R mix)
                                     └────────── same ──────────► out3  (L+R mix)
```

`[x]` = unused. `last_audio_right` on each chip is the SID's internal stereo
right channel — the MBSID Lead engine runs each chip in mono so it is silent.
The `(+)>>1` average on out2/out3 prevents overflow when both channels are at
full scale.

---

## How it works

USB-C is one host port shared, one-at-a-time, between MIDI and storage
(`USB Mode` on the Main menu card selects which). The diagram below is the
`USB Mode=MIDI` path; in `USB Mode=Storage` the port instead feeds a
separate `usb_msc` CSR for `.syx` patch import/export via the Usb menu
card (not shown here — see `docs/architecture.md`).

```
 MIDI in (TRS / USB host)
          │
          ▼
 ┌─────────────────────────────────────────┐
 │           RISC-V SoC (VexiiRiscv)        │
 │                                         │
 │  midi_read CSR FIFO ──► mbsid_note_on/  │
 │                          note_off / CC  │
 │                               │         │
 │   ┌───────────────────────────▼───────┐  │
 │   │  TIMER0 ISR  (fires at 1 kHz)     │  │
 │   │                                   │  │
 │   │   mbsid_tick()                    │  │
 │   │     ├─► sid_regs_t L image        │  │
 │   │     │   RegDiff ──► SIDPeripheral │  │
 │   │     └─► sid_regs_t R image        │  │
 │   │         RegDiff ──► SIDPeripheral_R│ │
 │   └───────────────────────────────────┘  │
 └─────────────────┬───────────┬────────────┘
                   │           │
          (reg writes)  (reg writes)
                   ▼           ▼
  ┌──────────────────┐  ┌──────────────────┐
  │  SIDPeripheral   │  │  SIDPeripheral_R  │
  │  depth-16 FIFO   │  │  depth-16 FIFO   │
  └────────┬─────────┘  └────────┬─────────┘
           ▼                     ▼
  ┌──────────────────┐  ┌──────────────────┐
  │  reSID0 (8580)   │  │  reSID1 (8580)   │
  │  phi2 ~1 MHz     │  │  phi2 ~1 MHz     │
  └────────┬─────────┘  └────────┬─────────┘
           │ 3-voice mix          │ 3-voice mix
           ▼                     ▼
       out0 (L)              out1 (R)
           └──────── avg ────────┘
                      │
                  out2/3 (L+R mix)
```

**Key points:**

- The **MBSID v3 C++ engine** (`mios32/apps/synthesizers/midibox_sid_v3/`, all
  four engines) is cross-compiled freestanding for `riscv32im` by `fw/build.rs`
  using clang++. It runs entirely on the RISC-V; no gateware changes versus
  `top/sid`. The active engine is dispatched from `patch.body.engine`.
- **Control rate is 1 kHz** (`TIMER0_ISR_PERIOD_MS = 1`). On each tick the
  engine produces two 29-register SID images (L and R); only changed registers
  are enqueued to the respective `SIDPeripheral` FIFO (RegDiff).
- **`.syx` patches** are standard MBSID v2 voice descriptions. The
  engine translates them into SID register writes — there is no SID register
  data in the patch file itself.
- **Static constructors** don't auto-run on riscv-rt; `mbsid_run_static_ctors()`
  (called from `mbsid_init()`) walks `.init_array` explicitly — do not remove it
  or the engine boots uninitialised.
- The vendored engine tree is licensed "for personal non-commercial use only; all other
  rights reserved" and gitignored; run `./fetch-mios32.sh` after cloning.

---

## Menu cards

The on-screen menu (encoder-driven) is three cards, selected from a Card row
at the top of every card, plus a fourth (**Usb**) that joins the Card row
only while `USB Mode` is set to `Storage`:

| Card | Purpose |
|------|---------|
| **Main** | patch bank/program browse + load, MIDI Src (TRS/USB), USB Mode (MIDI/Storage), Save (as user patch) |
| **CV Mod** | assign each of the 4 CV inputs to a modulation target |
| **Edit** | live-edit the loaded Lead patch's parameters, then Save |
| **Usb** *(Storage mode only)* | browse a plugged drive's `.syx` files, load one to a User slot, export the current/a slot's patch, or import a whole 128-slot bank |

Turning the encoder on the Card row cycles Main → CV Mod → Edit → Main.

### CV modulation (CV Mod card)

CV Mod maps directly onto Tiliqua's 4 audio/CV input jacks (`CV1`–`CV4` on the
gateware I/O legend, `pmod` inputs 0–3). Each input independently selects a
target from:

- **Off** — input ignored.
- **Knob1–Knob5** — routed into the engine's patch knob matrix
  (`mbsid_knob_set`), the same mechanism a MIDI CC would drive.
- **Volume / Phase / Detune / Cutoff / Reso** — routed into the parSet common
  block (`mbsid_par_set`, addresses `0x01`–`0x05`).
- **Pitch / Gate** — form a CV note machine on MIDI channel 1: `0 V = C2`,
  1 V/octave tracking with semitone quantization (integer, hysteresis-based —
  no float, no lookup table); Gate opens above 2 V and closes below 1 V.
  Assigning Gate alone (no Pitch) plays a fixed note (C-4) on every gate-on.
  Assigning **Pitch with no Gate target is inert** — Pitch alone never
  produces a note; you need both.

Continuous targets (Knob/Volume/Phase/Detune/Cutoff/Reso) are deadbanded on
the 8-bit scaled value, so small CV noise doesn't spam the engine. Retargeting
an input, or clearing a held Gate's target, releases any note it's currently
holding — no stuck notes.

CV Mod assignments persist across power cycles alongside the MIDI Src
setting (`fw/src/settings_store.rs`), saved ~2 s after the last change.

### Patch editing (Edit card)

The Edit card lists the current Lead patch's editable parameters and lets you
change them with the encoder; each change writes directly into the loaded
patch's in-memory body (so the value both sounds immediately and is what gets
captured by Save) and is marked with a `*` after the patch name in the title
bar as long as any edit is unsaved.

**Volatility contract:** there is no confirmation prompt. Loading a different
patch — by browsing to a new bank/program on the Main card, or by an incoming
MIDI Program Change — discards any unsaved edits immediately. The `*` in the
title bar is the only warning; if you want to keep an edit, Save it (to a user
slot) before switching patches.

**Lead-only:** the Edit card only shows parameter rows when the currently
loaded patch is a **Lead** engine patch. Bassline/Drum/Multi patches show a
"Lead patches only" placeholder instead — those engines' parameters aren't
exposed by this card.

**CV vs. Edit precedence:** if a CV Mod target and an Edit-card parameter
happen to affect the same underlying value (e.g. CV routed to Cutoff while
also editing Cutoff on the Edit card), the CV Mod ISR tick (1 kHz) wins for
what you currently hear *while the CV input is actively changing* — CV
targets are deadbanded on their 8-bit value, so once the CV settles an
Edit-card write to the same target takes effect live until the CV next moves
past its resolution step. The Edit card's write still lands in the patch
body, though, so it's the Edit card's value — not whatever CV was doing —
that gets saved.

---

## Layout

| Path | Role |
|------|------|
| `top.py` | Top-level gateware (`MBSIDSoc` — bumps mainram, sets `n_sids=2`) |
| `fw/` | RISC-V firmware (engine FFI, MIDI drain, RegDiff, patch menu) |
| `fw/csrc/` | `mbsid_shim.cpp` + `mios32_shim/` facade headers |
| `host_oracle/` | x86 oracle: bit-exact diff of shim vs JUCE engine |
| `tools/make_patch_bank/` | PC-side CLI: build a `BANK.SYX` from a directory of single-patch `.syx` dumps for [whole-bank USB import](docs/user-guide.md#importing-a-whole-bank-from-a-drive) |
| `fetch-mios32.sh` | One-time vendor checkout (personal non-commercial use only; gitignored) |
| `DESIGN.md` | Authoritative spec (interfaces, milestones, acceptance) |
| `CLAUDE.md` | Developer reference and gotchas |
