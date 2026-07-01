# MBSID

Run the **MIDIbox SID v3** sound engine on the Tiliqua, driven by MIDI. Two
cycle-accurate reSID cores (MOS 8580) produce true stereo output — the MBSID
engine distributes voices and timbres across both chips. All four MBSID engines
(**Lead, Bassline, Drum, Multi**) are present; the engine of a patch is selected
by the patch itself. Patches are the standard `.syx` format; a factory
bank ships in the bitstream.

---

## Quick start

1. **Fetch the engine** (GPL, not in the repo — one-time step after cloning):

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
> oracle; live hardware playback bring-up is still pending (see `DESIGN.md §7`,
> milestones 2–3).

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
- The vendored engine tree is **GPL** and gitignored; run `./fetch-mios32.sh`
  after cloning.

---

## Layout

| Path | Role |
|------|------|
| `top.py` | Top-level gateware (`MBSIDSoc` — bumps mainram, sets `n_sids=2`) |
| `fw/` | RISC-V firmware (engine FFI, MIDI drain, RegDiff, patch menu) |
| `fw/csrc/` | `mbsid_shim.cpp` + `mios32_shim/` facade headers |
| `host_oracle/` | x86 oracle: bit-exact diff of shim vs JUCE engine |
| `fetch-mios32.sh` | One-time vendor checkout (GPL, gitignored) |
| `DESIGN.md` | Authoritative spec (interfaces, milestones, acceptance) |
| `CLAUDE.md` | Developer reference and gotchas |
