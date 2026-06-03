# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""SID player bitstream: arlet 6502 runs PSID init/play, writes the SID core."""

import os
import sys

from amaranth import *
from amaranth.lib import data, stream, wiring  # data, stream used by A3+ classes
from amaranth.lib.fifo import SyncFIFO, SyncFIFOBuffered  # used by A3+ classes
from amaranth.lib.memory import Memory  # used by A3
from amaranth.lib.wiring import In, Out, connect, flipped  # connect/flipped used by A3+
from amaranth_soc import csr, wishbone  # used by A3+


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

        # Default: single-cycle, no stall (overridden by PSRAM path in A4).
        m.d.comb += self.cpu_RDY.eq(1)

        # --- BRAM ---
        m.d.comb += [
            rd.addr.eq(self.cpu_AB[0:11]),
            wr.addr.eq(self.cpu_AB[0:11]),
            wr.data.eq(self.cpu_DO),
            wr.en.eq(is_bram & self.cpu_WE),
        ]

        # --- SID FIFO ---
        m.d.comb += [
            self._sid_fifo.w_data.eq((self.cpu_DO << 5) | self.cpu_AB[0:5]),
            self._sid_fifo.w_en.eq(is_sid & self.cpu_WE),
        ]

        # --- Read data mux (PSRAM byte added in A4) ---
        with m.If(is_bram):
            m.d.comb += self.cpu_DI.eq(rd.data)
        with m.Else():
            m.d.comb += self.cpu_DI.eq(0xFF)  # placeholder until A4

        return m
