# SID Voice Scope Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an always-on, four-trace oscilloscope (SID voices V1/V2/V3 + mix) to the SID player, rendered in gateware to the 640×480 framebuffer, with a compact metadata header above it.

**Architecture:** Reuse the existing `tiliqua.raster` scope stack the way `src/top/macro_osc` does. Add a dedicated 4-port `FramebufferPlotter` and a 4-channel `ScopePeripheral` to the SID player SoC, fed by a non-blocking FIFO that taps the four audio channels already present on `pmod0.i_cal` (which carry `voice0/1/2_dca` + the mix). The `Persistance` peripheral that clears the additive plot already exists in the base `TiliquaSoc` — firmware just configures it for a fast (crisp) decay. Firmware configures the scope once at boot and otherwise only compacts the metadata draw into a header strip.

**Tech Stack:** Amaranth HDL (gateware), Rust `no_std` firmware (`tiliqua-hal`/`tiliqua-pac`), pdm build system.

**Spec:** `docs/superpowers/specs/2026-06-03-sid-voice-scope-design.md`

**Branch:** work continues on the current `sid-player` branch.

**Key reference files (read before starting):**
- `gateware/src/top/macro_osc/top.py` — reference integration of plotter + scope + tap FIFO
- `gateware/src/top/macro_osc/fw/src/{lib.rs,main.rs}` — reference firmware scope/persist config
- `gateware/src/tiliqua/raster/scope.py` — `ScopePeripheral` (CSR map, `n_channels`, `i`, `o[]`, `soc_en`)
- `gateware/src/tiliqua/raster/plot.py` — `FramebufferPlotter(bus_signature, n_ports)`
- `gateware/src/rs/hal/src/{scope.rs,persist.rs}` — `Scope0`/`Persist0` HAL APIs

**Build cheat-sheet:**
- Build bitstream: `cd gateware && pdm sid_player build`
- A failed build only surfaces as `CalledProcessError`; the real cause (e.g. yosys `found logic loop`) is in stdout — pipe through `tee` and grep.
- Utilisation report: `gateware/build/sid_player-r5/top.rpt` (grep `TRELLIS_COMB`, `TRELLIS_FF`, `Device utilisation`).

---

## Task 1: Record baseline FPGA utilisation (resource-risk gate)

The #1 risk in the spec is LUT headroom on the 25F part. Capture a baseline of the *current* design before adding anything, so Task 4 can measure the cost.

**Files:** none modified.

- [ ] **Step 1: Build the current design**

Run from `gateware/`:
```bash
pdm sid_player build 2>&1 | tee /tmp/sid_baseline.log
```
Expected: build completes, `top.bit` produced. (Slow — full place & route.)

- [ ] **Step 2: Record utilisation**

Run:
```bash
grep -iE "TRELLIS_COMB|TRELLIS_FF|Device utilisation|MULT18|DP16KD" gateware/build/sid_player-r5/top.rpt
```
Write the `TRELLIS_COMB` (LUT) count and percentage into a scratch note (e.g. paste into the PR description later). This is the baseline. No commit.

---

## Task 2: Gateware — scope peripheral, dedicated plotter, and tap FIFO

**Files:**
- Modify: `gateware/src/top/sid_player/top.py` (imports near top; `SIDPlayerSoc.__init__`; `SIDPlayerSoc.elaborate`)

- [ ] **Step 1: Add imports**

At the top of `top.py`, alongside the existing `from tiliqua.build... ` imports, add:
```python
from tiliqua import dsp
from tiliqua.raster import PSQ, scope
from tiliqua.raster.plot import FramebufferPlotter
```
(`data` and `stream` are already imported from `amaranth.lib`.)

- [ ] **Step 2: Instantiate the scope + plotter in `__init__`**

In `SIDPlayerSoc.__init__`, immediately **before** the existing `self.finalize_csr_bridge()` call, insert:
```python
        # Dedicated plotter for the scope (base SoC plotter's 3 ports are
        # taken by pixel_plot/blit/line). One port per scope channel.
        self.scope_plotter = FramebufferPlotter(
            bus_signature=self.psram_periph.bus.signature.flip(), n_ports=4)
        self.psram_periph.add_master(self.scope_plotter.bus)

        # 4-channel oscilloscope: V1, V2, V3, mix.
        self.scope_periph = scope.ScopePeripheral(
            n_channels=4, fs=self.clock_settings.audio_clock.fs())
        self.csr_decoder.add(self.scope_periph.bus, addr=0x1300, name="scope_periph")
```

- [ ] **Step 3: Wire the scope graph in `elaborate`**

