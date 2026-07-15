# USB MSC Read-Path Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add diagnostic-only observability that identifies where the first post-write READ(10) loses its 512-byte payload, without changing USB transport behavior.

**Architecture:** `SCSIBulkHost` exposes the command values it actually sampled and the number of data-IN bytes it accepted from the SIE. `USBMSCPeripheral` combines those live engine values with counters at its own RX byte-packer boundary in one read-only 32-bit CSR at `0x38`; firmware snapshots that word whenever `read_block` fails and appends it to the existing `rd1`/`rdL` UART lines. A write→read integration test keeps the read CSW busy with a NAK before accepting it, matching the hardware trace while asserting all five diagnostic fields.

**Tech Stack:** Python 3, Amaranth HDL and simulator, `amaranth_soc.csr`, Rust `no_std` firmware using the generated `tiliqua_pac`, pytest, PDM, nextpnr-ecp5.

## Global Constraints

- The working tree already contains substantial uncommitted round-five/round-six work, including edits to every production file in this plan. Do not run `git stash`, `git reset`, `git checkout`, cleanup commands, or broad formatters.
- Do not stage or commit during this plan unless the user first creates a baseline commit for the existing overlapping changes. `git add <file>` would otherwise stage the user's pre-existing work along with this diagnostic.
- Make diagnostic-only changes. Do not alter command sequencing, FIFO readiness, retry policy, watchdog duration, firmware spin limits, or FAT behavior.
- Preserve the existing CSR map. Add only `read_path_info` at unused offset `0x38`, gated by `with_write=True`; the legacy `with_write=False` map must remain unchanged.
- Use this exact packed layout: `engine_bytes[9:0]`, `periph_bytes[19:10]`, `periph_words[27:20]`, `stream_mode[28]`, `data_len_512[29]`, with bits `[31:30]` reading zero.
- Counters describe the current command and reset on either read `start` or `start_write`. Ten-bit byte counters represent 512 exactly; saturate rather than wrap at 1023. The eight-bit word counter represents 128 exactly; saturate rather than wrap at 255.
- Keep the new engine outputs live. The failing firmware snapshot occurs at about 3 seconds, before the engine's 10-second watchdog reset; do not add another cross-domain preservation latch for this round.
- Keep the existing `heapless::String<256>` allocation. Print the packed diagnostic as `pth={:08x}` instead of expanding all fields in firmware.
- A CSR layout change requires `pdm mbsid build --pac-only` before firmware compilation.
- Full verification must be sequential. Do not run nextpnr builds in parallel.
- Hardware validation remains restricted to disposable media until `M6_USB_STORAGE.md` section 7b passes.

## File Structure

- `gateware/tests/test_usb_msc_integration.py`: end-to-end write→read/CSW-NAK regression and packed-CSR assertions.
- `gateware/tests/test_usb_msc_csr.py`: focused RX-packer counter/reset/saturation tests and legacy-map guard.
- `gateware/src/vendor/guh_msc/msc.py`: engine-side sampled-command and accepted-byte outputs, plus `USBMSCHost` pass-through ports.
- `gateware/src/tiliqua/usb_msc_csr.py`: `read_path_info` register, packer counters, and new diagnostic inputs.
- `gateware/src/top/sid/top.py`: production glue from `USBMSCHost` diagnostics to `USBMSCPeripheral`.
- `gateware/src/top/mbsid/fw/src/usb_msc.rs`: failure-time snapshot storage.
- `gateware/src/top/mbsid/fw/src/main.rs`: compact `pth=` UART output.
- `gateware/src/top/mbsid/M6_USB_STORAGE.md`: field map, interpretation table, and next hardware procedure.
- `gateware/src/top/mbsid/CLAUDE.md`: concise bring-up gotcha so later agents decode the field consistently.

---

### Task 1: Lock the diagnostic contract down with failing simulations

**Files:**
- Modify: `gateware/tests/test_usb_msc_integration.py:347-410`
- Modify: `gateware/tests/test_usb_msc_csr.py:18-56`

**Interfaces:**
- Consumes: existing `_Fw.csr_read32(ctx, offset)`, `_Drive.do_in(ctx, payload, response)`, and `USBMSCPeripheral(with_write=True)`.
- Produces: executable expectations for `read_path_info` at CSR offset `0x38` and the exact bit layout used by all later tasks.

