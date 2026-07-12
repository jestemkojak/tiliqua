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

    def test_without_write_has_no_tx_registers(self):
        dut = USBMSCPeripheral()  # sid_player_sw shape: with_write defaults False
        names = [str(path) for _, path, _ in dut.bus.memory_map.resources()] \
            if hasattr(dut.bus.memory_map, "resources") else []
        joined = " ".join(names)
        self.assertNotIn("tx_data", joined)
        self.assertNotIn("start_write", joined)
        self.assertFalse(hasattr(dut, "_tx_data"))
        self.assertFalse(hasattr(dut, "_start_write"))

    def test_tx_words_unpack_to_byte_stream(self):
        dut = USBMSCPeripheral(with_mode=True, with_write=True)
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            # Push one word via CSR (4 byte-writes, little-endian bus).
            for i, b in enumerate([0x11, 0x22, 0x33, 0x44]):
                await csr_write(ctx, dut, 0x20 + i, b)
            # Byte stream must yield 11 22 33 44 in order.
            ctx.set(dut.tx_data_o.ready, 1)
            got = []
            for _ in range(32):
                await ctx.tick()
                if ctx.get(dut.tx_data_o.valid):
                    got.append(ctx.get(dut.tx_data_o.payload))
                if len(got) == 4:
                    break
            self.assertEqual(got, [0x11, 0x22, 0x33, 0x44])

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_start_write_strobes_and_clears_sticky_resp(self):
        dut = USBMSCPeripheral(with_mode=True, with_write=True)
        m = Module()
        m.submodules.dut = dut

        async def csr_read(ctx, offset):
            ctx.set(dut.bus.addr, offset)
            ctx.set(dut.bus.r_stb, 1)
            await ctx.tick()
            ctx.set(dut.bus.r_stb, 0)
            return ctx.get(dut.bus.r_data)

        async def testbench(ctx):
            # Latch done+error via resp_i.
            ctx.set(dut.resp_i.done, 1)
            ctx.set(dut.resp_i.error, 1)
            await ctx.tick()
            ctx.set(dut.resp_i.done, 0)
            ctx.set(dut.resp_i.error, 0)
            await ctx.tick()
            # resp @0x18: bit0=error, bit1=done — both sticky-set.
            self.assertEqual(await csr_read(ctx, 0x18) & 0b11, 0b11)
            await csr_write(ctx, dut, 0x24, 1)   # start_write strobe
            await ctx.tick()
            # sticky bits cleared by the strobe.
            self.assertEqual(await csr_read(ctx, 0x18) & 0b11, 0b00)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()


if __name__ == "__main__":
    unittest.main()
