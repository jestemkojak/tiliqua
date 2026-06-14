# SID φ2 Runtime PAL/NTSC Select Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Clock the SID at true C64 pitch — runtime-selectable PAL (985 500 Hz) / NTSC (1 023 000 Hz) via a CSR, auto-picked per tune from the PSID header — replacing the hardcoded 1 MHz divider that makes every tune +1.497 % sharp.

**Architecture:** A new `Phi2Divider` component (fractional-N divider, constants muxed by a 1-bit select) replaces the flat `/60` counter inside `SIDPeripheral`; a new `phi2_sel` RW CSR drives it. The SoC top instantiates two `AudioDecimator`s (one per standard, ratios 32/657 and 16/341) and muxes their outputs. Firmware writes `phi2_sel` from `hdr.clock()` on every tune load, with a `Clock: AUTO/PAL/NTSC` menu row for override.

**Spec:** `docs/superpowers/specs/2026-06-12-sid-phi2-runtime-select-design.md` (committed, approved).

**Tech Stack:** Amaranth (gateware), amaranth.sim + pytest (gateware tests), Rust no_std firmware (`fw/`), svd2rust PAC (regenerated via `pdm sid_player_sw build --pac-only`).

**Key numbers (derived in the spec, re-used throughout):**

| standard | nominal φ2 | chosen φ2 | NCO `P + num/den` (from 60 MHz) | decimator `n_up/m_down` |
|---|---|---|---|---|
| PAL  | 985 248   | 985 500   | 60 + 580/657 (pattern period 40 000 sync cycles = 657 φ2 edges) | 32/657 |
| NTSC | 1 022 727 | 1 023 000 | 58 + 222/341 (pattern period 20 000 sync cycles = 341 φ2 edges) | 16/341 |

**Build/test commands (from CLAUDE.md):**
- Gateware tests: `cd gateware && pdm run pytest tests/<file> -v` (full: `pdm test`)
- Firmware host tests: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib`
- PAC regen (required after any CSR change): `cd gateware && pdm sid_player_sw build --pac-only`
- Full build (~4–5 min): `cd gateware && pdm sid_player_sw build`; artifacts in `gateware/build/sid-player-sw-r5/` (note underscores→hyphens). Achieved Fmax = the **second** `Max frequency for clock '$glbnet$clk'` line in `top.tim`.

---

### Task 1: `Phi2Divider` component (TDD)

A standalone fractional-N divider component, host-testable in amaranth.sim (the select is a plain `In(1)` port here; Task 2 wires the CSR to it).

**Files:**
- Test: `gateware/tests/test_sid_phi2.py` (create)
- Modify: `gateware/src/top/sid/top.py` (add `Phi2Divider` above `SIDPeripheral`, add `from fractions import Fraction` import)

- [x] **Step 1: Write the failing test**

Create `gateware/tests/test_sid_phi2.py`:

```python
# Copyright (c) 2026 Tiliqua contributors
#
# SPDX-License-Identifier: CERN-OHL-S-2.0

"""
Phi2Divider: fractional-N divider for runtime PAL/NTSC SID phi2.

Average rate must be exact over the fractional pattern period:
  PAL  985 500 Hz: 60e6/985500  = 40000/657 -> 657 edges per 40 000 sync cycles
  NTSC 1 023 000 : 60e6/1023000 = 20000/341 -> 341 edges per 20 000 sync cycles
The steady-state edge pattern is periodic with the pattern-period length, so
ANY window of exactly that length (after warmup) contains exactly that many
edges. Defaults (1 MHz, 1 MHz) must behave bit-identically to the old flat /60
counter: every edge exactly 60 cycles apart, regardless of sel.
"""

import unittest

from amaranth import *
from amaranth.sim import Simulator

from top.sid.top import Phi2Divider

PHI2_HZ = (985_500, 1_023_000)
PAL_WINDOW,  PAL_EDGES  = 40_000, 657
NTSC_WINDOW, NTSC_EDGES = 20_000, 341


def _run_divider(phi2_hz, sel_program, window, warmup=2000):
    """Run Phi2Divider; `sel_program` is a list of (tick_to_apply, sel_value)
    applied before warmup counting starts at the LAST entry. Returns
    (edges_in_window, duty) counted over `window` ticks after `warmup` ticks
    past the last sel change."""
    m = Module()
    m.submodules.dut = dut = Phi2Divider(sync_hz=60_000_000, phi2_hz=phi2_hz)
    result = {}

    async def testbench(ctx):
        tick = 0
        for at, sel in sel_program:
            while tick < at:
                await ctx.tick()
                tick += 1
            ctx.set(dut.sel, sel)
        for _ in range(warmup):
            await ctx.tick()
        edges = 0
        high = 0
        for _ in range(window):
            edges += ctx.get(dut.phi2_edge)
            high += ctx.get(dut.phi2)
            await ctx.tick()
        result["edges"] = edges
        result["duty"] = high / window

    sim = Simulator(m)
    sim.add_clock(1 / 60e6)
    sim.add_testbench(testbench)
    sim.run()
    return result["edges"], result["duty"]


