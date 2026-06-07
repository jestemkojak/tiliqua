# Tiliqua

## Build & test (pdm project lives in `gateware/`, not repo root)
- `cd gateware && pdm <target> build` — build a bitstream (targets in `gateware/pyproject.toml [tool.pdm.scripts]`, e.g. `sid_player`, `polysyn`, `bootloader`)
- `pdm <target> build --pac-only` regenerates the Rust PAC from SVD (do this after any Amaranth CSR change, else firmware won't see new registers); `--fw-only` rebuilds firmware reusing the existing bitstream. The PAC `pac/src/generated/` is gitignored & regenerated each build, so CSR changes never show in `git status`.
- `pdm test` — run all tests (`pytest -n auto tests/`); or `pdm run pytest tests/test_x.py -v`
- Full bitstream build ≈ 4–5 min. Achieved (post-route) Fmax is the **second** `Max frequency for clock '$glbnet$clk'` line in `build/<target>-r5/top.tim` (a `Warning:`); the first (`Info:`) is the pre-route estimate.
- A failed build surfaces only as `CalledProcessError: 'build_top.sh' returned non-zero`; the real cause (e.g. yosys `found logic loop` / abc9 `no_loops`) is in stdout — grep for it.
- Generated synthesis artifacts (`cpu.v`, `top.ys`, `top.rpt`, `top.il`) land in `gateware/build/<target>-r5/`, but with **underscores→hyphens**: target `sid_player_sw` → `build/sid-player-sw-r5/` (globbing with underscores finds nothing).
- Flash a built bitstream: `pdm run flash archive build/<target>-r5/<name>.tar.gz` (archive name = git HEAD short hash at build time; a timing-fail build still produces a flashable bitstream).

## Clocks (`src/tiliqua/pll.py`)
- `sync` (60 MHz) is the **Main clock**: CPU, SoC, **and USB** (ULPI requires 60 MHz). `fast` = **2×sync** (120 MHz) drives the PSRAM/HyperRAM controller. So `sync` **cannot** be lowered to close timing without breaking USB and the PSRAM controller — free LUTs or move logic to a separate slower domain instead. Video (`dvi`/`dvi5x`) is a separate, bootloader/modeline-driven PLL.

## Gotchas
- arlet 6502 (`cpu.v`): `RDY` combinationally selects `DIMUX` (`DIMUX = ~RDY ? DIHOLD : DI`) which feeds `AB`. A peripheral driving `cpu_RDY` combinationally from `cpu_AB` forms an unsynthesizable comb loop — drive `RDY` from registered FSM state instead.
- VexiiRiscv (`src/vendor/vexiiriscv/vexiiriscv.py`) is built without a performance-counter plugin: reading the `mcycle`/`cycle` CSR (e.g. `riscv::register::mcycle`) traps and freezes the SoC. Use the gateware `Timer0` for firmware timing instead.
- Firmware menu/options reference impl is `src/top/macro_osc/` (the `opts` derive crate + `tiliqua_lib::ui`/`draw`, pages=cards; real-time work in the TIMER0 ISR via `irq::scope`+`handler!`+`critical_section`). Copy from it for new option-driven UIs.
- `amaranth.sim`: `ctx.get()`/`ctx.set()` work only inside `add_testbench` coroutines, not `add_process` (raises TypeError in a process).
- Assigning to a fixed-point (`PSQ`/`ASQ`) stream payload: use plain `.eq()` (value-preserving, aligns binary points) — `.as_value().eq(raw_int)` is a raw bit copy that changes scale and keeps DC bits. `PSQ`=Q1.13 (signed14), `ASQ`=Q1.15 (signed16); `ASQ→PSQ` value-preserving = `>>2`.
