# SYX Directory to MBSID Patch Bank Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Python CLI that validates and normalizes a directory of single-patch MBSID `.syx` files into one `BANK.SYX` accepted by the firmware's whole-bank importer.

**Architecture:** A single importable script separates filename discovery, SysEx decoding/encoding, bank assembly, and CLI/output handling. Tests exercise those public functions directly with `unittest`; the CLI writes through a temporary sibling and atomically replaces its destination.

**Tech Stack:** Python 3 standard library (`argparse`, `dataclasses`, `pathlib`, `re`, `tempfile`, `os`, `unittest`)

## Global Constraints

- Scan only immediate children of the input directory whose suffix is `.syx`, case-insensitively.
- Sort naturally and case-insensitively.
- Default output is `BANK.SYX` in the current working directory.
- Default slot assignment is sequential `0..127`; `--preserve-patch-numbers` uses embedded source slots.
- Skip malformed/unreadable files with warnings.
- Keep the first file and skip later duplicates in preserve mode.
- Include at most 128 valid patches and warn for further valid inputs.
- Emit normalized 1036-byte bank-1 Bank Write messages byte-compatible with `fw/src/usb_patch.rs::encode_syx`.
- Do not replace/create an output when no valid patches remain.
- Use only the Python standard library.

---

## File Structure

- Create `make_patch_bank.py`: importable codec, discovery, bank builder, atomic writer, and CLI.
- Create `tests/test_make_patch_bank.py`: standard-library unit and CLI tests.
- Modify `docs/user-guide.md`: document how to generate and import `BANK.SYX`.

### Task 1: MBSID SysEx Codec and Natural Discovery

**Files:**
- Create: `make_patch_bank.py`
- Create: `tests/test_make_patch_bank.py`

**Interfaces:**
- Produces: `PatchFormatError(ValueError)`.
- Produces: `natural_sort_key(path: Path) -> tuple[object, ...]`.
- Produces: `discover_syx_files(directory: Path, output: Path | None = None) -> list[Path]`.
- Produces: `decode_patch_message(data: bytes) -> tuple[bytes, int]`.
- Produces: `encode_bank_patch(patch: bytes, slot: int) -> bytes`.

- [ ] **Step 1: Write failing codec and discovery tests**

Create `tests/test_make_patch_bank.py` with imports and helpers:

```python
import tempfile
import unittest
from pathlib import Path

import make_patch_bank as bank


def patch_bytes(seed: int) -> bytes:
    return bytes((index * 31 + seed) & 0xFF for index in range(512))


class CodecTests(unittest.TestCase):
    def test_encode_matches_firmware_layout_and_round_trips(self):
        patch = patch_bytes(9)
        encoded = bank.encode_bank_patch(patch, 5)

        self.assertEqual(len(encoded), 1036)
        self.assertEqual(encoded[:10], bytes.fromhex("f0 00 00 7e 4b 00 02 00 01 05"))
        self.assertEqual(encoded[-1], 0xF7)
        self.assertEqual(encoded[-2], (-sum(encoded[10:1034])) & 0x7F)
        self.assertEqual(bank.decode_patch_message(encoded), (patch, 5))

    def test_decode_accepts_source_metadata_but_returns_embedded_slot(self):
        encoded = bytearray(bank.encode_bank_patch(patch_bytes(4), 73))
        encoded[7] = 0x7F
        encoded[8] = 0x00
        self.assertEqual(bank.decode_patch_message(bytes(encoded))[1], 73)

    def test_decode_rejects_each_malformed_field(self):
        valid = bytearray(bank.encode_bank_patch(patch_bytes(1), 0))
        mutations = {
            "length": bytes(valid[:-1]),
            "header": bytes([0xF0, 1]) + bytes(valid[2:]),
            "command": bytes(valid[:6]) + bytes([3]) + bytes(valid[7:]),
            "metadata": bytes(valid[:7]) + bytes([0x80]) + bytes(valid[8:]),
            "nibble": bytes(valid[:10]) + bytes([0x10]) + bytes(valid[11:]),
            "checksum": bytes(valid[:-2]) + bytes([(valid[-2] + 1) & 0x7F, 0xF7]),
            "terminator": bytes(valid[:-1]) + bytes([0x00]),
        }
        for label, message in mutations.items():
            with self.subTest(label=label):
                with self.assertRaises(bank.PatchFormatError):
                    bank.decode_patch_message(message)

    def test_encode_rejects_bad_patch_length_and_slot(self):
        for patch, slot in ((bytes(511), 0), (bytes(512), -1), (bytes(512), 128)):
            with self.subTest(length=len(patch), slot=slot):
                with self.assertRaises(ValueError):
                    bank.encode_bank_patch(patch, slot)


class DiscoveryTests(unittest.TestCase):
    def test_discovers_immediate_syx_files_in_natural_casefolded_order(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for name in ("Patch10.SYX", "patch2.syx", "PATCH1.sYx", "notes.txt"):
                (root / name).write_bytes(b"x")
            (root / "nested").mkdir()
            (root / "nested" / "patch0.syx").write_bytes(b"x")

            found = bank.discover_syx_files(root)

            self.assertEqual([path.name for path in found],
                             ["PATCH1.sYx", "patch2.syx", "Patch10.SYX"])

    def test_discovery_excludes_resolved_output(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            output = root / "BANK.SYX"
            output.write_bytes(b"old bank")
            (root / "patch1.syx").write_bytes(b"patch")
            self.assertEqual(bank.discover_syx_files(root, output),
                             [root / "patch1.syx"])


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
python3 -m unittest discover -s tests -p 'test_make_patch_bank.py' -v
```

