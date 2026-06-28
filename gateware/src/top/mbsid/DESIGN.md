# MBSID-on-Tiliqua — Design Spec (Milestone 1: Lead, mono, MIDI-played)

**Date:** 2026-06-26
**Status:** M1 complete and confirmed on hardware (`mbsid-port`). See §10 for the post-M1 roadmap.
**Scope of this doc:** design only. It defines interfaces and acceptance tests so the
implementation is mechanical. It does not change any existing tiliqua `top`.

---

## 1. Goal & non-goals

**Goal.** Run the MBSID **Lead** sound engine on Tiliqua by compiling the mios32
`midibox_sid_v3` C++ core for the VexiiRiscv softcore and FFI'ing it from the firmware
Rust, driving **one** reSID (mono), played live over MIDI, and validated **bit-exact**
against a host oracle built from the same engine source.

**Why this is needed.** The MBSID v2 `.syx` patches are MBSID v2 voice descriptions
(6 oscillators, 2 filters, 6 LFOs, multi-stage envelopes, mod matrix, wave sequencers).
They contain **zero SID-register data**. A bare reSID core emulates only the dumb
6581/8580. The MBSID firmware engine is the mandatory middle layer that interprets a
patch and writes the SID registers every control tick. the reference hardware is "MBSID v2 based"; the
portable implementation of that engine is the **C++** `midibox_sid_v3/core` family
(MBSID V3 is the C++ rewrite that contains the V2 engine). It already has a JUCE desktop
port that runs the engine against reSID — the same data flow Tiliqua's `sid` top uses.

**Non-goals (explicitly deferred past M1).**
- Stereo / dual-SID (second `SIDPeripheral` + φ2 divider per `docs/DUAL_SID_PLAN.md`).
- Bassline, Drum, and Multi engines.
- FPGA capacity work (LFE5U-45F target / 30 MHz SID clock domain).
- Patch bank storage, wave-sequencer UI, ASID.

---

## 2. Architecture & data flow

The mios32 JUCE port (`apps/synthesizers/midibox_sid_v3/juce/Source/PluginProcessor.cpp`)
already implements exactly the loop Tiliqua needs — it runs the engine at a 1 kHz control
rate and pushes a register image into reSID:

```
MIDI in (CSR FIFO) ─► MbSid::midiReceiveNote / midiReceivePitchBend / midiReceiveCC
                                     │  (updates engine state)
 1 kHz Timer0 ISR ─► MbSid::tick(speedFactor) ─► writes sid_regs_t image (L, R)
                  ─► Rust diffs image vs shadow copy
                  ─► changed regs as (data<<5)|addr ─► SIDPeripheral CSR FIFO
                  ─► reSID core (gateware, φ2 = 1 MHz) ─► codec audio
```

The engine writes a 32-byte `sid_regs_t` register image
(`modules/sid/sid.h`, `SID_REGS_NUM 32`) for each SID-half passed to `MbSid::init()`.
Tiliqua substitutes **gateware** reSID for the JUCE port's **software** reSID: after each
tick, diff the new register image against a shadow copy and enqueue only the changed
registers into the existing `SIDPeripheral` transaction FIFO (`transaction_data =
(data << 5) | addr`, 16-bit; backpressure via `TxnStatus`). Everything from the FIFO
onward — φ2 divider, reSID, audio routing to the codec — is the existing `sid` top,
reused unchanged.

**Mono oscillator collapse (M1 decision).** The Lead engine drives 6 oscillators (3 L +
3 R). For M1 we instantiate **one** SID and feed it the engine's **first three
oscillators** (the "L" register image). The "R" register image is computed but discarded.
This is a deliberate fidelity reduction for the smallest hardware-proving milestone; full
stereo (both images → two SIDs) is the M2/stereo follow-up.

---

## 3. Engine subset to vendor (the FFI target)

Vendor from `mios32/apps/synthesizers/midibox_sid_v3/core/` (upstream, GPL):

- **Top / Lead:** `MbSid.cpp`, `MbSidEnvironment.cpp`, `MbSidSe.cpp`, `MbSidSeLead.cpp`,
  `MbSidPatch.cpp`, `MbSidTables.cpp`, `MbSidSysEx.cpp` (only for `sysexSetPatch`).
- **`components/`:** `MbSidVoice`, `MbSidVoiceQueue`, `MbSidMidiVoice`, `MbSidFilter`,
  `MbSidLfo`, `MbSidEnvLead`, `MbSidWt`, `MbSidArp`, `MbSidMod`, `MbSidSeq`,
  `MbSidClock`, `MbSidRandomGen`.
- **Excluded from M1:** `MbSidSeBassline`, `MbSidSeDrum`, `MbSidSeMulti`, the `*Drum`
  components, `MbSidAsid`.

