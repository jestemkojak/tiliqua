#!/usr/bin/env bash
# run_oracle.sh — Task-2 keystone gate.
#
# Builds two x86 binaries from the SAME vendored MBSID Lead engine subset:
#   oracle      — engine wired exactly like juce/PluginProcessor.cpp (reference)
#   shim_driver — drives the flat mbsid_* ABI (fw/csrc/mbsid_shim.cpp)
# then runs every sequence x patch through both and diffs the L and R register
# traces.  Byte-identical == the shim faithfully wraps the engine.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"          # gateware/src/top/mbsid
CORE="$ROOT/mios32/apps/synthesizers/midibox_sid_v3/core"
MODSID="$ROOT/mios32/modules/sid"
NOTESTACK="$ROOT/mios32/modules/notestack"
RANDOM_DIR="$ROOT/mios32/modules/random"
SHIM="$ROOT/fw/csrc"
BUILD="$HERE/build"
mkdir -p "$BUILD"

CXX=${CXX:-g++}
CC=${CC:-gcc}

DEFS="-DMIOS32_FAMILY_EMULATION"
# hostinc exposes ONLY mios32.h (redirecting to the real shim header); the full
# mios32_shim dir is kept OFF the path so its freestanding <string.h> doesn't
# shadow host libc.  Order matters: hostinc first.
INC="-I$HERE/hostinc -I$CORE -I$CORE/components -I$MODSID -I$NOTESTACK -I$RANDOM_DIR"
# -fpermissive: after the ilp32-correct typedef fix, mios32.h's `u32` is a 32-bit
# uint32_t (== target width). MbSidSysEx::sendAck() does a `(u32)pointer` cast
# that is LOSSLESS on the 32-bit target but truncates a 64-bit pointer on this
# LP64 host — a hard g++ error. That function is the SysEx-ACK reply path: never
# driven by the oracle sequences and --gc-sections'd out of the M1 Lead firmware,
# so the truncated (and discarded) value never affects the L trace. Demote the
# host-only diagnostic so the oracle still builds; equivalence is then proven by
# the byte-identical traces below.
CXXFLAGS="-std=c++14 -O1 -fno-exceptions -fno-rtti -fpermissive $DEFS $INC -I$HERE -I$SHIM -Wall -Wno-unused"
# notestack.c calls MIOS32_MIDI_SendDebugMessage (the printf-style debug
# console); the shim header only provides the DEBUG_MSG macro, so the call is
# an implicit decl on host. Demote to warning; resolved by a no-op in
# host_stubs.cpp (a debug-console sink the engine never consumes).
CFLAGS="-O1 $DEFS $INC -Wno-implicit-function-declaration"

# --- vendored engine .cpp ---
# fw/csrc/vendor_sources.txt names the *Lead* subset (the 19 TUs the riscv
# firmware cares about), but that subset does NOT self-link: MbSid aggregates
# the Bassline/Drum/Multi SEs and MbSidAsid BY VALUE (unconditional members in
# MbSid.h), which transitively pull MbSidSeq*/MbSidWtDrum/MbSidVoiceDrum/...
# So for a working, faithful host link we compile the FULL engine (every
# core/*.cpp + components/*.cpp except app.cpp, the firmware main). The extra
# TUs are dead code on the Lead path — identical on both sides, so shim==engine
# equivalence is preserved. (Finding for Task 1: the riscv firmware link will
# need these too, or rely on --gc-sections to drop them.)
ENGINE_OBJS=()
for src in "$CORE"/*.cpp "$CORE"/components/*.cpp; do
    case "$src" in */app.cpp) continue;; esac
    obj="$BUILD/$(basename "$src").o"
    "$CXX" $CXXFLAGS -c "$src" -o "$obj"
    ENGINE_OBJS+=("$obj")
done

# --- REAL C modules the engine consumes (NOT stubbed) ---
#   notestack.c, jsw_rand.c per vendor_sources.txt; sid.c supplies the global
#   `sid_regs` array + SID_Update (referenced by MbSid/MbSidAsid).
"$CC" $CFLAGS -c "$NOTESTACK/notestack.c" -o "$BUILD/notestack.o"
"$CC" $CFLAGS -c "$RANDOM_DIR/jsw_rand.c" -o "$BUILD/jsw_rand.o"
"$CC" $CFLAGS -c "$MODSID/sid.c"          -o "$BUILD/sid.o"
C_OBJS=("$BUILD/notestack.o" "$BUILD/jsw_rand.o" "$BUILD/sid.o")

