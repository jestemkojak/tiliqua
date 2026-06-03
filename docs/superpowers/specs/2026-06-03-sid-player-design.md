# SID Player Bitstream — Design Spec

**Date:** 2026-06-03
**Target platform:** SoldierCrab R5 (ECP5 LFE5U-25F, 24k LUTs)
**Branch:** sid-player

---

## Overview

A dedicated bitstream that plays PSID-format SID music files from a USB thumb drive.
The arlet verilog-6502 runs the tune's init and play routines in gateware; VexiiRiscv
handles USB mass storage, FAT32, PSID parsing, and the display UI.

Scope: **PSID only** (no RSID — no CIA timer or VIC raster IRQ emulation required).

---

## Architecture

```
USB thumb drive
    │ (USB MSC / SCSI bulk)
    ▼
USBMSCHost ──► VexiiRiscv firmware
                    │ parses FAT32 + PSID header
                    │ loads tune data → PSRAM
                    │ writes bootstrap stub → PSRAM
                    │ sets play rate + releases reset
                    ▼
              arlet verilog-6502
                    │ init routine (once)
                    │ play routine (50/60 Hz via trampoline)
                    │ writes $D400–$D41F
                    ▼
              Cpu6502Bridge
               ├── $0000–$07FF → 2KB BRAM  (zero page + stack + scratch)
               ├── $D400–$D41F → SIDPeripheral transaction FIFO
               └── everything else → PSRAM (RDY stall, ~8 cycles)
                    ▼
              SID core ──► audio codec ──► audio out
```

VexiiRiscv is responsible for all software work (USB, FAT32, PSID parsing, display).
Once it releases the 6502 from reset, playback is entirely gateware-driven. The RISC-V
only intervenes on subtune change or play/pause.

---

## Gateware Components

### New components (`src/top/sid_player/top.py`)

**`Cpu6502`**
Thin Amaranth wrapper around arlet `cpu.v`, following the same pattern as the existing
`SID` component that wraps `sid_api.sv`. Exposes: `clk`, `reset`, `AB[15:0]`, `DI[7:0]`,
`DO[7:0]`, `WE`, `IRQ`, `NMI`, `RDY`. No debug/voice-tap outputs.

**`Cpu6502Bridge`**
Address decoder and memory router. On every 6502 bus cycle it inspects `AB` and `WE`:
- $0000–$07FF → 2KB on-chip BRAM (single-cycle, no stall)
- $D400–$D41F + `WE=1` → push `{DO, AB[4:0]}` into `SIDPeripheral` transaction FIFO
- Everything else → PSRAM at `(cpu6502_psram_base + AB)`, asserts `RDY=0` until done

**`PlayTimerPeripheral`**
Small CSR peripheral with three RISC-V-writable bits:

| Register | Bits | Purpose |
|---|---|---|
| `reset` | 1 | Hold 6502 in reset when 1 |
| `play_rate` | 1 | 0 = 50 Hz (PAL), 1 = 60 Hz (NTSC) |
| `irq_enable` | 1 | 0 = paused, 1 = play timer active |
| `play_addr` | 16 | Play routine address (written by RISC-V from PSID header) |

At the selected rate, generates a JSR-style trampoline call to `play_addr`: the bridge
pushes a sentinel return address onto the 6502 stack and sets PC = play_addr. The play
routine ends with `RTS`, returning to the sentinel, which executes `RTI` to restore CPU
state. This matches the standard PSID play convention (play called via JSR, not IRQ).

### Kept unchanged

**`SID`** — reDIP-SID SystemVerilog core. No changes.

**`SIDPeripheral`** — transaction FIFO and phi2 clock divider. No changes. FIFO is now
fed by `Cpu6502Bridge` instead of RISC-V directly.

### Replaced

`USBMIDIHost` + MIDI decode stack → **`USBMSCHost`** from `guh.engines.msc`
(same ULPI bus, same `guh` library).

### Removed

`ScopePeripheral`, `FramebufferPlotter` — dropped. Display is text-only via RISC-V.

---

## 6502 Memory Map

```
$0000–$00FF   Zero page       2KB BRAM  (on-chip, single-cycle)
$0100–$01FF   Stack           2KB BRAM  (same block)
$0200–$07FF   Scratch         2KB BRAM  (same block)
$0800–$CFFF   Tune data       PSRAM     (RDY stall)
$D000–$D3FF   Unused          PSRAM     (RDY stall)
$D400–$D41F   SID registers   → SIDPeripheral FIFO (write-only, no stall)
$D420–$FFEF   Unused          PSRAM     (RDY stall)
$FFF0–$FFF9   Bootstrap stub  PSRAM     (written by RISC-V before each init)
$FFFA–$FFFB   NMI vector      PSRAM     (points to RTI stub)
$FFFC–$FFFD   RESET vector    PSRAM     (points to $FFF0)
$FFFE–$FFFF   IRQ vector      PSRAM     (points to RTI stub)
```

### Bootstrap stub (written by RISC-V at $FFF0, ~10 bytes)

