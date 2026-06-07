# SID Player — status & root-cause record

**Status (2026-06-05): USB file browser with File/Song/State menu.** ✓

A dedicated bitstream that plays PSID files. The arlet verilog-6502 (`cpu.v`) runs
the tune's init/play routines in gateware feeding the reDIP-SID core; VexiiRiscv
handles USB MSC, FAT32, PSID parsing, and the display. Design specs:
`docs/superpowers/specs/2026-06-03-sid-player-design.md`,
`docs/superpowers/specs/2026-06-05-sid-player-usb-file-browser-design.md`.

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

## Video / display

The resolution is **bootloader-detected** (`FIXED_MODELINE = None`,
`"video": "<match-bootloader>"`); on the test rig that's **1280×720** (the video
PLL is fixed at 74.25 MHz = 720p60). Video was never validated during initial
bring-up, and the firmware had been written assuming a fixed **640×480**, which
produced a garbage/flickering display. Fixed (firmware-only, `main.rs`):

- **Resolution-aware layout.** Read `display.size()` for `h_active`/`v_active`
  instead of hardcoding 640×480; the header clear spans `h_active`, and the four
  scope traces (V1/V2/V3/MIX) are stacked evenly below the header from the real
  height (ypos is a *signed offset from centre* — `OffsetMode.CENTER` in gateware).
- **Header text persistence.** The persist peripheral decays the whole
  framebuffer every frame; the gateware re-plots the scope continuously, but the
  text was drawn once on state-change and faded to black. Now the header is
  redrawn **every loop** (full-width clear only on state change).
- **Flicker.** `persist` decay was too fast (`2`) → free-running traces strobed.
  Raised to `10` so successive sweeps overlay into a stable band.
- **Dotted traces.** At audio fs the samples are sparse per pixel (the proper fix
  is gateware upsampling, as in `macro_osc`'s `dsp.Resample` with `n_up=16` — see
  open items). Mitigated firmware-only by scaling the scope down ~50% in both
  axes (`set_xscale(7)`, `set_yscale(Scale2V)`; default shift is 6) and a slower
  `Timebase10ms`, which packs samples denser per pixel.

---

## USB file browser / menu

Encoder-driven three-item menu rendered in the header band:

| Row | Rotate | Press |
|-----|--------|-------|
| **File** | browse filenames (no load) | enter/commit/cancel — loads on commit only |
| **Song** | — | enter modify mode, then rotate changes subtune live |
| **State** | — | toggle PLAYING ↔ PAUSED |

**File row detail:** pressing enters *browse* mode; rotating moves the cursor through all `.SID` short names found at startup (up to 64, `heapless::Vec`). The currently-playing file is marked `*`. Pressing again on the same file cancels (no reload). Pressing on a different file loads it and restarts playback.

**Implementation layers:**
- `sid_scan.rs` — pure, host-testable: `list_root_sids`, `load_sid_by_index`, type aliases `SidName`/`SidList`. Tested via in-memory GPT+FAT images.
- `fat.rs` — hardware wrappers: `list_sids`, `load_sid`; `load_first_sid` is now a thin shim to `load_sid(_, 0, _)`.
- `main.rs` — `load_and_start` helper (init/hot-plug/file-commit share one path); startup enumerates file list; hot-plug re-enumerates on USB plug-in.

**Hot-plug:** if the fallback tune is playing and a USB drive appears, the file list is re-enumerated and index 0 loaded automatically.

---

## Playback rate (VBlank / CIA multispeed)

**Symptom:** some tunes "missed" fast note changes (arpeggios) and fast envelopes
felt wrong. **Root cause:** the play routine was driven by a fixed 50/60 Hz NMI,
and the rate selector misread the PSID header — `psid::is_ntsc` treated the
`speed` field (offset $12) as PAL/NTSC. Per the PSID spec `speed` is *not*
PAL/NTSC: each bit is the song's **timing source** (`0` = VBlank, `1` = CIA #1
timer). CIA-timed tunes are frequently **multispeed** (reprogram the CIA timer to
call play 2–4× per frame = 100–200 Hz). Driven at 60 Hz they play 2–3× too slow,
smearing fast effects. PAL vs NTSC actually lives in the v2 **`flags`** field
(offset $76, bits 2–3), which the parser never read.

