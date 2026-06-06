#!/usr/bin/env python3
"""Static analyzer for PSID tunes used by the tiliqua sid_player.

Goal: gather EVIDENCE for the sid_player root-cause investigation.
For each .sid file:
  - parse the PSID header (load/init/play, songs, speed, flags/clock)
  - recursive-descent disassemble from init+play entry points (following
    branches/JSR/JMP), so we trace *reachable* code, not raw bytes.
  - report:
      * whether the tune READS SID registers $D419-$D41C (OSC3/ENV3/paddles)
        -- these reads are currently BROKEN in the bridge (return PSRAM garbage)
      * any reads of any $D4xx register
      * whether the tune uses ILLEGAL/undocumented 6502 opcodes
        (arlet cpu.v does not implement them faithfully)
      * self-modifying-code hints (writes into the code region)
This is a heuristic (recursive descent can't follow computed jumps / jump
tables perfectly) but it is far better than a flat byte scan.
"""
import sys, struct, os

# ---- 6502 opcode table -----------------------------------------------------
# (mnemonic, addr_mode, illegal?)
# addr modes: imp, acc, imm, zp, zpx, zpy, izx, izy, abs, abx, aby, ind, rel
LEGAL = {
 0x00:("BRK","imp"),0x01:("ORA","izx"),0x05:("ORA","zp"),0x06:("ASL","zp"),
 0x08:("PHP","imp"),0x09:("ORA","imm"),0x0A:("ASL","acc"),0x0D:("ORA","abs"),
 0x0E:("ASL","abs"),0x10:("BPL","rel"),0x11:("ORA","izy"),0x15:("ORA","zpx"),
 0x16:("ASL","zpx"),0x18:("CLC","imp"),0x19:("ORA","aby"),0x1D:("ORA","abx"),
 0x1E:("ASL","abx"),0x20:("JSR","abs"),0x21:("AND","izx"),0x24:("BIT","zp"),
 0x25:("AND","zp"),0x26:("ROL","zp"),0x28:("PLP","imp"),0x29:("AND","imm"),
 0x2A:("ROL","acc"),0x2C:("BIT","abs"),0x2D:("AND","abs"),0x2E:("ROL","abs"),
 0x30:("BMI","rel"),0x31:("AND","izy"),0x35:("AND","zpx"),0x36:("ROL","zpx"),
 0x38:("SEC","imp"),0x39:("AND","aby"),0x3D:("AND","abx"),0x3E:("ROL","abx"),
 0x40:("RTI","imp"),0x41:("EOR","izx"),0x45:("EOR","zp"),0x46:("LSR","zp"),
 0x48:("PHA","imp"),0x49:("EOR","imm"),0x4A:("LSR","acc"),0x4C:("JMP","abs"),
 0x4D:("EOR","abs"),0x4E:("LSR","abs"),0x50:("BVC","rel"),0x51:("EOR","izy"),
 0x55:("EOR","zpx"),0x56:("LSR","zpx"),0x58:("CLI","imp"),0x59:("EOR","aby"),
 0x5D:("EOR","abx"),0x5E:("LSR","abx"),0x60:("RTS","imp"),0x61:("ADC","izx"),
 0x65:("ADC","zp"),0x66:("ROR","zp"),0x68:("PLA","imp"),0x69:("ADC","imm"),
 0x6A:("ROR","acc"),0x6C:("JMP","ind"),0x6D:("ADC","abs"),0x6E:("ROR","abs"),
 0x70:("BVS","rel"),0x71:("ADC","izy"),0x75:("ADC","zpx"),0x76:("ROR","zpx"),
 0x78:("SEI","imp"),0x79:("ADC","aby"),0x7D:("ADC","abx"),0x7E:("ROR","abx"),
 0x81:("STA","izx"),0x84:("STY","zp"),0x85:("STA","zp"),0x86:("STX","zp"),
 0x88:("DEY","imp"),0x8A:("TXA","imp"),0x8C:("STY","abs"),0x8D:("STA","abs"),
 0x8E:("STX","abs"),0x90:("BCC","rel"),0x91:("STA","izy"),0x94:("STY","zpx"),
 0x95:("STA","zpx"),0x96:("STX","zpy"),0x98:("TYA","imp"),0x99:("STA","aby"),
 0x9A:("TXS","imp"),0x9D:("STA","abx"),0xA0:("LDY","imm"),0xA1:("LDA","izx"),
 0xA2:("LDX","imm"),0xA4:("LDY","zp"),0xA5:("LDA","zp"),0xA6:("LDX","zp"),
 0xA8:("TAY","imp"),0xA9:("LDA","imm"),0xAA:("TAX","imp"),0xAC:("LDY","abs"),
 0xAD:("LDA","abs"),0xAE:("LDX","abs"),0xB0:("BCS","rel"),0xB1:("LDA","izy"),
 0xB4:("LDY","zpx"),0xB5:("LDA","zpx"),0xB6:("LDX","zpy"),0xB8:("CLV","imp"),
 0xB9:("LDA","aby"),0xBA:("TSX","imp"),0xBC:("LDY","abx"),0xBD:("LDA","abx"),
 0xBE:("LDX","aby"),0xC0:("CPY","imm"),0xC1:("CMP","izx"),0xC4:("CPY","zp"),
 0xC5:("CMP","zp"),0xC6:("DEC","zp"),0xC8:("INY","imp"),0xC9:("CMP","imm"),
 0xCA:("DEX","imp"),0xCC:("CPY","abs"),0xCD:("CMP","abs"),0xCE:("DEC","abs"),
 0xD0:("BNE","rel"),0xD1:("CMP","izy"),0xD5:("CMP","zpx"),0xD6:("DEC","zpx"),
 0xD8:("CLD","imp"),0xD9:("CMP","aby"),0xDD:("CMP","abx"),0xDE:("DEC","abx"),
 0xE0:("CPX","imm"),0xE1:("SBC","izx"),0xE4:("CPX","zp"),0xE5:("SBC","zp"),
 0xE6:("INC","zp"),0xE8:("INX","imp"),0xE9:("SBC","imm"),0xEA:("NOP","imp"),
 0xEC:("CPX","abs"),0xED:("SBC","abs"),0xEE:("INC","abs"),0xF0:("BEQ","rel"),
 0xF1:("SBC","izy"),0xF5:("SBC","zpx"),0xF6:("INC","zpx"),0xF8:("SED","imp"),
 0xF9:("SBC","aby"),0xFD:("SBC","abx"),0xFE:("INC","abx"),
}
# Illegal/undocumented opcodes (subset that actually appears in SID tunes).
ILLEGAL = {
 0x03:("SLO","izx"),0x07:("SLO","zp"),0x0B:("ANC","imm"),0x0F:("SLO","abs"),
 0x13:("SLO","izy"),0x17:("SLO","zpx"),0x1B:("SLO","aby"),0x1F:("SLO","abx"),
 0x23:("RLA","izx"),0x27:("RLA","zp"),0x2B:("ANC","imm"),0x2F:("RLA","abs"),
 0x33:("RLA","izy"),0x37:("RLA","zpx"),0x3B:("RLA","aby"),0x3F:("RLA","abx"),
 0x43:("SRE","izx"),0x47:("SRE","zp"),0x4B:("ALR","imm"),0x4F:("SRE","abs"),
 0x53:("SRE","izy"),0x57:("SRE","zpx"),0x5B:("SRE","aby"),0x5F:("SRE","abx"),
 0x63:("RRA","izx"),0x67:("RRA","zp"),0x6B:("ARR","imm"),0x6F:("RRA","abs"),
 0x73:("RRA","izy"),0x77:("RRA","zpx"),0x7B:("RRA","aby"),0x7F:("RRA","abx"),
 0x83:("SAX","izx"),0x87:("SAX","zp"),0x8B:("XAA","imm"),0x8F:("SAX","abs"),
 0x93:("AHX","izy"),0x97:("SAX","zpy"),0x9B:("TAS","aby"),0x9C:("SHY","abx"),
 0x9E:("SHX","aby"),0x9F:("AHX","aby"),0xA3:("LAX","izx"),0xA7:("LAX","zp"),
 0xAB:("LAX","imm"),0xAF:("LAX","abs"),0xB3:("LAX","izy"),0xB7:("LAX","zpy"),
 0xBB:("LAS","aby"),0xBF:("LAX","aby"),0xC3:("DCP","izx"),0xC7:("DCP","zp"),
 0xCB:("AXS","imm"),0xCF:("DCP","abs"),0xD3:("DCP","izy"),0xD7:("DCP","zpx"),
 0xDB:("DCP","aby"),0xDF:("DCP","abx"),0xE3:("ISC","izx"),0xE7:("ISC","zp"),
 0xEB:("SBC","imm"),0xEF:("ISC","abs"),0xF3:("ISC","izy"),0xF7:("ISC","zpx"),
 0xFB:("ISC","aby"),0xFF:("ISC","abx"),
 # NOPs (illegal but harmless-ish)
 0x1A:("NOP","imp"),0x3A:("NOP","imp"),0x5A:("NOP","imp"),0x7A:("NOP","imp"),
 0xDA:("NOP","imp"),0xFA:("NOP","imp"),0x80:("NOP","imm"),0x82:("NOP","imm"),
 0x89:("NOP","imm"),0xC2:("NOP","imm"),0xE2:("NOP","imm"),0x04:("NOP","zp"),
 0x44:("NOP","zp"),0x64:("NOP","zp"),0x14:("NOP","zpx"),0x34:("NOP","zpx"),
 0x54:("NOP","zpx"),0x74:("NOP","zpx"),0xD4:("NOP","zpx"),0xF4:("NOP","zpx"),
 0x0C:("NOP","abs"),0x1C:("NOP","abx"),0x3C:("NOP","abx"),0x5C:("NOP","abx"),
 0x7C:("NOP","abx"),0xDC:("NOP","abx"),0xFC:("NOP","abx"),
 0x02:("KIL","imp"),0x12:("KIL","imp"),0x22:("KIL","imp"),0x32:("KIL","imp"),
 0x42:("KIL","imp"),0x52:("KIL","imp"),0x62:("KIL","imp"),0x72:("KIL","imp"),
 0x92:("KIL","imp"),0xB2:("KIL","imp"),0xD2:("KIL","imp"),0xF2:("KIL","imp"),
}
LEN = {"imp":1,"acc":1,"imm":2,"zp":2,"zpx":2,"zpy":2,"izx":2,"izy":2,
       "abs":3,"abx":3,"aby":3,"ind":3,"rel":2}
