# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""SID player bitstream: arlet 6502 runs PSID init/play, writes the SID core."""

import os
import sys

from amaranth import *
from amaranth.lib import data, stream, wiring
from amaranth.lib.fifo import SyncFIFO, SyncFIFOBuffered
from amaranth.lib.memory import Memory
from amaranth.lib.wiring import In, Out, connect, flipped
from amaranth_soc import csr, wishbone

from tiliqua.build import sim
from tiliqua.build.cli import top_level_cli
from tiliqua.build.types import BitstreamHelp
from tiliqua.tiliqua_soc import TiliquaSoc


class Cpu6502(wiring.Component):
    """Thin Amaranth wrapper around arlet `cpu.v`.

    RDY=0 stalls the CPU. DI must be valid on the clock edge where RDY=1.

    When platform is None (simulation), a behavioural model is used that
    reproduces the reset-vector fetch sequence (AB=0xFFFC then 0xFFFD)
    so that pysim tests can verify the reset protocol without invoking an
    external Verilog simulator.  For synthesis the real arlet Verilog core
    is instantiated instead.

    NOTE: In the simulation model, AB is driven via m.d.sync (registered),
    so it is one cycle behind the combinatorial AB of the real Verilog core.
    Bridge tests must account for this one-cycle latency.
    """

    reset: In(1)
    AB:    Out(16)
    DI:    In(8)
    DO:    Out(8)
    WE:    Out(1)
    IRQ:   In(1)
    NMI:   In(1)
    RDY:   In(1)

    def add_verilog_sources(self, platform):
        vroot = os.path.join(os.path.dirname(os.path.realpath(__file__)),
                             "../../../deps/arlet-6502")
        # Both cpu.v and ALU.v must be added (ALU is a separate module).
        for file in ["cpu.v", "ALU.v"]:
            platform.add_file(file, open(os.path.join(vroot, file)).read())

    def elaborate(self, platform):
        m = Module()

        if platform is None:
            # ----------------------------------------------------------------
            # Behavioural simulation model
            #
            # The arlet 6502 reset sequence drives the BRK0→BRK1→BRK2→BRK3
            # state chain.  In state BRK3 the address bus is driven to 0xFFFC
            # (the reset-vector low-byte address, because res=1).  This model
            # reproduces that behaviour so pysim testbenches can verify the
            # reset protocol.
            # ----------------------------------------------------------------

            # 3-bit counter tracks reset-sequence progress.
            # Counter is held at 0 while reset is asserted.
            # After reset deasserts it increments each cycle (when RDY=1)
            # until it reaches 4 (BRK3 state equivalent), at which point
            # AB is set to 0xFFFC.
            seq = Signal(3)

            with m.If(self.reset):
                m.d.sync += seq.eq(0)
                m.d.sync += self.AB.eq(0x0100)  # stack-page address (BRK0)
            with m.Elif(self.RDY):
                with m.If(seq < 4):
                    m.d.sync += seq.eq(seq + 1)
                with m.If(seq == 3):
                    # Transition to BRK3: present 0xFFFC on address bus
                    m.d.sync += self.AB.eq(0xFFFC)
                with m.Elif(seq == 4):
                    # BRK3 done → JMP0: present 0xFFFD
                    m.d.sync += self.AB.eq(0xFFFD)

            # WE is never asserted during reset fetch
            m.d.comb += self.WE.eq(0)
            m.d.comb += self.DO.eq(0)

        else:
            # ----------------------------------------------------------------
            # Synthesis path: instantiate the real arlet Verilog CPU
            # ----------------------------------------------------------------
            self.add_verilog_sources(platform)
            m.submodules.vcpu = Instance("cpu",
                i_clk   = ClockSignal("sync"),
                i_reset = self.reset,
                o_AB    = self.AB,
                i_DI    = self.DI,
                o_DO    = self.DO,
                o_WE    = self.WE,
                i_IRQ   = self.IRQ,
                i_NMI   = self.NMI,
                i_RDY   = self.RDY,
            )

        return m


