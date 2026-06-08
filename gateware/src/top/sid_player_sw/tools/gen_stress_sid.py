#!/usr/bin/env python3
# Copyright (c) 2026
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""Generate a PSID stress-test tune for the sid_player_sw player.

The tune hammers the SID + 6502 every PLAY call to surface timing issues
(6502 throughput, SID-write rate, scope/PSRAM contention):

  * 3-voice fast arpeggio (a C-minor pentatonic run, voices a 3rd apart),
    new frequencies written to all three voices every PLAY call,
  * a continuous triangle filter-cutoff sweep (writes $D415/$D416 every call),
  * rotating waveform (tri -> saw -> pulse -> noise) on all 3 voices.

Two outputs are produced:
  * stress_vblank.sid  — 50 Hz VBlank timing (musical, baseline).
  * stress_fast.sid    — CIA multispeed (~200 Hz) — 4x the PLAY rate / load,
    to push the throughput limit. If audio glitches here but not in the
    VBlank build, you've found the timing ceiling.

The 6502 is hand-assembled by a tiny two-pass assembler below, then run through
a minimal 6502 simulator (INIT + N PLAY frames) to prove it terminates and
actually writes the SID before the file is emitted — so a bad opcode/branch
can't ship a tune that spins the real player.

Usage:  python3 gen_stress_sid.py [out_dir]   (default: this script's dir)
"""

import os
import sys
import struct

# ---------------------------------------------------------------------------
# Tiny 6502 assembler (only the opcodes/modes this tune uses).
# ---------------------------------------------------------------------------

# mnemonic -> (opcode, mode). Modes: imm, zp, abs, absx, absy, impl, rel.
OPS = {
    "LDA#": (0xA9, "imm"),  "LDA": (0xAD, "abs"), "LDAZ": (0xA5, "zp"),
    "LDAX": (0xBD, "absx"), "LDAY": (0xB9, "absy"),
    "LDX#": (0xA2, "imm"),  "LDY#": (0xA0, "imm"),
    "STA":  (0x8D, "abs"),  "STAZ": (0x85, "zp"),
    "STAX": (0x9D, "absx"), "STAY": (0x99, "absy"),
    "INCZ": (0xE6, "zp"),   "DECZ": (0xC6, "zp"),
    "AND#": (0x29, "imm"),  "ORA#": (0x09, "imm"), "EOR#": (0x49, "imm"),
    "ADC#": (0x69, "imm"),  "SBC#": (0xE9, "imm"), "ADCZ": (0x65, "zp"),
    "CMP#": (0xC9, "imm"),  "CPX#": (0xE0, "imm"), "CPY#": (0xC0, "imm"),
    "TAX": (0xAA, "impl"),  "TAY": (0xA8, "impl"),
    "TXA": (0x8A, "impl"),  "TYA": (0x98, "impl"),
    "INX": (0xE8, "impl"),  "INY": (0xC8, "impl"),
    "DEX": (0xCA, "impl"),  "DEY": (0x88, "impl"),
    "CLC": (0x18, "impl"),  "SEC": (0x38, "impl"),
    "RTS": (0x60, "impl"),  "NOP": (0xEA, "impl"),
    "BNE": (0xD0, "rel"),   "BEQ": (0xF0, "rel"),
    "BPL": (0x10, "rel"),   "BMI": (0x30, "rel"),
    "BCC": (0x90, "rel"),   "BCS": (0xB0, "rel"),
    "JMP": (0x4C, "abs"),   "JSR": (0x20, "abs"),
}
SIZE = {"imm": 2, "zp": 2, "abs": 3, "absx": 3, "absy": 3, "impl": 1, "rel": 2}


def assemble(program, org):
    """program: list of (label_or_None, mnemonic, operand). `.byte` mnemonic
    takes a list operand. operand may be an int or a label name (str)."""
    # Normalise entries to (label, mnem, operand); impl ops may omit operand.
    program = [(e[0], e[1], e[2] if len(e) > 2 else None) for e in program]
    # Pass 1: assign addresses to labels.
    labels, addr = {}, org
    for label, mnem, operand in program:
        if label:
            labels[label] = addr
        if mnem == ".byte":
            addr += len(operand)
        else:
            addr += SIZE[OPS[mnem][1]]
    # Pass 2: emit bytes.
    out, addr = bytearray(), org
    def val(o):
        return labels[o] if isinstance(o, str) else o
    for _, mnem, operand in program:
        if mnem == ".byte":
            out += bytes(operand); addr += len(operand); continue
        op, mode = OPS[mnem]
        sz = SIZE[mode]
        out.append(op)
        if mode == "impl":
            pass
        elif mode in ("imm", "zp"):
            out.append(val(operand) & 0xFF)
        elif mode in ("abs", "absx", "absy"):
            v = val(operand) & 0xFFFF
            out += bytes([v & 0xFF, (v >> 8) & 0xFF])
        elif mode == "rel":
            rel = val(operand) - (addr + 2)
            assert -128 <= rel <= 127, f"branch out of range to {operand!r} ({rel})"
            out.append(rel & 0xFF)
        addr += sz
    return bytes(out), labels


# ---------------------------------------------------------------------------
# Minimal 6502 simulator — just enough to validate the generated tune.
# ---------------------------------------------------------------------------

class Sim:
    def __init__(self, mem):
        self.m = bytearray(mem)
        self.a = self.x = self.y = 0
        self.sp = 0xFD
        self.pc = 0
        self.n = self.z = self.c = 0
        self.sid_writes = []

    def _set_nz(self, v):
        v &= 0xFF
        self.z = 1 if v == 0 else 0
        self.n = 1 if v & 0x80 else 0

    def _w(self, a, v):
        a &= 0xFFFF; v &= 0xFF
        if 0xD400 <= a <= 0xD41F:
            self.sid_writes.append((a, v))
        self.m[a] = v

    def run(self, addr, max_steps=2_000_000):
        """Run subroutine via RTS-to-sentinel, mirroring player::call()."""
        SENT = 0xFFFF
        ret = SENT - 1
        sp0 = self.sp
        self.m[0x0100 + self.sp] = (ret >> 8) & 0xFF
        self.m[0x0100 + ((self.sp - 1) & 0xFF)] = ret & 0xFF
        self.sp = (self.sp - 2) & 0xFF
        self.pc = addr
        for _ in range(max_steps):
            if self.sp == sp0:
                return True
            self.step()
        return False

    def step(self):
        m, pc = self.m, self.pc
        op = m[pc]
        imm = m[(pc + 1) & 0xFFFF]
        ab = m[(pc + 1) & 0xFFFF] | (m[(pc + 2) & 0xFFFF] << 8)
        # default advance; overridden by branches/jumps/rts
        if   op == 0xA9: self.a = imm; self._set_nz(self.a); self.pc += 2
        elif op == 0xA2: self.x = imm; self._set_nz(self.x); self.pc += 2
        elif op == 0xA0: self.y = imm; self._set_nz(self.y); self.pc += 2
        elif op == 0xA5: self.a = m[imm]; self._set_nz(self.a); self.pc += 2
        elif op == 0xAD: self.a = m[ab]; self._set_nz(self.a); self.pc += 3
        elif op == 0xBD: self.a = m[(ab + self.x) & 0xFFFF]; self._set_nz(self.a); self.pc += 3
        elif op == 0xB9: self.a = m[(ab + self.y) & 0xFFFF]; self._set_nz(self.a); self.pc += 3
        elif op == 0x85: self._w(imm, self.a); self.pc += 2
        elif op == 0x8D: self._w(ab, self.a); self.pc += 3
        elif op == 0x9D: self._w((ab + self.x) & 0xFFFF, self.a); self.pc += 3
        elif op == 0x99: self._w((ab + self.y) & 0xFFFF, self.a); self.pc += 3
        elif op == 0xE6: v = (m[imm] + 1) & 0xFF; m[imm] = v; self._set_nz(v); self.pc += 2
        elif op == 0xC6: v = (m[imm] - 1) & 0xFF; m[imm] = v; self._set_nz(v); self.pc += 2
        elif op == 0x29: self.a &= imm; self._set_nz(self.a); self.pc += 2
        elif op == 0x09: self.a |= imm; self._set_nz(self.a); self.pc += 2
        elif op == 0x49: self.a ^= imm; self._set_nz(self.a); self.pc += 2
        elif op == 0x69:
            s = self.a + imm + self.c; self.c = 1 if s > 0xFF else 0
            self.a = s & 0xFF; self._set_nz(self.a); self.pc += 2
        elif op == 0x65:
            s = self.a + m[imm] + self.c; self.c = 1 if s > 0xFF else 0
            self.a = s & 0xFF; self._set_nz(self.a); self.pc += 2
        elif op == 0xE9:
            s = self.a - imm - (1 - self.c); self.c = 0 if s < 0 else 1
            self.a = s & 0xFF; self._set_nz(self.a); self.pc += 2
        elif op == 0xC9:
            r = (self.a - imm) & 0x1FF; self.c = 1 if self.a >= imm else 0
            self._set_nz(r & 0xFF); self.pc += 2
        elif op == 0xE0:
            self.c = 1 if self.x >= imm else 0; self._set_nz((self.x - imm) & 0xFF); self.pc += 2
        elif op == 0xC0:
            self.c = 1 if self.y >= imm else 0; self._set_nz((self.y - imm) & 0xFF); self.pc += 2
        elif op == 0xAA: self.x = self.a; self._set_nz(self.x); self.pc += 1
        elif op == 0xA8: self.y = self.a; self._set_nz(self.y); self.pc += 1
        elif op == 0x8A: self.a = self.x; self._set_nz(self.a); self.pc += 1
        elif op == 0x98: self.a = self.y; self._set_nz(self.a); self.pc += 1
        elif op == 0xE8: self.x = (self.x + 1) & 0xFF; self._set_nz(self.x); self.pc += 1
        elif op == 0xC8: self.y = (self.y + 1) & 0xFF; self._set_nz(self.y); self.pc += 1
        elif op == 0xCA: self.x = (self.x - 1) & 0xFF; self._set_nz(self.x); self.pc += 1
        elif op == 0x88: self.y = (self.y - 1) & 0xFF; self._set_nz(self.y); self.pc += 1
        elif op == 0x18: self.c = 0; self.pc += 1
        elif op == 0x38: self.c = 1; self.pc += 1
        elif op == 0xEA: self.pc += 1
        elif op == 0xD0: self.pc += 2 + (self._rel(imm) if not self.z else 0)
        elif op == 0xF0: self.pc += 2 + (self._rel(imm) if self.z else 0)
        elif op == 0x10: self.pc += 2 + (self._rel(imm) if not self.n else 0)
        elif op == 0x30: self.pc += 2 + (self._rel(imm) if self.n else 0)
        elif op == 0x90: self.pc += 2 + (self._rel(imm) if not self.c else 0)
        elif op == 0xB0: self.pc += 2 + (self._rel(imm) if self.c else 0)
        elif op == 0x4C: self.pc = ab
        elif op == 0x20:
            ret = (pc + 2) & 0xFFFF
            self.m[0x0100 + self.sp] = (ret >> 8) & 0xFF
            self.m[0x0100 + ((self.sp - 1) & 0xFF)] = ret & 0xFF
            self.sp = (self.sp - 2) & 0xFF; self.pc = ab
        elif op == 0x60:
            lo = self.m[0x0100 + ((self.sp + 1) & 0xFF)]
            hi = self.m[0x0100 + ((self.sp + 2) & 0xFF)]
            self.sp = (self.sp + 2) & 0xFF
            self.pc = ((hi << 8) | lo) + 1
        else:
            raise RuntimeError(f"unimplemented opcode {op:#04x} at {pc:#06x}")
        self.pc &= 0xFFFF

    @staticmethod
    def _rel(b):
        return b - 256 if b & 0x80 else b


# ---------------------------------------------------------------------------
# The tune.
# ---------------------------------------------------------------------------

LOAD = 0x1000

# Zero-page state (persists across PLAY calls).
ARP, DIR, CUT, WIDX, WFCNT, ARPCNT, PWCNT = (
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08)

# SID registers.
def V(n, r):  # voice n (0..2), register r
    return 0xD400 + n * 7 + r
FCLO, FCHI, RESF, MODEVOL = 0xD415, 0xD416, 0xD417, 0xD418

# C-minor pentatonic, two octaves (PAL freq values). See header docstring.
NOTES = [0x08B4, 0x0A59, 0x0D0A, 0x0F81, 0x1168, 0x14B2, 0x1A14, 0x1F03]
WAVES = [0x11, 0x21, 0x41, 0x81]  # tri / saw / pulse / noise (+ gate bit 0)


# Independent per-voice material for the "ensemble" style.
BASS = [0x045A, 0x052D, 0x05CF, 0x0685]                        # C2 Eb2 F2 G2 (4)
LEAD = [0x1168, 0x14B2, 0x1A14, 0x1F03, 0x22CF, 0x1F03, 0x1A14, 0x14B2]  # C4..C5..(8)
LEAD_WAVES = [0x21, 0x41, 0x11]                                # saw / pulse / tri (+gate)


def build_program_ensemble(cia_timer):
    """Three independent voices: a slow pulse bass, a faster lead melody whose
    waveform rotates, and a noise-percussion with its own rhythm. Exercises
    distinct sequences / rates / waveforms per voice (not unison)."""
    # Zero-page state.
    DIR, CUT = 0x03, 0x04
    BCNT, BIDX = 0x05, 0x06          # bass step counter / sequence index
    LCNT, LIDX = 0x07, 0x08          # lead step counter / sequence index
    LWCNT, LWIDX = 0x09, 0x0A        # lead waveform rotation counter / index
    PCNT = 0x0B                      # percussion rhythm counter
    p = []
    a = p.append

    # ---- INIT ----
    a(("init", "LDA#", 0))                       # A=0, then clear all state vars
    for z in (DIR, BCNT, BIDX, LCNT, LIDX, LWCNT, LWIDX, PCNT):
        a((None, "STAZ", z))
    a((None, "LDA#", 0x80)); a((None, "STAZ", CUT))
    # Per-voice ADSR + PW (distinct envelopes).
    a((None, "LDA#", 0x1A)); a((None, "STA", V(0, 5)))   # bass AD
    a((None, "LDA#", 0xA8)); a((None, "STA", V(0, 6)))   # bass SR
    a((None, "LDA#", 0x00)); a((None, "STA", V(0, 2)))   # bass PW lo
    a((None, "LDA#", 0x08)); a((None, "STA", V(0, 3)))   # bass PW hi (~50%)
    a((None, "LDA#", 0x08)); a((None, "STA", V(1, 5)))   # lead AD
    a((None, "LDA#", 0x96)); a((None, "STA", V(1, 6)))   # lead SR
    a((None, "LDA#", 0x00)); a((None, "STA", V(1, 2)))   # lead PW lo
    a((None, "LDA#", 0x04)); a((None, "STA", V(1, 3)))   # lead PW hi (~25%)
    a((None, "LDA#", 0x09)); a((None, "STA", V(2, 5)))   # perc AD (fast)
    a((None, "LDA#", 0x04)); a((None, "STA", V(2, 6)))   # perc SR (short)
    # Filter: resonance F, route bass+lead (perc stays bright); lowpass + vol F.
    a((None, "LDA#", 0x00)); a((None, "STA", FCLO))
    a((None, "LDA#", 0x80)); a((None, "STA", FCHI))
    a((None, "LDA#", 0xF3)); a((None, "STA", RESF))
    a((None, "LDA#", 0x1F)); a((None, "STA", MODEVOL))
    if cia_timer is not None:
        a((None, "LDA#", cia_timer & 0xFF));        a((None, "STA", 0xDC04))
        a((None, "LDA#", (cia_timer >> 8) & 0xFF)); a((None, "STA", 0xDC05))
    a((None, "RTS"))

    # ---- PLAY ----
    # Bass (voice0): advance every 8 calls, held pulse.
    a(("play", "INCZ", BCNT)); a((None, "LDAZ", BCNT)); a((None, "CMP#", 8)); a((None, "BNE", "b_keep"))
    a((None, "LDA#", 0)); a((None, "STAZ", BCNT)); a((None, "INCZ", BIDX))
    a(("b_keep", "LDAZ", BIDX)); a((None, "AND#", 3)); a((None, "TAX"))
    a((None, "LDAX", "bass_lo")); a((None, "STA", V(0, 0)))
    a((None, "LDAX", "bass_hi")); a((None, "STA", V(0, 1)))
    a((None, "LDA#", 0x41)); a((None, "STA", V(0, 4)))            # pulse + gate

    # Lead (voice1): advance every 2 calls; rotate waveform every 64.
    a((None, "INCZ", LCNT)); a((None, "LDAZ", LCNT)); a((None, "CMP#", 2)); a((None, "BNE", "l_keep"))
    a((None, "LDA#", 0)); a((None, "STAZ", LCNT)); a((None, "INCZ", LIDX))
    a(("l_keep", "LDAZ", LIDX)); a((None, "AND#", 7)); a((None, "TAX"))
    a((None, "LDAX", "lead_lo")); a((None, "STA", V(1, 0)))
    a((None, "LDAX", "lead_hi")); a((None, "STA", V(1, 1)))
    a((None, "INCZ", LWCNT)); a((None, "LDAZ", LWCNT)); a((None, "AND#", 0x3F)); a((None, "BNE", "l_wave"))
    a((None, "LDAZ", LWIDX)); a((None, "CLC")); a((None, "ADC#", 1)); a((None, "CMP#", 3)); a((None, "BNE", "l_wstore"))
    a((None, "LDA#", 0))
    a(("l_wstore", "STAZ", LWIDX))
    a(("l_wave", "LDAZ", LWIDX)); a((None, "TAX")); a((None, "LDAX", "lead_waves")); a((None, "STA", V(1, 4)))

    # Percussion (voice2): noise, retrigger gate every 4 calls (hat-ish).
    a((None, "INCZ", PCNT)); a((None, "LDAZ", PCNT)); a((None, "AND#", 3)); a((None, "BNE", "p_off"))
    a((None, "LDA#", 0x00)); a((None, "STA", V(2, 0)))
    a((None, "LDA#", 0x40)); a((None, "STA", V(2, 1)))           # noise pitch
    a((None, "LDA#", 0x81)); a((None, "STA", V(2, 4)))           # noise + gate
    a((None, "JMP", "p_done"))
    a(("p_off", "LDA#", 0x80)); a((None, "STA", V(2, 4)))        # noise, gate off (decay)
    a(("p_done", "NOP"))

    # Filter sweep (triangle on cutoff hi).
    a((None, "LDAZ", DIR)); a((None, "BNE", "e_sw_dn"))
    a((None, "INCZ", CUT)); a((None, "LDAZ", CUT)); a((None, "CMP#", 0xF0)); a((None, "BNE", "e_sw_wr"))
    a((None, "LDA#", 1)); a((None, "STAZ", DIR)); a((None, "JMP", "e_sw_wr"))
    a(("e_sw_dn", "DECZ", CUT)); a((None, "LDAZ", CUT)); a((None, "CMP#", 0x10)); a((None, "BNE", "e_sw_wr"))
    a((None, "LDA#", 0)); a((None, "STAZ", DIR))
    a(("e_sw_wr", "LDA#", 0)); a((None, "STA", FCLO))
    a((None, "LDAZ", CUT)); a((None, "STA", FCHI))
    a((None, "RTS"))

    # ---- data ----
    a(("bass_lo", ".byte", [n & 0xFF for n in BASS]))
    a(("bass_hi", ".byte", [(n >> 8) & 0xFF for n in BASS]))
    a(("lead_lo", ".byte", [n & 0xFF for n in LEAD]))
    a(("lead_hi", ".byte", [(n >> 8) & 0xFF for n in LEAD]))
    a(("lead_waves", ".byte", LEAD_WAVES))
    return p


# Easter egg: the famous 8-note sequencer loop from Pink Floyd's "On the Run"
# (Dark Side of the Moon, 1973) — the line David Gilmour clocked out of the EMS
# Synthi AKS. An E-minor run that rises across the octave with E4 as a recurring
# lower anchor, looping every 8 sixteenth notes. PAL SID freq-register values.
RUN_SEQ = [
    0x15ED, 0x1A14, 0x1D45, 0x15ED,   # E4 G4 A4 E4
    0x1D45, 0x20DB, 0x2711, 0x2BDB,   # A4 B4 D5 E5
]


def build_program_runaway(cia_timer, note_div=5):
    """Easter egg — a transcription of Pink Floyd's "On the Run" intro: the
    8-note Synthi sequencer loop as a relentless sawtooth over a heavy resonant
    filter sweep. The note advances once every `note_div` PLAY calls so the run
    sits at the original ~165 BPM sixteenths regardless of the build's PLAY rate;
    the hi-hat retriggers on every step (the sequencer trigger pulse, as on the
    record), decaying in between."""
    DIR, CUT, SIDX, NCNT = 0x03, 0x04, 0x05, 0x06
    p = []
    a = p.append

    # ---- INIT ----
    a(("init", "LDA#", 0))
    for z in (DIR, SIDX, NCNT):
        a((None, "STAZ", z))
    a((None, "LDA#", 0x20)); a((None, "STAZ", CUT))      # start low, sweeps up
    a((None, "LDA#", 0x00)); a((None, "STA", V(0, 5)))   # lead AD (instant)
    a((None, "LDA#", 0xF0)); a((None, "STA", V(0, 6)))   # lead SR (full sustain)
    a((None, "LDA#", 0x09)); a((None, "STA", V(2, 5)))   # hat AD
    a((None, "LDA#", 0x03)); a((None, "STA", V(2, 6)))   # hat SR (short)
    a((None, "LDA#", 0x00)); a((None, "STA", FCLO))
    a((None, "LDA#", 0xF1)); a((None, "STA", RESF))      # resonance F, route lead
    a((None, "LDA#", 0x1F)); a((None, "STA", MODEVOL))   # lowpass + vol F
    if cia_timer is not None:
        a((None, "LDA#", cia_timer & 0xFF));        a((None, "STA", 0xDC04))
        a((None, "LDA#", (cia_timer >> 8) & 0xFF)); a((None, "STA", 0xDC05))
    a((None, "RTS"))

    # ---- PLAY ----
    # Advance one step of the 8-note "On the Run" loop every `note_div` calls
    # (~165 BPM sixteenths). Non-tick calls just let the filter sweep run.
    a(("play", "INCZ", NCNT)); a((None, "LDAZ", NCNT)); a((None, "CMP#", note_div)); a((None, "BNE", "r_hold"))
    a((None, "LDA#", 0)); a((None, "STAZ", NCNT))
    # new-note tick: emit next step + retrigger the hi-hat (sequencer pulse).
    a((None, "LDAZ", SIDX)); a((None, "AND#", 7)); a((None, "TAX"))
    a((None, "LDAX", "run_lo")); a((None, "STA", V(0, 0)))
    a((None, "LDAX", "run_hi")); a((None, "STA", V(0, 1)))
    a((None, "LDA#", 0x21)); a((None, "STA", V(0, 4)))          # saw + gate
    a((None, "INCZ", SIDX))
    a((None, "LDA#", 0x00)); a((None, "STA", V(2, 0)))
    a((None, "LDA#", 0x30)); a((None, "STA", V(2, 1)))
    a((None, "LDA#", 0x81)); a((None, "STA", V(2, 4)))         # noise hat + gate
    a((None, "JMP", "r_sw"))
    a(("r_hold", "LDA#", 0x80)); a((None, "STA", V(2, 4)))      # hat gate off (decay)
    # Resonant cutoff sweep (every call, smooth).
    a(("r_sw", "LDAZ", DIR)); a((None, "BNE", "r_sw_dn"))
    a((None, "INCZ", CUT)); a((None, "LDAZ", CUT)); a((None, "CMP#", 0xF0)); a((None, "BNE", "r_sw_wr"))
    a((None, "LDA#", 1)); a((None, "STAZ", DIR)); a((None, "JMP", "r_sw_wr"))
    a(("r_sw_dn", "DECZ", CUT)); a((None, "LDAZ", CUT)); a((None, "CMP#", 0x10)); a((None, "BNE", "r_sw_wr"))
    a((None, "LDA#", 0)); a((None, "STAZ", DIR))
    a(("r_sw_wr", "LDAZ", CUT)); a((None, "STA", FCHI))
    a((None, "RTS"))

    # ---- data ----
    a(("run_lo", ".byte", [n & 0xFF for n in RUN_SEQ]))
    a(("run_hi", ".byte", [(n >> 8) & 0xFF for n in RUN_SEQ]))
    return p


# --- Original cracktro tune: vi-IV-I-V loop (Am - F - C - G) ---------------
# Bass roots (octave 2) and their octave-up bounce; chord-arp tones (root/3rd/
# 5th, mid octave); and a lead melody over the 4-bar loop. All original.
CRK_BASS_LO = [0x0751, 0x05CF, 0x08B4, 0x0685]               # A2 F2 C3 G2
CRK_BASS_HI = [v * 2 for v in CRK_BASS_LO]                    # octave up
CRK_ARP = [                                                   # 4 chords x 3 tones
    0x0EA2, 0x1168, 0x15ED,   # Am: A3 C4 E4
    0x173B, 0x1D45, 0x22CF,   # F : F4 A4 C5
    0x1168, 0x15ED, 0x1A14,   # C : C4 E4 G4
    0x1A14, 0x20DB, 0x2713,   # G : G4 B4 D5
]
CRK_LEAD = [
    0x2BDB, 0x2713, 0x22CF, 0x1D45,   # E5 D5 C5 A4   (over Am)
    0x1D45, 0x22CF, 0x173B, 0x1D45,   # A4 C5 F4 A4   (over F)
    0x22CF, 0x2BDB, 0x1A14, 0x2BDB,   # C5 E5 G4 E5   (over C)
    0x2713, 0x20DB, 0x1A14, 0x2713,   # D5 B4 G4 D5   (over G)
]


def build_program_cracktro(cia_timer):
    """An original demoscene-cracktro loop: pumping octave bass, the classic
    single-voice chord arpeggio, and a lead melody over a 4-bar vi-IV-I-V."""
    # Zero-page state.
    CHCNT, CHIDX = 0x10, 0x11        # chord step counter / index (0..3)
    ACNT, ATONE = 0x12, 0x13         # arp step counter / tone (0..2)
    BCNT, BHI = 0x14, 0x15           # bass step counter / octave flag
    LCNT, LIDX = 0x16, 0x17          # lead step counter / index (0..15)
    DIR, CUT = 0x18, 0x19            # filter sweep
    p = []
    a = p.append

    # ---- INIT ----
    a(("init", "LDA#", 0))
    for z in (CHCNT, CHIDX, ACNT, ATONE, BCNT, BHI, LCNT, LIDX, DIR):
        a((None, "STAZ", z))
    a((None, "LDA#", 0xA0)); a((None, "STAZ", CUT))
    # Bass (v0): pulse, plucky-ish; arp (v1): pulse, sustained; lead (v2): saw.
    a((None, "LDA#", 0x08)); a((None, "STA", V(0, 5)))   # bass AD
    a((None, "LDA#", 0xA8)); a((None, "STA", V(0, 6)))   # bass SR
    a((None, "LDA#", 0x00)); a((None, "STA", V(0, 2)))
    a((None, "LDA#", 0x08)); a((None, "STA", V(0, 3)))   # bass PW ~50%
    a((None, "LDA#", 0x00)); a((None, "STA", V(1, 5)))   # arp AD (instant)
    a((None, "LDA#", 0xC0)); a((None, "STA", V(1, 6)))   # arp SR (sustain)
    a((None, "LDA#", 0x00)); a((None, "STA", V(1, 2)))
    a((None, "LDA#", 0x04)); a((None, "STA", V(1, 3)))   # arp PW ~25%
    a((None, "LDA#", 0x0A)); a((None, "STA", V(2, 5)))   # lead AD
    a((None, "LDA#", 0xA9)); a((None, "STA", V(2, 6)))   # lead SR
    # Gentle lowpass on the bass only; full volume.
    a((None, "LDA#", 0x00)); a((None, "STA", FCLO))
    a((None, "LDA#", 0x41)); a((None, "STA", RESF))      # res 4, route v0
    a((None, "LDA#", 0x1F)); a((None, "STA", MODEVOL))
    if cia_timer is not None:
        a((None, "LDA#", cia_timer & 0xFF));        a((None, "STA", 0xDC04))
        a((None, "LDA#", (cia_timer >> 8) & 0xFF)); a((None, "STA", 0xDC05))
    a((None, "RTS"))

    # ---- PLAY ----
    # Chord changes every 32 calls (one bar).
    a(("play", "INCZ", CHCNT)); a((None, "LDAZ", CHCNT)); a((None, "CMP#", 32)); a((None, "BNE", "c_chok"))
    a((None, "LDA#", 0)); a((None, "STAZ", CHCNT))
    a((None, "INCZ", CHIDX)); a((None, "LDAZ", CHIDX)); a((None, "AND#", 3)); a((None, "STAZ", CHIDX))
    a(("c_chok", "NOP"))

    # Bass: octave bounce every 8 calls.
    a((None, "INCZ", BCNT)); a((None, "LDAZ", BCNT)); a((None, "CMP#", 8)); a((None, "BNE", "c_bok"))
    a((None, "LDA#", 0)); a((None, "STAZ", BCNT))
    a((None, "LDAZ", BHI)); a((None, "EOR#", 1)); a((None, "STAZ", BHI))
    a(("c_bok", "LDAZ", CHIDX)); a((None, "TAX"))
    a((None, "LDAZ", BHI)); a((None, "BNE", "c_bhi"))
    a((None, "LDAX", "crk_blo_lo")); a((None, "STA", V(0, 0)))
    a((None, "LDAX", "crk_blo_hi")); a((None, "STA", V(0, 1))); a((None, "JMP", "c_bwr"))
    a(("c_bhi", "LDAX", "crk_bhi_lo")); a((None, "STA", V(0, 0)))
    a((None, "LDAX", "crk_bhi_hi")); a((None, "STA", V(0, 1)))
    a(("c_bwr", "LDA#", 0x41)); a((None, "STA", V(0, 4)))         # pulse + gate

    # Arp: advance tone (mod 3) every 2 calls; note = CRK_ARP[CHIDX*3 + tone].
    a((None, "INCZ", ACNT)); a((None, "LDAZ", ACNT)); a((None, "CMP#", 2)); a((None, "BNE", "c_aok"))
    a((None, "LDA#", 0)); a((None, "STAZ", ACNT))
    a((None, "LDAZ", ATONE)); a((None, "CLC")); a((None, "ADC#", 1)); a((None, "CMP#", 3)); a((None, "BNE", "c_ast"))
    a((None, "LDA#", 0))
    a(("c_ast", "STAZ", ATONE))
    a(("c_aok", "LDAZ", CHIDX)); a((None, "CLC")); a((None, "ADCZ", CHIDX)); a((None, "CLC")); a((None, "ADCZ", CHIDX))
    a((None, "CLC")); a((None, "ADCZ", ATONE)); a((None, "TAX"))
    a((None, "LDAX", "crk_arp_lo")); a((None, "STA", V(1, 0)))
    a((None, "LDAX", "crk_arp_hi")); a((None, "STA", V(1, 1)))
    a((None, "LDA#", 0x41)); a((None, "STA", V(1, 4)))           # pulse + gate

    # Lead: melody note every 8 calls.
    a((None, "INCZ", LCNT)); a((None, "LDAZ", LCNT)); a((None, "CMP#", 8)); a((None, "BNE", "c_lok"))
    a((None, "LDA#", 0)); a((None, "STAZ", LCNT))
    a((None, "INCZ", LIDX)); a((None, "LDAZ", LIDX)); a((None, "AND#", 15)); a((None, "STAZ", LIDX))
    a(("c_lok", "LDAZ", LIDX)); a((None, "TAX"))
    a((None, "LDAX", "crk_lead_lo")); a((None, "STA", V(2, 0)))
    a((None, "LDAX", "crk_lead_hi")); a((None, "STA", V(2, 1)))
    a((None, "LDA#", 0x21)); a((None, "STA", V(2, 4)))           # saw + gate

    # Gentle filter sweep on the bass.
    a((None, "LDAZ", DIR)); a((None, "BNE", "c_sw_dn"))
    a((None, "INCZ", CUT)); a((None, "LDAZ", CUT)); a((None, "CMP#", 0xE0)); a((None, "BNE", "c_sw_wr"))
    a((None, "LDA#", 1)); a((None, "STAZ", DIR)); a((None, "JMP", "c_sw_wr"))
    a(("c_sw_dn", "DECZ", CUT)); a((None, "LDAZ", CUT)); a((None, "CMP#", 0x40)); a((None, "BNE", "c_sw_wr"))
    a((None, "LDA#", 0)); a((None, "STAZ", DIR))
    a(("c_sw_wr", "LDAZ", CUT)); a((None, "STA", FCHI))
    a((None, "RTS"))

    # ---- data ----
    a(("crk_blo_lo", ".byte", [v & 0xFF for v in CRK_BASS_LO]))
    a(("crk_blo_hi", ".byte", [(v >> 8) & 0xFF for v in CRK_BASS_LO]))
    a(("crk_bhi_lo", ".byte", [v & 0xFF for v in CRK_BASS_HI]))
    a(("crk_bhi_hi", ".byte", [(v >> 8) & 0xFF for v in CRK_BASS_HI]))
    a(("crk_arp_lo", ".byte", [v & 0xFF for v in CRK_ARP]))
    a(("crk_arp_hi", ".byte", [(v >> 8) & 0xFF for v in CRK_ARP]))
    a(("crk_lead_lo", ".byte", [v & 0xFF for v in CRK_LEAD]))
    a(("crk_lead_hi", ".byte", [(v >> 8) & 0xFF for v in CRK_LEAD]))
    return p


def build_program_burst(cia_timer, n_writes=24):
    """FIFO-overflow exposer. Every PLAY call issues `n_writes` back-to-back SID
    writes — more than the gateware transaction FIFO (depth 16, drained 1 entry
    per phi2 ≈ 1MHz). Because the emulated 6502 runs faster than real-time, the
    whole burst is dumped far quicker than the FIFO drains, so writes past the
    FIFO's capacity are silently dropped (see SIDPeripheral in src/top/sid/top.py
    and memory sid-player-sw-fifo-overflow-dropped-notes).

    The burst is laid out **padding first, the actual note last**: a run of
    harmless cutoff writes fills the FIFO, then the three voices' freq + gate are
    written. On correct hardware (or after the backpressure fix) all writes land
    and you hear a clean unison arpeggio; on the buggy path the *final* note
    writes are exactly the ones dropped → silence / stuck pitch. Larger
    `n_writes` (e.g. 160 = 10× FIFO depth) makes the loss unmistakable.
    """
    ARP, ACNT = 0x02, 0x03
    GATE_OFF = 3                        # gate-off writes: one per voice, always land
    ESSENTIAL = 9                       # 3 voices × (freq lo + freq hi + gate on)
    pad = max(0, n_writes - GATE_OFF - ESSENTIAL)
    p = []
    a = p.append

    # ---- INIT ----
    a(("init", "LDA#", 0)); a((None, "STAZ", ARP)); a((None, "STAZ", ACNT))
    for v in range(3):
        a((None, "LDA#", 0x00)); a((None, "STA", V(v, 5)))   # AD instant
        a((None, "LDA#", 0xF0)); a((None, "STA", V(v, 6)))   # SR sustain
        a((None, "LDA#", 0x00)); a((None, "STA", V(v, 2)))   # pw lo
        a((None, "LDA#", 0x08)); a((None, "STA", V(v, 3)))   # pw hi ~50%
    a((None, "LDA#", 0x00)); a((None, "STA", FCLO))
    a((None, "LDA#", 0x80)); a((None, "STA", FCHI))
    a((None, "LDA#", 0xF7)); a((None, "STA", RESF))
    a((None, "LDA#", 0x1F)); a((None, "STA", MODEVOL))
    if cia_timer is not None:
        a((None, "LDA#", cia_timer & 0xFF));        a((None, "STA", 0xDC04))
        a((None, "LDA#", (cia_timer >> 8) & 0xFF)); a((None, "STA", 0xDC05))
    a((None, "RTS"))

    # ---- PLAY ----
    a(("play", "LDAZ", ARP)); a((None, "AND#", 7)); a((None, "TAX"))  # X = arp index
    # (0) gate OFF all voices — always lands (before the overflow zone).
    # If the freq+gate writes below are dropped by FIFO overflow, voices stay
    # silent instead of holding the previous note, making the gap clearly audible.
    for v in range(3):
        a((None, "LDA#", 0x40)); a((None, "STA", V(v, 4)))   # pulse, gate off
    # (1) padding burst: `pad` writes to cutoff-hi (fills the FIFO to overflow point).
    for i in range(pad):
        a((None, "LDA#", 0x10 + (i & 0x0F))); a((None, "STA", FCHI))
    # (2) the REAL note, written LAST so overflow drops it. Unison on all voices.
    for v in range(3):
        a((None, "LDAX", "note_lo")); a((None, "STA", V(v, 0)))
        a((None, "LDAX", "note_hi")); a((None, "STA", V(v, 1)))
        a((None, "LDA#", 0x41));      a((None, "STA", V(v, 4)))   # pulse + gate ON
    # advance the arp every call: each note written exactly once, so a dropped
    # write (FIFO overflow) produces an audible gap rather than a stuck note.
    a((None, "INCZ", ACNT)); a((None, "LDAZ", ACNT)); a((None, "CMP#", 1)); a((None, "BNE", "burst_done"))
    a((None, "LDA#", 0)); a((None, "STAZ", ACNT)); a((None, "INCZ", ARP))
    a(("burst_done", "RTS"))

    # ---- data ----
    a(("note_lo", ".byte", [n & 0xFF for n in NOTES]))
    a(("note_hi", ".byte", [(n >> 8) & 0xFF for n in NOTES]))
    return p


def build_program(cia_timer, arp_div=1, pwm=False):
    """cia_timer: None for VBlank, else 16-bit CIA Timer A value to set in INIT.
    arp_div: advance the arpeggio once every N PLAY calls (>=1; keeps fast/CIA
    rates musical). pwm: also sweep each voice's pulse width (+6 SID writes/frame).
    """
    assert arp_div >= 1
    p = []
    a = p.append

    # ---- INIT ($1000) ----
    a(("init", "LDA#", 0)); a((None, "STAZ", ARP))
    a((None, "STAZ", DIR)); a((None, "STAZ", WIDX)); a((None, "STAZ", WFCNT))
    a((None, "STAZ", ARPCNT)); a((None, "STAZ", PWCNT))
    a((None, "LDA#", 0x80)); a((None, "STAZ", CUT))      # mid cutoff
    # Per-voice setup loop (Y = 0,7,14): pw=50%, AD=0 (instant), SR=$F0 (sustain).
    a((None, "LDY#", 0))
    a(("vinit", "LDA#", 0x00)); a((None, "STAY", 0xD400))   # freq lo
    a((None, "STAY", 0xD401))                                # freq hi
    a((None, "STAY", 0xD402))                                # pw lo
    a((None, "LDA#", 0x08)); a((None, "STAY", 0xD403))      # pw hi (~50%)
    a((None, "LDA#", 0x00)); a((None, "STAY", 0xD404))      # ctrl (gate off)
    a((None, "STAY", 0xD405))                                # AD = 0
    a((None, "LDA#", 0xF0)); a((None, "STAY", 0xD406))      # SR = sustain F
    a((None, "TYA")); a((None, "CLC")); a((None, "ADC#", 7)); a((None, "TAY"))
    a((None, "CPY#", 21)); a((None, "BNE", "vinit"))
    # Filter: resonance F, route all 3 voices; lowpass + master volume F.
    a((None, "LDA#", 0x00)); a((None, "STA", FCLO))
    a((None, "LDA#", 0x80)); a((None, "STA", FCHI))
    a((None, "LDA#", 0xF7)); a((None, "STA", RESF))
    a((None, "LDA#", 0x1F)); a((None, "STA", MODEVOL))
    if cia_timer is not None:
        a((None, "LDA#", cia_timer & 0xFF));        a((None, "STA", 0xDC04))
        a((None, "LDA#", (cia_timer >> 8) & 0xFF)); a((None, "STA", 0xDC05))
    a((None, "RTS"))

    # ---- PLAY ----
    # Voice freq = NOTES[(ARP + 2*voice) & 7], written every call.
    a(("play", "LDAZ", ARP)); a((None, "AND#", 7)); a((None, "TAX"))
    a((None, "LDAX", "note_lo")); a((None, "STA", V(0, 0)))
    a((None, "LDAX", "note_hi")); a((None, "STA", V(0, 1)))
    a((None, "LDAZ", ARP)); a((None, "CLC")); a((None, "ADC#", 2)); a((None, "AND#", 7)); a((None, "TAX"))
    a((None, "LDAX", "note_lo")); a((None, "STA", V(1, 0)))
    a((None, "LDAX", "note_hi")); a((None, "STA", V(1, 1)))
    a((None, "LDAZ", ARP)); a((None, "CLC")); a((None, "ADC#", 4)); a((None, "AND#", 7)); a((None, "TAX"))
    a((None, "LDAX", "note_lo")); a((None, "STA", V(2, 0)))
    a((None, "LDAX", "note_hi")); a((None, "STA", V(2, 1)))
    # Advance the arp once every `arp_div` calls.
    if arp_div == 1:
        a((None, "INCZ", ARP))
    else:
        a((None, "INCZ", ARPCNT)); a((None, "LDAZ", ARPCNT)); a((None, "CMP#", arp_div))
        a((None, "BNE", "arp_done"))
        a((None, "LDA#", 0)); a((None, "STAZ", ARPCNT)); a((None, "INCZ", ARP))
        a(("arp_done", "NOP"))

    # Optional per-voice pulse-width sweep (extra SID-write load).
    if pwm:
        a((None, "INCZ", PWCNT))
        a((None, "LDAZ", PWCNT))
        a((None, "STA", V(0, 2))); a((None, "STA", V(1, 2))); a((None, "STA", V(2, 2)))  # pw lo
        a((None, "AND#", 0x0F))
        a((None, "STA", V(0, 3))); a((None, "STA", V(1, 3))); a((None, "STA", V(2, 3)))  # pw hi

    # Advance the waveform index (mod 4) every 16 PLAY calls.
    a((None, "INCZ", WFCNT)); a((None, "LDAZ", WFCNT)); a((None, "AND#", 0x0F))
    a((None, "BNE", "wave_write"))                       # not a 16-boundary -> skip advance
    a((None, "LDAZ", WIDX)); a((None, "CLC")); a((None, "ADC#", 1)); a((None, "AND#", 3)); a((None, "STAZ", WIDX))
    # Every call: load WAVES[WIDX] and write to all 3 control regs (gate stays on).
    a(("wave_write", "LDAZ", WIDX)); a((None, "TAX")); a((None, "LDAX", "waves"))
    a((None, "STA", V(0, 4))); a((None, "STA", V(1, 4))); a((None, "STA", V(2, 4)))

    # Triangle filter-cutoff sweep.
    a((None, "LDAZ", DIR)); a((None, "BNE", "sweep_dn"))
    a((None, "INCZ", CUT)); a((None, "LDAZ", CUT)); a((None, "CMP#", 0xF0)); a((None, "BNE", "sweep_wr"))
    a((None, "LDA#", 1)); a((None, "STAZ", DIR)); a((None, "JMP", "sweep_wr"))
    a(("sweep_dn", "DECZ", CUT)); a((None, "LDAZ", CUT)); a((None, "CMP#", 0x10)); a((None, "BNE", "sweep_wr"))
    a((None, "LDA#", 0)); a((None, "STAZ", DIR))
    a(("sweep_wr", "LDA#", 0)); a((None, "STA", FCLO))
    a((None, "LDAZ", CUT)); a((None, "STA", FCHI))
    a((None, "RTS"))

    # ---- data ----
    a(("note_lo", ".byte", [n & 0xFF for n in NOTES]))
    a(("note_hi", ".byte", [(n >> 8) & 0xFF for n in NOTES]))
    a(("waves", ".byte", WAVES))
    return p


def make_sid(cia_timer, name, style="ensemble", arp_div=1, pwm=False, note_div=5,
             burst_writes=24):
    if style == "ensemble":
        program = build_program_ensemble(cia_timer)
    elif style == "runaway":
        program = build_program_runaway(cia_timer, note_div=note_div)
    elif style == "cracktro":
        program = build_program_cracktro(cia_timer)
    elif style == "burst":
        program = build_program_burst(cia_timer, n_writes=burst_writes)
    else:
        program = build_program(cia_timer, arp_div=arp_div, pwm=pwm)
    code, labels = assemble(program, LOAD)

    # --- validate by simulation: INIT then 64 PLAY frames ---
    mem = bytearray(0x10000)
    mem[LOAD:LOAD + len(code)] = code
    sim = Sim(mem)
    assert sim.run(labels["init"]), "INIT did not terminate"
    total_writes = 0
    v0_freqs = set()   # distinct voice-0 freq-lo values -> sequencing actually runs
    for _ in range(64):
        sim.sid_writes.clear()
        assert sim.run(labels["play"]), "PLAY did not terminate"
        total_writes += len(sim.sid_writes)
        assert sim.sid_writes, "PLAY made no SID writes"
        for addr, v in sim.sid_writes:
            assert 0xD400 <= addr <= 0xD418, f"write outside SID: {addr:#06x}"
            if addr == 0xD400:
                v0_freqs.add(v)
    # sanity: the sequence actually advances (not a stuck single note).
    assert len(v0_freqs) > 1, "voice 0 frequency never changed"
    writes_per_frame = total_writes // 64

    # --- PSID v2 header (0x7C bytes) ---
    flags = (0b01 << 2) | (0b11 << 4)   # PAL clock, model "both" (no mismatch)
    speed = 1 if cia_timer is not None else 0
    hdr = bytearray(0x7C)
    hdr[0x00:0x04] = b"PSID"
    struct.pack_into(">H", hdr, 0x04, 2)             # version
    struct.pack_into(">H", hdr, 0x06, 0x7C)          # dataOffset
    struct.pack_into(">H", hdr, 0x08, LOAD)          # loadAddress
    struct.pack_into(">H", hdr, 0x0A, labels["init"])
    struct.pack_into(">H", hdr, 0x0C, labels["play"])
    struct.pack_into(">H", hdr, 0x0E, 1)             # songs
    struct.pack_into(">H", hdr, 0x10, 1)             # startSong
    struct.pack_into(">I", hdr, 0x12, speed)
    title = name.encode()[:31]
    hdr[0x16:0x16 + len(title)] = title
    hdr[0x36:0x36 + 9] = b"stress-gen"[:9]
    hdr[0x56:0x56 + 4] = b"2026"
    struct.pack_into(">H", hdr, 0x76, flags)
    return bytes(hdr) + code, writes_per_frame, len(code)


PHI2_PAL = 985248  # PAL phi2 clock (Hz)


def cia_timer_for(rate_hz):
    """CIA Timer A value reproducing `rate_hz` PLAY calls/sec on PAL phi2."""
    return max(1, round(PHI2_PAL / rate_hz) - 1)


def main():
    import argparse
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser(
        description="Generate PSID stress-test tunes for sid_player_sw.")
    ap.add_argument("out_dir", nargs="?", default=here,
                    help="output directory (default: this script's dir)")
    ap.add_argument("--rates", type=int, nargs="+", default=[0, 200],
                    help="PLAY rates in Hz; 0 = 50Hz VBlank, >0 = CIA multispeed "
                         "(default: 0 200). One .sid per rate.")
    ap.add_argument("--style", choices=["ensemble", "unison", "runaway", "cracktro", "burst"],
                    default="ensemble",
                    help="ensemble = independent bass/lead/noise voices (default); "
                         "unison = 3 voices on one transposed arp; "
                         "runaway = easter egg, Pink Floyd's \"On the Run\" Synthi sequence; "
                         "cracktro = an original demoscene-style tune (bass+chord-arp+lead); "
                         "burst = FIFO-overflow exposer (see --burst-writes).")
    ap.add_argument("--burst-writes", type=int, default=24,
                    help="[burst] SID writes per PLAY call (note written last so "
                         "FIFO overflow drops it). FIFO depth is 16; 24 = exposes "
                         "the bug, 160 = 10x extreme (default 24).")
    ap.add_argument("--arp-div", type=int, default=1,
                    help="[unison] advance the arpeggio every N PLAY calls "
                         "(keeps high rates musical; default 1).")
    ap.add_argument("--pwm", action="store_true",
                    help="[unison] also sweep each voice's pulse width "
                         "(+6 SID writes/frame).")
    ap.add_argument("--prefix", default="stress",
                    help="output filename prefix (default: stress).")
    args = ap.parse_args()
    os.makedirs(args.out_dir, exist_ok=True)

    for rate in args.rates:
        if rate == 0:
            cia, label, fname = None, "50 Hz VBlank", f"{args.prefix}_vblank.sid"
        else:
            cia = cia_timer_for(rate)
            label, fname = f"~{rate} Hz CIA (timer={cia})", f"{args.prefix}_{rate}hz.sid"
        title = f"STRESS {args.style[:3].upper()} {rate or 50}HZ"
        # "On the Run" runs at 165 BPM sixteenths = 11 notes/sec; advance the
        # runaway step every Nth PLAY call to hold that tempo at any PLAY rate.
        play_rate = 50 if rate == 0 else rate
        note_div = max(1, round(play_rate / 11.0))
        data, wpf, codelen = make_sid(cia, title, style=args.style,
                                      arp_div=args.arp_div, pwm=args.pwm,
                                      note_div=note_div, burst_writes=args.burst_writes)
        with open(os.path.join(args.out_dir, fname), "wb") as f:
            f.write(data)
        print(f"{fname}: {len(data)} bytes (code {codelen}B), "
              f"{wpf} SID writes/frame, {label}, style={args.style}")


if __name__ == "__main__":
    main()
