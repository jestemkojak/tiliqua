# SYX Directory to MBSID Patch Bank Design

## Goal

Add a standard-library-only Python command-line tool that converts the
single-patch `.syx` files in one directory into a single `BANK.SYX` stream
accepted by MBSID's existing whole-bank USB importer.

The generated bank is a concatenation of normalized 1036-byte MBSID v2 Patch
Write messages. Each output message is a bank-1 Bank Write, regardless of the
source message's type or bank metadata.

## Command-Line Interface

```text
python make_patch_bank.py DIRECTORY [-o BANK.SYX] [--preserve-patch-numbers]
```

- `DIRECTORY` is scanned non-recursively for filenames ending in `.syx`,
  case-insensitively.
- Files are processed in natural, case-insensitive filename order, so
  `patch2.syx` sorts before `patch10.syx`.
- `-o`/`--output` selects the output path and defaults to `BANK.SYX` in the
  current working directory.
- By default, valid inputs are assigned sequential output slots `0..127`.
- `--preserve-patch-numbers` instead uses each source message's embedded patch
  number.

## Components and Data Flow

`discover_syx_files(directory)` lists immediate `.syx` children and sorts them
using a natural, case-insensitive key. The resolved output path is excluded
from discovery, so rebuilding a bank in its source directory never attempts to
consume the previous `BANK.SYX`.

`decode_patch_message(data)` accepts exactly one complete MBSID v2 Patch Write
message. It validates:

- the six-byte `F0 00 00 7E 4B 00` header;
- command byte `0x02`;
- type, bank, and patch-number metadata bytes in the SysEx data range
  `0x00..0x7f`;
- the expected 1036-byte total length;
- exactly 1024 data nibbles, each in `0x00..0x0f`;
- the 7-bit checksum `(-sum(data_nibbles)) & 0x7f`;
- the terminating `0xf7`.

The source type and bank bytes are accepted but ignored. The function returns
the decoded 512-byte patch body and the source patch-number byte.

`encode_bank_patch(patch, slot)` emits the same representation as the
firmware's `fw/src/usb_patch.rs::encode_syx`:

```text
F0 00 00 7E 4B 00 02 00 01 <slot>
<1024 low-nibble/high-nibble data bytes>
<checksum> F7
```

`build_bank(files, preserve_patch_numbers)` decodes files in sorted order,
assigns slots, re-encodes valid patches, and concatenates the messages. In
default mode, malformed files do not consume slots: the next valid input gets
the next sequential slot. In preserve mode, gaps are retained.

`main()` owns argument parsing, diagnostics, the final summary, and atomic
output replacement. It writes a temporary sibling of the requested output and
replaces the destination only after the complete bank has been written.

## Skipping and Failure Behavior

Malformed, unreadable, empty, or multi-message inputs are skipped with a
filename-specific warning. The decoder rejects non-nibble payload bytes even
when their low four bits could otherwise produce a patch.

Default mode includes the first 128 valid files. Further valid files are
skipped with warnings. Preserve mode keeps the first valid file for each slot;
later files targeting an occupied slot are skipped with warnings.

A missing or non-directory input, an unreadable directory, or an output write
failure is fatal and exits nonzero. If no valid patches remain, the tool exits
nonzero and does not create or replace the output. A successful invocation
prints the output path, included patch count, and skipped-file count.

## Tests

Tests use Python's standard `unittest` framework and temporary directories.
They cover:

- natural, case-insensitive discovery order;
- successful decode and normalized encode;
- rejection of bad length, header, command, payload nibble, checksum, and
  terminator;
- byte-exact encode/decode round trips against the firmware's documented
  1036-byte layout;
- sequential assignment after invalid files are skipped;
- preserved patch numbers and first-file-wins duplicate handling;
- warning and truncation behavior after 128 valid patches;
- the default and explicit output paths;
- fatal input errors and zero-valid-patch behavior;
- output summaries and warning diagnostics.

The implementation will be written test-first. No third-party Python
dependencies are introduced.