In `SIDPlayerSoc.elaborate`, **after** the existing `pmod0` audio-routing `m.d.comb += [...]` block (the one assigning `pmod0.i_cal.payload[0..3]`) and **before** `return m`, insert:
```python
        # --- Voice scope ---------------------------------------------------
        m.submodules.scope_plotter = self.scope_plotter
        m.submodules.scope_periph  = self.scope_periph

        # Each scope channel drives one plotter port.
        for n in range(4):
            wiring.connect(m, self.scope_periph.o[n], self.scope_plotter.i[n])

        # Plotter writes into the live framebuffer (fan-out from fb.fbp,
        # same pattern as the base plotter/persist consumers).
        wiring.connect(m, wiring.flipped(self.fb.fbp), self.scope_plotter.fbp)
        self.scope_periph.source = pmod0.i_cal

        # Non-blocking tap of the 4 audio channels already on i_cal.
        # We deliberately ignore plot_fifo.i.ready so the SID audio stream
        # is never stalled by plotting (drops samples if the FIFO is full).
        m.submodules.plot_fifo = plot_fifo = dsp.SyncFIFOBuffered(
            shape=data.ArrayLayout(PSQ, 4), depth=32)
        m.d.comb += [
            plot_fifo.i.valid.eq(pmod0.i_cal.valid & pmod0.i_cal.ready),
            plot_fifo.i.payload[0].eq(pmod0.i_cal.payload[0]),
            plot_fifo.i.payload[1].eq(pmod0.i_cal.payload[1]),
            plot_fifo.i.payload[2].eq(pmod0.i_cal.payload[2]),
            plot_fifo.i.payload[3].eq(pmod0.i_cal.payload[3]),
        ]
        wiring.connect(m, plot_fifo.o, self.scope_periph.i)
```
Note: `pmod0.i_cal.payload[n]` is a fixed-point `ASQ`; `PSQ` shares the same integer bits, so Amaranth auto-reshapes on `.eq()` (this is exactly what `macro_osc` relies on). No upsampling is used (keeps LUTs down — the spec's first resource mitigation); if traces look too sparse on hardware, adding `dsp.Resample` upsamplers is a follow-up.

- [ ] **Step 4: Elaborate/build to verify wiring (no comb loops, CSR fits)**

Run from `gateware/`:
```bash
pdm sid_player build 2>&1 | tee /tmp/sid_scope_build.log | grep -iE "found logic loop|Error|top.bit|Bitstream"
```
Expected: no `found logic loop`, no elaboration `Error`; `top.bit` produced. If it fails, read `/tmp/sid_scope_build.log` for the real cause (CLAUDE.md gotcha: errors hide in stdout).

- [ ] **Step 5: Commit**

```bash
git add gateware/src/top/sid_player/top.py
git commit -m "sid_player: add 4-channel voice scope (plotter + tap FIFO)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: Firmware — scope HAL binding, boot config, compact header

The build in Task 2 regenerated the PAC with `SCOPE_PERIPH`. `Persist0` and `PERSIST_PERIPH` already exist (base SoC), so persistence needs no new binding.

**Files:**
- Modify: `gateware/src/top/sid_player/fw/src/lib.rs`
- Modify: `gateware/src/top/sid_player/fw/src/main.rs`

- [ ] **Step 1: Expose the scope HAL**

In `fw/src/lib.rs`, after the existing `hal::impl_tiliqua_soc_pac!();` line, add:
```rust
#[cfg(not(test))]
hal::impl_scope! {
    Scope0: pac::SCOPE_PERIPH,
}
```

- [ ] **Step 2: Add firmware imports**

In `fw/src/main.rs`, with the other `use` lines near the top, add:
```rust
use tiliqua_hal::persist::Persist;
use tiliqua_lib::scope::{Timebase, VScale};
use tiliqua_hal::embedded_graphics::primitives::{Rectangle, PrimitiveStyle};
use tiliqua_hal::embedded_graphics::geometry::Size;
```
(`Point`, the mono fonts, and `Text` are already imported.)

- [ ] **Step 3: Configure the scope + persistence once, after palette setup**

In `main()`, immediately after `palette::ColorPalette::default().write_to_hardware(&mut display);`, insert:
```rust
    // --- Voice scope: fixed config, always on -----------------------------
    let mut scope   = Scope0::new(peripherals.SCOPE_PERIPH, 6);
    let mut persist = Persist0::new(peripherals.PERSIST_PERIPH);

    // Crisp look: low persistence => fast decay (clears additive traces).
    persist.set_persistence(2);

    scope.set_intensity(8);
    scope.set_yscale(VScale::Scale1V);
    scope.set_timebase(Timebase::Timebase5ms);
    scope.set_trigger_level(0);
    scope.set_hue(0);          // per-channel hue is auto-offset (+3 per ch)
    scope.set_xpos_px(0);

    // Stack four traces below the header band. ypos is an offset from screen
    // centre (240 on a 480-tall fb); these put rows at ~120/200/280/360.
    scope.set_ypos_px(0, -120); // V1
    scope.set_ypos_px(1, -40);  // V2
    scope.set_ypos_px(2, 40);   // V3
    scope.set_ypos_px(3, 120);  // MIX

    // Free-run (trigger_always = true) so traces show without a trigger edge.
    scope.set_enabled(true, true);
```

- [ ] **Step 4: Compact the metadata draw into a header strip**

In the `if redraw { ... }` block of the main loop, **replace** the existing body (the `display.clear(...)` through the final `status` `Text::new(...).draw()`) with a two-line header that clears only the top band (so it never wipes the scope):
```rust
            redraw = false;

            // Clear only the header band, leaving the scope area untouched.
            Rectangle::new(Point::new(0, 0), Size::new(640, 64))
                .into_styled(PrimitiveStyle::with_fill(HI8::BLACK))
                .draw(&mut display)
                .ok();

            let name_str   = trim_ascii(&tune_buf[0x16..0x36]);
            let author_str = trim_ascii(&tune_buf[0x36..0x56]);

            // Line 1: title + tune name.
            let mut line1: String<80> = String::new();
            write!(line1, "SID PLAYER  {}", name_str).ok();
            Text::new(line1.as_str(), Point::new(20, 18), style)
                .draw(&mut display)
                .ok();

            // Line 2: author + song / state.
            let mut line2: String<96> = String::new();
            write!(line2, "{}   Song {}/{} [{}]",
                   author_str, current_subtune, hdr.songs,
                   if paused { "PAUSED" } else { "PLAYING" }).ok();
            Text::new(line2.as_str(), Point::new(20, 40), style_dim)
                .draw(&mut display)
                .ok();
```
The initial banner draw earlier in `main()` (the "SID PLAYER" / "Waiting for USB drive..." text before the USB wait) is unchanged.

- [ ] **Step 5: Build firmware via the bitstream build**

Firmware is compiled as part of the bitstream build. Run from `gateware/`:
```bash
pdm sid_player build 2>&1 | tee /tmp/sid_fw_build.log | grep -iE "error\[|error:|warning: unused|top.bit|Bitstream"
```
Expected: Rust compiles with no `error[`/`error:`; `top.bit` produced.

- [ ] **Step 6: Commit**

```bash
git add gateware/src/top/sid_player/fw/src/lib.rs gateware/src/top/sid_player/fw/src/main.rs
git commit -m "sid_player/fw: configure voice scope + compact metadata header

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: Verify on hardware and check the resource budget

**Files:** possibly `top.py` / `main.rs` for tuning (decay, ypos, timebase), committed if changed.

- [ ] **Step 1: Compare utilisation against the Task 1 baseline**

Run:
```bash
grep -iE "TRELLIS_COMB|Device utilisation" gateware/build/sid_player-r5/top.rpt
```
Compare LUT count/percentage to the Task 1 baseline. Decision gate:
- If utilisation is comfortably under ~85% and timing closed (no `Max frequency ... FAIL` for the `sync` clock in the build log): proceed.
- If over budget / timing fails: apply spec mitigations in order — (a) drop the MIX channel (`n_channels=3`, remove `payload[3]` tap + `set_ypos_px(3,…)`), (b) restrict the scope build to the 45F platform. Re-build and re-check.

- [ ] **Step 2: Flash and observe**

Flash `gateware/build/sid_player-r5/top.bit` to the Tiliqua, insert a USB drive with a `.sid` file, and confirm:
- Header shows on two lines (title+name, author+song/state) and is legible.
- Four scope traces are visible and animate with the music; pause freezes/clears them.
- Traces sit below the header without overlapping it.

- [ ] **Step 3: Tune fixed values if needed**

If traces clip or overlap, adjust in `main.rs`: `set_yscale` (vertical gain), `set_timebase` (horizontal span), the four `set_ypos_px` offsets (vertical placement), and `persist.set_persistence` (1 = crispest, higher = more trail). Rebuild via `pdm sid_player build` and re-observe.

- [ ] **Step 4: Commit any tuning changes**

```bash
git add gateware/src/top/sid_player/fw/src/main.rs gateware/src/top/sid_player/top.py
git commit -m "sid_player: tune voice scope layout/decay for hardware

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-review notes

- **Spec coverage:** stacked 4-trace per-voice scope (Task 2 `n_channels=4` from the 4 `i_cal` channels; Task 3 4× `set_ypos_px`); header strip + always-on scope (Task 3 Step 4 + `set_enabled(true, …)`); controls unchanged (main-loop control code untouched); crisp persistence (Task 3 `set_persistence(2)`); keep MIX, drop only if forced (Task 4 Step 1 mitigation); non-blocking audio tap (Task 2 Step 3); 25F resource risk (Tasks 1 & 4 gate). All covered.
- **No upsampling** is an intentional simplification vs `macro_osc` (resource-risk mitigation); flagged as a follow-up if traces look sparse.
- **Type consistency:** `scope_periph`/`scope_plotter` names, `Scope0`/`Persist0`, and the HAL method names (`set_intensity`, `set_yscale`, `set_timebase`, `set_trigger_level`, `set_hue`, `set_xpos_px`, `set_ypos_px`, `set_enabled`, `set_persistence`) match `gateware/src/rs/hal/src/{scope.rs,persist.rs}` exactly.
