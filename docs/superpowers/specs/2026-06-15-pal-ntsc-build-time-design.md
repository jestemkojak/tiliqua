# PAL/NTSC as a Build-Time Switch — Design

**Date:** 2026-06-15
**Status:** Approved (design); implementation plan to follow.
**Findings basis:** `2026-06-15-sid-recent-changes-cost-benefit-findings.md`.

## Problem

`sid_player_sw` supports PAL **and** NTSC at runtime: `top.py:301-304` instantiates **two** full
`AudioDecimator` instances (`audio_decim_pal` at 985.5 kHz, `audio_decim_ntsc` at 1.023 MHz), both
fed the same 1 MHz stream, with the `phi2_sel` CSR muxing which output reaches the codec
(`top.py:311-314`). The `Phi2Divider` likewise runs a runtime-selected rate, and the firmware
auto-selects the standard from the PSID header (`set_phi2`, a Clock menu row).

Each `AudioDecimator` contains the same single-MAC `FIR` (`MULT18X18D` + tap BRAM) — the ~17.55 ns
structure that is now the sync critical path. For a **PAL-only library** the NTSC instance is dead
weight: it occupies a DSP/BRAM/LUTs and adds routing congestion to compute a discarded result. This
runtime machinery was added for **pitch accuracy** (flat 1.000 MHz was +1.5 % sharp), not for any
playback bug, and it worsened sync Fmax 56.17 → 53.47 MHz.

## Goal / Acceptance

- A **build-time** `--clock {pal,ntsc}` flag (default `pal`) bakes the standard end-to-end.
- Exactly **one** `AudioDecimator` is instantiated; `audio_decim_ntsc` and the `phi2_sel` output mux
  are gone (confirm in `top.il` / `top.tim`: only one `resample.filt`).
- phi2 runs the selected single rate; pitch stays correct for the built standard; the restored
  reSID filter is unaffected.
- The **shared** `SIDPeripheral` and the `sid` MIDI-synth top still elaborate and build unchanged.
- `sid_player_sw` still builds; measure and record the sync Fmax / congestion delta.
- **Non-goal:** closing sync at 60 MHz (the remaining single PAL FIR is still ~17.55 ns — that needs
  `2026-06-15-fir-mac-pipeline-design.md`). This change reclaims area/congestion only.

## Approach

Mirror the existing `--sid-model` build flag (`top.py:391-398`, threaded
`top_level_cli → argparse_callback/argparse_fragment → __init__`).

### Build flag

`top_level_cli` accepts a single `argparse_callback`/`argparse_fragment`. Extend the existing
lambdas to add **both** arguments / keys:

```python
argparse_callback=lambda p: (
    p.add_argument("--sid-model", choices=["6581","8580"], default="8580", help=...),
    p.add_argument("--clock", choices=["pal","ntsc"], default="pal",
                   help="C64 clock standard to synthesize (default pal)."),
),
argparse_fragment=lambda args: {"sid_model": args.sid_model, "clock": args.clock},
```

`SIDPlayerSwSoc.__init__` pops `clock` (default `"pal"`) alongside `sid_model`.

### Single decimator + single phi2 rate

In `__init__`/`elaborate`:

```python
PHI2_HZ = {"pal": PHI2_HZ_PAL, "ntsc": PHI2_HZ_NTSC}
rate = PHI2_HZ[self.clock]
# one decimator
m.submodules.audio_decim = decim = AudioDecimator(fs_in=rate, fs_out=fs_out)
m.d.comb += [decim.i.valid.eq(self.sid_periph.audio_strobe),
             decim.i.payload.as_value().eq(self.sid_periph.last_audio_left >> 8)]
m.d.comb += audio_out.eq(decim.o)        # no phi2_sel mux
```

Construct `SIDPeripheral(..., phi2_hz=(rate, rate))` so the `Phi2Divider` produces the single
selected rate regardless of `phi2_sel`.

### Shared-component decision (minimal blast radius)

**Keep `SIDPeripheral`'s `phi2_sel` CSR / `FFSynchronizer` / divider `sel` as-is.** Passing
`phi2_hz=(rate, rate)` makes the runtime select a no-op (both branches equal), so no behavioural
change and **zero edits to the shared component** — the `sid` MIDI-synth top (which constructs
`SIDPeripheral()` with defaults) is untouched. The leftover 1-bit `sel`/FFSynchronizer is negligible
and is *not* the congestion source (the decimator was). Fully stripping that 1-bit path is **out of
scope** (not worth touching the shared component for ~1 LUT).

### Firmware

- `set_phi2` / the header auto-select / the **Clock menu row** become meaningless (the standard is
  baked). Remove the Clock row from the menu and the auto-select call; the firmware no longer writes
  `phi2_sel`.
- Show the baked standard in the title line (mirror the `build_model` CSR display): e.g.
  `SID PLAYER (8580 · PAL)`. A new read-only `build_clock` CSR (1 = NTSC, 0 = PAL) is the clean way
  to surface it; alternatively bake a compile-time constant into the firmware build. **Decision:**
  add a `build_clock` CSR on `SIDPlayerSwSoc` (small, matches the `build_model` precedent) so a
  single firmware binary correctly labels whatever bitstream it runs on.

## Components / Files

- `gateware/src/top/sid_player_sw/top.py` — `--clock` flag plumbing; `clock` param; one
  `AudioDecimator`; drop the mux; `phi2_hz=(rate,rate)`; add `build_clock` CSR.
- `gateware/src/top/sid_player_sw/fw/src/main.rs` — drop the Clock menu row + `set_phi2`
  auto-select; read `build_clock` for the title.
- `gateware/src/top/sid_player_sw/CLAUDE.md` — update the "Play rate / phi2" + menu sections to
  document the build-time switch (and that NTSC needs its own build, like `--sid-model`).
- (No change to `gateware/src/top/sid/top.py` `SIDPeripheral` / `SIDSoc`.)

## Testing & Verification

1. **Firmware host tests:** `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib` — still pass
   (menu/title changes don't touch host-tested modules).
2. **Build (default PAL):** `cd gateware && pdm sid_player_sw build`. Confirm in
   `build/sid-player-sw-r5/top.il` there is exactly **one** `resample.filt`, and in `top.tim` that
   `audio_decim_ntsc` is gone. Record the sync Fmax delta vs 56.99 MHz (expect a modest improvement
   from reduced congestion; the PAL FIR path itself stays ~17.55 ns).
3. **Build (NTSC) smoke:** `pdm sid_player_sw build --clock ntsc` elaborates/builds (the other
   standard still works as a separate bitstream).
4. **Regen PAC** after the `build_clock` CSR add: `pdm sid_player_sw build --pac-only`.
5. **HW listen:** a PAL tune (Commando) plays at correct pitch with the restored filter.

## Risks

- **NTSC tunes need a separate build** (acceptable — already the norm for `--sid-model`).
- The `sid` MIDI-synth top must still elaborate (guaranteed: `SIDPeripheral` unchanged, defaults
  intact). Verify with a quick `pdm sid build` if convenient.
- New `build_clock` CSR requires a PAC regen; firmware that reads it must be rebuilt (normal flow).
- Does **not** close 60 MHz on its own (see non-goal); complementary to the FIR-pipelining spec.

## Out of Scope

- Stripping the residual 1-bit `phi2_sel` from the shared `SIDPeripheral`.
- The FIR critical-path fix (separate spec).
- Any runtime PAL/NTSC capability (explicitly traded away for area/congestion).