# --- host-only link stubs ---
"$CXX" $CXXFLAGS -c "$HERE/host_stubs.cpp" -o "$BUILD/host_stubs.o"

# --- the two drivers ---
"$CXX" $CXXFLAGS -c "$HERE/oracle.cpp"     -o "$BUILD/oracle.o"
"$CXX" $CXXFLAGS -c "$HERE/shim_driver.cpp" -o "$BUILD/shim_driver.o"
"$CXX" $CXXFLAGS -c "$SHIM/mbsid_shim.cpp"  -o "$BUILD/mbsid_shim.o"

# --- no-crash sweep driver (shim side only) ---
"$CXX" $CXXFLAGS -c "$HERE/sweep_driver.cpp" -o "$BUILD/sweep_driver.o"
"$CXX" $CXXFLAGS "$BUILD/sweep_driver.o" "$BUILD/mbsid_shim.o" "${ENGINE_OBJS[@]}" "${C_OBJS[@]}" "$BUILD/host_stubs.o" -o "$BUILD/sweep_driver"

"$CXX" $CXXFLAGS "$BUILD/oracle.o"     "${ENGINE_OBJS[@]}" "${C_OBJS[@]}" "$BUILD/host_stubs.o" -o "$BUILD/oracle"
"$CXX" $CXXFLAGS "$BUILD/shim_driver.o" "$BUILD/mbsid_shim.o" "${ENGINE_OBJS[@]}" "${C_OBJS[@]}" "$BUILD/host_stubs.o" -o "$BUILD/shim_driver"

# --- Lead patch fixtures (rows in sid_bank_preset_0) ---
#   0  = A001: Lead Patch
#   51 = A052: Nice Lead
#   94 = A095: Monty Lead1
PATCHES=(0 51 94)

fail=0
for seq in "$HERE"/sequences/seq_lead_*.txt; do
    seqname="$(basename "$seq" .txt)"
    for row in "${PATCHES[@]}"; do
        for mode in patch pc; do
            tmp="$BUILD/${seqname}_${mode}${row}.seq"
            { echo "0 $mode $row"; cat "$seq"; } > "$tmp"
            "$BUILD/oracle"      "$tmp" > "$BUILD/oracle.trace"
            "$BUILD/shim_driver" "$tmp" > "$BUILD/shim.trace"
            if diff -u "$BUILD/oracle.trace" "$BUILD/shim.trace"; then
                echo "OK: $seqname $mode=$row"
            else
                echo "DIFF: $seqname $mode=$row"
                fail=1
            fi
        done
    done
done

# --- non-Lead engine fixtures: "row:sequence" pairs (row = Program Change) ---
#   Multi:    15 (A016), 60 (A061), 106 (A107)
#   Bassline: 98 (A099), 99 (A100)
#   Drum:     32-35 (A033-A036)
NONLEAD=(
    "15:seq_multi" "60:seq_multi" "106:seq_multi"
    "98:seq_bassline" "99:seq_bassline"
    "32:seq_drum" "33:seq_drum" "34:seq_drum" "35:seq_drum"
)
echo "=== non-Lead engines (Multi / Bassline / Drum), multi-channel ==="
for pair in "${NONLEAD[@]}"; do
    row="${pair%%:*}"; seqname="${pair##*:}"
    seq="$HERE/sequences/${seqname}.txt"
    for mode in patch pc; do
        tmp="$BUILD/${seqname}_${row}_${mode}.seq"
        { echo "0 $mode $row"; cat "$seq"; } > "$tmp"
        "$BUILD/oracle"      "$tmp" > "$BUILD/oracle.trace"
        "$BUILD/shim_driver" "$tmp" > "$BUILD/shim.trace"
        if ! diff -u "$BUILD/oracle.trace" "$BUILD/shim.trace"; then
            echo "DIFF: $seqname $mode=$row"; fail=1; continue
        fi
        # Non-triviality guard: a green diff of two near-empty traces proves
        # nothing. Each non-Lead sequence must drive real register activity.
        lines=$(wc -l < "$BUILD/oracle.trace")
        if [ "$lines" -lt 40 ]; then
            echo "TRIVIAL: $seqname $mode=$row produced only $lines reg writes (engine barely ran)"; fail=1
        else
            echo "OK: $seqname $mode=$row ($lines reg writes)"
        fi
    done