- [ ] **Step 1: Record the dirty-tree boundary before editing**

Run:

```bash
cd /home/pawel/code/tiliqua
git status --short
git diff -- gateware/tests/test_usb_msc_integration.py gateware/tests/test_usb_msc_csr.py gateware/src/vendor/guh_msc/msc.py gateware/src/tiliqua/usb_msc_csr.py gateware/src/top/sid/top.py gateware/src/top/mbsid/fw/src/usb_msc.rs gateware/src/top/mbsid/fw/src/main.rs gateware/src/top/mbsid/M6_USB_STORAGE.md gateware/src/top/mbsid/CLAUDE.md
```

Expected: the known round-five/round-six modifications are present. Review them in the terminal; do not write a backup into the repository and do not normalize or revert them.

- [ ] **Step 2: Extend the integration test with a read-CSW NAK and packed diagnostic assertion**

In `test_read_immediately_after_successful_write`, after draining the 128 words, wait until the drive testbench has served the NAKed CSW, then read `0x38`:

```python
                for _ in range(200000):
                    if result.get("read_csw_nak_seen"):
                        break
                    await ctx.tick("sync")
                else:
                    raise AssertionError("drive never NAKed the read CSW")
                result["read_path"] = await fw.csr_read32(ctx, 0x38)
                result["rerr"] = False
                result["rdata"] = data
```

In the drive side, replace the single successful read CSW with one NAK followed by success:

```python
                await drive.do_in(ctx, [], TransferResponse.NAK)
                result["read_csw_nak_seen"] = True
                await drive.do_in(ctx, _csw(0x00), TransferResponse.ACK)
```

Append these assertions to the test:

```python
        read_path = result.get("read_path", 0)
        self.assertEqual(read_path & 0x3FF, 512)          # engine_bytes
        self.assertEqual((read_path >> 10) & 0x3FF, 512) # periph_bytes
        self.assertEqual((read_path >> 20) & 0xFF, 128)  # periph_words
        self.assertEqual((read_path >> 28) & 1, 1)       # stream_mode
        self.assertEqual((read_path >> 29) & 1, 1)       # data_len_512
        self.assertEqual(read_path >> 30, 0)
```

- [ ] **Step 3: Add a focused peripheral test for counting, reset, and field packing**

Add a reusable 32-bit CSR reader beside `csr_write` in `test_usb_msc_csr.py`:

```python
async def csr_read32(ctx, dut, offset):
    value = 0
    for i in range(4):
        ctx.set(dut.bus.addr, offset + i)
        ctx.set(dut.bus.r_stb, 1)
        await ctx.tick()
        ctx.set(dut.bus.r_stb, 0)
        value |= ctx.get(dut.bus.r_data) << (8 * i)
    return value
```

Add this test to `UsbMscCsrTests`:

```python
    def test_read_path_info_counts_and_resets_on_read_start(self):
        dut = USBMSCPeripheral(with_mode=True, with_write=True)
        m = Module()
        m.submodules.dut = dut

        async def testbench(ctx):
            ctx.set(dut.engine_rx_bytes_i, 512)
            ctx.set(dut.engine_stream_mode_i, 1)
            ctx.set(dut.engine_data_len_512_i, 1)
            ctx.set(dut.rx_data.valid, 1)
            for b in range(8):
                ctx.set(dut.rx_data.payload.data, b)
                await ctx.tick()
            ctx.set(dut.rx_data.valid, 0)
            await ctx.tick()

            raw = await csr_read32(ctx, dut, 0x38)
            self.assertEqual(raw & 0x3FF, 512)
            self.assertEqual((raw >> 10) & 0x3FF, 8)
            self.assertEqual((raw >> 20) & 0xFF, 2)
            self.assertEqual((raw >> 28) & 1, 1)
            self.assertEqual((raw >> 29) & 1, 1)
            self.assertEqual(raw >> 30, 0)

            # Continue past both counter maxima. The byte counter must stop at
            # 1023, and the accepted-word counter must stop at 255 rather than
            # wrapping to zero when the 256-word FIFO becomes full.
            ctx.set(dut.rx_data.valid, 1)
            for b in range(1100):
                ctx.set(dut.rx_data.payload.data, b & 0xFF)
                await ctx.tick()
            ctx.set(dut.rx_data.valid, 0)
            await ctx.tick()
            raw = await csr_read32(ctx, dut, 0x38)
            self.assertEqual((raw >> 10) & 0x3FF, 1023)
            self.assertEqual((raw >> 20) & 0xFF, 255)

            await csr_write(ctx, dut, 0x10, 1)
            await ctx.tick()
            raw = await csr_read32(ctx, dut, 0x38)
            self.assertEqual(raw & 0x3FF, 512)  # live engine input
            self.assertEqual((raw >> 10) & 0x3FF, 0)
            self.assertEqual((raw >> 20) & 0xFF, 0)

        sim = Simulator(m)
        sim.add_clock(1e-6)
        sim.add_testbench(testbench)
        sim.run()
```

