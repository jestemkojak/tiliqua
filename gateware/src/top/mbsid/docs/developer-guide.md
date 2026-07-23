# MBSID-on-Tiliqua — Developer Guide

Practical workflow: getting a build, running the test tiers, and the rules
about what to re-run after which kind of change. Read
[architecture.md](architecture.md) first for the map;
[`../CLAUDE.md`](../CLAUDE.md) is the terse, always-current gotcha list.

## One-time setup

```sh
git clone <tiliqua repo> && cd tiliqua
cd gateware/src/top/mbsid
./fetch-mios32.sh          # vendored GPL engine — NOT in the repo
```

`fetch-mios32.sh` blobless-clones `github.com/midibox/mios32` and checks
out the pinned commit `44d8e6af401e41a8adf2319ce6a584cce154a14f` into
`./mios32` (idempotent — safe to re-run). Without it, `pdm mbsid build`
fails inside `fw/build.rs`.

Toolchain notes:

- The C++ engine is cross-compiled by **clang++** (no riscv-gcc needed).
- Rust target is `riscv32im`; the pdm build drives everything.
- The pdm project lives in `gateware/`, **not** the repo root.

## Building

```sh
cd gateware
pdm mbsid build                  # full bitstream, ≈4–5 min
pdm mbsid build --fw-only        # relink firmware only (fast)
pdm mbsid build --pac-only       # regenerate Rust PAC from SVD
```

- **`--fw-only`** reuses an existing bitstream. It needs a prior full
  build; without one it errors `missing top.bit` *after* the Rust crate
  compiled — the ELF (`fw/target/riscv32im-*/release/tiliqua-fw`) is still
  produced, only bitstream assembly fails. That error at the end of a
  fw-only rebuild against a valid prior build is **expected and fine**.
- **`--pac-only`** is required after **any CSR change** in the Amaranth
  code, before `--fw-only` — otherwise the firmware can't see the new
  registers. The generated PAC (`pac/src/generated/`) is gitignored, so
  CSR changes never show in `git status`.
- Build artifacts land in `gateware/build/mbsid-r5/` (target names get
  underscores→hyphens; glob with hyphens).
- A failed build surfaces only as `CalledProcessError: 'build_top.sh'
  returned non-zero` — the real cause (e.g. a yosys logic-loop error) is
  in stdout; grep for it.
- **Timing check:** post-route Fmax is the **second**
  `Max frequency for clock '$glbnet$clk'` line in
  `build/mbsid-r5/top.tim` (~line 1390+); the first occurrence is the
  pre-route estimate. On a PASS the post-route line is `Info:`, on a FAIL
  it's `Warning:` — distinguish by order, not by the tag. Current
  reference (post-M6, with the USB-MSC engine + mux **and** the M6b write
  leg included): `sync` 64.29 MHz PASS (60 MHz target), 22872/24288 (94%)
  `TRELLIS_COMB`. This number swings several MHz build-to-build on
  placement-seed noise alone — treat it as a snapshot, not a promise.

## Flashing

```sh
pdm run flash archive build/mbsid-r5/<name>.tar.gz
```

The archive name is the git HEAD short hash at build time. A build that
*failed timing* still produces a flashable archive — check `top.tim`
before trusting hardware behavior.

## The three test tiers

Run them in this order; each is a superset of confidence.

### 1. Host unit tests (seconds)

```sh
cd gateware/src/top/mbsid/fw
cargo test --target x86_64-unknown-linux-gnu --lib
```

155 tests: regdiff, patch store, SysEx capture, menu state machine, frame
diff/painter, param encodings, CV quantizer, USB patch list/load/export
(`usb_patch`: `encode_syx`/`export_patch` round-trip, FAT-image fixtures),
the menu's USB card (including the M6b `Export` and `Import` rows),
whole-bank import (`bank_import`: full/sparse replace, bad-file rejection),
and wall-clock uptime
(`uptime`: expiry-at-limit, `u32` wraparound, tick/now roundtrip). The `riscv32`
FFI is cfg-stubbed on host. Note: you must pass the explicit host
target — the crate's default target is `riscv32im`.

### 2. Host oracle (the keystone, ~a minute)

```sh
cd gateware/src/top/mbsid/host_oracle
./run_oracle.sh
```

Builds the engine + shim for x86 and byte-diffs the L/R SID register
streams of the instrumented upstream engine (`oracle`) vs our shim
(`shim_driver`) across all four engines. Green bar: **28/28 OK** plus the
multi-channel differential, the 128-patch no-crash sweep, and the SysEx
equivalence/bad-checksum tests.

**Re-run the oracle after any change to:** the shim (`mbsid_shim.cpp`),
the facade headers (`fw/csrc/mios32_shim/`), `build.rs` compile flags, or
the vendored-engine pin. Firmware-only Rust changes don't need it (the
oracle doesn't execute Rust), but the unit tests do.

### 3. Full bitstream + hardware

