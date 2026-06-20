import unittest
from amaranth import *
from amaranth.lib import wiring
from amaranth.sim import Simulator

from top.sid_player_sw.top import USBMSCPeripheral


class UsbMscSwPeriphTests(unittest.TestCase):
    """Sim tests proving the start-strobe reset semantics introduced by Task 2.

    These tests convert "needs HW" items into sim-proven guarantees:
    - partial byte packer is zeroed on start
    - sticky error latch is cleared on start
    - stale FIFO words are flushed on start
    """

    async def _feed_bytes(self, ctx, dut, data):
        """Feed a sequence of bytes on the rx_data stream."""
        for b in data:
            ctx.set(dut.rx_data.payload.data, b)
            ctx.set(dut.rx_data.valid, 1)
            await ctx.tick()
        ctx.set(dut.rx_data.valid, 0)

    async def _pulse_start_strobe(self, ctx, dut):
        """Write 1 to start.strobe (offset 0x10) to trigger reset.

        The CSR bus is addr_width=5, data_width=8. start.strobe is at offset 0x10.
        A write completes when w_stb is asserted with the correct addr and w_data.
        """
        ctx.set(dut.bus.addr, 0x10)
        ctx.set(dut.bus.w_data, 0x01)
        ctx.set(dut.bus.w_stb, 1)
        ctx.set(dut.bus.r_stb, 0)
        await ctx.tick()
        ctx.set(dut.bus.w_stb, 0)
        await ctx.tick()

    async def _pulse_resp_done_error(self, ctx, dut):
        """Drive resp_i.done=1 resp_i.error=1 for one cycle to latch error."""
        ctx.set(dut.resp_i.done, 1)
        ctx.set(dut.resp_i.error, 1)
        await ctx.tick()
        ctx.set(dut.resp_i.done, 0)
        ctx.set(dut.resp_i.error, 0)
        # Wait for the registered latch to take effect.
        await ctx.tick()

    def test_start_strobe_clears_byte_packer(self):
        """After feeding a partial (odd) number of bytes then pulsing start,
        the byte packer resets so a subsequent 4-byte group packs correctly.

        Concretely: feed 2 bytes (byte_ix reaches 2), pulse start (byte_ix→0),
        then feed another 4 bytes and confirm a single word is produced and
        contains only the new bytes (not the stale partial 0xAA/0xBB)."""
        dut = USBMSCPeripheral()
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            # Feed 2 bytes (partial word) — byte_ix should reach 2.
            await self._feed_bytes(ctx, dut, (0xAA, 0xBB))
            await ctx.tick()

            # Pulse start strobe — should reset byte_ix to 0.
            await self._pulse_start_strobe(ctx, dut)

            # FIFO should be empty (no complete word was assembled).
            self.assertEqual(ctx.get(dut._word_fifo.r_level), 0)

            # Now feed a clean 4 bytes and verify one word is packed correctly.
            await self._feed_bytes(ctx, dut, (0x11, 0x22, 0x33, 0x44))

            # Wait for the word to appear in the FIFO.
            for _ in range(8):
                await ctx.tick()
                if ctx.get(dut._word_fifo.r_level) > 0:
                    break

            self.assertEqual(ctx.get(dut._word_fifo.r_level), 1,
                             "Expected exactly one word in FIFO after a clean 4-byte group")
            self.assertEqual(ctx.get(dut._word_fifo.r_data), 0x44332211,
                             "Word should be 0x44332211 (little-endian); stale partial bytes must NOT appear")

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_start_strobe_clears_sticky_error(self):
        """After resp_i.done+error=1 latches resp_error_r, pulsing start clears it.

        Probes the register's r_data signal directly (the combinational feed into
        the resp CSR error field) rather than going through the bus, which avoids
        multi-cycle CSR protocol complexity in the testbench."""
        dut = USBMSCPeripheral()
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            # Confirm error starts at 0.
            self.assertEqual(ctx.get(dut._resp.f.error.r_data), 0,
                             "resp.error should start at 0")

            # Drive resp_i to latch an error.
            await self._pulse_resp_done_error(ctx, dut)

            # Confirm the error is latched (resp_error_r fed into r_data).
            self.assertEqual(ctx.get(dut._resp.f.error.r_data), 1,
                             "resp.error should be 1 after done+error pulse")

            # Pulse start strobe — should clear resp_error_r.
            await self._pulse_start_strobe(ctx, dut)

            # Error must be cleared.
            self.assertEqual(ctx.get(dut._resp.f.error.r_data), 0,
                             "resp.error should be 0 after start strobe clears the latch")

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_start_strobe_flushes_fifo(self):
        """After enqueuing a full word into the FIFO, pulsing start empties it.

        Verifies that no stale word survives a start strobe, preventing corruption
        of subsequent reads from an aborted prior transfer."""
        dut = USBMSCPeripheral()
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            # Feed 4 bytes to produce one word in the FIFO.
            await self._feed_bytes(ctx, dut, (0x11, 0x22, 0x33, 0x44))

            # Wait for the word to appear in the FIFO.
            for _ in range(8):
                await ctx.tick()
                if ctx.get(dut._word_fifo.r_level) > 0:
                    break
            self.assertEqual(ctx.get(dut._word_fifo.r_level), 1,
                             "Precondition: FIFO should have one word before flush")

            # Pulse start strobe — FIFO must be flushed.
            await self._pulse_start_strobe(ctx, dut)

            # After a couple of cycles the FIFO must be empty.
            for _ in range(4):
                await ctx.tick()
            self.assertEqual(ctx.get(dut._word_fifo.r_level), 0,
                             "FIFO must be empty after start strobe flush")

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()


if __name__ == "__main__":
    unittest.main()
