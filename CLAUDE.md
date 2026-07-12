# Tiliqua

## Agent skills

### Issue tracker

Issues are tracked locally as markdown files under `.scratch/`. See `docs/agents/issue-tracker.md`.

### Triage labels

Five canonical states: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context setup. Domain reference is `CLAUDE.md` (gateware/firmware project docs). See `docs/agents/domain.md`.

## Build & test (pdm project lives in `gateware/`, not repo root)
- `cd gateware && pdm <target> build` — build a bitstream (targets in `gateware/pyproject.toml [tool.pdm.scripts]`, e.g. `sid_player`, `polysyn`, `bootloader`)
- `pdm <target> build --pac-only` regenerates the Rust PAC from SVD (do this after any Amaranth CSR change, else firmware won't see new registers); `--fw-only` rebuilds firmware reusing the existing bitstream — it needs a prior full build, else it errors `missing top.bit` *after* the Rust crate already compiled cleanly (the firmware ELF `fw/target/riscv32im-*/release/tiliqua-fw` is still produced; only bitstream assembly fails). The PAC `pac/src/generated/` is gitignored & regenerated each build, so CSR changes never show in `git status`.
- `pdm test` — run all tests (`pytest -n auto tests/`); or `pdm run pytest tests/test_x.py -v`
- Full bitstream build ≈ 4–5 min. Achieved (post-route) Fmax is the **second** `Max frequency for clock '$glbnet$clk'` occurrence in `build/<target>-r5/top.tim` (~line 1390+); the first (~line 345) is the pre-route estimate. The post-route line is `Warning:` only when that clock **fails**; on a PASS it's `Info:` — distinguish by order/line-number, not the Warning tag.
- A failed build surfaces only as `CalledProcessError: 'build_top.sh' returned non-zero`; the real cause (e.g. yosys `found logic loop` / abc9 `no_loops`) is in stdout — grep for it.
- A subagent running a full build (~5 min) may return early saying it's "waiting for the build-completion notification" — that notification goes to the *dispatching* controller, not the subagent itself. Resume it with a status-check message (it'll then actually block on the build) rather than assuming it's done or stuck.
- Generated synthesis artifacts (`cpu.v`, `top.ys`, `top.rpt`, `top.il`) land in `gateware/build/<target>-r5/`, but with **underscores→hyphens**: target `sid_player_sw` → `build/sid-player-sw-r5/` (globbing with underscores finds nothing).
- Flash a built bitstream: `pdm run flash archive build/<target>-r5/<name>.tar.gz` (archive name = git HEAD short hash at build time; a timing-fail build still produces a flashable bitstream).

## Clocks (`src/tiliqua/pll.py`)
- `sync` (60 MHz) is the **Main clock**: CPU, SoC, **and USB** (ULPI requires 60 MHz). `fast` = **2×sync** (120 MHz) drives the PSRAM/HyperRAM controller. So `sync` **cannot** be lowered to close timing without breaking USB and the PSRAM controller — free LUTs or move logic to a separate slower domain instead. Video (`dvi`/`dvi5x`) is a separate, bootloader/modeline-driven PLL.
- A multiply/divide by a **runtime signal** (e.g. `freeze_rows * timings.h_active`) that feeds `sync`-domain logic synthesises a MULT/DSP onto the sync critical path. **Register the result** (modeline-derived signals change only on a mode switch) to keep it off the path — an unregistered one regressed sid_player_sw sync Fmax 57→50 MHz (PASS→FAIL).
- `dvi`/`dvi5x` are a separate modeline-driven video PLL; their achieved Fmax near the pixel-clock target flips PASS/FAIL on placement seed between otherwise-identical builds and is unaffected by `sync`/SoC logic changes — don't mistake it for a regression.
- `sync`'s post-route Fmax can also swing several MHz between builds from nextpnr placement-seed noise alone, even for a change with zero logic impact on its critical path — verified on mbsid: `persist_freeze_rows=320` (a pure build-time kwarg, no CSR/register change) moved `sync` Fmax 61.76→68.20 MHz. A PASS-side jump isn't evidence the change helped; only a FAIL is actionable.

