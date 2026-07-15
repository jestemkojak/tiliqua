# M6 USB Mass-Storage Patch Load/Export Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `M6_USB_STORAGE.md` — browse/load `.syx` patches from a FAT USB drive
(M6a) and export patches to it (M6b), behind a persisted USB Mode (MIDI/Storage) menu row,
with `top/sid` and `top/sid_player_sw` elaborating byte-identically when not opted in.

**Architecture:** One shared `UTMITranslator` owns the ULPI PHY; `USBMIDIHost` and
`USBMSCHost` are both instantiated with `bus=None` and a CSR mode bit muxes their UTMI
records (Option A), the unselected engine parked in reset. The proven sid_player_sw MSC
read stack (CSR peripheral + `usb_msc.rs`/`partition.rs`/`fat.rs`) is hoisted into shared
modules and reused. Patch files are the exact SysEx byte stream `SysexCapture` already
parses (relaxed "file mode"); export is the inverse (nibblize + 7-bit checksum). M6b
vendors `guh/engines/msc.py` and adds SCSI WRITE(10) + a bulk-OUT TX path + a TX-word CSR
FIFO. A Phase-0 probe build gates the whole plan on area/Fmax before any firmware work.

**Tech Stack:** Amaranth gateware (`src/tiliqua/`, `src/top/sid/top.py`), vendored guh MSC
engine (BSD-3), Rust `no_std` firmware (`fw/`), `fatfs` 0.4 (git, no_std), host tests via
`cargo test --target x86_64-unknown-linux-gnu --lib`, sim tests via `pdm test`.

## Global Constraints

