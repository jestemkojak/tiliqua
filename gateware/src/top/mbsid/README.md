# MBSID Lead

Run the **MIDIbox SID v3 Lead engine** on the Tiliqua, driven by MIDI. Two
cycle-accurate reSID cores (MOS 8580) produce true stereo output — the MBSID
engine distributes voices and timbres across both chips. Patches are the
standard zetaSID `.syx` format; a factory bank ships in the bitstream.

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
   browse the factory bank with the encoder.

---

## Audio outputs

Each reSID core mixes its three voices internally through the SID filter. The
two chip outputs map directly to the stereo jack pair, plus a summed mono mix
on the remaining outputs:

```
SID0 (sid_periph)          SID1 (sid_periph_r)
┌──────────────────┐       ┌──────────────────┐
│ last_audio_left  │       │ last_audio_left   │
│ last_audio_right │ [x]   │ last_audio_right  │ [x]
│ voice0_dca_o     │ [x]   │ voice0_dca_o      │ [x]
│ voice1_dca_o     │ [x]   │ voice1_dca_o      │ [x]
│ voice2_dca_o     │ [x]   │ voice2_dca_o      │ [x]
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

- The **MBSID v3 Lead C++ engine** (`mios32/apps/synthesizers/midibox_sid_v3/`)
  is cross-compiled freestanding for `riscv32im` by `fw/build.rs` using clang++.
  It runs entirely on the RISC-V; no gateware changes versus `top/sid`.
- **Control rate is 1 kHz** (`TIMER0_ISR_PERIOD_MS = 1`). On each tick the
  engine produces two 29-register SID images (L and R); only changed registers
  are enqueued to the respective `SIDPeripheral` FIFO (RegDiff).
- **zetaSID `.syx` patches** are standard MBSID v2 voice descriptions. The
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