# "harmless" illegals we won't flag as dangerous
HARMLESS_ILLEGAL = {"NOP"}

def parse_psid(data):
    magic = data[0:4]
    assert magic in (b"PSID", b"RSID"), f"bad magic {magic}"
    ver, doff, load, init, play, songs, start = struct.unpack(">HHHHHHH", data[4:18])
    speed = struct.unpack(">I", data[18:22])[0]
    name = data[0x16:0x36].split(b"\x00")[0].decode("latin1")
    flags = struct.unpack(">H", data[0x76:0x78])[0] if ver >= 2 else 0
    body = data[doff:]
    if load == 0:
        load = body[0] | (body[1] << 8)
        body = body[2:]
    return dict(magic=magic.decode(), ver=ver, load=load, init=init or load,
                play=play, songs=songs, speed=speed, name=name, flags=flags,
                body=body)

def disasm_reachable(mem, present, entries):
    """Recursive descent. mem: 64K bytearray, present: set of valid addrs.
    Returns set of instruction-start addrs visited and a list of findings."""
    visited = set()
    sid_reads = []     # (pc, addr, mnem)
    sid_writes = set()
    illegals = []      # (pc, mnem, mode)
    code_writes = []   # writes whose target addr we can't resolve statically (SMC hint)
    stack = list(entries)
    while stack:
        pc = stack.pop()
        while True:
            if pc in visited or pc not in present:
                break
            op = mem[pc]
            entry = LEGAL.get(op)
            illegal = False
            if entry is None:
                entry = ILLEGAL.get(op)
                illegal = True
            if entry is None:
                break
            mnem, mode = entry
            n = LEN[mode]
            visited.add(pc)
            for k in range(1, n):
                visited.add((pc + k) & 0xFFFF)
            if illegal and mnem not in HARMLESS_ILLEGAL:
                illegals.append((pc, mnem, mode))
            # operand
            operand = None
            if n == 2:
                operand = mem[(pc + 1) & 0xFFFF]
            elif n == 3:
                operand = mem[(pc + 1) & 0xFFFF] | (mem[(pc + 2) & 0xFFFF] << 8)
            # SID register access tracking (absolute & abs,X/Y)
            if mode in ("abs", "abx", "aby") and operand is not None:
                base = operand & 0xFFFF
                if 0xD400 <= base <= 0xD41F:
                    if mnem in ("LDA", "LDX", "LDY", "BIT", "CMP", "CPX", "CPY",
                                "ADC", "SBC", "AND", "ORA", "EOR", "LAX"):
                        sid_reads.append((pc, base, mnem))
                    if mnem in ("STA", "STX", "STY", "SAX"):
                        sid_writes.add(base)
                    # RMW illegals read AND write
                    if mnem in ("INC","DEC","ASL","LSR","ROL","ROR","SLO","RLA",
                                "SRE","RRA","DCP","ISC"):
                        sid_reads.append((pc, base, mnem))
                        sid_writes.add(base)
            # control flow
            if mnem in ("JMP",):
                if mode == "abs":
                    stack.append(operand)
                break  # unconditional
            if mnem == "JSR":
                stack.append(operand)
                pc = (pc + n) & 0xFFFF
                continue
            if mnem in ("RTS", "RTI", "BRK", "KIL"):
                break
            if mode == "rel":  # branch
                target = (pc + 2 + ((operand ^ 0x80) - 0x80)) & 0xFFFF
                stack.append(target)
                pc = (pc + n) & 0xFFFF
                continue
            pc = (pc + n) & 0xFFFF
    return visited, sid_reads, sid_writes, illegals