done

# --- Multi channel-routing differential: spread (ch0-5) vs collapsed (all ch0)
#     must differ, or per-channel routing has no observable effect (the exact
#     bug this milestone exists to rule out). Uses the oracle (engine) side.
echo "=== Multi channel-routing differential ==="
spread="$BUILD/multi_spread.seq"
collapsed="$BUILD/multi_collapsed.seq"
{ echo "0 patch 15"; cat "$HERE/sequences/seq_multi.txt"; } > "$spread"
# collapse: rewrite every 'ch <n>' to 'ch 0'
{ echo "0 patch 15"; sed -E 's/^([0-9]+ )ch [0-9]+/\1ch 0/' "$HERE/sequences/seq_multi.txt"; } > "$collapsed"
"$BUILD/oracle" "$spread"    > "$BUILD/multi_spread.trace"
"$BUILD/oracle" "$collapsed" > "$BUILD/multi_collapsed.trace"
if diff -q "$BUILD/multi_spread.trace" "$BUILD/multi_collapsed.trace" >/dev/null; then
    echo "FAIL: Multi spread==collapsed — channel routing has no effect!"; fail=1
else
    echo "OK: Multi channel routing is observable (spread != collapsed)"
fi

# --- Multi WT->filter modulation (A107 Poly Trancegate): cutoff must MOVE ---
# Reference-free check. The shim-vs-engine diff is blind to this fix (both sides
# run the same helper), so assert the filter-cutoff registers are sequenced by
# the WT rather than static. Cutoff is 11-bit, split across reg 21 (filter_l =
# FC_LO, low 3 bits) and reg 22 (filter_h = FC_HI, high 8 bits); a sweep's range
# lives mostly in FC_HI, so count changes to reg 21 OR 22 to be robust to how
# MbSidFilter splits the value. Pre-fix cutoff is static (0) -> <2 change events
# -> FAIL; post-fix the WT gates it -> many events -> PASS.
#
# The stock seq_multi.txt ends at t=1200ms, which is NOT enough: MbSidClock in
# AUTO mode stays in *slave* mode (waiting for an external MIDI clock) until
# incomingClkCtr crosses 0xfff (~4095 ticks/ms), only then falling back to its
# internal BPM master clock that actually raises eventClock and advances the
# WT (this is the same ~4.1s AUTO-clock behaviour documented for the Drum
# SIGSEGV in mbsid/CLAUDE.md). So the WT never leaves wtOut=-1 within 1200ms
# and reg 21/22 are provably static regardless of the fix. We build our own
# longer trace here: drop seq_multi.txt's terminating "end" (run_sequence ticks
# once per ms up to the last event's timestamp, so "end" would cut the trace
# short) and append a harmless tail event past the ~4.1s threshold to force
# enough master-clock ticks for the WT to step at least twice. Empirically
# (see task-1 report) this measured L=59 R=59 change events, identical counts
# on L and R (patch 106 drives one filter target across both channels), so the
# original "&&" gate (both L and R must show >=2 events) is satisfiable as
# written -- no need for the OR/combined-count fallback the brief allowed for.
echo "=== Multi WT->filter modulation (A107) ==="
a107="$BUILD/a107.seq"
{ echo "0 patch 106"; grep -v '^[0-9]* end$' "$HERE/sequences/seq_multi.txt"; echo "5500 cc 1 0"; } > "$a107"
"$BUILD/oracle" "$a107" > "$BUILD/a107.trace"
l_changes=$(grep -cE '^[0-9]+ L (21|22) ' "$BUILD/a107.trace" || true)
r_changes=$(grep -cE '^[0-9]+ R (21|22) ' "$BUILD/a107.trace" || true)
if [ "$l_changes" -ge 2 ] && [ "$r_changes" -ge 2 ]; then
    echo "OK: A107 filter cutoff is WT-sequenced (L=$l_changes R=$r_changes change events)"