**C++ freestanding constraints (real, named on purpose).** Target `riscv32imac`. Compile
with `-ffreestanding -fno-exceptions -fno-rtti -fno-threadsafe-statics`, no STL. The
codebase already uses a **custom fixed-size `array<T, N>`** (defined in `MbSidSe.h`, no
heap) — good. But `new` / `std::` tokens appear in several `.cpp` files (`MbSidClock`,
`MbSidArp`, `MbSidMod`, `MbSidVoice`, `MbSidVoiceQueue`, …). See **Spike 0** (§7) — these
are expected to be init-time or removable; any genuine heap use is replaced with static
storage. If a single component resists, that component — not the whole engine — is the
candidate for a Rust rewrite (the fallback path).

---

## 4. The `extern "C"` shim (the only new C++ we author)

One `mbsid_shim.cpp` wraps a single `MbSid` + `MbSidClock` and owns two static
`sid_regs_t`. All engine state lives in `.bss` (no allocator).

```c
void            mbsid_init(void);                       // construct engine; wire sid_regs L/R
int             mbsid_load_patch(const uint8_t *buf512); // sysexSetPatch(sid_patch_t*); 0 = ok
void            mbsid_note_on (uint8_t note, uint8_t vel);
void            mbsid_note_off(uint8_t note);
void            mbsid_pitch_bend(uint16_t bend14);
void            mbsid_cc(uint8_t cc, uint8_t val);
int             mbsid_tick(uint8_t speed_factor);       // returns 1 if regs changed
const uint8_t  *mbsid_regs_l(void);                     // 32-byte image (used by M1)
const uint8_t  *mbsid_regs_r(void);                     // 32-byte image (computed, unused in M1)
```

The 512-byte patch buffer is exactly what this repo's sibling tool `the reference `.syx` encoder script`
emits — same `sid_patch_t` layout. No re-encoding needed.

---

## 5. Rust / gateware integration (this branch)

A new `gateware/src/top/mbsid` top, derived from `top/sid`:

- `build.rs` compiles the vendored C++ subset + `mbsid_shim.cpp` into a static lib via
  the `cc` crate (freestanding flags from §3); an `mbsid-sys` FFI module declares the
  shim signatures.
- The firmware reuses `top/sid`'s SoC verbatim: VexiiRiscv @ 60 MHz, `riscv32im`,
  `SIDPeripheral`, φ2 = 1 MHz divider, MIDI-in CSR FIFO, Timer0. **No CSR changes** are
  expected for M1, so no PAC regeneration is needed (if any CSR changes, regenerate with
  `pdm sid build --pac-only`).
- The Timer0 ISR (already present in `sid/fw`) calls `mbsid_tick`, diffs `mbsid_regs_l()`
  against a 32-byte shadow, and writes changed registers to `SIDPeripheral`. MIDI-in CSR
  feeds `mbsid_note_on/off` / `mbsid_pitch_bend`.
- `mainram_size`: bump from `0x4000` if the engine + framebuffer working set overflows
  (perf review notes ~49 KB BRAM free on the 25F). Decide empirically during bring-up.

Build: `cd gateware && pdm mbsid build` (mirrors `pdm sid build`).

---

## 6. Oracle & validation — the keystone

**Oracle = the mios32 JUCE/standalone port itself** (the same source we FFI). Build it on
PC; instrument its `sidRegs[].ALL[reg]` writes to emit a timestamped register trace for a
fixed `(patch, note-sequence)`. Run the **identical** sequence through the shim built for
the host (same `.cpp` + `mbsid_shim.cpp`, x86) and diff the two register streams — they
must be **byte-identical**. This converts "did ~8k lines of C++ port correctly?" into a
mechanical regression test, runnable entirely on PC before any FPGA work.

- Validate on **≥3 Lead patches** from `a reference patch corpus/` plus `B005 - Avril
  TranceGate.syx` (a known-good Lead patch).
- The wasm reference-engine emulator (driven by the CDP harness in `external reference tooling/`) is an **optional
  secondary cross-check**, not required for M1.
- Because M1 keeps only the L image, the diff compares the **L register stream** of shim
  vs oracle.

---

## 7. Milestones (sequencing inside M1)

0. **Compile spike (gating).** Engine subset + shim build freestanding for `riscv32imac`;
   every `new` / `std::` / `printf` triaged to static storage or removed. Output: a clean
   `libmbsid.a` for the target and a list of any component that needed surgery.
1. **Host oracle.** JUCE-port register-trace harness + host build of the shim; bit-exact
   L-stream diff passes on ≥3 Lead patches.
2. **Gateware bring-up.** `top/mbsid` builds; engine ticks at 1 kHz; at least one register
   write reaches reSID (scope-verified tone on hardware).
3. **MIDI play.** note-on/off + pitch-bend → audible Lead patch on hardware; informal A/B
   against the emulator.

---

## 8. Risks / open items

