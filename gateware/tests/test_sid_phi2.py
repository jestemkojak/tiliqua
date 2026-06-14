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


def _run_divider_at(sync_hz, phi2_hz, sel, window, warmup=2000):
    m = Module()
    m.submodules.dut = dut = Phi2Divider(sync_hz=sync_hz, phi2_hz=phi2_hz)
    result = {}

    async def testbench(ctx):
        ctx.set(dut.sel, sel)
        for _ in range(warmup):
            await ctx.tick()
        edges = 0
        for _ in range(window):
            edges += ctx.get(dut.phi2_edge)
            await ctx.tick()
        result["edges"] = edges

    sim = Simulator(m)
    sim.add_clock(1 / sync_hz)
    sim.add_testbench(testbench)
    sim.run()
    return result["edges"]


class Phi2Divider30MHzTests(unittest.TestCase):
    PHI2 = (985_500, 1_023_000)

    def test_pal_rate_exact_30mhz(self):
        """sel=0 -> 657 edges per 20 000 30MHz cycles (Fraction(30e6,985500)=20000/657)."""
        self.assertEqual(_run_divider_at(30_000_000, self.PHI2, 0, 3 * 20_000),
                         3 * 657)

    def test_ntsc_rate_exact_30mhz(self):
        """sel=1 -> 341 edges per 10 000 30MHz cycles (Fraction(30e6,1023000)=10000/341)."""
        self.assertEqual(_run_divider_at(30_000_000, self.PHI2, 1, 3 * 10_000),
                         3 * 341)


if __name__ == "__main__":
    unittest.main()
