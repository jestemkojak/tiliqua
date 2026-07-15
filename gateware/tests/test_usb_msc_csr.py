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


async def csr_read32(ctx, dut, offset):
    value = 0
    for i in range(4):
        ctx.set(dut.bus.addr, offset + i)
        ctx.set(dut.bus.r_stb, 1)
        await ctx.tick()
        ctx.set(dut.bus.r_stb, 0)
        value |= ctx.get(dut.bus.r_data) << (8 * i)
    return value


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
        self.assertNotIn("read_path_info", joined)
        self.assertFalse(hasattr(dut, "_read_path_info"))

    def test_read_path_info_counts_and_resets_on_read_start(self):
        dut = USBMSCPeripheral(with_mode=True, with_write=True)
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            ctx.set(dut.engine_rx_bytes_i, 512)
            ctx.set(dut.engine_stream_mode_i, 1)
            ctx.set(dut.engine_data_len_512_i, 1)
            ctx.set(dut.rx_data.valid, 1)
            for b in range(8):
                ctx.set(dut.rx_data.payload.data, b)
                await ctx.tick()
            ctx.set(dut.rx_data.valid, 0)
            await ctx.tick()

            raw = await csr_read32(ctx, dut, 0x38)
            self.assertEqual(raw & 0x3FF, 512)
            self.assertEqual((raw >> 10) & 0x3FF, 8)
            self.assertEqual((raw >> 20) & 0xFF, 2)
            self.assertEqual((raw >> 28) & 1, 1)
            self.assertEqual((raw >> 29) & 1, 1)
            self.assertEqual(raw >> 30, 0)

            # Continue past both counter maxima. The byte counter must stop at
            # 1023, and the accepted-word counter must stop at 255 rather than
            # wrapping to zero when the 256-word FIFO becomes full.
            ctx.set(dut.rx_data.valid, 1)
            for b in range(1100):
                ctx.set(dut.rx_data.payload.data, b & 0xFF)
                await ctx.tick()
            ctx.set(dut.rx_data.valid, 0)
            await ctx.tick()
            raw = await csr_read32(ctx, dut, 0x38)
            self.assertEqual((raw >> 10) & 0x3FF, 1023)
            self.assertEqual((raw >> 20) & 0xFF, 255)

            await csr_write(ctx, dut, 0x10, 1)
            await ctx.tick()
            raw = await csr_read32(ctx, dut, 0x38)
            self.assertEqual(raw & 0x3FF, 512)  # live engine input
            self.assertEqual((raw >> 10) & 0x3FF, 0)
            self.assertEqual((raw >> 20) & 0xFF, 0)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

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

    def test_write_contract_strobe_then_fill_defers_start(self):
        # THE regression test for the 2026-07-14 drive-corruption incident:
        # the engine must never be started (start_write_o) until the full
        # 512-byte payload is banked in the TX FIFO. Contract: strobe
        # start_write FIRST (flushes leftovers, clears sticky resp), THEN
        # push 128 words; start_write_o fires exactly once, only after the
        # 128th word, and the byte stream then yields all 512 bytes intact.
        dut = USBMSCPeripheral(with_mode=True, with_write=True)
        m = Module()
        m.submodules.dut = dut
        # Count start_write_o pulses in hardware so no testbench-interleaving
        # gap can miss a 1-cycle pulse.
        pulse_count = Signal(8)
        with m.If(dut.start_write_o):
            m.d.sync += pulse_count.eq(pulse_count + 1)

        def word_bytes(i):
            return [(4 * i + k) & 0xFF for k in range(4)]

        async def testbench(ctx):
            ctx.set(dut.tx_data_o.ready, 0)
            await csr_write(ctx, dut, 0x24, 1)   # start_write strobe FIRST
            await ctx.tick()
            self.assertEqual(ctx.get(pulse_count), 0,
                             "engine started with an empty payload FIFO")
            # 127 words: still no start.
            for i in range(127):
                for off, b in [(0x20 + k, word_bytes(i)[k]) for k in range(4)]:
                    await csr_write(ctx, dut, off, b)
            await ctx.tick()
            self.assertEqual(ctx.get(pulse_count), 0,
                             "engine started before the full payload was banked")
            # 128th word completes the block: exactly one start pulse.
            for off, b in [(0x20 + k, word_bytes(127)[k]) for k in range(4)]:
                await csr_write(ctx, dut, off, b)
            for _ in range(8):
                await ctx.tick()
            self.assertEqual(ctx.get(pulse_count), 1)
            # Full payload must now stream out intact. Sample BEFORE each
            # tick: the byte on the payload wires is the one consumed at
            # that tick's edge (valid & ready).
            ctx.set(dut.tx_data_o.ready, 1)
            got = []
            for _ in range(2048):
                if ctx.get(dut.tx_data_o.valid):
                    got.append(ctx.get(dut.tx_data_o.payload))
                await ctx.tick()
                if len(got) == 512:
                    break
            self.assertEqual(got, [i & 0xFF for i in range(512)])
            self.assertEqual(ctx.get(pulse_count), 1)  # still exactly one

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_restrobe_after_partial_fill_flushes_and_rearms(self):
        # An abandoned partial fill (e.g. firmware died mid-write) must not
        # contaminate the next write: re-strobing flushes the leftovers and
        # re-arms, and the next full 128-word fill starts the engine with
        # ONLY the fresh payload.
        dut = USBMSCPeripheral(with_mode=True, with_write=True)
        m = Module()
        m.submodules.dut = dut
        pulse_count = Signal(8)
        with m.If(dut.start_write_o):
            m.d.sync += pulse_count.eq(pulse_count + 1)

        async def testbench(ctx):
            ctx.set(dut.tx_data_o.ready, 0)
            await csr_write(ctx, dut, 0x24, 1)          # arm
            for i in range(5):                          # partial fill, abandoned
                for k in range(4):
                    await csr_write(ctx, dut, 0x20 + k, 0xEE)
            await csr_write(ctx, dut, 0x24, 1)          # re-strobe: flush+rearm
            for i in range(128):                        # fresh full payload
                for k in range(4):
                    await csr_write(ctx, dut, 0x20 + k, (4 * i + k) & 0xFF)
            for _ in range(8):
                await ctx.tick()
            self.assertEqual(ctx.get(pulse_count), 1)
            ctx.set(dut.tx_data_o.ready, 1)
            got = []
            for _ in range(2048):
                if ctx.get(dut.tx_data_o.valid):
                    got.append(ctx.get(dut.tx_data_o.payload))
                await ctx.tick()
                if len(got) == 512:
                    break
            # No 0xEE leftovers — fresh payload only.
            self.assertEqual(got, [i & 0xFF for i in range(512)])

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_read_start_cancels_armed_write(self):
        # A read command (start strobe) issued after arming a write must
        # cancel the pending write-start: stray tx words later reaching 128
        # must NOT launch a surprise WRITE at a stale LBA.
        dut = USBMSCPeripheral(with_mode=True, with_write=True)
        m = Module()
        m.submodules.dut = dut
        pulse_count = Signal(8)
        with m.If(dut.start_write_o):
            m.d.sync += pulse_count.eq(pulse_count + 1)

        async def testbench(ctx):
            ctx.set(dut.tx_data_o.ready, 0)
            await csr_write(ctx, dut, 0x24, 1)   # arm write
            await csr_write(ctx, dut, 0x10, 1)   # read start: cancels
            for i in range(128):
                for k in range(4):
                    await csr_write(ctx, dut, 0x20 + k, 0x55)
            for _ in range(8):
                await ctx.tick()
            self.assertEqual(ctx.get(pulse_count), 0)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_start_write_resets_tx_fifo(self):
        # A leftover word from a prior (partial/failed) write must not
        # survive into the next write — start_write should flush the TX
        # FIFO, mirroring the RX FIFO's ResetInserter(start_strobe).
        dut = USBMSCPeripheral(with_mode=True, with_write=True)
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            # Push one leftover word (0x11 22 33 44) but never drain it
            # (tx_data_o.ready stays 0) — simulates a write that aborted
            # partway through.
            ctx.set(dut.tx_data_o.ready, 0)
            for i, b in enumerate([0x11, 0x22, 0x33, 0x44]):
                await csr_write(ctx, dut, 0x20 + i, b)
            await ctx.tick()

            # New write command: flush via start_write, then push a fresh
            # word (0xAA BB CC DD).
            await csr_write(ctx, dut, 0x24, 1)   # start_write strobe
            for i, b in enumerate([0xAA, 0xBB, 0xCC, 0xDD]):
                await csr_write(ctx, dut, 0x20 + i, b)

            ctx.set(dut.tx_data_o.ready, 1)
            got = []
            for _ in range(32):
                await ctx.tick()
                if ctx.get(dut.tx_data_o.valid):
                    got.append(ctx.get(dut.tx_data_o.payload))
                if len(got) == 4:
                    break
            # Only the fresh word should appear — no leftover 0x11 first.
            self.assertEqual(got, [0xAA, 0xBB, 0xCC, 0xDD])

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()


if __name__ == "__main__":
    unittest.main()
