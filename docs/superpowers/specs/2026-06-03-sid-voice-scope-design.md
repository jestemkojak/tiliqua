# SID Voice Scope — Design

**Date:** 2026-06-03
**Status:** Approved (pending spec review)
**Target:** `gateware/src/top/sid_player`

## Goal

Add an on-screen oscilloscope to the SID player that shows each SID voice's
waveform independently, rendered directly to the 640×480 framebuffer, while the
existing playback controls (pause, subtune select) and metadata display remain
unchanged.

## Decisions (from brainstorming)

- **Style:** stacked per-voice oscilloscope — one horizontal trace per voice
  (V1, V2, V3) plus the mixed output (MIX), four traces total.
- **Layout:** compact metadata header strip at top; scope traces always visible
  below. Scope is always on (not a separate toggled page).
- **Controls:** unchanged — encoder button = pause, rotation = subtune. No
  scope-tuning menu.
- **Persistence:** crisp look, implemented as the `Persistance` peripheral with a
  fast decay rate (the peripheral is required to clear additively-plotted traces;
  decay rate selects crisp-vs-glow).
- **Channels:** keep MIX as the 4th trace; drop it only if FPGA resources force it.

## Architecture

Reuse the existing `tiliqua.raster` scope stack (proven in `src/top/macro_osc`),
rendered entirely in gateware. Firmware configures it once at boot and is
otherwise unchanged except for compacting the metadata draw into a header strip.

No scope drawing logic is added to the firmware main loop.

## Data flow

```
SID core ──► voice0_dca / voice1_dca / voice2_dca / mixed audio
   (already wired to pmod0.i_cal.payload[0..3] → codec out, unchanged)
        │  tap into a non-blocking side FIFO (MUST NOT stall SID audio)
        ▼
   per-channel upsample (interpolate so traces look continuous)
        ▼
   ScopePeripheral(n_channels=4)  ──► 4 PlotRequest output streams
        ▼
   FramebufferPlotter(n_ports=4)  ──► writes trace pixels into the framebuffer
                                       (own PSRAM master)
        ▲
   Persistance peripheral (own PSRAM master) ──► fast decay clears old traces
```

The four scope channels are exactly **V1, V2, V3, MIX**. They already exist on
`pmod0.i_cal.payload[0..3]`, so the scope merely *observes* the same stream that
drives the codec. Audio output behaviour is unchanged.

## Components

### Gateware — `src/top/sid_player/top.py`

Mirror the `macro_osc` wiring, simplified (no vectorscope):

- `FramebufferPlotter(bus_signature=..., n_ports=4)` — added as a PSRAM master;
  its `fbp` connected to the framebuffer properties; one port per scope channel.
- `scope.ScopePeripheral(n_channels=4, fs=audio_fs * n_upsample)` — added to the
  CSR decoder at a new base address; its `source` set to `pmod0.i_cal`; its four
  `o[n]` outputs connected to `plotter.i[n]`.
- `raster.persist.Peripheral` (`Persistance`) — added as a PSRAM master and CSR
  peripheral; `fbp` connected to the framebuffer properties.
- A non-blocking plot FIFO tapping `pmod0.i_cal.payload[0..3]`, feeding
  per-channel `dsp.Resample` upsamplers, merged into the scope's input stream.
  Copy the non-blocking pattern from `macro_osc` so plotting never stalls the SID
  audio output.
- `VectorPeripheral` is intentionally **not** instantiated.

New CSR base addresses are assigned alongside the existing `sid_periph` (0x1000),
`play_timer` (0x1100), `usb_msc` (0x1200); finalize the CSR bridge after adding
them. PAC regenerates automatically from the SoC layout.

### Firmware — `src/top/sid_player/fw`

- Add the generated HAL bindings for the new peripherals, mirroring `macro_osc`
  (`impl_scope!` for the scope, `Persist0` for persistence, plotter backend).
- At boot, after display init:
  - enable the scope (`soc_en`);
  - write fixed per-channel config: hue (distinct per voice), `YPosition` (stack
    the four traces), `XScale`/`YScale`, `Timebase`, intensity, trigger level;
  - set `Persist` decay to a fast value (crisp).
- Main loop unchanged except the metadata is drawn as a compact header strip
  (one to two text lines at the top) instead of a full-screen info block, still
  redrawn only on state change.

### Layout

- **Header strip (top):** `SID PLAYER` · tune name · author · `Song x/y [STATE]`,
  in the existing fonts, redrawn on change.
- **Scope band (below header):** four stacked traces at fixed `YPosition`s, one
  hue per voice. Header height and `Persist` decay chosen so header text stays
  legible despite the decay pass; final values tuned on hardware.

## Risks / open items

1. **25F LUT headroom (primary risk).** The SID player already contains the 6502
   + bridge + USB MSC. Adding plotter + 4-channel scope + 4 upsamplers is
   non-trivial. Baseline utilisation first. Mitigations in order: reduce upsample
   factor; drop MIX (3 scopes); restrict the scope build to the 45F.
2. **Header/scope overlap.** Choose decay rate and header height so text remains
   legible; tune on hardware.
3. **Audio integrity.** The tap FIFO must be non-blocking; plotting must never
   stall the SID audio stream. Copy `macro_osc`'s FIFO pattern exactly.

## Out of scope (YAGNI)

Vectorscope mode, runtime scope-parameter menu, runtime per-voice color
customization, spectrogram.

## Reference

- Existing scope stack: `gateware/src/tiliqua/raster/{scope,persist,stroke,plot}.py`
- Reference integration: `gateware/src/top/macro_osc/top.py` and
  `src/top/macro_osc/fw/src/{main,lib}.rs`
- SID voice taps: `gateware/src/top/sid/top.py` (`voice0_dca`/`voice1_dca`/
  `voice2_dca`), routed in `src/top/sid_player/top.py` lines 547–554.
