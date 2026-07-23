# MBSID Patch Bank Tool Relocation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the existing patch-bank converter and its tests into a self-contained `tools/make_patch_bank/` directory without changing behavior.

**Architecture:** Preserve the converter as one directly executable/importable module. Add package markers so its relocated tests can import it through `tools.make_patch_bank`, and update only the live user guide to the new invocation path.

**Tech Stack:** Python 3 standard library, `unittest`, Markdown

## Global Constraints

- Final source path: `tools/make_patch_bank/make_patch_bank.py`.
- Final test path: `tools/make_patch_bank/tests/test_make_patch_bank.py`.
- Tests import `from tools.make_patch_bank import make_patch_bank as bank`.
- CLI invocation is `python3 tools/make_patch_bank/make_patch_bank.py DIRECTORY`.
- Existing CLI behavior, options, defaults, and binary output remain unchanged.
- Keep the original design and implementation-plan documents unchanged as historical records.

---

### Task 1: Relocate the Converter, Tests, and User Instructions

**Files:**
- Create: `tools/__init__.py`
- Create: `tools/make_patch_bank/__init__.py`
- Move: `make_patch_bank.py` to `tools/make_patch_bank/make_patch_bank.py`
- Move: `tests/test_make_patch_bank.py` to `tools/make_patch_bank/tests/test_make_patch_bank.py`
- Modify: `docs/user-guide.md`

**Interfaces:**
- Preserves: every public function and class currently exported by `make_patch_bank.py`.
- Produces: package import `from tools.make_patch_bank import make_patch_bank as bank`.
- Produces: CLI path `tools/make_patch_bank/make_patch_bank.py`.

- [ ] **Step 1: Move the test first and update its import**

Move the test to `tools/make_patch_bank/tests/test_make_patch_bank.py` and
replace:

```python
import make_patch_bank as bank
```

with:

```python
from tools.make_patch_bank import make_patch_bank as bank
```

Add empty `tools/__init__.py` and `tools/make_patch_bank/__init__.py`.

- [ ] **Step 2: Run the relocated suite and verify RED**

Run:

```bash
python3 -m unittest discover -s tools/make_patch_bank/tests -p 'test_*.py' -v
```

Expected: import failure because
`tools.make_patch_bank.make_patch_bank` does not exist yet.

- [ ] **Step 3: Move the implementation**

Move `make_patch_bank.py` unchanged to
`tools/make_patch_bank/make_patch_bank.py`.

- [ ] **Step 4: Update live user documentation**

In `docs/user-guide.md`, replace the converter command with:

```console
python3 tools/make_patch_bank/make_patch_bank.py path/to/patches
```

State that this command is run from `gateware/src/top/mbsid`, preserving the
existing explanation that default `BANK.SYX` is written in the current
working directory.

- [ ] **Step 5: Run the relocated suite and verify GREEN**

Run:

```bash
python3 -m unittest discover -s tools/make_patch_bank/tests -p 'test_*.py' -v
```

Expected: all 15 tests pass.

- [ ] **Step 6: Verify the complete relocation**

Run:

```bash
python3 -m py_compile \
  tools/make_patch_bank/make_patch_bank.py \
  tools/make_patch_bank/tests/test_make_patch_bank.py
python3 tools/make_patch_bank/make_patch_bank.py --help
test ! -e make_patch_bank.py
test ! -e tests/test_make_patch_bank.py
git diff --check
```

Expected: compilation and CLI help succeed, both old paths are absent, and
the diff check emits no errors.

- [ ] **Step 7: Commit**

```bash
git add \
  tools/__init__.py \
  tools/make_patch_bank/__init__.py \
  tools/make_patch_bank/make_patch_bank.py \
  tools/make_patch_bank/tests/test_make_patch_bank.py \
  make_patch_bank.py \
  tests/test_make_patch_bank.py \
  docs/user-guide.md
git commit -m "refactor(mbsid): move patch bank converter under tools"
```
