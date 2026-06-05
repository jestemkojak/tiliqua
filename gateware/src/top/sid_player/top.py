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

from luna.gateware.stream.future import Packet

from tiliqua import dsp
from tiliqua.build import sim
from tiliqua.build.cli import top_level_cli
from tiliqua.build.types import BitstreamHelp
from tiliqua.raster import PSQ, scope
from tiliqua.raster.plot import FramebufferPlotter
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

    # SID write outputs: connect to SIDPeripheral.ext_w_en / ext_w_data
    sid_w_en:   Out(1)
    sid_w_data: Out(16)

    def __init__(self, *, psram_base_bytes):
        self._psram_base_bytes = psram_base_bytes
        super().__init__()

    def elaborate(self, platform):
        m = Module()

        # Bus-cycle window length in sys-clk cycles.  60 MHz / 64 ≈ 0.94 MHz
        # effective 6502 speed — appropriate for a SID player and ample for a
        # single arbitrated PSRAM word (real HyperRAM single-word well under 64
        # cycles even with arbiter contention).
        N = 64

        bram = Memory(shape=unsigned(8), depth=2048, init=[])
        m.submodules.bram = bram
        wr = bram.write_port(granularity=8)
        rd = bram.read_port(domain="comb")

        # Window-stable registered copies of the CPU bus, latched at phase==0
        # each window.  All bridge logic uses *_r so cpu_AB is never live.
        cpu_AB_r  = Signal(16)
        cpu_DO_r  = Signal(8)
        cpu_WE_r  = Signal()
        is_bram_r  = Signal()
        is_sid_r   = Signal()
        is_psram_r = Signal()

        # Free-running phase counter 0…N-1.  Saturates at N-1 while waiting
        # for a PSRAM access to complete; wraps to 0 on the advance pulse.
        # Driving RDY from this register (never from live cpu_AB) is what
        # breaks the  cpu_AB→RDY→DIMUX→cpu_AB  combinational loop in arlet.
        phase = Signal(6)

        bus = self.psram_bus
        byte_addr = Signal(24)
        m.d.comb += byte_addr.eq(self._psram_base_bytes + cpu_AB_r)
        word_adr_r = byte_addr[2:24]   # 22-bit word index (comb from registered AB)
        byte_sel_r = cpu_AB_r[0:2]

        m.d.comb += bus.sel.eq(0b1111)

        # PSRAM access state
        psram_done_r  = Signal()
        captured_word = Signal(32)
        rmw_word      = Signal(32)

        # BRAM read: combinational read port addressed by registered AB.
        m.d.comb += rd.addr.eq(cpu_AB_r[0:11])

        # Byte extractor from a captured 32-bit PSRAM word.
        psram_byte = Signal(8)
        with m.Switch(byte_sel_r):
            for i in range(4):
                with m.Case(i):
                    m.d.comb += psram_byte.eq(captured_word[i*8:(i+1)*8])

        # Window data mux: BRAM (comb) or captured PSRAM byte.
        data_k = Signal(8)
        with m.If(is_bram_r):
            m.d.comb += data_k.eq(rd.data)
        with m.Else():
            m.d.comb += data_k.eq(psram_byte)

        # Advance pulse: fires at phase==N-1 once the access is complete.
        # All operands are registers → no combinational path from cpu_AB.
        advance = Signal()
        m.d.comb += advance.eq((phase == N - 1) & (~is_psram_r | psram_done_r))
        m.d.comb += self.cpu_RDY.eq(advance)

        # Phase counter: wraps on advance, saturates at N-1 while awaiting PSRAM.
        with m.If(advance):
            m.d.sync += phase.eq(0)
        with m.Elif(phase < N - 1):
            m.d.sync += phase.eq(phase + 1)

        # cpu_DI: combinationally driven by the current window's data.
        # The arlet core reads DI at the end of the window (when advance/RDY is 1)
        # and registers DIHOLD at the same edge. During stalls (RDY=0) it uses
        # DIHOLD, keeping the address bus stable.
        m.d.comb += self.cpu_DI.eq(data_k)

        # Latch CPU bus at phase==0 (start of each new window) and reset PSRAM
        # done flag so the next window's access can proceed.
        #
        # Decode with bit-slice EQUALITY, not magnitude comparison.  During the
        # arlet's reset/BRK stack pushes the address is {STACKPAGE, S} = $01xx
        # with S (and thus AB[0:8]) undefined; a `<`/`>=` comparison against
        # such an operand yields X in Verilog, poisoning is_psram_r → advance →
        # cpu_RDY = X and wedging the core forever.  Equality on only the
        # high-order bits classifies $01xx as BRAM regardless of the X low byte.
        #   BRAM  $0000-$07FF  ⟺ AB[11:] == 0
        #   SID   $D400-$D41F  ⟺ AB[5:]  == (0xD400 >> 5)
        is_bram_next  = (self.cpu_AB[11:] == 0)
        is_sid_next   = (self.cpu_AB[5:] == (0xD400 >> 5))
        is_psram_next = ~is_bram_next & ~is_sid_next
        with m.If(advance):
            m.d.sync += [
                cpu_AB_r.eq(self.cpu_AB),
                cpu_DO_r.eq(self.cpu_DO),
                cpu_WE_r.eq(self.cpu_WE),
                is_bram_r.eq(is_bram_next),
                is_sid_r.eq(is_sid_next),
                is_psram_r.eq(is_psram_next),
                psram_done_r.eq(0),
            ]

        # BRAM write: commit on the advance pulse using registered signals.
        m.d.comb += [
            wr.addr.eq(cpu_AB_r[0:11]),
            wr.data.eq(cpu_DO_r),
            wr.en.eq(advance & is_bram_r & cpu_WE_r),
        ]

        # SID write: one-cycle pulse on advance (registered AB/DO, no comb path).
        m.d.comb += [
            self.sid_w_en.eq(advance & is_sid_r & cpu_WE_r),
            self.sid_w_data.eq((cpu_DO_r << 5) | cpu_AB_r[0:5]),
        ]

        # PSRAM access sub-FSM: runs within the window, sets psram_done_r when
        # the access completes so the advance pulse can fire at phase==N-1.
        with m.FSM(name="psram_fsm"):
            with m.State("IDLE"):
                with m.If((phase == 1) & is_psram_r):
                    with m.If(cpu_WE_r):
                        m.next = "RD-FOR-RMW"
                    with m.Else():
                        m.next = "READ"

            with m.State("READ"):
                m.d.comb += [
                    bus.cyc.eq(1), bus.stb.eq(1), bus.we.eq(0),
                    bus.adr.eq(word_adr_r),
                    bus.cti.eq(wishbone.CycleType.CLASSIC),
                ]
                with m.If(bus.ack):
                    m.d.sync += [captured_word.eq(bus.dat_r), psram_done_r.eq(1)]
                    m.next = "IDLE"

            with m.State("RD-FOR-RMW"):
                m.d.comb += [
                    bus.cyc.eq(1), bus.stb.eq(1), bus.we.eq(0),
                    bus.adr.eq(word_adr_r),
                    bus.cti.eq(wishbone.CycleType.CLASSIC),
                ]
                with m.If(bus.ack):
                    new_word = Signal(32)
                    m.d.comb += new_word.eq(bus.dat_r)
                    with m.Switch(byte_sel_r):
                        for i in range(4):
                            with m.Case(i):
                                m.d.comb += new_word[i*8:(i+1)*8].eq(cpu_DO_r)
                    m.d.sync += rmw_word.eq(new_word)
                    m.next = "WRITE"

            with m.State("WRITE"):
                m.d.comb += [
                    bus.cyc.eq(1), bus.stb.eq(1), bus.we.eq(1),
                    bus.adr.eq(word_adr_r),
                    bus.dat_w.eq(rmw_word),
                    bus.cti.eq(wishbone.CycleType.CLASSIC),
                ]
                with m.If(bus.ack):
                    m.d.sync += psram_done_r.eq(1)
                    m.next = "IDLE"

        return m


