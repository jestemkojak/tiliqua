# host_render — virtual-Tiliqua SID reference renderer

Render a SID register write-stream through **the same reDIP-SID RTL the FPGA
runs**, verilated on the host, into 48 kHz WAVs. Lets us diff Tiliqua jack
captures against a bit-faithful digital reference and attribute a difference to
either the model/timing or our player/delivery chain (see
`docs/sid_player_sw_host_render_spec.md`).

## Three commands, end to end

```sh
# 1. dump the firmware's exact SID write stream (Stage 1, host test).
#    DUMP_FRAMES is 50 Hz frames; 1500 ≈ 30 s. Default tune = docs/Commando.sid.
cd gateware/src/top/sid_player_sw/fw
DUMP_FRAMES=1500 DUMP_OUT=/tmp/sid_writes.sidw \
  cargo test --target x86_64-unknown-linux-gnu --lib dump_writes -- --ignored --nocapture

# 2. render a tap through the verilated RTL -> WAV (Stage 2 + 3).
cd ../tools/host_render
./render.sh -i /tmp/sid_writes.sidw -m 6581 -t v1   # voice-1 jack
./render.sh -i /tmp/sid_writes.sidw -m 6581 -t mix  # mix output
# -> gateware/build/host_render/sid_writes-host-6581-{v1,mix}.wav

# 3. compare against a Tiliqua capture (existing tools).
cd ..
../../../../.venv/bin/python wav_onsets.py REF.wav host_render_out.wav 0 30
../../../../.venv/bin/python wav_compare.py REF.wav host_render_out.wav
```

`render.sh [-i dump.sidw] [-m 6581|8580] [-t mix|v0|v1|v2] [-o out.wav]`
(defaults: `-i /tmp/sid_writes.sidw -m 6581 -t mix`, phi2 = 1 000 000 Hz,
sample-rate = 48 000 Hz).

## tap vs mix semantics

- **`-t v0|v1|v2`** point-samples the `voiceN_dca_o` RTL tap at 48 kHz with **no
  anti-alias filter** (sample-and-hold of the latest phi2-edge value) — exactly
  what the hardware voice jacks do (`pmod0.i_cal` point-samples at 48 kHz), so
  the characteristic aliasing is reproduced for an apples-to-apples jack
  comparison. The external C64 RC output filter is also skipped (jack taps are
  pre-codec digital values). Raw format: **s16le**. The WAV is **AC-coupled**
  (`raw2wav.py --dc-block`): the 6581 voice DCA carries a ~+0.38-FS `VOICE_DC`
  bias that the AC-coupled hardware codec strips, so removing it here keeps the
  tap comparable to a jack capture / websid export (without it, `abs()`/RMS
  per-note analysis is swamped by the offset — see the spec's V4 findings).

  **Voice numbering — read this before comparing to a capture.** The taps are
  **0-indexed** (`v0` = `voice0_dca_o`). Recordings named `*-1voice-*` or
  `*-voice1` are **1-indexed** ("voice 1" = SID voice **index 0**), so the
  captured/soloed voice is **`-t v0`**, not `-t v1`. Picking the wrong voice
  yields a ~0.2 envelope correlation that looks like a fidelity bug; the right
  voice gives ~0.98.
- **`-t mix`** keeps upstream's `audio_o` path (24-bit, external RC filter on by
  default). Raw format: **s24be**. NB: this is *not yet* a port of the hardware
  `AudioDecimator` polyphase FIR (~19 kHz) — it is upstream's resampler. Treat
  the mix WAV as a first-pass reference (see spec Stage 2 item 2).

## SID model is compile-time

The reDIP-SID model (6581 vs 8580) is selected by a Verilog ``` `define SID2 ```
(8580), **not** a runtime flag. `render.sh -m` therefore builds and caches a
**separate verilated binary per model** under
`gateware/build/host_render/sim_<model>/`. The upstream `--sid-model` runtime
flag only masks filter-register bits and is unused here.

## Running the sim binary by hand

When you invoke the verilated binary (`Vsid_api`) directly without `render.sh`,
the upstream default sample rate is **96 000 Hz** (see `--help`). `render.sh`
always passes `--sample-rate 48000` explicitly, so the WAVs it produces are
always 48 kHz regardless of that default.

## Patch note (deps/ is never modified)

`harness.patch` is a `git apply -p1` diff against
`gateware/deps/sid/gateware/sid_api_sim.cpp` adding `--phi2-hz` and
`--tap {mix|v0|v1|v2}`. `deps/` is vendored upstream and is **never** edited in
place: `render.sh` copies `sid_api_sim.cpp` into the build dir and applies the
patch there. The build is idempotent/cached — delete
`gateware/build/host_render/` to force a rebuild.

## raw2wav.py

`raw2wav.py IN.raw OUT.wav --format {s24be|s16le} [--rate 48000]` — numpy-only
raw→16-bit-mono-WAV converter (run with `gateware/.venv/bin/python`). `render.sh`
calls it automatically with the right format for the chosen tap.
