# MBSID USB Mass-Storage Patch Load/Export — Feasibility + Design (M6)

Status: **M6a hardware-verified. M6b (write/export) root-caused and fixed in gateware
simulation (2026-07-14, same day as the incident); the export path is re-enabled in
firmware for §7b hardware validation, which must run on a disposable drive.** M6a (read)
passed its full hardware checklist (§7a). See the incident writeup + root cause right
below and §8's risk table. **Update (2026-07-15): round seven (below) landed a
handshake-fed watchdog + firmware wall-clock read/write timeouts — simulation/compile
-verified only, hardware validation still pending. The most recent full build's `sync`
post-route Fmax was 59.61 MHz, a FAIL against the 60 MHz target (traced to an unrelated
VexiiRiscv CPU/wishbone critical path, not this round's change, but not yet
re-confirmed) — do not read the round-five 64.46 MHz PASS figures elsewhere in this file
as current.**

## M6b hardware incident (2026-07-14) — write path corrupts real media, DO NOT re-enable

First hardware exercise of the M6b export path (`Card::Usb`'s `Export` row) corrupted a
real USB test drive: the GPT partition table (protective MBR + primary GPT header) and the
FAT32 boot sector (**both** the primary copy and its backup at partition-relative sector 6)
were all zeroed. `gdisk` fully recovered the GPT layer from its intact backup copy, but the
FAT32 boot sector had no surviving copy to restore from (`testdisk` couldn't even identify
the filesystem type without it) — recovery would have needed `photorec`-style raw file
carving; not pursued since the drive was a disposable test unit this time. **This will not
always be true — do not run this on a drive with real data before the root cause below is
fixed.**

What we learned isolating it (via a temporary diagnostic build, since removed — see
`fw/src/usb_msc.rs`'s `write_block` doc comment for the permanent warning):
- `usb_msc.write_block()`'s CSR sequence (`lba` → 128 `tx_data` words → `start_write` strobe
  → poll `resp.done`/`resp.error`) does not reliably write to the LBA it's told to target.
  A round-trip test (read LBA 0, write the *same* bytes back, read again) returned
  `write=false` (the CSR call itself reported failure) and the *following* read also failed
  — i.e. the write attempt left the MSC engine/drive in a state where even reads stopped
  working, consistent with a watchdog-triggered re-enumeration rather than a clean
  no-op failure.
- The corruption was not confined to the single LBA that was actually addressed (0): the
  GPT primary header at LBA 1 and the FAT32 boot sector at the partition's first data LBA
  (34, plus its backup at 34+6) were all zeroed too, despite no code path in this session
  ever issuing a write targeting those LBAs directly. Sampling further into the partition
  found a patchwork of zeroed and intact sectors, not a clean "only LBA 0 changed" result.
  At the time this pointed at the write engine's LBA targeting being unreliable; the root
  cause below supersedes that reading — the LBA path was correct, and the scatter is the
  signature of a bulk-only-transport desync (drive consuming later CBWs as write data).
- The very first fatfs-level attempt (before any raw CSR probing) had already failed with
  `root-fallback` — meaning both `open_dir("MBSID")` and `create_dir("MBSID")` failed, i.e.
  *every* real write attempt through the normal `export_patch` path was already failing via
  this same broken mechanism. The corruption most likely began with that very first ordinary
  `Export` press, not just from the follow-up raw-LBA diagnostic — so the bug is triggered by
  the normal M6b code path, not an artifact of hardware-debugging instrumentation.

### Root cause (found 2026-07-14, via code review + simulation — no hardware touched)

The final-review commit `a232efb` wrapped the CSR TX word FIFO in
`ResetInserter(start_write)` (`src/tiliqua/usb_msc_csr.py`) to flush leftover words from a
prior failed write. But the firmware contract was **fill 128 words, then strobe
`start_write`** — so the strobe that started every write **flushed the just-loaded 512-byte
payload on the same clock edge**. Every hardware write therefore issued a well-formed
WRITE(10) CBW (correct LBA — the LBA math was never wrong; it's byte-identical to the
working read path and sim-covered) and then stalled forever in the engine's `DATA-TX-LOAD`
state with an empty FIFO. The drive was left mid-command awaiting 512 data bytes until the
10 s watchdog hard-reset the host mid-BOT-transfer. Across repeated export attempts and
diagnostic rounds, that is a classic bulk-only-transport desync: the drive consumes
subsequent host traffic (CBWs — 31 bytes, mostly zeros) as pending write data and commits
mostly-zero sectors at effectively arbitrary LBAs. That reproduces every observed symptom:
the very first `Export` press failing (`root-fallback` — all fatfs writes timing out), reads
dying after a write attempt (engine wedged until watchdog re-enumeration), and the scattered
zeroed-sector corruption including LBAs nothing ever addressed.

Why no test caught it: `test_guh_msc_write.py` drives the engine directly with an
always-valid stream (bypasses the CSR FIFO), and `test_usb_msc_csr.py`'s reset-regression
test strobed *then* filled — the opposite of the firmware's order. No test exercised the
actual cross-layer contract.

**The fix (in tree, sim-verified):** the contract is now **strobe-then-fill with a deferred
engine start**. `start_write` only *arms* the write (flushing leftovers and clearing the
sticky resp bits — the original review concern stays addressed); the peripheral holds
`start_write_o` until the TX FIFO actually holds all 128 words, so a WRITE(10) CBW can never
be issued without its full data phase banked — the hazard class that destroyed the drive is
structurally impossible, even if firmware dies mid-fill (result: zero bus traffic, not a
hung command). A read `start` strobe disarms a pending write. Firmware's
`usb_msc::write_block` reordered to match. Regression tests:
`tests/test_usb_msc_csr.py::test_write_contract_strobe_then_fill_defers_start` (the exact
bug — fails against the old gateware), `test_restrobe_after_partial_fill_flushes_and_rearms`,
`test_read_start_cancels_armed_write`.

### Second bring-up round (2026-07-15) — three more engine bugs, found via UART diag + sim

Hardware retest with the fixed contract still failed; a UART0 diagnostic loop (reject
response/phase + CSW status/residue CSR readbacks added for the purpose) found, in order:

1. **CBW NAK treated as rejection** (`CBW-WAIT` had no NAK arm — inherited from upstream
   `guh`, latent on the read path too): a drive doing flash housekeeping NAKs the next CBW;
   the engine failed the whole command (`rej resp=2 phase=1` on hardware). Fixed: re-send
   the identical CBW, same PID; sim test `test_cbw_nak_retries_same_cbw`.
2. **CSW-RX and DATA-RX had no Default arm**: a STALLed CSW (BOT-standard drive behavior
   after a failed command) wedged the FSM until the 10 s watchdog — whose reset also zeroed
   the diagnostic registers, which is why one hardware log showed the self-contradictory
   "error but CSW passed, nothing rejected" (the last attempt's post-reset snapshot had
   overwritten the earlier real failures). Fixed: fail fast, latch reject phase 3 (CSW) /
   4 (DATA-RX).
3. **No REQUEST SENSE after CHECK CONDITION**: BOT drives keep pending sense data and some
   fail/STALL every subsequent command until it's drained — a plausible amplifier of the
   observed failure cascade. Fixed: `USBMSCHost` auto-issues REQUEST SENSE after a failed
   write and exposes key/ASC/ASCQ via the `sense_info` CSR (0x34) — key=7/ASC=0x27 is the
   drive literally reporting WRITE PROTECTED, the leading suspicion for the (tortured,
   reformatted) test stick. Firmware `write_block` now waits for ready instead of
   instant-failing while the sense exchange runs, and the diag keeps the FIRST failure's
   snapshot (the last-only cells destroyed their own evidence once retries ran).

### Third bring-up round (2026-07-15, cont.) — DATA-TX TIMEOUT, SIE exonerated at UTMI level

With rounds one and two fixed, hardware moved to the next layer: the first-failure diag
shows `rej resp=4 phase=2` — the write's 64-byte bulk-OUT **data packet gets no handshake
at all** (TIMEOUT), while 31-byte CBWs on the same endpoint are ACKed. Per USB 2.0 a
device stays silent exactly when it received a corrupt packet.
`tests/test_guh_sie_tx_packets.py` (new) drives the **real** SIE and captures its UTMI TX
byte stream: the 64-byte OUT packet is **bit-perfect** (token + PID + payload + CRC16
verified in Python) — so everything above the UTMI interface is exonerated, leaving the
ULPI translator/PHY (which has never carried a >31-byte host TX packet on this platform —
all guh host traffic to date is tokens and CBWs) or the drive itself. ~~Note reads working
implies the link is FS~~ — **falsified in round four**: the `status.speed` CSR encoding was
mis-documented as 0=FS/1=HS; the real encoding (guh `USBHostSpeed` = LUNA xcvr_select) is
**0=HIGH**, 1=FULL, 2=LOW, 3=UNKNOWN, so the measured `spd=0` meant the link was at High
Speed all along. That inversion steered this round toward FS-only explanations and away
from the actual (HS-only) root cause below.

**Current diagnostic build A/Bs packet length**: `top.py` sets
`usb_msc_tx_chunk_bytes=32` (TEMPORARY — CBW-sized packets are hardware-proven; the
engine's chunking is now parameterized). Outcomes: export works (or drive complains via
CSW/sense about the technically-out-of-spec short intermediate packets) → the failure
below UTMI is length-dependent → instrument the ULPI translator next; still
`rej resp=4 phase=2` with `txdone=0` → length exonerated → drive-side suspicion (A/B a
different stick). `reject_info.txdone` (32-byte units ACKed before the failure) and
`status.speed` are new CSR diagnostics wired into the UART trace.

### Fourth bring-up round (2026-07-15, cont.) — ROOT CAUSE: undecoded NYET handshake

The 31-byte diagnostic build (data packets byte-identical in length to hardware-proven
CBWs) still failed with `rej=4/2/0` — length fully exonerated, leaving *data-phase
context* as the only discriminator. The overlooked variable was link speed: the drive
runs at **High Speed** (`spd=0`, see the encoding correction above), and at HS a bulk-OUT
device may answer a DATA packet with **NYET** — "data accepted, endpoint busy for the
next packet" (USB 2.0 §8.5.1). Flash drives do this routinely on writes as flow control.

The stock guh SIE's `WAIT_HANDSHAKE` decodes only ACK/NAK/STALL; a NYET fell through to
the bus-idle arm and was reported as **TIMEOUT** — which the engine's Default arm treated
as rejection. Every observation fits: reads are immune (NYET exists only for OUT data),
CBWs get plain ACKs (tiny packet, device buffer always free), sim was green (no device
model ever spoke NYET), and the failure was independent of packet length and data toggle.
LUNA's `USBHandshakeDetector` had a `detected.nyet` strobe all along; guh never read it.

**Fix (sim-verified end to end):** `guh/usbh/sie.py` vendored to `src/vendor/guh_msc/sie.py`
(same rationale/pattern as `msc.py`; swapped into the stock enumerator at `SCSIBulkHost`
construction) with `TransferResponse.NYET = 7` + a `detected.nyet` decode arm in
`WAIT_HANDSHAKE`; the engine's `CBW-WAIT` and `DATA-TX-WAIT` treat NYET exactly like ACK
(the packet WAS accepted — the "busy" half is covered by the existing NAK-replay on the
following transaction; the optional HS PING protocol is deliberately skipped, devices
tolerate OUT→NAK). Tests: `test_guh_sie_tx_packets.py` now injects real device handshakes
at the UTMI level (ACK as harness control, NYET as the fix proof — pre-fix it read
TIMEOUT), and `test_usb_msc_integration.py` gained a firmware-exact write against a drive
that NYETs the CBW and every data packet. **Diagnostic lesson:** TIMEOUT in a reject diag
means "no response *we decode*", not "no response" — and a mis-documented encoding
(`speed`) can quietly veto the correct hypothesis class; verify enum encodings at the
source, not the comment.

### Fifth bring-up round (2026-07-15, cont.) — mid-data STALL, missing BOT clear-halt recovery

With the NYET fix flashed (and the 64-byte chunks restored), two different sticks produced
two *different* failures — the NYET fix demonstrably peeled a layer (data packets are now
accepted where before none were):

1. **8GB MBR stick: `rej=3/2/4`** — STALL, in DATA-TX, after 4×32B units (= exactly two
   64-byte packets) were ACKed. The drive **halts its bulk-OUT endpoint mid-data-phase**
   (legal per BOT §6.7.3 — a device that already knows the command fails may truncate the
   data phase this way). The engine had no CLEAR_FEATURE(ENDPOINT_HALT) recovery at all,
   so everything after was collateral: retries 2–4 STALLed on the CBW itself
   (`rej=3/1/4` — same halted endpoint), the auto REQUEST SENSE couldn't run
   (`sense valid=0`), the 10 s watchdog fired, and re-enumeration flailed
   (`conn=1 rdy=0`, then device gone — see also the new
   `.scratch/mbsid-usb-hotplug-redetect-broken/` issue).
2. **64GB GPT stick: `d_wrto=4`, `rej=0/0/0`** — a hard wedge with **zero evidence**: the
   engine never rejected anything, the firmware's 10M-spin poll timed out, and the
   watchdog reset that eventually ended the wedge **zeroed the diag CSRs before firmware
   could read them** — round two's "diagnostics destroy their own evidence" trap striking
   again through a different path (that round fixed last-vs-first latching firmware-side;
   this one is the reset domain itself wiping the source registers).

**Fixes (sim-verified end to end, `tests/test_usb_msc_integration.py`):**

- **BOT error recovery in the engine** (`src/vendor/guh_msc/msc.py`): on a DATA-TX STALL,
  issue CLEAR_FEATURE(ENDPOINT_HALT) on the OUT endpoint via a control transfer on ep0
  (SETUP DATA0 + IN status stage, driven through the same pass-through SIE interface the
  bulk states use), reset the endpoint's data toggle to DATA0 (USB 2.0 §9.4.5), then
  **read the CSW** — per BOT §6.7 the CSW after a clear-halt reports *why* the device
  bailed, and a failed CSW then flows into the round-two auto-REQUEST-SENSE, so
  `sense_info` finally gets populated instead of staying invalid. On a **CSW STALL**,
  clear the IN endpoint's halt and retry the CSW read exactly once (BOT §6.7.2); a second
  STALL rejects promptly (`rej` phase 3). A failed/rejected clear-halt itself latches
  phase **5** (CTRL).
- **Watchdog-proof diagnostics**: every diag (reject response/phase/txdone, NYET count,
  sense) is now **latched in the CSR peripheral** (`usb_msc_csr.py`), outside the engine's
  watchdog reset domain, on *change-to-nonzero* (the engine zeroes each per command, so
  stale values can't re-latch after the peripheral's clear-on-strobe; the watchdog's wipe
  arrives as a change to zero and is ignored — preserving exactly the evidence it used to
  destroy). New `reject_info.last_phase` field: the last live engine phase seen — on the
  next 64GB-style wedge this reports **which phase the engine was stuck in** even though
  no reject ever latched.
- **NYET counter** (`reject_info.nyets`): counts NYET handshakes per command. Purpose: the
  skipped HS PING protocol is the alternative suspect for the 8GB stick's STALL (a strict
  device could STALL an OUT sent while busy instead of NAKing it) — a STALL with
  `ny=0` rules PING out; `ny>0` moves it up the list. The UART diag line now prints
  `rej=r/p/t ny=N lph=P` for both first and last failure.

