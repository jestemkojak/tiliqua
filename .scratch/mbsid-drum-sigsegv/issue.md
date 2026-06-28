---
title: "Drum engine SIGSEGV in MbSidWtDrum::tick() ~4182ms after patch load (MASTER clock)"
status: needs-triage
created: 2026-06-28
---

## Description

In `MbSidWtDrum::tick()`, the pointer `mbSidDrumPtr` becomes `(MbSidDrum*)1` approximately
4182ms after loading a Drum patch when no external MIDI clock is present (MASTER clock mode).
This sentinel value bypasses the `== NULL` guard but is not a valid pointer, causing a SIGSEGV
on the next dereference.

### Root cause

`mbSidDrumPtr` is set to the sentinel value `(MbSidDrum*)1` during a clock-mode state
transition inside the engine. The existing `if (mbSidDrumPtr == NULL)` guard does not catch
this case, so the stale/corrupt pointer is dereferenced on the next `tick()` call.

### Trigger conditions

- Engine loaded with any Drum patch (e.g. factory rows A033–A036, patch indices 32–35)
- MASTER clock mode (default when no external MIDI clock/sync signal is present)
- Approximately 4182ms after patch load — the pointer corruption occurs at t≈4182ms

### Workaround (oracle gate)

`host_oracle/sequences/seq_drum.txt` ends at t=4181ms, staying just under the corruption
threshold. The oracle gate (`run_oracle.sh`) therefore exits 0. This is a deliberate
workaround; the underlying bug is NOT fixed.

### Hardware impact

On hardware this will crash the VexiiRiscv firmware approximately 4.18 seconds after the user
loads any Drum patch in MASTER clock mode. No recovery without a reset.

### Recommended fix direction

Option A (minimal, in shim): Add a guard in `mbsid_shim.cpp` or `MbSidWtDrum::tick()` that
checks `ptr > (MbSidDrum*)1` (i.e. sentinel-aware null check) before dereferencing.

Option B (root cause): Investigate why the clock-mode transition sets `mbSidDrumPtr` to the
sentinel `1` instead of `NULL` or a valid pointer, and fix the assignment in the vendor engine
(`MbSidWtDrum.cpp` or its caller).

Option A is lower-risk for the vendored C++ tree; Option B is the correct long-term fix but
requires deeper engine surgery.

## Acceptance criteria

- [ ] `mbSidDrumPtr` sentinel `(MbSidDrum*)1` is handled safely (no SIGSEGV after t>4182ms)
- [ ] `run_oracle.sh` seq_drum test can run beyond 4181ms without crash
- [ ] Hardware: loading a Drum patch and waiting >4.2s does not crash the firmware
- [ ] Fix does not break the oracle byte-identical diff for Drum sequences