- [ ] **Step 4: Strengthen the legacy-map guard**

Append these assertions to `test_without_write_has_no_tx_registers`:

```python
        self.assertNotIn("read_path_info", joined)
        self.assertFalse(hasattr(dut, "_read_path_info"))
```

- [ ] **Step 5: Run the two new expectations and verify RED**

Run:

```bash
cd /home/pawel/code/tiliqua/gateware
pdm run pytest tests/test_usb_msc_integration.py::UsbMscIntegrationTests::test_read_immediately_after_successful_write tests/test_usb_msc_csr.py::UsbMscCsrTests::test_read_path_info_counts_and_resets_on_read_start -v
```

Expected: FAIL because `USBMSCPeripheral` has no `engine_rx_bytes_i` port and offset `0x38` is not implemented. A failure caused only by the missing diagnostic proves the tests exercise the intended contract.

- [ ] **Step 6: Review checkpoint without staging or committing**

Run:

```bash
cd /home/pawel/code/tiliqua
git diff --check
git diff -- gateware/tests/test_usb_msc_integration.py gateware/tests/test_usb_msc_csr.py
```

Expected: no whitespace errors; the diff contains test changes only. Do not commit while overlapping user changes remain uncommitted.

---

### Task 2: Implement the gateware diagnostic path

**Files:**
- Modify: `gateware/src/vendor/guh_msc/msc.py:155-181,217-249,343-363,444-462,836-886`
- Modify: `gateware/src/tiliqua/usb_msc_csr.py:99-126,133-181,189-230,349-365`
- Modify: `gateware/src/top/sid/top.py:691-723`
- Modify: `gateware/tests/test_usb_msc_integration.py:143-190`

**Interfaces:**
- Consumes: `SCSIBulkHost` internal `rx_data_count`, `stream_mode`, and `data_len`; `USBMSCPeripheral` RX stream handshake and word FIFO readiness.
- Produces: `USBMSCHost.rx_bytes_o: Out(10)`, `stream_mode_o: Out(1)`, `data_len_512_o: Out(1)`; `USBMSCPeripheral.engine_rx_bytes_i: In(10)`, `engine_stream_mode_i: In(1)`, `engine_data_len_512_i: In(1)`; read-only CSR `read_path_info` at `0x38`.

- [ ] **Step 1: Add engine diagnostic outputs at the SCSI boundary**

Add these ports to `SCSIBulkHost` after `phase_o`:

```python
    # Live read-path diagnostics. Firmware samples these before the outer
    # 10-second watchdog can reset the engine.
    rx_bytes_o:       Out(10)  # bytes accepted from the SIE this data-IN phase
    stream_mode_o:    Out(1)   # command's sampled stream_data value
    data_len_512_o:   Out(1)   # sampled data_len equals one 512-byte block
```

After `stream_mode` and `data_dir_r` are declared, drive the outputs from the sampled state:

```python
        m.d.comb += [
            self.rx_bytes_o.eq(rx_data_count[:10]),
            self.stream_mode_o.eq(stream_mode),
            self.data_len_512_o.eq(data_len == 512),
        ]
```

Do not add a second counter: `rx_data_count` already increments exactly on `enum.ctrl.rxs.valid & enum.ctrl.rxs.ready`, which is the required SIE-boundary measurement.

- [ ] **Step 2: Pass the engine diagnostics through `USBMSCHost`**

Add these component ports after `phase_o`:

```python
    rx_bytes_o:       Out(10)
    stream_mode_o:    Out(1)
    data_len_512_o:   Out(1)
```

