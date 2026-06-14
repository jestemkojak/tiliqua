# Copyright (c) 2026 Tiliqua contributors
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""CDC primitives for the SID 30MHz domain: transaction AsyncFIFO (sync->sid)
and the audio-strobe PulseSynchronizer (sid->sync)."""
import unittest
from amaranth import *
from amaranth.lib.fifo import AsyncFIFO
from amaranth.lib.cdc import PulseSynchronizer
from amaranth.sim import Simulator


def _two_clocks(sim, sync_hz=60e6, sid_hz=30e6):
    sim.add_clock(1 / sync_hz, domain="sync")
    sim.add_clock(1 / sid_hz, domain="sid")


class TransactionAsyncFifoTests(unittest.TestCase):
    def test_words_cross_in_order(self):
        m = Module()
        m.domains.sync = ClockDomain()
        m.domains.sid = ClockDomain()
        m.submodules.fifo = fifo = AsyncFIFO(width=16, depth=16,
                                             w_domain="sync", r_domain="sid")
        sent = [0x0123, 0x1F0E, 0x00A5, 0x1234]
        got = []

        async def writer(ctx):
            for w in sent:
                while not ctx.get(fifo.w_rdy):
                    await ctx.tick("sync")
                ctx.set(fifo.w_data, w)
                ctx.set(fifo.w_en, 1)
                await ctx.tick("sync")
                ctx.set(fifo.w_en, 0)
                for _ in range(3):
                    await ctx.tick("sync")

        async def reader(ctx):
            ctx.set(fifo.r_en, 1)
            for _ in range(400):
                if ctx.get(fifo.r_rdy):
                    got.append(ctx.get(fifo.r_data))
                    if len(got) == len(sent):
                        break
                await ctx.tick("sid")

        sim = Simulator(m)
        _two_clocks(sim)
        sim.add_testbench(writer)
        sim.add_testbench(reader)
        sim.run()
        self.assertEqual(got, sent)


class StrobePulseSyncTests(unittest.TestCase):
    def test_each_sid_pulse_yields_one_sync_pulse(self):
        m = Module()
        m.domains.sync = ClockDomain()
        m.domains.sid = ClockDomain()
        m.submodules.ps = ps = PulseSynchronizer(i_domain="sid", o_domain="sync")
        counts = {"in": 0, "out": 0}

        async def driver(ctx):
            # 5 sid pulses, well separated (>4 sid cycles apart).
            for _ in range(5):
                ctx.set(ps.i, 1); await ctx.tick("sid")
                ctx.set(ps.i, 0); counts["in"] += 1
                for _ in range(8):
                    await ctx.tick("sid")

        async def counter(ctx):
            for _ in range(400):
                if ctx.get(ps.o):
                    counts["out"] += 1
                await ctx.tick("sync")

        sim = Simulator(m)
        _two_clocks(sim)
        sim.add_testbench(driver)
        sim.add_testbench(counter)
        sim.run()
        self.assertEqual(counts["out"], counts["in"])


if __name__ == "__main__":
    unittest.main()
