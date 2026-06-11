# SID Player SW

Software 6502 (`mos6502` crate, NMOS variant) runs the PSID tune on the VexiiRiscv;
memory is the 64 KB image in PSRAM at `0x20800000` via the RISC-V D-cache
(`fw/src/player.rs` `PsidBus`); `$D400-$D41F` writes are redirected to the
`SIDPeripheral` CSR (`(val<<5)|reg` → `transaction_data` register); `play()` is
driven by the **TIMER0 interrupt** at the tune's rate (NOT the UI loop). No
gateware 6502/bridge/play-timer. The bus/driver logic is host-tested
(`cargo test --target x86_64-unknown-linux-gnu --lib`).

**Limitation:** `mos6502` has no illegal/undocumented opcodes (none of the test
tunes use them); revisit if a future tune requires them.

## Architecture

- `fw/src/player.rs` — `PsidBus` (`Bus` impl), `call()` (run-until-RTS sentinel),
  `init()`, `sid_txn()`. Host-testable.
- `fw/src/main.rs` — CPU constructed over `&'static mut [u8; 0x10000]` at
  `0x20800000`. The CPU lives in a `static Mutex<RefCell<Option<Playback>>>`
  shared with the **TIMER0 ISR** (`play_tick` runs one PLAY frame); the SID hook
  is a plain `fn(u8,u8)` (steals SID_PERIPH) — NOT a closure — so the CPU type is
  nameable for the static. UI loop is best-effort (encoder + menu); tune/subtune
  changes go through `reload_tune()` (re-inits CPU under a critical section) and
  reset the TIMER0 reload. This is the `macro_osc` ISR pattern (`irq::scope` +
  `handler!` + `critical_section`).

## Audio output / anti-aliasing
- The reSID core emits one sample per phi2 cycle (~1MHz; phi2 = sync/60 in
  `src/top/sid/top.py`). The codec runs at 48kHz. **Do NOT** point-sample the 1MHz
  mix straight into `pmod0.i_cal.payload` — that's a zero-order-hold ~21x downsample
  with no anti-alias filter, so all SID content >24kHz folds into the audible band as
  broadband "grit" (the audible difference vs a software-reSID reference, which
  resamples with a FIR). Confirmed by WAV analysis: spectral flatness 0.43 vs reSID
  0.34, elevated >10kHz energy, stair-stepped waveform.