def _edge_intervals(phi2_hz, sel, n_edges=50, warmup=500):
    """Tick indices between consecutive phi2 edges."""
    m = Module()
    m.submodules.dut = dut = Phi2Divider(sync_hz=60_000_000, phi2_hz=phi2_hz)
    intervals = []

    async def testbench(ctx):
        ctx.set(dut.sel, sel)
        for _ in range(warmup):
            await ctx.tick()
        last = None
        tick = 0
        while len(intervals) < n_edges:
            if ctx.get(dut.phi2_edge):
                if last is not None:
                    intervals.append(tick - last)
                last = tick
            await ctx.tick()
            tick += 1
            assert tick < (n_edges + 2) * 70, "phi2 edges stopped"

    sim = Simulator(m)
    sim.add_clock(1 / 60e6)
    sim.add_testbench(testbench)
    sim.run()
    return intervals


class Phi2DividerTests(unittest.TestCase):

    def test_pal_rate_exact(self):
        """sel=0 -> exactly 657 edges per 40 000-cycle window (x3 windows)."""
        edges, _ = _run_divider(PHI2_HZ, [(0, 0)], 3 * PAL_WINDOW)
        self.assertEqual(edges, 3 * PAL_EDGES)

    def test_ntsc_rate_exact(self):
        """sel=1 -> exactly 341 edges per 20 000-cycle window (x3 windows)."""
        edges, _ = _run_divider(PHI2_HZ, [(0, 1)], 3 * NTSC_WINDOW)
        self.assertEqual(edges, 3 * NTSC_EDGES)

    def test_duty_near_50(self):
        for sel, window in ((0, PAL_WINDOW), (1, NTSC_WINDOW)):
            _, duty = _run_divider(PHI2_HZ, [(0, sel)], window)
            self.assertTrue(0.40 < duty < 0.60,
                            f"sel={sel}: duty {duty:.3f} not ~50%")

    def test_switch_settles(self):
        """Flip PAL->NTSC mid-stream (mid-period, tick 5003); after warmup the
        rate must be exactly NTSC — no stuck state."""
        edges, _ = _run_divider(PHI2_HZ, [(0, 0), (5_003, 1)], NTSC_WINDOW)
        self.assertEqual(edges, NTSC_EDGES)

    def test_default_back_compat_flat_60(self):
        """Default phi2_hz=(1MHz,1MHz): num=0 -> constant /60, identical to the
        old DIVIDE_BY=60 counter for BOTH sel values."""
        for sel in (0, 1):
            intervals = _edge_intervals((1_000_000, 1_000_000), sel)
            self.assertEqual(intervals, [60] * len(intervals),
                             f"sel={sel}: not a flat /60")


if __name__ == "__main__":
    unittest.main()
