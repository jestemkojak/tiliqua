#!/usr/bin/env python3
"""Measure the in-bridge read-cache win under realistic HyperRAM latency.

Exports the real Cpu6502Bridge (with icache_lines=0 and =N), runs the REAL
arlet 6502 under iverilog against a behavioural PSRAM with a fixed multi-cycle
latency (HyperRAM single-word ~ tens of cycles), and reports:
  - PSRAM read transactions (the thing the cache removes)
  - PSRAM write transactions (unchanged; cache is write-through)
  - cycles to reach NMI_TARGET play-handler entries (throughput proxy)
Run: pdm run python zfilter/measure_cache.py
"""
import os, sys, struct, subprocess, tempfile, re

_HERE = os.path.dirname(os.path.realpath(__file__))   # .../src/top/sid_player/tools
_SID_PLAYER = os.path.dirname(_HERE)                   # .../src/top/sid_player
sys.path.insert(0, _SID_PLAYER)
from amaranth.back import verilog
import importlib.util
_spec = importlib.util.spec_from_file_location(
    "_sp_top", os.path.join(_SID_PLAYER, "top.py"))
_sp = importlib.util.module_from_spec(_spec)
sys.modules["_sp_top"] = _sp
_spec.loader.exec_module(_sp)
Cpu6502Bridge = _sp.Cpu6502Bridge

ROOT = os.path.abspath(os.path.join(_HERE, "..", "..", "..", ".."))  # gateware/
ARLET = os.path.join(ROOT, "deps", "arlet-6502")
TUNE = os.path.join(ROOT, "tests", "data", "tiny_tune.bin")

def bridge_verilog(icache_lines):
    b = Cpu6502Bridge(psram_base_bytes=0x0, icache_lines=icache_lines)
    pb = b.psram_bus
    ports = [b.cpu_AB, b.cpu_DO, b.cpu_WE, b.cpu_DI, b.cpu_RDY,
             b.sid_w_en, b.sid_w_data,
             pb.adr, pb.dat_w, pb.dat_r, pb.sel, pb.cyc, pb.stb, pb.we,
             pb.ack, pb.cti, pb.bte]
    return verilog.convert(b, name="bridge", ports=ports)

def tune_hex():
    data = open(TUNE, "rb").read()
    words = [struct.unpack_from("<I", data, i)[0] for i in range(0, len(data), 4)]
    words += [0] * (16384 - len(words))
    return "\n".join(f"{w:08x}" for w in words) + "\n"

def big_tune_hex(body_instrs=200):
    """Synthetic tune whose NMI handler is a long straight-line run of code,
    so its instruction footprint (~3*body_instrs bytes) far exceeds the small
    in-bridge cache.  This models a real play routine: across calls the loop is
    too big to stay resident, so the only win is intra-word reuse of the 32-bit
    fetch -> the realistic ~4x code-fetch reduction (not the tiny_tune 100%).
    Layout (all >$07FF so it's in 'PSRAM'):
      $0800 reset: LDX #$FF; TXS; JMP $0806 (spin)
      $0820 NMI:   body_instrs x (LDA #$nn ; STA $D404) ; RTI
                   STA $D404 is a SID write (FIFO), keeps SID_WRITES alive and
                   adds realistic stores; the LDA/STA opcode+operand bytes are
                   the PSRAM code fetches we are measuring.
      vectors: NMI->$0820 RESET->$0800 IRQ->$0820
    """
    mem = bytearray(0x10000)
    # reset
    mem[0x0800:0x0806] = bytes([0xA2,0xFF, 0x9A, 0x4C,0x06,0x08])
    mem[0x0806:0x0809] = bytes([0x4C,0x06,0x08])  # spin: JMP $0806
    pc = 0x0820
    for i in range(body_instrs):
        mem[pc]=0xA9; mem[pc+1]=i & 0xFF                 # LDA #i
        mem[pc+2]=0x8D; mem[pc+3]=0x04; mem[pc+4]=0xD4   # STA $D404
        pc += 5
    mem[pc]=0x40                                          # RTI
    mem[0xFFFA]=0x20; mem[0xFFFB]=0x08   # NMI -> $0820
    mem[0xFFFC]=0x00; mem[0xFFFD]=0x08   # RESET -> $0800
    mem[0xFFFE]=0x20; mem[0xFFFF]=0x08   # IRQ -> $0820
    words = [struct.unpack_from("<I", mem, i)[0] for i in range(0, 0x10000, 4)]
    words = words[:16384] + [0]*(16384-min(16384,len(words)))
    return "\n".join(f"{w:08x}" for w in words[:16384]) + "\n", 0x0820

