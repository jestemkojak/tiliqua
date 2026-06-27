#!/usr/bin/env bash
# run_oracle.sh — Task-2 keystone gate.
#
# Builds two x86 binaries from the SAME vendored MBSID Lead engine subset:
#   oracle      — engine wired exactly like juce/PluginProcessor.cpp (reference)
#   shim_driver — drives the flat mbsid_* ABI (fw/csrc/mbsid_shim.cpp)
# then runs every sequence x patch through both and diffs the L-register
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

"$CXX" $CXXFLAGS "$BUILD/oracle.o"     "${ENGINE_OBJS[@]}" "${C_OBJS[@]}" "$BUILD/host_stubs.o" -o "$BUILD/oracle"
"$CXX" $CXXFLAGS "$BUILD/shim_driver.o" "$BUILD/mbsid_shim.o" "${ENGINE_OBJS[@]}" "${C_OBJS[@]}" "$BUILD/host_stubs.o" -o "$BUILD/shim_driver"

# --- Lead patch fixtures (rows in sid_bank_preset_0) ---
#   0  = A001: Lead Patch
#   51 = A052: Nice Lead
#   94 = A095: Monty Lead1
PATCHES=(0 51 94)

fail=0
for seq in "$HERE"/sequences/*.txt; do
    seqname="$(basename "$seq" .txt)"
    for row in "${PATCHES[@]}"; do
        tmp="$BUILD/${seqname}_p${row}.seq"
        { echo "0 patch $row"; cat "$seq"; } > "$tmp"
        "$BUILD/oracle"      "$tmp" > "$BUILD/oracle_L.trace"
        "$BUILD/shim_driver" "$tmp" > "$BUILD/shim_L.trace"
        if diff -u "$BUILD/oracle_L.trace" "$BUILD/shim_L.trace"; then
            echo "OK: $seqname patch=$row"
        else
            echo "DIFF: $seqname patch=$row"
            fail=1
        fi
    done
done

exit $fail
