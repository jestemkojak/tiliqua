# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""SID player bitstream: software 6502 (mos6502 crate) on VexiiRiscv, SID via CSR."""

import os
import sys

from amaranth import *
from amaranth.lib import data, stream, wiring
from amaranth.lib.fifo import SyncFIFO, SyncFIFOBuffered
from amaranth.lib.wiring import In, Out, connect, flipped
from amaranth import ResetInserter

from luna.gateware.stream.future import Packet

from tiliqua import dsp
from tiliqua.build import sim
from tiliqua.build.cli import top_level_cli
from tiliqua.build.types import BitstreamHelp
from tiliqua.raster import PSQ, scope
from tiliqua.raster.plot import FramebufferPlotter, PlotRequest
from tiliqua.tiliqua_soc import TiliquaSoc
from amaranth_soc import csr


def _load_by_path(relpath, modname):
    """Load a sibling .py by path to avoid the 'top' namespace collision when
    running as a script (src/top/sid_player_sw/ on sys.path shadows the top
    package with this top.py)."""
    import importlib.util
    p = os.path.realpath(os.path.join(
        os.path.dirname(os.path.realpath(__file__)), relpath))
    spec = importlib.util.spec_from_file_location(modname, p)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[modname] = mod
    spec.loader.exec_module(mod)
    return mod


_smooth = _load_by_path("smooth.py", "_smooth")
VoiceSmoother = _smooth.VoiceSmoother
LinearUpsampler = _smooth.LinearUpsampler
StreamThrottle = _smooth.StreamThrottle

# Runtime-selectable SID phi2 rates. "Clean" near-PAL/NTSC targets chosen so
# the AudioDecimator stays small (ratios 32/657 and 16/341; FIR tap ROM =
# 5*m_down): exact 985248/1022727 Hz would need 51k/1.7M-tap FIRs (infeasible).
# Pitch error vs a real C64: +0.44 / +0.46 cents — far below the ~5 cent
# audibility threshold. See
# docs/superpowers/specs/2026-06-12-sid-phi2-runtime-select-design.md.
PHI2_HZ_PAL  = 985_500
PHI2_HZ_NTSC = 1_023_000


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
        error: csr.Field(csr.action.R, unsigned(1))

    # Ports — all class-level so no dict/annotation conflict.
    bus:       In(csr.Signature(addr_width=5, data_width=8))
    rx_data:   In(stream.Signature(Packet(unsigned(8))))
    lba_o:     Out(32)
    start_o:   Out(1)
    status_i:  In(_USB_STATUS_LAYOUT)
    resp_i:    In(_USB_RESP_LAYOUT)

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

        # start_strobe: high for one cycle when firmware writes start.strobe=1.
        start_strobe = self._start.f.strobe.w_stb & self._start.f.strobe.w_data

        # Wrap the word FIFO with ResetInserter so start_strobe flushes it in
        # one cycle (idiomatic Amaranth sync-domain flush).
        m.submodules.word_fifo = wf = ResetInserter(start_strobe)(self._word_fifo)

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
        m.d.comb += self.start_o.eq(start_strobe)

        # Drain word FIFO on rx_data CSR read.
        m.d.comb += wf.r_en.eq(self._rx_data_reg.f.word.r_stb)
        with m.If(wf.r_level != 0):
            m.d.comb += self._rx_data_reg.f.word.r_data.eq(wf.r_data)
        with m.Else():
            m.d.comb += self._rx_data_reg.f.word.r_data.eq(0)

        # Sticky response latch (set on resp_i.done).
        # Reset on start_strobe so a new command never sees a stale error.
        resp_error_r = Signal()
        with m.If(self.resp_i.done):
            m.d.sync += resp_error_r.eq(self.resp_i.error)
        # start_strobe wins: placed AFTER the done block so a simultaneous
        # start+done clears rather than latches (new command takes priority).
        with m.If(start_strobe):
            m.d.sync += [byte_ix.eq(0), acc.eq(0), resp_error_r.eq(0)]
        m.d.comb += self._resp.f.error.r_data.eq(resp_error_r)

        return m


