# SID MIDI Synthesizer

Play the MOS 6581/8580 SID chip as a 3-voice synthesizer from CV or MIDI on the
Tiliqua. Each voice is available as a separate Eurorack output; a fourth output
carries the mixed sum.

---

## Quick start

```sh
cd gateware
pdm sid build
pdm run flash archive build/sid-r5/<name>.tar.gz
```

---

## Jack layout

| Jack | Direction | Signal |
|------|-----------|--------|
| **in0–in3** | Input | CV / modulation sources |
| **out0** | Output | Voice 0 (solo) |
| **out1** | Output | Voice 1 (solo) |
| **out2** | Output | Voice 2 (solo) |
| **out3** | Output | Voices 0–2 (sum) |

---

## Controls

Everything is driven by the **rotary encoder** (rotate to move, press to select, press
again to confirm / leave modify mode). The menu has three pages:

### CV page

Assign each CV input jack to a modulation target — pitch or gate of individual
voices, or all voices at once.

### Polyphony page

| Setting | Effect |
|---------|--------|
| **Poly** | Each voice is independent; notes are allocated with round-robin voice stealing |
| **Unison** | All 3 voices play the same pitch with a configurable detune spread (cents) for a fatter sound |

### Misc page

| Setting | Effect |
|---------|--------|
| **Control source** | Switch between **CV** and **MIDI** input |

---

## MIDI

Connect a MIDI controller via **TRS MIDI** or **USB host** (select source on the
Misc page).

When MIDI is active, notes are distributed across the 3 voices with
round-robin voice stealing. CV inputs can still apply additive pitch offsets
on top of MIDI-set base pitches; gate is MIDI-only.

Supported MIDI controllers:

| Controller | Effect |
|------------|--------|
| **Pitch bend** | ±2 semitone range, applied to all active voices |
| **Mod wheel (CC1)** | Offsets filter cutoff upward from its menu-set value |

---

## How it works

```
(CV / MIDI) ──► VexiiRiscv SoC ──► SID register writes ──► reSID core ──► audio out
```

The RISC-V firmware reads CV or MIDI events and translates them into SID register
writes. Audio routing from the SID chip to the output jacks is pure gateware; the
softcore only drives the register interface.