**Fix (no new gateware decode):**
- `psid.rs`: parse `flags` → `clock()` (PAL/NTSC); rename the speed-bit reader to
  `is_cia()`; add pure `play_period_cycles(clk_hz, clock, cia, cia_timer)` that
  returns the `PlayTimerPeripheral` divider (VBlank frame rate, or `φ2/(timer+1)`
  for CIA incl. multispeed; `timer==0` → ~60 Hz default). All host-tested.
- `PlayTimerPeripheral` (`top.py`): `control` reduced to `reset`/`irq_enable`;
  new 32-bit `period` register holds the firmware-computed divider (period 0 =
  never, safe pre-init default). Sim stimulus input is now `dbg_period`.
- `main.rs` `load_and_start`: zero $DC04/$DC05 in PSRAM before release; after INIT
  runs, `thrash_l1_cache()` evicts stale lines, read back the CIA Timer A the tune
  programmed, compute the period, and write the `period` CSR. Returns the rate in
  Hz; the header now shows **clock / VBI-or-CIA / Hz**.
- The cache thrash (formerly `flush_6502_image`) now serves *both* directions:
  flushing the written image out, and evicting so the CIA-timer read-back is fresh.

**Hardware-validated** (rate detection): `LEK_DNB` → `PAL CIA 55 Hz` (correct,
sounds right), `My_Day` → `NTSC VBI 59 Hz` (was wrongly 50 Hz before),
`Postcard from Ibiza` → `PAL CIA 300 Hz` (6× multispeed read back correctly). The
on-screen `clock / VBI-or-CIA / Hz` readout matches the SID database in every case.

Known limitation: only CIA timer values written during **INIT** are detected
(tunes that set the timer inside PLAY or via the IRQ vector fall back to the
default). Note: correct *rate detection* does not imply correct *playback* —
high-rate tunes (Postcard at 300 Hz) still play wrong because the 6502 can't
execute the play routine fast enough; see **6502 playback throughput** below.

---

## Root-cause re-investigation (2026-06-06) — distorted/“off” playback

Fresh deep static analysis of *why* tunes still sound wrong (Postcard fully
distorted; Commando “off” — missing notes/effects/ADSR). Goal was to rank
**all** plausible causes by probability, not just continue the throughput
thread. Tooling written for this pass: `gateware/src/top/sid_player/tools/sid_analyze.py`
(recursive-descent + flat 6502 disassembler over the `.sid` images — reports
SID-register reads, illegal opcodes, write targets per tune).

**Frame for the whole problem:** the SID *sound model* is **reDIP-SID**
(`deps/sid`, vk2seb) — a cycle-accurate, reSID-class 6581/8580 core with proper
DAC tables. It is **not** the suspect. Every credible cause is in the
*integration*: clocking, the 6502’s memory throughput, register-write/read
plumbing, and FPGA timing closure. (Web cross-check: normal tunes write ~25
registers at 50/60 Hz, so per-frame register updates — not cycle-exact intra-
frame timing — is what matters for non-digi tunes.)

### Ranked causes (most→least likely to explain the reported symptoms)

1. **FPGA timing NOT closed — `sync` runs at 60 MHz but places at 50.6 MHz
   (~18 % over Fmax).** CONFIRMED in `build/sid-player-r5/top.tim`:
   `Max frequency for clock '$glbnet$clk': 50.61 MHz (FAIL at 60.00 MHz)`
   (LUTs 21790/24288 = 89 %; `dvi_clk` also fails 69.4/74.25). Running ~18 % over
   Fmax means **intermittent setup violations on critical paths** anywhere — 6502
   fetch/ALU, the bridge, SID register writes, PSRAM. Errors are *data-dependent*,
   so they can look **consistent per-tune** (a given tune replays the same
   values/paths each frame). This is the prime suspect for the *general*
   “feels off / occasional missing notes” class (Commando), and it undermines
   every other measurement — **the design must meet timing before any other
   fix can be trusted.** Fixes: (a) cheapest test — drop `sync` to ≤50 MHz
   (must also retune SID `DIVIDE_BY` and `PlayTimerPeripheral(clk_hz=…)`; costs
   ~16 % 6502 throughput); (b) free LUTs to improve routing/Fmax (scope 4→3
   channels, trim plotter/persist); (c) pipeline the worst paths.

