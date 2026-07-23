# MBSID Patch Bank Tool Relocation Design

## Goal

Move the patch-bank converter and its tests out of the MBSID source root into
a self-contained `tools/make_patch_bank/` directory without changing its
behavior.

## Target Layout

```text
tools/
  __init__.py
  make_patch_bank/
    __init__.py
    make_patch_bank.py
    tests/
      test_make_patch_bank.py
```

The package markers give tests a stable import:

```python
from tools.make_patch_bank import make_patch_bank as bank
```

The converter remains directly executable:

```console
python3 tools/make_patch_bank/make_patch_bank.py DIRECTORY
```

Its existing options and defaults remain unchanged.

## Changes

- Move `make_patch_bank.py` to
  `tools/make_patch_bank/make_patch_bank.py`.
- Move `tests/test_make_patch_bank.py` to
  `tools/make_patch_bank/tests/test_make_patch_bank.py`.
- Add empty package markers at `tools/__init__.py` and
  `tools/make_patch_bank/__init__.py`.
- Update the test import to the package-qualified path.
- Update `docs/user-guide.md` to show the new CLI path and working directory.
- Leave the original design and implementation-plan documents unchanged as
  historical records of how the tool was first built.

## Verification

Run the relocated suite from the MBSID directory:

```console
python3 -m unittest discover -s tools/make_patch_bank/tests -p 'test_*.py' -v
```

Also compile the relocated Python files, run the relocated CLI's `--help`,
check that the old root script/test paths no longer exist, and run
`git diff --check`.