Expected: import fails with `ModuleNotFoundError: No module named 'make_patch_bank'`.

- [ ] **Step 3: Implement the minimal codec and discovery**

Create `make_patch_bank.py` with:

```python
#!/usr/bin/env python3
"""Build an MBSID BANK.SYX from a directory of single-patch SysEx files."""

from __future__ import annotations

import re
from pathlib import Path

HEADER = bytes((0xF0, 0x00, 0x00, 0x7E, 0x4B, 0x00))
MESSAGE_BYTES = 1036
PATCH_BYTES = 512
DATA_START = 10
DATA_END = 1034


class PatchFormatError(ValueError):
    """An input is not one complete MBSID v2 Patch Write message."""


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
```

- [ ] **Step 4: Run the tests and verify GREEN**

Run:

```bash
python3 -m unittest discover -s tests -p 'test_make_patch_bank.py' -v
```

Expected: all codec and discovery tests pass.

- [ ] **Step 5: Commit the codec**

```bash
git add make_patch_bank.py tests/test_make_patch_bank.py
git commit -m "feat(mbsid): add patch SysEx bank codec"
```

### Task 2: Bank Assembly and Skip Semantics

**Files:**
- Modify: `make_patch_bank.py`
- Modify: `tests/test_make_patch_bank.py`

**Interfaces:**
- Consumes: `decode_patch_message(data: bytes) -> tuple[bytes, int]`.
- Consumes: `encode_bank_patch(patch: bytes, slot: int) -> bytes`.
- Produces: `BuildResult(data: bytes, included: int, skipped: int, warnings: tuple[str, ...])`.
- Produces: `build_bank(files: list[Path], preserve_patch_numbers: bool = False) -> BuildResult`.

- [ ] **Step 1: Write failing bank-assembly tests**

Add to `tests/test_make_patch_bank.py`:

```python
class BankAssemblyTests(unittest.TestCase):
    def write_patch(self, root: Path, name: str, seed: int, slot: int) -> Path:
        path = root / name
        path.write_bytes(bank.encode_bank_patch(patch_bytes(seed), slot))
        return path

    def test_default_mode_skips_bad_file_without_consuming_slot(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            first = self.write_patch(root, "p1.syx", 1, 90)
            bad = root / "p2.syx"
            bad.write_bytes(b"bad")
            third = self.write_patch(root, "p3.syx", 3, 91)

            result = bank.build_bank([first, bad, third])

            self.assertEqual((result.included, result.skipped), (2, 1))
            self.assertIn("p2.syx", result.warnings[0])
            self.assertEqual(bank.decode_patch_message(result.data[:1036]),
                             (patch_bytes(1), 0))
            self.assertEqual(bank.decode_patch_message(result.data[1036:]),
                             (patch_bytes(3), 1))

    def test_preserve_mode_keeps_first_duplicate_and_retains_gaps(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            first = self.write_patch(root, "p1.syx", 1, 7)
            duplicate = self.write_patch(root, "p2.syx", 2, 7)
            later = self.write_patch(root, "p3.syx", 3, 42)

            result = bank.build_bank([first, duplicate, later], True)

            self.assertEqual((result.included, result.skipped), (2, 1))
            self.assertIn("duplicate slot 7", result.warnings[0])
            self.assertEqual(bank.decode_patch_message(result.data[:1036]),
                             (patch_bytes(1), 7))
            self.assertEqual(bank.decode_patch_message(result.data[1036:]),
                             (patch_bytes(3), 42))

    def test_default_mode_warns_for_each_valid_patch_after_128(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            files = [
                self.write_patch(root, f"p{index:03}.syx", index, index & 0x7F)
                for index in range(130)
            ]

            result = bank.build_bank(files)

            self.assertEqual((result.included, result.skipped), (128, 2))
            self.assertEqual(len(result.data), 128 * 1036)
            self.assertTrue(all("bank already has 128 patches" in warning
                                for warning in result.warnings))

    def test_unreadable_file_is_skipped(self):
        with tempfile.TemporaryDirectory() as raw:
            missing = Path(raw) / "missing.syx"
            result = bank.build_bank([missing])
            self.assertEqual((result.included, result.skipped), (0, 1))
            self.assertIn("cannot read", result.warnings[0])
```

