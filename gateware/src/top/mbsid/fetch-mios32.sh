#!/usr/bin/env bash
# fetch-mios32.sh — vendor the MBSID v3 C++ engine the firmware links against.
#
# The mios32/ tree is GPL and is deliberately gitignored (kept OUT of this
# CERN-OHL-S repo). A fresh clone therefore has no mios32/, and `pdm mbsid build`
# fails in fw/build.rs ("expected the full engine tree, only found N TUs").
# Run this once after cloning to populate ./mios32 at the pinned commit.
#
# Idempotent: if ./mios32 already exists, it does nothing.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
DEST="$HERE/mios32"
REPO="https://github.com/midibox/mios32.git"
PIN="44d8e6af401e41a8adf2319ce6a584cce154a14f"

if [ -d "$DEST" ]; then
    echo "mios32/ already present at $DEST — nothing to do."
    exit 0
fi

echo "Cloning mios32 @ $PIN into $DEST ..."
git clone --filter=blob:none "$REPO" "$DEST"
git -C "$DEST" checkout --detach "$PIN"
echo "Done. Pinned commit:"
git -C "$DEST" rev-parse HEAD
