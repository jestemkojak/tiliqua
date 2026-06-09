# sid_player_sw dropped notes — investigation, root cause, and the RISC-V (VexiiRiscv) rebuild setup

Status: 2026-06-08. Root cause found and verified. Fix (bigger CPU caches) in
progress; netlist-regen toolchain via podman is derisked and documented below.

---

## 1. What we're trying to fix

`sid_player_sw` (the software-6502 PSID tune player) **drops notes** on real
tunes (e.g. Commando) and on synthetic "burst" stress tunes. Tempo is correct
(no drift) — individual note-ons just go silent.

## 2. What we ruled out (with hardware measurement, not guesses)

The earlier theory (memory `sid-player-sw-fifo-overflow-dropped-notes`) blamed the
gateware SID **transaction-FIFO overflowing**. A backpressure fix was built and
flashed — **it didn't help**. We then instrumented and measured on hardware:

| Hypothesis | Test | Result | Verdict |
|---|---|---|---|
| FIFO overflow drops writes | Added `TxnStatus.level`/`writable` CSR; firmware logs peak FIFO occupancy over UART, playing `burst160` (160 writes/frame, 10× FIFO depth) | **`max_level=0/16, sat=0`** the whole time | **Ruled out.** FIFO never fills; emulator is *slower* than the 1 MHz drain. Backpressure & "throttle to 1 MHz" are both no-ops. |
| Skipped PLAY frames / stuck emulator | Timed each PLAY frame vs the play period in `play_tick` (Timer0 `counter()` before/after); counted overruns and `call()` max_steps overruns | `overrun=0 stuck=0`, frame uses ~60% of budget | **Ruled out.** Every write reaches the SID, on time. |
| Wrong SID model (filter) | Title line vs tune metadata; voice-scope check | Build 6581 = tune 6581; **note is gone from the voice scope too** (pre-filter) | **Ruled out** (model matches); and the *envelope never opened* — not a filter/audio-path issue. |

## 3. Root cause (verified)

Only one variable was left: the **sub-frame timing of SID writes**. We measured
the emulator's actual speed by reading `cpu.cycles` (the `mos6502` crate tracks
cycles) per PLAY frame against wall-clock (Timer0):

```
PLAY max_dur=716537 cpu_cyc=1248 eff=10% period=1197018 overrun=0 stuck=0
```

- A PLAY frame is ~**1248 6502-cycles** (a real C64 emits these writes in a tight
  ~1.25 ms burst), but emulating them takes ~**12 ms** wall-clock → **`eff ≈ 10%`,
  i.e. the software 6502 runs ~10× slower than a real 1 MHz 6502.**
- The reSID core is clocked at real 1 MHz continuously. So between two writes that
  are 8 cycles apart on hardware, reSID advances ~80 cycles. Every inter-write gap
  is stretched ~10×, which **breaks the envelope-trigger timing** SID note-ons rely
  on (ADSR delay bug / hard-restart) → the note never attacks → silent **and flat
  on the voice scope**. Matches every symptom.

### Why the emulator is 10× too slow (the real bottleneck)

The 64 KB 6502 image lives in PSRAM (`0x20800000`). The CPU is VexiiRiscv variant
`tiliqua_rv32im` with **tiny direct-mapped L1 caches**:

```
--lsu-l1-sets=8  --lsu-l1-ways=1   → D-cache ≈ 512 B  (64 B line)
--fetch-l1-sets=8 --fetch-l1-ways=1 → I-cache ≈ 512 B
```