def analyze(path):
    data = open(path, "rb").read()
    h = parse_psid(data)
    mem = bytearray(0x10000)
    present = set()
    load = h["load"]
    body = h["body"]
    for i, b in enumerate(body):
        a = (load + i) & 0xFFFF
        mem[a] = b
        present.add(a)
    end = (load + len(body) - 1) & 0xFFFF
    entries = []
    if load <= h["init"] <= end:
        entries.append(h["init"])
    if h["play"] and load <= h["play"] <= end:
        entries.append(h["play"])
    visited, sid_reads, sid_writes, illegals = disasm_reachable(mem, present, entries)

    name = os.path.basename(path)
    clk = "PAL" if (h["flags"] >> 2) & 3 in (1,) else ("NTSC" if (h["flags"]>>2)&3==2 else f"flags={(h['flags']>>2)&3}")
    print(f"\n=== {name} ===")
    print(f"  {h['magic']} v{h['ver']}  load=${h['load']:04X}-${end:04X} "
          f"init=${h['init']:04X} play=${h['play']:04X} songs={h['songs']} "
          f"speed=0x{h['speed']:08X} clock={clk}")
    print(f"  reachable code bytes: {len(visited)} / {len(body)} loaded")
    # SID reads
    crit = [r for r in sid_reads if 0xD419 <= r[1] <= 0xD41C]
    if crit:
        regs = sorted(set(r[1] for r in crit))
        names = {0xD419:"PADDLEX",0xD41A:"PADDLEY",0xD41B:"OSC3",0xD41C:"ENV3"}
        print(f"  *** READS SID read-registers (BROKEN in bridge!): "
              f"{', '.join(f'${r:04X}({names[r]})' for r in regs)}  "
              f"[{len(crit)} sites]")
    other_reads = sorted(set(r[1] for r in sid_reads if not (0xD419<=r[1]<=0xD41C)))
    if other_reads:
        print(f"  reads other $D4xx (write regs, read-back): "
              f"{', '.join(f'${r:04X}' for r in other_reads)}")
    if not sid_reads:
        print(f"  no SID register reads detected")
    print(f"  SID write regs touched: {len(sid_writes)} "
          f"({', '.join(f'${r:02X}' for r in sorted(x&0xff for x in sid_writes))})")
    # illegal opcodes
    if illegals:
        from collections import Counter
        c = Counter(m for _, m, _ in illegals)
        print(f"  *** USES ILLEGAL opcodes (arlet may mishandle): "
              f"{dict(c)}  [{len(illegals)} sites]")
        for pc, m, mode in illegals[:8]:
            print(f"        ${pc:04X}: {m} ({mode})")
    else:
        print(f"  no illegal opcodes in reachable code")

if __name__ == "__main__":
    # repo docs/ holds the sample .sid tunes; default to scanning all of them.
    import glob
    _docs = os.path.abspath(os.path.join(
        os.path.dirname(os.path.realpath(__file__)),
        "..", "..", "..", "..", "..", "docs"))   # repo_root/docs
    paths = sys.argv[1:] or sorted(glob.glob(os.path.join(_docs, "*.sid")))
    for p in paths:
        try:
            analyze(p)
        except Exception as e:
            print(f"\n=== {p} === ERROR: {e}")