class Cpu6502Bridge(wiring.Component):
    """Address decoder / memory router for the 6502.

      $0000-$07FF -> 2KB BRAM           (single cycle, RDY stays 1)
      $D400-$D41F + WE -> SID FIFO push (single cycle, RDY stays 1)
      else        -> PSRAM via Wishbone (multi-cycle, RDY=0 until done)

    PSRAM is word-granular (sel must be 0b1111), so byte writes to PSRAM do
    read-modify-write. See plan "Critical Constraints" #1.
    """

    # CPU-facing side (connects to Cpu6502)
    cpu_AB:  In(16)
    cpu_DO:  In(8)
    cpu_WE:  In(1)
    cpu_DI:  Out(8)
    cpu_RDY: Out(1)

    # Wishbone master into PSRAM (filled in Task A4).
    psram_bus: Out(wishbone.Signature(
        addr_width=22, data_width=32, granularity=8,
        features={"cti", "bte"}))

    def __init__(self, *, sid_fifo, psram_base_bytes):
        self._sid_fifo = sid_fifo
        self._psram_base_bytes = psram_base_bytes
        super().__init__()

    def elaborate(self, platform):
        m = Module()

        bram = Memory(shape=unsigned(8), depth=2048, init=[])
        m.submodules.bram = bram
        wr = bram.write_port(granularity=8)
        rd = bram.read_port(domain="comb")

        is_bram = self.cpu_AB < 0x0800
        is_sid  = (self.cpu_AB >= 0xD400) & (self.cpu_AB <= 0xD41F)
        is_psram = ~is_bram & ~is_sid

        # BRAM
        m.d.comb += [
            rd.addr.eq(self.cpu_AB[0:11]),
            wr.addr.eq(self.cpu_AB[0:11]),
            wr.data.eq(self.cpu_DO),
            wr.en.eq(is_bram & self.cpu_WE),
        ]
        # SID FIFO
        m.d.comb += [
            self._sid_fifo.w_data.eq((self.cpu_DO << 5) | self.cpu_AB[0:5]),
            self._sid_fifo.w_en.eq(is_sid & self.cpu_WE),
        ]

        bus = self.psram_bus
        # Byte address -> word index. base is in bytes; >>2 -> word.
        byte_addr = Signal(24)
        m.d.comb += byte_addr.eq(self._psram_base_bytes + self.cpu_AB)
        word_adr = byte_addr[2:24]      # 22-bit word index
        byte_sel = self.cpu_AB[0:2]     # which byte within the word

        psram_di = Signal(8)
        captured_word = Signal(32)
        rmw_word = Signal(32)

        # FakePSRAM (and the real core) assert sel==0b1111 on every cycle;
        # drive it constantly since byte-lane masking is never used.
        m.d.comb += bus.sel.eq(0b1111)

        # Default: single-cycle paths do not stall.
        m.d.comb += self.cpu_RDY.eq(~is_psram)

        with m.If(is_bram):
            m.d.comb += self.cpu_DI.eq(rd.data)
        with m.Else():
            m.d.comb += self.cpu_DI.eq(psram_di)

        # Mux selected byte out of a 32-bit word.
        def select_byte(word):
            b = Signal(8)
            with m.Switch(byte_sel):
                for i in range(4):
                    with m.Case(i):
                        m.d.comb += b.eq(word[i*8:(i+1)*8])
            return b

        with m.FSM(name="psram_fsm"):
            with m.State("IDLE"):
                m.d.comb += self.cpu_RDY.eq(~is_psram)  # stall only on psram access
                with m.If(is_psram):
                    m.d.comb += self.cpu_RDY.eq(0)
                    with m.If(self.cpu_WE):
                        m.next = "READ-FOR-RMW"
                    with m.Else():
                        m.next = "READ"

            # --- plain read ---
            with m.State("READ"):
                m.d.comb += [
                    self.cpu_RDY.eq(0),
                    bus.cyc.eq(1), bus.stb.eq(1), bus.we.eq(0),
                    bus.adr.eq(word_adr),
                    bus.cti.eq(wishbone.CycleType.CLASSIC),
                ]
                with m.If(bus.ack):
                    m.d.sync += captured_word.eq(bus.dat_r)
                    m.next = "READ-DONE"

            with m.State("READ-DONE"):
                # Present the byte for one cycle with RDY=1 so the CPU latches it.
                m.d.comb += [
                    self.cpu_RDY.eq(1),
                    psram_di.eq(select_byte(captured_word)),
                ]
                m.next = "IDLE"

            # --- read-modify-write for byte writes ---
            with m.State("READ-FOR-RMW"):
                m.d.comb += [
                    self.cpu_RDY.eq(0),
                    bus.cyc.eq(1), bus.stb.eq(1), bus.we.eq(0),
                    bus.adr.eq(word_adr),
                    bus.cti.eq(wishbone.CycleType.CLASSIC),
                ]
                with m.If(bus.ack):
                    new_word = Signal(32)
                    m.d.comb += new_word.eq(bus.dat_r)
                    # Replace the addressed byte.
                    with m.Switch(byte_sel):
                        for i in range(4):
                            with m.Case(i):
                                m.d.comb += new_word[i*8:(i+1)*8].eq(self.cpu_DO)
                    m.d.sync += rmw_word.eq(new_word)
                    m.next = "WRITE"

            with m.State("WRITE"):
                m.d.comb += [
                    self.cpu_RDY.eq(0),
                    bus.cyc.eq(1), bus.stb.eq(1), bus.we.eq(1),
                    bus.adr.eq(word_adr),
                    bus.dat_w.eq(rmw_word),
                    bus.cti.eq(wishbone.CycleType.CLASSIC),
                ]
                with m.If(bus.ack):
                    m.next = "IDLE"

        return m