2. **6502 throughput — ALL tune code executes from PSRAM; every fetch pays
   HyperRAM latency; no instruction cache.** CONFIRMED: BRAM is only
   `$0000–$07FF` (2 KB, zeropage+stack); the analysed tunes load at `$0800`
   (My_Day), `$0A00` (Gyroscope), `$1000` (8-Bits, LEK, Postcard), `$5000`
   (Commando) — i.e. **100 % of opcode/operand/data fetches are PSRAM reads**
   at ~tens of cycles each. The NMI is **edge-triggered and single-latched**
   (arlet `cpu.v:1215`; one pending NMI survives, but a 2nd timer pulse during
   one `play()` is **lost** — `top.py` `nmi_l` only toggles 0→1 once). So when
   `play()` overruns its budget, play calls are **silently dropped**, not merely
   delayed → at 300 Hz (Postcard, 3.33 ms budget) this is catastrophic =
   “completely distorted.” At 50 Hz (Commando, 20 ms budget) a normal `play()`
   (~2–3 ms estimated) fits comfortably, so throughput is **not** Commando’s
   main problem. Fix: `WishboneL2Cache` (already in `src/tiliqua/cache.py`,
   burst-fill, direct-mapped) on the 6502’s `psram_bus` master → amortise
   latency across the sequential instruction stream. **Caveat (coherency):**
   that cache is *write-back*; the firmware reads `$DC04/$DC05` (CIA timer) back
   from PSRAM after INIT, so a 6502 write sitting dirty in the cache would be
   invisible → CIA-rate detection breaks. Needs a flush-before-readback (or a
   write-through/uncached store path). **Caveat (resources):** design is 89 %
   LUTs and already failing timing — adding a cache may not fit / may worsen #1;
   may require freeing LUTs first (couples to #1). Bigger win: enlarge BRAM or
   run hot code from BRAM.
   - **IMPLEMENTED + measured (2026-06-06):** `Cpu6502Bridge(icache_lines=…)`,
     a small direct-mapped **write-through** read cache (default 8). Correctness:
     cosim + bridge + tune tests all green. Throughput: `src/top/sid_player/tools/measure_cache.py`
     (real arlet under iverilog) shows **2.5–4.3× more work per fixed time** on a
     code-fetch-bound `play()` (SID writes/2M cyc: 5208→12971→16196→22261 for
     cache 0/8/16/32). **HARDWARE BUILD VERDICT — net regression for now:** a
     `cache=8` build *fits* (90 % LUT, +155 LUT/+352 FF over 89 %) **but drops
     `sync` Fmax 50.6→44.8 MHz.** The cache is **not** on the critical path
     (that’s `pmod0.calibrator`’s MULT18X18D + CCU2 carry chain); it congests the
     already-90 %-full device and worsens routing of that path. On a 60 MHz part
     that’s worse, not better. **So the hardware instance is wired
     `icache_lines=0`** (`top.py`); tests keep the default 8 for coverage. This is
     concrete proof that **#1 gates #2**: throughput logic cannot help while the
     device is timing-bound and ~full. Close timing first (free LUTs / lower clk),
     then flip the cache on.

3. **SID clocked at fixed 1.000 MHz, not true φ2.** CONFIRMED:
   `sid/top.py` `DIVIDE_BY = 60` @ 60 MHz → exactly 1.000 MHz vs PAL 985248 Hz
   (1.5 % sharp) / NTSC 1022727 Hz (2.2 % flat). Audible as a slight detune
   (“feels off”), **not** missing notes. Also a tempo/pitch mismatch: firmware
   computes the CIA play period from *true* φ2 while the SID runs at 1 MHz. Fix:
   make `DIVIDE_BY` track the tune’s PAL/NTSC clock via a CSR (firmware already
   knows `clock()` from the v2 flags). Cheap, correctness-improving.

4. **SID register *reads* return PSRAM garbage (latent).** CONFIRMED in code:
   the bridge data mux (`sid_player/top.py:201`) returns the stale
   `captured_word` for any `is_sid_r` read; the SID’s `data_o`/`o_data_o` is
   never wired back to `cpu_DI`. Any tune reading `$D41B` (OSC3) / `$D41C`
   (ENV3) — common for randomness/arpeggio/modulation — would get garbage.
   **NOT triggered by the 6 current tunes:** a flat byte-scan found *zero*
   `$D419–$D41C` read patterns in any of them. Real bug, fix opportunistically:
   route SID `data_o` back through the bridge on SID reads.

5. **Intra-frame SID write cycle-timing requantised by the 1 MHz FIFO drain.**
   Only matters for digi/sample (`$D418` volume) tunes and tight cycle-exact
   effects; web-confirmed irrelevant for normal 25-reg/frame tunes. Low priority
   for the current symptoms.

6. **arlet illegal/undocumented opcodes unimplemented.** arlet decodes them via
   `casex` defaults (effectively wrong). **NOT found** in the reachable code of
   any of the 6 tunes (recursive descent); flat-scan “hits” are data-byte noise.
   Low probability here; if a future tune needs them, swap to an
   illegal-opcode-capable 6502 core.

7. **CIA timer set during PLAY/IRQ (not INIT) → wrong rate** (already a known
   limitation): the readback only sees timers programmed during INIT. Possible
   contributor to a Commando-class “off” if the tune retimes itself in play.

8. **PSRAM contention** — scope plotter + `persist` full-framebuffer DMA steal
   PSRAM bandwidth from the 6502, worsening #2. Plotter isn’t cleanly gated
   (the firmware `scope.set_enabled(false)` experiment didn’t stop its writes).

### Bottom line / recommended order
- **Postcard “distorted”** ← #2 (throughput → dropped NMIs), with #1 contributing.
- **Commando “off”** ← #1 (timing not closed) is the prime suspect; then #3/#7.
- Do **#1 first** (it’s a correctness prerequisite and the cheapest decisive
  test: lower `sync`, rebuild, listen). Then #2 (cache, mind the two caveats),
  then the cheap correctness wins #3 and #4.