- [ ] **Step 2: Run the new tests and verify RED**

Run:

```bash
python3 -m unittest discover -s tests -p 'test_make_patch_bank.py' -v
```

Expected: failures because `build_bank` and `BuildResult` do not exist.

- [ ] **Step 3: Implement bank assembly**

Add imports and definitions to `make_patch_bank.py`:

```python
from dataclasses import dataclass


@dataclass(frozen=True)
class BuildResult:
    data: bytes
    included: int
    skipped: int
    warnings: tuple[str, ...]


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
```

- [ ] **Step 4: Run all tests and verify GREEN**

Run:

```bash
python3 -m unittest discover -s tests -p 'test_make_patch_bank.py' -v
```

Expected: all tests pass.

- [ ] **Step 5: Commit assembly behavior**

```bash
git add make_patch_bank.py tests/test_make_patch_bank.py
git commit -m "feat(mbsid): assemble normalized patch banks"
```

### Task 3: CLI and Atomic Output

**Files:**
- Modify: `make_patch_bank.py`
- Modify: `tests/test_make_patch_bank.py`

**Interfaces:**
- Consumes: `discover_syx_files(directory: Path, output: Path | None) -> list[Path]`.
- Consumes: `build_bank(files: list[Path], preserve_patch_numbers: bool) -> BuildResult`.
- Produces: `atomic_write(output: Path, data: bytes) -> None`.
- Produces: `main(argv: list[str] | None = None) -> int`.

- [ ] **Step 1: Write failing CLI tests**

Add imports and tests to `tests/test_make_patch_bank.py`:

```python
import contextlib
import io
import os
from unittest import mock


class CliTests(unittest.TestCase):
    def run_main(self, argv):
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = bank.main(argv)
        return status, stdout.getvalue(), stderr.getvalue()

    def test_default_output_and_summary(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "patch1.syx"
            source.write_bytes(bank.encode_bank_patch(patch_bytes(1), 88))
            previous = Path.cwd()
            os.chdir(root)
            try:
                status, stdout, stderr = self.run_main([str(root)])
            finally:
                os.chdir(previous)

            self.assertEqual(status, 0)
            self.assertEqual(stderr, "")
            self.assertTrue((root / "BANK.SYX").is_file())
            self.assertIn("1 patch", stdout)
            self.assertIn("0 skipped", stdout)

    def test_custom_output_preserves_embedded_slots_and_warns(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "source"
            source.mkdir()
            (source / "p1.syx").write_bytes(bank.encode_bank_patch(patch_bytes(1), 12))
            (source / "p2.syx").write_bytes(bank.encode_bank_patch(patch_bytes(2), 12))
            output = root / "custom.syx"

            status, stdout, stderr = self.run_main([
                str(source), "-o", str(output), "--preserve-patch-numbers"
            ])

            self.assertEqual(status, 0)
            self.assertIn("duplicate slot 12", stderr)
            self.assertIn("1 skipped", stdout)
            self.assertEqual(bank.decode_patch_message(output.read_bytes())[1], 12)

    def test_missing_directory_is_fatal(self):
        status, _, stderr = self.run_main(["does-not-exist"])
        self.assertEqual(status, 2)
        self.assertIn("not a directory", stderr)

    def test_zero_valid_patches_does_not_replace_output(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "bad.syx").write_bytes(b"bad")
            output = root / "existing.syx"
            output.write_bytes(b"keep me")

            status, _, stderr = self.run_main([str(root), "-o", str(output)])

            self.assertEqual(status, 1)
            self.assertIn("no valid patches", stderr)
            self.assertEqual(output.read_bytes(), b"keep me")

    def test_atomic_write_failure_is_fatal_and_leaves_destination(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "patch.syx"
            source.write_bytes(bank.encode_bank_patch(patch_bytes(1), 0))
            output = root / "bank.syx"
            output.write_bytes(b"old")
            with mock.patch.object(bank.os, "replace", side_effect=OSError("replace failed")):
                status, _, stderr = self.run_main([str(root), "-o", str(output)])
            self.assertEqual(status, 1)
            self.assertIn("cannot write", stderr)
            self.assertEqual(output.read_bytes(), b"old")
            self.assertEqual(list(root.glob(".bank.syx.*")), [])
```

