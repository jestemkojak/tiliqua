# Copyright (c) 2024 S. Holzapfel <me@sebholzapfel.com>
#
# SPDX-License-Identifier: CERN-OHL-S-2.0
#

"""USB MIDI decoding."""

from amaranth import *
from amaranth.lib import data, enum, stream, wiring
from amaranth.lib.wiring import In, Out

from luna.gateware.stream.future import Packet

from ..dsp.stream_util import SyncFIFOBuffered as StreamFIFO
from .types import *

class USBMidiCIN(enum.Enum, shape=unsigned(4)):
    MISC              = 0x0
    CABLE_EVENT       = 0x1
    SYSTEM_COMMON_2   = 0x2
    SYSTEM_COMMON_3   = 0x3
    SYSEX_START       = 0x4
    SYSEX_END_1       = 0x5
    SYSEX_END_2       = 0x6
    SYSEX_END_3       = 0x7
    NOTE_OFF          = 0x8
    NOTE_ON           = 0x9
    POLY_PRESSURE     = 0xA
    CONTROL_CHANGE    = 0xB
    PROGRAM_CHANGE    = 0xC
    CHANNEL_PRESSURE  = 0xD
    PITCH_BEND        = 0xE
    SINGLE_BYTE       = 0xF

class USBHeader(data.Struct):
    """Byte 0 of a 4-byte USB-MIDI event."""
    cin:   USBMidiCIN
    cable: unsigned(4)