Add these assignments to the existing pass-through list in `elaborate`:

```python
            self.rx_bytes_o.eq(scsi.rx_bytes_o),
            self.stream_mode_o.eq(scsi.stream_mode_o),
            self.data_len_512_o.eq(scsi.data_len_512_o),
```

- [ ] **Step 3: Add the packed CSR type, register, and input ports**

Add this register class to `USBMSCPeripheral` after `SenseInfo`:

```python
    class ReadPathInfo(csr.Register, access="r"):
        """Live read-path discriminator for post-write READ(10) failures.
        engine_bytes counts bytes accepted from the SIE; periph_bytes and
        periph_words count what crossed and packed at this peripheral.
        stream_mode/data_len_512 are the values sampled by SCSIBulkHost."""
        engine_bytes: csr.Field(csr.action.R, unsigned(10))
        periph_bytes: csr.Field(csr.action.R, unsigned(10))
        periph_words: csr.Field(csr.action.R, unsigned(8))
        stream_mode:  csr.Field(csr.action.R, unsigned(1))
        data_len_512: csr.Field(csr.action.R, unsigned(1))
```

Register it only in the `with_write` block, immediately after `sense_info`:

```python
            self._read_path_info = regs.add(
                "read_path_info", self.ReadPathInfo(), offset=0x38)
```

Add these ports to the per-instance component signature:

```python
            "engine_rx_bytes_i":     In(10),
            "engine_stream_mode_i":  In(1),
            "engine_data_len_512_i": In(1),
```

- [ ] **Step 4: Count bytes and successfully enqueued words at the peripheral boundary**

Declare the counters beside `byte_ix` and `acc`:

```python
        rx_byte_count_r = Signal(10)
        rx_word_count_r = Signal(8)
```

Inside the existing RX handshake block, add a saturating byte increment:

```python
            with m.If(rx_byte_count_r != 0x3FF):
                m.d.sync += rx_byte_count_r.eq(rx_byte_count_r + 1)
```

Inside the existing `with m.If(byte_ix == 3):` block, count only word writes accepted by the FIFO:

```python
                with m.If(wf.w_rdy & (rx_word_count_r != 0xFF)):
                    m.d.sync += rx_word_count_r.eq(rx_word_count_r + 1)
```

In the existing `with m.If(start_strobe):` reset list, add both counters:

```python
                rx_byte_count_r.eq(0),
                rx_word_count_r.eq(0),
```

In the `with_write` block, also reset them on `start_write` so a write cannot leave stale read-path counts:

```python
            with m.If(start_write):
                m.d.sync += [
                    rx_byte_count_r.eq(0),
                    rx_word_count_r.eq(0),
                ]
```

Drive the register fields in the existing diagnostic readback list:

```python
                self._read_path_info.f.engine_bytes.r_data.eq(
                    self.engine_rx_bytes_i),
                self._read_path_info.f.periph_bytes.r_data.eq(rx_byte_count_r),
                self._read_path_info.f.periph_words.r_data.eq(rx_word_count_r),
                self._read_path_info.f.stream_mode.r_data.eq(
                    self.engine_stream_mode_i),
                self._read_path_info.f.data_len_512.r_data.eq(
                    self.engine_data_len_512_i),
```

- [ ] **Step 5: Wire the new ports in production and in the integration harness**

Add these assignments to the MSC glue list in `gateware/src/top/sid/top.py`:

```python
                    self.usb_msc.engine_rx_bytes_i.eq(msc.rx_bytes_o),
                    self.usb_msc.engine_stream_mode_i.eq(msc.stream_mode_o),
                    self.usb_msc.engine_data_len_512_i.eq(msc.data_len_512_o),
```

Add the same three assignments to `_build()` in `gateware/tests/test_usb_msc_integration.py`:

```python
        periph.engine_rx_bytes_i.eq(host.rx_bytes_o),
        periph.engine_stream_mode_i.eq(host.stream_mode_o),
        periph.engine_data_len_512_i.eq(host.data_len_512_o),
```

- [ ] **Step 6: Run the focused tests and verify GREEN**

Run:

```bash
cd /home/pawel/code/tiliqua/gateware
pdm run pytest tests/test_usb_msc_integration.py::UsbMscIntegrationTests::test_read_immediately_after_successful_write tests/test_usb_msc_csr.py::UsbMscCsrTests::test_read_path_info_counts_and_resets_on_read_start tests/test_usb_msc_csr.py::UsbMscCsrTests::test_without_write_has_no_tx_registers -v
```

