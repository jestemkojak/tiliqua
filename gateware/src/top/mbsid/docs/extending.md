# MBSID-on-Tiliqua — Extending the Project

How to add features without breaking the properties that make the port
trustworthy. Read [architecture.md](architecture.md) first.

## The three hard rules

1. **Never edit vendored `mios32/` C++.** It's GPL upstream at a pinned
   commit; local edits are invisible to a fresh clone and unreviewable.
   If upstream behavior must change, interpose at link time (M4 hooked the
   non-virtual `bankSave` stub this way) or absorb it in the
   `fw/csrc/mios32_shim/` facade.
2. **Reuse upstream, don't reimplement.** If the engine already has the
   feature (SysEx, banks, knobs, parameters, ensembles, ASID), plumb it
   through the shim. New synthesis/modulation logic in Rust is a design
   smell — firmware samples inputs and calls engine entry points.
3. **Oracle first.** Any change touching the shim, facade, or engine build
   must keep `host_oracle/run_oracle.sh` bit-exact (28/28) *before* any
   FPGA work. Extend the oracle when you extend the shim.

And the process rule: **write a milestone spec before non-trivial code**
(interfaces + acceptance tests + a hardware checklist), in the style of
`M2`–`M5`. It makes implementation mechanical and reviewable.

## Recipe: add a parameter to the Edit card

Purely firmware — no shim, no gateware.

1. Add a `ParamDesc` row to `LEAD_PARAMS` in `fw/src/params.rs`. Encodings
   available: `Enc::Byte { shift, mask }` (sub-byte fields), `Enc::Wide12`
   (12-bit LE pair, e.g. pulsewidth), `Enc::Cutoff11` (11-bit split that
   preserves the FIP flag bit).
2. Respect the **mirror invariant**: voice-region rows (`0x60..0xC0`)
   mirror `addr + 0x30` (voice n → n+3, Right SID); filter rows
   (`0x54..0x60`) mirror `+6`. The unit tests enforce this and the
   Lead-region bounds — update the row-count assertion.
3. Offsets are the `sid_patch_t` `.L` view (`MbSidStructs.h` in the
   vendored tree): globals `0x50–0x53`, filters @ `0x54`/`0x5A`, voices
   @ `0x60` (16 bytes each), LFOs @ `0xC0` (5 bytes each).
4. `cargo test` on host; check the row still fits `MENU_H` scrolling.

## Recipe: add a CV modulation target

1. Extend the target enum in `fw/src/cv.rs`/`fw/src/menu.rs` and its
   label.
2. Route it to an **existing engine entry point** — `mbsid_knob_set`,
   `mbsid_par_set` (parSet common block addresses), or the note machine.
   If the engine entry point isn't exposed yet, see the shim recipe below.
3. Keep the 8-bit deadband (`CvState::tick`'s `last8` dedup) unless the
   target genuinely needs more resolution — it's what keeps CV noise from
   spamming the engine at 1 kHz.
4. Bump the settings record if the enum encoding changes:
   `fw/src/settings_store.rs` (magic `"MBS5"`) — bump the **version** byte
   so old records decode to defaults instead of misinterpreting.

## Recipe: add a shim/FFI function

1. Add the `extern "C"` function to `fw/csrc/mbsid_shim.cpp` (and its
   header). Keep all state in the existing statics — no allocation.
2. Update **all callers in the same change**: `fw/src/mbsid_sys.rs` (both
   the real `riscv32` extern block and the host cfg-stub) and **both**
   oracle drivers (`host_oracle/shim_driver.cpp`, plus `oracle.cpp` if the
   reference side needs the same stimulus). `extern "C"` means no
   compiler check catches a signature drift.
3. Add an oracle sequence or check that exercises it; re-run
   `run_oracle.sh` to the full green bar.
4. Remember MIDI-shaped functions take the **real channel** as first arg.

## Recipe: add a CSR / gateware change

1. Prefer extending `top/sid`'s peripherals in an **opt-in** way (M4's
   `forward_sysex`/`with_sysex` flag is the pattern) so `top/sid` and
   other bitstreams are unaffected.
2. After the Amaranth change: `pdm mbsid build --pac-only` **before**
   `--fw-only` — the firmware cannot see new registers otherwise.
3. Full build; verify post-route `sync` Fmax in `top.tim` (second
   occurrence of the Max-frequency line). Watch two traps: multiplies by
   runtime signals on the `sync` path must be registered, and anything
   heavy belongs in a slower domain (the 30 MHz `sid` domain exists for
   exactly this reason).
4. LUT budget on the 25F is tight (two reSIDs); check utilization before
   committing to a feature that adds logic.
5. If reading a byte-stream where `0x00` is valid data, copy the
   `sysex_read` valid-bit idiom, not `midi_read`'s read-until-zero.

## The roadmap (deferred items with prior scoping)

From `DESIGN.md §10` and the M-spec non-goals, roughly in order of
leverage:

| Item | Notes / prior scoping |
|---|---|
| **Hardware bring-up of M2–M5** | the actual next step: walk `M4 §7` + `M5 §8` checklists on a real unit |
| **MIDI TX (ACK/DISACK, Patch Read)** | unblocks editor round-trips; needs a MIDI-out path (gateware) + facade route back; scoped as candidate follow-on in `M4 §8` |
| **Per-engine Edit tables** | `params.rs` is table-driven on purpose; add Bassline/Drum/Multi tables + engine dispatch in `menu.rs` |
| **Wave-sequencer / fuller MBSID UI** | the `top/macro_osc` `opts`/`ui`/`draw` framework is the reference pattern; note M5 deliberately kept the hand-rolled menu |
| **Ensemble dumps** | upstream `type 0x70` is a TODO in the vendored engine itself — check upstream first |
| **ASID protocol** | `MbSidAsid` is already linked-but-unused; needs a SysEx-stream route + validation |
| **Multiple user banks** | `patch_store.rs` is one 128-slot bank; flash window has room, SysEx bank byte already carries the index |
| **Drum-engine crash fix** | upstream bug (`(MbSidDrum*)1` sentinel); the clean fix is an upstream PR, not a local patch |
| **Wear leveling for the user bank** | current store is torn-write-safe but writes slots in place |

## Extending validation

- New engine-visible behavior → new oracle sequence in
  `host_oracle/sequences/` + a check in `run_oracle.sh`. For anything
  wavetable-dependent, the sequence must run **past ~4.1 s** (the clock
  AUTO-mode threshold) and include a discriminating negative check —
  shorter sequences silently no-op the WT path.
- New firmware logic → host unit tests (`cargo test --target
  x86_64-unknown-linux-gnu --lib`); keep modules host-pure where possible
  (`regdiff` is the model).
- New hardware-facing behavior → add a line to the milestone's hardware
  checklist; that's the only tier that tests ctors/ISR/CSR/flash/CV.

## Documentation duties

When a feature lands: update `../README.md` (user-visible surface),
`../CLAUDE.md` (new gotchas), the milestone spec's status header, and the
relevant pages in this folder. If a code comment cites a design doc, that
doc must be committed (not under the gitignored `docs/superpowers/`).
