#!/usr/bin/env bash
# Render a SID write-stream dump (.sidw) through the verilated reDIP-SID RTL into
# a 48 kHz WAV, reproducing the Tiliqua signal path (1 MHz phi2; point-sampled
# voice taps / decimated-ish mix).
#
# Usage: render.sh [-i dump.sidw] [-m 6581|8580] [-t mix|v0|v1|v2] [-o out.wav]
#
# Stage A builds a verilated, patched sim binary (cached, per SID model — the
# model is a COMPILE-TIME `define SID2`, not a runtime flag). Stage B runs it on
# the dump and converts the raw output to WAV via raw2wav.py.
set -euo pipefail

# ---- locations ----
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# tools/host_render -> .../sid_player_sw -> .../top -> .../src -> gateware
GATEWARE_DIR="$(cd "$SCRIPT_DIR/../../../../.." && pwd)"
DEPS_DIR="$GATEWARE_DIR/deps/sid/gateware"
BUILD_ROOT="$GATEWARE_DIR/build/host_render"
VENV_PY="$GATEWARE_DIR/.venv/bin/python"
VERILATOR="${VERILATOR:-verilator}"
PATCH="$SCRIPT_DIR/harness.patch"
RAW2WAV="$SCRIPT_DIR/raw2wav.py"

# ---- defaults ----
INPUT="/tmp/sid_writes.sidw"
MODEL="6581"
TAP="mix"
OUT=""
PHI2_HZ="1000000"
SAMPLE_RATE="48000"

usage() { echo "Usage: render.sh [-i dump.sidw] [-m 6581|8580] [-t mix|v0|v1|v2] [-o out.wav]"; exit 1; }

while getopts "i:m:t:o:h" opt; do
    case "$opt" in
        i) INPUT="$OPTARG" ;;
        m) MODEL="$OPTARG" ;;
        t) TAP="$OPTARG" ;;
        o) OUT="$OPTARG" ;;
        *) usage ;;
    esac
done

case "$MODEL" in 6581|8580) ;; *) echo "bad -m: $MODEL"; usage ;; esac
case "$TAP" in mix|v0|v1|v2) ;; *) echo "bad -t: $TAP"; usage ;; esac
[ -f "$INPUT" ] || { echo "input not found: $INPUT" >&2; exit 1; }
command -v "$VERILATOR" >/dev/null 2>&1 || { echo "verilator not found: $VERILATOR (set \$VERILATOR to override)" >&2; exit 1; }

if [ -z "$OUT" ]; then
    base="$(basename "$INPUT" .sidw)"
    OUT="$GATEWARE_DIR/build/host_render/${base}-host-${MODEL}-${TAP}.wav"
fi

# Output format: mix = s24be (upstream), taps = s16le (see harness.patch).
if [ "$TAP" = "mix" ]; then RAWFMT="s24be"; else RAWFMT="s16le"; fi

# ----------------------------------------------------------------------------
# Stage A: build a patched verilated sim binary (cached per model).
# The reDIP-SID model is selected at COMPILE time via `define SID2` (8580).
# ----------------------------------------------------------------------------
SID2_FLAG=""
[ "$MODEL" = "8580" ] && SID2_FLAG="-DSID2"

BUILD_DIR="$BUILD_ROOT/sim_${MODEL}"
BIN="$BUILD_DIR/Vsid_api"

if [ ! -x "$BIN" ] || [ "$PATCH" -nt "$BIN" ] || [ "$DEPS_DIR/sid_api_sim.cpp" -nt "$BIN" ]; then
    echo "[stage A] building verilated sim for ${MODEL} ..."
    mkdir -p "$BUILD_DIR"
    # Patched harness lives in our tree only; deps/ is never modified.
    cp "$DEPS_DIR/sid_api_sim.cpp" "$BUILD_DIR/sid_api_sim.cpp"
    # Prefer patch(1) (fuzz-tolerant), fall back to git apply (always present
    # alongside this repo) — Fedora minimal installs lack patch.
    if command -v patch >/dev/null 2>&1; then
        patch -p1 -d "$BUILD_DIR" < "$PATCH" || { echo "patch failed — harness.patch may be stale relative to deps/" >&2; exit 1; }
    else
        ( cd "$BUILD_DIR" && git apply -p1 "$PATCH" ) || { echo "git apply failed — harness.patch may be stale relative to deps/" >&2; exit 1; }
    fi
    # Symlink the $readmemh .hex tables (referenced by relative path at runtime)
    # and cells_sim into the build dir so the binary can run from here.
    for hx in "$DEPS_DIR"/*.hex; do ln -sf "$hx" "$BUILD_DIR/"; done
    ln -sfn "$DEPS_DIR/cells_sim" "$BUILD_DIR/cells_sim"
    # Mirror the upstream `sim_audio` verilator flags, adding -DSID2 for 8580.
    # -y "$DEPS_DIR": upstream runs verilator from the deps dir, so module-name
    # auto-lookup (sid_filter.sv, sid_pot.sv, ...) resolves via the cwd default
    # library path. We build out-of-tree, so add the deps dir as a -y libdir.
    ( cd "$BUILD_DIR" && "$VERILATOR" --Mdir obj_dir --clk clk --cc -O3 \
        -CFLAGS "-Wall" --x-assign fast --x-initial fast --noassert --exe --build \
        -Wno-fatal -Icells_sim -y "$DEPS_DIR" $SID2_FLAG \
        "$DEPS_DIR/sid_pkg.sv" "$DEPS_DIR/sid_api.sv" \
        --top sid_api "$BUILD_DIR/sid_api_sim.cpp" )
    cp "$BUILD_DIR/obj_dir/Vsid_api" "$BIN"
    echo "[stage A] built $BIN"
else
    echo "[stage A] reusing cached $BIN"
fi

# ----------------------------------------------------------------------------
# Stage B: render the dump -> raw -> WAV.
# Run from BUILD_DIR so the relative .hex paths resolve.
# ----------------------------------------------------------------------------
RAW="$BUILD_DIR/render_${TAP}.raw"
echo "[stage B] rendering tap=$TAP model=$MODEL phi2=$PHI2_HZ rate=$SAMPLE_RATE ..."
( cd "$BUILD_DIR" && "$BIN" \
    --sample-rate "$SAMPLE_RATE" --phi2-hz "$PHI2_HZ" --tap "$TAP" < "$INPUT" )
mv "$BUILD_DIR/sid_api_audio.raw" "$RAW"

mkdir -p "$(dirname "$OUT")"
"$VENV_PY" "$RAW2WAV" "$RAW" "$OUT" --format "$RAWFMT" --rate "$SAMPLE_RATE"
echo "[done] $OUT"
