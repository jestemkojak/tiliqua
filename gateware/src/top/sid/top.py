# Copyright (c) 2024 Seb Holzapfel
#
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""
This example instantiates a SID chip, which can be modulated via CV or MIDI.

    .. code-block:: text

        ┌────┐
        │in0 │◄─ modulation source 0
        │in1 │◄─ modulation source 1
        │in2 │◄─ modulation source 2
        │in3 │◄─ modulation source 3
        └────┘
        ┌────┐
        │out0│─► voice 0 (solo)
        │out1│─► voice 1 (solo)
        │out2│─► voice 2 (solo)
        │out3│─► voices 0-2 (sum)
        └────┘

Using the menu system, each input channel can be assigned to a modulation target
(i.e pitch / gate of specific voices or multiple voices).

The soft CPU then uses this mapping to redirect CV to perform specific register
writes on the SID chip. To add new modulation types or for more complex
modulation, only the rust firmware needs to be changed.

The audio routing out the SID chip to the audio outputs however is pure
gateware. The softcore is only used for register writes.

    .. code-block:: text

                        ┌──────────┐  ┌───┐
        (CV / MIDI) ───►│VexiiRiscv│  │SID│ ─────► (audio out)
                        └────┬─────┘  └───┘
                             │          ▲
                             └──────────┘
                           (register writes)

Control source (CV or MIDI) is selected on the Misc page. In MIDI mode,
notes are distributed across the 3 voices with round-robin voice stealing.

The Polyphony page controls voice allocation. In poly mode each voice is
independent. In unison mode all 3 voices play the same pitch with a
configurable detune spread (in cents) for a fatter sound.