**Open question for the next hardware round:** whether the 8GB stick's STALL is a genuine
SCSI-level abort (CSW + sense will now say — e.g. write-protect key=7/asc=0x27) or a
PING-protocol objection (`ny>0` on the STALLed command). The 64GB wedge needs its
`lph` phase breadcrumb read before theorizing. Also check the write-protect posture of
both sticks on a PC (`lsblk -o NAME,RO`; some sticks expose a hardware RO switch).

### Sixth bring-up round (2026-07-15, cont.) — FIRST SUCCESSFUL WRITE; read-after-write fails; PAUSED to regroup

With round five's clear-halt + diag build flashed, the 8GB stick completed a real
WRITE(10) with a passing CSW — **the first successful device write in the project**
(`d_wr=1 d_wrok=1`, ~38k spins). PC-side `fsck.vfat` confirmed the write landed: one FAT
copy updated, 4 orphaned clusters reclaimed = the allocation for a file whose directory
entry never got written. The export still fails: the **first two device reads after that
write fail deterministically** (`d_rd=7 d_rderr=2`, byte-identical counts across two runs
— including one with the read spin cap raised 1M→10M, which rules out simple impatience at
that scale), aborting the FAT flush mid-sequence. The drive stays healthy afterwards
(keepalive reads ran fine for ~70 s until unplug). The 64GB stick meanwhile showed the new
diags working as designed: `ny=3 lph=3` = whole data phase accepted (routine HS NYET flow
control), engine parked in the CSW phase while the drive busy-NAKed status for longer than
every timeout — round six raised firmware polls (reads 1M→10M spins ≈3 s, writes 10M→40M
≈12 s so the engine's 10 s watchdog decides, not the poll).

**Sim exonerates the gateware for the 8GB case**: a firmware-exact `write_block` →
`read_block` sequence through the full glue passes
(`test_read_immediately_after_successful_write` — READ(10) correctly routed after a
write, block intact), so the failure needs real-drive behavior the stub doesn't model.
**Known evidence gap**: the `rej=/ny=/lph=` cells in the export log are only captured on
*write* failures — the two read failures recorded nothing. Round six adds read-failure
diagnostics (`fw/src/usb_msc.rs` `rd_fail`/`rd_fail_first`, printed as `export: rd1/rdL
rsn=… w=… lba=… sp=… rej=…/… ny=… lph=…`): reason 1=not-ready at entry, 2=engine error
(reject snapshot says where), 3=spin timeout (sp says how long we waited). Next hardware
run discriminates: rsn=3/high-sp = drive busy-NAKs reads after a write longer than 3 s;
rsn=2 = engine-level rejection (CSW failed / bus reject, snapshot decodes it); rsn=1 =
engine still busy from the previous command.

**Status: investigation PAUSED here (user call, 2026-07-15) — no fix attempted for the
read-after-write failure; diagnostics are in the flashable archive awaiting the next
round.**

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

#### Round-six diagnostic results (2026-07-15 hardware run, 8GB stick)

```
export: begin P000.SYX got=1 rdy=1 conn=1 bs=512
export: ok=0 mount=1 d_rd=7 d_rderr=2 d_wr=1 d_wrok=1 ... spins=35533
export: rd1 rsn=3 w=0 lba=2048 sp=10000000 rej=0/0 ny=0 lph=3 pth=00000000
export: rdL rsn=1 w=0 lba=32560 sp=0 rej=0/0 ny=0 lph=3 pth=00000000
usb: conn=0 rdy=0 bs=512 spd=3 t=99961ms
```

`pth=00000000` on both snapshots. Decoded carefully, this run settles more than it
looks like it does:

1. **The Tiliqua datapath is exonerated.** `periph_bytes`/`periph_words` live in the
   CSR peripheral's `sync` domain, are immune to the engine watchdog, and reset only
   on start strobes — and no start strobe happened after `rd1`'s (the `rdL` call
   failed the ready check *before* strobing, and the keepalive stops once
   `drive_ready` drops). So `periph_bytes=0` is trustworthy across the whole failure
   window: **not one data byte crossed into the peripheral for the post-write
   READ(10)**. The engine's stream FIFO drains combinationally into the peripheral
   (`rx_data.ready` is tied high), so the engine itself also received ~0 bytes from
   the SIE. Packer/FIFO/CSR/firmware legs of the interpretation table are all ruled
   out — the loss is at the USB transaction level: the drive never delivered data.
