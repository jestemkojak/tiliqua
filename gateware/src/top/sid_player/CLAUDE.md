# SID Player

6502 (arlet `cpu.v`) running a PSID tune from PSRAM via `Cpu6502Bridge`, feeding
a SID peripheral. The bridge (`top.py`) is a windowed clock-enable interface:
a free-running `phase` counter pulses `advance`/`cpu_RDY` once per N-cycle window;
the CPU bus is frozen and decode latched into `*_r` registers each window, so
`cpu_RDY` is never a combinational function of live `cpu_AB` (avoids the
arlet `cpu_AB→RDY→DIMUX→cpu_AB` comb loop — see root CLAUDE.md).

## Play rate (VBlank / CIA multispeed)
- The play routine is driven by `PlayTimerPeripheral`'s NMI; its rate is a
  firmware-computed 32-bit `period` CSR (sys-clk cycles), **not** a PAL/NTSC bit.
  `psid::play_period_cycles` computes it. PSID `speed` (offset $12) is VBI(0) vs
  CIA(1) timing — **not** PAL/NTSC (that's the v2 `flags` field, offset $76).
- CIA/multispeed rate is read back from PSRAM at `$DC04/$DC05` (CIA Timer A) *after*
  INIT runs: zero it first, `thrash_l1_cache()` to evict, then read. Only timers set
  during INIT are seen (not those set in PLAY / via the IRQ vector).

## Gotchas (arlet in simulation)
- arlet's `S`/regs are X at reset, so during reset/BRK stack pushes `AB={STACKPAGE,S}=$01xx` with an X low byte. Decode addresses with bit-slice **equality** (`AB[11:]==0`, `AB[5:]==(0xD400>>5)`), not magnitude `<`/`>=` — Verilog comparisons against X yield X, poison `cpu_RDY`, and wedge the core in a BRK loop.
- arlet never initializes `S`: a 6502 program/test tune must `LDX #$FF; TXS` before any IRQ/NMI, or `RTI` pulls garbage off the stack and the PC goes to `xxxx`.

## Firmware host tests (`fw/`)
- Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib` (the crate's
  default target is `riscv32im`, so the host triple must be explicit).
- Host-testable modules are gated `#![cfg_attr(not(test), no_std)]` and listed in
  `fw/src/lib.rs` *without* `#[cfg(not(test))]`: `bootstrap`, `psid`, `partition`,
  `sid_scan` (pure / `fatfs`-only, no `tiliqua_pac`). `usb_msc`/`fat` are
  `#[cfg(not(test))]` (hardware-bound) — keep new testable logic out of them.
- The pac CSR asm (`pac/src/macros.rs`) is `target_arch`-gated so the crate
  compiles on the host; that file is regenerated from `src/rs/template/pac/` on
  build, so fix it **in the template**, not just the generated copy.
- `sid_scan` builds a real GPT+FAT image in memory (`fatfs`) so the USB
  `.SID`-loading path is tested without hardware. USB sticks are partitioned
  (GPT/MBR): the FAT volume is **not** at LBA 0 — `partition::first_partition_lba`
  finds its start LBA and `MscStorage` offsets every read by it.

## Testing the bridge
- `tests/test_cpu6502_cosim.py` is the authoritative bridge test: it exports the Amaranth bridge to Verilog and runs the *real* arlet under iverilog (`SAW_SPIN`/`SID_WRITES`/`NMI_ENTERS`). Its REF harness (registered memory, `RDY=1` always) isolates core/image soundness from the bridge's bus timing — if REF passes but the bridge cosim fails, it's the bridge.
- `tests/data/tiny_tune.bin` is a hand-built 64KB fixture (reset=$0800 init, NMI=$0820 `INC $D400;RTI`, spin at $0808). Regenerate via a small Python script writing the image; keep the test interpreters / cosim spin-address checks in sync.
- Windowed-bridge timing for unit tests (`tests/test_cpu6502_bridge.py`): an access is *latched* on the 1st `cpu_RDY` pulse and its effect (committed write, `sid_w_en`, settled `cpu_DI`) appears on the *2nd* — sample on the 2nd pulse.
- `cpu_DI` is driven combinationally from the window's registered address (one-window lag), not via a registered `di_r`.