- **C++ freestanding fights** (Spike 0 outcome). Mitigation: per-component Rust-rewrite
  fallback; the custom `array<T,N>` already avoids the heap in the data path.
- **`updateSpeedFactor` cadence.** M1 fixes the control rate at the JUCE port's **1 kHz**
  so the oracle diff is apples-to-apples; confirm the speed-factor value the port passes
  to `tick()` and mirror it in the ISR.
- **SoC RAM.** Engine state (patch + per-voice/LFO/env/mod/WT runtime) is a few KB atop
  the UI/framebuffer set; bump `mainram_size` if it overflows.
- **Licensing.** MBSID is **GPL**; linking the C++ into the bitstream firmware makes that
  firmware GPL on distribution (fine for personal/open use).

---

## 9. Reference pointers

- Engine source (vendored, reference): `mios32/apps/synthesizers/midibox_sid_v3/core/`
  and `juce/Source/PluginProcessor.cpp` (the oracle harness model).
- `sid_regs_t` / `SID_REGS_NUM`: `mios32/modules/sid/sid.h`.
- Existing register-write path & SoC facts: `gateware/src/top/sid/top.py`
  (`SIDPeripheral`), `gateware/src/top/sid/fw/src/main.rs`.
- Patch decode/encode reference: the MBSID v2 `.syx` format (512-byte `sid_patch_t`
  layout, see §4).

---

## 10. What's next (post-M1 roadmap)

**M1 is complete and confirmed on hardware** (Lead, mono, MIDI-played; §7 milestones 0–3
all done). This section summarises the deferred work, ordered the way the source docs frame
it. None of these have a worked-out spec yet — they are scoped pointers, not commitments.

### M2 — Stereo / dual-SID (the clear next step)

**Now specced in `M2_DUAL_SID.md`** (full 6-osc / dual-filter stereo). Summary below.

§2's mono-collapse was a deliberate fidelity reduction; restoring the discarded **R**
register image to a **second** SID is the headline follow-up. `docs/DUAL_SID_PLAN.md` sketches
the gateware shape (second `SIDPeripheral` + second φ2 divider; ~7k LUTs for 2×SID) — but it
**predates this work and is likely stale** (it targets `top/sid`, assumes a unison/detune
firmware model, and its LUT/timing numbers are unverified against the current tree). Treat it
as a starting sketch to re-validate, not a spec.

For MBSID specifically the firmware change is smaller than that doc's generic "unison/detune"
options: the engine **already computes both L and R images** every tick (§2). M2 = stop
discarding `mbsid_regs_r()`, diff it against a *second* 32-byte shadow, and enqueue to a
second `SIDPeripheral`. No new engine work — purely gateware (2nd SID) + a second diff loop.

**The 30 MHz SID domain is already done** (inherited from `top/sid`): the reSID core +
`SIDPeripheral` already run in the 30 MHz `sid` domain (AsyncFIFO sync→sid, pulse-synced
strobe), defined in `pll.py` across all PLL variants. So the reSID filter muladd is already off
the `sync` critical path; M2 places SID #1 in that *same* domain — no new PLL/CDC work.

**Gated on FPGA capacity** (the real M2 risk, not firmware or `sync` timing):
- A second reSID adds ~7k LUTs (`DUAL_SID_PLAN.md`); the concern is **LUT area / routing
  congestion**, not `sync` Fmax (the filter is already at 30 MHz).
- **Only the 25F (r5) is available** — no 45F board, so it must fit on the 25F via LUT
  reduction. Levers (in order): strip mbsid's inherited-but-unused scope + scope-plotter
  gateware (the firmware drives no scope — `fw/src/main.rs:13`); drop the optional voice-tap
  codec channels; share one phi2 divider between the two SIDs. See `M2_DUAL_SID.md §6`.

### Further deferred (no plan doc yet)

- **Bassline / Drum / Multi engines.** **Done** — all three are validated to the
  oracle bit-exact bar across the 9 non-Lead factory patches, with real per-channel
  MIDI input (Multi multi-timbral across both SIDs). See the all-engines milestone
  spec/plan and `README.md` for the channel map.
- **Patch bank storage.** Read-only ROM-baked factory bank **done** — see
  `M3_PATCH_BANKS.md` (all 128 factory patches selectable over MIDI Program Change).
  Writable user banks (flash) and a browse UI remain deferred.
- **Wave-sequencer / full MBSID UI** (the macro_osc `opts`/`ui`/`draw` pattern is the model).
- **ASID** (`MbSidAsid` — currently excluded from the Lead subset, §3).

### Suggested sequencing

1. **M2 stereo** — restore the R image to a 2nd SID (the 30 MHz domain it needs is already in
   place). Must fit on the 25F via LUT reduction (re-validate `docs/DUAL_SID_PLAN.md` against
   the current tree first; don't take its numbers/firmware model at face value).
2. **Patch banks**, then **UI**, then **additional engines** as appetite allows.