class MidiDecodeUSB(wiring.Component):

    """
    Parse 4-byte USB MIDI packets into structured :py:`MidiMessage`.

    Real-time messages can optionally be forwarded on :py:`o_rt`.
    Sysex packets are drained by default, forwarded on ``o_sysex``
    (backpressure honored) with ``forward_sysex=True``.

    When :py:`cable_filter` is set to an integer (0-15), only events from
    that cable number are processed.

    Warn: backpressure on ``o_rt`` is ignored!
    """

    def __init__(self, forward_rt=False, cable_filter=None, forward_sysex=False):
        self.forward_rt = forward_rt
        self.cable_filter = cable_filter
        self.forward_sysex = forward_sysex
        sig = {
            "i": In(stream.Signature(Packet(unsigned(8)))),
            "o": Out(stream.Signature(MidiMessage)),
        }
        if forward_rt:
            sig["o_rt"] = Out(stream.Signature(Status.RT))
        if forward_sysex:
            sig["o_sysex"] = Out(stream.Signature(unsigned(8)))
        super().__init__(sig)

    def elaborate(self, platform):
        m = Module()

        i_payload = self.i.payload.data

        # header == byte 0 of each 4-byte USB-MIDI event
        header = Signal(USBHeader)

        if self.forward_rt:
            # small fifo, rt stream does not support backpressure
            m.submodules.rt_fifo = rt_fifo = StreamFIFO(shape=Status.RT, depth=4)
            wiring.connect(m, rt_fifo.o, wiring.flipped(self.o_rt))

        if self.forward_sysex:
            # remaining valid sysex bytes in the current packet (1..3)
            syx_cnt = Signal(2)

        with m.FSM() as fsm:

            with m.State('IDLE'):
                m.d.comb += self.i.ready.eq(1)
                i_header = USBHeader(i_payload)
                with m.If(self.i.valid & self.i.payload.first):
                    m.d.sync += header.eq(i_header)
                    if self.forward_sysex:
                        is_syx = Signal()
                        m.d.comb += is_syx.eq(
                            (i_header.cin == USBMidiCIN.SYSEX_START) |
                            (i_header.cin == USBMidiCIN.SYSEX_END_1) |
                            (i_header.cin == USBMidiCIN.SYSEX_END_2) |
                            (i_header.cin == USBMidiCIN.SYSEX_END_3))
                        with m.Switch(i_header.cin):
                            with m.Case(USBMidiCIN.SYSEX_END_1):
                                m.d.sync += syx_cnt.eq(1)
                            with m.Case(USBMidiCIN.SYSEX_END_2):
                                m.d.sync += syx_cnt.eq(2)
                            with m.Default():
                                m.d.sync += syx_cnt.eq(3)  # START / END_3
                    if self.cable_filter is not None:
                        with m.If(i_header.cable != self.cable_filter):
                            m.next = 'DRAIN'
                        with m.Else():
                            if self.forward_sysex:
                                with m.If(is_syx):
                                    m.next = 'SYSEX-FWD'
                                with m.Else():
                                    m.next = 'STATUS'
                            else:
                                m.next = 'STATUS'
                    else:
                        if self.forward_sysex:
                            with m.If(is_syx):
                                m.next = 'SYSEX-FWD'
                            with m.Else():
                                m.next = 'STATUS'
                        else:
                            m.next = 'STATUS'

            with m.State('STATUS'):
                m.d.comb += self.i.ready.eq(1)
                i_status = Status(i_payload)
                with m.If(self.i.valid):
                    with m.Switch(header.cin):
                        with m.Case(USBMidiCIN.SINGLE_BYTE):
                            if self.forward_rt:
                                m.d.comb += [
                                    rt_fifo.i.payload.eq(i_status.nibble.sys.sub.rt),
                                    rt_fifo.i.valid.eq(1),
                                ]
                            m.next = 'DRAIN'
                        with m.Case(USBMidiCIN.NOTE_OFF, USBMidiCIN.NOTE_ON,
                                    USBMidiCIN.POLY_PRESSURE,
                                    USBMidiCIN.CONTROL_CHANGE,
                                    USBMidiCIN.PITCH_BEND,
                                    USBMidiCIN.PROGRAM_CHANGE,
                                    USBMidiCIN.CHANNEL_PRESSURE):
                            m.d.sync += self.o.payload.status.eq(i_status)
                            m.next = 'READ0'
                        with m.Default():
                            m.next = 'DRAIN'

            with m.State('DRAIN'):
                m.d.comb += self.i.ready.eq(1)
                with m.If(self.i.valid & self.i.payload.last):
                    m.next = 'IDLE'

            if self.forward_sysex:
                with m.State('SYSEX-FWD'):
                    with m.If(syx_cnt != 0):
                        # forward payload bytes, honoring o_sysex backpressure
                        m.d.comb += [
                            self.o_sysex.payload.eq(i_payload),
                            self.o_sysex.valid.eq(self.i.valid),
                            self.i.ready.eq(self.o_sysex.ready),
                        ]
                        with m.If(self.i.valid & self.o_sysex.ready):
                            m.d.sync += syx_cnt.eq(syx_cnt - 1)
                            with m.If(self.i.payload.last):
                                m.next = 'IDLE'
                    with m.Else():
                        # padding bytes past the valid count: discard
                        m.d.comb += self.i.ready.eq(1)
                        with m.If(self.i.valid & self.i.payload.last):
                            m.next = 'IDLE'

            with m.State('READ0'):
                m.d.comb += self.i.ready.eq(1)
                with m.If(self.i.valid):
                    m.d.sync += self.o.payload.midi_payload.raw.byte0.eq(i_payload)
                    with m.If((header.cin == USBMidiCIN.PROGRAM_CHANGE) |
                              (header.cin == USBMidiCIN.CHANNEL_PRESSURE)):
                        m.next = 'WAIT-READY'
                    with m.Else():
                        m.next = 'READ1'

            with m.State('READ1'):
                m.d.comb += self.i.ready.eq(1)
                with m.If(self.i.valid):
                    m.d.sync += self.o.payload.midi_payload.raw.byte1.eq(i_payload)
                    m.next = 'WAIT-READY'

            with m.State('WAIT-READY'):
                m.d.comb += self.o.valid.eq(1)
                with m.If(self.o.ready):
                    m.next = 'IDLE'

        return m
