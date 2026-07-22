# MBSID-on-Tiliqua — Documentation

MBSID-on-Tiliqua runs the **MIDIbox SID v3** (MBSID) sound engine on the
Tiliqua Eurorack module: a RISC-V softcore executes the original mios32 C++
synthesis engine, which drives **two cycle-accurate reSID (MOS 8580) cores**
in gateware for true stereo dual-SID output, played live over MIDI (TRS or
USB host) with CV modulation, an on-device menu, patch banks, and SysEx
patch upload.

This folder is the narrative documentation. It complements — and links into —
the authoritative per-milestone design specs that live one directory up.

## Reading guide

| You are… | Start with |
|---|---|
| A **musician / end user** who wants to play it | [User Guide](user-guide.md) |
| A **developer** new to the codebase | [Architecture](architecture.md), then [Developer Guide](developer-guide.md) |
| Debugging or planning around known constraints | [Limitations & Known Issues](limitations.md) |
| Adding a feature | [Extending the Project](extending.md) |

## Contents

- **[user-guide.md](user-guide.md)** — hooking up MIDI/audio/CV, the sound
  engines and their MIDI channel map, the three menu cards (Main / CV Mod /
  Edit), patch banks, saving patches, uploading patches over SysEx,
  troubleshooting.
- **[architecture.md](architecture.md)** — how the whole thing works:
  gateware (SoC, SID peripherals, clocking), firmware (ISR, register diff,
  menu, flash stores), the vendored C++ engine and its FFI shim, and the
  host-oracle validation strategy.
- **[developer-guide.md](developer-guide.md)** — cloning, fetching the GPL
  engine, building, flashing, running the three test tiers, and the
  development workflow rules (what to re-run after which kind of change).
- **[limitations.md](limitations.md)** — by-design limitations (no MIDI TX,
  USB is host-only, Lead-only editing) and known upstream issues (Drum-engine
  clock crash, Multi voice-allocation surprise), plus resource budgets.
- **[extending.md](extending.md)** — the roadmap, plus concrete recipes:
  adding Edit-card parameters, adding CV targets, adding shim/FFI functions,
  adding CSRs, and the hard rules (never edit vendored C++; validate against
  the oracle first).

## Authoritative specs (one directory up)

The design specs are the source of truth for interfaces, milestones, and
acceptance criteria. This folder explains; those decide.

| Doc | Milestone |
|---|---|
| [`../DESIGN.md`](../DESIGN.md) | M1 — Lead engine, mono, MIDI-played; roadmap in §10 |
| [`../M2_DUAL_SID.md`](../M2_DUAL_SID.md) | M2 — stereo dual-SID (full 6-oscillator fidelity) |
| [`../M3_PATCH_BANKS.md`](../M3_PATCH_BANKS.md) | M3 — read-only factory bank, MIDI Program Change |
| [`../M4_USER_PATCH_BANKS.md`](../M4_USER_PATCH_BANKS.md) | M4 — writable user bank, save UI, SysEx upload |
| [`../M5_MENU_CARDS_CV_MOD.md`](../M5_MENU_CARDS_CV_MOD.md) | M5 — menu cards, CV modulation, on-device patch edit |
| [`../CLAUDE.md`](../CLAUDE.md) | Developer gotcha reference (kept current) |
| [`../README.md`](../README.md) | Short project overview + quick start |

## Status (2026-07-03)

All five milestones are **implemented and validated on the host**: the
firmware/engine combination is bit-exact against the upstream engine
(host oracle, 28/28 sequences across all four sound engines), host unit
tests pass (41/41), and the full bitstream builds with timing closure
(`sync` 61.76 MHz PASS). M1–M5 (Lead mono, stereo playback, patch banks,
SysEx upload, menu/CV) are all confirmed on hardware — see the hardware
checklists in the M4/M5 specs.

## Licensing

The Tiliqua gateware/firmware in this repo is CERN-OHL-S. The vendored MBSID
engine (`mios32/`) is **GPL** and deliberately **not** checked into this
repository — `./fetch-mios32.sh` clones it at a pinned commit. Linking it
into the firmware makes a *distributed* bitstream's firmware GPL (fine for
personal and open use). See [limitations.md](limitations.md#licensing).