```

- [x] **Step 2: Run test to verify it fails**

Run: `cd gateware && pdm run pytest tests/test_sid_phi2.py -v`
Expected: FAIL (ImportError: cannot import name 'Phi2Divider' from 'top.sid.top')

- [x] **Step 3: Implement `Phi2Divider`**

In `gateware/src/top/sid/top.py`, add to the imports near the top of the file:

```python
from fractions import Fraction
```

Then add this component immediately above `class SIDPeripheral` (after `class SID`):

```python
class Phi2Divider(wiring.Component):

    """
    Fractional-N divider generating the SID phi2 square wave + edge strobe.

    The period alternates between ``P`` and ``P+1`` sync cycles, steered by a
    small accumulator (classic fractional-N), so the *average* rate is exactly
    ``sync_hz / (P + num/den)`` for the selected standard. phi2 edges land on
    the sync-clock grid -> +/-1 sync cycle (16.7ns) deterministic jitter;
    sidebands are <-80dB in the audio band and further averaged by the
    decimation FIR downstream — inaudible.

    All divider state updates only at phi2 cadence (~1MHz), so nothing here
    joins the 60MHz critical path (the reSID filter muladd).

    With the default ``phi2_hz=(1_000_000, 1_000_000)`` the fraction is 0 and
    the divider degenerates to the old flat /60 counter, bit-identical for
    both ``sel`` values — non-opted-in SID targets are unchanged.
    """

    def __init__(self, sync_hz=60_000_000, phi2_hz=(1_000_000, 1_000_000)):
        self._consts = []
        for hz in phi2_hz:
            frac = Fraction(sync_hz, hz)
            p = frac.numerator // frac.denominator
            self._consts.append((p, frac.numerator - p * frac.denominator,
                                 frac.denominator))
        super().__init__({
            "sel":       In(1),   # 0 = phi2_hz[0], 1 = phi2_hz[1]
            "phi2":      Out(1),  # ~50% duty square wave
            "phi2_edge": Out(1),  # 1-sync-cycle pulse at each period wrap
        })

    def elaborate(self, platform):
        m = Module()

        (p0, n0, d0), (p1, n1, d1) = self._consts
        max_p = max(p0, p1) + 1  # +1: the long (P+1) period

        # Selected constants (sel is quasi-static: a CSR the firmware writes
        # on tune load; the muxes feed only phi2-cadence registers).
        base = Signal(range(max_p + 1))
        num  = Signal(range(max(n0, n1) + 1))
        den  = Signal(range(max(d0, d1) + 1))
        m.d.comb += [
            base.eq(Mux(self.sel, p1, p0)),
            num .eq(Mux(self.sel, n1, n0)),
            den .eq(Mux(self.sel, d1, d0)),
        ]

        counter = Signal(range(max_p + 1))
        period  = Signal(range(max_p + 1), init=p0)
        # Invariant acc < den in steady state; a sel switch can leave
        # acc >= new den, which the subtract branch drains within a few
        # periods (period reads base+1 meanwhile — harmless transient).
        acc = Signal(range(2 * max(d0, d1)))

        # `>=` (not `!=`) so a period shrink across a sel switch can never
        # strand the counter past the wrap point.
        with m.If(counter >= period - 1):
            m.d.sync += counter.eq(0)
            # Fractional accumulator chooses the next period length.
            with m.If(acc + num >= den):
                m.d.sync += [acc.eq(acc + num - den), period.eq(base + 1)]
            with m.Else():
                m.d.sync += [acc.eq(acc + num), period.eq(base)]
        with m.Else():
            m.d.sync += counter.eq(counter + 1)

        m.d.comb += [
            self.phi2.eq(counter > (period >> 1)),
            self.phi2_edge.eq(counter == period - 1),
        ]
        return m
```

Note: `wiring`, `In`, `Out`, `Signal`, `Mux`, `Module` are already imported in this file (used by `SIDPeripheral`). Only `Fraction` is new.

- [x] **Step 4: Run test to verify it passes**

Run: `cd gateware && pdm run pytest tests/test_sid_phi2.py -v`
Expected: 5 passed.

- [x] **Step 5: Commit**

```bash
cd /home/pawel/code/tiliqua
git add gateware/src/top/sid/top.py gateware/tests/test_sid_phi2.py
git commit -m "sid: Phi2Divider fractional-N divider for runtime PAL/NTSC phi2"
```

---

### Task 2: Wire `Phi2Divider` + `phi2_sel` CSR into `SIDPeripheral`

**Files:**
- Modify: `gateware/src/top/sid/top.py` (`SIDPeripheral.__init__` ~line 205, CSR classes ~line 197, `elaborate` ~lines 256–268)
- Regression: `gateware/tests/test_sid_periph.py` (NO changes — it must pass as-is, proving back-compat)

- [x] **Step 1: Add the `Phi2Sel` CSR register class**

In `gateware/src/top/sid/top.py`, after `class BuildModel` (~line 199), add:

```python
    class Phi2Sel(csr.Register, access="rw"):
        """SID phi2 standard select: 0 = phi2_hz[0] (PAL), 1 = phi2_hz[1] (NTSC).
        Reset 0. Firmware writes this on every tune load (AUTO follows the
        PSID header) — see sid_player_sw fw/src/main.rs."""
        sel: csr.Field(csr.action.RW, unsigned(1))
```

- [x] **Step 2: Extend `__init__`**

Change the signature (~line 205):

```python
    def __init__(self, *, transaction_depth=16, sid2_define=True,
                 sync_hz=60_000_000, phi2_hz=(1_000_000, 1_000_000)):
        self._sid2_define = sid2_define
        self._sync_hz  = sync_hz
        self._phi2_hz  = phi2_hz
```

Bump the CSR address space (the old 5-bit window is full: 8 regs × 4 bytes = 0x20) and add the register after `txn_status`:

```python
        regs = csr.Builder(addr_width=6, data_width=8)
```

```python
        self._txn_status  = regs.add("txn_status",    self.TxnStatus(),   offset=0x1C)
        self._phi2_sel    = regs.add("phi2_sel",      self.Phi2Sel(),     offset=0x20)
```

Add the readback port to the signature dict in `super().__init__({...})` (after `"usb_midi_cfg_id": Out(4),`):

```python
            # phi2 standard currently selected (mirrors the phi2_sel CSR) —
            # read by the SoC top to mux the per-standard audio decimators.
            "phi2_sel":        Out(1),