`pdm mbsid build`, check `top.tim`, flash, and walk the relevant hardware
acceptance checklist (`M4_USER_PATCH_BANKS.md §7`, `M5 §8`). Hardware is
the only tier that exercises: static-ctor execution, ISR timing, CSR/FIFO
plumbing, flash writes, CV ADCs, and the display.

## Change-type → required actions

| You changed… | Then |
|---|---|
| Firmware Rust only | `cargo test` (host) → `pdm mbsid build --fw-only` |
| `mbsid_shim.cpp` / facade / engine subset | oracle → `cargo test` → `--fw-only` |
| Shim ABI signature | update `fw/src/mbsid_sys.rs` **and both oracle drivers** in the same change (extern "C" — no mangling protection), then oracle |
| Amaranth CSR layout | `--pac-only` **first**, then `--fw-only` |
| Any gateware | full `pdm mbsid build` + check post-route Fmax in `top.tim` |
| Vendored `mios32/` | **don't.** Never edit vendored C++ — see [extending.md](extending.md) |

## Menu rendering (diff painter)

The menu never clears its background. `menu::build_frame` produces a
`frame::Frame` — up to 12 positioned text items — and `menu::Painter::paint`
diffs it against the previously painted frame (`frame::diff`), emitting only:

- **Erase**: re-blit an old item's text at intensity 0. The blitter draws
  glyph 1-bits in REPLACE mode (no zero-color skip), so this clears exactly
  the pixels the old draw touched.
- **Draw**: blit a new/changed item in its dim/bright style. Items whose text
  is unchanged but whose style changed are redrawn without an erase (identical
  glyph pixels are simply overwritten).

Erases always precede draws, and both go through the single blitter command
FIFO, so ordering is exact. A typical encoder detent costs two rows of glyph
blits; the old implementation cleared the whole 380×244 box per-pixel through
the pixel_plot FIFO first, which was visible as a black wipe.

`frame.rs` (model + diff) and `build_frame` are host-pure and covered by
`cargo test --target x86_64-unknown-linux-gnu --lib`. The persist peripheral
is prevented from decaying the menu band by `persist_freeze_rows=320`
(`top.py`); mbsid has no scope, so phosphor decay has no other consumer.

## Debugging tips

- **Ignore rust-analyzer errors in `fw/`.** The LSP compiles the `no_std`
  crates against the host and floods false diagnostics (missing `std`,
  bogus argument-count errors). The truth is `--fw-only` + host
  `cargo test`, nothing else.
- **No `mcycle`.** VexiiRiscv here has no performance-counter plugin —
  reading `mcycle`/`cycle` **traps and freezes the SoC**. Use gateware
  Timer0 for firmware timing.
- **No atomics.** `riscv32im` has no atomic RMW; use
  `critical_section::Mutex<RefCell<T>>` for ISR/main-loop sharing.
- **Fixed-point streams:** assign `PSQ`/`ASQ` payloads with plain `.eq()`
  (value-preserving); `.as_value().eq(...)` is a raw bit copy that breaks
  scaling.
- **RAM checks:** use `llvm-size -A` for real per-section numbers; the
  default summary folds `.bss`+`.heap`+`.stack` together, and the `.stack`
  section size is just the linker's leftover allocation, not usage. Peak
  stack is measured on hardware with a paint-and-scan probe
  (`M4_USER_PATCH_BANKS.md §6f`).
- **Display code:** redraw only on a `dirty` flag (drawing isn't atomic —
  every-iteration redraws flicker), and `display.clear(HI8::BLACK)` once
  at boot to erase bootloader ghosting.

## Repository layout

```
top/mbsid/
├── top.py                  # MBSIDSoc (subclasses top/sid's SIDSoc; ~30 lines of deltas)
├── fetch-mios32.sh         # one-time vendored-engine checkout (pinned commit)
├── mios32/                 # vendored GPL engine (gitignored)
├── fw/
│   ├── build.rs            # cross-compiles the C++ engine via the cc crate
│   ├── init_array.x        # linker script exposing .init_array (static ctors)
│   ├── csrc/
│   │   ├── mbsid_shim.cpp  # the only authored C++ — extern "C" ABI
│   │   └── mios32_shim/    # platform facade headers
│   └── src/                # firmware Rust (see architecture.md for the module table)
├── host_oracle/            # x86 bit-exactness harness (run_oracle.sh)
├── DESIGN.md, M2..M5*.md   # authoritative specs
├── CLAUDE.md               # terse gotcha reference
└── docs/                   # this folder
```

## Conventions

- **Specs before code** for anything non-trivial: each milestone has a
  design doc with interfaces + acceptance tests written first, so
  implementation is mechanical. Follow that pattern for new milestones.
- **Never edit vendored `mios32/`** — hook behavior at link time or via
  the facade instead (M4's `bankSave` interposer is the worked example).
- Cite design docs from code comments only if the doc is committed under
  plain `docs/` (the repo's `docs/superpowers/` is gitignored scratch).
- Keep `../CLAUDE.md` updated when you learn a new gotcha the hard way.
