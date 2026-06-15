# Remove the Paced-Replay Fixed Anchor (`play_tick`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `play_tick`'s fixed-anchor busy-wait SID-write replay with a simple backpressured drain, freeing 6502 real-time budget without changing playback.

**Architecture:** The TIMER0 ISR currently emulates a PLAY frame, then spins on the Timer0 down-counter to deliver each captured SID write at a fixed per-frame anchor + "1 MHz spacing." That anchor guarded against an ADSR-phase jitter that was later disproven (0 affected notes at ±1 ms), while its busy-wait burns up to ~½ the play period inside the critical section. We delete the anchor and instead drain the frame's captured writes via the existing `sid_write_bp` (which polls the FIFO's `writable`), so the depth-16 transaction FIFO + its 1-per-phi2 drain provide the spacing and bursts can't overflow.

**Tech Stack:** Rust (`riscv32im`, `no_std`), the `mos6502` crate, `critical_section`, the gateware `Timer0`/`SIDPeripheral` PAC. Firmware-only change; no gateware.

**Spec:** `docs/superpowers/specs/2026-06-15-remove-paced-replay-anchor-design.md`

> **Honesty note:** `play_tick` and `PLAY_PERIOD` live in `fw/src/main.rs` (the binary), which the host `--lib` tests do **not** compile. There is no automated test for the ISR delivery path. The compile gate is therefore the **firmware build** (`--fw-only`); functional correctness rests on the **hardware listen A/B** in Task 3. This is the deferred §4 A/B made permanent — do not fake a unit test for it.

---

### Task 1: Remove the anchor; drain writes backpressured

**Files:**
- Modify: `gateware/src/top/sid_player_sw/fw/src/main.rs` — `play_tick` (currently lines 162-212), `PLAY_PERIOD` static (146-148), `set_play_period` (155-160)

- [ ] **Step 1: Rewrite `play_tick`**

Replace the **entire** `play_tick` function (the current body summons `timer`/`c_start` and runs the offset/`t0`/`base`/`bailed` spin loop calling `sid_write`) with this version — it drops `timer`/`c_start` (now unused) and the whole anchor block, and drains via `sid_write_bp`:

```rust
/// TIMER0 ISR body: run one PLAY frame on the software 6502. Real-time work
/// lives here (not the UI loop) so menu redraws can never starve the audio.
fn play_tick() {
    // Count every tick (even while paused — the timer keeps firing) so the UI
    // loop can pace its repaints. load/store, not fetch_add: riscv32im has no
    // atomic RMW; single-writer (this ISR).
    PLAY_TICKS.store(PLAY_TICKS.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
    critical_section::with(|cs| {
        let mut g = PLAYBACK.borrow_ref_mut(cs);
        if let Some(pb) = g.as_mut() {
            if !pb.paused && pb.play_addr != 0 {
                let _ = player::call(&mut pb.cpu, pb.play_addr, 2_000_000);
                // Drain this frame's captured writes to the SID, backpressured so
                // a >16-write burst cannot overflow the depth-16 transaction FIFO.
                // The FIFO's 1-per-phi2 (~1MHz) drain provides the inter-write
                // spacing; we no longer busy-wait to a fixed per-frame anchor. The
                // ADSR-phase jitter that anchor guarded against was disproven (0
                // affected notes at ±1ms), and the spin wasted the real-time budget
                // that 200Hz tunes need. See docs/superpowers/specs/
                // 2026-06-15-remove-paced-replay-anchor-design.md.
                for w in pb.cpu.memory.writes.iter() {
                    sid_write_bp(w.reg, w.val);
                }
            }
        }
    });
}
```

- [ ] **Step 2: Remove the now-dead `PLAY_PERIOD` static**

`PLAY_PERIOD` was read only by the deleted anchor, so it is now write-only. Delete its doc comment + declaration (currently lines 146-148):

```rust
/// Current play period in sync cycles (TIMER0 reload), ISR-visible: the replay
/// anchor offset is derived from it. Written via `set_play_period` only.
static PLAY_PERIOD: AtomicU32 = AtomicU32::new(0);
```

(Leave `PLAY_TICKS` and its doc directly below untouched — it is still read by the UI loop. `AtomicU32` stays imported because `PLAY_TICKS` uses it.)

- [ ] **Step 3: Simplify `set_play_period`**

Drop the `PLAY_PERIOD.store`; keep the function (4 callers) as a thin TIMER0-reload wrapper. Replace the doc + body (currently lines 155-160):

```rust
/// Set the play rate: program the TIMER0 reload (`period` sync cycles; the timer
/// is a down-counter). Called on tune/subtune (re)load.
fn set_play_period(timer: &mut Timer0, period: u32) {
    timer.set_timeout_ticks(period);
}
```

- [ ] **Step 4: Compile gate — build the firmware**