class PlayTimerPeripheral(wiring.Component):
    """CSR control of the 6502 + periodic NMI at the PSID play rate.

    CSR register 'control': bit0 reset, bit1 irq_enable. CSR register 'period':
    32-bit NMI divider in sys-clk cycles (firmware computes it from the tune's
    VBlank/CIA timing — see fw psid::play_period_cycles). Emits nmi_pulse (1
    cycle) every `period` cycles when enabled and not in reset; period 0 = never
    (safe default before firmware programs it). Also drives cpu_reset out.
    """

    class Control(csr.Register, access="w"):
        reset:      csr.Field(csr.action.W, unsigned(1))
        irq_enable: csr.Field(csr.action.W, unsigned(1))

    class Period(csr.Register, access="w"):
        value: csr.Field(csr.action.W, unsigned(32))

    def __init__(self, *, clk_hz=60_000_000):
        self._clk_hz = clk_hz
        regs = csr.Builder(addr_width=5, data_width=8)
        self._control = regs.add("control", self.Control(), offset=0x0)
        self._period  = regs.add("period",  self.Period(),  offset=0x4)
        self._bridge = csr.Bridge(regs.as_memory_map())
        super().__init__({
            "bus":          In(csr.Signature(addr_width=regs.addr_width, data_width=regs.data_width)),
            "nmi_pulse":    Out(1),
            "cpu_reset":    Out(1),
            # Sim-only stimulus: the control/period registers are write-only, so
            # unit tests drive these to exercise the timer without a CSR master.
            # Tied to 0 in hardware (firmware uses the CSRs).
            "dbg_reset":      In(1),
            "dbg_irq_enable": In(1),
            "dbg_period":     In(32),
        })
        self.bus.memory_map = self._bridge.bus.memory_map

    def elaborate(self, platform):
        m = Module()
        m.submodules.bridge = self._bridge
        wiring.connect(m, wiring.flipped(self.bus), self._bridge.bus)

        reset_r  = Signal()
        en_r     = Signal()
        period_r = Signal(32)
        with m.If(self._control.f.reset.w_stb):
            m.d.sync += reset_r.eq(self._control.f.reset.w_data)
        with m.If(self._control.f.irq_enable.w_stb):
            m.d.sync += en_r.eq(self._control.f.irq_enable.w_data)
        with m.If(self._period.f.value.w_stb):
            m.d.sync += period_r.eq(self._period.f.value.w_data)

        reset_eff  = reset_r  | self.dbg_reset
        en_eff     = en_r     | self.dbg_irq_enable
        period_eff = period_r | self.dbg_period

        m.d.comb += self.cpu_reset.eq(reset_eff)

        counter = Signal(32)
        m.d.comb += self.nmi_pulse.eq(0)
        with m.If(~en_eff | reset_eff | (period_eff == 0)):
            m.d.sync += counter.eq(0)
        with m.Elif(counter >= (period_eff - 1)):
            m.d.sync += counter.eq(0)
            m.d.comb += self.nmi_pulse.eq(1)
        with m.Else():
            m.d.sync += counter.eq(counter + 1)

        return m


