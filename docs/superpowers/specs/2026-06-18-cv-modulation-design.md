# CV Modulation of SID Parameters (sid_player_sw)

Date: 2026-06-18
Status: Approved (brainstorming) → implementation planning

## Goal

Let the three eurorack CV inputs modulate live SID playback:

- **CV1 (input jack 0)** → global filter **cutoff** offset.
- **CV2 (input jack 1)** → **pulse-width** offset, all 3 voices.
- **CV3 (input jack 2)** → progressive **voice muting** (channel matrix).

The feature is **entirely firmware** (`sid_player_sw/fw/src/`). No gateware change,
no PAC regeneration, no menu/UI. Input jack 3 (`sample_i3`) is unused.

## Key decisions (from brainstorming)

- **Combine mode = Offset/modulate**, not replace. 0V = the tune plays untouched;
  CV pushes the parameter around the tune's own automation.
- **CV2 = pulse width only** (the discrete waveform-select bits don't offset
  cleanly), applied to **all 3 voices**.
- **CV3 = progressive drop**: 0V all on → mute V3 → V2 → V1 (4 zones), with
  hysteresis. Muting is done **via SID register writes** (firmware), not a
  gateware mixer.
- **Engagement = auto on patch-detect**: a CV affects the SID only while its jack
  is physically patched (read the `jack` CSR). Unpatched ⇒ clean passthrough,
  zero writes.
- **Voltage convention**: CV1/CV2 **bipolar** (±5V, 0V = no change); CV3
  **unipolar** (0→+5V steps through the 4 zones, ≤0V = all on).

## Data path

The 4 pmod input jacks are read by firmware as **calibrated signed ASQ (Q1.15)**
samples via the existing `PMOD0_PERIPH.sample_i0..3` CSRs (sign-extended to 32
bits). `PMOD0_PERIPH.jack` (8-bit) gives per-jack insertion detect (bits 0–3 =
input jacks). `PMOD0_PERIPH.info.counts_per_mv = 1 << (ASQ.f_bits - 13) = 4`, i.e.
**4000 counts/volt**. These CSRs are always instantiated by
`eurorack_pmod.Peripheral`, so they already exist in the PAC — no `--pac-only`.

The injection point is the **TIMER0 `play_tick` ISR** (where the tune's frame
writes already drain to the chip via the backpressured `sid_write_bp`). CV
overrides are applied *after* that drain, so the CV-derived value wins until the
tune's next write to the same register.

