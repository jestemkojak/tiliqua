#!/usr/bin/env python3
"""Build an MBSID BANK.SYX from a directory of single-patch SysEx files."""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

HEADER = bytes((0xF0, 0x00, 0x00, 0x7E, 0x4B, 0x00))
MESSAGE_BYTES = 1036
PATCH_BYTES = 512
DATA_START = 10
DATA_END = 1034


class PatchFormatError(ValueError):
    """An input is not one complete MBSID v2 Patch Write message."""


@dataclass(frozen=True)
class BuildResult:
    data: bytes
    included: int
    skipped: int
    warnings: tuple[str, ...]


def natural_sort_key(path: Path) -> tuple[object, ...]:
    return tuple(
        int(part) if part.isdigit() else part.casefold()
        for part in re.split(r"(\d+)", path.name)
    )


def discover_syx_files(directory: Path, output: Path | None = None) -> list[Path]:
    excluded = output.resolve() if output is not None else None
    files = [
        path
        for path in directory.iterdir()
        if path.is_file()
        and path.suffix.casefold() == ".syx"
        and (excluded is None or path.resolve() != excluded)
    ]
    return sorted(files, key=natural_sort_key)


def decode_patch_message(data: bytes) -> tuple[bytes, int]:
    if len(data) != MESSAGE_BYTES:
        raise PatchFormatError(f"expected {MESSAGE_BYTES} bytes, got {len(data)}")
    if data[:6] != HEADER:
        raise PatchFormatError("bad MBSID SysEx header")
    if data[6] != 0x02:
        raise PatchFormatError(f"unsupported command 0x{data[6]:02x}")
    if any(value >= 0x80 for value in data[7:10]):
        raise PatchFormatError("metadata byte outside SysEx data range")
    nibbles = data[DATA_START:DATA_END]
    if any(value > 0x0F for value in nibbles):
        raise PatchFormatError("patch payload contains a non-nibble byte")
    expected = (-sum(nibbles)) & 0x7F
    if data[DATA_END] != expected:
        raise PatchFormatError("bad patch checksum")
    if data[-1] != 0xF7:
        raise PatchFormatError("missing SysEx terminator")
    patch = bytes(low | (high << 4) for low, high in zip(nibbles[::2], nibbles[1::2]))
    return patch, data[9]


def encode_bank_patch(patch: bytes, slot: int) -> bytes:
    if len(patch) != PATCH_BYTES:
        raise ValueError(f"patch must be exactly {PATCH_BYTES} bytes")
    if not 0 <= slot < 128:
        raise ValueError("slot must be in 0..127")
    nibbles = bytearray()
    for value in patch:
        nibbles.extend((value & 0x0F, value >> 4))
    checksum = (-sum(nibbles)) & 0x7F
    return HEADER + bytes((0x02, 0x00, 0x01, slot)) + bytes(nibbles) + bytes((checksum, 0xF7))


def build_bank(
    files: list[Path], preserve_patch_numbers: bool = False
) -> BuildResult:
    messages: list[bytes] = []
    warnings: list[str] = []
    occupied: set[int] = set()

    for path in files:
        try:
            patch, embedded_slot = decode_patch_message(path.read_bytes())
        except OSError as error:
            warnings.append(f"{path}: cannot read: {error}")
            continue
        except PatchFormatError as error:
            warnings.append(f"{path}: invalid patch: {error}")
            continue

        if preserve_patch_numbers:
            slot = embedded_slot
            if slot in occupied:
                warnings.append(f"{path}: duplicate slot {slot}; keeping first")
                continue
        else:
            if len(messages) == 128:
                warnings.append(f"{path}: bank already has 128 patches; skipping")
                continue
            slot = len(messages)

        occupied.add(slot)
        messages.append(encode_bank_patch(patch, slot))

    return BuildResult(
        data=b"".join(messages),
        included=len(messages),
        skipped=len(warnings),
        warnings=tuple(warnings),
    )