```asm
LDA #<subtune>    ; 0-based: subtune 1 → A=0, subtune 2 → A=1, per PSID spec
JSR <init_addr>   ; call tune's init routine
JMP *             ; spin — RISC-V re-asserts reset after init completes
```

PSRAM `cpu6502_psram_base` = `0x20800000` (8MB into PSRAM). This gives a clean 64KB
window clear of firmware (starts at `0x20000000`), framebuffer (`0x20FB0000`), and
bootinfo (`0x20FC0000 - 4096`).

---

## RISC-V Firmware

**Crate:** `src/top/sid_player/fw/` — new Rust no_std firmware, follows existing
SID bitstream firmware structure.

**FAT32 library:** `fatfs` crate (no_std, block-device backed).

### Startup sequence

1. Wait for `USBMSCHost.status.ready`
2. Read block 0 (MBR) → locate FAT32 partition start LBA
3. Read FAT32 boot sector → compute cluster size, FAT start, root directory start
4. Walk root directory → find first `.SID` file (case-insensitive match)
5. Read all file clusters into PSRAM working buffer
6. Parse PSID v1/v2 header:
   - `load_addr`, `init_addr`, `play_addr`
   - `songs` (total subtune count), `start_song` (1-based default)
   - `speed` flags (bit 0: 0 = 50 Hz, 1 = 60 Hz)
   - `name`, `author`, `copyright` strings (32 bytes each, ASCII)
7. Copy tune payload (bytes after 124-byte header) into PSRAM at `load_addr`
8. Write bootstrap stub + vectors into PSRAM at $FFF0–$FFFF
9. Write `play_rate` to `PlayTimerPeripheral`
10. Clear 6502 BRAM (zero page + stack)
11. Release 6502 from reset (`reset = 0`) → init runs → playback starts
12. Set `irq_enable = 1`

### Subtune change (encoder rotate)

1. `irq_enable = 0`, `reset = 1`
2. Update subtune index (1-based display value, clamp to 1–songs; pass as A = index-1 to init)
3. Clear 6502 BRAM
4. Re-write bootstrap stub with new subtune index
5. `reset = 0` → init runs for new subtune
6. `irq_enable = 1`

### Play/pause (encoder click)

- **Pause:** `irq_enable = 0` — play timer stops, 6502 idles, SID voices fade via envelope release
- **Resume:** `irq_enable = 1` — play timer restarts, 6502 continues from preserved state

### Display

Rendered continuously via `embedded_graphics` to the existing framebuffer:

```
┌─────────────────────────────┐
│ <name>                      │
│ <author>                    │
│ <copyright>                 │
│                             │
│ Song: N / M    [PLAYING]    │
└─────────────────────────────┘
```

`[PLAYING]` / `[PAUSED]` indicator reflects `irq_enable` state.

---

## Resource Estimates

### LUT (ECP5 25F, 24k total)

| Component | Est. LUTs |
|---|---|
| VexiiRiscv | ~10,000 |
| SID core (reDIP-SID) | ~3,500 |
| Video PHY + framebuffer DMA | ~2,500 |
| USBMSCHost (guh) | ~2,500 |
| PSRAM controller | ~1,500 |
| Cpu6502 (arlet, no debug outputs) | ~2,500 |
| Cpu6502Bridge + PlayTimerPeripheral | ~500 |
| Audio codec + encoder + misc | ~500 |
| **Total estimate** | **~23,500** |

Within the 24k budget. First cut if PnR fails timing: reduce FIFO depths.

### BRAM (ECP5 25F, 56 EBR ≈ 126KB total)

| Use | Size | EBR blocks |
|---|---|---|
| VexiiRiscv mainram | 16KB | 8 |
| 6502 BRAM (zero page + stack + scratch) | 2KB | 1–2 |
| FIFOs (SID, USB, misc) | ~4KB | 2–3 |
| **Total** | **~22KB** | **~12–13** |

43 EBR blocks free — comfortable.

---

## Risks

| Risk | Mitigation |
|---|---|
| LUT timing closure on 25F | Build baseline first; reduce FIFO depths if needed |
| FAT32 edge cases (fragmented files, FAT16 drives) | `fatfs` crate handles these; root-dir-only scan for now |
| PSID tunes loading above $CFFF | Cpu6502Bridge forwards $D000–$D3FF to PSRAM; only $D400–$D41F is intercepted |
| Play trampoline stack correctness | Validate with 10+ known-good HVSC tunes during bring-up |
| USB drive spin-up latency | USBMSCHost watchdog already handles up to 10s |

---

## File Layout

```
gateware/src/top/sid_player/
    __init__.py
    top.py              ← SIDPlayerSoc, Cpu6502, Cpu6502Bridge, PlayTimerPeripheral
    fw/
        Cargo.toml
        src/
            main.rs     ← startup, subtune/pause handling, display
            usb_msc.rs  ← USBMSCHost CSR driver + block reader
            fat.rs      ← fatfs integration
            psid.rs     ← PSID header parser
```

Arlet `cpu.v` added as a dependency under `gateware/deps/` (similar to `gateware/deps/sid/`).
