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
- Fix: `top/sid/audio.py` `AudioDecimator` = polyphase FIR (`dsp.Resample`). Two
  instances (PAL 985.5kHz → 32/657, NTSC 1.023MHz → 16/341) run in parallel;
  the `phi2_sel` CSR (0=PAL, 1=NTSC, firmware auto-sets from the PSID header
  per tune, Clock menu row overrides) muxes which one reaches the codec/scope
  mix. phi2 itself comes from `Phi2Divider` (fractional-N, `src/top/sid/top.py`)
  at the same rates — true C64 pitch within +0.5 cents. Small input FIFO
  (absorbs the single-MAC FIR's per-output backpressure burst). Fed by
  `SIDPeripheral.audio_strobe`.
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
- Ghost-text discipline (band is frozen → no decay erases stale pixels): a changed
  displayed value MUST clear the region that actually changed. `redraw_row = Some(selected)`
  only clears the *pressed* row — if the change lands on a different row (e.g. an unsupported
  pick flips the State row while you're on the File row), use full `redraw`. Title/author
  shrink needs `redraw_title` (full-width); rows use a narrow centred strip (`band_x`/`band_w`,
  widen if long text ghosts at the edges); `first_paint` does the one full clear (boot splash).
- `fat::load_sid` fills `tune_buf` *before* `reload_tune` validates, so a failed/unsupported
  load leaves the rejected file in `tune_buf` while the old tune plays on. Never paint display
  data live from `tune_buf` (name $16 / author $36) — snapshot it into owned strings
  (`cur_name`/`cur_author`) on each *good* load (`snapshot_meta`).

### File-browser limits (`sid_scan.rs`)
- **Only the first 64 root `.SID` files are browsable.** `SidList = heapless::Vec<SidName,
  MAX_SIDS>` with `MAX_SIDS = 64` (`sid_scan.rs:12`): `list_root_sids` stops pushing once the
  Vec is full (`out.push(...).is_err() -> break`), and `browse_idx` is clamped to
  `file_list.len()-1`, so the 65th+ file (in FAT directory order) can't be reached from the UI.
  The cap is a fixed-capacity heapless allocation — no heap in this `no_std` firmware, so the
  list must be statically sized; cost is `MAX_SIDS × 16 B ≈ 1 KB` RAM. To raise it, bump
  `MAX_SIDS` (RAM scales linearly). Not currently a problem; documented so it's findable.
- **Root directory only, non-recursive** — subdirectories are skipped (`entry.is_dir()`), and
  names are FAT **8.3 short names** (`SidName = String<16>`, e.g. `GYROSC~1.SID`), not LFNs.
  `Rescan USB` (Config card) and the boot/hot-plug paths all share this same enumeration.

## Play rate (VBlank / CIA multispeed)
- Rate computed by `psid::play_period_cycles` from `hdr.clock()` / `hdr.is_cia(subtune)`.
  PSID `speed` (offset $12) is VBI(0) vs CIA(1) — **not** PAL/NTSC.
- CIA timer value is read from `cpu.memory.mem[0xDC04/0xDC05]` directly after INIT
  (the RISC-V is the only PSRAM master of the image, so no cache-thrash needed).
- PLAY-frame SID writes are captured into `cpu.memory.writes` by `call()` and
  drained to the chip **backpressured** (`sid_write_bp`, polling the FIFO's
  `writable`) at the end of `play_tick` — NOT paced to a fixed per-frame anchor.
  The depth-16 transaction FIFO + its 1-per-phi2 drain supply the spacing; the
  old fixed-anchor busy-wait was removed (it burned ISR budget to defeat an
  ADSR-jitter that was disproven). See
  `docs/superpowers/specs/2026-06-15-remove-paced-replay-anchor-design.md`.

## CV / eurorack inputs (firmware)
- Read a calibrated CV input: `PMOD0_PERIPH.sample_iN().read().bits() as i16 as i32` — the
  **double cast** sign-extends the signed ASQ (Q1.15) out of the unsigned register word (a bare
  `as i32` zero-extends and corrupts negative CV). **4000 counts = 1 volt.**
- Per-jack patch-detect: `PMOD0_PERIPH.jack().read().bits()` is a `u8`, one bit per input jack
  (set = patched). Used to auto-engage CV features only when a cable is present.
- CV modulation of live playback (CV1 cutoff / CV2 PW / CV3 progressive mute) is the `cvmod.rs`
  module: a pure, host-tested `compute(&shadow, dirty, cv_raw, jacks) -> WriteList` called from
  the `play_tick` ISR after the tune-write drain. The 6502's per-frame SID writes are mirrored
  into a `shadow` + a `dirty` mask so the override re-asserts only on change (steady CV ≈ 0 extra
  writes/frame). Reset the shadow + `CvMod` on every (re)load AND seed it with the INIT writes
  before `drain_sid_writes` clears them (boot + `reload_tune` both). Spec/plan:
  `docs/superpowers/{specs,plans}/2026-06-18-cv-modulation*.md`.

## Gotchas (firmware)
- **VexiiRiscv has no `mcycle`/perf-counter CSR** (`vexiiriscv.py`: no perf-counter
  plugin). Reading `mcycle`/`cycle` traps → freezes the whole SoC. Use the gateware
  `Timer0` for any firmware timing (counter is a down-counter; ISR via `enable_tick_isr`).
- **Real-time work must run in the TIMER0 ISR, not the UI loop.** The UI loop
  repaints the menu and (best-effort) handles input; it's too slow to also host
  `play()` — coupling them throttles/jitters playback.
- **The menu band (`y < HEADER_H`) is frozen from `persist` decay in gateware**
  (`persist_freeze_rows=200` in `top.py` → `Persistance.freeze_rows`, which skips
  decay for framebuffer rows above the band). So the UI loop repaints the menu
  **only on change** (input / `redraw` / `redraw_row`), NOT every frame — this
  keeps it off the PSRAM bus between interactions (more bandwidth for 6502
  fetches). `HEADER_H` (main.rs) and `persist_freeze_rows` (top.py) MUST stay
  equal. The scope region below the band still decays normally. NB: `freeze_rows`
  is **registered** in `persist.py` — `freeze_rows*h_active` is a runtime multiply
  and an unregistered version lands a MULT18X18D on the sync critical path
  (regressed sync Fmax 57→50 MHz). `freeze_rows=0` is a const-0 no-op for all
  other tops.
  - **The freeze is rotation-aware** (`persist.py`). The firmware draws the menu in
    *logical* screen coordinates, but the plot backend (`raster/plot.py`) rotates
    writes into *framebuffer-memory* space. At 720×720 the HAL forces `Rotate::Left`
    (round-screen hack, `rs/hal/dma_framebuffer.rs`), so the logical top band lands
    in the **last `freeze_rows` columns of every memory row**, NOT the first
    `freeze_rows` memory rows. `Persistance` therefore reads `fbp.rotation` and
    freezes the trailing-column band under `LEFT` (a wrapping `col_word` counter),
    the leading-row band under `NORMAL` (external monitors); `INVERTED`/`RIGHT` fall
    back to `NORMAL` (unused on real HW). A rotation-blind freeze (the original)
    leaves the menu in the decaying region → it flashes on encoder input then
    vanishes. Regression test: `tests/test_raster.py::test_persist_freeze_left`.
    General lesson: any gateware effect reasoning in framebuffer memory space must
    account for the round screen's `LEFT` rotation.
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

## Audio debugging tools (`tools/`)
- Run them with `gateware/.venv/bin/python` (has matplotlib+scipy); the system
  `python3` has only numpy → `wav_compare.py` etc. die with `ModuleNotFoundError: matplotlib`.
- `host_render/` renders the firmware's dumped SID write-stream through **verilated
  reSID** (timing-immune): dump via `DUMP_SID=… cargo test --lib dump_writes -- --ignored
  --nocapture`, render with `host_render/render.sh`. If host output is correct but HW is
  wrong, the bug is gateware/hardware (e.g. the 60MHz timing FAIL corrupts the reSID filter
  → unfiltered mix), NOT firmware/model.
- Envelope cross-correlation is unreliable on dense SID tunes (no silence to align on);
  prefer host_render as the reference and compare per-window drift / spectral bands.

## Sibling target
- `src/top/sid_player/` is the OLD **hardware-6502** variant (gateware `Cpu6502Bridge` +
  arlet `cpu.v`). This `_sw` tree is the active **software-6502** player — don't cross-edit.