```

- [x] **Step 3: Replace the divider block in `elaborate`**

Replace lines 256–268 (the `DIVIDE_BY = 60` block through the `phi2`/`phi2_edge` comb assignments) with:

```python
        # Fractional-N phi2 divider, runtime-selectable standard (phi2_sel
        # CSR). Defaults make this a flat /60 (~1MHz) exactly as before.
        m.submodules.phi2_div = phi2_div = Phi2Divider(
            sync_hz=self._sync_hz, phi2_hz=self._phi2_hz)
        m.d.comb += [
            phi2_div.sel.eq(self._phi2_sel.f.sel.data),
            self.phi2_sel.eq(self._phi2_sel.f.sel.data),
        ]
        phi2      = phi2_div.phi2
        phi2_edge = phi2_div.phi2_edge
```

Everything downstream (`self.sid.bus_i.phi2`, `audio_strobe`, the `startup` reset window, transaction pop) keeps using `phi2`/`phi2_edge` unchanged.

- [x] **Step 4: Run the regression + new tests**

Run: `cd gateware && pdm run pytest tests/test_sid_periph.py tests/test_sid_phi2.py -v`
Expected: ALL pass with **zero changes to `test_sid_periph.py`** (it builds `SIDPeripheral()` with defaults → flat /60; its `DIVIDE_BY = 60` phase calibration must still hold). If it fails, the back-compat degeneration is broken — fix the divider, do not touch the test.

- [x] **Step 5: Commit**

```bash
cd /home/pawel/code/tiliqua
git add gateware/src/top/sid/top.py
git commit -m "sid: phi2_sel CSR drives Phi2Divider in SIDPeripheral (default = old /60)"
```

---

### Task 3: SoC top — dual decimators + output mux; parametrize decimator test

**Files:**
- Modify: `gateware/src/top/sid_player_sw/top.py` (constants near top; `__init__` ~line 214; `elaborate` ~lines 283–298)
- Modify: `gateware/tests/test_sid_audio.py` (parametrize over the two new rates)

- [x] **Step 1: Update the decimator test first (it should fail only on runtime length, then pass — the DUT code is unchanged)**

In `gateware/tests/test_sid_audio.py`, replace the two test methods:

```python
# Runtime-selectable SID phi2 rates (must match PHI2_HZ_PAL/NTSC in
# top/sid_player_sw/top.py). Decimator ratios: 32/657 and 16/341.
PHI2_RATES = (985_500, 1_023_000)


class SidAudioTests(unittest.TestCase):

    def test_passband_tone_survives(self):
        """A 1kHz tone (well within the audio band) passes ~unattenuated at
        both phi2 rates. warmup covers the PAL FIR group delay (3296 taps ->
        ~1648 input samples)."""
        for fs_in in PHI2_RATES:
            rms = _measure_tone_rms(1_000, fs_in=fs_in, n_in=5500, warmup=2600)
            assert rms > 0.15, (
                f"fs_in={fs_in}: 1kHz tone unexpectedly attenuated: rms={rms:.4f}")

    def test_aliasing_tone_rejected(self):
        """A 100kHz tone would alias into the audio band under naive 48kHz
        sampling; the anti-alias FIR must reject it at both phi2 rates."""
        for fs_in in PHI2_RATES:
            rms_pass = _measure_tone_rms(1_000, fs_in=fs_in, n_in=5500, warmup=2600)
            rms_alias = _measure_tone_rms(100_000, fs_in=fs_in, n_in=5500, warmup=2600)
            assert rms_alias < 0.25 * rms_pass, (
                f"fs_in={fs_in}: 100kHz tone not rejected: alias_rms={rms_alias:.4f} "
                f"passband_rms={rms_pass:.4f}")
```

(`_measure_tone_rms` already takes `fs_in`/`n_in`/`warmup` parameters — unchanged.)

Run: `cd gateware && pdm run pytest tests/test_sid_audio.py -v`
Expected: 2 passed (slower than before — bigger FIRs; a few minutes is normal). This proves `AudioDecimator` needs no code changes for the new rates.

- [x] **Step 2: Add the rate constants and thread params into `SIDPeripheral`**

In `gateware/src/top/sid_player_sw/top.py`, add at module level (after the imports):

```python
# Runtime-selectable SID phi2 rates. "Clean" near-PAL/NTSC targets chosen so
# the AudioDecimator stays small (ratios 32/657 and 16/341; FIR tap ROM =
# 5*m_down): exact 985248/1022727 Hz would need 51k/1.7M-tap FIRs (infeasible).
# Pitch error vs a real C64: +0.44 / +0.46 cents — far below the ~5 cent
# audibility threshold. See
# docs/superpowers/specs/2026-06-12-sid-phi2-runtime-select-design.md.
PHI2_HZ_PAL  = 985_500
PHI2_HZ_NTSC = 1_023_000
```

In `__init__` (~line 214), replace:

```python
        self.sid_periph = SIDPeripheral(sid2_define=(self.sid_model == "8580"))
