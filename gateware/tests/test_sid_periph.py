# Copyright (c) 2026 Tiliqua contributors
#
# SPDX-License-Identifier: CERN-OHL-S-2.0

"""
SIDPeripheral transaction-FIFO -> SID bus write delivery.

Every queued transaction must reach the SID as a write cycle regardless of
which sync-clock phase (relative to the phi2 divider) the write strobe lands
on. The FIFO is drained at 1 entry per phi2, and during normal playback the
producer is slower than the drain, so nearly every write lands in an *empty*
FIFO at an arbitrary divider phase — exactly the condition swept here.

Regression for the swallowed-write race: a write strobed on the same cycle as
``phi2_edge`` into an empty FIFO was popped by the unconditionally-registered
``r_en`` one cycle later, while the SID bus had already been latched as a
*read* (``level`` was still 0 at the edge). The transaction vanished with no
overflow and no level excursion — dropping SID register writes (note-ons) at
random ~1/60 incidence.
"""

import unittest

from amaranth import *
from amaranth.lib import data
from amaranth.sim import Simulator

from top.sid.top import SIDPeripheral


class _SidStub:
    """Bare signals standing in for the SID component (whose elaborate would
    pull in the vendored SystemVerilog Instance, unusable in amaranth.sim).
    Only the members SIDPeripheral.elaborate touches are provided."""
    def __init__(self):
        self.bus_i = Signal(data.StructLayout({
            "res":   unsigned(1),
            "r_w_n": unsigned(1),
            "phi2":  unsigned(1),
            "data":  unsigned(8),
            "addr":  unsigned(5),
        }))
        self.cs      = Signal(4)
        self.data_o  = Signal(8)
        self.audio_o = Signal(data.StructLayout({
            "right": signed(24),
            "left":  signed(24),
        }))


DIVIDE_BY = 60  # phi2 divider in SIDPeripheral


class SidPeripheralWriteDeliveryTests(unittest.TestCase):

    def test_write_delivered_at_every_divider_phase(self):
        periph = SIDPeripheral(transaction_depth=16, sid2_define=False)
        periph.sid = _SidStub()

        m = Module()
        m.submodules.periph = periph

        delivered = {}   # phase -> list of (addr, data) write cycles seen

        async def testbench(ctx):
            bus = periph.sid.bus_i

            # Let the 24-edge SID reset window pass.
            for _ in range(25 * DIVIDE_BY):
                await ctx.tick()

            # Calibrate the divider phase from the SID-visible phi2: a 1->0
            # transition of bus_i.phi2 happens on the cycle where the internal
            # counter == 1 (phi2 comb falls at counter 0, registered +1).
            prev = ctx.get(bus.phi2)
            n = 0
            while True:
                await ctx.tick()
                n += 1
                cur = ctx.get(bus.phi2)
                if prev == 1 and cur == 0:
                    break
                prev = cur
                assert n < 3 * DIVIDE_BY, "phi2 never toggled"
            phase = 1  # internal phi2_clk_counter value on this cycle

            async def step():
                nonlocal phase
                await ctx.tick()
                phase = (phase + 1) % DIVIDE_BY

            for inject_phase in range(DIVIDE_BY):
                reg = inject_phase % 25
                val = 0x80 | inject_phase
                txn = ((val << 5) | reg) & 0xFFFF

                # Wait for the divider to reach the phase under test.
                while phase != inject_phase:
                    await step()

                # One-cycle write strobe into an empty FIFO (the normal
                # playback condition: producer slower than the 1/phi2 drain).
                self.assertEqual(ctx.get(periph._transactions.level), 0,
                                 "FIFO not drained before injection")
                ctx.set(periph.ext_w_en, 1)
                ctx.set(periph.ext_w_data, txn)
                await step()
                ctx.set(periph.ext_w_en, 0)

                # Observe the SID bus for two full phi2 periods: an applied
                # write = any cycle with r_w_n low; capture its addr/data.
                seen = []
                was_write = False
                for _ in range(2 * DIVIDE_BY + 5):
                    is_write = ctx.get(bus.r_w_n) == 0
                    if is_write and not was_write:
                        seen.append((ctx.get(bus.addr), ctx.get(bus.data)))
                    was_write = is_write
                    await step()
                delivered[inject_phase] = (reg, val, seen)

        sim = Simulator(m)
        sim.add_clock(1 / 60e6)
        sim.add_testbench(testbench)
        sim.run()

        lost = []
        for ph, (reg, val, seen) in sorted(delivered.items()):
            if (reg, val) not in seen:
                lost.append((ph, reg, val, seen))
        self.assertEqual(lost, [], (
            f"{len(lost)}/{DIVIDE_BY} divider phases swallowed the SID write "
            f"(phase, reg, val, writes-seen): {lost}"))


