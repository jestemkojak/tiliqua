# SID Player — prior-art review & alternative architectures (2026-06-06)

Review of external references (provided by the user) to (a) see if they help the
current sid_player problems and (b) find an alternative way to play SID files
that could become a new project if fixing the current gateware-6502 approach
fails. Current problems recap (see `docs/SID_PLAYER.md`): **#1 the design fails
`sync` timing** (60 MHz clocked, ~50.6 MHz placed, 90 % LUT) and **#2 the
gateware 6502 is throughput-bound** (all tune code in PSRAM, per-fetch HyperRAM
latency, dropped NMIs on high-rate tunes).

---

## TL;DR

- The single most useful idea found is the **pre-computed register-dump** approach
  (somuch.guru `.sidraw`): a PC tool runs the tune through libsidplayfp and
  records every SID register write + cycle timestamp; the FPGA just *replays*
  them. This **eliminates the emulated 6502, the bridge, the PSRAM-throughput
  problem (#2), the CIA timing problem, and most of the LUTs (helps #1)**.
- A second strong option: **run a 6502 emulator in *software* on the VexiiRiscv**
  we already have (it has caches + runs from fast RAM → no per-fetch HyperRAM
  stall), writing the hardware SID via the existing CSR path. Deletes the
  gateware 6502 + bridge (LUT/timing relief), keeps live `.sid` playback from USB.
- A **ready-made cycle-accurate CIA core** (daglem `reDIP-CIA`) exists if we keep
  the gateware-6502 path and want to fix the "rate set in PLAY/IRQ" limitation —
  but it *adds* logic, so it's wrong for #1 until timing is closed.
- Immediately actionable regardless of approach: **run Klaus2m5's 6502 functional
  test suite** through our cosim to definitively confirm the arlet core is correct.

None of the references suggest the current symptoms are a SID-core fidelity
problem — reDIP-SID is reSID-class. They point at the *CPU-execution architecture*.

---

## Per-reference notes

| Reference | What it is | Relevance to us |
|---|---|---|
| **somuch.guru — "Creating an FPGA SID chiptune player (sort of)"** | PC tool (`sidraw-dump`) runs `.sid` through **libsidplayfp**, captures every SID register write timestamped to the 1 MHz scheduler clock, emits a `.sidraw` = `{write reg / wait N ticks}` stream; FPGA (Cyclone III/MiST) just replays it into a SID core. Patched libsidplayfp `currentCycle()` to kill ±100 ms drift. | **Highest.** This is *Alt A* below. Bypasses 6502/bridge/CIA/throughput entirely; the FPGA replay engine is a counter + FIFO. |
| **hankdraco/realsidplayer** | ATxmega128 (AVR) MCU runs a **software 6502 emulator** executing the real tune code, driving a **real SID chip**. Limited to ~7 KB tunes by AVR RAM. | High — *proves a modest CPU can emulate the 6502 fast enough for SID*. Validates *Alt B* (software 6502); our VexiiRiscv + PSRAM removes the AVR's RAM limit. |
| **lowlander/spi_sid** | Zephyr-RTOS MCU streams SID register writes over **SPI** to an FPGA SID bridge. | Medium — same "stream register writes, no CPU emulation" philosophy as `.sidraw`, live from an MCU. Reinforces *Alt A/B*. |
| **daglem/reDIP-64** | Full C64-component FPGA platform on **ECP5-5G (LFE5UM5G-25)** + HyperRAM (same FPGA *family* as Tiliqua's ECP5-25k), designed to physically replace C64 chips (6510/VIC/CIA/SID). README doesn't pin the CPU core/opcodes. | Medium — reference for the *full faithful C64* path (*Alt C*). Heavy; "replace real chips" board, not a self-contained player. |
| **daglem/reDIP-CIA** | **Cycle-accurate MOS 6526/8521/8520 CIA** in SystemVerilog, open toolchain (tiny iCE5LP1K). Same author as our reDIP-SID. | Medium — droppable into our design to give faithful CIA timer IRQs (fixes the "timer set in PLAY/IRQ not detected" limitation) **if** we keep the gateware 6502. Adds LUTs → only after #1. |
| **MiSTer-devel/C64_MiSTer** | Full C64 core, "based on **FPGA64** by Peter Wendrich"; dual SID, CIA, VIC; targets Cyclone V. | Reference for *Alt C* (full emulation). Big; uses FPGA64's 6510 core. No dedicated SID-player mode (plays SIDs by *being* a C64). |
| **Klaus2m5/6502_65C02_functional_tests** | Comprehensive 6502/65C02 assembly test suite: all valid opcodes+addressing, interrupts, decimal mode; run binary, watch for success trap address. Author explicitly lists "fpga core" as a target. | **Actionable now.** Load `6502_functional_test.bin` into our cosim PSRAM, run the real arlet, assert it reaches the success trap → definitively confirm/deny "is arlet the problem." |
| **6502.org t=7418** | Transistor-level dissection of the MOS 8521 CIA. | Low — restoration/analysis, not playback. (Useful only if hand-building a CIA core.) |
| dustlayer "Tick Tock / clock", laughtonelectronics "65xx timing", deblauweschicht "mos6581", reDIP-SID main | C64 φ2 clock basics, 65xx cycle timing, SID internals, the SID core we already use. | Background — consistent with what we know (φ2 ≈ 0.985/1.022 MHz; reDIP-SID = reSID-class). No new lever. |
| misterfpga.org t=2693 | MiSTer forum thread — **could not fetch (HTTP 403)**; retry with an authenticated fetch/`gh` if needed. | Unknown. |
| rkrajnc/sidsynth-mist | SID synth on MiST (not deep-read). | Likely a MIDI/synth use of a SID core (like our `sid` top), not a file player. |

---

## The three alternative architectures

### Alt A — Pre-computed register-dump replay (`.sidraw`)  ← simplest, kills #1 and #2
**How:** offline PC tool runs each `.sid` through libsidplayfp, dumps timestamped
SID register writes to a `.sidraw` stream. The Tiliqua loads `.sidraw` from USB
(VexiiRiscv/FAT, already implemented) and a **trivial replay engine** (a 1 MHz/φ2
tick counter + the existing SID write FIFO) emits each write at its tick.

- **Kills #2 entirely:** no 6502, no instruction fetch, no PSRAM code reads.
- **Helps #1 strongly:** delete `Cpu6502` (~800 LUT), `Cpu6502Bridge` (+cache),
  `PlayTimerPeripheral` → big LUT/congestion relief → easier timing.
- **Plays *everything* libsidplayfp plays:** illegal opcodes, digi/samples,
  multispeed, RSID, dynamic CIA — because all the hard emulation happens on the PC.
- **Cons:** not live — each tune needs offline conversion (libsidplayfp is too
  heavy for the softcore); `.sidraw` is larger than `.sid` (a write stream vs
  compact player+data); no "load any .sid off a random stick" without a converter.
- **Verdict:** the cleanest, lowest-risk way to get *correct audio for the tunes
  you care about*, at the cost of a PC preprocessing step.

### Alt B — Software 6502 on the VexiiRiscv  ← keeps live `.sid`, also kills #1 and #2
**How:** delete the gateware 6502 + bridge; port a compact 6502 emulator
(e.g. `fake6502`/`chips` 6502, or libsidplayfp's core) into the RISC-V firmware.
It executes the tune's INIT/PLAY over the 64 KB image (in PSRAM, served through
**VexiiRiscv's own I/D cache** → fetch latency amortised, the exact thing the
gateware 6502 lacks), and writes the hardware SID via the **existing
`SIDPeripheral` CSR `transaction_data` path** (already used by the `sid` top).

- **Kills #2:** the RISC-V has a real cache and runs from fast RAM; emulating a
  6502 instruction is a handful of RISC-V instructions — a 50 Hz/1000-instr
  `play()` is trivial, and even 300 Hz multispeed is easy.
- **Helps #1:** removes the gateware 6502 + bridge + cache → LUT/timing relief.
- **Live `.sid` from USB retained;** CIA timers, illegal opcodes, decimal mode are
  *easy in software*. Effectively what `realsidplayer` does on an AVR, but our
  RISC-V + PSRAM has no 64 KB / tune-size limit.
- **Cons:** firmware CPU budget must cover emulation + USB + UI (almost certainly
  fine); needs a software player loop + interrupt model; the SID CSR write path is
  one-write-per-φ2 (already adequate).
- **Verdict:** best balance — live playback, removes both root causes, reuses the
  already-proven RISC-V→CSR→SID path. This is the recommended new-project shape.

### Alt C — Full faithful FPGA C64 (T65/FPGA64 + reDIP-CIA + reDIP-SID)
**How:** instantiate a complete C64 (6510 with illegal opcodes, real CIA, VIC for
raster IRQ, SID) as in C64_MiSTer/reDIP-64, and run the PSID environment on it.

- **Most faithful** (handles every edge case, RSID, demos).
- **Cons:** **largest LUT/timing cost** — directly worsens #1 on an already-90 %,
  timing-failing 25 k device. T65 alone is bigger than arlet; add CIA+VIC.
- **Verdict:** wrong direction for this board unless paired with a much larger
  FPGA. Good reference, not a fix.

---

## Recommendations

1. **Immediately (validation, no new project):** run **Klaus2m5 `6502_functional_test`**
   (and `6502_interrupt_test`) through `tests/test_cpu6502_cosim.py`'s real-arlet
   harness — load the test binary into the behavioural PSRAM, run, assert the
   success trap PC. This settles whether arlet's correctness (incl. the lack of
   illegal opcodes) contributes at all. Cheap, high-value.
2. **If we keep the current gateware-6502 design:** finish the two plans
   (`2026-06-06-sid-timing-remove-scope.md` then `…-psram-fetch-cache.md`); add
   **reDIP-CIA** later only after timing has headroom, to fix the PLAY/IRQ-set-rate
   limitation.
3. **If the current design can't be made to meet timing / play high-rate tunes
   (new project):** pursue **Alt B (software 6502 on VexiiRiscv)** first — it
   removes both root causes and keeps live `.sid` playback by reusing the
   `sid` top's RISC-V→CSR→SID path. Keep **Alt A (`.sidraw`)** as the
   guaranteed-correct fallback (trivial gateware, needs PC preprocessing).
   Avoid **Alt C** on the 25 k device.

## Open follow-ups
- Re-fetch misterfpga.org t=2693 (403'd) via `gh`/authenticated fetch if its
  content matters.
- Confirm libsidplayfp licensing (LGPL/GPL) before shipping `.sidraw` tooling.
- For Alt B, pick the 6502 emulator (license + size): `fake6502` (public-domain,
  illegal opcodes), `chips` 6502 (cycle-stepped), or libsidplayfp's core.