---

## Open items

- **6502 playback throughput** (next priority) — heavy or high-rate tunes play
  wrong because the emulated 6502, running its code from PSRAM, can't finish the
  play routine before the next NMI. Evidence:
    - *Commando* (PAL VBI 50 Hz, no SID reads, but **85 RMW writes/frame** into its
      own $5000–$5FFF region): "some notes failed". The only previously-working tune
      (`Gyroscope_3`) does **zero** PSRAM writes, so this path was never stressed.
    - *Postcard* (CIA **300 Hz**, 6×): badly choppy — only 3.33 ms/call budget.
  - **Adaptive bridge window (commit `8ac25f9`) — done, but largely ineffective.**
    The bridge no longer burns a fixed `N=64` window: `advance` now fires on
    `psram_done_r` for PSRAM, a 4-cycle settle for BRAM, full window for SID
    (FIFO-drain safety). Sim shows PSRAM windows drop 64→~9. **On hardware:**
    Postcard *slightly* better, **Commando unchanged**. Conclusion: the fixed
    window was **not** the real cap — real HyperRAM single-word latency (~tens of
    cycles, incl. command/latency/turnaround) is already ≈ the old 64-cycle window,
    so uncapping PSRAM did almost nothing; the small Postcard gain came from the
    BRAM (zeropage/stack) 64→4 speedup. **The true bottleneck is per-fetch HyperRAM
    latency**, paid on every instruction/data byte because the 6502 has no read
    cache.
  - **Real fix (next):** amortise HyperRAM latency across the (sequential)
    instruction stream. Cheapest high-value step: the bridge already reads a full
    **32-bit word** (`captured_word`) but uses one byte and re-fetches for the next
    3 — cache the last word/line and serve sequential byte reads from it (≈4× fewer
    PSRAM reads for code fetch). Better: a small burst-filled line buffer / prefetch.
  - Secondary levers: contention — the scope plotter and `persist` full-framebuffer
    DMA compete for PSRAM. NB the firmware `scope.set_enabled(false)` experiment did
    **not** actually stop the plotter's PSRAM writes (it only stops sample intake;
    the plotter keeps redrawing the held line), so that experiment was inconclusive;
    a clean test needs the plotter master gated in gateware.
