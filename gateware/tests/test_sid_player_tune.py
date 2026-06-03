import struct
import unittest

from amaranth import *
from amaranth.lib import wiring
from amaranth.lib.fifo import SyncFIFO
from amaranth.sim import Simulator

from top.sid_player.top import Cpu6502Bridge, PlayTimerPeripheral
from tiliqua.test import psram as test_psram


def image_to_words(path):
    with open(path, "rb") as f:
        data = f.read()
    return [struct.unpack_from("<I", data, i)[0] for i in range(0, len(data), 4)]


class SidPlayerTuneTests(unittest.TestCase):
    def test_play_routine_writes_sid_register(self):
        bridge = Cpu6502Bridge(psram_base_bytes=0x0)
        # 2000 Hz / 50 Hz = 40-cycle NMI period (fast enough to see 2+ writes).
        timer = PlayTimerPeripheral(clk_hz=2000, rate_hz_pal=50, rate_hz_ntsc=60)

        # Pre-initialise FakePSRAM with the full 64KB image (16384 words).
        init_words = image_to_words("tests/data/tiny_tune.bin")
        fake = test_psram.FakePSRAM(
            addr_width=22, data_width=32,
            storage_words=len(init_words), latency_cycles=4,
            init_words=init_words,
        )

        # External FIFO fed by bridge's SID write output ports.
        sid_fifo = SyncFIFO(width=16, depth=64)

        m = Module()
        m.submodules.bridge = bridge
        m.submodules.timer = timer
        m.submodules.sid_fifo = sid_fifo
        m.submodules.fake = fake
        wiring.connect(m, bridge.psram_bus, fake.bus)
        m.d.comb += [
            sid_fifo.w_en  .eq(bridge.sid_w_en),
            sid_fifo.w_data.eq(bridge.sid_w_data),
        ]

        # Sticky NMI latch: set by timer pulse, clearable by testbench.
        nmi_l = Signal()
        nmi_clear = Signal()
        with m.If(nmi_clear):
            m.d.sync += nmi_l.eq(0)
        with m.Elif(timer.nmi_pulse):
            m.d.sync += nmi_l.eq(1)

        async def testbench(ctx):
            ctx.set(timer.dbg_irq_enable, 1)
            ctx.set(timer.dbg_reset, 0)

            async def mem_read(addr):
                ctx.set(bridge.cpu_WE, 0)
                ctx.set(bridge.cpu_AB, addr & 0xFFFF)
                await ctx.tick()
                while not ctx.get(bridge.cpu_RDY):
                    await ctx.tick()
                return ctx.get(bridge.cpu_DI)

            async def mem_write(addr, val):
                ctx.set(bridge.cpu_WE, 1)
                ctx.set(bridge.cpu_AB, addr & 0xFFFF)
                ctx.set(bridge.cpu_DO, val & 0xFF)
                await ctx.tick()
                while not ctx.get(bridge.cpu_RDY):
                    await ctx.tick()
                ctx.set(bridge.cpu_WE, 0)

            # Boot: fetch reset vector, form PC.
            lo = await mem_read(0xFFFC)
            hi = await mem_read(0xFFFD)
            pc = (hi << 8) | lo  # expects $0800 from tiny_tune.bin

            regs = {"A": 0}
            seen = 0

            for _ in range(8000):
                # Handle NMI between instructions (latched sticky signal).
                if ctx.get(nmi_l):
                    ctx.set(nmi_clear, 1)
                    await ctx.tick()
                    ctx.set(nmi_clear, 0)
                    lo = await mem_read(0xFFFA)
                    hi = await mem_read(0xFFFB)
                    pc = (hi << 8) | lo  # NMI vector: $0820

                # Fetch and execute one instruction.
                opcode = await mem_read(pc)
                pc = (pc + 1) & 0xFFFF

                if opcode == 0xA9:          # LDA imm
                    regs["A"] = await mem_read(pc)
                    pc = (pc + 1) & 0xFFFF

                elif opcode == 0x8D:        # STA abs
                    lo = await mem_read(pc)
                    hi = await mem_read(pc + 1)
                    pc = (pc + 2) & 0xFFFF
                    await mem_write((hi << 8) | lo, regs["A"])

                elif opcode == 0x4C:        # JMP abs
                    lo = await mem_read(pc)
                    hi = await mem_read(pc + 1)
                    pc = (hi << 8) | lo

                elif opcode == 0xEE:        # INC abs
                    lo = await mem_read(pc)
                    hi = await mem_read(pc + 1)
                    pc = (pc + 2) & 0xFFFF
                    addr = (hi << 8) | lo
                    val = await mem_read(addr)
                    await mem_write(addr, (val + 1) & 0xFF)

                elif opcode == 0x40:        # RTI — return to spin loop
                    pc = 0x0805

                # Drain any SID FIFO write that just happened.
                if ctx.get(sid_fifo.r_level) > 0:
                    seen += 1
                    ctx.set(sid_fifo.r_en, 1)
                    await ctx.tick()
                    ctx.set(sid_fifo.r_en, 0)

                if seen >= 2:
                    break

            self.assertGreaterEqual(
                seen, 2, "play routine never wrote SID registers via NMI trampoline"
            )

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()


if __name__ == "__main__":
    unittest.main()
