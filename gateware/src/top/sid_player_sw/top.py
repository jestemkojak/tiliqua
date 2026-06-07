# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""SID player bitstream: software 6502 (mos6502 crate) on VexiiRiscv, SID via CSR."""

import os
import sys

from amaranth import *
from amaranth.lib import data, stream, wiring
from amaranth.lib.fifo import SyncFIFO, SyncFIFOBuffered
from amaranth.lib.wiring import In, Out, connect, flipped

from luna.gateware.stream.future import Packet

from tiliqua import dsp
from tiliqua.build import sim
from tiliqua.build.cli import top_level_cli
from tiliqua.build.types import BitstreamHelp
from tiliqua.raster import PSQ, scope
from tiliqua.raster.plot import FramebufferPlotter
from tiliqua.tiliqua_soc import TiliquaSoc
from amaranth_soc import csr


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


class SIDPlayerSwSoc(TiliquaSoc):
    """SoC that runs PSID tunes: software 6502 (mos6502 crate) on VexiiRiscv,
    SID writes via SIDPeripheral CSR transaction_data register."""

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
        when running as a script (Python adds src/top/sid_player_sw/ to sys.path,
        shadowing the top package with the current top.py script)."""
        import importlib.util
        _p = os.path.join(os.path.dirname(os.path.realpath(__file__)), "../sid/top.py")
        spec = importlib.util.spec_from_file_location("_sid_top", os.path.realpath(_p))
        mod = importlib.util.module_from_spec(spec)
        sys.modules["_sid_top"] = mod
        spec.loader.exec_module(mod)
        return mod

    @staticmethod
    def _import_sid_audio():
        """Load src/top/sid/audio.py by path (same 'top' collision avoidance as
        _import_sid_top)."""
        import importlib.util
        _p = os.path.join(os.path.dirname(os.path.realpath(__file__)), "../sid/audio.py")
        spec = importlib.util.spec_from_file_location("_sid_audio", os.path.realpath(_p))
        mod = importlib.util.module_from_spec(spec)
        sys.modules["_sid_audio"] = mod
        spec.loader.exec_module(mod)
        return mod

    def __init__(self, **kwargs):
        self.sid_model = kwargs.pop("sid_model", "8580")  # build-time SID chip
        _sid = self._import_sid_top()
        SIDPeripheral = _sid.SIDPeripheral
        super().__init__(finalize_csr_bridge=False, mainram_size=0x4000, **kwargs)
        self.sid_periph = SIDPeripheral()
        self.csr_decoder.add(self.sid_periph.bus, addr=0x1000, name="sid_periph")
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

        m.submodules.sid = sid = SID(sid2_define=(self.sid_model == "8580"))
        m.submodules.sid_periph = self.sid_periph

        # SID writes come exclusively from the RISC-V via the CSR transaction_data
        # register; the external write port is unused.
        m.d.comb += [
            self.sid_periph.ext_w_en  .eq(0),
            self.sid_periph.ext_w_data.eq(0),
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

        # Anti-alias the ~1MHz SID mix down to the codec rate. Point-sampling the
        # 1MHz stream at 48kHz (as a bare assignment would) folds all SID content
        # above 24kHz into the audible band as broadband grit; the polyphase FIR
        # band-limits it first. See top/sid/audio.py.
        AudioDecimator = self._import_sid_audio().AudioDecimator
        m.submodules.audio_decim = audio_decim = AudioDecimator(
            fs_in=1_000_000, fs_out=self.clock_settings.audio_clock.fs())
        m.d.comb += [
            audio_decim.i.valid.eq(self.sid_periph.audio_strobe),
            audio_decim.i.payload.as_value().eq(self.sid_periph.last_audio_left >> 8),
        ]

        pmod0 = self.pmod0_periph.pmod
        m.d.comb += [
            pmod0.i_cal.valid.eq(1),
            pmod0.i_cal.payload[0].as_value().eq(sid.voice0_dca),
            pmod0.i_cal.payload[1].as_value().eq(sid.voice1_dca),
            pmod0.i_cal.payload[2].as_value().eq(sid.voice2_dca),
            pmod0.i_cal.payload[3].as_value().eq(audio_decim.o.as_value()),
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
    top_level_cli(SIDPlayerSwSoc, path=this_path,
                  archiver_callback=lambda a: a.with_option_storage(),
                  argparse_callback=lambda p: p.add_argument(
                      "--sid-model", choices=["6581", "8580"], default="8580",
                      help="SID chip model to synthesize (default 8580)."),
                  argparse_fragment=lambda args: {"sid_model": args.sid_model})