Expected: 3 passed. The integration test must show 512 engine bytes, 512 peripheral bytes, 128 peripheral words, stream mode 1, and length-is-512 1 despite the read CSW being NAKed once.

- [ ] **Step 7: Run the production elaboration guard**

Run:

```bash
cd /home/pawel/code/tiliqua/gateware
pdm run pytest tests/test_usb_msc_integration.py::ProductionElaborationTest::test_production_engine_elaborates -v
```

Expected: 1 passed. This catches cross-package enum and component-signature mistakes that the stub path cannot expose.

- [ ] **Step 8: Review checkpoint without staging or committing**

Run:

```bash
cd /home/pawel/code/tiliqua
git diff --check
git diff -- gateware/src/vendor/guh_msc/msc.py gateware/src/tiliqua/usb_msc_csr.py gateware/src/top/sid/top.py gateware/tests/test_usb_msc_integration.py gateware/tests/test_usb_msc_csr.py
```

Expected: only the diagnostic ports, counters, CSR, wiring, and tests are added around the existing uncommitted work. Do not commit while the overlapping changes lack a baseline.

---

### Task 3: Snapshot and print the diagnostic in firmware

**Files:**
- Modify: `gateware/src/top/mbsid/fw/src/usb_msc.rs:8-83,119-180`
- Modify: `gateware/src/top/mbsid/fw/src/main.rs:730-750`

**Interfaces:**
- Consumes: generated PAC accessor `USB_MSC.read_path_info()` with fields `engine_bytes`, `periph_bytes`, `periph_words`, `stream_mode`, and `data_len_512`.
- Produces: `MscDiag.rd_path_first: Cell<u32>`, `MscDiag.rd_path: Cell<u32>`, and `pth=XXXXXXXX` on both read-failure UART lines.

- [ ] **Step 1: Regenerate the PAC before compiling firmware**

Run:

```bash
cd /home/pawel/code/tiliqua/gateware
pdm mbsid build --pac-only
```

Expected: PAC generation completes and the generated `USB_MSC` API contains `read_path_info`. PAC output is gitignored; do not add it to version control.

- [ ] **Step 2: Add first/last packed read-path storage**

Add these fields after `rd_fail` in `MscDiag`:

```rust
    /// Packed read_path_info CSR captured with rd_fail_first/rd_fail.
    /// [9:0]=engine bytes, [19:10]=peripheral bytes,
    /// [27:20]=peripheral words, [28]=stream mode,
    /// [29]=sampled data length was 512.
    pub rd_path_first: core::cell::Cell<u32>,
    pub rd_path: core::cell::Cell<u32>,
```

Clear both in `begin()`:

```rust
        self.rd_path_first.set(0);
        self.rd_path.set(0);
```

Change `record_rd_failure` to keep the tuple stable and store the packed word separately:

```rust
    fn record_rd_failure(
        &self,
        v: (u8, u8, u32, u32, u8, u8, u8, u8),
        path: u32,
    ) {
        self.rd_fail.set(v);
        self.rd_path.set(path);
        if !self.rd_first_set.get() {
            self.rd_first_set.set(true);
            self.rd_fail_first.set(v);
            self.rd_path_first.set(path);
        }
    }
```

- [ ] **Step 3: Add a stable firmware-side packer for the generated PAC fields**

Add this method after `reject_snapshot`:

```rust
    fn read_path_snapshot(&self) -> u32 {
        let p = self.regs.read_path_info().read();
        u32::from(p.engine_bytes().bits())
            | (u32::from(p.periph_bytes().bits()) << 10)
            | (u32::from(p.periph_words().bits()) << 20)
            | (u32::from(p.stream_mode().bit_is_set()) << 28)
            | (u32::from(p.data_len_512().bit_is_set()) << 29)
    }
```

This repacks named fields rather than depending on a generated raw-register API, making the bit contract explicit in source.

- [ ] **Step 4: Capture the path word at every existing read failure site**

At the not-ready failure, replace the current recorder call with:

```rust
            let path = self.read_path_snapshot();
            self.diag.record_rd_failure(
                (1, 0, lba, 0, rr, rp, ny, lp), path);
```