MIDI controllers supported:
- Pitch bend: +/-2 semitone range, applied to all active voices.
- Mod wheel (CC1): offsets the filter cutoff upward from its menu-set value.
"""

import os
import sys
from fractions import Fraction

from amaranth import *
from amaranth.lib import data, stream, wiring
from amaranth.lib.fifo import SyncFIFO, SyncFIFOBuffered, AsyncFIFO
from amaranth.lib.cdc import FFSynchronizer, PulseSynchronizer
from amaranth.lib.wiring import In, Out, connect, flipped
from amaranth_soc import csr

from tiliqua import midi
from guh.engines.midi import USBMIDIHost
from tiliqua.build import sim
from tiliqua.build.cli import top_level_cli
from tiliqua.build.types import BitstreamHelp
from tiliqua.raster import scope
from tiliqua.raster.plot import FramebufferPlotter
from tiliqua.tiliqua_soc import TiliquaSoc


class SID(wiring.Component):

    clk:     In(1)
    bus_i:   In(data.StructLayout({
        "res":   unsigned(1),
        "r_w_n": unsigned(1),
        "phi2":  unsigned(1),
        "data":  unsigned(8),
        "addr":  unsigned(5),
        }))
    cs:      In(4)
    data_o:  Out(8)
    audio_o: Out(data.StructLayout({
        "right": signed(24),
        "left":  signed(24),
        }))

    # internal signals for each voice after VCA, but before filter, interesting to see
    voice0_dca: Out(signed(16))
    voice1_dca: Out(signed(16))
    voice2_dca: Out(signed(16))

    def __init__(self, sid2_define=True):
        self._sid2_define = sid2_define
        super().__init__()

    def add_verilog_sources(self, platform):
        vroot = os.path.join(os.path.dirname(os.path.realpath(__file__)),
                                             "../../../deps/sid/gateware")

        # Use MOS8580 sim, it has no DC offset.
        platform.add_file("sid_defines.sv", "`define SID2" if self._sid2_define else "")

        # Include all files necessary for top-level 'sid_api.sv' to be instantiated.

        for file in ["sid_pkg.sv",
                     "sid_api.sv",
                     "sid_filter.sv",
                     "sid_voice.sv",
                     "sid_dac.sv",
                     "sid_pot.sv",
                     "sid_envelope.sv",
                     "sid_waveform.sv",
                     "sid_waveform_PST.svh",
                     "sid_waveform__ST.svh",
                     "sid_waveform_PS__6581.hex",
                     "sid_waveform_PS__8580.hex",
                     "sid_waveform_P_T_6581.hex",
                     "sid_waveform_P_T_8580.hex",
                     "dac_6581_envelope.hex",
                     "dac_6581_cutoff.hex",
                     "sid_control.sv",
                     "dac_6581_waveform.hex"]:
            platform.add_file(file, open(os.path.join(vroot, file)).read())

        # Exclude ICE40 muladd.sv, replace with a generic one that works on ECP5 --

        platform.add_file("muladd_ecp5.sv", """
            module muladd (
                input  logic signed [31:0] c,
                input  logic               s,
                input  logic signed [15:0] a,
                input  logic signed [15:0] b,
                output logic signed [31:0] o
            );

            always_comb begin
                if (s == 0)
                    o = c + (a*b);
                else
                    o = c - (a*b);
            end

            endmodule
        """)

    def elaborate(self, platform) -> Module:

        m = Module()

        self.add_verilog_sources(platform)

        # rough usage
        # - i_clk must be >20x phi2 clk (on bus_i); sid=30MHz => 30 cycles/phi2
        # - falling edge of phi2 starts internal pipeline, takes ~20 cycles
        # procedure:
        # - bring phi2 low for ~12 cycles
        # - bring phi2 high for ~12 cycles
        # - before next phi2 low:
        #   - save latest audio sample
        #   - maybe write to sid register using bus_i (keep data there until next clock)

        m.submodules.vsid = Instance("sid_api",
            i_clk     = ClockSignal("sid"),
            i_bus_i   = self.bus_i,
            i_cs      = self.cs,
            o_data_o  = self.data_o,
            o_audio_o = self.audio_o,
            o_voice0_dca_o = self.voice0_dca,
            o_voice1_dca_o = self.voice1_dca,
            o_voice2_dca_o = self.voice2_dca,
        )

        return m

class Phi2Divider(wiring.Component):

    """
    Fractional-N divider generating the SID phi2 square wave + edge strobe.

    The period alternates between ``P`` and ``P+1`` sync cycles, steered by a
    small accumulator (classic fractional-N), so the *average* rate is exactly
    ``sync_hz / (P + num/den)`` for the selected standard. phi2 edges land on
    the sync-clock grid -> +/-1 sync cycle (16.7ns) deterministic jitter;
    sidebands are <-80dB in the audio band and further averaged by the
    decimation FIR downstream — inaudible.

    All divider state updates only at phi2 cadence (~1MHz), so nothing here
    joins the 60MHz critical path (the reSID filter muladd).

    With the default ``phi2_hz=(1_000_000, 1_000_000)`` the fraction is 0 and
    the divider degenerates to the old flat /60 counter, bit-identical for
    both ``sel`` values — non-opted-in SID targets are unchanged.
    """

    def __init__(self, sync_hz=60_000_000, phi2_hz=(1_000_000, 1_000_000)):
        self._consts = []
        for hz in phi2_hz:
            frac = Fraction(sync_hz, hz)
            p = frac.numerator // frac.denominator
            self._consts.append((p, frac.numerator - p * frac.denominator,
                                 frac.denominator))
        super().__init__({
            "sel":       In(1),   # 0 = phi2_hz[0], 1 = phi2_hz[1]
            "phi2":      Out(1),  # ~50% duty square wave
            "phi2_edge": Out(1),  # 1-sync-cycle pulse at each period wrap
        })

    def elaborate(self, platform):
        m = Module()

        (p0, n0, d0), (p1, n1, d1) = self._consts
        max_p = max(p0, p1) + 1  # +1: the long (P+1) period

        # Selected constants (sel is quasi-static: a CSR the firmware writes
        # on tune load; the muxes feed only phi2-cadence registers).
        base = Signal(range(max_p + 1))
        num  = Signal(range(max(n0, n1) + 1))
        den  = Signal(range(max(d0, d1) + 1))
        m.d.comb += [
            base.eq(Mux(self.sel, p1, p0)),
            num .eq(Mux(self.sel, n1, n0)),
            den .eq(Mux(self.sel, d1, d0)),
        ]

        counter = Signal(range(max_p + 1))
        period  = Signal(range(max_p + 1), init=p0)
        # Invariant acc < den in steady state; a sel switch can leave
        # acc >= new den, which the subtract branch drains within a few
        # periods (period reads base+1 meanwhile — harmless transient).
        acc = Signal(range(2 * max(d0, d1)))

        # `>=` (not `!=`) so a period shrink across a sel switch can never
        # strand the counter past the wrap point.
        with m.If(counter >= period - 1):
            m.d.sync += counter.eq(0)
            # Fractional accumulator chooses the next period length.
            with m.If(acc + num >= den):
                m.d.sync += [acc.eq(acc + num - den), period.eq(base + 1)]
            with m.Else():
                m.d.sync += [acc.eq(acc + num), period.eq(base)]
        with m.Else():
            m.d.sync += counter.eq(counter + 1)

        m.d.comb += [
            self.phi2.eq(counter > (period >> 1)),
            self.phi2_edge.eq(counter == period - 1),
        ]
        return m


class SIDPeripheral(wiring.Component):
    class TransactionData(csr.Register, access="w"):
        transaction_data: csr.Field(csr.action.W, unsigned(16))

    class MidiWrite(csr.Register, access="w"):
        """Write a MIDI message (reserved for future firmware-generated injection)."""
        msg: csr.Field(csr.action.W, unsigned(24))

    class MidiRead(csr.Register, access="r"):
        """Read next MIDI message from hardware FIFO; returns 0 when empty."""
        msg: csr.Field(csr.action.R, unsigned(24))

    class UsbMidiHost(csr.Register, access="w"):
        """0 = use TRS MIDI (default). 1 = enable USB host + VBUS."""
        host: csr.Field(csr.action.W, unsigned(1))

    class UsbMidiCfg(csr.Register, access="w"):
        """USB MIDI streaming endpoint config word (hardcoded to 1)."""
        value: csr.Field(csr.action.W, unsigned(4))

    class BuildModel(csr.Register, access="r"):
        model: csr.Field(csr.action.R, unsigned(1))

    class TxnStatus(csr.Register, access="r"):
        """Transaction-FIFO status for firmware backpressure."""
        writable: csr.Field(csr.action.R, unsigned(1))
        level:    csr.Field(csr.action.R, unsigned(5))   # occupancy; fits depth<=31

    class Phi2Sel(csr.Register, access="rw"):
        """SID phi2 standard select: 0 = phi2_hz[0] (PAL), 1 = phi2_hz[1] (NTSC).
        Reset 0. Firmware writes this on every tune load (AUTO follows the
        PSID header) — see sid_player_sw fw/src/main.rs."""
        sel: csr.Field(csr.action.RW, unsigned(1))

    class SysexRead(csr.Register, access="r"):
        """Raw SysEx sideband byte. Read pops one byte from the 64-deep FIFO.
        ``valid``=0 means empty (``data`` is then meaningless). A valid bit is
        required because 0x00 is a legal SysEx data byte — the midi_read
        'read until 0' idiom cannot work here."""
        data:  csr.Field(csr.action.R, unsigned(8))
        valid: csr.Field(csr.action.R, unsigned(1))

    def __init__(self, *, transaction_depth=16, sid2_define=True,
                 sid_hz=30_000_000, phi2_hz=(1_000_000, 1_000_000),
                 with_sysex=False):
        self._sid2_define = sid2_define
        self._sid_hz   = sid_hz
        self._phi2_hz  = phi2_hz
        self._with_sysex = with_sysex
        self._transactions = AsyncFIFO(width=16, depth=transaction_depth,
                                       w_domain="sync", r_domain="sid")
        self._midi_read_fifo = SyncFIFOBuffered(width=24, depth=8)
        if with_sysex:
            self._sysex_fifo = SyncFIFOBuffered(width=8, depth=64)

        regs = csr.Builder(addr_width=6, data_width=8)
        self._transaction_data = regs.add("transaction_data", self.TransactionData(), offset=0x0)
        self._midi_write  = regs.add("midi_write",    self.MidiWrite(),   offset=0x4)
        self._midi_read   = regs.add("midi_read",     self.MidiRead(),    offset=0x8)
        self._midi_host   = regs.add("usb_midi_host", self.UsbMidiHost(), offset=0xC)
        self._midi_cfg    = regs.add("usb_midi_cfg",  self.UsbMidiCfg(),  offset=0x10)
        self._midi_endp   = regs.add("usb_midi_endp", self.UsbMidiCfg(),  offset=0x14)
        self._build_model = regs.add("build_model",   self.BuildModel(),  offset=0x18)
        self._txn_status  = regs.add("txn_status",    self.TxnStatus(),   offset=0x1C)
        self._phi2_sel    = regs.add("phi2_sel",      self.Phi2Sel(),     offset=0x20)
        if with_sysex:
            self._sysex_read = regs.add("sysex_read", self.SysexRead(), offset=0x24)
        self._bridge = csr.Bridge(regs.as_memory_map())

        self.sid = None

        # audio
        self.last_audio_left  = Signal(signed(24))
        self.last_audio_right = Signal(signed(24))
        # Pulses for one sync cycle each time last_audio_* is refreshed with a new
        # SID output sample (~1MHz). Consumers anti-alias/decimate from this rate.
        self.audio_strobe     = Signal()

        sig = {
            "bus":           In(csr.Signature(addr_width=regs.addr_width, data_width=regs.data_width)),
            "i_midi":        In(stream.Signature(midi.MidiMessage)),
            # External SID write path (e.g. from 6502 bridge in sid_player)
            "ext_w_en":      In(1),
            "ext_w_data":    In(16),
            # MIDI output ports (driven from CSR writes, read by SIDSoc)
            "usb_midi_host":   Out(1),
            "usb_midi_cfg_id": Out(4),
            # phi2 standard currently selected (mirrors the phi2_sel CSR) —
            # read by the SoC top to mux the per-standard audio decimators.
            "phi2_sel":        Out(1),
            # Voice taps captured into `sync` (gated by the synchronized audio
            # strobe), so the tops never sample a raw sid-domain bus.
            "voice0_dca_o": Out(signed(16)),
            "voice1_dca_o": Out(signed(16)),
            "voice2_dca_o": Out(signed(16)),
        }
        if with_sysex:
            # Raw SysEx byte sideband (backpressure honored -> FIFO never drops)
            sig["i_sysex"] = In(stream.Signature(unsigned(8)))
        super().__init__(sig)
        self.bus.memory_map = self._bridge.bus.memory_map


    def elaborate(self, platform):
        m = Module()

        m.submodules.bridge  = self._bridge
        connect(m, flipped(self.bus), self._bridge.bus)
        m.submodules.transactions = self._transactions

        # CSRs -> Transactions FIFO (ORed with optional external write path)
        m.d.comb += [
            self._transactions.w_en  .eq(self._transaction_data.f.transaction_data.w_stb | self.ext_w_en),
            self._transactions.w_data.eq(Mux(self.ext_w_en, self.ext_w_data, self._transaction_data.f.transaction_data.w_data)),
        ]

        # Fractional-N phi2 divider, runtime-selectable standard (phi2_sel
        # CSR). Runs in the `sid` domain (input clock = sid_hz). Defaults make
        # this a flat /30 (~1MHz) at the 30MHz sid clock.
        m.submodules.phi2_div = phi2_div = DomainRenamer("sid")(
            Phi2Divider(sync_hz=self._sid_hz, phi2_hz=self._phi2_hz))
        sel_sid = Signal()
        m.submodules += FFSynchronizer(self._phi2_sel.f.sel.data, sel_sid,
                                       o_domain="sid")
        m.d.comb += [
            phi2_div.sel.eq(sel_sid),
            self.phi2_sel.eq(self._phi2_sel.f.sel.data),  # sync mirror for decim mux
        ]
        phi2      = phi2_div.phi2
        phi2_edge = phi2_div.phi2_edge

        # SID bus + reset run in the sid domain.
        m.d.sid += [
            self.sid.bus_i.phi2  .eq(phi2),
            self.sid.cs          .eq(0b0100), # cs_n = 0, cs_io1_n = 1
        ]

        startup = Signal(8)

        # route FIFO'd transactions -> SID (in the sid domain)
        m.d.sid += self._transactions.r_en.eq(0)
        # sid-domain holding registers (updated once per phi2 edge).
        hold_l = Signal(signed(24)); hold_r = Signal(signed(24))
        hold_v0 = Signal(signed(16)); hold_v1 = Signal(signed(16)); hold_v2 = Signal(signed(16))
        with m.If(phi2_edge):

            # TODO verify
            with m.If(startup < 24):
                m.d.sid += startup.eq(startup+1)
                m.d.sid += self.sid.bus_i.res.eq(1)
            with m.Else():
                m.d.sid += self.sid.bus_i.res.eq(0)

            m.d.sid += [
                # Consume 1 transaction iff the head existed AT this edge. r_en
                # must not be asserted unconditionally: a word strobed into an
                # empty FIFO on this same cycle becomes readable next cycle,
                # where a blanket r_en pops it while the SID bus was already
                # latched as a read (level was 0 here) — silently swallowing
                # the write (~1/60 of writes; dropped SID note-ons).
                self._transactions.r_en.eq(self._transactions.r_rdy),
                self.sid.bus_i.r_w_n .eq(~self._transactions.r_rdy),
                self.sid.bus_i.addr  .eq(self._transactions.r_data),
                self.sid.bus_i.data  .eq(self._transactions.r_data >> 5),
                # audio + voice taps -> sid-domain holding registers
                hold_l.eq(self.sid.audio_o.left),
                hold_r.eq(self.sid.audio_o.right),
                hold_v0.eq(self.sid.voice0_dca),
                hold_v1.eq(self.sid.voice1_dca),
                hold_v2.eq(self.sid.voice2_dca),
            ]

        # Strobe sid->sync; capture holds into sync regs when it fires (holds are
        # stable for ~tens of sync cycles between phi2 edges, so this multi-bit
        # crossing is coherent).
        m.submodules.strobe_ps = strobe_ps = PulseSynchronizer(i_domain="sid", o_domain="sync")
        m.d.comb += strobe_ps.i.eq(phi2_edge)
        m.d.sync += self.audio_strobe.eq(strobe_ps.o)
        with m.If(strobe_ps.o):
            m.d.sync += [
                self.last_audio_left .eq(hold_l),
                self.last_audio_right.eq(hold_r),
                self.voice0_dca_o.eq(hold_v0),
                self.voice1_dca_o.eq(hold_v1),
                self.voice2_dca_o.eq(hold_v2),
            ]

        m.submodules.midi_read_fifo = midi_read_fifo = self._midi_read_fifo

        # Accept all incoming MIDI messages; silently drop if FIFO full
        m.d.comb += [
            self.i_midi.ready.eq(1),
            midi_read_fifo.w_data.eq(self.i_midi.payload),
            midi_read_fifo.w_en.eq(self.i_midi.valid),
        ]

        # FIFO -> midi_read CSR (firmware drains by reading until 0)
        m.d.comb += midi_read_fifo.r_en.eq(self._midi_read.element.r_stb)
        with m.If(midi_read_fifo.r_level != 0):
            m.d.comb += self._midi_read.f.msg.r_data.eq(midi_read_fifo.r_data)
        with m.Else():
            m.d.comb += self._midi_read.f.msg.r_data.eq(0)

        if self._with_sysex:
            m.submodules.sysex_fifo = sysex_fifo = self._sysex_fifo
            m.d.comb += [
                sysex_fifo.w_data.eq(self.i_sysex.payload),
                sysex_fifo.w_en.eq(self.i_sysex.valid),
                self.i_sysex.ready.eq(sysex_fifo.w_rdy),   # honored backpressure
                sysex_fifo.r_en.eq(self._sysex_read.element.r_stb),
                self._sysex_read.f.data.r_data.eq(sysex_fifo.r_data),
                self._sysex_read.f.valid.r_data.eq(sysex_fifo.r_rdy),
            ]

        # USB host + cfg CSR writes -> output ports.
        with m.If(self._midi_host.f.host.w_stb):
            m.d.sync += self.usb_midi_host.eq(self._midi_host.f.host.w_data)
        with m.If(self._midi_cfg.f.value.w_stb):
            m.d.sync += self.usb_midi_cfg_id.eq(self._midi_cfg.f.value.w_data)

        # Build-time SID model: drive as a constant read-only CSR.
        m.d.comb += self._build_model.f.model.r_data.eq(int(self._sid2_define))

        # Transaction FIFO status: firmware polls this for backpressure.
        m.d.comb += [
            self._txn_status.f.writable.r_data.eq(self._transactions.w_rdy),
            self._txn_status.f.level.r_data.eq(self._transactions.w_level),
        ]

        return m

class SIDSoc(TiliquaSoc):

    # Used by `tiliqua_soc.py` to create a MODULE_DOCSTRING rust constant used by the 'help' page.
    module_docstring = sys.modules[__name__].__doc__

    # Stored in manifest and used by bootloader for brief summary of each bitstream.
    bitstream_help = BitstreamHelp(
        brief="MOS 6581 (SID) emulation with MIDI input.",
        io_left=['modulate0', 'modulate1', 'modulate2', 'modulate3', 'voice0', 'voice1', 'voice2', 'voice mix'],
        io_right=['navigate menu', 'MIDI host', 'video out', '', '', 'TRS MIDI in']
    )

    def __init__(self, *, with_scope=True, n_sids=1, with_sysex=False, **kwargs):
        # Don't finalize CSR bridge yet. Default mainram (BRAM) is 0x4000 — the big
        # opts struct eats stack. Subclasses (e.g. top/mbsid, whose by-value C++
        # engine state needs more .bss) may override via the mainram_size kwarg.
        kwargs.setdefault("mainram_size", 0x4000)
        super().__init__(finalize_csr_bridge=False,
                         **kwargs)

        self.with_scope = with_scope
        self.n_sids = n_sids
        self.with_sysex = with_sysex

        # Add SID peripheral
        self.sid_periph = SIDPeripheral(with_sysex=with_sysex)
        self.csr_decoder.add(self.sid_periph.bus, addr=0x1000, name="sid_periph")

        if self.n_sids > 1:
            # Second SID for the R register image (stereo). Own AsyncFIFO +
            # 30MHz phi2 divider; MIDI-in CSRs unused on this instance.
            self.sid_periph_r = SIDPeripheral()
            self.csr_decoder.add(self.sid_periph_r.bus, addr=0x1200, name="sid_periph_r")

        if self.with_scope:
            # Dedicated framebuffer plotter for scope (4 channels)
            self.plotter = FramebufferPlotter(
                bus_signature=self.psram_periph.bus.signature.flip(), n_ports=4)
            self.psram_periph.add_master(self.plotter.bus)

            # Add scope peripheral
            self.scope_periph = scope.ScopePeripheral(
                fs=self.clock_settings.audio_clock.fs())
            self.csr_decoder.add(self.scope_periph.bus, addr=0x1100, name="scope_periph")

            # Note: Arbiter is now built into the FramebufferPlotter

        # Now finalize the CSR bridge
        self.finalize_csr_bridge()

    def elaborate(self, platform):

        m = Module()

        # Scope plotting infrastructure
        if self.with_scope:
            m.submodules.plotter = self.plotter

        # Main components
        m.submodules.sid = sid = SID()
        m.submodules.sid_periph = self.sid_periph

        if self.with_scope:
            m.submodules.scope_periph = self.scope_periph

            # Connect scope periph channels to plotter ports
            for n in range(4):
                wiring.connect(m, self.scope_periph.o[n], self.plotter.i[n])

            # Connect framebuffer propreties to plotter backend
            wiring.connect(m, wiring.flipped(self.fb.fbp), self.plotter.fbp)

        m.submodules += super().elaborate(platform)

        pmod0 = self.pmod0_periph.pmod

        self.sid_periph.sid = sid

        # ext_w_* unused in SIDSoc (RISC-V uses CSR path); drive to 0.
        m.d.comb += [self.sid_periph.ext_w_en.eq(0), self.sid_periph.ext_w_data.eq(0)]

        if self.n_sids > 1:
            m.submodules.sid_r = sid_r = SID()
            m.submodules.sid_periph_r = self.sid_periph_r
            self.sid_periph_r.sid = sid_r
            m.d.comb += [self.sid_periph_r.ext_w_en.eq(0),
                         self.sid_periph_r.ext_w_data.eq(0)]
            # Stereo: SID0 mix -> LEFT, SID1 mix -> RIGHT. payload[2]/[3] carry
            # a voice tap per SID for optional debug (no scope consumer in mbsid).
            m.d.comb += [
                pmod0.i_cal.valid.eq(1),
                pmod0.i_cal.payload[0].as_value().eq(self.sid_periph.last_audio_left>>8),
                pmod0.i_cal.payload[1].as_value().eq(self.sid_periph_r.last_audio_left>>8),
                pmod0.i_cal.payload[2].as_value().eq(
                    (self.sid_periph.last_audio_left + self.sid_periph_r.last_audio_left) >> 9),
                pmod0.i_cal.payload[3].as_value().eq(
                    (self.sid_periph.last_audio_left + self.sid_periph_r.last_audio_left) >> 9),
            ]
        else:
            m.d.comb += [
                pmod0.i_cal.valid.eq(1),
                pmod0.i_cal.payload[0].as_value().eq(self.sid_periph.voice0_dca_o),
                pmod0.i_cal.payload[1].as_value().eq(self.sid_periph.voice1_dca_o),
                pmod0.i_cal.payload[2].as_value().eq(self.sid_periph.voice2_dca_o),
                pmod0.i_cal.payload[3].as_value().eq(self.sid_periph.last_audio_left>>8),
            ]

        if self.with_scope:
            # Note: when n_sids > 1, payload[3] fed to scope.i.payload[0] is a
            # voice tap (SID_R voice0_dca), not the audio mix — scope ch0 shows
            # a voice waveform rather than the mix output in stereo builds.
            m.d.comb += [
                # TODO: we're actually overriding soc_en indirectly here because of the early elaboration.
                # this signal should be split properly to avoid it.
                self.scope_periph.i.valid.eq(pmod0.i_cal.valid & pmod0.i_cal.ready & self.scope_periph.soc_en),
                self.scope_periph.i.payload[0].eq(pmod0.i_cal.payload[3]),
                self.scope_periph.i.payload[1].eq(pmod0.i_cal.payload[0]),
                self.scope_periph.i.payload[2].eq(pmod0.i_cal.payload[1]),
                self.scope_periph.i.payload[3].eq(pmod0.i_cal.payload[2]),
            ]

        if sim.is_hw(platform):
            # TRS MIDI (serial)
            midi_pins = platform.request("midi")
            m.submodules.serialrx = serialrx = midi.SerialRx(
                system_clk_hz=60e6, pins=midi_pins)
            m.submodules.midi_decode_trs = midi_decode_trs = midi.MidiDecodeSerial(
                forward_rt=True, forward_sysex=self.with_sysex)
            wiring.connect(m, serialrx.o, midi_decode_trs.i)

            # USB MIDI host
            ulpi = platform.request(platform.default_usb_connection)
            m.submodules.usb = usb = USBMIDIHost(bus=ulpi)
            m.submodules.midi_decode_usb = midi_decode_usb = midi.MidiDecodeUSB(
                forward_rt=True, forward_sysex=self.with_sysex)
            wiring.connect(m, usb.o_midi, midi_decode_usb.i)

            # Source mux: USB host or TRS, controlled by CSR bit
            vbus_o = platform.request("usb_vbus_en").o
            with m.If(self.sid_periph.usb_midi_host):
                wiring.connect(m, midi_decode_usb.o, self.sid_periph.i_midi)
                m.d.comb += vbus_o.eq(1)
                if self.with_sysex:
                    wiring.connect(m, midi_decode_usb.o_sysex, self.sid_periph.i_sysex)
                    # unselected side drained so a source switch mid-dump
                    # can't wedge the other decoder
                    m.d.comb += midi_decode_trs.o_sysex.ready.eq(1)
            with m.Else():
                wiring.connect(m, midi_decode_trs.o, self.sid_periph.i_midi)
                m.d.comb += vbus_o.eq(0)
                if self.with_sysex:
                    wiring.connect(m, midi_decode_trs.o_sysex, self.sid_periph.i_sysex)
                    m.d.comb += midi_decode_usb.o_sysex.ready.eq(1)

            # Drain RT streams (clock sync not used by SID)
            m.d.comb += [
                midi_decode_trs.o_rt.ready.eq(1),
                midi_decode_usb.o_rt.ready.eq(1),
            ]

        return m


if __name__ == "__main__":
    this_path = os.path.dirname(os.path.realpath(__file__))
    top_level_cli(SIDSoc, path=this_path, archiver_callback=lambda archiver: archiver.with_option_storage())
