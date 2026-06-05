"""Co-simulation: the REAL arlet 6502 (cpu.v) driving the REAL Amaranth bridge.

The other sid_player tests drive the bridge with a hand-written fake CPU or use
the behavioural Cpu6502 stub (platform=None), so the actual arlet-core ↔ bridge
RDY/AB/DI handshake — the thing that runs on hardware — was never simulated.

This test exports the bridge to Verilog, wires it to the arlet cpu.v + ALU.v and
a behavioural wishbone PSRAM preloaded with tiny_tune.bin, and runs it under
iverilog. tiny_tune.bin's reset vector is $0800 and its NMI play routine does
`STA $D4xx`, so a correctly-working core+bridge must emit SID writes
(bridge.sid_w_en pulses). Zero writes ⟹ the CPU never reaches the tune
(e.g. a BRK/interrupt storm from reading stale/zero memory).
"""
import os
import shutil
import struct
import subprocess
import tempfile
import unittest

from amaranth.back import verilog

from top.sid_player.top import Cpu6502Bridge

HERE = os.path.dirname(os.path.realpath(__file__))
ARLET = os.path.join(HERE, "../deps/arlet-6502")
TINY = os.path.join(HERE, "data/tiny_tune.bin")


def _bridge_verilog():
    b = Cpu6502Bridge(psram_base_bytes=0x0)
    pb = b.psram_bus
    ports = [b.cpu_AB, b.cpu_DO, b.cpu_WE, b.cpu_DI, b.cpu_RDY,
             b.sid_w_en, b.sid_w_data,
             pb.adr, pb.dat_w, pb.dat_r, pb.sel, pb.cyc, pb.stb, pb.we,
             pb.ack, pb.cti, pb.bte]
    return verilog.convert(b, ports=ports, name="bridge")


def _tune_hex():
    with open(TINY, "rb") as f:
        data = f.read()
    words = [struct.unpack_from("<I", data, i)[0] for i in range(0, len(data), 4)]
    words += [0] * (16384 - len(words))
    return "\n".join(f"{w:08x}" for w in words) + "\n"


