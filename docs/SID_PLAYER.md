# SID Player — status & root-cause record

**Status (2026-06-05): playing PSID tunes from USB on hardware.** ✓

A dedicated bitstream that plays PSID files. The arlet verilog-6502 (`cpu.v`) runs
the tune's init/play routines in gateware feeding the reDIP-SID core; VexiiRiscv
handles USB MSC, FAT32, PSID parsing, and the display. Design spec:
`docs/superpowers/specs/2026-06-03-sid-player-design.md`.

Durable implementation gotchas live in the two `CLAUDE.md` files
(`gateware/src/top/sid_player/CLAUDE.md` and repo root). This file is the
historical record of *why* it was broken and what's still open.

---

## What was actually wrong (and how it was fixed)

Three independent bugs, found in order. The first headline diagnosis was **wrong**
and is recorded here only so it isn't re-investigated.

### 1. Bridge bus-timing mismatch — the real root cause
The arlet core uses a **pipelined synchronous bus**: when `RDY=1`, `DI` must carry
the data for the address accepted on the *previous* cycle while the core has already
advanced `AB`. The original combinational/clock-enable bridge delivered current-`AB`
data, so the reset vector mis-resolved (`$0800`→`$0008`), the core landed on `$00`
(BRK) and spun in an interrupt storm (`AB` only ever `$01xx`/`$fffx`, `sid_writes=0`).

Proven in cosim against an ideal cacheless Verilog PSRAM — **not** a cache or
regression issue. Fix: the **windowed clock-enable bridge** (`top.py`, `N=64`). A
free-running `phase` counter pulses `advance`/`cpu_RDY` once per window; the CPU bus
is frozen and decode latched into `*_r` registers each window; `cpu_DI` is registered
and updated only on `advance`, giving the required one-transaction lag (the "REF4"
reference behaviour). `advance` is gated on registered `psram_done_r`, never on live
`cpu_AB` — this is what avoids the `cpu_AB→RDY→DIMUX→cpu_AB` comb loop.

### 2. Decode X-poison (reset/BRK wedge)
During arlet reset/BRK stack pushes, `AB = {STACKPAGE, S}` = `$01xx` with `S` (the
low byte) undefined in sim. Magnitude comparisons (`AB < 0x0800`, `>= 0xD400`) against
X yield X → poisons `is_psram_r` → `cpu_RDY` X → core wedged at BRK forever. Fixed by
decoding with **bit-slice equality** on high bits only (`AB[11:]==0` for BRAM,
`AB[5:]==(0xD400>>5)` for SID).

### 3. Tune never set up the stack (NMI/RTI wedge)
The `tiny_tune` fixture did `LDA/STA/JMP` but never `LDX #$FF; TXS`, so `S` stayed X;
the first NMI's `RTI` pulled garbage and the PC went to `xxxx`. A tune bug, not a
bridge bug. Fixed by regenerating `tiny_tune.bin` with `LDX #$FF; TXS` (spin moved
`$0805`→`$0808`) and updating the test interpreters/cosim.

### The discarded diagnosis: "RISC-V write-back D-cache not flushed"
Originally believed to be the root cause (RISC-V stores stay dirty in L1, the 6502
reads stale PSRAM via its separate wishbone master). The bridge bug reproduced against
a cacheless PSRAM, so this was **not** the cause of the no-audio symptom. Cache
coherency is still a *real* concern for the RISC-V→PSRAM write path, however — see
open items.

---

## Verification

- **Cosim** (`tests/test_cpu6502_cosim.py`) is the authoritative bridge gate: exports
  the Amaranth bridge to Verilog, runs the real arlet under iverilog. Now green:
  `SAW_SPIN=1`, `NMI_ENTERS=5`, `SID_WRITES=6` (1 init + 5 NMI), `FINAL_AB=$080a`.
  Its REF harness (registered memory, `RDY=1`) isolates core/image soundness from bus
  timing.
- Unit tests rewritten to windowed timing (latch on 1st `cpu_RDY` pulse, effect on 2nd).
- Full `pdm test` green.
- **Hardware:** PSID playback from USB confirmed working.

USB note: USB sticks are partitioned (GPT/MBR) — the FAT volume is not at LBA 0.
`partition::first_partition_lba` finds its start LBA; `MscStorage` offsets every read.
(Was the cause of "No .SID on USB" — `fatfs` read the protective MBR as the BPB.)

---

## Open items

- **`flush_6502_image()` (firmware, `main.rs:54`) is still present** and called after
  writing the 6502 image (lines 244/326/375). Unlike the no-audio symptom, write
  coherency *is* a genuine concern: the RISC-V writes the image through its write-back
  L1, while the 6502 reads PSRAM via a separate wishbone master. The current 64 KiB
  cache-thrash works but is crude — prefer replacing it with a real cache-clean, or
  confirm empirically whether any flush is needed now that the bridge is correct.
  Needs a hardware re-test if changed.
- **PSRAM RMW writes** (tune storing above `$07FF`): the bridge now implements the
  sub-FSM (`RD-FOR-RMW → WRITE → psram_done_r`, replacing the old WRITE→IDLE wedge),
  but this path is not yet exercised by a cosim case or validated on hardware
  (Gyroscope_3 never writes PSRAM). Add a cosim tune that stores above `$07FF`.
- **Debug CSRs** (`sid_writes`, `nmi_count`, `state`, `cpu_ab`, `psram_acks`,
  `ab_changes`) — **removed** once audio + USB were confirmed on hardware (commit
  `ad14d3b`), to reclaim LUTs on the ~95%-full 25F. `PlayTimerPeripheral` now
  exposes only the functional `control` register. The `dbg_reset` /
  `dbg_play_rate` / `dbg_irq_enable` inputs are kept as sim-only test stimulus
  (control is write-only), tied to 0 in hardware.
