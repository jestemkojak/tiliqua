# MBSID-on-Tiliqua — Limitations & Known Issues

Split into: (1) by-design limitations you should not "fix" without a
milestone decision, (2) known upstream engine issues, (3) resource budgets,
(4) validation status.

## 1. By-design limitations

### No MIDI output — ACK/DISACK is swallowed
The firmware has no route to any MIDI output; the SysEx facade discards
the engine's acknowledge replies. Consequence: editors/librarians that
wait for a per-patch ACK before sending the next dump **stall or time
out**. Workaround: scripted fire-and-forget sends (`amidi`, `sendmidi`).
Adding MIDI TX is a documented candidate follow-on (`M4 §8`).

### USB-C is a MIDI *host* port, not a device port
By hardware/gateware design (`USBMIDIHost` — drives VBUS, enumerates the
plugged-in device). Tiliqua will never appear in a PC's `lsusb`/`amidi -l`,
and a PC↔Tiliqua USB cable carries no MIDI either way (two hosts can't
enumerate each other). PC-scripted workflows must use the TRS input via a
class-compliant USB-MIDI interface. This is not a bug; don't file it.

### Fixed per-engine MIDI channel maps
Lead/Drum on ch 1, Bassline ch 1–2 (split at note 60), Multi ch 1–6. The
map is the upstream engine's `updatePatch` behavior, not configurable per
patch from the firmware.

### Edit card is Lead-only, curated subset
Only Lead-engine patches get parameter rows (~32 curated params out of
~500 significant patch bytes). Bassline/Drum/Multi show a placeholder. The
full editing surface remains SysEx from the MIDIbox SID Editor. The
parameter-table design leaves room for per-engine tables later
([extending.md](extending.md)).

### Unsaved edits are volatile, no confirmation prompt
Loading any other patch — menu browse **or inbound MIDI Program Change** —
silently discards unsaved Edit-card changes. The `*` in the title bar is
the only warning. Accepted UX trade-off (`M5`).

### SysEx subset
Implemented: Patch **Write** (RAM Write = audition-only; Bank Write to
bank 1 = persisted). Not implemented / not validated: Patch Read
(dump-to-editor), Ensemble dumps (upstream TODO), ASID; Parameter Write
works via the engine but is unvalidated. Bank-Writes to bank 0 (Factory)
are intentionally ignored — it's ROM.

### CV modulation targets engine parameters, never raw SID registers
Deliberate (M5 decision): the engine owns the register image; raw register
pokes would fight the 1 kHz diff loop. CV resolution is effectively 8-bit
with a deadband; Pitch is semitone-quantized (no continuous glide by
design); Pitch without Gate is inert.

### CV vs. Edit precedence
While a CV assigned to a target is actively changing, its 1 kHz ISR writes
win over an Edit-card change to the same target for the *live* sound; once
the CV settles inside its deadband the Edit value applies. The Edit value
is always what gets Saved. Subtle but intended — documented in the user
guide.

### GPL boundary
The vendored engine is GPL; the repo is CERN-OHL-S. Hence `mios32/` is
gitignored and fetched per-clone, and a *distributed* firmware/bitstream
that links it is GPL. The zetaSID Cortex-M binary is proprietary and is
never touched or disassembled. <a name="licensing"></a>

### Single-context SoC constraints
- **No FP / no atomics** on `riscv32im`: firmware is integer-only;
  ISR/main sharing via `critical_section`.
- **No `mcycle` CSR** (traps → SoC freeze); Timer0 is the only clock.
- The scope/plotter gateware from `top/sid` is stripped
  (`with_scope=False`) to fit the second reSID on the 25F — this bitstream
  has no oscilloscope display.

## 2. Known upstream engine issues (do not fix in vendored code)

### Drum engine SIGSEGV ≈4.2 s after load (no external MIDI clock)
`MbSidWtDrum::tick()` dereferences a sentinel pointer `(MbSidDrum*)1`
about 4.18 s after loading a Drum patch when the clock is in MASTER mode
(no external MIDI clock arriving). Oracle sequences end before the window;
on hardware, run an external MIDI clock or reload before ~4 s. Tracked in
`.scratch/mbsid-drum-sigsegv/issue.md`.

### Clock AUTO mode freezes wavetables until ~4.1 s
`MbSidClock` AUTO mode sits in MIDI-slave (wavetables frozen) until
~4095 ms, then falls back to the internal BPM master. Any test that needs
the WT to actually step must run past ~4.1 s — shorter sequences silently
no-op the WT path. Same threshold as the Drum crash above.

### Multi engine: repeated notes alternate L/R in blocks of 3
With a patch whose `voice_asg` is 0 ("all voices"), note-ons round-robin
all 6 physical oscillators (voices 0–2 = Left SID, 3–5 = Right), so
retriggering one note cycles 3 notes left, 3 right, forever. Reproduced
bit-exact on the upstream engine — **by upstream design, not a port bug**.
Fix, if wanted, is patch-side (`voice_asg` left-/right-only).

## 3. Resource budgets

| Resource | Budget | Measured |
|---|---|---|
| Main RAM (BRAM) | 0x8000 (32 KB) | `.bss` ≈ 6.9 KB; peak stack **4016 B** of ~25.8 KB region (hardware probe, post-M4) → ~21.8 KB true headroom |
| `sync` timing @ 60 MHz | must PASS | 61.76 MHz PASS (current reference) |
| FPGA | LFE5U-**25F** only (no 45F board) | second reSID fits only because scope gateware is stripped; LUT headroom is tight — new gateware features need a budget check |
| SID FIFO | depth 16 per SID, φ2 = 1 MHz | diff-based writes keep it shallow; full-image writes would overflow |
| User bank flash | `0xF00000..0xF80000`, 128 × 4 KiB slots | header-after-payload gives torn-write safety, not wear leveling |

Measurement gotchas: `llvm-size`'s default summary merges
`.bss`/`.heap`/`.stack`; the `.stack` *section* size is the linker's
leftover allocation, not usage. Real stack usage needs the on-hardware
paint-and-scan probe (`M4 §6f`).

## 4. Validation status (2026-07-03)

**Green (host):** oracle 28/28 bit-exact across all four engines +
differential + 128-patch sweep + SysEx equivalence/rejection; 41/41 host
unit tests; full bitstream build with timing PASS. M1 (Lead, mono) was
confirmed on hardware.

**Not yet validated on hardware:** stereo playback (M2), patch banks (M3),
SysEx upload & user-bank save (M4), menu cards / CV modulation / patch
edit (M5). The checklists live in `M4 §7` and `M5 §8`. Until those are
walked, treat hardware behavior of these features as untested.

Also inherently untestable on the host: static-ctor execution
(`.init_array` walk), ISR timing, CSR/FIFO plumbing, flash writes, CV
ADC scaling, display rendering.