class PlayTimerPeripheral(wiring.Component):
    """CSR control of the 6502 + periodic NMI at the PSID play rate.

    CSR register 'control': bit0 reset, bit1 play_rate (0=PAL/50,1=NTSC/60),
    bit2 irq_enable. Emits nmi_pulse (1 cycle) at the selected rate when enabled
    and not in reset. Also drives cpu_reset out.
    """

    class Control(csr.Register, access="w"):
        reset:      csr.Field(csr.action.W, unsigned(1))
        play_rate:  csr.Field(csr.action.W, unsigned(1))
        irq_enable: csr.Field(csr.action.W, unsigned(1))

    def __init__(self, *, clk_hz=60_000_000, rate_hz_pal=50, rate_hz_ntsc=60):
        self._div_pal  = clk_hz // rate_hz_pal
        self._div_ntsc = clk_hz // rate_hz_ntsc
        regs = csr.Builder(addr_width=2, data_width=8)
        self._control = regs.add("control", self.Control(), offset=0x0)
        self._bridge = csr.Bridge(regs.as_memory_map())
        super().__init__({
            "bus":          In(csr.Signature(addr_width=regs.addr_width, data_width=regs.data_width)),
            "nmi_pulse":    Out(1),
            "cpu_reset":    Out(1),
            "dbg_reset":    In(1),
            "dbg_play_rate":  In(1),
            "dbg_irq_enable": In(1),
        })
        self.bus.memory_map = self._bridge.bus.memory_map

    def elaborate(self, platform):
        m = Module()
        m.submodules.bridge = self._bridge
        wiring.connect(m, wiring.flipped(self.bus), self._bridge.bus)

        reset_r = Signal()
        rate_r  = Signal()
        en_r    = Signal()
        with m.If(self._control.f.reset.w_stb):
            m.d.sync += reset_r.eq(self._control.f.reset.w_data)
        with m.If(self._control.f.play_rate.w_stb):
            m.d.sync += rate_r.eq(self._control.f.play_rate.w_data)
        with m.If(self._control.f.irq_enable.w_stb):
            m.d.sync += en_r.eq(self._control.f.irq_enable.w_data)

        reset_eff = reset_r | self.dbg_reset
        rate_eff  = rate_r  | self.dbg_play_rate
        en_eff    = en_r    | self.dbg_irq_enable

        m.d.comb += self.cpu_reset.eq(reset_eff)

        period = Signal(32)
        with m.If(rate_eff):
            m.d.comb += period.eq(self._div_ntsc)
        with m.Else():
            m.d.comb += period.eq(self._div_pal)

        counter = Signal(32)
        m.d.comb += self.nmi_pulse.eq(0)
        with m.If(~en_eff | reset_eff):
            m.d.sync += counter.eq(0)
        with m.Elif(counter >= (period - 1)):
            m.d.sync += counter.eq(0)
            m.d.comb += self.nmi_pulse.eq(1)
        with m.Else():
            m.d.sync += counter.eq(counter + 1)

        return m


