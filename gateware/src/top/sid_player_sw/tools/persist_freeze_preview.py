#!/usr/bin/env python3
# Framebuffer dump of the persist *freeze* region, straight from the real
# `Persistance` gateware, for each display rotation.
#
# Why: at 720x720 the framebuffer HAL forces Rotate::Left (round-screen hack),
# so the firmware's logical top menu band is plotted into the *trailing columns*
# of every framebuffer-memory row, not the first rows. The persist freeze must
# protect that same region or the menu decays away (flash-on-input bug). This
# tool makes the protected region visible without needing hardware or a full
# (impractically slow) SoC sim:
#
#   - fill the framebuffer with full-intensity pixels,
#   - run ONE persist decay pass with decay=15 (max): unfrozen pixels drop to
#     intensity 0 (black), frozen pixels stay at 0xf (white),
#   - dump the framebuffer intensity as a PNG.
#
# The white band IS the frozen region. Under NORMAL it's the top rows (external
# monitor); under LEFT it's the right columns (the 720x720 round screen) — which
# is where the rotated menu lands. INVERTED/RIGHT fall back to the NORMAL band.
#
# Run (needs the venv with numpy + matplotlib):
#   gateware/.venv/bin/python src/top/sid_player_sw/tools/persist_freeze_preview.py
#   gateware/.venv/bin/python .../persist_freeze_preview.py --size 720 --rotation left
#
# Output: persist_freeze_<rotation>.png in the cwd.

import argparse

import numpy as np
from amaranth import *
from amaranth.lib import wiring
from amaranth.sim import Simulator

from tiliqua.raster import persist
from tiliqua.video import framebuffer, modeline, palette
from tiliqua.video.types import Rotation


ROTATIONS = {
    "normal":   Rotation.NORMAL,
    "left":     Rotation.LEFT,
    "inverted": Rotation.INVERTED,
    "right":    Rotation.RIGHT,
}


def make_modeline(size):
    """A square modeline of the requested side length. Only h_active/v_active
    (and the derived active_pixels) matter here — the dvi domain is never
    clocked in this harness — so the sync/total values are just plausible
    filler. size==720 uses the real production round-screen modeline."""
    if size == 720:
        return modeline.DVIModeline.all_timings()["720x720p60r2"]
    return modeline.DVIModeline(
        h_active=size, h_sync_start=size + 8, h_sync_end=size + 16,
        h_total=size + 32, h_sync_invert=False,
        v_active=size, v_sync_start=size + 4, v_sync_end=size + 8,
        v_total=size + 16, v_sync_invert=False, pixel_clk_mhz=30.0)


def dump(size, rotation_name):
    rotation = ROTATIONS[rotation_name]
    ml = make_modeline(size)
    h_active, v_active = ml.h_active, ml.v_active
    freeze_rows = max(1, round(size * 200 / 720))  # 200 rows @ 720, scaled
    fb_len_words = (h_active * v_active) // 4       # 8-bit pixels, 4 per word

    m = Module()
    fb = framebuffer.DMAFramebuffer(fixed_modeline=ml, palette=palette.ColorPalette())
    dut = persist.Persistance(bus_signature=fb.bus.signature, freeze_rows=freeze_rows)
    wiring.connect(m, wiring.flipped(fb.fbp), dut.fbp)
    m.submodules += [dut, fb, fb.palette]

    # Backing store: every pixel full-intensity (byte 0xff = color 0xf, intensity 0xf).
    mem = np.full(fb_len_words, 0xffffffff, dtype=np.uint64)
    writes_total = 0

    async def testbench(ctx):
        nonlocal writes_total
        ctx.set(fb.fbp.enable, 1)
        ctx.set(fb.fbp.rotation, rotation)
        ctx.set(dut.holdoff, 0)   # back-to-back bursts (fast)
        ctx.set(dut.decay, 15)    # one pass fully decays the unfrozen region
        ctx.set(dut.skip, 0)
        # Service the persist DMA against `mem` until a whole frame of writes
        # has been drained (one decay pass — idempotent after that).
        safety = 0
        max_bursts = 8 * (fb_len_words // 16 + 1)  # backstop, ~4x one frame
        while writes_total < fb_len_words and safety < max_bursts:
            safety += 1
            while not ctx.get(dut.bus.stb):
                await ctx.tick()
            await ctx.tick()
            ctx.set(dut.bus.ack, 1)
            while ctx.get(dut.bus.stb):
                adr = ctx.get(dut.bus.adr) % fb_len_words
                if ctx.get(dut.bus.we):
                    mem[adr] = ctx.get(dut.bus.dat_w)
                    writes_total += 1
                else:
                    ctx.set(dut.bus.dat_r, int(mem[adr]))
                await ctx.tick()
            ctx.set(dut.bus.ack, 0)

    sim = Simulator(m)
    sim.add_clock(1e-6)
    sim.add_testbench(testbench)
    sim.run()

    # intensity nibble (bits 4..7) of each pixel byte -> grayscale image.
    px = mem.astype(np.uint32).view(np.uint8)[:h_active * v_active]
    intensity = (px >> 4) & 0x0f
    img = intensity.reshape(v_active, h_active)

    out = f"persist_freeze_{rotation_name}.png"
    import matplotlib.pyplot as plt
    plt.imsave(out, img, cmap="gray", vmin=0, vmax=15)
    frozen = int((img == 0xf).sum())
    print(f"{rotation_name:8s} {h_active}x{v_active} freeze_rows={freeze_rows}: "
          f"wrote {out}  (frozen pixels: {frozen} = {100*frozen/img.size:.1f}%)")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--size", type=int, default=240,
                    help="square side length (default 240 = fast; use 720 for "
                         "the exact production round-screen geometry)")
    ap.add_argument("--rotation", choices=list(ROTATIONS) + ["all"], default="all",
                    help="which rotation(s) to dump (default: all)")
    args = ap.parse_args()
    names = list(ROTATIONS) if args.rotation == "all" else [args.rotation]
    for name in names:
        dump(args.size, name)


if __name__ == "__main__":
    main()