class SIDPlayerSwSoc(TiliquaSoc):
    """SoC that runs PSID tunes: software 6502 (mos6502 crate) on VexiiRiscv,
    SID writes via SIDPeripheral CSR transaction_data register."""

    module_docstring = sys.modules[__name__].__doc__

    bitstream_help = BitstreamHelp(
        brief="PSID music player from USB mass storage.",
        io_left=["cutoff cv", "pulse-width cv", "voice-mute cv", "",
                 "voice0", "voice1", "voice2", "voice mix"],
        io_right=["navigate menu", "USB drive", "video out", "", "", ""],
    )

    # Byte offset within PSRAM where the 6502's 64KB address space begins.
    # System PSRAM base = 0x20000000; 6502 base = 0x20800000 → offset 8MB.
    CPU6502_PSRAM_BASE_BYTES = 0x00800000

    def __init__(self, **kwargs):
        self.sid_model = kwargs.pop("sid_model", "8580")  # build-time SID chip
        _sid = _load_by_path("../sid/top.py", "_sid_top")
        SIDPeripheral = _sid.SIDPeripheral
        # Bigger L1 caches (2KB each): the software 6502 thrashes the default
        # 512B caches against the 64KB PSRAM image -> ~10x too slow -> SID write
        # timing smears -> dropped notes. See docs/sid_player_sw_dropped_notes_*.
        kwargs.setdefault("cpu_variant", "tiliqua_rv32im_bigcache")
        # Freeze the top menu band (y < HEADER_H=200, see fw/src/main.rs) from
        # persist phosphor decay so the menu text doesn't flicker at fast
        # (low) scope-decay settings, and the UI loop can stop re-blitting it
        # every frame (firmware task).
        kwargs.setdefault("persist_freeze_rows", 200)
        super().__init__(finalize_csr_bridge=False, mainram_size=0x4000, **kwargs)
        self.sid_periph = SIDPeripheral(
            sid2_define=(self.sid_model == "8580"),
            sid_hz=30_000_000,
            phi2_hz=(PHI2_HZ_PAL, PHI2_HZ_NTSC))
        self.csr_decoder.add(self.sid_periph.bus, addr=0x1000, name="sid_periph")
        self.usb_msc = USBMSCPeripheral()
        self.csr_decoder.add(self.usb_msc.bus, addr=0x1200, name="usb_msc")

        # Dedicated plotter for the scope (base SoC plotter's 3 ports are
        # taken by pixel_plot/blit/line). One port per scope channel.
        self.scope_plotter = FramebufferPlotter(
            bus_signature=self.psram_periph.bus.signature.flip(), n_ports=4)
        self.psram_periph.add_master(self.scope_plotter.bus)

        # 4-channel oscilloscope: V1, V2, V3, mix. fs is scaled by the display
        # interpolation factor so the firmware's timebase matches the upsampled
        # stream fed to the scope (see LinearUpsampler).
        self.scope_n_upsample = 8
        # Max one scope plot point per this many sync cycles, per channel, so the
        # scope plotter never starves the 6502's PSRAM tune fetches (audio >
        # visuals). scope_throttle*scope_n_upsample < ~1250 (sync cycles per
        # 48kHz frame) keeps all points (spread, not dropped); higher = safer for
        # audio / sparser traces.
        self.scope_throttle = 64
        self.scope_periph = scope.ScopePeripheral(
            n_channels=4,
            fs=self.clock_settings.audio_clock.fs() * self.scope_n_upsample)
        self.csr_decoder.add(self.scope_periph.bus, addr=0x1300, name="scope_periph")

        self.finalize_csr_bridge()

    def elaborate(self, platform):
        _sid = _load_by_path("../sid/top.py", "_sid_top")
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
        AudioDecimator = _load_by_path("../sid/audio.py", "_sid_audio").AudioDecimator
        fs_out = self.clock_settings.audio_clock.fs()
        # One decimator per phi2 standard (the FIR ratio is fixed at
        # elaboration by fs_in); both always run off the same strobe, the
        # phi2_sel CSR muxes which output is heard. The unselected one sees a
        # ~3.8%-off fs_in — harmless, its output is ignored.
        m.submodules.audio_decim_pal = decim_pal = AudioDecimator(
            fs_in=PHI2_HZ_PAL, fs_out=fs_out)
        m.submodules.audio_decim_ntsc = decim_ntsc = AudioDecimator(
            fs_in=PHI2_HZ_NTSC, fs_out=fs_out)
        for decim in (decim_pal, decim_ntsc):
            m.d.comb += [
                decim.i.valid.eq(self.sid_periph.audio_strobe),
                decim.i.payload.as_value().eq(self.sid_periph.last_audio_left >> 8),
            ]
        audio_out = Signal(dsp.ASQ)
        with m.If(self.sid_periph.phi2_sel):
            m.d.comb += audio_out.eq(decim_ntsc.o)
        with m.Else():
            m.d.comb += audio_out.eq(decim_pal.o)

        pmod0 = self.pmod0_periph.pmod
        m.d.comb += [
            pmod0.i_cal.valid.eq(1),
            pmod0.i_cal.payload[0].as_value().eq(self.sid_periph.voice0_dca_o),
            pmod0.i_cal.payload[1].as_value().eq(self.sid_periph.voice1_dca_o),
            pmod0.i_cal.payload[2].as_value().eq(self.sid_periph.voice2_dca_o),
            pmod0.i_cal.payload[3].as_value().eq(audio_out.as_value()),
        ]

        # --- Voice scope ---------------------------------------------------
        m.submodules.scope_plotter = self.scope_plotter
        m.submodules.scope_periph  = self.scope_periph

        # Each scope channel drives one plotter port, via a throttle so the
        # scope plotter can never monopolise the PSRAM bus the 6502 reads its
        # tune from: audio/SID timing must always keep bandwidth headroom
        # (audio > visuals, always). The throttle spreads the upsampler's
        # per-frame burst across many cycles; at scope_throttle*n_up < ~1250
        # sync-cycles-per-frame no plot points are dropped, only spread.
        for n in range(4):
            thr = StreamThrottle(PlotRequest, period=self.scope_throttle)
            setattr(m.submodules, f"scope_throttle{n}", thr)
            wiring.connect(m, self.scope_periph.o[n], thr.i)
            wiring.connect(m, thr.o, self.scope_plotter.i[n])

        # Plotter writes into the live framebuffer (fan-out from fb.fbp,
        # same pattern as the base plotter/persist consumers).
        wiring.connect(m, wiring.flipped(self.fb.fbp), self.scope_plotter.fbp)

        # Smooth the three raw voice taps (~1MHz reSID outputs) before the scope
        # samples them at 48kHz: point-sampling them unfiltered aliases their
        # >24kHz content into broadband scatter, so the voice traces render as
        # dot-clouds. A cheap multi-pole leaky integrator (adders/shifts only)
        # band-limits them enough to draw clean lines. This is on the SCOPE
        # BRANCH ONLY — the audio outputs above keep reading raw voiceN_dca.
        # 3 channels: only the raw voice taps need smoothing — the mix (scope
        # ch3) is already band-limited by AudioDecimator and is fed to the
        # plot_fifo directly from i_cal.
        m.submodules.voice_smooth = voice_smooth = VoiceSmoother(n_channels=3, k=7, poles=4)
        m.d.comb += [
            voice_smooth.strobe.eq(self.sid_periph.audio_strobe),
            voice_smooth.i[0].eq(self.sid_periph.voice0_dca_o),
            voice_smooth.i[1].eq(self.sid_periph.voice1_dca_o),
            voice_smooth.i[2].eq(self.sid_periph.voice2_dca_o),
        ]

        # Non-blocking tap into the scope: smoothed voices + the already
        # band-limited mix (i_cal ch3). We deliberately ignore plot_fifo.i.ready
        # so plotting never stalls the audio stream (drops if the FIFO is full).
        m.submodules.plot_fifo = plot_fifo = dsp.SyncFIFOBuffered(
            shape=data.ArrayLayout(PSQ, 4), depth=32)
        # voiceN_dca are ASQ-scaled (Q1.15) samples but the scope frame type is
        # PSQ (Q1.13). The original fed them via i_cal.payload (ASQ) and let the
        # fixed-point .eq do the value-preserving ASQ->PSQ conversion (a >>2).
        # The smoother works in the raw integer domain, so reproduce that shift
        # here — otherwise the voice traces render 4x too tall.
        _asq_to_psq = dsp.ASQ.f_bits - PSQ.f_bits  # = 2
        m.d.comb += [
            plot_fifo.i.valid.eq(pmod0.i_cal.valid & pmod0.i_cal.ready),
            plot_fifo.i.payload[0].as_value().eq(voice_smooth.o[0] >> _asq_to_psq),
            plot_fifo.i.payload[1].as_value().eq(voice_smooth.o[1] >> _asq_to_psq),
            plot_fifo.i.payload[2].as_value().eq(voice_smooth.o[2] >> _asq_to_psq),
            plot_fifo.i.payload[3].eq(pmod0.i_cal.payload[3]),
        ]
        # Linearly interpolate between the 48kHz frames so steep edges render as
        # connected lines instead of vertical dot-columns (the scope rasterizer
        # plots points, not lines). Cheap: adders/shifts only, no DSP/BRAM.
        m.submodules.scope_upsample = scope_upsample = LinearUpsampler(
            n_channels=4, n_up=self.scope_n_upsample)
        wiring.connect(m, plot_fifo.o, scope_upsample.i)
        wiring.connect(m, scope_upsample.o, self.scope_periph.i)

        return m


if __name__ == "__main__":
    this_path = os.path.dirname(os.path.realpath(__file__))
    top_level_cli(SIDPlayerSwSoc, path=this_path,
                  display_name="SID-PLAYER",  # user-visible name (no "-SW" suffix);
                                              # build dir / archive stay "sid-player-sw"
                  archiver_callback=lambda a: a.with_option_storage(),
                  argparse_callback=lambda p: p.add_argument(
                      "--sid-model", choices=["6581", "8580"], default="8580",
                      help="SID chip model to synthesize (default 8580)."),
                  argparse_fragment=lambda args: {"sid_model": args.sid_model})
