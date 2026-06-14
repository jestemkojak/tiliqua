# Copyright (c) 2026 Tiliqua contributors
#
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""Integrated SIDPeripheral CDC (sync<->sid) with a stubbed SID core.

amaranth.sim cannot execute the vendored ``Instance("sid_api")``, but
``SIDPeripheral`` takes ``self.sid`` as an externally-assigned component -- so we
substitute a tiny ``FakeSid`` with the same signature that echoes bus writes into
a register file and drives ``audio_o``/``voiceN_dca`` from it. Running the whole
refactored peripheral with two clocks then verifies the full CDC loop
functionally:

    ext_w (sync) -> AsyncFIFO -> sid-domain bus -> FakeSid regfile
                 -> audio_o/voiceN -> captured back into the sync outputs.

Injection approach: the real peripheral exposes the ``ext_w_en``/``ext_w_data``
ports (src/top/sid/top.py ~L326), ORed into the transaction AsyncFIFO write side
in elaborate (~L348). We drive those ports from the sync-domain testbench -- the
intended public injection path -- rather than poking the FIFO submodule.
"""
import unittest

from amaranth import *
from amaranth.lib import data, wiring
from amaranth.lib.wiring import In, Out
from amaranth.sim import Simulator

from top.sid.top import SIDPeripheral

# Match the real SID.bus_i / SID.audio_o layouts in top.py exactly (unsigned
# bus fields; signed(24) audio). StructLayout is keyed by name, so the only
# thing that must match is the field names + widths/signedness.
BUS = data.StructLayout({
    "res":   unsigned(1),
    "r_w_n": unsigned(1),
    "phi2":  unsigned(1),
    "data":  unsigned(8),
    "addr":  unsigned(5),
})
AUDIO = data.StructLayout({
    "right": signed(24),
    "left":  signed(24),
})


class FakeSid(wiring.Component):
    """Same signature SIDPeripheral expects of ``self.sid``. Writes (r_w_n=0)
    land in a register file; audio/voice taps mirror low registers so the test
    can read injected values back out through the capture path.

    The peripheral drives the SID bus in the ``sid`` domain, so the regfile is
    written in ``m.d.sid``."""
    clk:        In(1)
    bus_i:      In(BUS)
    cs:         In(4)
    data_o:     Out(8)
    audio_o:    Out(AUDIO)
    voice0_dca: Out(signed(16))
    voice1_dca: Out(signed(16))
    voice2_dca: Out(signed(16))

    def elaborate(self, platform):
        m = Module()
        regs = Array([Signal(8, name=f"reg{i}") for i in range(32)])
        with m.If(~self.bus_i.r_w_n):
            m.d.sid += regs[self.bus_i.addr].eq(self.bus_i.data)
        m.d.comb += [
            self.audio_o.left.eq(regs[0].as_signed()),
            self.voice0_dca.eq(regs[1].as_signed()),
            self.voice1_dca.eq(regs[2].as_signed()),
            self.voice2_dca.eq(regs[3].as_signed()),
        ]
        return m


class IntegratedCdcTests(unittest.TestCase):
    def test_write_and_capture_round_trip(self):
        # Flat /30 phi2 (1MHz) at a 30MHz sid clock -> simple deterministic CDC.
        dut = SIDPeripheral(sid_hz=30_000_000, phi2_hz=(1_000_000, 1_000_000))
        fake = FakeSid()
        dut.sid = fake
        m = Module()
        m.domains.sync = ClockDomain()
        m.domains.sid = ClockDomain()
        m.submodules.dut = dut
        m.submodules.fake = fake

        writes = [(0, 0x12), (1, 0x34), (2, 0x56), (3, 0x78)]  # (reg, val<0x80)
        out = {}

        async def driver(ctx):
            # i_midi idle; ext_w path is what we exercise.
            ctx.set(dut.i_midi.valid, 0)
            ctx.set(dut.ext_w_en, 0)
            for reg, val in writes:
                ctx.set(dut.ext_w_data, (val << 5) | reg)
                ctx.set(dut.ext_w_en, 1)
                await ctx.tick("sync")
                ctx.set(dut.ext_w_en, 0)
                for _ in range(4):
                    await ctx.tick("sync")

        async def sampler(ctx):
            # The bus holds res=1 for the first 24 phi2 edges (~24*30 sid cycles
            # at flat /30); writes only land after that, then must cross
            # AsyncFIFO + a phi2 edge + the PulseSynchronizer capture back to
            # sync. Settle generously.
            for _ in range(6000):
                await ctx.tick("sync")
            out["audio"] = ctx.get(dut.last_audio_left)
            out["v0"] = ctx.get(dut.voice0_dca_o)
            out["v1"] = ctx.get(dut.voice1_dca_o)
            out["v2"] = ctx.get(dut.voice2_dca_o)

        sim = Simulator(m)
        sim.add_clock(1 / 60e6, domain="sync")
        sim.add_clock(1 / 30e6, domain="sid")
        sim.add_testbench(driver)
        sim.add_testbench(sampler)
        sim.run()
        self.assertEqual(out["audio"], 0x12)
        self.assertEqual(out["v0"], 0x34)
        self.assertEqual(out["v1"], 0x56)
        self.assertEqual(out["v2"], 0x78)


if __name__ == "__main__":
    unittest.main()