_TB = r"""
`timescale 1ns/1ps
module tb;
  reg clk = 0;
  always #5 clk = ~clk;

  wire [15:0] AB;
  wire [7:0]  DO, DI;
  wire        WE, RDY;
  reg         cpu_reset = 1;

  wire [21:0] wb_adr;
  wire [31:0] wb_dat_w;
  reg  [31:0] wb_dat_r;
  wire [3:0]  wb_sel;
  wire        wb_cyc, wb_stb, wb_we;
  reg         wb_ack = 0;
  wire [2:0]  wb_cti;
  wire [1:0]  wb_bte;

  wire        sid_w_en;
  wire [15:0] sid_w_data;
  reg         bridge_rst = 1;

  // periodic NMI, latched; cleared when the CPU fetches the NMI vector ($FFFA)
  reg [15:0] nmi_div = 0;
  reg        nmi_l   = 0;
  wire       NMI = nmi_l;
  always @(posedge clk) begin
    if (cpu_reset) begin nmi_div <= 0; nmi_l <= 0; end
    else begin
      nmi_div <= nmi_div + 1;
      if (nmi_div == 16'd1999) begin nmi_div <= 0; nmi_l <= 1; end
      else if (AB == 16'hFFFA) nmi_l <= 0;
    end
  end

  cpu arlet(.clk(clk), .reset(cpu_reset), .AB(AB), .DI(DI), .DO(DO),
            .WE(WE), .IRQ(1'b0), .NMI(NMI), .RDY(RDY));

  bridge br(.clk(clk), .rst(bridge_rst),
            .cpu_AB(AB), .cpu_DO(DO), .cpu_WE(WE), .cpu_DI(DI), .cpu_RDY(RDY),
            .sid_w_en(sid_w_en), .sid_w_data(sid_w_data),
            .psram_bus__adr(wb_adr), .psram_bus__dat_w(wb_dat_w),
            .psram_bus__dat_r(wb_dat_r), .psram_bus__sel(wb_sel),
            .psram_bus__cyc(wb_cyc), .psram_bus__stb(wb_stb),
            .psram_bus__we(wb_we), .psram_bus__ack(wb_ack),
            .psram_bus__cti(wb_cti), .psram_bus__bte(wb_bte));

  // behavioural classic-wishbone PSRAM: 16384 words, fixed read/write latency
  reg [31:0] mem [0:16383];
  integer lat = 0;
  localparam LATENCY = 4;
  always @(posedge clk) begin
    if (wb_cyc && wb_stb && !wb_ack) begin
      if (lat == LATENCY) begin
        wb_ack   <= 1;
        wb_dat_r <= mem[wb_adr[13:0]];
        if (wb_we) mem[wb_adr[13:0]] <= wb_dat_w;
        lat      <= 0;
      end else lat <= lat + 1;
    end else begin
      wb_ack <= 0;
      lat    <= 0;
    end
  end

  // count SID register writes, and record whether the CPU reached the tune's
  // post-store spin loop at $0808 (only reachable if STA $D404 executed).
  // Also: latch the SID-write count at the moment init completes (first $0808),
  // and count entries into the NMI play handler ($0820, `INC $D400; RTI`), so a
  // separate assertion can prove the NMI trampoline keeps writing the SID past
  // the single init write — not just the reset/init path.
  integer sid_count   = 0;
  integer saw_spin    = 0;
  integer sid_at_init = 0;
  integer nmi_enters  = 0;
  reg     in_handler  = 0;
  always @(posedge clk) begin
    if (sid_w_en) sid_count <= sid_count + 1;
    if (AB == 16'h0808) begin
      if (!saw_spin) sid_at_init <= sid_count;
      saw_spin <= 1;
    end
    // count a handler entry on each fresh fetch of $0820 (debounced)
    if (AB == 16'h0820 && !in_handler) begin
      nmi_enters <= nmi_enters + 1;
      in_handler <= 1;
    end else if (AB != 16'h0820) begin
      in_handler <= 0;
    end
  end

  always @(posedge clk) begin
    if (!cpu_reset && $time/10 < 200) begin
      $display("t=%0d st=%0d AB=%04x RDY=%0b DI=%02x WE=%0b DO=%02x is_psram=%0b phase=%0d",
               $time/10, arlet.state, AB, RDY, DI, WE, DO, br.is_psram_r, br.phase);
    end
  end

  initial begin
    $readmemh("tune.hex", mem);
    cpu_reset = 1; bridge_rst = 1;
    repeat (8) @(posedge clk);
    bridge_rst = 0;
    repeat (4) @(posedge clk);
    cpu_reset = 0;
    repeat (12000) @(posedge clk);
    $display("SID_WRITES=%0d", sid_count);
    $display("SAW_SPIN=%0d", saw_spin);
    $display("SID_AT_INIT=%0d", sid_at_init);
    $display("NMI_ENTERS=%0d", nmi_enters);
    $display("FINAL_AB=%04x", AB);
    $finish;
  end
endmodule
"""


# Reference harness: the SAME arlet core against an ideal *registered*
# single-cycle memory (DI = mem[AB] one cycle later, RDY=1). This is the bus
# protocol the arlet actually expects; it proves the core + tiny_tune.bin are
# sound, so any failure of the bridge cosim above is the bridge's bus timing,
# not the core or the image.
_REF_TB = r"""
`timescale 1ns/1ps
module tb;
  reg clk = 0;
  always #5 clk = ~clk;
  wire [15:0] AB; wire [7:0] DO; reg [7:0] DI; wire WE; wire RDY = 1'b1;
  reg cpu_reset = 1;
  reg [7:0] bmem [0:65535];
  always @(posedge clk) begin
    DI <= bmem[AB];
    if (WE) bmem[AB] <= DO;
  end
  cpu arlet(.clk(clk), .reset(cpu_reset), .AB(AB), .DI(DI), .DO(DO),
            .WE(WE), .IRQ(1'b0), .NMI(1'b0), .RDY(RDY));
  integer sid_count = 0, saw_spin = 0;
  always @(posedge clk) begin
    if (!cpu_reset && AB >= 16'hD400 && AB <= 16'hD41F && WE)
      sid_count <= sid_count + 1;
    if (!cpu_reset && AB == 16'h0808) saw_spin <= 1;
  end
  initial begin
    $readmemh("tune_bytes.hex", bmem);
    cpu_reset = 1; repeat (8) @(posedge clk); cpu_reset = 0;
    repeat (4000) @(posedge clk);
    $display("SID_WRITES=%0d", sid_count);
    $display("SAW_SPIN=%0d", saw_spin);
    $display("FINAL_AB=%04x", AB);
    $finish;
  end
endmodule
"""