```

with:

```python
        self.sid_periph = SIDPeripheral(
            sid2_define=(self.sid_model == "8580"),
            sync_hz=self.clock_settings.frequencies.sync,
            phi2_hz=(PHI2_HZ_PAL, PHI2_HZ_NTSC))
```

- [x] **Step 3: Dual decimators + mux in `elaborate`**

Replace lines 283–289 (single `audio_decim` instantiation + feed) with:

```python
        AudioDecimator = self._import_sid_audio().AudioDecimator
        fs_out = self.clock_settings.audio_clock.fs()
        # One decimator per phi2 standard (the FIR ratio is fixed at
        # elaboration by fs_in); both always run off the same strobe, the
        # phi2_sel CSR muxes which output is heard. The unselected one sees a
        # ~3.8%-off fs_in — harmless, its output is ignored.
        m.submodules.audio_decim_pal = decim_pal = AudioDecimator(
            fs_in=PHI2_HZ_PAL, fs_out=fs_out)
        m.submodules.audio_decim_ntsc = decim_ntsc = AudioDecimator(
            fs_in=PHI2_HZ_NTSC, fs_out=fs_out)
        for decim in (decim_pal, decim_ntsc):
            m.d.comb += [
                decim.i.valid.eq(self.sid_periph.audio_strobe),
                decim.i.payload.as_value().eq(self.sid_periph.last_audio_left >> 8),
            ]
        audio_out = Signal(dsp.ASQ)
        with m.If(self.sid_periph.phi2_sel):
            m.d.comb += audio_out.eq(decim_ntsc.o)
        with m.Else():
            m.d.comb += audio_out.eq(decim_pal.o)
```

And in the `pmod0` block (~line 297), replace:

```python
            pmod0.i_cal.payload[3].as_value().eq(audio_decim.o.as_value()),
```

with:

```python
            pmod0.i_cal.payload[3].as_value().eq(audio_out.as_value()),
```

(The scope mix channel taps `pmod0.i_cal.payload[3]` at ~line 353, so it follows the mux automatically — no scope-branch change. `dsp` is already imported in this file.)

- [x] **Step 4: Elaboration smoke test**

There is no fast full-elaboration unit test for the SoC; rely on the gateware test suite + the full build in Task 6. Run the related suites now:

Run: `cd gateware && pdm run pytest tests/test_sid_phi2.py tests/test_sid_periph.py tests/test_sid_audio.py -v`
Expected: all pass.

- [x] **Step 5: Commit**

```bash
cd /home/pawel/code/tiliqua
git add gateware/src/top/sid_player_sw/top.py gateware/tests/test_sid_audio.py
git commit -m "sid_player_sw: dual PAL/NTSC decimators muxed by phi2_sel"
```

---

### Task 4: Firmware — PAC regen, auto-select on tune load, Clock menu row

**Files:**
- Modify: `gateware/src/top/sid_player_sw/fw/src/main.rs` (helpers near `reload_tune` ~line 218; state vars ~line 449; rotate-edit match ~line 501; button match ~line 569; labels/values ~lines 694–725; metadata ~lines 666–746; `rows_in` ~line 248; boot path ~line 423)
- Generated (not committed): `pac/src/generated/` via `--pac-only`

- [x] **Step 1: Regenerate the PAC**

Run: `cd gateware && pdm sid_player_sw build --pac-only`
Expected: completes; the PAC now has `SID_PERIPH.phi2_sel()` with field `sel()`. (PAC output is gitignored — nothing to commit from this step.)

- [x] **Step 2: Add the helpers**

In `fw/src/main.rs`, immediately above `fn reload_tune` (~line 214), add:

```rust
/// Drive the gateware phi2 divider (0 = PAL 985.5kHz, 1 = NTSC 1.023MHz).
/// Like the scope CSRs this register is independent of the SID ISR state, so
/// no critical section is needed; the worst race with reload_tune is
/// last-write-wins of two writes derived from the same header.
fn set_phi2(clock: psid::Clock) {
    let ntsc = clock == psid::Clock::Ntsc;
    unsafe { (*pac::SID_PERIPH::ptr()).phi2_sel().write(|w| w.sel().bit(ntsc)) };
}

