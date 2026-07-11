# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""Shared CSR peripheral wrapping guh USBMSCHost.

Hoisted verbatim from `top/sid_player_sw/top.py` so it can be reused by other
bitstreams (e.g. `top/mbsid`) that need to share a USB-C port between
USB-MIDI-host and mass-storage. `with_mode=False` (the default) reproduces
sid_player_sw's original CSR map and elaboration byte-for-byte; `with_mode=True`
adds one opt-in register (`mode` at offset 0x1C) plus a `mode_o` port used to
mux ownership of the underlying USB PHY.
"""

from amaranth import *
from amaranth.lib import data, stream, wiring
from amaranth.lib.fifo import SyncFIFOBuffered
from amaranth.lib.wiring import In, Out
from amaranth import ResetInserter

from luna.gateware.stream.future import Packet

from amaranth_soc import csr


USB_STATUS_LAYOUT = data.StructLayout({
    "connected": 1, "ready": 1, "busy": 1,
    "block_size": 16, "block_count": 32})
USB_RESP_LAYOUT = data.StructLayout({"done": 1, "error": 1})


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

    class Mode(csr.Register, access="rw"):
        """USB port owner: 0 = USB-MIDI host engine (default), 1 = MSC (storage).
        Firmware mirrors the menu's `USB Mode` row here every redraw."""
        storage: csr.Field(csr.action.RW, unsigned(1))

    # Ports — all class-level so no dict/annotation conflict.
    bus:       In(csr.Signature(addr_width=5, data_width=8))
    rx_data:   In(stream.Signature(Packet(unsigned(8))))
    lba_o:     Out(32)
    start_o:   Out(1)
    status_i:  In(USB_STATUS_LAYOUT)
    resp_i:    In(USB_RESP_LAYOUT)
    mode_o:    Out(1)   # with_mode only: 0 = USB-MIDI owns the PHY, 1 = MSC

    def __init__(self, *, word_fifo_depth=256, with_mode=False):
        self._with_mode = with_mode
        self._word_fifo = SyncFIFOBuffered(width=32, depth=word_fifo_depth)
        regs = csr.Builder(addr_width=5, data_width=8)
        self._status      = regs.add("status",      self.Status(),     offset=0x00)
        self._block_size  = regs.add("block_size",  self.BlockSize(),  offset=0x04)
        self._block_count = regs.add("block_count", self.BlockCount(), offset=0x08)
        self._lba         = regs.add("lba",         self.Lba(),        offset=0x0C)
        self._start       = regs.add("start",       self.Start(),      offset=0x10)
        self._rx_data_reg = regs.add("rx_data",     self.RxData(),     offset=0x14)
        self._resp        = regs.add("resp",        self.Resp(),       offset=0x18)
        if with_mode:
            self._mode = regs.add("mode", self.Mode(), offset=0x1C)
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

        if self._with_mode:
            m.d.comb += self.mode_o.eq(self._mode.f.storage.data)

        return m