2. **The engine-side live fields were wiped by the watchdog — the snapshot happened
   too late.** `stream_mode=0` + `data_len_512=0` cannot be a live mid-read
   observation: both are latched from the command in `IDLE` (READ sets
   `stream_data=1`, `data_len=512`) and `data_len` never decrements. The outer FSM
   also accepted the read start cleanly (it was in `READY`; the write had completed
   with a passing CSW). So by snapshot time the engine had been watchdog-reset:
   **`MAX_SPIN=10_000_000` outlasts the 10 s watchdog** — each spin does two 32-bit
   CSR reads (`status`, `resp`), each serialized as 4 byte-wide CSR bus accesses, so
   the loop is far slower than the ~3 s the `usb_msc.rs` comment estimated.
3. **`lph=3` is NOT the failing read's phase.** `last_phase` is last-change-wins and
   was overwritten by the post-watchdog recovery: after re-enumeration the outer FSM
   issues TEST UNIT READY (`data_len=0`, `stream=0`), which goes CBW→CSW directly —
   and wedged in the CSW phase too. That also explains `rdL rsn=1` (engine never got
   back to `READY`) and the final `conn=0 spd=3` (enumeration eventually lost; with
   hotplug re-detection broken it stays lost).

**Net picture:** the stick accepts WRITE(10)+data+CSW, then goes into a busy/wedged
state where it NAKs the following READ(10) exchange (no data, no STALL, no reject)
for >10 s, and after the watchdog reset it still won't complete a TEST UNIT READY.
This is drive-side/transport behavior, not a Tiliqua data-path bug.