/// Effective SID clock standard for the Clock menu row:
/// 0 = AUTO (follow the PSID header), 1 = force PAL, 2 = force NTSC.
fn effective_clock(clock_sel: usize, hdr: &psid::PsidHeader) -> psid::Clock {
    match clock_sel {
        1 => psid::Clock::Pal,
        2 => psid::Clock::Ntsc,
        _ => hdr.clock(),
    }
}
```

- [x] **Step 3: Auto-select in `reload_tune`**

Change the signature (~line 218) to take the override state:

```rust
fn reload_tune(tune_buf: &[u8], len: usize, hdr: &mut psid::PsidHeader,
               subtune: u16, clock_sel: usize) -> Option<(u32, u32)> {
```

and just before the final `period.map(...)` (~line 239), add:

```rust
    if period.is_some() {
        // Successful load: retune the SID phi2 to this tune's standard
        // (or the forced override). Pitch follows the same header source
        // that already drives tempo.
        set_phi2(effective_clock(clock_sel, hdr));
    }
```

Update the three call sites to pass `clock_sel` (hot-plug ~line 481, subtune edit ~line 524, file load ~line 583): each becomes e.g.

```rust
reload_tune(tune_buf, n, &mut hdr, start, clock_sel)
```

- [x] **Step 4: Boot path + state var**

Add to the menu-state declarations block (after `let mut hue: u8 = 0;`, ~line 457):

```rust
    // Player-card Clock row: 0=AUTO (follow PSID header), 1=PAL, 2=NTSC.
    let mut clock_sel: usize = 0;
```

The initial tune is loaded *before* that block without `reload_tune` (~line 418–423), so apply phi2 there explicitly — after the `let period = psid::play_period_cycles(...)` line (~line 423), add:

```rust
    set_phi2(hdr.clock()); // boot = AUTO: match the initial tune's standard
```

- [x] **Step 5: Menu row — count, labels, values, handlers**

1. `rows_in` (~line 248): `Page::Player => 4` → `Page::Player => 5`.

2. Rotate-edit match (~line 530, after the `(Page::Player, 2)` subtune arm), insert:

```rust
                        (Page::Player, 3) => {
                            clock_sel = (clock_sel as i16 + ticks as i16)
                                .clamp(0, 2) as usize;
                            set_phi2(effective_clock(clock_sel, &hdr));
                        }
```

3. Button match (~line 569): the pause toggle currently on `(Page::Player, 3)` moves to row 4; Clock gets a modify toggle. Replace:

```rust
                    (Page::Player, 2) => { modify = !modify; }
                    (Page::Player, 3) => {
                        paused = !paused;
```

with:

```rust
                    (Page::Player, 2) => { modify = !modify; }
                    (Page::Player, 3) => { modify = !modify; }
                    (Page::Player, 4) => {
                        paused = !paused;
```

(the rest of the pause arm is unchanged; the `(Page::Player, _)` label/value catch-alls below still resolve State as the last row.)

4. Labels match (~line 697), insert before the `(Page::Player, _)` catch-all:

```rust
                    (Page::Player, 3) => "Clock",
```

5. Values match (~line 714), insert before the `(Page::Player, _)` State catch-all:

```rust
                    (Page::Player, 3) => {
                        match clock_sel {
                            1 => { write!(value, "PAL").ok(); }
                            2 => { write!(value, "NTSC").ok(); }
                            _ => {
                                let c = match hdr.clock() {
                                    psid::Clock::Ntsc => "NTSC",
                                    psid::Clock::Pal  => "PAL",
                                };
                                write!(value, "AUTO ({})", c).ok();
                            }
                        }
                    }
```

- [x] **Step 6: Move the metadata line below the 5th row**

Player now has 5 rows (last baseline y = 72 + 18·4 = 144; FONT_9X15 glyphs span ~y−11..y+3), so the metadata line at y=150 would collide. Three coordinated edits:

1. Metadata draw (~line 743): `Point::new(cx, 150)` → `Point::new(cx, 162)`.
2. Row-band clear (~line 668): `let bot = if page == Page::Player && row == 2 { 155 } else { y + 5 };` → `{ 167 }` (the Song row drives the metadata line, so its clear band must reach the moved line).
3. Comment (~line 665): "metadata line at y=150" → "metadata line at y=162".

`HEADER_H` (190) still covers everything (Scope card's 6 rows reach y=162+ band ≤ 185 — no change needed; leave the constant and its comment as-is).

- [x] **Step 7: Build firmware + host tests**

Run: `cd gateware && pdm sid_player_sw build --fw-only`
Expected: compiles cleanly (uses the Task-1–3 gateware only at final flash; this checks the firmware against the regenerated PAC).

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: all existing host tests pass (no host-testable logic changed; `set_phi2`/UI live in `main.rs`, outside the lib).

- [x] **Step 8: Commit**

```bash
cd /home/pawel/code/tiliqua
git add gateware/src/top/sid_player_sw/fw/src/main.rs
git commit -m "sid_player_sw fw: auto-select phi2 from PSID header + Clock menu row"
```

---

### Task 5: host_render keeps matching hardware (φ2 = 985 500 default)

The host dump/render pipeline models the FPGA. Its hardware-quantum path hardcodes 60 sync-cycles-per-φ2; after this change the hardware runs PAL tunes at 985 500 Hz (≈60.883 sync/φ2).

**Files:**
- Modify: `gateware/src/top/sid_player_sw/fw/src/player.rs` (`schedule_events` ~line 846, its unit test ~line 908, `dump_writes` env block ~line 975 and call ~line 1068)
- Modify: `gateware/src/top/sid_player_sw/tools/host_render/render.sh` (~line 29 + header comment)
- Modify: `gateware/src/top/sid_player_sw/tools/host_render/README.md` (env table ~line 42, defaults line ~line 31)

- [x] **Step 1: Update the `schedule_events` unit test (TDD)**

In `player.rs`, in `fn schedule_events_properties` (~line 908): all existing calls gain a final `phi2_hz` argument, and assertion (b) derives its expectation from it. Replace the three call sites and the (b) expectation:

```rust
        let hw = schedule_events(&prelude, &events, 1_197_037, init_burst_end, false, 985_500);
```

```rust
        // (b) HW mode: mean frame-anchor spacing over 1 000 synthetic frames
        //     must track phi2_hz: period_sync * phi2_hz / 60e6.
        for phi2_hz in [985_500u64, 1_000_000u64] {
            let hw2 = schedule_events(&[], &single_write_per_frame, period_sync, 100, false, phi2_hz);
            assert_eq!(hw2.len(), n_frames);
            let spacings: std::vec::Vec<f64> = hw2.windows(2)
                .map(|w| (w[1].0 - w[0].0) as f64)
                .collect();
            let mean_spacing = spacings.iter().sum::<f64>() / spacings.len() as f64;
            let expected = period_sync as f64 * phi2_hz as f64 / 60_000_000.0;
            assert!(
                (mean_spacing - expected).abs() < 0.05,
                "HW mean frame spacing {mean_spacing:.4} not within ±0.05 of {expected:.4} (phi2={phi2_hz})"
            );
        }
```

```rust
        let c64_out = schedule_events(&[], &single_write_per_frame, period_sync, 100, true, 985_500);
```

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib schedule_events`
Expected: FAIL to compile (`schedule_events` takes 5 args).

- [x] **Step 2: Generalize `schedule_events`**

Change the signature (~line 846) and the conversion math:

```rust
    fn schedule_events(
        prelude:           &[(u8, u8)],
        events:            &[(usize /*frame*/, u32 /*stamp*/, u8, u8)],
        period_sync:       u32,
        init_burst_phi2_end: u64,
        c64:               bool,
        phi2_hz:           u64,
    ) -> std::vec::Vec<(u64, u8, u8)> {
```

and replace the hardcoded ×60 / ÷60 conversions (the `base_sync` definition and the `else` arm of `abs_phi2`):

```rust
        // Hardware quantum: keep the timeline in 60 MHz sync ticks and convert
        // to phi2 at emission so the fractional phi2/frame remainder
        // accumulates correctly. phi2_hz=985_500 matches the gateware's
        // PAL-rate fractional divider (DUMP_PHI2 to override; 1_000_000
        // reproduces pre-phi2-select builds).
        let clk: u64 = 60_000_000;
        let base_sync: u64 = (init_burst_phi2_end + 2) * clk / phi2_hz;
```

```rust
            } else {
                let t_sync = base_sync
                    + (frame as u64 + 1) * p
                    + p / 2
                    + stamp as u64 * clk / phi2_hz;
                t_sync * phi2_hz / clk
            };
```

(u64 headroom: t_sync ≤ ~1.3e10 sync ticks × 985 500 ≈ 1.3e16 ≪ u64::MAX.)

- [x] **Step 3: Thread `DUMP_PHI2` through `dump_writes`**

In the env-parameter block of `fn dump_writes` (~line 975, after `c64_mode`):

```rust
        let phi2_hz: u64 = std::env::var("DUMP_PHI2")
            .ok().and_then(|s| s.parse().ok()).unwrap_or(985_500);
```

and pass it at the call (~line 1068):

```rust
        let scheduled = schedule_events(
            &full_prelude,
            &play_events,
            period_sync,
            full_prelude_end,   // init_burst_phi2_end (= end of full prelude)
            c64_mode,
            phi2_hz,
        );
```

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: all pass (including `schedule_events_properties`).

- [x] **Step 4: render.sh + README**

`render.sh` line 29: `PHI2_HZ="1000000"` → `PHI2_HZ="${PHI2_HZ:-985500}"`. Header comment (line 3): change `(1 MHz phi2; ...)` to `(985.5 kHz PAL-rate phi2, override with PHI2_HZ env; ...)`.

`README.md`: line ~31 `phi2 = 1 000 000 Hz` → `phi2 = 985 500 Hz (PAL-rate; PHI2_HZ env to override)`; add to the env table (~line 46):

```markdown
| `DUMP_PHI2`    | `985500`             | phi2 Hz for hardware-quantum scheduling   |
```

- [x] **Step 5: Commit**

```bash
cd /home/pawel/code/tiliqua
git add gateware/src/top/sid_player_sw/fw/src/player.rs \
        gateware/src/top/sid_player_sw/tools/host_render/render.sh \
        gateware/src/top/sid_player_sw/tools/host_render/README.md
git commit -m "host_render: parametrize phi2 (default 985500) to track new gateware"
```

---

### Task 6: Full build + resource/Fmax verification + full test suite

**Files:** none modified — verification only, then docs.

- [x] **Step 1: Full bitstream build**

Run: `cd gateware && pdm sid_player_sw build` (~4–5 min)
Expected: completes. If it fails with `CalledProcessError: 'build_top.sh' returned non-zero`, the real cause is in stdout — grep for `ERROR`/`logic loop`.

- [x] **Step 2: Resource + Fmax checks**

Run: `grep -E "DP16KD|MULT18X18D|TRELLIS_COMB" gateware/build/sid-player-sw-r5/top.tim`
Expected (vs pre-change 34 / 11 / 20772):
- `DP16KD` ≈ 39/56 (net +5: −1 old 6/125 tap ROM, +4 PAL, +2 NTSC; ±1 from small sample-memory mapping is acceptable; must be < 56)
- `MULT18X18D` = 12/28
- `TRELLIS_COMB` < 24288 (if it no longer fits, fall back per the spec: share one FIR datapath — stop and surface this rather than improvising)

Run: `grep "Max frequency.*glbnet.*clk'" gateware/build/sid-player-sw-r5/top.tim`
Expected: the **second** line (the `Warning:` post-route one) shows achieved Fmax not lower than the current build's (~55 MHz; the design's known 60 MHz shortfall is the reSID muladd, pre-existing). A drop > ~1 MHz means the divider/mux landed on the critical path — investigate before proceeding.

- [x] **Step 3: Full test suites**

Run: `cd gateware && pdm test`
Expected: all pass.

Run: `cd gateware/src/top/sid_player_sw/fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: all pass.

- [x] **Step 4: Update `sid_player_sw/CLAUDE.md`**

In `gateware/src/top/sid_player_sw/CLAUDE.md`, the "Audio output / anti-aliasing" section's `n_up/m_down from fs_out/fs_in → 6/125` wording is now stale. Update that sentence to:

```markdown
- Fix: `top/sid/audio.py` `AudioDecimator` = polyphase FIR (`dsp.Resample`). Two
  instances (PAL 985.5kHz → 32/657, NTSC 1.023MHz → 16/341) run in parallel;
  the `phi2_sel` CSR (0=PAL, 1=NTSC, firmware auto-sets from the PSID header
  per tune, Clock menu row overrides) muxes which one reaches the codec/scope
  mix. phi2 itself comes from `Phi2Divider` (fractional-N, `src/top/sid/top.py`)
  at the same rates — true C64 pitch within +0.5 cents. Small input FIFO
  (absorbs the single-MAC FIR's per-output backpressure burst). Fed by
  `SIDPeripheral.audio_strobe`.
```

- [x] **Step 5: Commit**

```bash
cd /home/pawel/code/tiliqua
git add gateware/src/top/sid_player_sw/CLAUDE.md
git commit -m "sid_player_sw: docs for runtime phi2 PAL/NTSC select"
```

---

### Task 7: Hardware verification (user-in-the-loop — do not claim success without it)

Per the spec's verification section and the verification-before-completion skill: the pitch claim must be **measured**, not asserted. This task needs the Tiliqua connected; coordinate with the user.

- [x] **Step 1: Flash**

Run: `cd gateware && pdm run flash archive build/sid-player-sw-r5/<git-short-hash>.tar.gz`
(archive name = git HEAD short hash at build time.)

- [x] **Step 2: UI sanity**

With a PAL tune (e.g. Commando): title unchanged; metadata row shows `6581  PAL …`; Player card shows `Clock  AUTO (PAL)`. Rotate the Clock row to NTSC while playing — pitch must audibly rise (~+0.6 semitone) immediately; back to AUTO restores it.

- [x] **Step 3: Pitch/timing A/B against the C64 reference**

Capture Commando voice-0 from the hardware jack (same setup as the existing `docs/recordings/` captures) and compare against `docs/recordings/commando-c64ref-6581-v0.wav` (real-C64 PAL timing, 208.4 s):
- time-stretch factor 1.000 ± 0.03 % (the old build measured 1.497 %),
- envelope correlation ≥ 0.99 **without** time-warping,
- measured pitch offset ≤ 1 cent (target +0.44 ¢).

- [x] **Step 4: NTSC spot check**

Load one NTSC-flagged tune: metadata shows `NTSC`, `Clock AUTO (NTSC)`, play rate ≈ 59.83 Hz on the metadata line, and pitch correct on the same bitstream (this is the runtime-select payoff).

- [x] **Step 5: Final commit / report**

Report measured numbers (stretch factor, correlation, pitch offset, Fmax, utilisation) — pass or fail — honestly. If all pass, the feature is done.