This is the only build that compiles `main.rs` (host `--lib` tests don't). It reuses the existing bitstream/PAC (no gateware/CSR changed):

Run: `cd /home/pawel/code/tiliqua/gateware && pdm sid_player_sw build --fw-only`
Expected: builds successfully, and **no `unused variable` / `dead_code` / `never read` warnings** for `timer`, `c_start`, or `PLAY_PERIOD` (their removal is the point — a leftover warning means something wasn't fully deleted). If a warning appears, find the straggler reference and remove it.

- [ ] **Step 5: Regression guard — host tests**

Run: `cd /home/pawel/code/tiliqua/gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: PASS (these exercise `player`/`psid`/`sid_scan`/`partition` — they don't touch `play_tick`, but confirm the capture/`call` path and the rest of the lib still build and pass after the edit).

- [ ] **Step 6: Commit**

```bash
cd /home/pawel/code/tiliqua
git add gateware/src/top/sid_player_sw/fw/src/main.rs
git commit -m "sid_player_sw: drain PLAY writes backpressured, drop paced-replay anchor

The fixed-offset 1MHz-spacing replay busy-waited inside the TIMER0 critical
section to defeat an ADSR-phase jitter that was disproven (0 affected notes at
±1ms). Replace it with a backpressured drain via sid_write_bp: the depth-16
FIFO's 1-per-phi2 drain provides the spacing and bursts can't overflow. Frees
6502 real-time budget the >=150Hz multispeed tunes need. PLAY_PERIOD (read only
by the anchor) and the play_tick timer/c_start locals are now dead and removed.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Document the delivery change

**Files:**
- Modify: `gateware/src/top/sid_player_sw/CLAUDE.md` — the "Play rate (VBlank / CIA multispeed)" section

- [ ] **Step 1: Add a one-line note**

CLAUDE.md currently has no replay/anchor mention. Append this sentence to the end of the "## Play rate (VBlank / CIA multispeed)" section so the delivery model is documented:

```markdown
- PLAY-frame SID writes are captured into `cpu.memory.writes` by `call()` and
  drained to the chip **backpressured** (`sid_write_bp`, polling the FIFO's
  `writable`) at the end of `play_tick` — NOT paced to a fixed per-frame anchor.
  The depth-16 transaction FIFO + its 1-per-phi2 drain supply the spacing; the
  old fixed-anchor busy-wait was removed (it burned ISR budget to defeat an
  ADSR-jitter that was disproven). See
  `docs/superpowers/specs/2026-06-15-remove-paced-replay-anchor-design.md`.
```

- [ ] **Step 2: Commit**

```bash
cd /home/pawel/code/tiliqua
git add gateware/src/top/sid_player_sw/CLAUDE.md
git commit -m "docs(sid): document backpressured PLAY-write drain (no paced anchor)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Hardware listen A/B (manual gate — run on the Tiliqua)

**Files:** none (hardware verification). This is the real correctness gate; it cannot be automated.

- [ ] **Step 1: Flash the new firmware**

Run: `cd /home/pawel/code/tiliqua/gateware && pdm run flash archive build/sid-player-sw-r5/<archive>.tar.gz`
(`<archive>` = the `--fw-only` build's tarball, named with the git HEAD short hash.)

- [ ] **Step 2: Commando (PAL 50 Hz) — must be unchanged**

Play `docs/Commando.sid`. Expected: **no new dropped notes, stutter, or start glitch** versus the pre-change build — it should sound identical (the envelope model showed Commando is insensitive to the removed write-phase pacing). If it regresses, the change is reverted (Task 1 commit) and re-examined — do not paper over it.

- [ ] **Step 3: A Drop of Blue (PAL CIA 200 Hz) — drift improved or flat**

Play `docs/A_Drop_of_Blue.sid`, capture the voice-mix jack, and compare cumulative tempo drift vs the reSID reference:
Run: `cd /home/pawel/code/tiliqua/gateware/src/top/sid_player_sw && ../../../.venv/bin/python tools/host_render/../wav_compare.py docs/recordings/adrop-host-v0filtered.wav <capture>.wav`
Expected: drift **shrinks or stays flat** vs the pre-change build (less ISR spinning = more 6502 headroom). It must never worsen.

- [ ] **Step 4: Record the verdict**

Update the `sid-player-sw-adrop-200hz-tempo-drift` and `sid-player-sw-playback-vs-timing-attribution` memories with the measured Commando (unchanged?) and 200 Hz drift (delta) results, and whether the paced-replay removal is kept.

---

## Self-Review Notes

- **Spec coverage:** anchor removal + backpressured drain (Task 1 Steps 1,3-5); `PLAY_PERIOD`/locals cleanup (Task 1 Steps 1-3); `sid_write_bp` reuse, no explicit clear because `call()` clears at entry (Task 1 Step 1 + spec); CLAUDE.md doc (Task 2); host tests pass + firmware builds (Task 1 Steps 4-5); HW listen A/B as the gate with the honesty note (Task 3 + header). `set_play_period`'s 4 callers are unchanged (signature preserved).
- **No placeholders:** every code step shows the full replacement text and exact commands.
- **Consistency:** `sid_write_bp(reg, val)` matches its definition (`main.rs:102`); `cpu.memory.writes` matches `call()`'s capture buffer (`player.rs`); `set_play_period(&mut timer, period)` signature unchanged so callers at lines 493/514/555/622 still compile.