- [ ] **Step 2: Run the CLI tests and verify RED**

Run:

```bash
python3 -m unittest discover -s tests -p 'test_make_patch_bank.py' -v
```

Expected: failures because `main` and `atomic_write` do not exist.

- [ ] **Step 3: Implement argument parsing, diagnostics, and atomic output**

Add imports and functions to `make_patch_bank.py`:

```python
import argparse
import os
import sys
import tempfile


def atomic_write(output: Path, data: bytes) -> None:
    output = output.resolve()
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb",
            dir=output.parent,
            prefix=f".{output.name}.",
            delete=False,
        ) as stream:
            temporary = Path(stream.name)
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, output)
        temporary = None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build an MBSID BANK.SYX from single-patch .syx files."
    )
    parser.add_argument("directory", type=Path)
    parser.add_argument("-o", "--output", type=Path, default=Path("BANK.SYX"))
    parser.add_argument(
        "--preserve-patch-numbers",
        action="store_true",
        help="use each input message's embedded patch number",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if not args.directory.is_dir():
        print(f"error: not a directory: {args.directory}", file=sys.stderr)
        return 2
    try:
        files = discover_syx_files(args.directory, args.output)
    except OSError as error:
        print(f"error: cannot read directory {args.directory}: {error}", file=sys.stderr)
        return 2

    result = build_bank(files, args.preserve_patch_numbers)
    for warning in result.warnings:
        print(f"warning: {warning}", file=sys.stderr)
    if result.included == 0:
        print("error: no valid patches; output not written", file=sys.stderr)
        return 1

    try:
        atomic_write(args.output, result.data)
    except OSError as error:
        print(f"error: cannot write {args.output}: {error}", file=sys.stderr)
        return 1

    noun = "patch" if result.included == 1 else "patches"
    print(
        f"Wrote {args.output}: {result.included} {noun}, "
        f"{result.skipped} skipped"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

- [ ] **Step 4: Run all Python tests and verify GREEN**

Run:

```bash
python3 -m unittest discover -s tests -p 'test_make_patch_bank.py' -v
```

Expected: all tests pass with no warnings or errors.

- [ ] **Step 5: Run CLI help as a smoke test**

Run:

```bash
python3 make_patch_bank.py --help
```

Expected: exit 0; usage includes `DIRECTORY`, `--output`, and
`--preserve-patch-numbers`.

- [ ] **Step 6: Commit the CLI**

```bash
git add make_patch_bank.py tests/test_make_patch_bank.py
git commit -m "feat(mbsid): add patch bank converter CLI"
```

### Task 4: User Documentation and Final Compatibility Verification

**Files:**
- Modify: `docs/user-guide.md`
- Test: `tests/test_make_patch_bank.py`

**Interfaces:**
- Consumes: `python3 make_patch_bank.py DIRECTORY [-o BANK.SYX] [--preserve-patch-numbers]`.
- Produces: user instructions that place the result at `/MBSID/BANK.SYX`.

- [ ] **Step 1: Add converter usage to the bank-import guide**

Insert after the introduction of `### Importing a whole bank from a drive` in
`docs/user-guide.md`:

````markdown
To build a bank from a directory of individual patch dumps, run:

```console
python3 make_patch_bank.py path/to/patches
```

Files are naturally sorted by name (`patch2.syx` before `patch10.syx`) and
assigned to slots from 0 upward. Invalid files and valid files beyond the
128-slot limit are skipped with warnings. To retain the slot number embedded
in each dump, add `--preserve-patch-numbers`; when two files name the same
slot, the first file wins. Use `-o PATH` to choose a name other than the
default `BANK.SYX`.
````

- [ ] **Step 2: Run the focused Python suite**

Run:

```bash
python3 -m unittest discover -s tests -p 'test_make_patch_bank.py' -v
```

Expected: all tests pass.

- [ ] **Step 3: Verify source formatting and repository diff**

Run:

```bash
python3 -m py_compile make_patch_bank.py tests/test_make_patch_bank.py
git diff --check
git status --short
```

Expected: compilation succeeds, `git diff --check` produces no output, and
status contains only the intended script, test, guide, and plan changes.

- [ ] **Step 4: Commit documentation**

```bash
git add docs/user-guide.md
git commit -m "docs(mbsid): document patch bank converter"
```

- [ ] **Step 5: Run final verification**

Run:

```bash
python3 -m unittest discover -s tests -p 'test_*.py' -v
python3 make_patch_bank.py --help
git status --short
```

Expected: the full Python suite passes, CLI help exits 0, and no uncommitted
changes remain from this feature.
