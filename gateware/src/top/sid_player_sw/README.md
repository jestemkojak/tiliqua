# SID Player (software 6502)

Play Commodore 64 `.SID` tunes on the Tiliqua, straight from a USB stick. The
bitstream emulates a C64's MOS 6502 in software and feeds a cycle-accurate
reSID-class 6581/8580 sound chip, with the three SID voices drawn live on the
oscilloscope display. Eurorack CV inputs can modulate the playing tune in real
time.

---

## Quick start

1. **Build & flash** a bitstream for the chip model you want (see *SID model*
   below):

   ```sh
   cd gateware
   pdm sid_player_sw build --sid-model 8580   # or 6581
   pdm run flash archive build/sid-player-sw-r5/<name>.tar.gz
   ```

2. **Copy `.SID` files** to the **root** of a FAT-formatted USB stick (PSID
   format; see *Supported tunes*).

3. **Plug the stick in.** The first tune loads and starts playing automatically.
   Use the encoder to browse the rest.

---

## Using the player

Everything is driven by the **rotary encoder** (rotate to move, press to
select). The menu lives in the frozen band at the top of the screen; the
oscilloscope traces fill the area below.

### Controls

| Action | Effect |
|--------|--------|
| **Rotate** | Move the selection cursor along the current row |
| **Press** | Toggle *modify* mode on the selected row |
| **Modify + rotate** | Change that row's value (browse files, pick subtune, edit a scope param) |

The menu is organised as two **cards** (pages); row 0 of each card switches
between them:

- **Player card** — `File`, `Song`, `Clock`, `State`
- **Config card** — `Decay`, `Timebase`, `Y-Scale`, `Intensity`, `Hue`, `Rescan USB`

### Player card

| Row | What it does |
|-----|--------------|
| **File** | Enter browse mode, rotate through the `.SID` files found on the stick, press to load the highlighted one (press again on the current tune to cancel). The playing file is marked `*`. |
| **Song** | For multi-subtune collections, select which subtune plays. Changes take effect live. |
| **Clock** | Choose the SID clock standard: `AUTO` (follow the PSID header), `PAL`, or `NTSC`. Affects pitch/tempo. |
| **State** | Toggle **PLAYING ↔ PAUSED**. Pause mutes the output cleanly (held notes resume correctly on un-pause). |

### Title / metadata lines

- **Title line** reads `SID PLAYER (8580)  <tune name>` — the model in
  parentheses is the chip **baked into the bitstream** you flashed. The tune's
  author is shown dimmed below it.
- **Metadata line** shows the **tune's declared model** (from its PSID header),
  the clock standard, the timing source, and the detected play rate — e.g.
  `6581  PAL  CIA  200 Hz`.

If the bitstream model and the tune's declared model disagree, the timbre
(filter character, combined waveforms) won't be authentic — flash the build that
matches your library. Most classic tunes are 6581.

### File-browser limits

- Only the **first 64 `.SID` files** in the USB root are browsable.
- **Root directory only** — subfolders are skipped.
- Names are shown as FAT **8.3 short names** (e.g. `GYROSC~1.SID`).

Bad or unsupported files (e.g. multi-chip PSID v4) are rejected with
`UNSUPPORTED!` and the current tune keeps playing — they never crash the player.

### Config card

Each voice (V1/V2/V3) plus the final mix is plotted on the scope. Adjust the
look live, and rescan the USB drive here:

- **Decay** — how long traces persist before fading
- **Timebase** — horizontal time per division
- **Y-Scale** — vertical gain
- **Intensity / Hue** — brightness and colour
- **Rescan USB** — re-enumerate the `.SID` files on the stick

Scope settings are **not** saved across reboots. Heavy scope settings compete
with playback for memory bandwidth — if a busy tune starts to stutter, lower the
**Timebase** (the player always prioritises audio over visuals).

### CV / Eurorack modulation

Patch a cable into an input jack and the matching feature engages automatically:

| Input | Effect |
|-------|--------|
| **CV1** | Filter **cutoff** modulation |
| **CV2** | **Pulse-width** modulation |
| **CV3** | Progressive **voice mute** |

Calibration is **4000 counts per volt**. Steady CV adds essentially no overhead;
modulation re-asserts only when the value changes.

---

## Supported tunes

- **PSID format** `.SID` files (the most common kind).
- **PAL and NTSC** tunes — the player reads the clock standard from the header
  and sets true C64 pitch (within ~0.5 cents) automatically. A **Clock** row can
  override it.
- **VBlank and CIA (multispeed)** tunes — play rate (50 Hz up through several
  hundred Hz) is detected from the header / CIA timer.

**Not supported:** multi-SID PSID v4 files, and tunes that rely on illegal /
undocumented 6502 opcodes. Very fast multispeed
tunes (≳200 Hz) can drag on the hardware because the emulated 6502 is the limit.