- **60 MHz timing not closed** — the `sync` domain places at **~50.5 MHz** but is
  clocked at 60 MHz (~16% over). Clocking above Fmax causes intermittent setup
  violations and is the likely source of the *general* "some songs sometimes miss
  notes" symptom (distinct from the throughput issue above). Closing it on the
  ~92%-full 25F is real work (fewer scope channels / logic trimming).
- **`thrash_l1_cache()` (firmware, `main.rs`, formerly `flush_6502_image`)** — the
  RISC-V writes through its write-back L1 while the 6502 reads PSRAM via a separate
  wishbone master, so coherency is a genuine concern in both directions (image-out,
  CIA-timer read-back-in). The 64 KiB cache-thrash works but is crude — prefer a real
  cache-clean/invalidate if VexiiRiscv gains one. Needs a hardware re-test if changed.
- **Scope voice scatter** (clean traces) — DONE for `sid_player_sw`. The earlier
  `VoiceUpsampler` (macro_osc upsample port) was a **misdiagnosis**: it had no
  effect because the dots aren't display-undersampling (macro_osc's problem) but
  *aliasing*. The three `voiceN_dca` taps are raw ~1MHz reSID outputs that the
  scope point-samples at 48kHz with no anti-alias filter, folding their >24kHz
  content into broadband scatter (the mix channel renders cleanly because it
  already goes through `AudioDecimator`). Since the scope is a visualisation, not
  a measurement, the fix is a cheap multi-pole leaky integrator (`VoiceSmoother`,
  `src/top/sid_player_sw/smooth.py`; adders/shifts only, no DSP/BRAM) on the three
  voice taps **on the scope branch only** — audio outputs keep reading raw
  `voiceN_dca`. The upsampler was removed and `scope_periph` `fs` restored. Build
  fit: TRELLIS_COMB 82% / DSP 11/28 / sync 56.64 MHz post-route (still FAILs 60,
  the pre-existing design-wide timing issue — but lighter than the upsampler it
  replaced: 86%→82% LUT, 15→11 DSP). The original `sid_player` (arlet) top has the
  same raw-voice-tap aliasing if a voice scope is added there.
- **PSRAM RMW writes** (tune storing above `$07FF`): the bridge implements the
  sub-FSM (`RD-FOR-RMW → WRITE → psram_done_r`, replacing the old WRITE→IDLE wedge).
  Correctness is cosim-tested (`test_psram_rmw_preserves_adjacent_byte`: byte-merge
  + read-back) and now exercised on hardware by *Commando* (85 RMW/frame) — it does
  not wedge or corrupt obviously, so the RMW problem is **cost, not correctness**
  (each RMW = serialized read+write ≈ 2× latency, feeding the throughput issue
  above). Still worth a dedicated cosim *tune* that stores above `$07FF`.
- **Debug CSRs** (`sid_writes`, `nmi_count`, `state`, `cpu_ab`, `psram_acks`,
  `ab_changes`) — **removed** once audio + USB were confirmed on hardware (commit
  `ad14d3b`), to reclaim LUTs on the ~95%-full 25F. `PlayTimerPeripheral` now
  exposes `control` (reset/irq_enable) + `period`. The `dbg_reset` /
  `dbg_irq_enable` / `dbg_period` inputs are kept as sim-only test stimulus
  (the CSRs are write-only), tied to 0 in hardware.
