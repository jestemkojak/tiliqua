import unittest
from amaranth import *
from amaranth.lib import wiring
from amaranth.sim import Simulator

from top.sid_player.top import USBMSCPeripheral


class UsbMscPeriphTests(unittest.TestCase):
    def test_rx_bytes_pack_into_word_fifo(self):
        dut = USBMSCPeripheral()
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            # Feed 4 bytes 0x11 0x22 0x33 0x44 on the rx_data stream.
            for b in (0x11, 0x22, 0x33, 0x44):
                ctx.set(dut.rx_data.payload.data, b)
                ctx.set(dut.rx_data.valid, 1)
                await ctx.tick()
            ctx.set(dut.rx_data.valid, 0)
            # The packed word FIFO should now hold 0x44332211 (little-endian).
            for _ in range(8):
                await ctx.tick()
                if ctx.get(dut.dbg_word_level) > 0:
                    break
            self.assertEqual(ctx.get(dut.dbg_word_data), 0x44332211)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()


if __name__ == "__main__":
    unittest.main()