else
    echo "FAIL: A107 filter cutoff static (L=$l_changes R=$r_changes) — WT->filter not applied"; fail=1
fi

# --- M4 SysEx RAM-Write path: a full nibblized+checksummed dump through
#     MbSidSysEx::parse must be byte-identical to the direct sysexSetPatch
#     load of the same preset (proves the engine-side SysEx receive path
#     end-to-end, zero gateware). Also shim==oracle on the syx path itself. ---
echo "=== SysEx RAM-Write equivalence (syxpc == patch) ==="
for row in 0 123; do
    for cmd in patch syxpc; do
        tmp="$BUILD/syx_${cmd}_${row}.txt"
        # seq_lead_basic.txt deliberately omits the patch-select line (see its
        # header comment); prepend "0 $cmd $row" the same way the main loop
        # above prepends "0 patch/pc $row" for the non-syx equivalence sweep.
        { echo "0 $cmd $row"; cat "$HERE/sequences/seq_lead_basic.txt"; } > "$tmp"
        "$BUILD/oracle"      "$tmp" > "$BUILD/syx_${cmd}_${row}.oracle.trace"
        "$BUILD/shim_driver" "$tmp" > "$BUILD/syx_${cmd}_${row}.shim.trace"
        if ! diff -u "$BUILD/syx_${cmd}_${row}.oracle.trace" "$BUILD/syx_${cmd}_${row}.shim.trace"; then
            echo "FAIL: syx block shim!=oracle ($cmd row=$row)"; exit 1
        fi
    done
    if ! diff -u "$BUILD/syx_patch_${row}.oracle.trace" "$BUILD/syx_syxpc_${row}.oracle.trace"; then
        echo "FAIL: RAM-Write dump != direct load (row=$row)"; exit 1
    fi
    lines=$(wc -l < "$BUILD/syx_syxpc_${row}.oracle.trace")
    if [ "$lines" -lt 10 ]; then
        echo "FAIL: syx trace trivially empty (row=$row, $lines lines)"; exit 1
    fi
    echo "OK: SysEx RAM-Write == direct load (row=$row, $lines reg writes)"
done

# Negative: a corrupted-checksum dump must change NOTHING (trace identical to
# the same sequence with the dump line removed entirely).
echo "=== SysEx bad-checksum rejection ==="
python3 - "$HERE" "$BUILD" <<'EOF'
import sys, os
here, build = sys.argv[1], sys.argv[2]
# Build a RAM-Write dump (type 0x08 — would apply LIVE if accepted, which
# makes "state unchanged" the sharpest assertion) with a WRONG checksum.
body = bytearray([0xF0,0x00,0x00,0x7E,0x4B,0x00,0x02,0x08,0x00,0x00])
body += bytes(1024)              # nibbles of an all-zero patch, sum = 0
body += bytes([0x01])            # correct would be 0x00
body += bytes([0xF7])
hexs = body.hex()
with open(os.path.join(build, "syx_bad.txt"), "w") as f:
    f.write(f"0 syx {hexs}\n")
    f.write("10 on 60 100\n900 off 60\n1000 end\n")
with open(os.path.join(build, "syx_none.txt"), "w") as f:
    f.write("10 on 60 100\n900 off 60\n1000 end\n")
EOF
"$BUILD/shim_driver" "$BUILD/syx_bad.txt"  > "$BUILD/syx_bad.trace"
"$BUILD/shim_driver" "$BUILD/syx_none.txt" > "$BUILD/syx_none.trace"
if ! diff -u "$BUILD/syx_none.trace" "$BUILD/syx_bad.trace"; then
    echo "FAIL: bad-checksum dump altered engine state"; exit 1
fi
echo "OK: bad-checksum dump rejected (state unchanged)"

echo "=== no-crash sweep (all 128 factory patches, incl. non-Lead) ==="
if timeout 30 "$BUILD/sweep_driver"; then
    echo "OK: no-crash sweep"
else
    echo "FAIL: no-crash sweep (crash or hang, exit $?)"
    fail=1
fi

exit $fail