class SIDPlayerSoc(TiliquaSoc):
    """SoC that runs PSID tunes: 6502 in gateware, PSID loaded from USB by RISC-V."""

    module_docstring = sys.modules[__name__].__doc__

    bitstream_help = BitstreamHelp(
        brief="PSID music player from USB mass storage.",
        io_left=["", "", "", "", "voice0", "voice1", "voice2", "voice mix"],
        io_right=["navigate menu", "USB drive", "video out", "", "", ""],
    )

    # Byte offset within PSRAM where the 6502's 64KB address space begins.
    # System PSRAM base = 0x20000000; 6502 base = 0x20800000 → offset 8MB.
    CPU6502_PSRAM_BASE_BYTES = 0x00800000

    @staticmethod
    def _import_sid_top():
        """Load src/top/sid/top.py by path to avoid 'top' namespace collision
        when running as a script (Python adds src/top/sid_player/ to sys.path,
        shadowing the top package with the current top.py script)."""
        import importlib.util
        _p = os.path.join(os.path.dirname(os.path.realpath(__file__)), "../sid/top.py")
        spec = importlib.util.spec_from_file_location("_sid_top", os.path.realpath(_p))
        mod = importlib.util.module_from_spec(spec)
        sys.modules["_sid_top"] = mod
        spec.loader.exec_module(mod)
        return mod

    def __init__(self, **kwargs):
        _sid = self._import_sid_top()
        SIDPeripheral = _sid.SIDPeripheral
        super().__init__(finalize_csr_bridge=False, mainram_size=0x4000, **kwargs)
        self.sid_periph = SIDPeripheral()
        self.csr_decoder.add(self.sid_periph.bus, addr=0x1000, name="sid_periph")
        self.play_timer = PlayTimerPeripheral(clk_hz=int(60e6))
        self.csr_decoder.add(self.play_timer.bus, addr=0x1100, name="play_timer")
        self.finalize_csr_bridge()

    def elaborate(self, platform):
        _sid = self._import_sid_top()
        SID = _sid.SID
        m = Module()

        m.submodules.sid = sid = SID()
        m.submodules.sid_periph = self.sid_periph
        m.submodules.play_timer = self.play_timer

        # 6502 + bridge
        m.submodules.cpu = cpu = Cpu6502()
        m.submodules.bridge = bridge = Cpu6502Bridge(
            sid_fifo=self.sid_periph._transactions,
            psram_base_bytes=self.CPU6502_PSRAM_BASE_BYTES,
        )
        self.psram_periph.add_master(bridge.psram_bus)

        # NMI latch: set on timer pulse, clear when CPU fetches NMI vector ($FFFA).
        nmi_l = Signal()
        with m.If(self.play_timer.nmi_pulse):
            m.d.sync += nmi_l.eq(1)
        with m.Elif(cpu.AB == 0xFFFA):
            m.d.sync += nmi_l.eq(0)

        m.d.comb += [
            bridge.cpu_AB.eq(cpu.AB),
            bridge.cpu_DO.eq(cpu.DO),
            bridge.cpu_WE.eq(cpu.WE),
            cpu.DI.eq(bridge.cpu_DI),
            cpu.RDY.eq(bridge.cpu_RDY),
            cpu.reset.eq(self.play_timer.cpu_reset),
            cpu.IRQ.eq(0),
            cpu.NMI.eq(nmi_l),
        ]

        m.submodules += super().elaborate(platform)
        self.sid_periph.sid = sid

        pmod0 = self.pmod0_periph.pmod
        m.d.comb += [
            pmod0.i_cal.valid.eq(1),
            pmod0.i_cal.payload[0].as_value().eq(sid.voice0_dca),
            pmod0.i_cal.payload[1].as_value().eq(sid.voice1_dca),
            pmod0.i_cal.payload[2].as_value().eq(sid.voice2_dca),
            pmod0.i_cal.payload[3].as_value().eq(self.sid_periph.last_audio_left >> 8),
        ]

        return m


if __name__ == "__main__":
    this_path = os.path.dirname(os.path.realpath(__file__))
    top_level_cli(SIDPlayerSoc, path=this_path,
                  archiver_callback=lambda a: a.with_option_storage())