## Gotchas
- `docs/superpowers/` is gitignored (plan/spec scratch for the planning skill) — a doc under it existing in the working tree does NOT mean it's committed. If a fix's code comments cite a design doc as living documentation, put that doc under plain `docs/` and commit it with (or before) the fix — don't assume.
- arlet 6502 (`cpu.v`): `RDY` combinationally selects `DIMUX` (`DIMUX = ~RDY ? DIHOLD : DI`) which feeds `AB`. A peripheral driving `cpu_RDY` combinationally from `cpu_AB` forms an unsynthesizable comb loop — drive `RDY` from registered FSM state instead.
- VexiiRiscv (`src/vendor/vexiiriscv/vexiiriscv.py`) is built without a performance-counter plugin: reading the `mcycle`/`cycle` CSR (e.g. `riscv::register::mcycle`) traps and freezes the SoC. Use the gateware `Timer0` for firmware timing instead.
- Firmware menu/options reference impl is `src/top/macro_osc/` (the `opts` derive crate + `tiliqua_lib::ui`/`draw`, pages=cards; real-time work in the TIMER0 ISR via `irq::scope`+`handler!`+`critical_section`). Copy from it for new option-driven UIs.
- `amaranth.sim`: `ctx.get()`/`ctx.set()` work only inside `add_testbench` coroutines, not `add_process` (raises TypeError in a process).
- **Ignore the editor's rust-analyzer errors on firmware** (`fw/` crates are `no_std`/`riscv32im`): the LSP compiles them against the host and floods false `cannot find crate/macro std|test|assert` diagnostics — and can also surface unrelated-looking errors this way (e.g. a false `E0061` "wrong argument count" on a call that's actually correct). Don't trust the diagnostic's shape to decide it's a false positive; verify by actually running `pdm run <target> build --fw-only` + `cargo test --target x86_64-unknown-linux-gnu --lib`.
- Assigning to a fixed-point (`PSQ`/`ASQ`) stream payload: use plain `.eq()` (value-preserving, aligns binary points) — `.as_value().eq(raw_int)` is a raw bit copy that changes scale and keeps DC bits. `PSQ`=Q1.13 (signed14), `ASQ`=Q1.15 (signed16); `ASQ→PSQ` value-preserving = `>>2`.
- **DMAFramebuffer redraw**: the display scanout reads PSRAM continuously; calling `draw()` every tight loop iteration flickers because the background-clear and text-draw are not atomic. Use a `dirty` flag and only redraw when state changes (pattern from `macro_osc`). Also call `display.clear(HI8::BLACK)` once at startup to erase bootloader ghosting — `HI8` and `DrawTarget` are already in scope from `impl_tiliqua_soc_pac!()`, no extra `use` needed.
- **`riscv32im` has no atomic RMW extension**: `core::sync::atomic::{AtomicUsize, ...}::{fetch_add, load, store}` fail to compile (`method not found`) on firmware targets in this repo. Use `critical_section::Mutex<RefCell<T>>` (the existing ISR/main-loop sharing pattern) or, for state only ever touched by one side, a plain `static mut` guarded by that invariant.
- **RAM budget checks**: `llvm-size <elf>`'s default summary folds `.bss`+`.heap`+`.stack` into one "bss" number, and the `.stack` *section* size reported by `llvm-size -A` is just the linker's leftover-region allocation (whatever's left of mainram), NOT measured usage — don't read "stack section nearly full" as "stack nearly overflowing." To get real peak stack usage, measure on hardware: paint the stack region with a sentinel byte at the top of `main()`, then scan from the bottom for the first non-sentinel byte (a monotonic high-water mark); log growth over a free UART.