Per frame:
1. Tune's captured writes drain as today **and** mirror into a new
   `sid_shadow: [u8; 0x20]` (the tune's base intent per register).
2. Read `sample_i0/1/2` + `jack` (4 CSR reads).
3. `let writes = cvmod.compute(&sid_shadow, [cv0,cv1,cv2], jack_bits);`
4. Drain `writes` through the same `sid_write_bp` (respects FIFO `writable`
   backpressure — no swallowed-write race).

## `CvMod::compute` — the mapping (pure function, host-testable)

`compute(&sid_shadow, dirty: u32, cv_raw: [i32; 3], jacks: u8) -> heapless::Vec<(u8,
u8), N>` has **no hardware access**, so the entire mapping is unit-testable on the host.
`CvMod` owns the mutable state (slew accumulators, per-register last-emitted
cache, per-CV prev-patched flags).

Each enabled CV is **slew-limited** by a 1-pole integer EMA
(`acc += (raw - acc) >> K`, K ≈ 3) to kill zipper noise. `compute` works in raw
counts; depth constants are expressed per-volt and converted once
(`counts/volt = 4000`).

### CV1 — cutoff offset (bipolar)
- `base = (shadow[0x15] & 7) | (shadow[0x16] << 3)` → 11-bit (0–2047).
- `offset = cv1_slewed * CUTOFF_CTS_PER_V / 4000`. Default
  `CUTOFF_CTS_PER_V ≈ 410` → ±5V ≈ ±2050 ≈ full range.
- `final = clamp(base + offset, 0, 2047)`; emit `0x15 = final & 7`,
  `0x16 = final >> 3`.

### CV2 — pulse-width offset (bipolar), all 3 voices
- Voice PW reg pairs: V1 `(0x02, 0x03)`, V2 `(0x09, 0x0A)`, V3 `(0x10, 0x11)`.
- Per voice: `base = shadow[pwlo] | ((shadow[pwhi] & 0xF) << 8)` → 12-bit (0–4095).
- `offset = cv2_slewed * PW_CTS_PER_V / 4000`. Default `PW_CTS_PER_V ≈ 400` →
  ±5V ≈ ±2000 (≈ ±half range; stays musical, doesn't slam the silent 0/4095 rails).
- `final = clamp(base + offset, 0, 4095)`; emit the 2 PW regs per voice.

### CV3 — progressive mute (unipolar, 0→+5V, 4 zones, hysteresis)
- Voice control regs: V1 `0x04`, V2 `0x0B`, V3 `0x12`.
- `zone = floor(volts / 1.25)` clamped 0–3, with a ±0.12V deadband at each
  boundary (carry previous zone inside the band) so noisy CV doesn't chatter.
- Zones → muted voices: `0:none, 1:{V3}, 2:{V3,V2}, 3:{V3,V2,V1}`.
- Mute mechanism (per muted voice): emit `ctrl = shadow[ctrl_reg] & 0x0F` —
  clears waveform-select bits 4–7, keeps gate/sync/ring/test (bits 0–3) so the
  tune's envelope/gate timing is undisturbed and unmuting restores cleanly.
- **Open implementation item**: validate on host_render that zero-waveform =
  silence on **both** 6581 and 8580. If the 6581 floating-DAC leaks, fall back
  to forcing the **TEST bit (bit 3)** on the muted voice instead.

### Change-detection (real-time guard)
`compute` only emits a write when the register's desired final value differs from
what the chip currently holds. The catch: the tune's own writes drain to the chip
**before** the override each frame, so a register the tune wrote this frame holds
the tune's base value, not compute's last override. So compute is told which
registers the tune wrote this frame via a **dirty mask** (`u32`, bit r set if the
tune wrote SID reg r): the "current chip value" of reg r is `shadow[r]` (base) if
dirty, else compute's own `last_emit[r]`. It emits iff desired ≠ current. A static
CV into a tune that holds that register steady costs **0 writes/frame**; a CV that
modulates a register the tune also rewrites every frame re-asserts every frame
(correct). Worst case (all 3 sweeping) ≤ 2 (cutoff) + 6 (PW) + 3 (mute) = 11
writes/frame — small, and only while actively moving. Combined with patch-gating,
the feature is ~free when nothing is plugged in (protects the fast-CIA-tune
real-time budget).

The dirty mask is built in `play_tick` from this frame's `cpu.memory.writes`
(the same loop that mirrors writes into `sid_shadow`), then passed to `compute`.

## Edge cases & restore

- **Unpatch (falling edge of a jack bit):** emit a one-shot restore of that CV's
  targets to the shadow base (cutoff/PW → tune value; muted voices' control regs
  restored). Then contribute nothing until re-patched. Prevents a stale offset or
  stuck-muted voice.
- **Tune reload (`reload_tune`):** reset `CvMod` (clear slew accumulators,
  invalidate last-emitted cache, mark all CVs unpatched). Pairs with zeroing the
  shadow alongside the existing `sid_reset()`.
- **Clamping:** every final value is clamped to its bit-width; offsets can't wrap
  a register.
- **Pause:** unchanged (codec mute). CV writes still flow harmlessly to a muted
  output; nothing is stale on resume.

## `sid_shadow`

`sid_shadow: [u8; 0x20]` lives alongside `Playback`. Kept correct by two paths:
- Every drained tune write `(reg, val)` also stores `sid_shadow[reg] = val`
  (one array store per write — negligible).
- Zeroed in lockstep with the existing `sid_reset()` on every tune (re)load, so a
  fresh tune starts from a clean base.

## Files touched

- `fw/src/player.rs` — `sid_shadow`, `CvMod` struct + `compute`, host tests.
- `fw/src/main.rs` — 4 CSR reads + `compute` call + drain in `play_tick`;
  `CvMod` reset in `reload_tune`.

No gateware, no PAC, no menu/UI.

## Testing & verification

- **Host unit tests** (`cd fw && cargo test --target x86_64-unknown-linux-gnu
  --lib`): offset math + clamping at the rails; CV2 across all 3 voices; CV3 zone
  boundaries with hysteresis (sweep up and down, assert no chatter in the
  deadband); patch-gating (unpatched ⇒ no writes); unpatch restore one-shot;
  change-detection (static CV ⇒ 0 writes after the first).
- **6581/8580 mute check** via host_render on a dumped write-stream: confirm
  zero-waveform = silence on both models; switch to the TEST-bit fallback if 6581
  leaks.
- **HW**: LFO→CV1 (cutoff sweep), LFO→CV2 (PWM on pulse voices), ramp/env→CV3
  (voices drop progressively); unplug each and confirm clean restore. Run a fast
  CIA tune (e.g. *A Drop of Blue*) with all 3 patched and confirm no new dropped
  notes (audio-priority guard).