- Fix: `top/sid/audio.py` `AudioDecimator` = polyphase FIR (`dsp.Resample`,
  n_up/m_down from fs_out/fs_in → 6/125, ~19kHz cutoff) + small input FIFO (absorbs the
  single-MAC FIR's per-output backpressure burst). Fed by `SIDPeripheral.audio_strobe`
  (1MHz, pulses the cycle after each `last_audio_*` latch so it sees the fresh sample).
  Costs ~570 LUTs + 1 multiplier; runs at phi2 cadence so it does NOT join the critical
  path (sync Fmax unchanged ~55MHz). Host-tested in `tests/test_sid_audio.py` (1kHz
  passes, 100kHz alias-tone rejected).
## Voice scope signal path (audio ALWAYS wins over visuals)
- The 3 voice taps (`sid.voiceN_dca`) are ~1MHz reSID outputs (ASQ Q1.15) with a
  model-dependent DC bias (6581 `VOICE_DC` = ½ dynamic range; 8580 = 0). Point-sampling
  them at 48kHz aliases (dot scatter); the bias offsets/wraps the trace once scaled right.
- Scope-branch conditioning (`smooth.py`, scope branch only — audio out untouched):
  `VoiceSmoother` (anti-alias leaky-LP + DC-block AC-couple) → `>>2` ASQ→PSQ →
  `LinearUpsampler` (fills vertical gaps; scale `scope_periph.fs` ×`scope_n_upsample`) →
  `StreamThrottle` → scope. The mix channel is already band-limited via `AudioDecimator`.
- The scope plotter is a PSRAM master sharing the round-robin bus with the 6502's tune
  fetches (each pixel = a read-modify-write). Heavy scope work starves playback → music
  lags. **Never sacrifice audio/SID timing for visuals**: throttle/deprioritise the scope
  (`scope_throttle`, `scope_n_upsample`); the plot FIFO is intentionally non-blocking (drops points).
- Pause masks held notes via codec mute (`output_mute`, pmod `flags.mute`), not by touching
  the SID — so playback resumes cleanly. Unsupported/corrupt `.SID` files (e.g. multi-SID v4)
  must skip gracefully (`load_psid_to_mem`/`reload_tune` return `Result`/`Option`, show
  `UNSUPPORTED!`) — never `.expect()`-panic the player on a bad header.
- Every tune (re)load runs `sid_reset()` ($00 to all 25 regs, `writable`-backpressured)
  between a successful image load and INIT: PSID tunes assume a freshly **reset** chip
  (Commando's INIT writes only the 3 gates + volume; stale waveform/freq/SR from the
  previous tune otherwise plays a loud noise burst at tune start). Reset only after a
  known-good load — an unsupported file must leave the still-playing tune untouched.

## Dual SID chip model (6581 vs 8580)
- **Build-time selection:** `pdm sid_player_sw build --sid-model {6581,8580}` (default 8580).
  The flag threads `sid2_define` (True for 8580, False for 6581) through `top_level_cli` → `top.py`'s
  `argparse_callback`/`argparse_fragment` → the shared `SID` component (`src/top/sid/top.py`)
  and the `SIDPeripheral` CSR. Different filter curves + combined-waveform tables between models.
- **Runtime visibility:** firmware reads the baked-in `build_model` CSR (1=8580, 0=6581) at boot
  and shows it in the title line: `SID PLAYER (6581)` or `SID PLAYER (8580)`. The **metadata line**
  (row 2) shows the tune's declared model from the PSID header (bit4-5 of `flags` offset $76).
  A mismatch is visible at a glance — flash the build that matches the tune for correct timbre.
- Most tunes (e.g. Commando) declare 6581; pre-8580 builds played them with wrong filter character.
  Now: select the model once and flash it matching your library.

## Menu / UI (`main.rs`)
- Hand-rolled menu (NOT the `opts` derive framework — macro_osc's `opts`/`tiliqua_lib::ui`
  doesn't fit the dynamic USB File browser). Card/page model: `enum Page`, row 0 of every
  card is the "Page" selector; `rows_in(page)` gives the row count. Cards: Player
  (File/Song/State) and Scope (Decay/Timebase/Y-Scale/Intensity/Hue).
- Navigation: rotate moves `selected`; press toggles `modify`; modify+rotate edits the value.
- Scope params are set live from the UI loop via `Scope0`/`Persist0` HAL (`set_timebase`,
  `set_yscale`, `set_intensity`, `set_hue`, `persist.set_persistence`) — independent CSRs, so
  NO critical section vs the SID ISR is needed. `tiliqua_lib::scope::{Timebase,VScale}` are
  `Copy`+`IntoStaticStr` (`.into()` → "10ms/d"/"2V/d"); step them via the `TIMEBASES`/`VSCALES`
  const arrays. Scope settings are not persisted across reboots.
- `HEADER_H` bounds the menu region; rows past it overlap the waveform — grow it if you add rows.

## Play rate (VBlank / CIA multispeed)
- Rate computed by `psid::play_period_cycles` from `hdr.clock()` / `hdr.is_cia(subtune)`.
  PSID `speed` (offset $12) is VBI(0) vs CIA(1) — **not** PAL/NTSC.
- CIA timer value is read from `cpu.memory.mem[0xDC04/0xDC05]` directly after INIT
  (the RISC-V is the only PSRAM master of the image, so no cache-thrash needed).

## Gotchas (firmware)
- **VexiiRiscv has no `mcycle`/perf-counter CSR** (`vexiiriscv.py`: no perf-counter
  plugin). Reading `mcycle`/`cycle` traps → freezes the whole SoC. Use the gateware
  `Timer0` for any firmware timing (counter is a down-counter; ISR via `enable_tick_isr`).
- **Real-time work must run in the TIMER0 ISR, not the UI loop.** The `persist`
  peripheral decays the framebuffer, so the menu must be repainted every frame —
  too slow to also host `play()`. Coupling them throttles/jitters playback.
- **`mos6502` emits a `debug!` per emulated instruction** → at Trace level it floods
  UART and (blocking on UART) throttles playback. `log::set_max_level_racy(Info)` early.
- **`mos6502` panics on an unimplemented opcode** (`cpu.rs:1159`); a *decode* miss
  instead spins without advancing PC (burns `max_steps`). Repro tunes on the host:
  `include_bytes!` the SID, run `init`/`call` under `mos6502`.

## Firmware host tests (`fw/`)
- Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib` (default
  target is `riscv32im`, so the host triple must be explicit).
- Host-testable modules listed in `fw/src/lib.rs` without `#[cfg(not(test))]`:
  `partition`, `psid`, `sid_scan`, `player`. `usb_msc`/`fat` are hardware-bound.
- The pac CSR asm (`pac/src/macros.rs`) is `target_arch`-gated; fix it in the
  template (`src/rs/template/pac/`), not just the generated copy.
