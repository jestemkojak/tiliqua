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


if __name__ == "__main__":
    unittest.main()