---

## How it works

Unlike the older `sid_player` (which ran a 6502 in gateware), this player runs
the C64 CPU **in software** on the SoC's RISC-V core. The RISC-V handles USB,
the file system, the menu and the display; the SID chip itself is real gateware.

```
   USB stick (.SID)                            Eurorack CV in
        │                                      (CV1/2/3 jacks)
        ▼                                             │
 ┌─────────────────────────────────────────────┐     │
 │            RISC-V SoC (VexiiRiscv)            │     │
 │                                              │     │
 │  USB MSC ─► FAT32 ─► PSID parse              │     │
 │                       │                      │     │
 │        load 64 KB C64 image into PSRAM       │     │
 │                       │                      │     │
 │   ┌───────────────────▼──────────────────┐   │     │
 │   │  TIMER0 ISR  (fires at the tune rate) │   │     │
 │   │                                       │   │     │
 │   │   software 6502 runs PLAY frame       │   │     │
 │   │   $D400-$D41F writes ──► captured     │   │     │
 │   │   + CV modulation overlay  ◄──────────┼───┼─────┘
 │   └───────────────────┬──────────────────┘   │
 │                       │ SID register writes   │
 └───────────────────────┼──────────────────────┘
                         ▼
              ┌─────────────────────┐
              │  SIDPeripheral CSR  │  depth-16 txn FIFO
              └──────────┬──────────┘
                         ▼
              ┌─────────────────────┐      phi2 ~1 MHz
              │  reSID core (6581/  │◄──── (fractional-N,
              │  8580, gateware)    │       true PAL/NTSC)
              └─────┬──────────┬────┘
            mix     │          │  3 voice taps
                    ▼          ▼
        ┌────────────────┐  ┌──────────────────┐
        │ AudioDecimator │  │  VoiceSmoother   │
        │ (polyphase FIR │  │  (anti-alias)    │
        │  ~1MHz ► 48kHz)│  │       │          │
        └───────┬────────┘  └───────▼──────────┘
                ▼                  scope plotter
          48 kHz codec               (display)
```

**Key points:**

- The CPU is emulated by the third-party **[`mos6502`](https://crates.io/crates/mos6502)**
  Rust crate (v0.9, built `no_std`). The firmware drives it via the crate's
  `Bus` trait — `PsidBus` (`fw/src/player.rs`) — and the NMOS instruction set
  (`Nmos6502`). The crate has **no illegal/undocumented opcodes**; none of the
  common test tunes need them.
- The **64 KB C64 memory image** lives in PSRAM; the software 6502 reaches it
  through the RISC-V data cache. Writes to `$D400-$D41F` are redirected by
  `PsidBus` to the SID hardware instead of memory.
- Playback is driven by the **TIMER0 interrupt** at the tune's real rate (not the
  UI loop), so the menu and display never throttle the music.
- The SID core emits one sample per ~1 MHz clock; a **polyphase FIR decimator**
  resamples that cleanly to the 48 kHz codec (point-sampling would alias). The
  phi2 clock is generated by a fractional divider so PAL and NTSC tunes play at
  true C64 pitch.
- The scope branch runs the raw voice taps through an **anti-alias smoother**
  before plotting — audio output is never compromised for the visuals.

### Custom RISC-V build

This target does **not** use the stock Tiliqua CPU. It selects a dedicated
VexiiRiscv variant, **`tiliqua_rv32im_bigcache`** (`top.py`, defined in
`src/vendor/vexiiriscv/vexiiriscv.py`) — identical to the standard
`tiliqua_rv32im` but with **4× larger L1 caches** (512 B → 2 KB each, for both
instruction fetch and data: 16 sets × 2 ways × 64 B line). The software 6502
thrashes the default 512 B caches against the 64 KB PSRAM tune image, running
~10× slower than a real 1 MHz 6502 and smearing SID writes across the frame
(dropped notes); the bigger caches recover most of that throughput.

Because the CPU flags differ from upstream, this variant has its **own
pre-generated VexiiRiscv netlist** cached in the repo — regenerating it (only
needed if the CPU flags change) uses SpinalHDL; see
`docs/sid_player_sw_dropped_notes_and_riscv_rebuild.md`.

For the deeper implementation notes, durable gotchas, and the rationale behind
these choices, see **`CLAUDE.md`** in this directory.

---

## Layout

| Path | Role |
|------|------|
| `top.py` | Top-level gateware (SoC, SID, scope, CSRs) |
| `smooth.py` | Scope-branch voice conditioning (anti-alias + DC-block) |
| `fw/` | RISC-V firmware (6502 emulation, USB/FAT, PSID, menu, CV) |
| `sid/` | Shared SID + audio gateware (with `src/top/sid/`) |
| `tools/` | Audio analysis / host-render debugging utilities |
| `CLAUDE.md` | Developer reference and gotchas |