- Repo root for relative paths: `gateware/src/top/mbsid/` unless the path starts with
  `gateware/` or `src/` (then it's relative to `gateware/`).
- **`top/sid` and `top/sid_player_sw` must elaborate byte-identically when not opted in**
  (spec §1 goal). Every new CSR register/port is behind a constructor flag that defaults off.
- Never edit files under `gateware/.venv/` (pinned pip `guh`). Write support = vendor
  `guh/engines/msc.py` into `src/vendor/guh_msc/` (BSD-3 header kept), per spec §2/§4b.
- Never edit vendored `mios32/` C++ (GPL, pinned).
- **Any `SIDPeripheral`/new-peripheral CSR change requires `pdm mbsid build --pac-only`
  before `--fw-only`** (root CLAUDE.md). The PAC is gitignored/regenerated; CSR changes
  never show in `git status`.
- All USB/FAT I/O runs in the **main loop only** — never the 1 ms Timer0 ISR (spec §1).
- No new heap; buffers are stack/`static`; re-measure stack headroom is a HW-checklist
  item (21.8 KB measured headroom post-M4, expected adder 2–3 KB — spec §6f).
- Host firmware tests: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib`.
  Ignore rust-analyzer/LSP errors on `fw/` (no_std false positives — root CLAUDE.md).
- Firmware relink: `cd gateware && pdm mbsid build --fw-only` — ends with an expected
  `missing top.bit` error **after** the ELF builds (that error is success for fw-only;
  a Rust compile error is failure).
- Full build ≈ 4–5 min. Post-route sync Fmax = the **second** `Max frequency for clock
  '$glbnet$clk'` line in `build/mbsid-r5/top.tim` (~line 1390+). PASS lines are `Info:`,
  FAIL lines are `Warning:` — distinguish by order, not tag.
- `sync` == `usb` == the same 60 MHz PLL output (`src/tiliqua/pll.py:470` drives both from
  `feedback60`), which is why sid_player_sw wires CSR (sync) signals straight to the MSC
  engine (usb) with no CDC. Keep that idiom; do not add synchronizers between them.
- Oracle gate after any shim/engine change: none expected in this milestone (no shim
  changes). If a task somehow touches `fw/csrc/`, run `host_oracle/run_oracle.sh`.
- Menu rendering stays a blit-diff (`frame::Frame` + `Painter`): no `Rectangle`/fill in
  the redraw path, no text drawn outside `build_frame`.
- Commit messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Before the final commit of each part, reconcile test counts quoted in `CLAUDE.md` /
  `docs/developer-guide.md` against a fresh test run (doc-count-drift gotcha).

---

### Task 1: Stopgap export script (zero gateware, independent)

Spec §7 "Stopgap export": a PC-side script that turns a raw dump of the M4 user-bank
flash region (`0xF00000..0xF80000`) into standard `.syx` files. De-risks "patches are
trapped in flash" today and doubles as a backup path forever.

**Files:**
- Create: `gateware/scripts/mbsid-export-userbank.py`
- Test: inline `--self-test` mode (no pytest hook; this is a host tool, not gateware).

**Interfaces:**
- Consumes: the `patch_store.rs` slot layout — 4 KiB slots, 8-byte header
  `b"MBUP" | version(1) | 0 | checksum_le(2)`, payload = 512-byte `sid_patch_t` at
  offset 8; payload checksum = u16 wrapping byte-sum (`patch_store.rs:19-21`).
- Produces: `Pnnn.SYX` files — MBSID v2 single-patch dumps
  `F0 00 00 7E 4B 00 02 00 01 00 <1024 nibbles lo-first> <(-sum)&0x7F> F7`
  (same framing `sysex_capture.rs` tests encode; bank byte 1 = User so re-sending the
  file over MIDI lands back in the user bank).

- [ ] **Step 1: Write the script**

```python
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
```

- [ ] **Step 2: Run the self-test**

Run: `python3 gateware/scripts/mbsid-export-userbank.py --self-test`
Expected: `self-test OK`

- [ ] **Step 3: Make executable and commit**

```bash
chmod +x gateware/scripts/mbsid-export-userbank.py
git add gateware/scripts/mbsid-export-userbank.py
git commit -m "feat(mbsid): stopgap user-bank .syx export script (M6 spec §7)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Shared `USBMSCPeripheral` module (`src/tiliqua/usb_msc_csr.py`)

Hoist sid_player_sw's CSR peripheral into a shared module so mbsid can reuse it. Add an
opt-in `with_mode` register (the MIDI/Storage mux bit) that sid_player_sw does not enable
→ its CSR map and elaboration stay identical.

**Files:**
- Create: `src/tiliqua/usb_msc_csr.py`
- Modify: `src/top/sid_player_sw/top.py` (delete lines 55–170: `_USB_STATUS_LAYOUT`,
  `_USB_RESP_LAYOUT`, `class USBMSCPeripheral`; add the import)
- Test: `gateware/tests/test_usb_msc_csr.py` (new), plus the existing
  `tests/test_usb_msc_sw_periph.py` must still pass unchanged.

**Interfaces:**
- Consumes: nothing new (verbatim class from `sid_player_sw/top.py:55-170`).
- Produces (used by Tasks 3, 12):
  - `tiliqua.usb_msc_csr.USBMSCPeripheral(word_fifo_depth=256, with_mode=False)`
  - `tiliqua.usb_msc_csr.{USB_STATUS_LAYOUT, USB_RESP_LAYOUT}` (renamed, no underscore)
  - New port (always present, wired only when `with_mode`): `mode_o: Out(1)`
  - New CSR (only when `with_mode=True`): `mode` at offset **0x1C**, RW, field
    `storage` (1 bit, reset 0 = MIDI).
  - `from top.sid_player_sw.top import USBMSCPeripheral` keeps working (re-export).

- [ ] **Step 1: Write the failing test**

Create `gateware/tests/test_usb_msc_csr.py`:

```python
import unittest
from amaranth import *
from amaranth.sim import Simulator

from tiliqua.usb_msc_csr import USBMSCPeripheral


async def csr_write(ctx, dut, offset, value):
    """8-bit CSR bus write of one byte."""
    ctx.set(dut.bus.addr, offset)
    ctx.set(dut.bus.w_data, value)
    ctx.set(dut.bus.w_stb, 1)
    await ctx.tick()
    ctx.set(dut.bus.w_stb, 0)
    await ctx.tick()


class UsbMscCsrTests(unittest.TestCase):
    def test_mode_register_drives_mode_o(self):
        dut = USBMSCPeripheral(with_mode=True)
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            self.assertEqual(ctx.get(dut.mode_o), 0)  # reset = MIDI
            await csr_write(ctx, dut, 0x1C, 1)
            await ctx.tick()
            self.assertEqual(ctx.get(dut.mode_o), 1)
            await csr_write(ctx, dut, 0x1C, 0)
            await ctx.tick()
            self.assertEqual(ctx.get(dut.mode_o), 0)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_without_mode_has_no_register_and_mode_o_zero(self):
        dut = USBMSCPeripheral()  # sid_player_sw shape
        names = [r.name for r, _, _ in dut.bus.memory_map.resources()] \
            if hasattr(dut.bus.memory_map, "resources") else []
        # The map must end at resp (0x18): no "mode" resource.
        self.assertNotIn("mode", " ".join(str(n) for n in names))


if __name__ == "__main__":
    unittest.main()
```

(If the `memory_map.resources()` API differs in the pinned amaranth-soc, assert instead
that `USBMSCPeripheral()` has no `_mode` attribute — the point is: default off.)

- [ ] **Step 2: Run it to verify it fails**

Run: `cd gateware && pdm run pytest tests/test_usb_msc_csr.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'tiliqua.usb_msc_csr'`

- [ ] **Step 3: Create the shared module**

Create `src/tiliqua/usb_msc_csr.py`: copy `_USB_STATUS_LAYOUT`, `_USB_RESP_LAYOUT`, and
`class USBMSCPeripheral` **verbatim** from `src/top/sid_player_sw/top.py:55-170`
(including the imports it needs: `amaranth`, `amaranth.lib.{data, stream, wiring}`,
`SyncFIFOBuffered`, `In/Out`, `Packet`, `csr`, `ResetInserter`), renaming the layouts to
`USB_STATUS_LAYOUT` / `USB_RESP_LAYOUT`, then apply exactly this delta:

```python
    # In the class body, after `resp_i: In(_USB_RESP_LAYOUT)`:
    mode_o:    Out(1)   # with_mode only: 0 = USB-MIDI owns the PHY, 1 = MSC

    def __init__(self, *, word_fifo_depth=256, with_mode=False):
        self._with_mode = with_mode
        self._word_fifo = SyncFIFOBuffered(width=32, depth=word_fifo_depth)
        regs = csr.Builder(addr_width=5, data_width=8)
        # ... existing regs.add() calls unchanged (status..resp, 0x00..0x18) ...
        if with_mode:
            self._mode = regs.add("mode", self.Mode(), offset=0x1C)
        self._bridge = csr.Bridge(regs.as_memory_map())
        super().__init__()
        self.bus.memory_map = self._bridge.bus.memory_map
```

with the register class added next to the others:

```python
    class Mode(csr.Register, access="rw"):
        """USB port owner: 0 = USB-MIDI host engine (default), 1 = MSC (storage).
        Firmware mirrors the menu's `USB Mode` row here every redraw."""
        storage: csr.Field(csr.action.RW, unsigned(1))
```

and at the end of `elaborate` (before `return m`):

```python
        if self._with_mode:
            m.d.comb += self.mode_o.eq(self._mode.f.storage.data)
```

- [ ] **Step 4: Point sid_player_sw at it**

In `src/top/sid_player_sw/top.py`, delete lines 55–170 (the two layouts + the class) and
add near the other tiliqua imports:

```python
from tiliqua.usb_msc_csr import USBMSCPeripheral
```

(`tests/test_usb_msc_sw_periph.py` imports `USBMSCPeripheral` from
`top.sid_player_sw.top` — the module-level import re-exports it, so the test is
unchanged.) `sid_player_sw` continues to instantiate `USBMSCPeripheral()` with no
arguments → identical CSR map, identical elaboration. Leave `top/sid_player` (non-sw)
alone — its local copy has divergent debug ports.

- [ ] **Step 5: Run the tests**

Run: `cd gateware && pdm run pytest tests/test_usb_msc_csr.py tests/test_usb_msc_sw_periph.py tests/test_usb_msc_periph.py -v`
Expected: all PASS.

- [ ] **Step 6: Prove sid_player_sw still elaborates**

Run: `cd gateware && pdm sid_player_sw build --pac-only`
Expected: exits 0 (SoC elaborates, SVD/PAC regenerate — catches any import/shape break
without a 5-min bitstream). The regenerated sid_player_sw PAC must show **no** `mode`
register on `USB_MSC` (spot-check `src/top/sid_player_sw/pac/svd/soc.svd` if present:
`grep -c '"mode"' → 0` / `grep -A2 usb_msc`).

- [ ] **Step 7: Commit**

```bash
git add src/tiliqua/usb_msc_csr.py src/top/sid_player_sw/top.py gateware/tests/test_usb_msc_csr.py
git commit -m "refactor(tiliqua): hoist USBMSCPeripheral into shared usb_msc_csr module

Verbatim from sid_player_sw; adds opt-in with_mode register (offset 0x1C)
for the M6 MIDI/Storage port mux. sid_player_sw imports it with no args ->
byte-identical CSR map and elaboration.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Phase-0 gateware — `with_usb_msc` opt-in + UTMI mux + PROBE BUILD (decision gate)

Option A from spec §3: both engines instantiated side by side over one shared
`UTMITranslator`; the `mode` CSR bit muxes the UTMI records and parks the unselected
engine in reset; VBUS driven in both modes. This task ends in the **go/no-go gate** for
the whole milestone.

Key source facts (verified):
- `USBMIDIHost(bus=None)` / `USBMSCHost(bus=None)` skip the internal `UTMITranslator`
  and expose a drivable `UTMIInterface` Record at `engine.enumerator.sie.utmi`
  (`guh/usbh/sie.py:343-347`; `USBMSCHost.sie` property exists, `USBMIDIHost` needs
  `usb_midi.enumerator.sie`).
- `UTMIInterface` fields (luna `interface/utmi.py:92-126`): PHY→engine =
  `rx_data, rx_active, rx_valid, tx_ready, line_state, vbus_valid, session_valid,
  session_end, rx_error, host_disconnect, id_digital`; engine→PHY = `tx_data, tx_valid,
  xcvr_select, term_select, op_mode, suspend, id_pullup, dm_pulldown, dp_pulldown,
  chrg_vbus, dischrg_vbus, use_external_vbus_indicator`.
- `UTMITranslator` exposes those same names as attributes (`interface/ulpi.py:695-701`);
  a few (e.g. `use_external_vbus_indicator`, `id_pullup`) may be absent — guard with
  `hasattr` on the PHY side and tie the engine input to 0 if missing.
- Both engines run entirely in the `usb` domain, which is the same 60 MHz PLL net as
  `sync` — no CDC (existing sid_player_sw precedent).

**Files:**
- Modify: `src/top/sid/top.py` (`SIDSoc.__init__` + `elaborate` hw block, lines 498–646)
- Modify: `src/top/mbsid/top.py` (`MBSIDSoc.__init__`)
- Test: probe build metrics (this is a hardware-shape task; sim coverage for the CSR is
  Task 2's, for the engine FSMs upstream's).

**Interfaces:**
- Consumes: `tiliqua.usb_msc_csr.USBMSCPeripheral` (Task 2), `guh.engines.msc.USBMSCHost`,
  `guh.engines.midi.USBMIDIHost`, `luna.gateware.interface.ulpi.UTMITranslator`.
- Produces (used by Tasks 8–9 firmware):
  - `SIDSoc(with_usb_msc=False)` kwarg; when True: `self.usb_msc` =
    `USBMSCPeripheral(with_mode=True)` at CSR **`0x1300`**, name `"usb_msc"`
    (0x1000 = SID_PERIPH, 0x1200 = SID_PERIPH_R — spec §4a).
  - PAC peripheral `pac::USB_MSC` with registers `status/block_size/block_count/lba/
    start/rx_data/resp/mode`.
  - Behavior contract: `mode.storage=0` → USB-MIDI host owns the PHY (M5 behavior,
    except VBUS now always on); `=1` → MSC owns it, MIDI engine held in reset (drops any
    enumerated keyboard; TRS MIDI unaffected). Either engine re-enumerates from scratch
    on a mode flip (reset release → its `WAIT-ENUMERATION`).

- [ ] **Step 1: Add the kwarg and peripheral to `SIDSoc.__init__`**

In `src/top/sid/top.py`, change the signature and add after the `sid_periph_r` block:

```python
    def __init__(self, *, with_scope=True, n_sids=1, with_sysex=False,
                 with_usb_msc=False, **kwargs):
        ...
        self.with_usb_msc = with_usb_msc
        ...
        if self.with_usb_msc:
            # USB mass-storage CSR block (M6). 0x1300: 0x1000/0x1200 taken by
            # the two SIDPeripherals. with_mode adds the MIDI/Storage mux bit.
            from tiliqua.usb_msc_csr import USBMSCPeripheral
            self.usb_msc = USBMSCPeripheral(with_mode=True)
            self.csr_decoder.add(self.usb_msc.bus, addr=0x1300, name="usb_msc")
```

(Place it before `self.finalize_csr_bridge()`.)

- [ ] **Step 2: Restructure the hw USB block in `SIDSoc.elaborate`**

Replace the current `if sim.is_hw(platform):` USB section (lines 616–638; keep the TRS
serial MIDI part above it untouched) with:

```python
            ulpi = platform.request(platform.default_usb_connection)
            vbus_o = platform.request("usb_vbus_en").o

            if not self.with_usb_msc:
                m.submodules.usb = usb = USBMIDIHost(bus=ulpi)
            else:
                # M6 Option A: one UTMITranslator owns the ULPI PHY; both class
                # engines are built with bus=None (raw UTMIInterface records)
                # and a CSR mode bit muxes them. The unselected engine is held
                # in usb-domain reset so a mode flip re-enumerates from scratch
                # (composes with each engine's internal watchdog ResetInserter).
                from guh.engines.msc import USBMSCHost
                from luna.gateware.interface.ulpi import UTMITranslator
                from amaranth import ResetInserter as _RI

                m.submodules.utmi_phy = phy = UTMITranslator(
                    ulpi=ulpi, handle_clocking=True)

                storage_mode = Signal()
                m.d.comb += storage_mode.eq(self.usb_msc.mode_o)

                usb = USBMIDIHost(bus=None)
                msc = USBMSCHost(bus=None)
                m.submodules.usb = _RI({"usb": storage_mode})(usb)
                m.submodules.usb_msc_host = _RI({"usb": ~storage_mode})(msc)

                midi_utmi = usb.enumerator.sie.utmi
                msc_utmi = msc.sie.utmi
                # PHY -> engines: fan out to both (the parked engine is in
                # reset; feeding it RX is harmless).
                for name in ("rx_data", "rx_active", "rx_valid", "tx_ready",
                             "line_state", "vbus_valid", "session_valid",
                             "session_end", "rx_error", "host_disconnect",
                             "id_digital"):
                    src = getattr(phy, name, None)
                    for eng in (midi_utmi, msc_utmi):
                        m.d.comb += getattr(eng, name).eq(
                            src if src is not None else 0)
                # engines -> PHY: mux by mode bit.
                for name in ("tx_data", "tx_valid", "xcvr_select",
                             "term_select", "op_mode", "suspend", "id_pullup",
                             "dm_pulldown", "dp_pulldown", "chrg_vbus",
                             "dischrg_vbus", "use_external_vbus_indicator"):
                    dst = getattr(phy, name, None)
                    if dst is not None:
                        m.d.comb += dst.eq(Mux(storage_mode,
                                               getattr(msc_utmi, name),
                                               getattr(midi_utmi, name)))

                # MSC engine <-> CSR peripheral (same wiring as
                # sid_player_sw/top.py:258-268).
                m.submodules.usb_msc = self.usb_msc
                wiring.connect(m, msc.rx_data, self.usb_msc.rx_data)
                m.d.comb += [
                    self.usb_msc.status_i.connected.eq(msc.status.connected),
                    self.usb_msc.status_i.ready.eq(msc.status.ready),
                    self.usb_msc.status_i.busy.eq(msc.status.busy),
                    self.usb_msc.status_i.block_size.eq(msc.status.block_size),
                    self.usb_msc.status_i.block_count.eq(msc.status.block_count),
                    msc.cmd.lba.eq(self.usb_msc.lba_o),
                    msc.cmd.start.eq(self.usb_msc.start_o),
                    self.usb_msc.resp_i.done.eq(msc.resp.done),
                    self.usb_msc.resp_i.error.eq(msc.resp.error),
                ]

            m.submodules.midi_decode_usb = midi_decode_usb = midi.MidiDecodeUSB(
                forward_rt=True, forward_sysex=self.with_sysex)
            wiring.connect(m, usb.o_midi, midi_decode_usb.i)

            # Source mux: USB host or TRS, controlled by CSR bit. VBUS: with
            # the MSC option a thumb drive needs power in Storage mode even
            # though usb_midi_host=0, so drive it constantly; otherwise keep
            # the M5 behavior (VBUS only in USB-MIDI mode).
            if self.with_usb_msc:
                m.d.comb += vbus_o.eq(1)
            with m.If(self.sid_periph.usb_midi_host):
                wiring.connect(m, midi_decode_usb.o, self.sid_periph.i_midi)
                if not self.with_usb_msc:
                    m.d.comb += vbus_o.eq(1)
                if self.with_sysex:
                    wiring.connect(m, midi_decode_usb.o_sysex, self.sid_periph.i_sysex)
                    m.d.comb += midi_decode_trs.o_sysex.ready.eq(1)
            with m.Else():
                wiring.connect(m, midi_decode_trs.o, self.sid_periph.i_midi)
                if not self.with_usb_msc:
                    m.d.comb += vbus_o.eq(0)
                if self.with_sysex:
                    wiring.connect(m, midi_decode_trs.o_sysex, self.sid_periph.i_sysex)
                    m.d.comb += midi_decode_usb.o_sysex.ready.eq(1)
```

Notes for the implementer:
- `ResetInserter(...)(comp)` wraps elaboration only; `usb.o_midi` etc. remain the
  original component's signals — the existing `wiring.connect` idiom is unchanged.
- If `platform.request("usb_vbus_en")` was previously requested inside the If/Else,
  hoist it exactly once (shown above) — double-request raises.
- `with_usb_msc=False` must produce the exact pre-task structure (default path): diff
  the generated `build/sid-r5/top.il` only if in doubt; the `pdm test` suite plus a
  `pdm sid build --pac-only` elaboration check is the routine gate.

- [ ] **Step 3: Opt mbsid in**

In `src/top/mbsid/top.py` `MBSIDSoc.__init__`, after `kwargs.setdefault("with_sysex", True)`:

```python
        # M6: USB mass-storage patch load/export (M6_USB_STORAGE.md). Adds the
        # USBMSCHost + UTMI mux + usb_msc CSR block at 0x1300 (PAC regen!).
        kwargs.setdefault("with_usb_msc", True)
```

- [ ] **Step 4: Elaboration + PAC + existing-test gate**

```bash
cd gateware
pdm run pytest tests/test_usb_msc_csr.py tests/test_sid_periph.py tests/test_midi.py -v
pdm sid build --pac-only      # non-opted top still elaborates
pdm mbsid build --pac-only    # CSR change -> regen mbsid PAC (root CLAUDE.md)
```
Expected: tests PASS; both `--pac-only` runs exit 0; the mbsid SVD gains `usb_msc` with a
`mode` register at 0x1C.

- [ ] **Step 5: PROBE BUILD, seed 1**

```bash
cd gateware && pdm mbsid build
```
Expected: build completes (timing-allow-fail is on; even a FAIL produces metrics).
Record from `build/mbsid-r5/top.tim`:
- post-route sync Fmax — the **second** `Max frequency for clock '$glbnet$clk'` line;
- LUT usage — `grep TRELLIS_COMB build/mbsid-r5/top.tim | head -1` (baseline
  19444/24288 = 80%).

- [ ] **Step 6: PROBE BUILD, seed 2**

```bash
cd gateware && AMARANTH_nextpnr_opts="--timing-allow-fail --seed 2" pdm mbsid build
```
(Amaranth templated platforms honor `AMARANTH_<option>` env overrides. **Verify the
override took**: `grep -o '\-\-seed 2' build/mbsid-r5/build_top.sh` — if absent, instead
temporarily change `"nextpnr_opts": "--timing-allow-fail"` at
`src/tiliqua/build/cli.py:306` to `"--timing-allow-fail --seed 2"`, rebuild, and revert.)
Record the same two numbers.

- [ ] **Step 7: DECISION GATE (spec §3 decision rule)**

- **PASS** iff post-route sync Fmax ≥ **61 MHz** (60 target + 1 margin) on **both**
  seeds and the design routed. → proceed with this plan (Option A).
- **FAIL** → STOP HERE. Do not start Task 4. Report the numbers and re-plan gateware as
  Option B (combined `USBDualModeHost`, spec §3): Tasks 4–10 (firmware) survive a
  re-plan almost unchanged, Task 3 is replaced. (Remember: a PASS-side Fmax jump vs
  baseline is seed noise, only a FAIL is actionable — root CLAUDE.md.)

- [ ] **Step 8: Commit (gate passed)**

```bash
git add src/top/sid/top.py src/top/mbsid/top.py
git commit -m "feat(mbsid): with_usb_msc opt-in — dual USB engines behind UTMI mux (M6 Phase 0)

Option A probe passed: sync Fmax <X>/<Y> MHz over 2 seeds, <Z>% LUT.
usb_msc CSR block at 0x1300 (mode bit at 0x1C). top/sid & sid_player_sw
default path unchanged.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Firmware MSC read stack (copy from sid_player_sw, generic block-IO)

**Files:**
- Modify: `fw/Cargo.toml` (add fatfs)
- Modify: `fw/src/lib.rs`
- Create: `fw/src/usb_msc.rs` (from `../sid_player_sw/fw/src/usb_msc.rs`)
- Create: `fw/src/partition.rs` (verbatim from `../sid_player_sw/fw/src/partition.rs`,
  including its `#[cfg(test)]` module)
- Create: `fw/src/fat.rs` (from `../sid_player_sw/fw/src/fat.rs`, made generic — see below)

**Interfaces:**
- Consumes: `pac::USB_MSC` (Task 3 PAC).
- Produces (used by Tasks 6, 9, 13, 14):
  - `usb_msc::UsbMsc::{new(pac::USB_MSC), ready() -> bool, connected() -> bool,
    block_size() -> u16, set_mode(storage: bool),
    read_block(lba: u32, &mut [u8;512]) -> Result<(), MscError>}`
  - `fat::BlockIo` trait: `fn read_block(&mut self, lba: u32, buf: &mut [u8; 512])
    -> Result<(), ()>;` (M6b Task 13 adds `write_block`)
  - `fat::MscStorage<B: BlockIo>::{new(B) -> Self, base_lba() -> u32}` implementing
    fatfs `Read + Write + Seek` (writes error in M6a)
  - `partition::first_partition_lba(read_block_closure) -> u32`

- [ ] **Step 1: Cargo + lib.rs plumbing**

`fw/Cargo.toml`, after `midi-convert`:

```toml
fatfs = { git = "https://github.com/rafalh/rust-fatfs", default-features = false }
```

`fw/src/lib.rs` — change the first line and add the modules (host tests need std for
in-memory FAT images, same shape as sid_player_sw's lib.rs; existing heapless-style
tests are unaffected by the extra permissiveness):

```rust
#![cfg_attr(not(test), no_std)]

pub mod cv;
pub mod frame;
pub mod regdiff;
pub mod mbsid_sys;
pub mod menu;
pub mod sysex_capture;
pub mod patch_store;
pub mod params;
pub mod settings_store;
pub mod partition;
pub mod fat;
pub mod usb_patch;          // Task 6 (add the file in that task; keep this line commented until then)
#[cfg(not(test))]
pub mod usb_msc;            // pac-dependent: embedded only
```

(Leave `pub mod usb_patch;` commented out until Task 6 so this task compiles.)

- [ ] **Step 2: `fw/src/usb_msc.rs`**

Copy `../sid_player_sw/fw/src/usb_msc.rs` verbatim, then add two methods and the
`BlockIo` impl:

```rust
    pub fn connected(&self) -> bool {
        self.regs.status().read().connected().bit_is_set()
    }

    /// Mirror of the menu's USB Mode row: 1 = MSC owns the PHY (Storage).
    pub fn set_mode(&self, storage: bool) {
        self.regs.mode().write(|w| w.storage().bit(storage));
    }
```

and at the bottom:

```rust
impl crate::fat::BlockIo for &UsbMsc {
    fn read_block(&mut self, lba: u32, buf: &mut [u8; 512]) -> Result<(), ()> {
        UsbMsc::read_block(self, lba, buf).map_err(|_| ())
    }
}
```

- [ ] **Step 3: `fw/src/partition.rs`**

Copy verbatim from `../sid_player_sw/fw/src/partition.rs` (147 lines incl. tests). Its
tests use `extern crate alloc` + `alloc::vec` — with the Step 1 `cfg_attr` change, test
builds have std, so `alloc` resolves.

- [ ] **Step 4: `fw/src/fat.rs`** — the sid_player_sw adapter, generic over `BlockIo`

```rust
//! fatfs storage adapter over a generic 512-byte block device.
//!
//! Ported from top/sid_player_sw/fw/src/fat.rs, with the concrete `UsbMsc`
//! dependency replaced by the `BlockIo` trait so the adapter (and, in M6b,
//! its write-back cache) is host-testable against an in-memory disk.
//! M6a: read-only (writes error, like upstream).

use fatfs::{IoBase, IoError, Read, Seek, SeekFrom, Write};
pub use fatfs::{FileSystem, FsOptions};

/// Minimal 512-byte block device. Implemented by `&usb_msc::UsbMsc` on
/// target and by in-memory disks in host tests.
pub trait BlockIo {
    fn read_block(&mut self, lba: u32, buf: &mut [u8; 512]) -> Result<(), ()>;
}

#[derive(Debug)]
pub struct StorageError;

impl IoError for StorageError {
    fn is_interrupted(&self) -> bool { false }
    fn new_unexpected_eof_error() -> Self { StorageError }
    fn new_write_zero_error() -> Self { StorageError }
}

/// Block-cached storage adapter: presents the first FAT partition as a
/// byte stream starting at its BPB (fatfs mounts at stream offset 0).
pub struct MscStorage<B: BlockIo> {
    io: B,
    pos: u64,
    base_lba: u32,
    cache_lba: Option<u32>,
    cache: [u8; 512],
}

impl<B: BlockIo> MscStorage<B> {
    pub fn new(mut io: B) -> Self {
        let base_lba = crate::partition::first_partition_lba(
            |lba, buf| io.read_block(lba, buf));
        Self { io, pos: 0, base_lba, cache_lba: None, cache: [0u8; 512] }
    }

    pub fn base_lba(&self) -> u32 { self.base_lba }

    fn ensure_block(&mut self, lba: u32) -> Result<(), StorageError> {
        if self.cache_lba != Some(lba) {
            let mut buf = [0u8; 512];
            self.io.read_block(lba, &mut buf).map_err(|_| StorageError)?;
            self.cache = buf;
            self.cache_lba = Some(lba);
        }
        Ok(())
    }
}

impl<B: BlockIo> IoBase for MscStorage<B> { type Error = StorageError; }

impl<B: BlockIo> Read for MscStorage<B> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() { return Ok(0); }
        let lba = self.base_lba + (self.pos / 512) as u32;
        let off = (self.pos % 512) as usize;
        self.ensure_block(lba)?;
        let n = (512 - off).min(buf.len());
        buf[..n].copy_from_slice(&self.cache[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<B: BlockIo> Write for MscStorage<B> {
    fn write(&mut self, _buf: &[u8]) -> Result<usize, Self::Error> {
        Err(StorageError) // M6a: read-only (M6b un-stubs this)
    }
    fn flush(&mut self) -> Result<(), Self::Error> { Ok(()) }
}

impl<B: BlockIo> Seek for MscStorage<B> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let new_pos: i64 = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.pos as i64 + n,
            SeekFrom::End(_) => return Err(StorageError),
        };
        if new_pos < 0 { return Err(StorageError); }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}
```

- [ ] **Step 5: Run host tests**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: PASS, count = previous 86 + 4 (partition.rs tests). Record the new total.

- [ ] **Step 6: Verify the target still links**

Run: `cd gateware && pdm mbsid build --fw-only`
Expected: Rust compiles clean; ends with the expected `missing top.bit`.
(This proves `pac::USB_MSC` + `mode` register exist — i.e. Task 3's `--pac-only` ran.)

- [ ] **Step 7: Commit**

```bash
git add fw/Cargo.toml fw/src/lib.rs fw/src/usb_msc.rs fw/src/partition.rs fw/src/fat.rs
git commit -m "feat(mbsid): MSC read stack — UsbMsc driver, partition scan, generic fatfs adapter (M6a)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: `SysexCapture` file mode

Spec §6c: a relaxed accept condition for file-sourced bytes — any cmd-0x02 patch dump
(header, 1024 nibbles, checksum, F7 still enforced; type/bank/patch ignored). The
ISR/live path keeps today's strict bank-1 rule.

**Files:**
- Modify: `fw/src/sysex_capture.rs`

**Interfaces:**
- Produces (used by Task 6): `SysexCapture::file_mode() -> Self`; in file mode `feed`
  returns true for any valid patch dump regardless of type/bank.

- [ ] **Step 1: Write the failing tests** (append to the existing `tests` module)

```rust
    #[test]
    fn file_mode_accepts_factory_bank0_dump() {
        let p = test_patch();
        let msg = encode(0x00, 0x00, 3, &p); // bank 0: strict mode rejects this
        let mut cap = SysexCapture::file_mode();
        assert_eq!(feed_all(&mut cap, &msg), 1);
        assert_eq!(cap.data(), &p);
    }

    #[test]
    fn file_mode_accepts_ram_write_type() {
        let p = test_patch();
        let msg = encode(0x08, 0x00, 0, &p); // RAM Write framing, same body
        let mut cap = SysexCapture::file_mode();
        assert_eq!(feed_all(&mut cap, &msg), 1);
        assert_eq!(cap.data(), &p);
    }

    #[test]
    fn file_mode_still_rejects_bad_checksum() {
        let mut msg = encode(0x00, 0x00, 7, &test_patch());
        msg[1034] = (msg[1034] + 1) & 0x7F;
        let mut cap = SysexCapture::file_mode();
        assert_eq!(feed_all(&mut cap, &msg), 0);
    }

    #[test]
    fn strict_mode_unchanged_by_file_mode_addition() {
        let msg = encode(0x00, 0x00, 3, &test_patch());
        let mut cap = SysexCapture::new();
        assert_eq!(feed_all(&mut cap, &msg), 0); // bank 0 still read-only live
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib sysex_capture`
Expected: FAIL — `no function or associated item named 'file_mode'`.

- [ ] **Step 3: Implement**

In `SysexCapture`: add field `file_mode: bool` (set in both constructors):

```rust
    pub fn new() -> Self { Self::with_mode(false) }

    /// File-import mode (M6 spec §6c): accept ANY cmd-0x02 patch dump —
    /// type/bank/patch bytes are ignored (a file explicitly chosen by the
    /// user carries its own intent) — while still enforcing header, nibble
    /// count, checksum, and the F7 terminator. The live MIDI path keeps
    /// `new()`'s strict Bank-Write/bank-1 rule.
    pub fn file_mode() -> Self { Self::with_mode(true) }

    fn with_mode(file_mode: bool) -> Self {
        Self {
            state: State::Idle, hdr_ix: 0, buf: [0u8; 512],
            data_ix: 0, checksum: 0, lnibble: false, bank: 0, patch: 0,
            file_mode,
        }
    }
```

In `feed`, two changes:

```rust
        if b >= 0x80 {
            let done = b == 0xF7 && self.state == State::Term;
            let (bank, complete) = (self.bank, done);
            self.reset();
            return complete && (self.file_mode || bank == USER_BANK);
        }
```

Wait — `self.file_mode` must be read **before** `reset()` only if reset clears it; it
doesn't (reset touches state/hdr_ix only), so the line above is fine as written.

```rust
            State::Type => {
                // Strict: only Bank Write to sid 0. File mode: any type — the
                // body framing (bank+patch+1024 nibbles) is identical.
                if b == TYPE_BANK_WRITE_SID0 || self.file_mode {
                    self.state = State::Bank;
                    self.data_ix = 0;
                    self.checksum = 0;
                    self.lnibble = false;
                } else {
                    self.state = State::Skip;
                }
            }
```

- [ ] **Step 4: Run tests**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib sysex_capture`
Expected: all sysex_capture tests PASS (12 existing + 4 new).

- [ ] **Step 5: Commit**

```bash
git add fw/src/sysex_capture.rs
git commit -m "feat(mbsid): SysexCapture::file_mode — relaxed accept for file import (M6a §6c)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: `fw/src/usb_patch.rs` — list + load (host-tested against an in-memory FAT image)

**Files:**
- Create: `fw/src/usb_patch.rs`
- Modify: `fw/src/lib.rs` (uncomment `pub mod usb_patch;`)

**Interfaces:**
- Consumes: `fat::{FileSystem, BlockIo?}` — functions are generic over
  `IO: fatfs::ReadWriteSeek` exactly like sid_player_sw's `sid_scan.rs`;
  `SysexCapture::file_mode` (Task 5).
- Produces (used by Task 9; Task 14 adds `export_patch`/`encode_syx`):
  - `pub type FileName = heapless::String<16>;`
  - `pub const MAX_FILES: usize = 64;`
  - `pub type FileList = heapless::Vec<FileName, MAX_FILES>;`
  - `pub fn list_patch_files<IO: ReadWriteSeek>(fs: &FileSystem<IO>, out: &mut FileList) -> usize`
  - `pub fn load_patch_by_index<IO: ReadWriteSeek>(fs: &FileSystem<IO>, idx: usize, dst: &mut [u8; 512]) -> bool`
  - `pub fn parse_patch_file(bytes: &[u8], dst: &mut [u8; 512]) -> bool` (pure)

- [ ] **Step 1: Write the module with failing tests**

```rust
//! USB-drive patch-file finder/loader over a mounted FAT volume (M6a §6b).
//!
//! Mirrors sid_player_sw's sid_scan.rs split: generic over ReadWriteSeek so
//! host tests drive the same code against an in-memory FAT image. Files live
//! in `/MBSID/` (preferred) or the root dir (fallback, so hand-copied files
//! Just Work). A patch file is either a standard MBSID v2 single-patch SysEx
//! dump (*.SYX, parsed by SysexCapture::file_mode) or a raw 512-byte
//! sid_patch_t (exact size match).

use fatfs::{Dir, FileSystem, Read, ReadWriteSeek};
use crate::sysex_capture::SysexCapture;

pub type FileName = heapless::String<16>;
pub const MAX_FILES: usize = 64;
pub type FileList = heapless::Vec<FileName, MAX_FILES>;

/// Upper bound on an accepted file: one 1036-byte single-patch dump with
/// slack for editors that pad; anything bigger is not a single patch.
pub const MAX_FILE_BYTES: usize = 2048;

fn is_syx_name(name: &[u8]) -> bool {
    name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(b".SYX")
}

fn candidate(name: &[u8], len: u64) -> bool {
    (is_syx_name(name) && len as usize <= MAX_FILE_BYTES) || len == 512
}

/// The directory patch files live in: `/MBSID/` if present, else the root.
fn patch_dir<IO: ReadWriteSeek>(fs: &FileSystem<IO>) -> Dir<'_, IO> {
    let root = fs.root_dir();
    match root.open_dir("MBSID") {
        Ok(d) => d,
        Err(_) => root,
    }
}

pub fn list_patch_files<IO: ReadWriteSeek>(
    fs: &FileSystem<IO>, out: &mut FileList) -> usize {
    let dir = patch_dir(fs);
    for entry in dir.iter() {
        let Ok(e) = entry else { break };
        if e.is_dir() { continue; }
        let name = e.short_file_name_as_bytes();
        if !candidate(name, e.len()) { continue; }
        let mut s = FileName::new();
        let _ = s.push_str(core::str::from_utf8(name).unwrap_or("?"));
        if out.push(s).is_err() { break; }
    }
    out.len()
}

/// Parse file bytes into a 512-byte sid_patch_t image. Raw 512-byte files
/// are taken verbatim; anything else must contain a valid patch dump.
pub fn parse_patch_file(bytes: &[u8], dst: &mut [u8; 512]) -> bool {
    if bytes.len() == 512 {
        dst.copy_from_slice(bytes);
        return true;
    }
    let mut cap = SysexCapture::file_mode();
    for &b in bytes {
        if cap.feed(b) {
            dst.copy_from_slice(cap.data());
            return true;
        }
    }
    false
}

/// Read the idx-th candidate file and parse it into `dst`.
pub fn load_patch_by_index<IO: ReadWriteSeek>(
    fs: &FileSystem<IO>, idx: usize, dst: &mut [u8; 512]) -> bool {
    let dir = patch_dir(fs);
    let mut count = 0usize;
    for entry in dir.iter() {
        let Ok(e) = entry else { return false };
        if e.is_dir() { continue; }
        let name = e.short_file_name_as_bytes();
        if !candidate(name, e.len()) { continue; }
        if count == idx {
            let mut file = e.to_file();
            let mut buf = [0u8; MAX_FILE_BYTES];
            let mut total = 0usize;
            while total < buf.len() {
                match file.read(&mut buf[total..]) {
                    Ok(0) => break,
                    Ok(n) => total += n,
                    Err(_) => return false,
                }
            }
            return parse_patch_file(&buf[..total], dst);
        }
        count += 1;
    }
    false
}
```

Tests (same in-memory `VecDisk` + `build_gpt_fat_image` helpers as
`../sid_player_sw/fw/src/sid_scan.rs:104-213` — copy them verbatim into this module's
`#[cfg(test)]`, they are self-contained; then):

```rust
    /// Encode a single-patch dump exactly as sysex_capture's tests do.
    fn syx_bytes(patch: &[u8; 512]) -> Vec<u8> {
        const HEADER: [u8; 6] = [0xF0, 0x00, 0x00, 0x7E, 0x4B, 0x00];
        let mut out = Vec::new();
        out.extend_from_slice(&HEADER);
        out.extend_from_slice(&[0x02, 0x00, 0x01, 0x00]);
        let mut sum: u32 = 0;
        for &d in patch.iter() {
            let (lo, hi) = (d & 0x0F, (d >> 4) & 0x0F);
            out.push(lo); out.push(hi);
            sum += (lo + hi) as u32;
        }
        out.push(((sum as i32).wrapping_neg() & 0x7F) as u8);
        out.push(0xF7);
        out
    }

    fn test_patch(seed: u8) -> [u8; 512] {
        let mut p = [0u8; 512];
        for (i, b) in p.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31).wrapping_add(seed);
        }
        p
    }

    #[test]
    fn lists_syx_and_raw512_skips_others() {
        let p = test_patch(1);
        let raw = test_patch(2);
        let mut img = build_gpt_fat_image(&[
            ("LEAD1.SYX", &syx_bytes(&p)),
            ("README.TXT", b"not a patch"),
            ("RAW.BIN", &raw),          // exactly 512 bytes -> candidate
            ("BIG.SYX", &[0u8; 4096]),  // too big -> skipped
        ]);
        let base = BASE_LBA as usize * SECTOR;
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut out = FileList::new();
        assert_eq!(list_patch_files(&fs, &mut out), 2);
        assert_eq!(out[0].as_str(), "LEAD1.SYX");
        assert_eq!(out[1].as_str(), "RAW.BIN");
    }

    #[test]
    fn loads_syx_by_index_and_parses_body() {
        let p = test_patch(3);
        let mut img = build_gpt_fat_image(&[("A.SYX", &syx_bytes(&p))]);
        let base = BASE_LBA as usize * SECTOR;
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut dst = [0u8; 512];
        assert!(load_patch_by_index(&fs, 0, &mut dst));
        assert_eq!(dst, p);
    }

    #[test]
    fn loads_raw_512_verbatim() {
        let p = test_patch(4);
        let mut img = build_gpt_fat_image(&[("R.BIN", &p)]);
        let base = BASE_LBA as usize * SECTOR;
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut dst = [0u8; 512];
        assert!(load_patch_by_index(&fs, 0, &mut dst));
        assert_eq!(dst, p);
    }

    #[test]
    fn mbsid_dir_preferred_over_root() {
        // Create /MBSID/IN.SYX plus a root ROOT.SYX; the /MBSID one must win.
        let p_dir = test_patch(5);
        let mut img = build_gpt_fat_image(&[("ROOT.SYX", &syx_bytes(&test_patch(6)))]);
        {
            let base = BASE_LBA as usize * SECTOR;
            let part = VecDisk::new(&mut img[base..]);
            let fs = FileSystem::new(part, FsOptions::new()).unwrap();
            let d = fs.root_dir().create_dir("MBSID").unwrap();
            let mut f = d.create_file("IN.SYX").unwrap();
            write_all(&mut f, &syx_bytes(&p_dir));
            f.flush().unwrap();
        }
        let base = BASE_LBA as usize * SECTOR;
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut out = FileList::new();
        assert_eq!(list_patch_files(&fs, &mut out), 1);
        assert_eq!(out[0].as_str(), "IN.SYX");
        let mut dst = [0u8; 512];
        assert!(load_patch_by_index(&fs, 0, &mut dst));
        assert_eq!(dst, p_dir);
    }

    #[test]
    fn corrupt_syx_load_fails() {
        let mut bytes = syx_bytes(&test_patch(7));
        let n = bytes.len();
        bytes[n - 2] = (bytes[n - 2] + 1) & 0x7F; // break checksum
        let mut img = build_gpt_fat_image(&[("BAD.SYX", &bytes)]);
        let base = BASE_LBA as usize * SECTOR;
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut dst = [0u8; 512];
        assert!(!load_patch_by_index(&fs, 0, &mut dst));
    }
```

(Test-module imports: `use super::*;`, plus `std::vec::Vec` is available since test
builds have std after Task 4's `cfg_attr`. Reuse `write_all` from the copied helpers.
Note `build_gpt_fat_image` formats FAT and the helpers slice at `BASE_LBA` — identical
mechanics to sid_scan.rs, already proven there.)

- [ ] **Step 2: Run to verify test failure, then compile clean**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib usb_patch`
Expected first run: compile errors until `lib.rs` line uncommented; then all 5 PASS.
(If `fatfs::Dir::open_dir` / `e.len()` names differ in the pinned fatfs 0.4 git rev,
check `~/.cargo` checkout of `rust-fatfs` — `DirEntry::len()` and `Dir::open_dir`
exist in 0.4; adjust only the call-sites, not the interface this task Produces.)

- [ ] **Step 3: Full host test run**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: PASS. Record count.

- [ ] **Step 4: Commit**

```bash
git add fw/src/usb_patch.rs fw/src/lib.rs
git commit -m "feat(mbsid): usb_patch — /MBSID .syx & raw-512 list/load over FAT (M6a §6b)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Settings record v2 — persist `usb_mode`

**Files:**
- Modify: `fw/src/settings_store.rs`

**Interfaces:**
- Produces: `Settings { midi_src: u8, cv_targets: [u8; 4], usb_mode: u8 }` (0 = MIDI,
  1 = Storage), record layout `"MBS5" | ver=2 | midi_src | cv_targets[4] | usb_mode |
  reserved[4] | chk` (usb_mode at byte 10). v1 records decode with `usb_mode = 0`
  (their reserved bytes are zero, checksum unchanged) — no settings loss on upgrade;
  anything else decodes to defaults (spec §3).

- [ ] **Step 1: Write the failing tests** (append)

```rust
    #[test]
    fn v2_roundtrip_with_usb_mode() {
        let s = Settings { midi_src: 1, cv_targets: [0, 3, 11, 12], usb_mode: 1 };
        assert_eq!(decode(&encode(&s)), Some(s));
    }

    #[test]
    fn v1_record_decodes_with_usb_mode_default() {
        // A v1 record as M5 wrote it: version byte 1, byte 10 reserved-zero.
        let s = Settings { midi_src: 1, cv_targets: [1, 2, 3, 4], usb_mode: 0 };
        let mut r = encode(&s);
        r[4] = 1; // rewrite as v1
        let sum: u8 = r[..15].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        r[15] = sum.wrapping_neg();
        assert_eq!(decode(&r), Some(s)); // decodes, usb_mode defaults to 0
    }

    #[test]
    fn unknown_version_rejected() {
        let s = Settings::default();
        let mut r = encode(&s);
        r[4] = 3;
        let sum: u8 = r[..15].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        r[15] = sum.wrapping_neg();
        assert_eq!(decode(&r), None);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib settings_store`
Expected: FAIL — `struct Settings has no field named usb_mode`.

- [ ] **Step 3: Implement**

```rust
const VERSION: u8 = 2;   // v2 adds usb_mode (M6); v1 records still decode.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Settings {
    pub midi_src: u8,        // 0 = TRS, 1 = USB
    pub cv_targets: [u8; 4], // CvTarget::to_u8 encoding
    pub usb_mode: u8,        // 0 = MIDI, 1 = Storage (M6)
}

pub fn encode(s: &Settings) -> [u8; RECORD_LEN] {
    let mut r = [0u8; RECORD_LEN];
    r[0..4].copy_from_slice(&MAGIC);
    r[4] = VERSION;
    r[5] = s.midi_src;
    r[6..10].copy_from_slice(&s.cv_targets);
    r[10] = s.usb_mode;
    let sum: u8 = r[..15].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    r[15] = sum.wrapping_neg();
    r
}

pub fn decode(r: &[u8; RECORD_LEN]) -> Option<Settings> {
    if r[0..4] != MAGIC || !(r[4] == 1 || r[4] == 2) { return None; }
    if r.iter().fold(0u8, |a, &b| a.wrapping_add(b)) != 0 { return None; }
    let mut cv = [0u8; 4];
    cv.copy_from_slice(&r[6..10]);
    // v1 records carry reserved-zero at byte 10, so reading it as usb_mode
    // is exactly the intended "old records default to MIDI" behavior.
    Some(Settings { midi_src: r[5], cv_targets: cv, usb_mode: r[10] & 1 })
}
```

Also update the module doc comment (layout line) accordingly.

- [ ] **Step 4: Run tests**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib settings_store`
Expected: all PASS (5 existing + 3 new; the two existing `Settings {...}` literals in
older tests gain `usb_mode: 0` — fix them as part of this step).

- [ ] **Step 5: Commit**

```bash
git add fw/src/settings_store.rs
git commit -m "feat(mbsid): settings v2 — persist USB Mode (MIDI/Storage), v1-compatible decode (M6a)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Menu — `USB Mode` row + `Card::Usb`

**Files:**
- Modify: `fw/src/menu.rs`
- Modify: `fw/src/frame.rs` (only the `MAX_ITEMS` comment; 12 still suffices — worst
  card after this task is Main: title + 6 rows + detail + status = 9)

**Interfaces:**
- Consumes: nothing new.
- Produces (used by Task 9; Task 14 adds the Export row):
  - `Card::Usb` variant; `Card::step(self, delta: i8, usb_enabled: bool)` (signature
    change — update the one caller in `on_turn`).
  - `MenuState` new pub fields: `usb_storage: bool` (default false),
    `usb_file: i16` (default -1 = none), `usb_file_count: u8` (default 0, set by main
    loop after each list refresh), `usb_slot: i16` (default -1 = Cancel).
  - Row constants: `MAIN_ROW_USBMODE: u8 = 5` (Main row_count → 6);
    `USB_ROW_FILE: u8 = 1`, `USB_ROW_LOADSLOT: u8 = 2` (Usb row_count → 3 in M6a).
  - `PressResult` gains `UsbLoad(u8)` (audition file idx) and
    `UsbLoadToSlot { file: u8, slot: u8 }`.
  - `TurnResult::SettingsChanged` reused for the USB Mode toggle (persists usb_mode).
  - `DriveState { NoDrive, Ready, Busy }` and
    `UsbInfo<'a> { drive: DriveState, file_name: Option<&'a str>, file_count: u8,
    slot_name: Option<&'a str> }`; `build_frame` gains a `usb: Option<&UsbInfo>`
    parameter (passed `None` by non-Usb-card callers is NOT how it works — main.rs
    always passes `Some` when `card == Usb`, `None` otherwise; build_frame only reads
    it on the Usb card).
  - Semantics: Usb card is reachable via the Card row only when `usb_storage` is true.
    `USB_ROW_FILE`: Edit-turn scrolls `usb_file` in `[-1, file_count-1]`; Edit→Nav
    press = `UsbLoad(file)` when `usb_file >= 0`, else `Cancel`. `USB_ROW_LOADSLOT`:
    save-row twin over `usb_slot` (Cancel-first on Edit entry); commit =
    `UsbLoadToSlot { file, slot }` (requires `usb_file >= 0`, else `Cancel`).

- [ ] **Step 1: Write the failing tests** (append to menu tests)

```rust
    #[test]
    fn usb_mode_row_toggles_and_reports_settings_change() {
        let mut m = MenuState::new(1, 0, 0);
        assert!(!m.usb_storage);
        m.focus = MAIN_ROW_USBMODE;
        let _ = m.on_press();
        assert_eq!(m.on_turn(1), TurnResult::SettingsChanged);
        assert!(m.usb_storage);
        assert_eq!(m.on_turn(1), TurnResult::SettingsChanged); // toggles back
        assert!(!m.usb_storage);
    }

    #[test]
    fn usb_card_unreachable_unless_storage_mode() {
        let mut m = MenuState::new(1, 0, 0);
        let _ = m.on_press(); // Edit on Card row
        for _ in 0..5 { let _ = m.on_turn(1); }
        assert_eq!(m.card, Card::PatchEdit); // clamped, no Usb
        m.usb_storage = true;
        let _ = m.on_turn(1);
        assert_eq!(m.card, Card::Usb);
    }

    #[test]
    fn usb_file_row_scrolls_and_loads_on_press() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.usb_file_count = 3;
        m.focus = USB_ROW_FILE;
        assert_eq!(m.on_press(), PressResult::Toggled); // Nav -> Edit
        let _ = m.on_turn(2);           // -1 -> 1
        assert_eq!(m.usb_file, 1);
        let _ = m.on_turn(10);          // clamp at 2
        assert_eq!(m.usb_file, 2);
        assert_eq!(m.on_press(), PressResult::UsbLoad(2)); // Edit -> Nav commits
        assert_eq!(m.mode, Mode::Nav);
    }

    #[test]
    fn usb_file_row_with_no_files_cancels() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.usb_file_count = 0;
        m.focus = USB_ROW_FILE;
        let _ = m.on_press();
        let _ = m.on_turn(5);           // nothing to select
        assert_eq!(m.usb_file, -1);
        assert_eq!(m.on_press(), PressResult::Cancel);
    }

    #[test]
    fn usb_loadslot_row_commits_file_and_slot() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.usb_file_count = 2;
        m.usb_file = 1;
        m.focus = USB_ROW_LOADSLOT;
        assert_eq!(m.on_press(), PressResult::Toggled);
        assert_eq!(m.usb_slot, -1);     // Cancel-first
        let _ = m.on_turn(4);
        assert_eq!(m.on_press(),
                   PressResult::UsbLoadToSlot { file: 1, slot: 3 });
    }

    #[test]
    fn usb_loadslot_without_file_cancels() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.usb_file = -1;
        m.focus = USB_ROW_LOADSLOT;
        let _ = m.on_press();
        let _ = m.on_turn(4);
        assert_eq!(m.on_press(), PressResult::Cancel);
    }

    #[test]
    fn build_frame_usb_card_rows() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.usb_file_count = 2;
        m.usb_file = 0;
        let usb = UsbInfo { drive: DriveState::Ready,
                            file_name: Some("LEAD1.SYX"), file_count: 2,
                            slot_name: None };
        let fr = build_frame(&m, "P", None, None, None, true,
                             Some(&usb), 60, 80);
        // title + Card + Drive + File + Load>Slot = 5 (no status)
        assert_eq!(fr.items.len(), 5);
        assert!(fr.items[2].text.contains("Ready"));
        assert!(fr.items[3].text.contains("LEAD1.SYX"));
        assert!(fr.items[4].text.contains("Load>Slot"));
    }

    #[test]
    fn build_frame_main_shows_usb_mode_row() {
        let m = MenuState::new(1, 0, 0);
        let fr = build_frame(&m, "P", None, None, None, true, None, 60, 80);
        // title + Card + Bank + Program + Save + MidiSrc + UsbMode + detail = 8
        assert_eq!(fr.items.len(), 8);
        assert!(fr.items[6].text.contains("USB Mode MIDI"));
    }
```

Also mechanically update every existing `build_frame(...)` call in the test module to
pass `None` for the new `usb` parameter (before `pos_x`), and the two frame-count
assertions that cover the Main card (`build_frame_main_card_rows_and_styles` expects 8
items now, detail row index shifts 6→7; `focus_move_changes_exactly_two_rows` and
friends are index-agnostic).

- [ ] **Step 2: Run to verify failure**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib menu`
Expected: compile FAIL (`Card::Usb`, `MAIN_ROW_USBMODE`, … missing).

- [ ] **Step 3: Implement**

`Card`:

```rust
pub enum Card { Main, CvMod, PatchEdit, Usb }

impl Card {
    pub fn label(self) -> &'static str {
        match self { Self::Main => "Main", Self::CvMod => "CV Mod",
                     Self::PatchEdit => "Edit", Self::Usb => "USB" }
    }
    fn step(self, delta: i8, usb_enabled: bool) -> Self {
        let ix = match self {
            Self::Main => 0i16, Self::CvMod => 1,
            Self::PatchEdit => 2, Self::Usb => 3,
        };
        let hi = if usb_enabled { 3 } else { 2 };
        match (ix + delta as i16).clamp(0, hi) {
            0 => Self::Main, 1 => Self::CvMod, 2 => Self::PatchEdit,
            _ => Self::Usb,
        }
    }
}
```

Constants + state:

```rust
pub const MAIN_ROW_USBMODE: u8 = 5;
pub const USB_ROW_FILE: u8 = 1;
pub const USB_ROW_LOADSLOT: u8 = 2;
```

`MenuState` fields (with `new()` defaults `usb_storage: false, usb_file: -1,
usb_file_count: 0, usb_slot: -1`), `row_count`:

```rust
            Card::Main => 6,
            Card::Usb  => 3,   // Card + File + Load>Slot (Export lands in M6b)
```

`on_turn` — the Card row arm becomes
`self.card = self.card.step(delta, self.usb_storage);`; the `Card::Main` match gains:

```rust
                        MAIN_ROW_USBMODE => {
                            if delta != 0 {
                                self.usb_storage = !self.usb_storage;
                                TurnResult::SettingsChanged
                            } else { TurnResult::None }
                        }
```

and a `Card::Usb` arm:

```rust
                    Card::Usb => match self.focus {
                        USB_ROW_FILE => {
                            let hi = self.usb_file_count as i16 - 1;
                            self.usb_file = clamp_i16(
                                self.usb_file + delta as i16, -1, hi.max(-1));
                            TurnResult::None
                        }
                        USB_ROW_LOADSLOT => {
                            self.usb_slot =
                                clamp_i16(self.usb_slot + delta as i16, -1, 127);
                            TurnResult::None
                        }
                        _ => TurnResult::None,
                    },
```

`on_press` — extend the commit logic (keep the existing save-row path intact):

```rust
    pub fn on_press(&mut self) -> PressResult {
        let result = if self.card == Card::Usb && self.mode == Mode::Edit {
            match self.focus {
                USB_ROW_FILE => {
                    if self.usb_file >= 0 { PressResult::UsbLoad(self.usb_file as u8) }
                    else { PressResult::Cancel }
                }
                USB_ROW_LOADSLOT => {
                    if self.usb_file >= 0 && self.usb_slot >= 0 {
                        PressResult::UsbLoadToSlot {
                            file: self.usb_file as u8, slot: self.usb_slot as u8 }
                    } else { PressResult::Cancel }
                }
                _ => PressResult::Toggled,
            }
        } else if self.is_save_row() && self.mode == Mode::Edit {
            if self.save_cursor < 0 { PressResult::Cancel }
            else { PressResult::Commit(self.save_cursor as u8) }
        } else {
            PressResult::Toggled
        };
        self.mode = match self.mode {
            Mode::Nav => {
                if self.is_save_row() { self.save_cursor = -1; }
                if self.card == Card::Usb && self.focus == USB_ROW_LOADSLOT {
                    self.usb_slot = -1; // Cancel-first, same as Save
                }
                Mode::Edit
            }
            Mode::Edit => Mode::Nav,
        };
        result
    }
```

New display types + `build_frame` Usb arm (signature gains `usb: Option<&UsbInfo>`
between `lead_loaded` and `pos_x`):

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriveState { NoDrive, Ready, Busy }

pub struct UsbInfo<'a> {
    pub drive: DriveState,
    pub file_name: Option<&'a str>, // name of usb_file, if any
    pub file_count: u8,
    pub slot_name: Option<&'a str>, // name of the Load>Slot target slot
}
```

```rust
        Card::Usb => {
            let (drive, fname, count, sname) = match usb {
                Some(u) => (u.drive, u.file_name, u.file_count, u.slot_name),
                None => (DriveState::NoDrive, None, 0, None),
            };
            line.clear();
            let _ = match drive {
                DriveState::NoDrive => write!(line, "  Drive    No drive"),
                DriveState::Busy    => write!(line, "  Drive    BUSY"),
                DriveState::Ready   => write!(line, "  Drive    Ready ({} files)", count),
            };
            f.push(pos_x, pos_y + 2 * ROW_DY, false, &line);

            let marker = row_marker(st, USB_ROW_FILE);
            line.clear();
            if st.usb_file < 0 {
                let _ = write!(line, "{} File     -", marker);
            } else {
                let _ = write!(line, "{} File     {:03}  {}", marker, st.usb_file,
                               fname.unwrap_or("?"));
            }
            f.push(pos_x, pos_y + 3 * ROW_DY, st.focus == USB_ROW_FILE, &line);

            let marker = row_marker(st, USB_ROW_LOADSLOT);
            line.clear();
            if st.usb_slot < 0 {
                let _ = write!(line, "{} Load>Slot Cancel", marker);
            } else {
                let _ = write!(line, "{} Load>Slot U{:03}  {}", marker, st.usb_slot,
                               sname.unwrap_or("Empty"));
            }
            f.push(pos_x, pos_y + 4 * ROW_DY, st.focus == USB_ROW_LOADSLOT, &line);

            if let Some(s) = status {
                f.push(pos_x, pos_y + 7 * ROW_DY, true, s);
            }
        }
```

Main card gains (after the MidiSrc row, shifting the detail row to `7 * ROW_DY` and
status to `8 * ROW_DY` — update the affected y-assertions in existing tests):

```rust
            let marker = row_marker(st, MAIN_ROW_USBMODE);
            line.clear();
            let _ = write!(line, "{} USB Mode {}", marker,
                           if st.usb_storage { "Storage" } else { "MIDI" });
            f.push(pos_x, pos_y + 6 * ROW_DY, st.focus == MAIN_ROW_USBMODE, &line);
```

- [ ] **Step 4: Run tests**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib menu`
Expected: all PASS (existing + 8 new; fixed-up frame-count/y assertions included).

- [ ] **Step 5: Commit**

```bash
git add fw/src/menu.rs fw/src/frame.rs
git commit -m "feat(mbsid): menu USB Mode row + USB card (file browse, load, load-to-slot) (M6a §6d)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: `main.rs` wiring — mode CSR, mount-per-op, load dispatch

**Files:**
- Modify: `fw/src/main.rs`

**Interfaces:**
- Consumes: everything above. Key rule (M5 lesson, spec §6d): derive Usb-card
  visibility and drive state from live state **every loop iteration**, not from cached
  menu state.
- Produces: the working M6a firmware. All USB/FAT I/O in the main loop; the ISR is
  untouched by this task.

- [ ] **Step 1: Implement**

Additions to `main()` (after `let sid = peripherals.SID_PERIPH;`):

```rust
    // M6a: USB mass-storage. All access is main-loop-only; a slow drive
    // stalls UI redraw, never audio (the ISR keeps ticking the engine).
    let usb_msc = tiliqua_fw::usb_msc::UsbMsc::new(peripherals.USB_MSC);
    let mut usb_files: usb_patch::FileList = usb_patch::FileList::new();
    let mut usb_listed = false;
```

imports:

```rust
use tiliqua_fw::usb_patch;
use tiliqua_fw::fat::{FileSystem, FsOptions, MscStorage};
use tiliqua_fw::menu::{DriveState, UsbInfo};
```

after the settings load:

```rust
    state.usb_storage = settings.usb_mode == 1;
```

a mount-per-op helper above `main()` (mirrors sid_player_sw's `load_sid`/`list_sids`
free functions; a mount is a partition scan + BPB read — a handful of blocks):

```rust
/// Mount the drive's first FAT volume and run `f` on it. Every USB menu
/// action re-mounts (sid_player_sw idiom): no FileSystem lifetime to hold
/// across drive unplugs, and patch files are tiny so the cost is a few
/// 512-byte reads.
fn with_fat<R>(msc: &tiliqua_fw::usb_msc::UsbMsc,
               f: impl FnOnce(&FileSystem<MscStorage<&tiliqua_fw::usb_msc::UsbMsc>>) -> R)
               -> Option<R> {
    let storage = MscStorage::new(msc);
    match FileSystem::new(storage, FsOptions::new()) {
        Ok(fs) => Some(f(&fs)),
        Err(_) => None,
    }
}
```

in the main loop, right after the `lead_loaded` resync (derive-per-iteration block):

```rust
            // M6: derive USB state every iteration (M5 lesson). Leaving
            // Storage mode (or losing the drive) collapses the Usb card and
            // invalidates the cached file list.
            if !state.usb_storage && state.card == menu::Card::Usb {
                state.card = menu::Card::Main;
                state.focus = menu::ROW_CARD;
                dirty = true;
            }
            let drive_ready = state.usb_storage && usb_msc.ready()
                && usb_msc.block_size() == 512;
            if !drive_ready && usb_listed {
                usb_listed = false;           // drive unplugged / mode left
                usb_files.clear();
                state.usb_file = -1;
                state.usb_file_count = 0;
                if state.card == menu::Card::Usb { dirty = true; }
            }
            if drive_ready && !usb_listed && state.card == menu::Card::Usb {
                usb_files.clear();
                let n = with_fat(&usb_msc, |fs| {
                    usb_patch::list_patch_files(fs, &mut usb_files)
                }).unwrap_or(0);
                state.usb_file_count = n as u8;
                state.usb_file = if n > 0 { 0 } else { -1 };
                usb_listed = true;
                dirty = true;
            }
```

press dispatch — extend the `match state.on_press()`:

```rust
                    PressResult::UsbLoad(ix) => {
                        status = Some(usb_load(&usb_msc, ix as usize, None,
                                               &mut store, &mut patch_buf,
                                               &mut state, &mut user_detail));
                    }
                    PressResult::UsbLoadToSlot { file, slot } => {
                        status = Some(usb_load(&usb_msc, file as usize, Some(slot),
                                               &mut store, &mut patch_buf,
                                               &mut state, &mut user_detail));
                    }
```

with the helper (above `main()`, after `save_status`):

```rust
/// Load USB patch file `ix` into the engine (audition); optionally also
/// persist it to user-bank `slot`. Same engine entry as the SysEx path
/// (`load_patch`), so behavior is provably identical to a MIDI upload of
/// the same bytes (spec §6e).
fn usb_load<F: tiliqua_hal::nor_flash::NorFlash + tiliqua_hal::nor_flash::ReadNorFlash>(
    msc: &tiliqua_fw::usb_msc::UsbMsc,
    ix: usize,
    slot: Option<u8>,
    store: &mut UserPatchStore<F>,
    patch_buf: &mut [u8; 512],
    state: &mut MenuState,
    user_detail: &mut Option<(u8, u8)>,
) -> heapless::String<24> {
    let mut s = heapless::String::new();
    let ok = with_fat(msc, |fs| {
        usb_patch::load_patch_by_index(fs, ix, patch_buf)
    }).unwrap_or(false);
    if !ok {
        let _ = core::fmt::Write::write_str(&mut s, "USB load FAILED");
        return s;
    }
    critical_section::with(|_cs| {
        mbsid_sys::load_patch(patch_buf);
    });
    *user_detail = Some((patch_buf[0x10], patch_buf[0x50]));
    state.refresh_params(|a| mbsid_sys::patch_byte(a));
    state.edited = false;
    match slot {
        Some(n) => {
            if store.save(n, patch_buf).is_ok() {
                let _ = core::fmt::Write::write_fmt(&mut s,
                    format_args!("Loaded -> U{:03}", n));
            } else {
                let _ = core::fmt::Write::write_fmt(&mut s,
                    format_args!("Save FAILED U{:03}", n));
            }
        }
        None => { let _ = core::fmt::Write::write_str(&mut s, "Loaded (audition)"); }
    }
    s
}
```

settings persist — the `Settings` literal in the debounced-save block gains:

```rust
                        usb_mode: state.usb_storage as u8,
```

redraw block — mirror the mode bit and force-TRS while in Storage (drive needs the PHY;
TRS keeps working so you can play while browsing — spec §3), and build the UsbInfo:

```rust
                sid.usb_midi_host().write(|w| unsafe {
                    w.host().bit(state.midi_src == menu::MidiSource::Usb
                                 && !state.usb_storage)
                });
                usb_msc.set_mode(state.usb_storage);
```

```rust
                let usb_info = if state.card == menu::Card::Usb {
                    let mut slotbuf = [0u8; 16];
                    let slot_name: Option<&str> =
                        if state.usb_slot >= 0
                            && store.name(state.usb_slot as u8, &mut slotbuf) {
                            core::str::from_utf8(&slotbuf).ok()
                        } else { None };
                    Some(UsbInfo {
                        drive: if drive_ready { DriveState::Ready }
                               else { DriveState::NoDrive },
                        file_name: if state.usb_file >= 0 {
                            usb_files.get(state.usb_file as usize)
                                     .map(|n| n.as_str())
                        } else { None },
                        file_count: state.usb_file_count,
                        slot_name,
                    })
                } else { None };
                let frame = menu::build_frame(&state, name, detail,
                                              save_name, status.as_deref(), lead_loaded,
                                              usb_info.as_ref(), MENU_X, MENU_Y);
```

(Borrow note: `usb_info` borrows `slotbuf`/`usb_files`; declare `slotbuf` in the same
scope so lifetimes line up — the whole block lives inside `if dirty { ... }`.)

Also: while the Usb card is focused, redraw on drive state changes — the
`usb_listed` transitions above already set `dirty`.

- [ ] **Step 2: Host tests still green**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib`
Expected: PASS (main.rs is not host-compiled, but menu/API changes must not break libs).

- [ ] **Step 3: Target build**

Run: `cd gateware && pdm mbsid build --fw-only`
Expected: ELF builds; expected `missing top.bit` tail. Then check RAM:
`llvm-size -A fw/target/riscv32im-unknown-none-elf/release/tiliqua-fw | grep -E '\.bss|\.stack|\.text'`
Expected: `.bss` grows by roughly the FileList (~1 KB) + fatfs statics; the `.stack`
*section* number is just the linker leftover — do NOT read it as usage (root CLAUDE.md).

- [ ] **Step 4: Commit**

```bash
git add fw/src/main.rs
git commit -m "feat(mbsid): wire USB storage into main loop — mode CSR, file list, load/load-to-slot (M6a §6e)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 10: M6a checkpoint — full build, docs, HW checklist

**Files:**
- Modify: `CLAUDE.md` (this dir), `docs/` user guide + developer guide,
  `M6_USB_STORAGE.md` (status line), `DESIGN.md` (milestone table).

- [ ] **Step 1: Full bitstream build + metrics**

```bash
cd gateware && pdm mbsid build
```
Expected: completes; record post-route sync Fmax (second `Max frequency` line in
`build/mbsid-r5/top.tim`) — must still be ≥ 60 MHz — and LUT%.

- [ ] **Step 2: Full host test run + count reconcile**

```bash
cd fw && cargo test --target x86_64-unknown-linux-gnu --lib 2>&1 | tail -3
grep -rn "cargo test --lib\|tests:" ../CLAUDE.md ../docs/developer-guide.md
```
Reconcile every quoted count against the fresh run (doc-count-drift gotcha).

- [ ] **Step 3: Documentation**

- `CLAUDE.md` (this dir): add an M6a gotcha block — usb_msc CSR at `0x1300` (`mode` bit
  0x1C, PAC-regen rule), the UTMI-mux/Option-A shape, "Storage mode forces TRS MIDI +
  VBUS always on with `with_usb_msc`", mount-per-op idiom, and update the status line.
- `docs/` user guide: USB Mode row, patch load walkthrough, drive format requirements
  (FAT32/MBR/first partition; `/MBSID/` dir or root), "modes are exclusive at the
  connector".
- `M6_USB_STORAGE.md`: Status → "M6a implemented (hardware bring-up pending); M6b spec".
- `DESIGN.md`: add M6 to the milestone table.

- [ ] **Step 4: Hardware checklist (record in `M6_USB_STORAGE.md`, execute when HW available)**

From spec §7 M6a: drive enumerates (Drive row shows Ready+count); files listed; load
auditions correctly (same patch via TRS SysEx must sound identical); Load→Slot persists
across power cycle; unplug mid-browse degrades to `No drive` (no hang); mode switch back
to MIDI re-enumerates a keyboard; TRS MIDI keeps playing in Storage mode; stack-paint
re-measure (§6f).

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md docs/ M6_USB_STORAGE.md DESIGN.md
git commit -m "docs(mbsid): M6a USB patch load — status, user guide, HW checklist

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 11: M6b gateware — vendor the MSC engine, add SCSI WRITE(10) + bulk-OUT TX

**Files:**
- Create: `src/vendor/guh_msc/__init__.py` (empty)
- Create: `src/vendor/guh_msc/msc.py` (vendored from
  `.venv/lib/python3.13/site-packages/guh/engines/msc.py`, BSD-3 header + provenance
  comment kept: upstream repo + pinned rev `d44315` + "diverges: write support, M6b")
- Modify: `src/top/sid/top.py` (one import swap, `with_usb_msc` path only)
- Test: `gateware/tests/test_guh_msc_write.py` (new)

**Interfaces:**
- Consumes: `guh.usbh.*` internals (stay upstream-pinned — only `engines/msc.py` is
  vendored, spec §2).
- Produces (used by Task 12):
  - `vendor.guh_msc.msc.USBMSCHost` — drop-in superset of upstream:
    - `SCSIOpCode.WRITE_10 = 0x2A`
    - `USBMSCHost.Command` gains `write: unsigned(1)` (start+write=1 → block write)
    - new port `tx_data: In(stream.Signature(unsigned(8)))` — exactly
      `status.block_size` bytes consumed per write command
    - `SCSIBulkHost.Command` gains `data_dir: unsigned(1)` (0=IN, 1=OUT);
      `bmCBWFlags` derives from it, not from `data_len > 0`
  - Testability seam: `SCSIBulkHost.__init__(*, enumerator=None, **kwargs)` — tests
    inject a stub enumerator exposing `.ctrl` (an `USBSIEInterface()` flipped),
    `.status.enumerated/.dev_addr`, `.parser.o.{valid,i_endp.number,o_endp.number}`.

- [ ] **Step 1: Vendor the file**

```bash
mkdir -p src/vendor/guh_msc
touch src/vendor/guh_msc/__init__.py
cp .venv/lib/python3.13/site-packages/guh/engines/msc.py src/vendor/guh_msc/msc.py
```
Add below the SPDX line:

```python
# Vendored from guh @ d44315 (github upstream) — see gateware/pyproject.toml pin.
# Diverges from upstream: SCSI WRITE(10) + bulk-OUT data phase (M6b,
# src/top/mbsid/M6_USB_STORAGE.md §4b). Candidate for an upstream PR.
```

- [ ] **Step 2: Investigation gate — SIE TX packet size** *(do this before writing FSM code)*

The SIE tx FIFO is 64 deep (`guh/usbh/sie.py:339` `self.fifo_depth = 64  # Max USB
packet size`). Read `USBSIE.elaborate`'s OUT-transfer path to answer: **can one bulk
OUT transaction carry 512 bytes** (txs fed concurrently while transmitting), or is a
packet hard-capped at 64 bytes (FIFO preloaded, then sent)?

- If 512 works (streaming feed): implement DATA-TX as one 512-byte OUT transaction per
  block. Preferred.
- If capped at 64: implement DATA-TX as 8 × 64-byte OUT transactions per 512-byte
  block (PID toggling per ACK, retry-on-NAK per packet). Most MSC devices accept
  non-max-size OUT packets mid-transfer since the CBW announces total length, but this
  is out-of-spec-ish — record it as an HW-checklist risk ("test a cheap stick + an SSD
  enclosure", spec §8) and prefer proposing an SIE `fifo_depth` parameter upstream
  later.
Document the finding in a comment at the DATA-TX state.

- [ ] **Step 3: Implement the engine changes**

In `src/vendor/guh_msc/msc.py`:

```python
class SCSIOpCode(enum.Enum, shape=unsigned(8)):
    TEST_UNIT_READY  = 0x00
    REQUEST_SENSE    = 0x03
    READ_CAPACITY_10 = 0x25
    READ_10          = 0x28
    WRITE_10         = 0x2A
```

`SCSIBulkHost.Command` gains `data_dir: unsigned(1)` (after `stream_data`); the CBW
builder becomes:

```python
            cbw_sig.bmCBWFlags.eq(Mux(self.cmd.data_dir,
                                      CBWFlags.DATA_OUT, CBWFlags.DATA_IN)),
```

(with `data_len == 0` commands keeping `data_dir=0` → flags byte is DATA_IN(0x80)?
No: upstream used DATA_OUT(0x00) for zero-length. Preserve that exactly:
`Mux(self.cmd.data_len > 0, Mux(self.cmd.data_dir, CBWFlags.DATA_OUT, CBWFlags.DATA_IN), CBWFlags.DATA_OUT)`.)

New port + injection seam:

```python
    tx_data: In(stream.Signature(unsigned(8)))

    def __init__(self, *, enumerator=None, **kwargs):
        self.enumerator = enumerator if enumerator is not None else \
            USBHostEnumerator(
                **kwargs,
                config_number=1,
                parser=USBDescriptorParser(...unchanged...),
            )
        super().__init__()
```

`CBW-WAIT`'s ACK case routes on direction:

```python
                            with m.If(data_len > 0):
                                with m.If(data_dir_r):
                                    m.next = "DATA-TX-START"
                                with m.Else():
                                    m.next = "DATA"
```

(`data_dir_r` latched in IDLE alongside `data_len`/`stream_mode`.) New states —
one OUT transaction per `CHUNK` bytes, where `CHUNK` is a module-level constant set by
Step 2's finding (`_TX_CHUNK_BYTES = 512` if the SIE streams, `64` if hard-capped):

```python
            tx_sent = Signal(16)   # bytes handed to the SIE this transaction
            tx_total = Signal(16)  # bytes ACKed so far this data phase

            with m.State("DATA-TX-START"):
                m.d.usb += tx_sent.eq(0)
                m.next = "DATA-TX-LOAD"

            with m.State("DATA-TX-LOAD"):
                # Stream payload bytes into the SIE tx FIFO (CBW-LOAD pattern).
                m.d.comb += [
                    enum.ctrl.txs.valid.eq(self.tx_data.valid),
                    enum.ctrl.txs.payload.eq(self.tx_data.payload),
                    self.tx_data.ready.eq(enum.ctrl.txs.ready),
                ]
                with m.If(self.tx_data.valid & enum.ctrl.txs.ready):
                    m.d.usb += tx_sent.eq(tx_sent + 1)
                    with m.If(tx_sent == CHUNK - 1):
                        m.next = "DATA-TX-XFER"

            with m.State("DATA-TX-XFER"):
                with m.If(enum.ctrl.status.idle):
                    m.d.comb += start_bulk_out(endp_out)
                    m.next = "DATA-TX-WAIT"

            with m.State("DATA-TX-WAIT"):
                with m.If(enum.ctrl.status.idle):
                    with m.Switch(enum.ctrl.status.response):
                        with m.Case(TransferResponse.ACK):
                            m.d.usb += [
                                pid_out.eq(Mux(pid_out, DataPID.DATA0, DataPID.DATA1)),
                                tx_total.eq(tx_total + CHUNK),
                            ]
                            with m.If(tx_total + CHUNK >= data_len):
                                m.next = "CSW"
                            with m.Else():
                                m.next = "DATA-TX-START"
                        with m.Case(TransferResponse.NAK):
                            # Same data, same PID: retransmit the chunk. The
                            # SIE consumed its FIFO, so reload — requires the
                            # payload source to replay; the peripheral's word
                            # FIFO does NOT replay, so NAK handling re-issues
                            # the transfer from the SIE's still-loaded FIFO if
                            # the SIE preserves it on NAK — VERIFY in Step 2;
                            # if it doesn't, add a 512-byte replay buffer here
                            # (Memory, usb domain) filled in DATA-TX-LOAD.
                            m.next = "DATA-TX-XFER"
```

**NAK-replay is a correctness decision the implementer must settle in sim** (Step 5's
test drives a NAK): if `USBSIE` drains its FIFO on a NAKed OUT, add the local replay
`Memory`; the interface above doesn't change either way. (`tx_total` reset to 0 in
IDLE.)

`USBMSCHost`: `Command` gains `write: unsigned(1)`; `tx_data: In(stream.Signature(unsigned(8)))`
forwarded with `wiring.connect(m, wiring.flipped(self.tx_data), scsi.tx_data)`;
declare `is_write = Signal()` next to `current_lba`; `READY` dispatches:

```python
            with m.State("READY"):
                m.d.comb += self.status.busy.eq(0)
                with m.If(self.cmd.start):
                    m.d.usb += [current_lba.eq(self.cmd.lba),
                                is_write.eq(self.cmd.write)]
                    with m.If(self.cmd.write):
                        m.next = "WRITE"
                    with m.Else():
                        m.next = "READ"
```

with `WRITE`/`WRITE-WAIT` mirroring `READ`/`READ-WAIT` (`_BLOCKS_PER_READ = 1` reused —
one block per command, spec §4b):

```python
            def write_cdb():
                return [
                    cdb10.opcode.eq(SCSIOpCode.WRITE_10),
                    cdb10.lba_be.eq(Cat(current_lba[24:32], current_lba[16:24],
                                        current_lba[8:16], current_lba[0:8])),
                    cdb10.xfer_len_be.eq(Cat(Const(0, 8), Const(1, 8))),
                    scsi_cmd.data_len.eq(block_size),
                    scsi_cmd.data_dir.eq(1),
                    scsi_cmd.stream_data.eq(0),
                ]

            with m.State("WRITE"):
                m.d.comb += write_cdb() + [scsi_cmd.start.eq(1)]
                m.next = "WRITE-WAIT"

            with m.State("WRITE-WAIT"):
                m.d.comb += write_cdb()
                with m.If(scsi.status.done):
                    with m.If(~scsi.status.error & ~scsi.status.rejected):
                        m.d.usb += watchdog.eq(0)
                    m.d.comb += [
                        self.resp.done.eq(1),
                        self.resp.error.eq(scsi.status.error | scsi.status.rejected),
                    ]
                    m.next = "READY"
```

Existing `READ` path: set `scsi_cmd.data_dir.eq(0)` explicitly (it defaults 0 — add it
anyway for self-documentation).

- [ ] **Step 4: Point the opted-in top at the vendored engine**

In `src/top/sid/top.py`'s `with_usb_msc` branch (Task 3 Step 2), change

```python
                from guh.engines.msc import USBMSCHost
```
to
```python
                from vendor.guh_msc.msc import USBMSCHost
```
(`src/` is on sys.path — same mechanism as `src/vendor/vexiiriscv/`; verify with the
Step 6 elaboration check.)

- [ ] **Step 5: Sim test with a stub enumerator**

Create `gateware/tests/test_guh_msc_write.py`. Stub shape:

```python
import unittest
from amaranth import *
from amaranth.lib import wiring
from amaranth.lib.wiring import In, Out
from amaranth.sim import Simulator

from guh.usbh.sie import USBSIEInterface, TransferType, TransferResponse
from vendor.guh_msc.msc import SCSIBulkHost, SCSIOpCode, CBW_SIZE_BYTES


class StubEnumerator(wiring.Component):
    """Just enough surface for SCSIBulkHost: a driven-from-testbench SIE
    interface + always-enumerated status + fixed endpoint numbers."""
    ctrl: Out(USBSIEInterface())   # flip so the testbench drives responses

    def __init__(self):
        super().__init__()
        # plain attributes, not ports: SCSIBulkHost reads them structurally
        class _O: pass
        self.status = _O(); self.status.enumerated = Signal(init=1)
        self.status.dev_addr = Signal(7, init=0x12)
        self.parser = _O(); self.parser.o = _O()
        self.parser.o.valid = Signal(init=1)
        class _EP: pass
        self.parser.o.i_endp = _EP(); self.parser.o.i_endp.number = Signal(4, init=1)
        self.parser.o.o_endp = _EP(); self.parser.o.o_endp.number = Signal(4, init=2)

    def elaborate(self, platform):
        return Module()
```

(Adjust to the real `USBSIEInterface`/status field names — the point of the seam is
that the testbench plays the SIE: capture `ctrl.txs` bytes while asserting
`txs.ready`, assert `ctrl.status.idle`, and respond ACK/NAK on `ctrl.status.response`
after each `ctrl.xfer.start`. Everything runs in a renamed `usb` domain:
`DomainRenamer("usb")` is unnecessary if the sim adds a clock named "usb" —
`sim.add_clock(1/60e6, domain="usb")`.)

Assertions (spec §4b):
1. Issuing a write command (`cmd.start=1, data_dir=1, data_len=512,
   cdb10.opcode=WRITE_10, lba=0x11223344`) emits a 31-byte CBW on `ctrl.txs` whose
   flags byte (offset 12) is `0x00`, opcode byte (offset 15) is `0x2A`, and LBA bytes
   (offsets 17–20) are big-endian `11 22 33 44`.
2. After the testbench ACKs the CBW OUT transfer, the DUT streams exactly 512 payload
   bytes on `ctrl.txs` (feed a counting pattern into `tx_data`), then starts an OUT
   transfer (`ctrl.xfer.type == OUT`, ep 2), with `pid_out` toggling across chunks.
3. NAK on a data OUT transfer → the same chunk is re-issued with the same PID.
4. After the data phase, the DUT issues a bulk IN (CSW); feed a passing 13-byte CSW →
   `status.done=1, status.error=0`. Feed `bCSWStatus=1` in a second test →
   `status.error=1`.
5. Regression: a READ command (`data_dir=0`) still emits flags `0x80` — the read path
   is untouched.

- [ ] **Step 6: Run tests + elaboration check**

```bash
cd gateware
pdm run pytest tests/test_guh_msc_write.py -v
pdm mbsid build --pac-only        # opted-in top elaborates with vendored engine
pdm test                          # nothing else regressed
```
Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add src/vendor/guh_msc/ src/top/sid/top.py gateware/tests/test_guh_msc_write.py
git commit -m "feat(mbsid): vendor guh MSC engine + SCSI WRITE(10)/bulk-OUT data phase (M6b §4b)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 12: M6b CSR — TX word FIFO + start_write + done bit

**Files:**
- Modify: `src/tiliqua/usb_msc_csr.py` (opt-in `with_write=False`)
- Modify: `src/top/sid/top.py` (wire tx path in the `with_usb_msc` branch;
  instantiate `USBMSCPeripheral(with_mode=True, with_write=True)`)
- Test: extend `gateware/tests/test_usb_msc_csr.py`

**Interfaces:**
- Produces (used by Task 13):
  - CSRs (only when `with_write=True`): `tx_data` (W, 32-bit) at **0x20** — pushes one
    little-endian word into a 128-deep TX word FIFO; `start_write` (W strobe) at
    **0x24** — legal only when the FIFO holds exactly 128 words; resets the byte
    unpacker + sticky resp and pulses `start_write_o`. `resp` gains a `done` field
    (bit 1): sticky, set on `resp_i.done`, cleared by `start` OR `start_write`.
  - Ports: `tx_data_o: Out(stream.Signature(unsigned(8)))` (512 bytes per command,
    drained by the engine), `start_write_o: Out(1)`.
  - sid_player_sw (`with_write=False` default): unchanged map, unchanged elaboration.
  - Firmware contract: write `lba`, push 128 words to `tx_data`, strobe `start_write`,
    poll `resp.done`; `resp.error` then distinguishes success/failure.

- [ ] **Step 1: Write the failing tests** (extend `test_usb_msc_csr.py`)

```python
    def test_tx_words_unpack_to_byte_stream(self):
        dut = USBMSCPeripheral(with_mode=True, with_write=True)
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            # Push one word via CSR (4 byte-writes, little-endian bus).
            for i, b in enumerate([0x11, 0x22, 0x33, 0x44]):
                await csr_write(ctx, dut, 0x20 + i, b)
            # Byte stream must yield 11 22 33 44 in order.
            ctx.set(dut.tx_data_o.ready, 1)
            got = []
            for _ in range(32):
                await ctx.tick()
                if ctx.get(dut.tx_data_o.valid):
                    got.append(ctx.get(dut.tx_data_o.payload))
                if len(got) == 4:
                    break
            self.assertEqual(got, [0x11, 0x22, 0x33, 0x44])

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()

    def test_start_write_strobes_and_clears_sticky_resp(self):
        dut = USBMSCPeripheral(with_mode=True, with_write=True)
        m = Module()
        m.submodules.dut = dut

        async def csr_read(ctx, offset):
            ctx.set(dut.bus.addr, offset)
            ctx.set(dut.bus.r_stb, 1)
            await ctx.tick()
            ctx.set(dut.bus.r_stb, 0)
            return ctx.get(dut.bus.r_data)

        async def testbench(ctx):
            # Latch done+error via resp_i.
            ctx.set(dut.resp_i.done, 1)
            ctx.set(dut.resp_i.error, 1)
            await ctx.tick()
            ctx.set(dut.resp_i.done, 0)
            ctx.set(dut.resp_i.error, 0)
            await ctx.tick()
            # resp @0x18: bit0=error, bit1=done — both sticky-set.
            self.assertEqual(await csr_read(ctx, 0x18) & 0b11, 0b11)
            await csr_write(ctx, dut, 0x24, 1)   # start_write strobe
            await ctx.tick()
            # sticky bits cleared by the strobe.
            self.assertEqual(await csr_read(ctx, 0x18) & 0b11, 0b00)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()
```

(If the amaranth-soc CSR bus needs an extra settle tick between `r_stb` and a valid
`r_data`, follow `tests/test_usb_msc_sw_periph.py`'s existing bus-poking idiom.)

Also assert `USBMSCPeripheral()` (no flags) has no `tx_data`/`start_write` resources.

- [ ] **Step 2: Run to verify failure**

Run: `cd gateware && pdm run pytest tests/test_usb_msc_csr.py -v`
Expected: FAIL — unexpected keyword `with_write` / missing `tx_data_o`.

- [ ] **Step 3: Implement in `usb_msc_csr.py`**

Constructor: `with_write=False`; when set, build `self._tx_fifo =
SyncFIFOBuffered(width=32, depth=128)` and add:

```python
    class TxData(csr.Register, access="w"):
        """One little-endian 32-bit word of write-payload. Push exactly 128
        words (512 B), then strobe start_write."""
        word: csr.Field(csr.action.W, unsigned(32))

    class StartWrite(csr.Register, access="w"):
        strobe: csr.Field(csr.action.W, unsigned(1))

    class RespW(csr.Register, access="r"):
        error: csr.Field(csr.action.R, unsigned(1))
        done:  csr.Field(csr.action.R, unsigned(1))
```

Register `resp` as `RespW()` when `with_write` else the existing `Resp()` (same offset
0x18; `done` reads as a new bit — only opted-in PACs see it). `tx_data` at 0x20,
`start_write` at 0x24. Ports (class-level, always present; inert when off):

```python
    tx_data_o:     Out(stream.Signature(unsigned(8)))
    start_write_o: Out(1)
```

Elaborate additions (guarded by `if self._with_write:`):

```python
            m.submodules.tx_fifo = txf = self._tx_fifo
            start_write = (self._start_write.f.strobe.w_stb
                           & self._start_write.f.strobe.w_data)
            m.d.comb += [
                txf.w_en.eq(self._tx_data.f.word.w_stb),
                txf.w_data.eq(self._tx_data.f.word.w_data),
                self.start_write_o.eq(start_write),
            ]
            # word -> byte unpacker (mirror of the RX byte->word packer)
            tx_ix = Signal(2)
            m.d.comb += [
                self.tx_data_o.valid.eq(txf.r_rdy),
                self.tx_data_o.payload.eq(
                    txf.r_data.word_select(tx_ix, 8)),
                txf.r_en.eq(0),
            ]
            with m.If(self.tx_data_o.valid & self.tx_data_o.ready):
                m.d.sync += tx_ix.eq(tx_ix + 1)
                with m.If(tx_ix == 3):
                    m.d.comb += txf.r_en.eq(1)
            with m.If(start_write):
                m.d.sync += tx_ix.eq(0)
            # sticky done (with_write resp variant)
            resp_done_r = Signal()
            with m.If(self.resp_i.done):
                m.d.sync += resp_done_r.eq(1)
            with m.If(start_strobe | start_write):
                m.d.sync += [resp_done_r.eq(0), resp_error_r.eq(0)]
            m.d.comb += self._resp.f.done.r_data.eq(resp_done_r)
```

(The existing `resp_error_r` logic already clears on `start_strobe`; extend its clear
term rather than duplicating the register. NOTE the CSR data bus is 8-bit — a 32-bit
W field is written as 4 byte-lane writes and `w_stb` fires once on the last lane;
that is exactly how the existing `Lba` register works, so `txf.w_en` on `w_stb` is
correct.)

- [ ] **Step 4: Wire in the top + PAC**

In the `with_usb_msc` branch of `src/top/sid/top.py`:

```python
                self.usb_msc = USBMSCPeripheral(with_mode=True, with_write=True)
                # ... existing wiring ... plus:
                wiring.connect(m, self.usb_msc.tx_data_o, msc.tx_data)
                m.d.comb += msc.cmd.write.eq(write_pending)
```

where `write_pending` is a sync register: set by `start_write_o`, cleared by
`start_o`, so `cmd.start` can be shared:

```python
                write_pending = Signal()
                with m.If(self.usb_msc.start_write_o):
                    m.d.sync += write_pending.eq(1)
                with m.If(self.usb_msc.start_o):
                    m.d.sync += write_pending.eq(0)
                m.d.comb += msc.cmd.start.eq(
                    self.usb_msc.start_o | self.usb_msc.start_write_o)
```

(`cmd.write` must be valid ON the start cycle: use
`msc.cmd.write.eq(self.usb_msc.start_write_o | write_pending)` — the strobe cycle
itself carries write=1.)
Move the peripheral construction accordingly (it's in `__init__`; pass both flags there).

```bash
pdm mbsid build --pac-only    # resp.done / tx_data / start_write in the PAC
pdm sid_player_sw build --pac-only   # unchanged map (no mode/tx_data/done)
```

- [ ] **Step 5: Run tests**

Run: `cd gateware && pdm run pytest tests/test_usb_msc_csr.py tests/test_usb_msc_sw_periph.py -v`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/tiliqua/usb_msc_csr.py src/top/sid/top.py gateware/tests/test_usb_msc_csr.py
git commit -m "feat(mbsid): usb_msc CSR write path — TX word FIFO, start_write, sticky done (M6b §4b)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 13: M6b firmware — `write_block` + FAT write-back cache

**Files:**
- Modify: `fw/src/usb_msc.rs`
- Modify: `fw/src/fat.rs`

**Interfaces:**
- Produces (used by Task 14):
  - `UsbMsc::write_block(&self, lba: u32, buf: &[u8; 512]) -> Result<(), MscError>`
  - `fat::BlockIo` gains `fn write_block(&mut self, lba: u32, buf: &[u8; 512])
    -> Result<(), ()>;`
  - `MscStorage`: `Write::write` does read-modify-write on the sector cache;
    `flush()` and cache eviction write back a dirty sector. Single-sector cache kept
    (patch files are tiny — spec §6a).

- [ ] **Step 1: Write the failing host tests** (in `fat.rs`'s new `#[cfg(test)]`)

```rust
    /// In-memory BlockIo over a byte vec; counts writes for flush assertions.
    struct MemDisk { data: std::vec::Vec<u8>, writes: usize }
    impl crate::fat::BlockIo for &mut MemDisk {
        fn read_block(&mut self, lba: u32, buf: &mut [u8; 512]) -> Result<(), ()> {
            let o = lba as usize * 512;
            if o + 512 > self.data.len() { return Err(()); }
            buf.copy_from_slice(&self.data[o..o + 512]);
            Ok(())
        }
        fn write_block(&mut self, lba: u32, buf: &[u8; 512]) -> Result<(), ()> {
            let o = lba as usize * 512;
            if o + 512 > self.data.len() { return Err(()); }
            self.data[o..o + 512].copy_from_slice(buf);
            self.writes += 1;
            Ok(())
        }
    }

    #[test]
    fn write_rmw_lands_after_flush() {
        let mut disk = MemDisk { data: std::vec![0xAAu8; 8 * 512], writes: 0 };
        // superfloppy layout (no partition table) -> base_lba 0 fallback is
        // fine: we drive MscStorage directly, not through fatfs here.
        {
            let mut s = MscStorage::new(&mut disk);
            use fatfs::{Seek, SeekFrom, Write};
            s.seek(SeekFrom::Start(512 + 5)).unwrap();
            s.write(&[1, 2, 3]).unwrap();
            s.flush().unwrap();
        }
        assert_eq!(disk.writes, 1);
        assert_eq!(&disk.data[512 + 5..512 + 8], &[1, 2, 3]);
        assert_eq!(disk.data[512 + 4], 0xAA); // RMW preserved neighbors
    }

    #[test]
    fn crossing_sector_boundary_writes_back_dirty_sector() {
        let mut disk = MemDisk { data: std::vec![0u8; 8 * 512], writes: 0 };
        {
            let mut s = MscStorage::new(&mut disk);
            use fatfs::{Seek, SeekFrom, Write};
            s.seek(SeekFrom::Start(510)).unwrap();
            s.write(&[9; 4]).unwrap(); // spans sectors 0 and 1
            s.flush().unwrap();
        }
        assert_eq!(&disk.data[510..514], &[9, 9, 9, 9]);
        assert!(disk.writes >= 2);
    }

    #[test]
    fn read_of_other_sector_evicts_dirty_cache_first() {
        let mut disk = MemDisk { data: std::vec![0u8; 8 * 512], writes: 0 };
        {
            let mut s = MscStorage::new(&mut disk);
            use fatfs::{Read, Seek, SeekFrom, Write};
            s.seek(SeekFrom::Start(0)).unwrap();
            s.write(&[7; 8]).unwrap();
            s.seek(SeekFrom::Start(3 * 512)).unwrap();
            let mut b = [0u8; 4];
            s.read(&mut b).unwrap();   // must not lose sector 0's dirty data
        }
        assert_eq!(&disk.data[0..8], &[7; 8]);
    }
```

(Drop the `disk_writes_via` placeholder — assert only post-scope; borrow rules make
mid-scope inspection awkward. Keep the three end-state assertions.)

- [ ] **Step 2: Run to verify failure**

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib fat`
Expected: compile FAIL — `write_block` not a member of `BlockIo`.

- [ ] **Step 3: Implement**

`fat.rs`:

```rust
pub trait BlockIo {
    fn read_block(&mut self, lba: u32, buf: &mut [u8; 512]) -> Result<(), ()>;
    fn write_block(&mut self, lba: u32, buf: &[u8; 512]) -> Result<(), ()>;
}
```

`MscStorage` gains `dirty: bool` (init false);

```rust
    fn flush_cache(&mut self) -> Result<(), StorageError> {
        if self.dirty {
            let lba = self.cache_lba.ok_or(StorageError)?;
            self.io.write_block(lba, &self.cache).map_err(|_| StorageError)?;
            self.dirty = false;
        }
        Ok(())
    }

    fn ensure_block(&mut self, lba: u32) -> Result<(), StorageError> {
        if self.cache_lba != Some(lba) {
            self.flush_cache()?;           // evict dirty sector first
            let mut buf = [0u8; 512];
            self.io.read_block(lba, &mut buf).map_err(|_| StorageError)?;
            self.cache = buf;
            self.cache_lba = Some(lba);
        }
        Ok(())
    }
```

```rust
impl<B: BlockIo> Write for MscStorage<B> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() { return Ok(0); }
        let lba = self.base_lba + (self.pos / 512) as u32;
        let off = (self.pos % 512) as usize;
        self.ensure_block(lba)?;           // RMW: sector loaded first
        let n = (512 - off).min(buf.len());
        self.cache[off..off + n].copy_from_slice(&buf[..n]);
        self.dirty = true;
        self.pos += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> Result<(), Self::Error> { self.flush_cache() }
}
```

`usb_msc.rs`:

```rust
    /// Write one 512-byte block at `lba`. Same block_size()==512 precondition
    /// as read_block. Contract (Task 12): lba -> 128 tx words -> start_write
    /// -> poll sticky resp.done, then resp.error.
    pub fn write_block(&self, lba: u32, buf: &[u8; 512]) -> Result<(), MscError> {
        if !self.ready() { return Err(MscError::NotReady); }
        self.regs.lba().write(|w| unsafe { w.value().bits(lba) });
        for i in 0..128usize {
            let w32 = u32::from_le_bytes(buf[i * 4..i * 4 + 4].try_into().unwrap());
            self.regs.tx_data().write(|w| unsafe { w.word().bits(w32) });
        }
        self.regs.start_write().write(|w| w.strobe().set_bit());
        const MAX_SPIN: u32 = 10_000_000; // writes can be slower than reads
        let mut spins: u32 = 0;
        loop {
            let r = self.regs.resp().read();
            if r.done().bit_is_set() {
                return if r.error().bit_is_set() {
                    Err(MscError::WriteError)
                } else { Ok(()) };
            }
            spins += 1;
            if spins >= MAX_SPIN { return Err(MscError::WriteError); }
        }
    }
```

(`MscError` gains `WriteError`; `BlockIo for &UsbMsc` gains the write_block
forwarder. NOTE `&UsbMsc` methods take `&self` — the trait takes `&mut self`, the
forwarder just calls through.)

- [ ] **Step 4: Run tests + target build**

```bash
cd fw && cargo test --target x86_64-unknown-linux-gnu --lib
cd ../../../.. && cd gateware && pdm mbsid build --fw-only
```
Expected: host PASS (+3), target ELF builds (`missing top.bit` tail).

- [ ] **Step 5: Commit**

```bash
git add fw/src/usb_msc.rs fw/src/fat.rs
git commit -m "feat(mbsid): MSC write_block + FAT write-back sector cache (M6b §6a)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 14: M6b — `export_syx` + Export menu row + main-loop dispatch

**Files:**
- Modify: `fw/src/usb_patch.rs`
- Modify: `fw/src/menu.rs`
- Modify: `fw/src/main.rs`

**Interfaces:**
- Produces:
  - `usb_patch::encode_syx(patch: &[u8; 512], slot: u8, out: &mut [u8; 1036])` — pure;
    same framing as Task 1's script (bank byte = 1).
  - `usb_patch::export_patch<IO: ReadWriteSeek>(fs: &FileSystem<IO>, name: &str,
    patch: &[u8; 512], slot: u8) -> bool` — creates `/MBSID/` if absent, writes
    `<name>` (8.3: `PNNN.SYX` / `EDIT.SYX`), flushes, then **verifies by re-reading
    and re-parsing** (spec §6b).
  - Menu: `USB_ROW_EXPORT: u8 = 3` (Usb row_count → 4); state field
    `usb_export: i16` (-1 = Cancel, 0 = EDIT buffer, 1..=128 = slot n-1);
    `PressResult::UsbExport { source: ExportSource }` with
    `pub enum ExportSource { Edit, Slot(u8) }`.

- [ ] **Step 1: Failing tests — usb_patch export round-trip**

```rust
    #[test]
    fn encode_syx_roundtrips_through_file_parser() {
        let p = test_patch(9);
        let mut syx = [0u8; 1036];
        encode_syx(&p, 5, &mut syx);
        let mut dst = [0u8; 512];
        assert!(parse_patch_file(&syx, &mut dst));
        assert_eq!(dst, p);
        assert_eq!(syx[8], 0x01); // bank byte 1 = User (re-sendable over MIDI)
        assert_eq!(syx[9], 5);    // slot
    }

    #[test]
    fn export_then_reimport_is_byte_identical() {
        let p = test_patch(10);
        let mut img = build_gpt_fat_image(&[("SEED.TXT", b"x")]);
        let base = BASE_LBA as usize * SECTOR;
        {
            let part = VecDisk::new(&mut img[base..]);
            let fs = FileSystem::new(part, FsOptions::new()).unwrap();
            assert!(export_patch(&fs, "P007.SYX", &p, 7));
        }
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut out = FileList::new();
        assert_eq!(list_patch_files(&fs, &mut out), 1); // in /MBSID/, .SYX
        assert_eq!(out[0].as_str(), "P007.SYX");
        let mut dst = [0u8; 512];
        assert!(load_patch_by_index(&fs, 0, &mut dst));
        assert_eq!(dst, p);
    }

    #[test]
    fn export_overwrites_existing_file() {
        let (p1, p2) = (test_patch(11), test_patch(12));
        let mut img = build_gpt_fat_image(&[("SEED.TXT", b"x")]);
        let base = BASE_LBA as usize * SECTOR;
        for p in [&p1, &p2] {
            let part = VecDisk::new(&mut img[base..]);
            let fs = FileSystem::new(part, FsOptions::new()).unwrap();
            assert!(export_patch(&fs, "EDIT.SYX", p, 0));
        }
        let part = VecDisk::new(&mut img[base..]);
        let fs = FileSystem::new(part, FsOptions::new()).unwrap();
        let mut dst = [0u8; 512];
        assert!(load_patch_by_index(&fs, 0, &mut dst));
        assert_eq!(dst, p2); // second export won, file not duplicated
    }
```

- [ ] **Step 2: Run to verify failure, then implement usb_patch**

```rust
/// Encode `patch` as a standard MBSID v2 single-patch dump (bank 1 = User,
/// so the exported file re-sent over MIDI lands in the user bank).
pub fn encode_syx(patch: &[u8; 512], slot: u8, out: &mut [u8; 1036]) {
    const HEADER: [u8; 6] = [0xF0, 0x00, 0x00, 0x7E, 0x4B, 0x00];
    out[..6].copy_from_slice(&HEADER);
    out[6] = 0x02;        // cmd: Patch Write
    out[7] = 0x00;        // type: Bank Write, sid 0
    out[8] = 0x01;        // bank: User
    out[9] = slot & 0x7F;
    let mut sum: u32 = 0;
    for (i, &d) in patch.iter().enumerate() {
        let (lo, hi) = (d & 0x0F, (d >> 4) & 0x0F);
        out[10 + 2 * i] = lo;
        out[11 + 2 * i] = hi;
        sum += (lo + hi) as u32;
    }
    out[1034] = ((sum as i32).wrapping_neg() & 0x7F) as u8;
    out[1035] = 0xF7;
}

/// Export `patch` as `/MBSID/<name>`; flush; verify by readback+reparse.
pub fn export_patch<IO: ReadWriteSeek>(
    fs: &FileSystem<IO>, name: &str, patch: &[u8; 512], slot: u8) -> bool {
    use fatfs::Write as _;
    let root = fs.root_dir();
    let dir = match root.open_dir("MBSID") {
        Ok(d) => d,
        Err(_) => match root.create_dir("MBSID") {
            Ok(d) => d,
            Err(_) => root, // e.g. read-only quirk: fall back to root
        },
    };
    let mut syx = [0u8; 1036];
    encode_syx(patch, slot, &mut syx);
    {
        let Ok(mut f) = dir.create_file(name) else { return false };
        if f.truncate().is_err() { return false; }
        let mut rest: &[u8] = &syx;
        while !rest.is_empty() {
            match f.write(rest) {
                Ok(0) | Err(_) => return false,
                Ok(n) => rest = &rest[n..],
            }
        }
        if f.flush().is_err() { return false; }
    }
    // Verify: re-open, re-read, re-parse, byte-compare (cheap end-to-end
    // check that the write path actually landed — spec §6b).
    let Ok(mut f) = dir.open_file(name) else { return false };
    let mut back = [0u8; 1036];
    let mut total = 0usize;
    while total < back.len() {
        match f.read(&mut back[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => return false,
        }
    }
    let mut dst = [0u8; 512];
    total == 1036 && parse_patch_file(&back, &mut dst) && dst == *patch
}
```

(Import `fatfs::Write` in the module header instead of locally if cleaner. If the
pinned fatfs lacks `open_file`/`truncate` under these names, adapt call-sites —
`create_file` on an existing path opens it, then `truncate()` clears it; both exist
in fatfs 0.4.)

Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib usb_patch` → PASS.

- [ ] **Step 3: Menu Export row (failing tests, then implement)**

Tests:

```rust
    #[test]
    fn usb_export_row_selects_edit_or_slot() {
        let mut m = MenuState::new(1, 0, 0);
        m.usb_storage = true;
        m.card = Card::Usb;
        m.focus = USB_ROW_EXPORT;
        assert_eq!(m.on_press(), PressResult::Toggled);
        assert_eq!(m.usb_export, -1);                      // Cancel-first
        let _ = m.on_turn(1);                              // -> EDIT
        assert_eq!(m.on_press(), PressResult::UsbExport { source: ExportSource::Edit });
        let _ = m.on_press();                              // re-enter Edit
        let _ = m.on_turn(3);                              // -> slot 1 (0-based: 1+? )
        assert_eq!(m.on_press(),
                   PressResult::UsbExport { source: ExportSource::Slot(1) });
        let _ = m.on_press();
        assert_eq!(m.on_press(), PressResult::Cancel);     // press at Cancel
    }
```

Implementation: `USB_ROW_EXPORT: u8 = 3`, Usb `row_count` → 4; field
`usb_export: i16 = -1`; `on_turn` Usb arm gains
`USB_ROW_EXPORT => { self.usb_export = clamp_i16(self.usb_export + delta as i16, -1, 128); TurnResult::None }`;
`on_press` Usb-Edit match gains:

```rust
                USB_ROW_EXPORT => match self.usb_export {
                    -1 => PressResult::Cancel,
                    0 => PressResult::UsbExport { source: ExportSource::Edit },
                    n => PressResult::UsbExport {
                        source: ExportSource::Slot((n - 1) as u8) },
                }
```

Cancel-first reset on Edit entry (next to the `usb_slot` reset):
`if self.card == Card::Usb && self.focus == USB_ROW_EXPORT { self.usb_export = -1; }`.
`build_frame` Usb card gains the row at `pos_y + 5 * ROW_DY` (status moves to
`7 * ROW_DY` — already there):

```rust
            let marker = row_marker(st, USB_ROW_EXPORT);
            line.clear();
            match st.usb_export {
                -1 => { let _ = write!(line, "{} Export   Cancel", marker); }
                0 => { let _ = write!(line, "{} Export   EDIT -> USB", marker); }
                n => { let _ = write!(line, "{} Export   U{:03} -> USB", marker, n - 1); }
            }
            f.push(pos_x, pos_y + 5 * ROW_DY, st.focus == USB_ROW_EXPORT, &line);
```

(Fix the `build_frame_usb_card_rows` count 5 → 6.)

- [ ] **Step 4: main.rs dispatch**

```rust
                    PressResult::UsbExport { source } => {
                        let (slot, got) = match source {
                            menu::ExportSource::Edit => {
                                critical_section::with(|_cs| {
                                    mbsid_sys::current_patch_raw(&mut patch_buf);
                                });
                                (0u8, true)
                            }
                            menu::ExportSource::Slot(n) =>
                                (n, store.load(n, &mut patch_buf)),
                        };
                        let mut fname: heapless::String<16> = heapless::String::new();
                        let _ = match source {
                            menu::ExportSource::Edit =>
                                core::fmt::Write::write_str(&mut fname, "EDIT.SYX"),
                            menu::ExportSource::Slot(n) =>
                                core::fmt::Write::write_fmt(&mut fname,
                                    format_args!("P{:03}.SYX", n)),
                        };
                        let ok = got && with_fat(&usb_msc, |fs| {
                            usb_patch::export_patch(fs, &fname, &patch_buf, slot)
                        }).unwrap_or(false);
                        let mut s: heapless::String<24> = heapless::String::new();
                        let _ = if ok {
                            core::fmt::Write::write_fmt(&mut s,
                                format_args!("Exported {}", fname))
                        } else {
                            core::fmt::Write::write_str(&mut s, "Export FAILED")
                        };
                        status = Some(s);
                        usb_listed = false;   // new file: refresh the list
                    }
```

- [ ] **Step 5: Tests + target build**

```bash
cd fw && cargo test --target x86_64-unknown-linux-gnu --lib
cd ../../../../ && cd gateware && pdm mbsid build --fw-only
```
Expected: PASS + ELF builds.

- [ ] **Step 6: Commit**

```bash
git add fw/src/usb_patch.rs fw/src/menu.rs fw/src/main.rs
git commit -m "feat(mbsid): patch export to USB — encode_syx, /MBSID writer with verify, Export row (M6b)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 15: M6b checkpoint — full build, docs, counts, HW checklist

- [ ] **Step 1: Full build + metrics**

```bash
cd gateware && pdm mbsid build
```
Record post-route sync Fmax (second `Max frequency` line, ≥ 60 MHz required) and LUT%.
The TX FIFO + engine write leg is small, but this is the design's first full build with
it — a FAIL here is actionable (root CLAUDE.md), a pass-side wiggle is seed noise.

- [ ] **Step 2: Full test sweep + count reconcile**

```bash
cd gateware && pdm test
cd src/top/mbsid/fw && cargo test --target x86_64-unknown-linux-gnu --lib 2>&1 | tail -3
grep -rn "cargo test --lib\|tests:" ../CLAUDE.md ../docs/developer-guide.md
```
Reconcile all quoted counts.

- [ ] **Step 3: Documentation**

- `CLAUDE.md` (this dir): M6b gotchas — vendored `src/vendor/guh_msc/` (never edit
  `.venv` guh; upstream-PR candidate), write CSR contract (128 words → `start_write` →
  poll sticky `resp.done`), the 8.3 filename choice, **update the "no export path"
  framing in the no-MIDI-TX gotcha** (spec §9), status line.
- `docs/` user guide: export walkthrough, "don't unplug while BUSY".
- `M6_USB_STORAGE.md`: Status → implemented, HW bring-up pending.

- [ ] **Step 4: Hardware checklist (record in `M6_USB_STORAGE.md`)**

From spec §7 M6b: exported file mounts clean on a PC (`fsck.vfat` clean); byte-identical
round-trip (export → PC → send same file over TRS SysEx → engine state identical; and
export → re-import on device → 512-byte compare equal); unplug during export leaves a
mountable FS with at worst a truncated/missing file; 50 repeated exports then
`fsck.vfat` (no leaked clusters); quirky-drive sweep (cheap stick + SSD enclosure —
also covers the Step-2/Task-11 OUT-packet-size risk); stack-paint re-measure.

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md docs/ M6_USB_STORAGE.md
git commit -m "docs(mbsid): M6b USB patch export — status, user guide, HW checklist

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Self-review notes (spec coverage)

- Spec §3 (mode switch, VBUS both modes, TRS forced, engine reset on flip): Tasks 3, 9.
- Spec §4a (CSR 0x1300, opt-in flag, PAC regen): Tasks 2–3. §4b (vendored engine,
  WRITE_10, TX CSR, sim test incl. NAK/PID/flags/LBA-BE assertions): Tasks 11–12.
- Spec §5 (on-disk format, /MBSID dir, naming, raw-512 import): Tasks 6, 14 — naming
  decided as plain 8.3 `PNNN.SYX`/`EDIT.SYX` (fatfs built without the `lfn` feature,
  matching sid_player_sw; spec §5 explicitly allows this).
- Spec §6a–f (reused modules, usb_patch, file-mode capture, menu card, main-loop-only
  I/O, RAM budget): Tasks 4–9, 13–14.
- Spec §7 (Phase 0 probe + decision rule, M6a/M6b phasing, stopgap export): Tasks 3
  (gate), 1 (stopgap), part boundaries.
- Spec §8 risks carried into tasks: area/Fmax → Task 3 gate; FAT corruption → verify-by
  -readback + flush (Task 14) + HW checklist; quirky drives → `block_size()==512` guard
  (Task 9) + HW checklist; guh fork drift → vendor only `engines/msc.py` (Task 11);
  mode-switch wedge → ResetInserter parking (Task 3); main-loop stall → spin caps
  (Tasks 4, 13).
- Known intentional deviations from upstream-file-verbatim: `fat.rs` made generic over
  `BlockIo` (host-testability of the M6b write-back cache); mbsid `lib.rs` switched to
  `#![cfg_attr(not(test), no_std)]` (in-memory FAT images in tests; sid_player_sw
  precedent).
