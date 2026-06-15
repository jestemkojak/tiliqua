# Remove the Paced-Replay Fixed Anchor (`play_tick`) — Design

**Date:** 2026-06-15
**Status:** Approved (design); implementation plan to follow.
**Findings basis:** `2026-06-15-sid-recent-changes-cost-benefit-findings.md`.

## Problem

Each TIMER0 frame, `play_tick` (`fw/src/main.rs:164-211`) emulates the 6502 with `player::call`
(which **captures** the frame's SID writes into `cpu.memory.writes`, stamped with frame-relative
6502 cycles — `player.rs:61-74`), then **replays** them to the SID via a **busy-wait spin loop**
that paces each write to its "real 1 MHz spacing," anchored at a fixed offset (half the play
period):

```rust
let offset = PLAY_PERIOD.load(...) / 2;
let c_mid = timer.counter();
let lead = c_start.wrapping_sub(c_mid);
let (t0, base) = if lead < offset { (c_start, offset) } else { (c_mid, 0) };
for w in pb.cpu.memory.writes.iter() {
    let target = base + w.cycle * 60;
    loop { let c = timer.counter();
           if c > t0 { bailed = true; break; }      // period boundary
           if t0 - c >= target { break; } }          // <-- spin until the write's slot
    sid_write(w.reg, w.val);                          // sid_write: NO backpressure
}
```

**The justification is unproven.** The comment says the fixed anchor prevents emulation-duration
jitter from leaking into inter-frame write spacing and "re-rolling the SID envelope (ADSR delay
bug) phase." But the dropped-notes investigation built a reSID-faithful envelope model, drove it
with Commando's real write stream, and found **0 delayed / 0 weak notes of 2274 even at ±1 ms anchor
jitter** — the mechanism does not occur. The checkpoint record states the §4 A/B
(replay-vs-no-replay) "was deferred" and never run.

**The cost is real.** The spin **busy-waits inside the TIMER0 critical section** for up to ~½ the
play period, doing nothing. For the throughput-bound 200 Hz case (*A Drop of Blue*, ~5 ms budget
already overrunning at ~6.3 ms) that is exactly where real-time headroom must not be wasted. The
only *demonstrated* value of pacing is keeping `sid_write` (which has no backpressure) from
overflowing the depth-16 FIFO on a burst — a simpler problem already solved elsewhere by
`sid_write_bp`.

## Goal / Acceptance

- The fixed-anchor spin-loop is removed; the frame's captured writes are delivered by a
  **backpressured unpaced drain** (no per-write busy-wait on a timer target).
- Commando playback is **unchanged** (HW listen — no new drops/glitches; matches the current
  build).
- The *A Drop of Blue* 200 Hz drift is **improved or unchanged** (less ISR spinning = more 6502
  headroom).
- Firmware still builds; host tests still pass.
- Reversible: this is the deferred §4 A/B made permanent in favour of direct drain; if a future tune
  proves to need pacing, the decision is revisited.

## Approach

Replace the anchor/offset/`t0`/`base`/`target`/spin block in `play_tick` with a straight
backpressured drain of the captured writes:

```rust
if !pb.paused && pb.play_addr != 0 {
    let _ = player::call(&mut pb.cpu, pb.play_addr, 2_000_000);
    for w in pb.cpu.memory.writes.iter() {
        sid_write_bp(w.reg, w.val);   // poll `writable` -> never overflows the FIFO
    }
}
```

Rationale:
- **`sid_write_bp` (existing, `main.rs:102-110`)** replaces `sid_write`: it polls the FIFO's
  `writable` and so cannot overflow on a >16-write burst — which is the only thing the pacing was
  protecting against. The FIFO + its 1-per-phi2 (~1 MHz) drain provides the spacing for free; the
  spin only happens when the FIFO is genuinely full (rare, bounded at 100 000 polls), which is
  strictly *less* ISR busy time than today's per-frame anchor spin.
- **No explicit clear:** `call()` clears `cpu.memory.writes` at entry every frame
  (`player.rs:61`), so the next frame starts empty; the drain just consumes this frame's writes.
- **`call()`'s capture + cycle-stamping stays** — it is consumed by the host analysis probes and is
  harmless on hardware. Only the *delivery* path in `play_tick` changes.

### Cleanup

- `PLAY_PERIOD` keeps its TIMER0-reload role; its **anchor-offset role disappears** — update the
  doc comment (`main.rs:146-148`) to drop "the replay anchor is derived from it."
- Remove the now-dead locals (`offset`, `c_mid`, `lead`, `t0`, `base`, `bailed`) and the long
  anchor comment; `c_start` is no longer needed unless kept for an eff/UART metric (drop if unused).

## Components / Files

- `gateware/src/top/sid_player_sw/fw/src/main.rs` — `play_tick` body (the replace above);
  `PLAY_PERIOD` doc comment.
- `gateware/src/top/sid_player_sw/CLAUDE.md` — the "Play rate / replay" note should reflect that
  PLAY writes are now drained backpressured (not paced at a fixed anchor).
- No gateware change.

## Testing & Verification

1. **Firmware host tests:** `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib` — must
   still pass. (They exercise `call`/`init`/`PsidBus`, *not* `play_tick`, so they confirm the build
   and capture path are intact but cannot validate the delivery change — see honesty note.)
2. **Build firmware:** `cd gateware && pdm sid_player_sw build --fw-only`.
3. **HW listen A/B (the real gate):** flash, then compare against the current build:
   - **Commando (50 Hz):** no new dropped notes / stutter / glitch; sounds identical.
   - **A Drop of Blue (PAL CIA 200 Hz):** measure cumulative drift vs the websid reference
     (`wav_compare.py`); expect it to shrink or stay flat, never worsen.

**Honesty note:** `play_tick` is hardware-ISR code with no automated test; correctness here rests on
the HW listen A/B. This is acceptable because (a) the change only swaps the *delivery* timing, which
the envelope model showed Commando is insensitive to, and (b) it is trivially reversible if the A/B
regresses.

## Risks

- A tune whose envelopes *are* sensitive to inter-frame write phase could in principle regress
  (disproven for Commando, not proven for all tunes) → mitigated by the HW A/B and reversibility.
- Backpressure spinning still occurs inside the critical section on >16-write bursts, but it is
  bounded and strictly less than the current per-frame anchor spin.

## Out of Scope

- Writing the SID *during* `call()` (eliminating the capture Vec entirely) — a larger refactor; the
  drain approach is the minimal change.
- Increasing the transaction FIFO depth (a separate gateware tradeoff).
- The decimator / FIR timing work (separate specs).