**Gaps for the next diagnostic round** (the round-six plan's "snapshot happens ~3 s
in, before the watchdog" assumption is disproven):

- Latch the read-path fields watchdog-immune in the peripheral (same
  change-to-nonzero pattern as `reject_info`), and/or take an *early* firmware
  `pth` snapshot a fixed spin count in (e.g. 100k spins, safely pre-watchdog) in
  addition to the final one.
- A phases-seen OR-mask (5 bits, frozen when `connected` falls = watchdog fired)
  would pin whether the failing read wedged at CBW (drive NAKed the command) or at
  DATA (CBW accepted, data-IN NAKed forever) — `last_phase` alone can't, because
  recovery traffic overwrites it.
- Calibrate spins→ms once via Timer0 so `sp=` values convert to wall time.
- Candidate *fix* directions (not diagnostics): BOT Bulk-Only Mass Storage Reset
  recovery (never issued today — §6.7's reset recovery is class request 0xFF +
  clear both halts, distinct from the clear-halt-only path added in round five),
  and re-examining whether a drive that busy-NAKs reads for >10 s after a write
  simply needs the watchdog/retry budget rethought around FTL commit time.

**Testing lesson encoded in `tests/test_usb_msc_integration.py`:** every per-layer test was
green while the assembled stack failed on hardware — the CSR peripheral, the engine, and the
`top.py` command glue had never been simulated *together*, and the glue had zero sim coverage.
The integration suite drives the firmware's exact CSR sequence against a scripted
"disagreeable drive" (NAKed CBWs/CSWs, failing CSWs, STALLed CSWs, auto-sense exchange).

**Current state:** `PressResult::UsbExport` in `fw/src/main.rs` is re-enabled (real export
logic restored) for §7b hardware validation. Run the checklist **on a disposable/scratch
medium, never a drive with data**, stopping at the first sign of failure (see the memory
`mbsid-usb-write-test-destroyed-drive` for the postmortem of how the incident happened).
If exports still fail with `sense key=7 asc=27`, the stick itself is write-protected —
retry on a different disposable drive before suspecting the gateware further.

**M6a (read).** Browse a FAT drive from the menu, load = audition, Load→Slot = audition +
persist to a User bank slot. Built end-to-end: gateware (`usb_msc` CSR at `0x1300`, dual USB
engines behind a UTMI mux per §3's Option A, decided without needing the Option B fallback),
firmware (MSC block-read driver, FAT adapter, `usb_patch.rs` file finder/loader, file-mode
SysEx parser, settings persistence, `Card::Usb` menu card, main-loop wiring).

**M6b (write/export).** Export the live EDIT buffer or any User-bank slot as a standard MBSID
v2 single-patch SysEx `.syx` file back to the drive. Built end-to-end: gateware (vendored
`guh` MSC engine at `src/vendor/guh_msc/msc.py` extended with SCSI WRITE(10) + a bulk-OUT
`DATA-TX` state, `USBMSCPeripheral(with_write=True)`'s `tx_data`/`start_write` CSR pair),
firmware (`usb_msc.rs`'s `write_block`, `fat.rs`'s write-back sector cache, `usb_patch.rs`'s
`export_patch`/`encode_syx`, the menu's `Export` row on `Card::Usb`). Landed across five
commits including two review-round fixes (a CSR read-after-write timing bug on the write
path, a partial-write-progress bug in the FAT write-back cache) — see `CLAUDE.md`'s M6b
gotcha block for both.

Host tests are green (118/118, `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib`)
and a full bitstream build with **both** the M6a read path and the M6b write path included
passes timing — see `CLAUDE.md`'s status line for the current numbers. **Neither M6a nor M6b
has been exercised on real hardware** — see the hardware checklists below (§7a for M6a, §7b
for M6b).

Original investigation below is kept for context. Loading patches *from* a USB drive turned
out cheap as predicted — the whole read-only MSC stack (gateware + FAT firmware) already
existed, proven, in `top/sid_player_sw`, and M6a is a straight port of it plus the UTMI mux.
Exporting patches *to* a USB drive still requires new gateware: the vendored `guh` MSC host
engine is **read-only by design** (its SCSI FSM implements READ(10) only, and its CBW builder
cannot even express a host→device data phase). Write support means forking/vendoring `guh`'s
MSC engine and adding a SCSI WRITE(10) + bulk-OUT data path — unchanged from the original
assessment, not attempted in M6a.

Export is nevertheless the strategically important half: **this device has no other patch
egress.** TRS MIDI is RX-only in hardware (`src/tiliqua/platform.py:185` — the `midi`
resource has only an `rx` subsignal on every board revision), the USB-C port is a host port
(can never appear as a MIDI device to a PC, `CLAUDE.md`), and M4 already documents "No MIDI
TX — ACK/DISACK is swallowed". A user-edited patch saved to the M4 flash bank currently can
never leave the module except via debug flash tooling. USB drive export closes that gap.

### Seventh round (2026-07-15) — handshake-fed watchdog + wall-clock firmware timeouts (behavior change, not just diagnostics)

Round six established that the drive never sent one data byte for the post-write
READ(10) and that the engine's completion-fed 10 s watchdog reset the engine (and
wiped the live diagnostics) while firmware was still polling. Round seven changes
behavior accordingly — direction (a) of the two fix candidates; BOT Reset Recovery
is direction (b), not attempted here:

- **Handshake-fed watchdog** (`vendor/guh_msc/msc.py`, `resp_live`): the watchdog
  is held cleared while the SIE's last transaction ended ACK/NAK/NYET — a NAKing
  drive is present and flow-controlling (FTL commit after a write) and now gets
  unbounded engine-side patience. TIMEOUT/NONE (silent bus = unplug), STALL
  (clear-halt paths own it; a stall-everything drive should re-enumerate), and
  CRC/OVERFLOW still count. No new ports or CSRs. Sim:
  `test_read_survives_nak_wait_longer_than_watchdog` (fails against the old
  engine) + `test_watchdog_still_fires_on_silent_drive` (unplug guard).
- **Firmware wall-clock timeouts** (`fw/src/uptime.rs` + `usb_msc.rs`): spin caps
  were uncalibrated (round six's "10M spins ~ 3 s" was actually >10 s — 2
  byte-serialized CSR reads per spin); read/write polls now budget
  `READ_TIMEOUT_MS`/`WRITE_TIMEOUT_MS` = 30 s of Timer0 1 ms uptime, with an
  early abort (new read reason **4**) when `connected` drops mid-command.
  Failure lines print `ms=` beside `sp=`, giving spins->ms calibration for free
  (`wms=` on the export summary line does the same for writes).

**What the next disposable-media run tells us:** with the watchdog no longer
firing mid-NAK-wait, the failing verify read either (1) completes after N s —
the export just works and `wms=`/keepalive logs bound N; (2) fails at rsn=3 with
`ms=30000` and a **live** `pth` (the engine is no longer reset, so
engine_bytes/stream_mode/data_len_512 are finally trustworthy — decode against
the round-six table); or (3) fails rsn=4 (engine lost the drive: the drive
itself dropped off the bus, which no host-side patience fixes and which argues
for direction (b)'s BOT Reset Recovery); or (4) a drive that NAKs a data phase
forever — never completing, never going silent — keeps the engine's watchdog
held cleared by design (that's the point of this round), so it never fires, but
firmware's 30 s wall-clock deadline still bounds the *firmware* hang: at 30 s
firmware gives up (rsn=3), while the gateware engine itself is left parked
mid-command (`status.busy`/ready only clears when the engine's own FSM reaches
READY, which it never will while the drive keeps NAKing) — the drive shows
`not ready` afterward with no automatic recovery, and a bitstream restart is
the only way out today. This is exactly the direction-(b) BOT Reset Recovery
gap this round scoped out, not attempted here. UI note: the export runs synchronously,
so worst case the menu now freezes up to ~30 s per failing read — expected, not
a hang; the 8.3/`BUSY` caveats from the M6b gotcha block still apply.

## 1. Goal & non-goals

Goals:
- **M6a (read):** browse `*.syx` patch files on a FAT-formatted USB drive from the menu,
  load one into the engine (audition) and optionally commit it to an M4 user-bank slot.
- **M6b (write):** export the currently-loaded/edited patch, or any user-bank slot, as a
  standard MBSID v2 SysEx dump file on the drive — interchangeable with zetaSID/MIOS Studio
  tooling and re-sendable over MIDI from a PC.
- Keep `top/sid` and `top/sid_player_sw` unaffected (same opt-in pattern as M4's
  `forward_sysex`).

Non-goals:
- USB hubs (the `guh` enumerator is single-device), multi-LUN devices, exFAT/NTFS
  (FAT12/16/32 only, whatever `fatfs` 0.4 mounts), or USB I/O anywhere near the audio path
  (all file access runs in the main loop, never the Timer0 ISR).
- Simultaneous USB MIDI + USB storage. One physical port, one plugged device — the modes are
  inherently exclusive at the connector (see §3).
- Whole-bank import/export in one file (single-patch files first; a 128-patch bank file
  format can layer on later without gateware changes).

## 2. Source-verified constraints (why the design is shaped this way)

- **`guh` MSC engine is read-only.** `guh/engines/msc.py` (`USBMSCHost` docstring: "read-only
  block device interface … TODO: write support?"). Harder than the TODO suggests at the
  transport layer: `SCSIBulkHost` sets `bmCBWFlags = Mux(data_len > 0, DATA_IN, DATA_OUT)`
  (msc.py:192) — any command with a data phase is assumed device→host, and the FSM's only
  data state issues bulk-IN transfers. BUT the SIE primitive for bulk OUT already exists and
  is exercised on every command: the CBW itself is transmitted via `enum.ctrl.txs` + a bulk
  OUT transfer (`CBW-LOAD`/`CBW-XFER` states). Write support = a direction bit in
  `SCSIBulkHost.Command`, a `tx_data` stream input, and a `DATA-TX` state that streams 512
  payload bytes through the same `txs` path, plus WRITE(10)=0x2A in the opcode enum and a
  write leg in `USBMSCHost`'s FSM. Moderate, well-bounded gateware work.
- **`guh` is a pinned pip git dependency** (`pyproject.toml:15`, `guh @ git+…@d44315`,
  BSD-3-Clause). We cannot patch it in-place; write support means vendoring the touched
  modules (repo precedent: `src/vendor/vexiiriscv/`) or maintaining a fork and repinning.
  Vendoring `engines/msc.py` alone is enough — it only imports stable `guh.usbh.*` internals.
- **One ULPI PHY, currently owned by USB MIDI.** `SIDSoc.elaborate` requests
  `platform.default_usb_connection` and instantiates `USBMIDIHost` unconditionally on hw
  (`../sid/top.py:617-618`). A second `platform.request` of the same resource is impossible;
  MSC needs either a mux in front of two engines or a combined engine (§3).
- **Area headroom is the #1 risk.** mbsid is at **80% LUT** (19444/24288 `TRELLIS_COMB`,
  `build/mbsid-r5/top.tim`), sync Fmax 61.76 MHz vs 60 target. sid_player_sw carries one MSC
  host at 84%. Both class engines are thin FSMs (~130 lines MIDI, ~540 lines MSC+SCSI) over
  the shared-by-design heavy part (`USBHostEnumerator` + SIE + ULPI + descriptor parser,
  ~1.6k lines of Amaranth). Instantiating both engines duplicates the heavy part.
- **`usb` and `sync` are the same 60 MHz clock** (`src/tiliqua/pll.py:277` "sync, usb: 60 MHz
  (Main clock)"). New USB-side logic lands on the same timing budget that's passing by only
  1.76 MHz — keep added FSMs shallow, register CSR-crossing paths (root `CLAUDE.md` MULT
  lesson applies).
- **The read-side firmware stack is done and host-tested** in `sid_player_sw/fw/src/`:
  `usb_msc.rs` (57-line CSR block-read driver with spin caps), `partition.rs` (MBR/GPT →
  first-partition LBA), `fat.rs` (`fatfs` 0.4 no_std adapter, single-block cache, writes
  stubbed to error), `sid_scan.rs` (root-dir extension scan + load-by-index, host-tested
  against an in-memory FAT image). All directly reusable with the extension changed to
  `.SYX`.
- **A `.syx` patch file is exactly the byte stream `SysexCapture` already parses.** zetaSID
  patch files are MBSID v2 SysEx dumps (`F0 00 00 7E 4B 00 …`, cmd 0x02, 1024 nibblized
  bytes = the same 512-byte `sid_patch_t` the whole port runs on). File import can feed file
  bytes through the existing parser (relaxed to accept any bank/type for file mode, §6c);
  file export is the trivial inverse (nibblize 512 bytes + 7-bit checksum). No new format.
- **RAM fits.** Measured peak stack 4016/25824 B post-M4 (~21.8 KB headroom,
  `M4_USER_PATCH_BANKS.md §6f`). sid_player_sw runs the identical fatfs stack in a *smaller*
  mainram (0x4000 vs mbsid's 0x8000). Budget adder here: `FileSystem` object + one 512 B
  sector cache + one 512 B patch buffer + dir-scan name list ≈ 2–3 KB. Re-measure with the
  stack-paint probe at implementation.
- **`fatfs` 0.4 (git, `default-features=false`) includes write support in no_std** — the
  write path is not feature-gated; sid_player_sw simply stubs `Write::write` in its adapter.
  M6b un-stubs it (read-modify-write on the sector cache + dirty write-back), no new crate.

## 3. USB port sharing: mode switch, not concurrency

Only one device can occupy the USB-C port, so "MIDI keyboard" vs "thumb drive" is already a
physical either/or. Model it as an explicit **USB Mode: MIDI / Storage** setting:

- New menu row (Main card) + persisted in the M5 settings record (bump `settings_store.rs`
  version; unknown/old records default to MIDI — same corrupt-record-→-defaults contract).
- Storage mode forces the effective MIDI source to TRS (TRS MIDI keeps working while a drive
  is plugged — you can still play while browsing patches). The existing `usb_midi_host` CSR
  semantics stay; a new mode bit gates which engine owns the PHY and asserts `usb_vbus_en`
  in *both* modes (today VBUS is only driven in USB-MIDI mode, `../sid/top.py:627` — a drive
  needs it too).
- Mode switch resets the newly-selected engine so it re-enumerates from scratch (both `guh`
  engines already self-reset via watchdog `ResetInserter`s; wire the mode bit into the same
  reset term).

Two gateware shapes, to be decided by a **probe build (Phase 0)**:

**Option A — two engines + ULPI mux (preferred if it fits).** Instantiate `USBMIDIHost` and
`USBMSCHost` side by side; a CSR-driven mux hands the ULPI signals to one and parks the
other in reset. Zero changes to either engine for M6a. Cost: a duplicated enumerator/SIE —
estimate +2.5–3.5k LUTs on a design at 80% → ~92–95%, plausibly routable on ECP5 but with
real Fmax risk. Cheap to try: one probe build answers it definitively.

**Option B — combined host, shared enumerator (fallback).** Vendor a `USBDualModeHost`:
one `USBHostEnumerator`/SIE, the descriptor parser's class/subclass/protocol match constants
made runtime-`Mux`ed from the mode bit (they are plain equality compares in
`guh/usbh/descriptor.py`), and both thin class FSMs on top with only one active. Adds
~1–1.5k LUTs. More surgery (touches `descriptor.py` matching, needs the enumerator's
`parser` handling audited for endpoint-filter differences: MIDI filters IN-only, MSC needs
IN_AND_OUT), but this is the shape upstream `guh` would likely accept as a PR.

Decision rule: run Phase 0 with Option A; accept it if post-route sync Fmax ≥ 60 MHz with
≥ 1 MHz margin across 2 seeds, else fall back to Option B.

**Outcome (M6a implementation): Option A shipped.** `../sid/top.py`'s `SIDSoc.elaborate`
instantiates `USBMIDIHost` and `USBMSCHost` with `bus=None` behind one shared
`UTMITranslator`, muxed by `usb_msc.mode_o`, each wrapped in a `ResetInserter` keyed off the
mode bit so the unselected engine sits in reset. Option B (shared enumerator) was never
needed — see `CLAUDE.md`'s status line for the build's post-route sync Fmax against the
60 MHz target.

## 4. Gateware

### 4a. M6a (read-only) — pure reuse

- Copy `USBMSCPeripheral` from `sid_player_sw/top.py:61-170` into a shared module (e.g.
  `src/tiliqua/usb_msc_csr.py`) or import it; register at CSR **`0x1300`** on the mbsid SoC
  (`0x1000` = SID_PERIPH, `0x1200` = SID_PERIPH_R). Add the mode bit + mux per §3 behind a
  `SIDSoc` opt-in flag (e.g. `with_usb_msc=False` default) so `top/sid` and existing tops
  elaborate byte-identically — the M4 `forward_sysex` pattern.
- **This is a CSR change:** `pdm mbsid build --pac-only` before `--fw-only` (root CLAUDE.md).

### 4b. M6b (write) — vendored MSC engine + TX path

Vendor `guh/engines/msc.py` → `src/vendor/guh_msc/msc.py` (BSD-3 header kept) and extend:

- `SCSIBulkHost.Command` gains `data_dir` (0=IN, 1=OUT); `bmCBWFlags` derives from it, not
  from `data_len > 0`. New `tx_data: In(stream.Signature(Packet(unsigned(8))))` + a
  `DATA-TX` state: per 512-byte chunk, stream bytes to `enum.ctrl.txs` (`CBW-LOAD` pattern),
  issue `start_bulk_out(endp_out)`, toggle `pid_out` on ACK, retry chunk on NAK. Then `CSW`
  as today.
- `USBMSCHost`: `SCSIOpCode.WRITE_10 = 0x2A`; `cmd` gains a `write` flag; `READY` dispatches
  to a `WRITE`/`WRITE-WAIT` leg mirroring `READ`/`READ-WAIT` (`_BLOCKS_PER_READ = 1` — one
  block per command keeps the FSM and FIFO sizing trivial; patch export is ~1.1 KB, write
  throughput is irrelevant).
- `USBMSCPeripheral` gains: `tx_data` CSR (W, 32-bit, fills a 128×32 word FIFO), a
  `start_write` strobe (the peripheral unpacks words → bytes little-endian, symmetric to the
  RX packer), and `resp` reused as-is. Firmware contract **(revised post-incident — the
  original fill-then-strobe order was the incident's root cause, see the writeup at the top)**:
  strobe `start_write` first (arms: flushes leftover TX words, clears sticky resp), then fill
  128 words; the peripheral defers the engine start until the 128th word is banked, so a
  WRITE(10) CBW is never issued without its full data phase. Then poll `resp`.
- Error handling stays retry-at-firmware-level: on `resp.error`, firmware re-issues the
  block write once, then fails the file operation visibly (no silent success). REQUEST_SENSE
  refinement can come later; the CSW status already distinguishes success/failure.
- Sim test: extend/copy whatever drives `USBMSCHost` today (check `guh` upstream tests) with
  a mock SIE asserting: CBW bytes carry flags=0x00 + opcode 0x2A + correct BE LBA, 512
  payload bytes emerge in order on bulk OUT, CSW consumed, `resp.done/error` correct, NAK
  mid-data retries the chunk with the same PID sequence.

## 5. On-disk format & directory layout

- Directory: `/MBSID/` on the first FAT partition (created on first export if absent; import
  also falls back to scanning the root dir so hand-copied files Just Work).
- File format: **standard MBSID v2 single-patch SysEx dump** (`F0 00 00 7E 4B <dev> 02 …
  F7`, 1024 nibblized data bytes + 7-bit checksum) — byte-compatible with zetaSID patch
  `.syx` files, MIOS Studio, and our own `sysex_capture.rs` framing. A file exported by
  Tiliqua can be re-imported, sent to real MBSID hardware, or pushed back over TRS MIDI by
  any PC tool, unchanged.
- Export naming: `Pnnn_<name>.SYX` (8.3-safe: `Pnnn~1.SYX` via fatfs LFN off is fine too —
  decide at implementation; `nnn` = user-bank slot or `EDT` for the live edit buffer;
  `<name>` = patch body bytes 0..16, sanitized to FAT charset).
- Import accepts any file whose SysEx body parses (bank/type bytes ignored in file mode) —
  plus, as a convenience, raw 512-byte files (exact size match) treated as a bare
  `sid_patch_t`.

## 6. Firmware

### 6a. Reused wholesale (from `sid_player_sw/fw/src/`, adjusted paths only)

- `usb_msc.rs` — block-read driver (spin-capped). M6b adds `write_block(lba, &[u8;512])`.
- `partition.rs` — MBR/GPT first-partition LBA. Unchanged.
- `fat.rs` — `MscStorage` adapter. M6a unchanged (writes error). M6b: dirty flag on the
  512 B cache, `Write::write` mutates the cache via read-modify-write, `flush()` +
  sector-boundary crossings write back. Keep the single-sector cache — patch files are tiny.

### 6b. New: `fw/src/usb_patch.rs` (host-testable like `sid_scan.rs`)

- `list_syx(fs, out)` — scan `/MBSID/` (fallback root) for `.SYX`/512-byte files, bounded
  list (name + size), same shape as `list_root_sids`.
- `load_syx_by_index(fs, ix, &mut [u8;512]) -> Option<()>` — read file, run bytes through
  the file-mode SysEx parser (§6c) or accept raw 512.
- M6b: `export_syx(fs, name, &patch512)` — nibblize + checksum + write, `flush`, verify by
  reading back and re-parsing (cheap end-to-end check that the write path actually landed).

### 6c. `sysex_capture.rs` — file mode

Add a constructor/flag `SysexCapture::file_mode()` that relaxes the accept condition to any
cmd-0x02 patch dump (ignore bank/type/patch-number match; still enforce header, nibble
count, checksum, F7). The ISR/live path keeps today's strict bank-1 rule. Host tests: strict
mode rejects what it rejects today; file mode accepts a factory-bank dump and a zetaSID file
fixture; both reject a corrupted checksum.

### 6d. Menu (`menu.rs`) — USB card

- Main card: `USB Mode: MIDI | Storage` row (persisted, §3).
- New `Card::Usb` (visible only in Storage mode, same collapse pattern as
  `lead_loaded`-gated rows — and remember the M5 lesson: derive visibility from live state
  every loop iteration, not from cached menu state): rows = drive status (`No drive` /
  `Ready N files`), file selector (name scroll), `Load` (audition — engine only), `Load→Slot
  nnn` (audition + M4 `UserPatchStore::save`), and M6b `Export: EDIT|Slot nnn → USB`.
- All USB/FAT I/O runs in the **main loop** on menu commands (Timer0 ISR keeps ticking the
  engine; a slow drive stalls only UI redraw — show a `BUSY` row state, don't redraw-spam per
  the DMAFramebuffer dirty-flag rule). The `read_block` 1M-iteration spin cap bounds the
  worst-case stall to well under a second per block.

### 6e. `main.rs` wiring

- Instantiate `UsbMsc` from the new PAC block; plumb into menu dispatch. Mount lazily on
  first USB-card entry (mount = partition scan + BPB read ≈ a handful of blocks), drop the
  `FileSystem` on drive-removed (`status.connected` low) or mode switch.
- Loaded-patch plumbing reuses the M4 path exactly: a file-sourced 512-byte image goes
  through the same `mbsid_bank_write`-equivalent entry the SysEx capture path uses today
  (patch → engine + optional flash slot), so engine-side behavior is provably identical to a
  MIDI upload of the same bytes.

### 6f. Footprint (re-verify at implementation)

- +`FileSystem` + caches + name list ≈ 2–3 KB (.bss/stack, main-loop only). Stack-paint
  re-measure per root CLAUDE.md; 21.8 KB headroom expected to absorb it easily.

## 7. Phasing & validation

**Phase 0 — probe build (½ day, no firmware).** mbsid + `USBMIDIHost` + `USBMSCHost` +
ULPI mux (Option A). Judge LUT% + post-route sync Fmax over 2 seeds (read the *second*
`Max frequency` line in `top.tim`). Decides Option A vs B before any real work.

**M6a — load (read-only).** Gateware §4a + firmware §6 minus export. Host tests: fat/scan/
parser suites (all runnable on PC, fixtures = zetaSID `.syx` + generated FAT images, same
harness as `sid_scan.rs`) — part of the **118/118 green** host suite, see `CLAUDE.md`.
Gateware built and passing timing — see `CLAUDE.md`'s status line for the current post-route
sync Fmax. **Not yet run on real hardware.**

### 7a. M6a hardware checklist (record results here once hardware is available)

Plain checklist, not something executable in this environment — no hardware is available
here. Walk this in order on a real Tiliqua r5 with a USB-C-to-A adapter and a FAT32 thumb
drive containing a few `.syx` files under `/MBSID/`:

- [x] **Drive enumerates.** Switch `USB Mode` to `Storage`, plug in the drive, open the
  `Usb` card. `Drive` row goes `No drive` → (briefly `BUSY`) → `Ready (N files)` with `N`
  matching the number of `.syx`/512-byte files actually on the drive.
- [x] **Drive stays Ready while idle (≥ 30 s).** First real-hardware bug (2026-07-14): the
  MSC engine's 10 s watchdog is only fed by a completed SCSI command and keeps counting in
  its `READY` state (inherited from stock `guh`, not an M6b regression), so an idle drive
  was hard-reset + re-enumerated every 10 s (`Ready` → `No drive` → scanning → `Ready`
  loop). Fixed firmware-side: `main.rs` reads LBA 0 every 2 s while `drive_ready`
  (keepalive), chosen over silencing the watchdog in gateware because the watchdog reset is
  the *only* unplug detection the enumerator has (`enumerated` is set once, never cleared).
  Leave the `Usb` card open and untouched for 30+ s — `Drive` must stay `Ready`.
- [x] **Files listed correctly.** Scroll the `File` row through all `N` entries; names match
  the files on the drive (spot-check a few, including one in `/MBSID/` and, if tested, one
  falling back to the root-dir scan).
- [x] **Load auditions correctly.** Pick a file, commit on `File` (audition-only load).
  Compare by ear (and ideally by SID-register capture) against the *same* patch sent via TRS
  SysEx RAM Write — must sound/diff identical, since both paths land in the engine through
  the same entry point.
- [x] **Load→Slot persists across power cycle.** Pick a file, commit on `Load>Slot` into a
  chosen User slot. Power-cycle the module, switch to Bank `User`, select that slot — the
  patch loads and sounds the same as it did on first load.
- [x] **Unplug mid-browse degrades cleanly.** With the `Usb` card open and a file selected,
  physically unplug the drive. `Drive` row falls back to `No drive`, `File` shows `-`, no
  hang/freeze, encoder navigation keeps working, audio keeps playing throughout.
- [x] **Mode switch back to MIDI re-enumerates a keyboard.** With a drive plugged and then
  removed (or still plugged), switch `USB Mode` back to `MIDI`, plug in a MIDI keyboard/
  controller — it enumerates and plays normally, same as before M6a existed.
- [x] **TRS MIDI keeps playing in Storage mode.** With `USB Mode` = `Storage` and a drive
  plugged, play notes over the TRS MIDI input — audio responds normally the whole time,
  including while a load is in progress.
- [x] **Stack-paint re-measure** (methodology: root `CLAUDE.md`'s RAM-budget-checks gotcha;
  prior measurement `M4_USER_PATCH_BANKS.md §6f`). Measured 2026-07-14 with a temporary
  UART0 probe added directly to `fw/src/main.rs` for this purpose (mbsid has no logger/UI
  wiring at all — unlike `sid_player_sw`'s `handlers::logger_init` — so this bypassed that
  and talked to `Serial0`/UART0 directly; paints the stack region with `0xAA` at boot, scans
  for the high-water mark every 64 main-loop iterations, logs new peaks at 115200 baud).
  **Result: 22736 / 25856 B peak, hit by a `Usb` card `Load→Slot`** — only **~3.1 KB (12%)
  headroom**, far tighter than the §6f estimate (+2–3 KB over M4's 4016 B baseline, i.e.
  ~6–7 KB expected) predicted. This is the M6a read-only path; **M6b's export leg (tx_data
  fill loop + FAT write-back cache) has not been measured and will add on top of this** —
  see the flagged risk in §8 and the now-higher-priority stack-paint item in §7b. The probe
  is deliberately still present in `fw/src/main.rs` (marked `TEMPORARY`, two blocks — search
  "end TEMPORARY") to re-use for that §7b measurement; remove both blocks once §7b's number
  is recorded.

**M6b — export (write).** Gateware §4b (vendored `guh` MSC engine with SCSI WRITE(10) +
bulk-OUT `DATA-TX`, `USBMSCPeripheral(with_write=True)`'s `tx_data`/`start_write` CSR pair,
sim-tested), firmware §6a/6b (FAT write-back sector cache in `fat.rs`, `usb_patch.rs`'s
`export_patch`/`encode_syx`), menu §6d (`Card::Usb`'s `Export` row). All implemented and part
of the 118/118 host test suite; full bitstream build with the write path included passes
timing (see `CLAUDE.md`'s status line). **Run on real hardware 2026-07-14 — corrupted the
test drive's GPT + FAT32 boot sector; root-caused the same day (the CSR TX-FIFO flush fired
on the strobe that started the write — payload-less WRITE(10) CBWs desyncing the drive's
bulk-only transport) and fixed in gateware (strobe-then-fill + deferred engine start),
sim-verified.** See the incident writeup + root cause at the top of this document and §8's
risk table. The export path stays hard-disabled in firmware; §7b's checklist below is the
remaining gate, and must run on disposable media.

### 7b. M6b hardware checklist (record results here once hardware is available)

Plain checklist, not something executable in this environment — no hardware is available
here. Walk this in order on a real Tiliqua r5 with a USB-C-to-A adapter and a writable
FAT32 thumb drive, after first confirming the M6a checklist (§7a) passes on the same drive:

- [ ] **Exported file mounts clean on a PC.** From the `Usb` card, `Export` the live EDIT
  buffer (or a User slot) to the drive. Unplug from Tiliqua, plug into a PC/Linux box, run
  `fsck.vfat -n <device>` — clean, no errors, no lost chains. Confirm the file appears under
  `/MBSID/<name>.SYX` (`EDIT.SYX` for the live buffer, `Pnnn.SYX` for slot `nnn`) with
  plausible size (1036 bytes — 6-byte header + cmd/type/bank/slot + 1024 nibblized data +
  checksum + `F7`).
- [ ] **Byte-identical round-trip via MIDI.** Export a patch, note its sound/register image.
  Send the exported `.syx` file back to Tiliqua over **TRS** SysEx (`amidi -s <file>` per the
  user guide's SysEx section) — the resulting engine state (audition) must be indistinguishable
  from the original, since `encode_syx`'s output is a standard MBSID v2 Bank Write dump
  addressed to User bank 1 and lands through the same `SysexCapture`/engine entry point as any
  other upload.
- [ ] **Byte-identical round-trip via re-import.** Export a patch, then use the `Usb` card's
  `File`/`Load` row to re-import the same file from the same drive. The re-imported 512-byte
  patch body must compare equal to the original (this is also asserted automatically by
  `export_patch`'s own internal readback-verify — this checklist item is the same check one
  layer up, exercising the *load* path too, not just the write+verify the firmware already
  does on every export).
- [ ] **Unplug during export leaves a mountable filesystem.** Start an `Export`, physically
  unplug the drive partway through (the screen will be frozen/unresponsive for the write's
  duration — see `CLAUDE.md`'s M6b gotcha on the missing live `BUSY` indicator; unplug at any
  point while it's unresponsive). Re-mount on a PC: `fsck.vfat` should find, at worst, a
  truncated or missing target file — never a corrupted directory structure or an
  unmountable volume.
- [ ] **50 repeated exports, then `fsck.vfat`.** Export the same (or varying) patches 50
  times in a row (overwriting or using distinct filenames). Unmount, run `fsck.vfat -n` on a
  PC — no leaked clusters, no orphaned chains, free-space accounting still correct.
- [ ] **Quirky-drive sweep.** Repeat the core export+round-trip checks (first three items
  above) on at least two different drives: a cheap/slow flash stick and a USB-SSD enclosure.
  This also covers Task 11's known, accepted risk: the vendored write path's OUT data phase
  is chunked in 64-byte packets, which is technically out-of-spec-ish for a mid-transfer
  non-max-size packet on some USB implementations — a drive that's picky about this should
  surface as a write error (visible `Export FAILED` status), not silent corruption; confirm
  which behavior actually occurs on the quirkier of the two drives.
- [ ] **Stack-paint re-measure — HIGH PRIORITY, do this before trusting M6b on hardware.**
  Same methodology as §7a's stack-paint item (now measured; see there for the probe
  technique), but exercise the deepest M6b path instead: menu navigation into `Usb`, an
  `Export` of a User slot (the write leg adds the `tx_data` fill loop + FAT write-back cache
  on top of whatever the read leg already uses). §7a's `Load→Slot` alone already measured
  **22736/25856 B (~3.1 KB headroom)** — far tighter than the pre-M6 baseline
  (`M4_USER_PATCH_BANKS.md §6f`, 4016/25824 B) or the original +2–3 KB estimate predicted.
  M6b's additional write-path stack usage could plausibly exhaust the remaining ~3.1 KB;
  do not skip this measurement or assume it's fine by extrapolation.

## 8. Risks

| Risk | Exposure | Mitigation |
|---|---|---|
| **Write path corrupts real media** — realized 2026-07-14: a real-hardware M6b `Export` attempt zeroed a test drive's GPT partition table and FAT32 boot sector (both copies). **Root-caused same day**: the CSR TX FIFO's `ResetInserter(start_write)` flushed the just-loaded payload on the strobe that started the write, so every WRITE(10) was payload-less, hanging the drive mid-command and desyncing its bulk-only transport (mostly-zero data committed at arbitrary LBAs) | **Critical — root cause fixed in sim, hardware re-validation pending** | Gateware fixed: strobe-then-fill contract with the engine start deferred until all 128 words are banked (`usb_msc_csr.py`), so a payload-less CBW is structurally impossible; regression tests in `tests/test_usb_msc_csr.py`. `PressResult::UsbExport` re-enabled for §7b validation, which must run on **disposable media only**, stopping at the first failure signal — see the incident writeup above and memory `mbsid-usb-write-test-destroyed-drive` |
| Area/Fmax: +USB engine on an 80%-full, 61.76 MHz design | High (project-gating) | Phase 0 probe; Option B fallback; last resort: `with_sysex` and MSC engine made build-time exclusive (two bitstream variants — ugly, avoid) — superseded: M6a measured 91% LUT, 66.41 MHz post-route sync Fmax PASS; M6b (read+write path both included) measured **94% LUT, 64.29 MHz post-route sync Fmax PASS** — LUT climbed as expected with the write leg, still routes with margin over the 60 MHz target |
| Stack exhaustion: M6a's `Load→Slot` measured **22736/25856 B peak on real hardware (2026-07-14), ~3.1 KB headroom** — nearly 6x the §6f estimate (+2–3 KB over M4's 4016 B) | High (untested M6b adds more on top: `tx_data` fill loop + FAT write-back cache) | Re-run the stack-paint probe (methodology in §7a's now-checked item) against M6b's `Export` path before trusting it on hardware; if headroom is gone, prime suspects are `fatfs`'s own call depth (deeper than `sid_player_sw`'s smaller mainram ever exercised) and/or the `UserPatchStore::save` + FAT path both being live on the stack at once during `Load→Slot` — profile with the same probe at finer granularity (e.g. log call-site tags, not just a periodic scan) if a fix is needed |
| FAT corruption on unplug during export | Medium | Writes only on explicit user action; flush eagerly; verify-by-readback (`export_patch` re-reads+re-parses+byte-compares before reporting success); document "don't unplug while the menu is unresponsive" — note there is no live `BUSY` row during a write (`CLAUDE.md`'s M6b gotcha: `DriveState::Busy` exists but is never constructed), the frozen screen itself is the busy signal |
| Quirky drives (slow spin-up, non-512 blocks) | Medium | `block_size()==512` guard already in driver (reject others visibly); `guh` 10 s init watchdog handles slow SSDs; test a cheap flash stick + an SSD enclosure |
| `guh` fork drift | Low | Vendor only `engines/msc.py`; `usbh/*` internals stay upstream-pinned; offer write support upstream as a PR |
| Mode-switch wedge (half-enumerated device) | Low | Mode bit resets the incoming engine; both engines already watchdog-reset on stall |
| Main-loop stall from a stalling drive | Low | Per-block spin cap already in `usb_msc.rs`; per-op block-count cap in `usb_patch.rs` |

## 9. Documentation follow-ups

- `docs/` user guide: **done.** USB Mode row, patch load walkthrough (M6a), export
  walkthrough (M6b), drive format requirements (FAT32, MBR, ≤1 partition tested), unplug
  warnings for both load and export.
- `CLAUDE.md` (this dir): **done.** USB mode mux + `0x1300` CSR + PAC-regen note (M6a); M6b
  gotcha block (vendored `guh_msc`, write CSR contract, 8.3 filenames, missing live `BUSY`
  indicator); the "no export path" framing in the no-MIDI-TX gotcha updated now that export
  exists.
- `DESIGN.md`: M6 (both halves) is in the milestone table (§7) as of M6a; not touched by this
  checkpoint — see `DESIGN.md §7`'s own M6 entry for the up-to-date phrasing if it needs a
  pass.

## 10. Reference pointers

- Read stack (proven): `../sid_player_sw/top.py:61-270` (CSR periph + wiring),
  `../sid_player_sw/fw/src/{usb_msc,fat,partition,sid_scan}.rs`.
- MSC engine, read-only upstream: `.venv/…/guh/engines/msc.py` (`SCSIBulkHost`,
  `USBMSCHost`) — read-only reference only, never edited.
- MSC engine, vendored + write-extended (M6b): `src/vendor/guh_msc/msc.py` — the file
  actually built; adds `data_dir`, `DATA-TX`, `WRITE_10`.
- Write CSR peripheral: `src/tiliqua/usb_msc_csr.py` (`USBMSCPeripheral(with_write=True)`,
  `tx_data`/`start_write` at offsets `0x20`/`0x24`); instantiated `../sid/top.py:539`.
- MIDI host being displaced/muxed: `.venv/…/guh/engines/midi.py`; instantiation
  `../sid/top.py:607-644` (incl. the VBUS gating to change).
- Patch/SysEx formats: `fw/src/sysex_capture.rs` (framing), `fw/src/patch_store.rs` (flash
  bank), `M4_USER_PATCH_BANKS.md §6b-c`.
- Export firmware: `fw/src/usb_patch.rs` (`encode_syx`, `export_patch`); `fw/src/usb_msc.rs`
  (`write_block`); `fw/src/fat.rs` (write-back sector cache).
- Area/Fmax baseline: `build/mbsid-r5/top.tim` — pre-M6 19444/24288 COMB, 61.76 MHz; M6a-only
  22127/24288 (91%), 66.41 MHz; M6a+M6b (this checkpoint) 22872/24288 (94%), 64.29 MHz, all
  PASS at the 60 MHz target.