At the response-error failure, replace the current recorder call with:

```rust
                    let path = self.read_path_snapshot();
                    self.diag.record_rd_failure(
                        (2, i as u8, lba, sp, rr, rp, ny, lp), path);
```

At the spin-timeout failure, replace the current recorder call with:

```rust
                        let path = self.read_path_snapshot();
                        self.diag.record_rd_failure(
                            (3, i as u8, lba, spins, rr, rp, ny, lp), path);
```

- [ ] **Step 5: Append compact packed values to the existing UART lines**

Read the two packed cells beside the existing tuple reads:

```rust
                            let path1 = d.rd_path_first.get();
                            let path2 = d.rd_path.get();
```

Change the two format lines and their arguments to:

```rust
                                    "export: rd1 rsn={} w={} lba={} sp={} \
                                     rej={}/{} ny={} lph={} pth={:08x}\r\n\
                                     export: rdL rsn={} w={} lba={} sp={} \
                                     rej={}/{} ny={} lph={} pth={:08x}\r\n",
                                    r1, w1, l1, s1, rr1, rp1, n1, p1, path1,
                                    r2, w2, l2, s2, rr2, rp2, n2, p2, path2));
```

Keep `heapless::String<256>` unchanged. The two 13-character additions fit within the existing capacity and avoid increasing measured stack pressure.

- [ ] **Step 6: Compile and run host firmware tests**

Run:

```bash
cd /home/pawel/code/tiliqua/gateware/src/top/mbsid/fw
cargo test --target x86_64-unknown-linux-gnu --lib
```

Expected: 118 tests pass, or the current branch's higher total if tests were added elsewhere; no compile error in `usb_msc.rs` or `main.rs`.

- [ ] **Step 7: Review checkpoint without staging or committing**

Run:

```bash
cd /home/pawel/code/tiliqua
git diff --check
git diff -- gateware/src/top/mbsid/fw/src/usb_msc.rs gateware/src/top/mbsid/fw/src/main.rs
```

Expected: only packed snapshot storage and `pth=` logging are added around the existing diagnostics. Do not commit while the overlapping changes lack a baseline.

---

### Task 4: Document interpretation and run full verification

**Files:**
- Modify: `gateware/src/top/mbsid/M6_USB_STORAGE.md`
- Modify: `gateware/src/top/mbsid/CLAUDE.md`
- Verify: `gateware/tests/test_usb_msc_integration.py`
- Verify: `gateware/tests/test_usb_msc_csr.py`
- Verify: `gateware/tests/test_guh_msc_write.py`
- Verify: `gateware/tests/test_guh_msc_write_fullloop.py`
- Verify: `gateware/tests/test_guh_sie_tx_packets.py`

**Interfaces:**
- Consumes: the `pth` packed layout and successful simulation evidence from Tasks 1-3.
- Produces: an unambiguous hardware decoding table and a timing-checked flashable archive for the next disposable-media run.

- [ ] **Step 1: Add the diagnostic layout and interpretation table to the round-six incident section**

Add this text to `M6_USB_STORAGE.md` after the latest round-six hardware log analysis:

```markdown
#### Read-path discriminator (`read_path_info`, CSR `0x38`)

The next diagnostic-only build snapshots a packed `pth=XXXXXXXX` word on the
first and last failed reads. It does not change USB command sequencing, retry
policy, watchdogs, or firmware timeouts.

| Bits | Field | Meaning |
|---|---|---|
| `[9:0]` | `engine_bytes` | Data-IN bytes accepted from the SIE for the current command |
| `[19:10]` | `periph_bytes` | Bytes accepted by `USBMSCPeripheral`'s RX packer |
| `[27:20]` | `periph_words` | Complete 32-bit words successfully enqueued by the packer |
| `[28]` | `stream_mode` | `stream_data` value sampled by `SCSIBulkHost` |
| `[29]` | `data_len_512` | Sampled command length was exactly 512 bytes |
| `[31:30]` | reserved | Always zero |

Interpret the failing `rd1` snapshot in this order:

- `engine_bytes=512, stream_mode=0, periph_bytes=0`: the command ran in
  capture mode; investigate command sampling/state handoff.
- `engine_bytes=512, stream_mode=1, periph_bytes=0`: bytes entered the
  engine's stream FIFO but did not cross into the CSR peripheral.
- `engine_bytes=512, periph_bytes=512, periph_words=0`: the RX byte packer
  saw data but complete words were not accepted by its FIFO.
- `engine_bytes=512, periph_bytes=512, periph_words=128`: the full data path
  completed; investigate FIFO reset/readback visibility rather than USB.
- `engine_bytes<512` or `data_len_512=0` while `lph=3`: re-examine the
  engine's CSW transition and sampled command length.
```