The emulator thrashes **both**: data side (`self.mem[a]` scattered across zero-page,
stack, player code, music tables ≫ 512 B) and instruction side (mos6502's huge
opcode-`match` ≫ 512 B). So ~every access misses → full HyperRAM latency → ~574
sync cycles per 6502 cycle. (The CLAUDE.md "via the D-cache" is true but
misleading: the cache exists, it's just far too small.)

## 4. The fix we chose: bigger CPU L1 caches

Make emulation fast enough (ideally `eff > 100%`) so we can pace it to exactly
1 MHz → correct sub-frame write timing, **zero latency**, and it also cures the
separate heavy/high-rate-tune choppiness (memory `sid-player-6502-bottleneck`).

**Verified constraints / risks (checked before committing):**
- Sync clock **already fails timing**: `$glbnet$clk` = **49.28 MHz vs 60 MHz**.
  The L1 D-cache is on the sync-domain LSU critical path; a bigger cache may push
  Fmax lower. (top.tim, 2nd "Max frequency" line.)
- **LUTs 87% full** (TRELLIS_COMB 21189/24288). BRAM OK (DP16KD 25/56 = 44%).
- First try: **2 KB each (16 sets × 2 ways)** — matches the known-fitting
  `rv32imafc` variant. Measure `eff%` + Fmax + fit, then iterate.

**Fallbacks if the bitstream won't close timing / fit:**
- Firmware hot-RAM cache: back the 6502's hot pages in fast 16 KB blockram
  (BSS/stack already live there); no gateware/timing risk, partial speedup.
- Decouple delivery from emulation: capture writes stamped with `cpu.cycles`,
  replay at correct inter-write spacing (firmware replay, or a gateware
  timestamped-write FIFO). Fixes timing despite the slowness; adds ~1 frame latency.

### Diagnostic instrumentation currently in `fw/src/main.rs`

Temporary, remove once fixed: `PLAY_MAX_DUR / PLAY_CPU_CYC / PLAY_OVERRUN /
PLAY_CALL_BAD` statics + a throttled UART line in the UI loop; `sid_write`
backpressure was removed (it was a no-op). The gateware `TxnStatus` CSR can stay
(cheap) or be reverted.

---

## 5. Rebuilding the VexiiRiscv RISC-V netlist via podman (THE setup guide)

Changing CPU cache flags changes the netlist hash → VexiiRiscv must be
**regenerated from SpinalHDL** (`sbt`). `sbt` is not installed on the host (and
host Java 25 is too new for the pinned sbt 1.10.0 / Scala 2.12.19). Use the
bundled toolchain container. **This is fully working; below is exactly how.**

### Image
```
podman pull docker.io/leviathanch/vexiiriscv     # ~26 GB, has sbt+JDK21+verilator
```

### Three Fedora/podman gotchas (all solved)
1. **The image's login shell hangs.** `bash -lc` stalls forever sourcing profile.
   → Use a **non-login** shell: `--entrypoint /bin/bash ... -c '...'`.
2. **Fedora SELinux blocks the bind-mount** (Permission denied on `/work` files).
   → Add **`--security-opt label=disable`** (doesn't relabel host files). (`:z` also works but mutates host labels.)
3. **The image ignores `-w /work`** (sbt ran in `/tmp/hsperfdata_root` → wrong
   project → `ClassNotFoundException: vexiiriscv.Generate`).
   → **`cd /work`** explicitly inside the shell command.

Rootless podman maps container-root → host user, so generated files are owned by
your user and readable by the build — no `chown` needed.

### One-shot manual generation (sanity check)
```bash
cd gateware/deps/VexiiRiscv
podman run --rm -v "$PWD":/work --security-opt label=disable --network=host \
  --entrypoint /bin/bash leviathanch/vexiiriscv \
  -c 'cd /work && sbt "Test/runMain vexiiriscv.Generate"'
# -> writes deps/VexiiRiscv/VexiiRiscv.v   (~52 s after first-run dep download)
```
`--network=host` is needed so sbt can download Scala/ivy deps on first run.

### Integrated regen (so `pdm <target> build` works transparently)
The Python build (`src/vendor/vexiiriscv/vexiiriscv.py`) shells out to bare `sbt`
with `cwd=deps/VexiiRiscv` and the exact args (cache flags + `--region ...` +
`--reset-vector ...`), then copies `deps/VexiiRiscv/VexiiRiscv.v` to
`src/vendor/vexiiriscv/verilog/VexiiRiscv_<md5>.v` (hash of git-sha + args).

Provide an **`sbt` shim** on PATH that forwards into the container
(`~/.cache/vexii-shim/sbt`, persistent dep caches so repeat runs are fast):
```bash
#!/bin/bash
exec podman run --rm \
  -v /home/pawel/code/tiliqua/gateware/deps/VexiiRiscv:/work \
  -v /home/pawel/.cache/vexii-sbt/ivy2:/root/.ivy2 \
  -v /home/pawel/.cache/vexii-sbt/sbt:/root/.sbt \
  -v /home/pawel/.cache/vexii-sbt/coursier:/root/.cache/coursier \
  --security-opt label=disable --network=host \
  --entrypoint /bin/bash leviathanch/vexiiriscv \
  -c 'cd /work && exec sbt "$@"' _ "$@"
```
Then any build that needs a new netlist just works:
```bash
PATH=/home/pawel/.cache/vexii-shim:$PATH  pdm sid_player_sw build [--pac-only]
```
If a netlist for the requested (variant+regions) hash already exists in
`verilog/`, no regen happens. To force regen of an existing config, move that
`VexiiRiscv_<hash>.v` aside first.

### To change the cache size (the actual fix)
Edit `gateware/src/vendor/vexiiriscv/vexiiriscv.py`, `CPU_VARIANTS["tiliqua_rv32im"]`:
```python
'--lsu-l1-sets=16', '--lsu-l1-ways=2',     # D-cache 512 B -> 2 KB
'--fetch-l1-sets=16','--fetch-l1-ways=2',  # I-cache 512 B -> 2 KB
```
Then `PATH=…/vexii-shim:$PATH pdm sid_player_sw build` (regen + full bitstream).
Read back the new sync Fmax (2nd "Max frequency" line in
`build/sid-player-sw-r5/top.tim`), LUT fit, and re-measure `eff%` on hardware.

### 2a integration test result (2026-06-08) — PASSED, with a caveat
Moved the checked-in `VexiiRiscv_ae96…v` aside and ran
`PATH=…/vexii-shim:$PATH pdm sid_player_sw build --pac-only`. The build detected
the missing netlist, drove `sbt` through the shim/container, and reproduced the
hash-named netlist. **Integration works.**

⚠️ **Caveat:** the regenerated RTL is **not byte-identical** to the checked-in
one (696946 vs 697039 B, different md5) despite identical args/hash. Cause is
SpinalHDL-"dev" version drift / non-determinism (`git head : ???`), not config.
Implication: any netlist we generate here (including the bigger-cache one) is
functionally the same generator+args but **not the validated bitstream RTL**, so
it must be validated by building + testing on hardware. The blessed original was
restored after the test (kept the unvalidated copy at `/tmp/ae96_regenerated.v`).

### Verifying success
- Build closes (or how far off) at 60 MHz sync, fits the ECP5-25 LUTs.
- UART `eff%` rises well above 10% (goal: >100% so we can then pace to 1 MHz).
- Commando's dropped notes return; voice-scope traces show the envelopes.

---

## 6. CHECKPOINT — 2026-06-08 end of day (resume here)

### 2 KB-cache build result (measured on hardware)
| metric | 512 B (orig) | 2 KB (16 sets × 2 ways) |
|---|---|---|
| `eff%` (emulation speed vs 1 MHz 6502) | ~10% | **~17–19%** |
| `max_dur` (sync cycles/frame) | ~716k | **~377–391k** (≈ halved) |
| sync Fmax (post-route) | 49.28 MHz | **57.06 MHz** (improved!) |
| LUT (TRELLIS_COMB) | 87% | 84% |
| BRAM (DP16KD) | 44% | 66% |
| overrun / stuck | 0 / 0 | 0 / 0 |

**So 4× cache → ~2× speed.** Diminishing returns + BRAM wall (66% already) mean
**cache alone cannot reach the `eff>100%`** needed for cycle-accurate pacing
(would need ~16 KB caches ≫ 56 BRAM blocks). The 2 KB cache is still a keeper
(2× faster, better Fmax, headroom for heavy tunes), but it does NOT by itself fix
dropped notes — at eff=19% writes are still ~5× time-stretched.

### ⚠️ 6581 vs 8580 confound
The 2 KB build above was the **default 8580** model. **Commando is a 6581 tune** —
rebuild with `--sid-model 6581` before judging its audio. Some of the "problems in
other parts" heard on the 8580 build may be filter-model mismatch, not timing.
(`eff%` itself is model-independent, so the speed numbers stand.)

## 7. UPDATE — 2026-06-09: 4 KB build measured; cache wall is Fmax, not BRAM

Committed the 2 KB variant (`tiliqua_rv32im_bigcache`, commit `1f8f4a1`), then
built+flashed a **4 KB** experiment (32 sets × 2 ways, `--sid-model 6581`).

### Full cache-scaling table (all measured on hardware)
| cache | `eff%` | `max_dur` (sync cyc) | sync Fmax (post-route) | BRAM (DP16KD) |
|---|---|---|---|---|
| 512 B (orig)        | 10%      | 716k | 49.3 MHz       | 44% (25/56) |
| 2 KB (16 sets×2)    | 19%      | 377k | **57.1 MHz**   | 66% |
| **4 KB (32 sets×2)**| **32–33%** | **227k** | **48.2 MHz (FAIL)** | **60% (34/56)** |

### Two checkpoint assumptions OVERTURNED
1. **Scaling is NOT "diminishing returns": ~1.7× speed per cache doubling**
   (10→19→32%). Extrapolated: 8 KB≈54%, 16 KB≈90%, ~32 KB would cross eff>100%.
2. **BRAM is NOT the wall.** Verified in the netlist: each cache data RAM is one
   `*_mem_symbol*` of depth = sets×line/4 bytes; at 4 KB that's 512×8 = 4 Kbit,
   which still fits a *single* 18 Kbit DP16KD — same block count as 2 KB. Caches
   up to 16 KB (16 Kbit < 18 Kbit) cost no extra BRAM blocks. The earlier
   "≫56 BRAM blocks" reasoning was wrong (BRAM even went 66%→60%).

### The actual wall: sync Fmax
4 KB drops sync to **48.2 MHz**, a hard FAIL vs the mandatory 60 MHz (USB +
PSRAM). It emits UART but is overclocked ~25% past close → not reliable. Bigger
caches (needed for eff>100%) make Fmax strictly *worse*. **We hit the timing
wall well before the speed wall** — so cache alone cannot be the fix, and the
2 KB build (57 MHz, best Fmax) is the base to keep. The 4 KB experiment is being
reverted.

### Netlist cache-size verification (how to re-check any build)
The bitstream's actual cache is provable from the netlist `top.ys` read:
```
grep "read_verilog  VexiiRiscv_" build/sid-player-sw-r5/top.ys   # -> hash used
grep "LsuL1Plugin_logic_ways_0_mem \["  verilog/VexiiRiscv_<hash>.v # tag depth = #sets
grep "LsuL1Plugin_logic_banks_0_mem_symbol0 \[" verilog/VexiiRiscv_<hash>.v # data depth
```
4 KB (`71ac3b8…`): tag `[0:31]` (32 sets), data `[0:511]`; 2 KB (`bb993242…`):
tag `[0:15]` (16 sets), data `[0:255]`. ways count = `grep -c ..._ways_[0-9]+_mem`.

### DECISION (settled 2026-06-09): capture + replay
Cache is confirmed insufficient (Fmax wall). Implement **capture+replay** next;
it fixes timing at ANY eff so the 2 KB cache's 19% is plenty. Keep 2 KB, drop 4 KB.

### (original options, for reference)
How to actually eliminate the dropped notes, given cache can't reach 100%:
1. **Capture + replay (recommended, firmware-only):** during the ~6 ms emulation,
   buffer each SID write stamped with `cpu.cycles`; replay into the SID at correct
   *relative* 1 MHz spacing. Fixes timing at ANY eff; fits the frame (6 ms emulate
   + ~1.25 ms replay ≪ 20 ms). Cost: ~one-frame latency, minor jitter (pin replay
   to a fixed offset to remove it). `mos6502` exposes `cpu.cycles` for the stamps.
2. **4 KB cache first:** one more build for headroom/scaling data; won't reach 100%,
   BRAM → ~88%. Then still need replay.
3. **Firmware hot-RAM split:** zero-page/stack (+ emulator hot code) into fast
   blockram for more raw speed; uncertain, more invasive than replay.

### Working-tree state (UNCOMMITTED — all intentional, nothing committed yet)
- `gateware/src/vendor/vexiiriscv/vexiiriscv.py` — added `tiliqua_rv32im_bigcache`
  variant (2 KB I+D).
- `gateware/src/top/sid_player_sw/top.py` — `kwargs.setdefault("cpu_variant",
  "tiliqua_rv32im_bigcache")`.
- `gateware/src/top/sid_player_sw/fw/src/main.rs` — diagnostic instrumentation
  (`PLAY_MAX_DUR/CPU_CYC/OVERRUN/CALL_BAD` + throttled UART `eff%` line);
  backpressure spin REMOVED from `sid_write` (it was a proven no-op). The gateware
  `TxnStatus` CSR (in `top/sid/top.py`) is still present (harmless) — revert later.
- untracked: this doc; `verilog/VexiiRiscv_bb993242….v` (the generated 2 KB netlist
  — keep, it's what the bigcache build uses).

### To resume / rebuild (remember the shim + 6581!)
```bash
cd gateware
PATH=/home/pawel/.cache/vexii-shim:$PATH  pdm sid_player_sw build --sid-model 6581
pdm run flash archive build/sid-player-sw-r5/sid-player-sw-<HEAD>--r5.tar.gz
```
Play Commando, read the UART `PLAY … eff=…%` line. (sbt shim + deps cache already
set up; see §5. podman works.)
