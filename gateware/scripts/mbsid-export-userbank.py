#!/usr/bin/env python3
"""Export MBSID user-bank patches from a raw flash dump to .syx files.

The M4 user bank lives at flash 0xF00000..0xF80000: 128 x 4KiB slots, each
8-byte header (MBUP | ver | 0 | checksum u16 LE) + 512-byte sid_patch_t
(see gateware/src/top/mbsid/fw/src/patch_store.rs). Input is a raw dump of
that region (or of the whole flash; pass --base 0xF00000 then).

Usage:
  mbsid-export-userbank.py dump.bin -o outdir/ [--base 0]
  mbsid-export-userbank.py --self-test
"""
import argparse, pathlib, sys

SLOT_SIZE, N_SLOTS, HEADER_LEN = 4096, 128, 8
MAGIC, VERSION = b"MBUP", 1
SYX_HEADER = bytes([0xF0, 0x00, 0x00, 0x7E, 0x4B, 0x00])
CMD_PATCH_WRITE, TYPE_BANK_WRITE_SID0, USER_BANK = 0x02, 0x00, 0x01


def checksum16(payload: bytes) -> int:
    return sum(payload) & 0xFFFF


def encode_syx(patch: bytes, slot: int) -> bytes:
    assert len(patch) == 512
    out = bytearray(SYX_HEADER)
    out += bytes([CMD_PATCH_WRITE, TYPE_BANK_WRITE_SID0, USER_BANK, slot & 0x7F])
    s = 0
    for b in patch:
        lo, hi = b & 0x0F, (b >> 4) & 0x0F
        out += bytes([lo, hi])
        s += lo + hi
    out += bytes([(-s) & 0x7F, 0xF7])
    assert len(out) == 1036
    return bytes(out)


def export(dump: bytes, base: int, outdir: pathlib.Path) -> int:
    n = 0
    for slot in range(N_SLOTS):
        off = base + slot * SLOT_SIZE
        blk = dump[off:off + HEADER_LEN + 512]
        if len(blk) < HEADER_LEN + 512:
            break
        hdr, payload = blk[:HEADER_LEN], blk[HEADER_LEN:]
        if hdr[0:4] != MAGIC or hdr[4] != VERSION:
            continue  # empty/torn slot
        if int.from_bytes(hdr[6:8], "little") != checksum16(payload):
            continue  # corrupt payload
        name = payload[0:16].rstrip(b"\x00 ").decode("ascii", "replace") or "?"
        (outdir / f"P{slot:03d}.SYX").write_bytes(encode_syx(payload, slot))
        print(f"P{slot:03d}.SYX  {name}")
        n += 1
    return n


def self_test() -> None:
    patch = bytes((i * 37) & 0xFF for i in range(512))
    syx = encode_syx(patch, 42)
    # Decode it back the way sysex_capture.rs does.
    assert syx[:6] == SYX_HEADER and syx[6:10] == bytes([0x02, 0x00, 0x01, 42])
    nib, s = syx[10:10 + 1024], 0
    dec = bytearray(512)
    for i in range(512):
        lo, hi = nib[2 * i], nib[2 * i + 1]
        dec[i] = lo | (hi << 4)
        s += lo + hi
    assert bytes(dec) == patch and syx[1034] == ((-s) & 0x7F) and syx[1035] == 0xF7
    # Round-trip through a fake flash dump.
    slot = bytearray(b"\xFF" * SLOT_SIZE * N_SLOTS)
    hdr = MAGIC + bytes([VERSION, 0]) + checksum16(patch).to_bytes(2, "little")
    slot[3 * SLOT_SIZE:3 * SLOT_SIZE + 8] = hdr
    slot[3 * SLOT_SIZE + 8:3 * SLOT_SIZE + 8 + 512] = patch
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        assert export(bytes(slot), 0, pathlib.Path(d)) == 1
        assert (pathlib.Path(d) / "P003.SYX").read_bytes() == encode_syx(patch, 3)
    print("self-test OK")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("dump", nargs="?", type=pathlib.Path)
    ap.add_argument("-o", "--outdir", type=pathlib.Path, default=pathlib.Path("."))
    ap.add_argument("--base", type=lambda s: int(s, 0), default=0)
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        self_test(); sys.exit(0)
    if a.dump is None:
        ap.error("dump file required (or --self-test)")
    a.outdir.mkdir(parents=True, exist_ok=True)
    n = export(a.dump.read_bytes(), a.base, a.outdir)
    print(f"{n} patches exported")
