# Copyright (c) 2024 Seb Holzapfel <me@sebholzapfel.com>
#
# SPDX-License-Identifier: CERN-OHL-S-2.0

import struct
import unittest

from amaranth import *
from amaranth.sim import *
from amaranth.lib import wiring

from tiliqua.test import wishbone, stream, csr as csr_util
from tiliqua.raster import persist, stroke, plot, blit, line
from tiliqua.video import framebuffer, modeline, palette
from tiliqua.video.types import Rotation

from amaranth_soc import csr
from amaranth_soc.csr import wishbone as csr_wishbone


class RasterTests(unittest.TestCase):

    MODELINE = modeline.DVIModeline.all_timings()["1280x720p60"]

    def test_persist(self):

        m = Module()
        fb = framebuffer.DMAFramebuffer(
            fixed_modeline=self.MODELINE, palette=palette.ColorPalette())
        dut = persist.Persistance(bus_signature=fb.bus.signature)
        wiring.connect(m, wiring.flipped(fb.fbp), dut.fbp)
        check = wishbone.BusChecker(dut.bus, prefix='[bus] ')
        m.submodules += [dut, fb, fb.palette, check]

        # No actual FB backing store, just simulating WB transactions

        async def testbench(ctx):
            ctx.set(fb.fbp.enable, 1)
            # Simulate N burst accesses
            for _ in range(4):
                ix = 0
                while not ctx.get(dut.bus.stb):
                    await ctx.tick()
                # Simulate acks delayed from stb
                await ctx.tick().repeat(8)
                ctx.set(dut.bus.ack, 1)
                while ctx.get(dut.bus.stb):
                    # for all burst accesses, simulate full intensity.
                    ctx.set(dut.bus.dat_r, 0xffffff00 | (ix&0xf))
                    if ctx.get(dut.bus.we):
                        # for all burst reads, verify intensity of every
                        # pixel is reduced as expected
                        self.assertEqual(ctx.get(dut.bus.dat_w),
                                         0xefefef00 | (ix&0xf))
                    await ctx.tick()
                    ix = ix + 1
                ctx.set(dut.bus.ack, 0)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        with sim.write_vcd(vcd_file=open("test_persist.vcd", "w")):
            sim.run()

    def test_persist_freeze(self):

        m = Module()
        fb = framebuffer.DMAFramebuffer(
            fixed_modeline=self.MODELINE, palette=palette.ColorPalette())
        # freeze_rows=4 -> header_words = 4*h_active/4 = h_active = 1280 words.
        # The handful of bursts below stay well under that offset, so every
        # written pixel is inside the frozen band and must be unchanged.
        dut = persist.Persistance(bus_signature=fb.bus.signature, freeze_rows=4)
        wiring.connect(m, wiring.flipped(fb.fbp), dut.fbp)
        check = wishbone.BusChecker(dut.bus, prefix='[bus] ')
        m.submodules += [dut, fb, fb.palette, check]

        writes_seen = 0

        async def testbench(ctx):
            nonlocal writes_seen
            ctx.set(fb.fbp.enable, 1)
            for _ in range(4):
                while not ctx.get(dut.bus.stb):
                    await ctx.tick()
                await ctx.tick().repeat(8)
                ctx.set(dut.bus.ack, 1)
                while ctx.get(dut.bus.stb):
                    # Constant full-intensity pixels (intensity nibble = 0xf).
                    ctx.set(dut.bus.dat_r, 0xffffffff)
                    if ctx.get(dut.bus.we):
                        # Frozen band: written back unchanged (NOT decayed to 0xef).
                        self.assertEqual(ctx.get(dut.bus.dat_w), 0xffffffff)
                        writes_seen += 1
                    await ctx.tick()
                ctx.set(dut.bus.ack, 0)
            # Guard against a false green: if the write burst were ever skipped,
            # the assertion above would run zero times and the test would still
            # pass. Require at least one checked write.
            self.assertGreater(writes_seen, 0)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        with sim.write_vcd(vcd_file=open("test_persist_freeze.vcd", "w")):
            sim.run()

    def test_persist_freeze_left(self):
        # At 720x720 the framebuffer HAL forces Rotate::Left (round-screen
        # hack), so the firmware's logical top menu band (y < freeze_rows) is
        # written to the *last* freeze_rows COLUMNS of every memory row, not the
        # first freeze_rows rows. The frozen band must follow the rotation:
        # freeze the trailing column band, decay the leading columns.
        MODELINE = modeline.DVIModeline.all_timings()["720x720p60r2"]
        freeze_rows = 200
        row_words = MODELINE.h_active // 4               # 180 words / memory row
        col_threshold = (MODELINE.h_active - freeze_rows) // 4  # 130

        m = Module()
        fb = framebuffer.DMAFramebuffer(
            fixed_modeline=MODELINE, palette=palette.ColorPalette())
        dut = persist.Persistance(bus_signature=fb.bus.signature,
                                  freeze_rows=freeze_rows)
        wiring.connect(m, wiring.flipped(fb.fbp), dut.fbp)
        check = wishbone.BusChecker(dut.bus, prefix='[bus] ')
        m.submodules += [dut, fb, fb.palette, check]

        frozen_seen = 0
        decayed_seen = 0

        async def testbench(ctx):
            nonlocal frozen_seen, decayed_seen
            ctx.set(fb.fbp.enable, 1)
            ctx.set(fb.fbp.rotation, Rotation.LEFT)
            # Enough bus cycles to sweep past one full memory row (180 words):
            # read- and write-bursts alternate (~16 words each), so we need
            # plenty to cross col_threshold (130) and the row wrap and thus
            # exercise both the leading (decayed) and trailing (frozen) columns.
            for _ in range(48):
                while not ctx.get(dut.bus.stb):
                    await ctx.tick()
                await ctx.tick().repeat(8)
                ctx.set(dut.bus.ack, 1)
                while ctx.get(dut.bus.stb):
                    # Constant full-intensity pixels (intensity nibble = 0xf).
                    ctx.set(dut.bus.dat_r, 0xffffffff)
                    if ctx.get(dut.bus.we):
                        # Write offset == dma_offs_out-1 (fbp.base == 0 in sim);
                        # the freeze decision uses col_word == dma_offs_out mod
                        # row_words == (adr+1) mod row_words.
                        adr = ctx.get(dut.bus.adr)
                        col = (adr + 1) % row_words
                        if col >= col_threshold:
                            # Trailing column band: frozen (NOT decayed).
                            self.assertEqual(ctx.get(dut.bus.dat_w), 0xffffffff)
                            frozen_seen += 1
                        else:
                            # Leading columns: decayed (0xf -> 0xe intensity).
                            self.assertEqual(ctx.get(dut.bus.dat_w), 0xefefefef)
                            decayed_seen += 1
                    await ctx.tick()
                ctx.set(dut.bus.ack, 0)
            # Both arms must actually execute, else a false green.
            self.assertGreater(frozen_seen, 0)
            self.assertGreater(decayed_seen, 0)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        with sim.write_vcd(vcd_file=open("test_persist_freeze_left.vcd", "w")):
            sim.run()

    def test_stroke(self):

        m = Module()
        dut = stroke.Stroke()
        m.submodules += [dut]

        N = 8

        async def stimulus(ctx):
            # Send a few sample points to the stroke
            for n in range(N):
                await stream.put(ctx, dut.i, [0, 0, 0, 0])
                await ctx.tick().repeat(4)

        async def testbench(ctx):
            for _ in range(N):
                # Test passes if we got something
                _ = await stream.get(ctx, dut.o)
                await ctx.tick()

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.add_process(stimulus)
        with sim.write_vcd(vcd_file=open("test_stroke.vcd", "w")):
            sim.run()

    def test_plot_backend(self):

        m = Module()
        fb = framebuffer.DMAFramebuffer(
            fixed_modeline=self.MODELINE, palette=palette.ColorPalette())
        dut = plot._FramebufferBackend(
            wishbone.Signature(addr_width=fb.bus.addr_width, data_width=32, granularity=8)
        )
        wiring.connect(m, wiring.flipped(fb.fbp), dut.fbp)
        check = wishbone.BusChecker(dut.bus, prefix='[bus] ')
        m.submodules += [dut, fb, fb.palette, check]

        async def testbench(ctx):
            ctx.set(fb.fbp.enable, 1)
            # Absolute positioning with replacement
            await stream.put(ctx, dut.i, {
                'x': 1,
                'y': 0,
                'pixel': {
                    'color': 0xa,
                    'intensity': 0xb,
                },
                'blend': plot.BlendMode.REPLACE,
                'offset': plot.OffsetMode.ABSOLUTE,
            })
            result = await wishbone.classic_ack(ctx, dut.bus)
            self.assertEqual(result.adr, 0x0)
            self.assertEqual(result.dat_w, 0xbabababa)
            self.assertEqual(result.sel, 0b0010)

            # Center positioning with replacement
            await stream.put(ctx, dut.i, {
                'x': 1,
                'y': 0,
                'pixel': {
                    'color': 0xa,
                    'intensity': 0xb,
                },
                'blend': plot.BlendMode.REPLACE,
                'offset': plot.OffsetMode.CENTER,
            })
            result = await wishbone.classic_ack(ctx, dut.bus)
            self.assertEqual(
                result.adr,
                int(self.MODELINE.h_active/4*(self.MODELINE.v_active/2 + 1/2)))
            self.assertEqual(result.dat_w, 0xbabababa)
            self.assertEqual(result.sel, 0b0010)

            # Absolute positioning, additive blending
            await stream.put(ctx, dut.i, {
                'x': 1,
                'y': 0,
                'pixel': {
                    'color': 0xa,
                    'intensity': 0xb,
                },
                'blend': plot.BlendMode.ADDITIVE,
                'offset': plot.OffsetMode.ABSOLUTE,
            })
            # Read cycle (get current pixel value)
            ctx.set(dut.bus.dat_r, 0x00009100)
            result = await wishbone.classic_ack(ctx, dut.bus)
            self.assertEqual(result.adr, 0x0)
            self.assertEqual(result.sel, 0b0010)
            # Write cycle (put newly calculated pixel value)
            result = await wishbone.classic_ack(ctx, dut.bus)
            self.assertEqual(result.adr, 0x0)
            self.assertEqual(result.sel, 0b0010)
            self.assertEqual(result.dat_w, 0xfafafafa)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        with sim.write_vcd(vcd_file=open("test_plot_backend.vcd", "w")):
            sim.run()

    def test_blit_peripheral(self):

        """
        Write a spritesheet to the blitter and blit a sub-rectangle of it, recording
        the plotted points into an ASCII grid for inspection.
        """

        m = Module()
        dut = blit.Peripheral()
        decoder = csr.Decoder(addr_width=28, data_width=8)
        decoder.add(dut.csr_bus, addr=0, name="dut")
        bridge = csr_wishbone.WishboneCSRBridge(decoder.bus, data_width=32)
        m.submodules += [dut, decoder, bridge]

        async def test_stimulus(ctx):

            async def csr_write(ctx, register, fields):
                await csr_util.wb_csr_w_dict(
                        ctx, dut.csr_bus, bridge.wb_bus, register, fields)

            # Write spritesheet to the blitter - read 32-bit words from file in the same
            # way that a RISCV32 would memcpy a raw spritesheet to sprite memory.
            word_addr = 0
            with open('tests/data/font_9x15.raw', 'rb') as f:
                while True:
                    word_bytes = f.read(4)
                    if not word_bytes:
                        break
                    if len(word_bytes) < 4:
                        word_bytes += b'\x00' * (4 - len(word_bytes))
                    word_data = struct.unpack('<I', word_bytes)[0]
                    await wishbone.classic_wr(ctx, dut.sprite_mem_bus, adr=word_addr, dat_w=word_data)
                    word_addr += 1

            # Set spritesheet width so core can index it properly
            sheet_width = 144
            sheet_height = 90 # (width*height)//32 must fit in the sprite mem
            await csr_write(ctx, "sheet_width", {
                "width": sheet_width,
            })

            # Issue a blit
            await csr_write(ctx, "src", {
                "src_x": 0,
                "src_y": 0,
                "width": 0x28,
                "height": 0x24,
            })

            await csr_write(ctx, "blit", {
                "pixel": 0xab,
                "dst_x": 5,
                "dst_y": 3,
            })

            while True:
                await ctx.tick()

        async def test_response(ctx):
            # Collect all the blitted points into an ASCII grid, and print it
            ctx.set(dut.o.ready, 1)
            points = set()
            for _ in range(8000):
                if ctx.get(dut.o.valid):
                    p_x = ctx.get(dut.o.payload.x)
                    p_y = ctx.get(dut.o.payload.y)
                    points.add((p_x, p_y))
                await ctx.tick()
            print()
            for y in range(40):
                row = ''
                for x in range(50):
                    if (x, y) in points:
                        row = row+'#'
                    else:
                        row = row+'.'
                print(row)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(test_stimulus, background=True)
        sim.add_testbench(test_response)
        with sim.write_vcd(vcd_file=open("test_blit_peripheral.vcd", "w")):
            sim.run()

    def test_line_peripheral(self):

        """
        Draw lines using the line plotter, recording the plotted points into
        an ASCII grid for inspection.
        """

        m = Module()
        dut = line.Peripheral()
        decoder = csr.Decoder(addr_width=28, data_width=8)
        decoder.add(dut.csr_bus, addr=0, name="dut")
        bridge = csr_wishbone.WishboneCSRBridge(decoder.bus, data_width=32)
        m.submodules += [dut, decoder, bridge]

        async def test_stimulus(ctx):

            async def csr_write(ctx, register, fields):
                await csr_util.wb_csr_w_dict(
                        ctx, dut.csr_bus, bridge.wb_bus, register, fields)

            # Draw a closed triangle line strip

            await csr_write(ctx, "point", {
                "cmd": 0,
                "pixel": 0xff,
                "x": 10,
                "y": 5,
            })

            await csr_write(ctx, "point", {
                "cmd": 0,
                "pixel": 0xff,
                "x": 30,
                "y": 5,
            })

            await csr_write(ctx, "point", {
                "cmd": 0,
                "pixel": 0xff,
                "x": 20,
                "y": 20,
            })

            await csr_write(ctx, "point", {
                "cmd": 1, # end
                "pixel": 0xff,
                "x": 10,
                "y": 5,
            })

            # Draw a single isolated line.

            await csr_write(ctx, "point", {
                "cmd": 0,
                "pixel": 0xff,
                "x": 4,
                "y": 5,
            })

            await csr_write(ctx, "point", {
                "cmd": 1, # end
                "pixel": 0xff,
                "x": 12,
                "y": 20,
            })

            while True:
                await ctx.tick()

        async def test_response(ctx):
            # Collect all the plotted points into an ASCII grid
            ctx.set(dut.o.ready, 1)
            points = set()
            for _ in range(5000):
                if ctx.get(dut.o.valid):
                    p_x = ctx.get(dut.o.payload.x)
                    p_y = ctx.get(dut.o.payload.y)
                    points.add((p_x, p_y))
                await ctx.tick()
            print()
            for y in range(25):
                row = ''
                for x in range(40):
                    if (x, y) in points:
                        row = row+'#'
                    else:
                        row = row+'.'
                print(row)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(test_stimulus, background=True)
        sim.add_testbench(test_response)
        with sim.write_vcd(vcd_file=open("test_line_peripheral.vcd", "w")):
            sim.run()