TB = r"""
`timescale 1ns/1ps
module tb;
  reg clk = 0; always #5 clk = ~clk;
  wire [15:0] AB; wire [7:0] DO, DI; wire WE, RDY; reg cpu_reset = 1;
  wire [21:0] wb_adr; wire [31:0] wb_dat_w; reg [31:0] wb_dat_r;
  wire [3:0] wb_sel; wire wb_cyc, wb_stb, wb_we; reg wb_ack = 0;
  wire [2:0] wb_cti; wire [1:0] wb_bte;
  wire sid_w_en; wire [15:0] sid_w_data; reg bridge_rst = 1;

  reg [15:0] nmi_div = 0; reg nmi_l = 0; wire NMI = nmi_l;
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

  // behavioural PSRAM with fixed multi-cycle latency (models HyperRAM word).
  reg [31:0] mem [0:16383]; integer lat = 0; localparam LATENCY = %LAT%;
  integer reads = 0, writes = 0;
  always @(posedge clk) begin
    if (wb_cyc && wb_stb && !wb_ack) begin
      if (lat == LATENCY) begin
        wb_ack <= 1; wb_dat_r <= mem[wb_adr[13:0]];
        if (wb_we) begin mem[wb_adr[13:0]] <= wb_dat_w; writes <= writes + 1; end
        else reads <= reads + 1;
        lat <= 0;
      end else lat <= lat + 1;
    end else begin wb_ack <= 0; lat <= 0; end
  end

  integer sid_count = 0, nmi_enters = 0; reg in_handler = 0;
  integer cyc = 0; integer cyc_at_target = -1; localparam NMI_TARGET = %TGT%;
  always @(posedge clk) if (!cpu_reset) cyc <= cyc + 1;
  always @(posedge clk) begin
    if (sid_w_en) sid_count <= sid_count + 1;
    if (AB == 16'h0820 && !in_handler) begin
      nmi_enters <= nmi_enters + 1; in_handler <= 1;
      if (nmi_enters + 1 == NMI_TARGET && cyc_at_target < 0) cyc_at_target <= cyc;
    end else if (AB != 16'h0820) in_handler <= 0;
  end

  initial begin
    $readmemh("tune.hex", mem);
    cpu_reset = 1; bridge_rst = 1; repeat (8) @(posedge clk);
    bridge_rst = 0; repeat (4) @(posedge clk); cpu_reset = 0;
    repeat (%RUN%) @(posedge clk);
    $display("READS=%0d", reads); $display("WRITES=%0d", writes);
    $display("SID_WRITES=%0d", sid_count); $display("NMI_ENTERS=%0d", nmi_enters);
    $display("CYC_AT_TARGET=%0d", cyc_at_target);
    $finish;
  end
endmodule
"""

def run(icache_lines, latency, hexdata, target=5, run_cycles=400000):
    with tempfile.TemporaryDirectory() as d:
        open(os.path.join(d, "bridge.v"), "w").write(bridge_verilog(icache_lines))
        open(os.path.join(d, "cpu.v"), "w").write(open(os.path.join(ARLET, "cpu.v")).read())
        open(os.path.join(d, "ALU.v"), "w").write(open(os.path.join(ARLET, "ALU.v")).read())
        tb = (TB.replace("%LAT%", str(latency)).replace("%TGT%", str(target))
                .replace("%RUN%", str(run_cycles)))
        open(os.path.join(d, "tb.v"), "w").write(tb)
        open(os.path.join(d, "tune.hex"), "w").write(hexdata)
        subprocess.run(["iverilog", "-g2012", "-o", "sim", "tb.v", "bridge.v",
                        "cpu.v", "ALU.v"], cwd=d, check=True,
                       capture_output=True, text=True)
        out = subprocess.run(["vvp", "sim"], cwd=d, capture_output=True, text=True).stdout
        return {k: int(v) for k, v in re.findall(r"(\w+)=(-?\d+)", out)}

if __name__ == "__main__":
    tiny = tune_hex()
    print("########## tiny_tune.bin (hot loop fits entirely in cache) ##########")
    for lat in (24, 48):
        base = run(0, lat, tiny); cached = run(8, lat, tiny)
        red = 100*(base['READS']-cached['READS'])/base['READS'] if base['READS'] else 0
        print(f"  lat={lat:2d}: reads {base['READS']:6d} -> {cached['READS']:5d} "
              f"({red:.1f}% fewer)  sid_w {base['SID_WRITES']}->{cached['SID_WRITES']} "
              f"nmi {base['NMI_ENTERS']}->{cached['NMI_ENTERS']}  [correctness check]")

    big, _ = big_tune_hex(body_instrs=200)
    print("\n########## synthetic big play() (~1000 B handler >> cache) ##########")
    print("  realistic: working set exceeds cache, so only intra-32-bit-word reuse")
    lat = 48
    base = run(0, lat, big, run_cycles=2_000_000)
    print(f"  {'cache':>8s} {'reads':>8s} {'reduction':>10s} {'sid_w':>6s} {'nmi':>4s}")
    print(f"  {'none':>8s} {base['READS']:8d} {'--':>10s} "
          f"{base['SID_WRITES']:6d} {base['NMI_ENTERS']:4d}")
    for nl in (4, 8, 16, 32):
        c = run(nl, lat, big, run_cycles=2_000_000)
        red = 100*(base['READS']-c['READS'])/base['READS'] if base['READS'] else 0
        ratio = base['READS']/c['READS'] if c['READS'] else 0
        print(f"  {('icache='+str(nl)):>8s} {c['READS']:8d} "
              f"{red:7.1f}% {ratio:.2f}x {c['SID_WRITES']:6d} {c['NMI_ENTERS']:4d}")
