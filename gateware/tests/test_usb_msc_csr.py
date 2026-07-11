import unittest
from amaranth import *
from amaranth.sim import Simulator

from tiliqua.usb_msc_csr import USBMSCPeripheral


async def csr_write(ctx, dut, offset, value):
    """8-bit CSR bus write of one byte."""
    ctx.set(dut.bus.addr, offset)
    ctx.set(dut.bus.w_data, value)
    ctx.set(dut.bus.w_stb, 1)
    await ctx.tick()
    ctx.set(dut.bus.w_stb, 0)
    await ctx.tick()


class UsbMscCsrTests(unittest.TestCase):
    def test_mode_register_drives_mode_o(self):
        dut = USBMSCPeripheral(with_mode=True)
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            self.assertEqual(ctx.get(dut.mode_o), 0)  # reset = MIDI
            await csr_write(ctx, dut, 0x1C, 1)
            await ctx.tick()
            self.assertEqual(ctx.get(dut.mode_o), 1)
            await csr_write(ctx, dut, 0x1C, 0)
            await ctx.tick()
            self.assertEqual(ctx.get(dut.mode_o), 0)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_without_mode_has_no_register_and_mode_o_zero(self):
        dut = USBMSCPeripheral()  # sid_player_sw shape
        names = [str(path) for _, path, _ in dut.bus.memory_map.resources()] \
            if hasattr(dut.bus.memory_map, "resources") else []
        # The map must end at resp (0x18): no "mode" resource.
        self.assertNotIn("mode", " ".join(names))
        self.assertFalse(hasattr(dut, "_mode"))


if __name__ == "__main__":
    unittest.main()