class SidPeripheralWriteTrainTests(unittest.TestCase):
    """Multi-write delivery sweeps.

    The original regression covers a single write into an *empty* FIFO at every
    divider phase. These sweeps cover the conditions it does not: writes landing
    while the FIFO is non-empty, writes landing on the pop cycle (the cycle
    after ``phi2_edge`` where ``r_en`` is high), paced trains at exactly one
    write per phi2 period (the firmware replay's phase-locked pattern),
    back-to-back bursts (the replay's same-stamp / bail path), and inter-write
    gaps that straddle the period boundary. Every scenario asserts the full
    delivered sequence — values AND order, one write slot per phi2 period —
    not merely "some write appeared".
    """

    def _sweep(self, scenarios):
        """scenarios: list of (label, inject_phase, [gap_to_next_write...]).

        For each scenario, writes are strobed via ext_w_en: the first on the
        cycle where the internal divider == inject_phase, each subsequent one
        ``gap`` cycles after the previous (gap >= 1; gap == 1 means
        back-to-back cycles). Returns {label: (expected, delivered)}.
        """
        periph = SIDPeripheral(transaction_depth=16, sid2_define=False)
        periph.sid = _SidStub()
        m = Module()
        m.submodules.periph = periph
        results = {}

        async def testbench(ctx):
            bus = periph.sid.bus_i

            for _ in range(25 * DIVIDE_BY):
                await ctx.tick()

            # Calibrate divider phase (same method as the single-write test).
            prev = ctx.get(bus.phi2)
            n = 0
            while True:
                await ctx.tick()
                n += 1
                cur = ctx.get(bus.phi2)
                if prev == 1 and cur == 0:
                    break
                prev = cur
                assert n < 3 * DIVIDE_BY, "phi2 never toggled"
            phase = 1

            # One delivery slot per phi2 period: the SID bus registers are
            # latched on the edge (counter 59 -> 0), so sample them once per
            # period at phase 0. A write slot has r_w_n low.
            delivered = []

            async def step():
                nonlocal phase
                await ctx.tick()
                phase = (phase + 1) % DIVIDE_BY
                if phase == 0 and ctx.get(bus.r_w_n) == 0:
                    delivered.append((ctx.get(bus.addr), ctx.get(bus.data)))

            seq = 0
            for label, inject_phase, gaps in scenarios:
                # Quiesce: FIFO empty and a clean slot boundary.
                while ctx.get(periph._transactions.level) != 0:
                    await step()
                for _ in range(2 * DIVIDE_BY):
                    await step()
                delivered.clear()

                expected = []
                while phase != inject_phase:
                    await step()
                # First write plus one per gap.
                for i, gap in enumerate([None] + list(gaps)):
                    if gap is not None:
                        for _ in range(gap - 1):
                            await step()
                    reg = (seq + i) % 25
                    val = (seq + i * 7) & 0xFF
                    expected.append((reg, val))
                    ctx.set(periph.ext_w_en, 1)
                    ctx.set(periph.ext_w_data, ((val << 5) | reg) & 0xFFFF)
                    await step()
                    ctx.set(periph.ext_w_en, 0)
                seq += len(expected)

                # Drain: every queued write takes one period; pad generously.
                for _ in range((len(expected) + 3) * DIVIDE_BY):
                    await step()
                results[label] = (expected, list(delivered))

        sim = Simulator(m)
        sim.add_clock(1 / 60e6)
        sim.add_testbench(testbench)
        sim.run()
        return results

    def _assert_all_delivered(self, results):
        bad = {label: (exp, got) for label, (exp, got) in results.items()
               if got != exp}
        self.assertEqual(bad, {}, (
            f"{len(bad)}/{len(results)} scenarios mis-delivered "
            f"(label: expected vs SID-bus writes): {bad}"))

    def test_paced_train_at_every_phase(self):
        """5 writes spaced exactly one phi2 period apart (the replay's
        phase-locked steady state), starting at every divider phase."""
        scenarios = [(f"paced@{p}", p, [DIVIDE_BY] * 4) for p in range(DIVIDE_BY)]
        self._assert_all_delivered(self._sweep(scenarios))

    def test_burst_at_every_phase(self):
        """5 back-to-back writes (one per sync cycle: the same-stamp burst /
        replay bail pattern) starting at every divider phase, so the FIFO is
        non-empty while later writes land and while pops are in flight."""
        scenarios = [(f"burst@{p}", p, [1] * 4) for p in range(DIVIDE_BY)]
        self._assert_all_delivered(self._sweep(scenarios))

    def test_write_pair_gap_sweep(self):
        """Two writes with gaps that bracket the pop cycle and the period
        boundary, at every phase: the second write lands at every offset
        relative to the drain of the first."""
        gaps = [1, 2, 3, 5, 10, 30, 58, 59, 60, 61, 62, 90, 119, 120, 121]
        scenarios = [(f"pair@{p}+{g}", p, [g])
                     for p in range(DIVIDE_BY) for g in gaps]
        self._assert_all_delivered(self._sweep(scenarios))


if __name__ == "__main__":
    unittest.main()