- [ ] **Step 2: Add the compact decoding reminder to `CLAUDE.md`**

Add this bullet to the M6b hardware-bring-up gotcha block:

```markdown
- **Round-six read-path `pth` diagnostic:** `pth` is the raw `read_path_info`
  CSR: engine bytes `[9:0]`, peripheral bytes `[19:10]`, packed words
  `[27:20]`, sampled stream mode `[28]`, sampled-length-is-512 `[29]`.
  It is diagnostic-only and sampled before the engine's 10 s watchdog.
```

- [ ] **Step 3: Run the complete scoped MSC simulation suite sequentially**

Run:

```bash
cd /home/pawel/code/tiliqua/gateware
pdm run pytest tests/test_usb_msc_integration.py tests/test_usb_msc_csr.py tests/test_guh_msc_write.py tests/test_guh_msc_write_fullloop.py tests/test_guh_sie_tx_packets.py -v
```

Expected: all tests pass. The handoff baseline was 30 passing tests; the total should increase by the focused read-path test additions without regressions.

- [ ] **Step 4: Re-run host firmware tests after the complete tree is assembled**

Run:

```bash
cd /home/pawel/code/tiliqua/gateware/src/top/mbsid/fw
cargo test --target x86_64-unknown-linux-gnu --lib
```

Expected: all host tests pass.

- [ ] **Step 5: Run one full build and inspect the actual final timing result**

Run:

```bash
cd /home/pawel/code/tiliqua/gateware
pdm mbsid build
rg -n "Max frequency for clock '\$glbnet\$clk'|Info: Device utilisation|Warning: Max frequency" build/mbsid-r5/top.tim
```

Expected: the build completes, the second/post-route max-frequency block is fresh, `sync` meets the 60 MHz target, and all five clocks pass. Do not infer success from process exit code alone; inspect the log tail and `top.tim` because prior elaboration failures returned exit code zero.

- [ ] **Step 6: Audit scope and preserve the dirty tree**

Run:

```bash
cd /home/pawel/code/tiliqua
git diff --check
git status --short
git diff --stat
git diff -- gateware/src/vendor/guh_msc/msc.py gateware/src/tiliqua/usb_msc_csr.py gateware/src/top/sid/top.py gateware/tests/test_usb_msc_integration.py gateware/tests/test_usb_msc_csr.py gateware/src/top/mbsid/fw/src/usb_msc.rs gateware/src/top/mbsid/fw/src/main.rs gateware/src/top/mbsid/M6_USB_STORAGE.md gateware/src/top/mbsid/CLAUDE.md
```

Expected: no whitespace errors and no files outside the declared scope changed by this implementation. Do not stage or commit the overlapping dirty files without the user's explicit baseline/commit decision.

- [ ] **Step 7: Prepare the next hardware handoff without claiming a fix**

Report the archive path, post-route timing result, simulation count, and host-test count. Ask the user to flash the archive and run one export on the same disposable 8GB stick, then return both `rd1` and `rdL` lines including `pth=`. Describe the result as a diagnostic build; no root-cause or fix claim is valid until the hardware `pth` value is captured.

## Self-Review Result

- Spec coverage: the plan measures all five approved fields, exercises a busy-NAKing read CSW, snapshots first/last failures, documents decoding, regenerates the PAC, and performs scoped plus full verification.
- Placeholder scan: every code-changing step contains the exact ports, fields, packing, reset behavior, commands, and expected result needed for execution.
- Type consistency: engine/peripheral byte counts are 10-bit, peripheral words are 8-bit, the packed CSR and Rust snapshot are `u32`, and the same bit positions are used by tests, gateware, firmware, and documentation.
- Scope control: the plan makes no transport, timeout, filesystem, or recovery behavior change and explicitly forbids staging the user's overlapping uncommitted work.
