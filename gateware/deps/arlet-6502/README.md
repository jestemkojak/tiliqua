# arlet verilog-6502 (vendored)

Source: Arlet Ottens, https://github.com/Arlet/verilog-6502
Vendored at commit e930327ffecc5062bfce70bbcfba2bbfa6de6e4c. License: see LICENSE.
Used by the `sid_player` bitstream via gateware/src/top/sid_player/top.py.

## Files

- `cpu.v` - 6502 CPU top module (`module cpu(clk, reset, AB, DI, DO, WE, IRQ, NMI, RDY)`)
- `ALU.v` - ALU submodule, instantiated by `cpu.v` (`module ALU(clk, op, right, AI, BI, CI, CO, BCD, OUT, V, Z, N, HC, RDY)`)
- `LICENSE` - zlib license

## Module Interface

```
module cpu( clk, reset, AB, DI, DO, WE, IRQ, NMI, RDY );
  input clk;              // CPU clock
  input reset;            // reset signal
  output reg [15:0] AB;   // address bus
  input [7:0] DI;         // data in, read bus
  output [7:0] DO;        // data out, write bus
  output WE;              // write enable
  input IRQ;              // interrupt request
  input NMI;              // non-maskable interrupt request
  input RDY;              // Ready signal. Pauses CPU when RDY=0
```