def _tune_bytes_hex():
    with open(TINY, "rb") as f:
        data = f.read()
    return "\n".join(f"{b:02x}" for b in data) + "\n"


@unittest.skipUnless(shutil.which("iverilog") and shutil.which("vvp"),
                     "iverilog/vvp not available")
class Cpu6502CosimTests(unittest.TestCase):
    def _run(self, files, tb, hexes):
        """Compile + run an iverilog sim; return parsed DISPLAY fields dict."""
        with tempfile.TemporaryDirectory() as d:
            paths = []
            for name, text in files.items():
                p = os.path.join(d, name)
                with open(p, "w") as f:
                    f.write(text)
                if name.endswith(".v"):
                    paths.append(p)
            for name, text in hexes.items():
                with open(os.path.join(d, name), "w") as f:
                    f.write(text)
            tbp = os.path.join(d, "tb.v")
            with open(tbp, "w") as f:
                f.write(tb)

            vvp = os.path.join(d, "sim.vvp")
            comp = subprocess.run(
                ["iverilog", "-g2012", "-o", vvp, tbp, *paths,
                 os.path.join(ARLET, "cpu.v"), os.path.join(ARLET, "ALU.v")],
                capture_output=True, text=True)
            self.assertEqual(comp.returncode, 0, f"iverilog failed:\n{comp.stderr}")
            run = subprocess.run(["vvp", vvp], cwd=d,
                                 capture_output=True, text=True, timeout=300)
            print(run.stdout)
            out = {}
            for line in run.stdout.splitlines():
                if "=" in line:
                    k, v = line.split("=", 1)
                    out[k.strip()] = v.strip()
            return out

    def test_reference_registered_memory_plays_tune(self):
        """Sanity: arlet + ideal registered memory must run the tune (proves the
        core and tiny_tune.bin are good, isolating the bridge as the variable)."""
        out = self._run({}, _REF_TB, {"tune_bytes.hex": _tune_bytes_hex()})
        self.assertTrue(int(out.get("SAW_SPIN", "0")),
                        f"reference core never reached $0808 (AB={out.get('FINAL_AB')})")
        self.assertGreater(int(out.get("SID_WRITES", "0")), 0,
                           "reference core produced no SID writes — core/image broken")

    def test_real_arlet_core_plays_tune_through_bridge(self):
        out = self._run({"bridge.v": _bridge_verilog()}, _TB,
                        {"tune.hex": _tune_hex()})
        self.assertTrue(int(out.get("SAW_SPIN", "0")),
                        "CPU never reached the tune spin loop $0808 — stuck in a "
                        f"reset/BRK storm (final AB={out.get('FINAL_AB')}). The "
                        "bridge's RDY/DI bus timing does not match the arlet "
                        "core's pipelined protocol.")
        self.assertGreater(int(out.get("SID_WRITES", "0")), 0,
                           "real arlet core + bridge produced no SID writes")
        # Cover the NMI play-routine trampoline, not just reset/init: the
        # periodic NMI must enter the $0820 handler and its `INC $D400` must
        # keep the SID-write count climbing past the single init STA $D404.
        self.assertGreater(int(out.get("NMI_ENTERS", "0")), 1,
                           "NMI play handler ($0820) was never re-entered — "
                           "the NMI vector fetch / trampoline path is broken")
        self.assertGreater(int(out.get("SID_WRITES", "0")),
                           int(out.get("SID_AT_INIT", "0")),
                           "SID writes did not climb after init — the NMI play "
                           "routine is not writing the SID")


if __name__ == "__main__":
    unittest.main(verbosity=2)