_USB_STATUS_LAYOUT = data.StructLayout({
    "connected": 1, "ready": 1, "busy": 1,
    "block_size": 16, "block_count": 32})
_USB_RESP_LAYOUT = data.StructLayout({"done": 1, "error": 1})


class USBMSCPeripheral(wiring.Component):
    """CSR wrapper around guh USBMSCHost: status, LBA/start command, packed
    32-bit read-data FIFO drained by firmware."""

    class Status(csr.Register, access="r"):
        connected:   csr.Field(csr.action.R, unsigned(1))
        ready:       csr.Field(csr.action.R, unsigned(1))
        busy:        csr.Field(csr.action.R, unsigned(1))
        rx_avail:    csr.Field(csr.action.R, unsigned(1))

    class BlockSize(csr.Register, access="r"):
        value: csr.Field(csr.action.R, unsigned(16))

    class BlockCount(csr.Register, access="r"):
        value: csr.Field(csr.action.R, unsigned(32))

    class Lba(csr.Register, access="w"):
        value: csr.Field(csr.action.W, unsigned(32))

    class Start(csr.Register, access="w"):
        strobe: csr.Field(csr.action.W, unsigned(1))

    class RxData(csr.Register, access="r"):
        word: csr.Field(csr.action.R, unsigned(32))

    class Resp(csr.Register, access="r"):
        done:  csr.Field(csr.action.R, unsigned(1))
        error: csr.Field(csr.action.R, unsigned(1))

    # Ports — all class-level so no dict/annotation conflict.
    bus:       In(csr.Signature(addr_width=5, data_width=8))
    rx_data:   In(stream.Signature(Packet(unsigned(8))))
    lba_o:     Out(32)
    start_o:   Out(1)
    status_i:  In(_USB_STATUS_LAYOUT)
    resp_i:    In(_USB_RESP_LAYOUT)
    dbg_word_level: Out(8)
    dbg_word_data:  Out(32)

    def __init__(self, *, word_fifo_depth=256):
        self._word_fifo = SyncFIFOBuffered(width=32, depth=word_fifo_depth)
        regs = csr.Builder(addr_width=5, data_width=8)
        self._status      = regs.add("status",      self.Status(),     offset=0x00)
        self._block_size  = regs.add("block_size",  self.BlockSize(),  offset=0x04)
        self._block_count = regs.add("block_count", self.BlockCount(), offset=0x08)
        self._lba         = regs.add("lba",         self.Lba(),        offset=0x0C)
        self._start       = regs.add("start",       self.Start(),      offset=0x10)
        self._rx_data_reg = regs.add("rx_data",     self.RxData(),     offset=0x14)
        self._resp        = regs.add("resp",        self.Resp(),       offset=0x18)
        self._bridge = csr.Bridge(regs.as_memory_map())
        super().__init__()
        self.bus.memory_map = self._bridge.bus.memory_map

    def elaborate(self, platform):
        m = Module()
        m.submodules.bridge = self._bridge
        wiring.connect(m, wiring.flipped(self.bus), self._bridge.bus)
        m.submodules.word_fifo = wf = self._word_fifo

        # Pack incoming bytes (little-endian) into 32-bit words.
        byte_ix = Signal(2)
        acc = Signal(32)
        m.d.comb += [self.rx_data.ready.eq(1), wf.w_en.eq(0)]
        with m.If(self.rx_data.valid & self.rx_data.ready):
            b = self.rx_data.payload.data
            with m.Switch(byte_ix):
                for i in range(4):
                    with m.Case(i):
                        m.d.sync += acc[i*8:(i+1)*8].eq(b)
            m.d.sync += byte_ix.eq(byte_ix + 1)
            with m.If(byte_ix == 3):
                m.d.comb += [wf.w_data.eq(Cat(acc[0:24], b)), wf.w_en.eq(1)]

        # Status / capacity readback.
        m.d.comb += [
            self._status.f.connected.r_data.eq(self.status_i.connected),
            self._status.f.ready.r_data.eq(self.status_i.ready),
            self._status.f.busy.r_data.eq(self.status_i.busy),
            self._status.f.rx_avail.r_data.eq(wf.r_level != 0),
            self._block_size.f.value.r_data.eq(self.status_i.block_size),
            self._block_count.f.value.r_data.eq(self.status_i.block_count),
        ]

        # Command: latch LBA, pulse start.
        with m.If(self._lba.f.value.w_stb):
            m.d.sync += self.lba_o.eq(self._lba.f.value.w_data)
        m.d.comb += self.start_o.eq(
            self._start.f.strobe.w_stb & self._start.f.strobe.w_data)

        # Drain word FIFO on rx_data CSR read.
        m.d.comb += wf.r_en.eq(self._rx_data_reg.f.word.r_stb)
        with m.If(wf.r_level != 0):
            m.d.comb += self._rx_data_reg.f.word.r_data.eq(wf.r_data)
        with m.Else():
            m.d.comb += self._rx_data_reg.f.word.r_data.eq(0)

        # Sticky response latch (set on resp_i.done).
        resp_done_r = Signal()
        resp_error_r = Signal()
        with m.If(self.resp_i.done):
            m.d.sync += [resp_done_r.eq(1),
                         resp_error_r.eq(self.resp_i.error)]
        m.d.comb += [
            self._resp.f.done.r_data.eq(resp_done_r),
            self._resp.f.error.r_data.eq(resp_error_r),
        ]

        m.d.comb += [
            self.dbg_word_level.eq(wf.r_level),
            self.dbg_word_data.eq(wf.r_data),
        ]
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
        self.usb_msc = USBMSCPeripheral()
        self.csr_decoder.add(self.usb_msc.bus, addr=0x1200, name="usb_msc")

        # Dedicated plotter for the scope (base SoC plotter's 3 ports are
        # taken by pixel_plot/blit/line). One port per scope channel.
        self.scope_plotter = FramebufferPlotter(
            bus_signature=self.psram_periph.bus.signature.flip(), n_ports=4)
        self.psram_periph.add_master(self.scope_plotter.bus)

        # 4-channel oscilloscope: V1, V2, V3, mix.
        self.scope_periph = scope.ScopePeripheral(
            n_channels=4, fs=self.clock_settings.audio_clock.fs())
        self.csr_decoder.add(self.scope_periph.bus, addr=0x1300, name="scope_periph")

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
            psram_base_bytes=self.CPU6502_PSRAM_BASE_BYTES,
        )
        self.psram_periph.add_master(bridge.psram_bus)
        m.d.comb += [
            self.sid_periph.ext_w_en  .eq(bridge.sid_w_en),
            self.sid_periph.ext_w_data.eq(bridge.sid_w_data),
        ]

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

        if sim.is_hw(platform):
            from guh.engines.msc import USBMSCHost
            ulpi = platform.request(platform.default_usb_connection)
            m.submodules.usb = usb = USBMSCHost(bus=ulpi)
            m.submodules.usb_msc = self.usb_msc
            wiring.connect(m, usb.rx_data, self.usb_msc.rx_data)
            m.d.comb += [
                self.usb_msc.status_i.connected.eq(usb.status.connected),
                self.usb_msc.status_i.ready.eq(usb.status.ready),
                self.usb_msc.status_i.busy.eq(usb.status.busy),
                self.usb_msc.status_i.block_size.eq(usb.status.block_size),
                self.usb_msc.status_i.block_count.eq(usb.status.block_count),
                usb.cmd.lba.eq(self.usb_msc.lba_o),
                usb.cmd.start.eq(self.usb_msc.start_o),
                self.usb_msc.resp_i.done.eq(usb.resp.done),
                self.usb_msc.resp_i.error.eq(usb.resp.error),
                platform.request("usb_vbus_en").o.eq(1),
            ]

        pmod0 = self.pmod0_periph.pmod
        m.d.comb += [
            pmod0.i_cal.valid.eq(1),
            pmod0.i_cal.payload[0].as_value().eq(sid.voice0_dca),
            pmod0.i_cal.payload[1].as_value().eq(sid.voice1_dca),
            pmod0.i_cal.payload[2].as_value().eq(sid.voice2_dca),
            pmod0.i_cal.payload[3].as_value().eq(self.sid_periph.last_audio_left >> 8),
        ]

        # --- Voice scope ---------------------------------------------------
        m.submodules.scope_plotter = self.scope_plotter
        m.submodules.scope_periph  = self.scope_periph

        # Each scope channel drives one plotter port.
        for n in range(4):
            wiring.connect(m, self.scope_periph.o[n], self.scope_plotter.i[n])

        # Plotter writes into the live framebuffer (fan-out from fb.fbp,
        # same pattern as the base plotter/persist consumers).
        wiring.connect(m, wiring.flipped(self.fb.fbp), self.scope_plotter.fbp)

        # Non-blocking tap of the 4 audio channels already on i_cal.
        # We deliberately ignore plot_fifo.i.ready so the SID audio stream
        # is never stalled by plotting (drops samples if the FIFO is full).
        m.submodules.plot_fifo = plot_fifo = dsp.SyncFIFOBuffered(
            shape=data.ArrayLayout(PSQ, 4), depth=32)
        m.d.comb += [
            plot_fifo.i.valid.eq(pmod0.i_cal.valid & pmod0.i_cal.ready),
            plot_fifo.i.payload[0].eq(pmod0.i_cal.payload[0]),
            plot_fifo.i.payload[1].eq(pmod0.i_cal.payload[1]),
            plot_fifo.i.payload[2].eq(pmod0.i_cal.payload[2]),
            plot_fifo.i.payload[3].eq(pmod0.i_cal.payload[3]),
        ]
        wiring.connect(m, plot_fifo.o, self.scope_periph.i)

        return m


if __name__ == "__main__":
    this_path = os.path.dirname(os.path.realpath(__file__))
    top_level_cli(SIDPlayerSoc, path=this_path,
                  archiver_callback=lambda a: a.with_option_storage())
