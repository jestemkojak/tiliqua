# MBSID-on-Tiliqua — M2 Design Spec (Stereo / dual-SID, full 6-oscillator fidelity)

**Date:** 2026-06-27
**Status:** Design draft (`mbsid-port`). M1 (Lead, mono, MIDI-played) is complete and
confirmed on hardware; this is the follow-up.
**Scope:** restore the full MBSID stereo voice architecture — **two SIDs, 6
oscillators (3 L + 3 R), two filters** — by un-discarding the engine's R register image and
driving a second gateware reSID with it. Design only: interfaces + acceptance tests so the
implementation is mechanical.

---

## 1. Goal & why

**M1 deliberately collapsed to mono** (`DESIGN.md §2`): one reSID fed the engine's **L**
register image (oscillators 1–3 + Filter 0); the **R** image (oscillators 4–6 + Filter 1) was
*computed every tick but discarded*. That is a fidelity reduction, not a simplification of the
engine — the MBSID Lead engine **always** produces both 32-byte register images.

MBSID v2 is a **dual-SID stereo** design: oscillators split 3 Left + 3 Right with two
independent filters (`external feasibility notes`). Many Lead patches use all 6 oscillators
and/or both filters (stereo detune/spread, L/R-panned layers, cross-filtered voices). Running
them on one SID silently drops half the voice architecture. **M2 makes playback true to the
patch design** by giving the R image its own SID.

**The engine work is already done.** Because the shim already exposes both images
(`mbsid_regs_l()` / `mbsid_regs_r()`, `DESIGN.md §4`), M2 is **pure integration**: a second
gateware SID + a second register-diff loop in firmware. No engine reconfiguration, no
re-vendoring, no change to oscillator count in the C++ — we stop throwing the R image away.

**Non-goals (still deferred):** Bassline/Drum/Multi engines, ASID, patch-bank storage,
wave-sequencer UI. (`DESIGN.md §10`.)

---

## 2. The 30 MHz SID clock domain — already in place (inherited)

The user-requested "move the SID to a 30 MHz domain" is **already done in shared code** and
mbsid already runs SID #0 there. Confirmed in the current tree:

- `src/tiliqua/pll.py` defines `m.domains.sid = ClockDomain()` at **30 MHz** in all three PLL
  variants (`TiliquaDomainGeneratorPLLExternal` ~L339/411 uses CLKOS2 = VCO 600/20;
  `TiliquaDomainGenerator2PLLs` ~L508/566 uses **CLKOS3** because CLKOS2 is taken by `audio`;
  `TiliquaDomainGenerator4PLLs` ~L656 uses CLKOS2). Reset tied to `locked60`.
- `top/sid` `SID.elaborate` clocks the reSID core from `ClockSignal("sid")`
  (`top/sid/top.py:167`); the `>20×phi2` requirement is met (30 cycles/phi2).
- `SIDPeripheral` defaults `sid_hz=30_000_000`, crosses the write path with an **AsyncFIFO**
  (`w_domain="sync", r_domain="sid"`, `top.py:294`), runs the phi2 divider under
  `DomainRenamer("sid")` (`top.py:356`), and pulse-synchronizes the audio strobe sid→sync
  (`top.py:412`). Voice taps are captured into `sync` only on the synced strobe.
- `sid_player_sw` already ships this (`sid_hz=30_000_000`, `top/sid_player_sw/top.py:206`).

**Consequence for M2:** SID #1 goes in the **same existing `sid` domain** — no new PLL output,
no new CDC machinery to design. The reSID filter muladd (the path that failed `sync` at
60 MHz, see `docs/sid_player_sw_perf_review.md §1` and `[[sid-player-sw-timing-critical-path]]`)
is already off the `sync` critical path for both SIDs. The remaining M2 hardware risk is
**LUT area / routing congestion**, not `sync` timing (see §6).

---

## 3. Architecture & data flow (M2)

```
MIDI in (CSR FIFO) ─► mbsid_note_on/off / pitch_bend / cc      (engine state; unchanged)

1 kHz Timer0 ISR ─► mbsid_tick(speed_factor)
   ├─ mbsid_regs_l() ─► diff vs shadow_L ─► changed (data<<5)|addr ─► SIDPeripheral_L ─► reSID0 ─► codec LEFT
   └─ mbsid_regs_r() ─► diff vs shadow_R ─► changed (data<<5)|addr ─► SIDPeripheral_R ─► reSID1 ─► codec RIGHT
        (both reSIDs in the 30 MHz `sid` domain, φ2 = 1 MHz each)
```

Everything from each FIFO onward (φ2 divider, reSID, audio capture) is the existing,
validated `top/sid` path — instantiated twice.

---

## 4. Gateware changes

All in `top/sid` + the mbsid top. The base `top/sid` is shared with `sid`/`sid_player_sw`, so
the second SID is added **opt-in, defaulting off**, keeping those tops bit-identical.

### 4.1 Parametrize `SIDSoc` for a second SID (recommended)

Add an `n_sids=1` (or `stereo=False`) constructor kwarg to `SIDSoc` (`top/sid/top.py:469`).
When `>1`:

- **Second `SIDPeripheral`** at a non-overlapping CSR window. SIDPeripheral spans 64 B
  (`addr_width=6`); base SID is `0x1000`, scope `0x1100` — put the R peripheral at **`0x1200`**:
  ```python
  self.sid_periph_r = SIDPeripheral()
  self.csr_decoder.add(self.sid_periph_r.bus, addr=0x1200, name="sid_periph_r")
  ```
  Reusing `SIDPeripheral` verbatim is intentional: it brings its own AsyncFIFO + phi2 divider
  in the `sid` domain. Its MIDI-in/USB CSRs go unused for the R instance (harmless; only the L
  peripheral sources MIDI).
- **Second `SID` core** in `elaborate`, wired to `sid_periph_r`:
  ```python
  m.submodules.sid_r = sid_r = SID()
  self.sid_periph_r.sid = sid_r
  m.d.comb += [self.sid_periph_r.ext_w_en.eq(0), self.sid_periph_r.ext_w_data.eq(0)]
  ```
- Both phi2 dividers run in `sid`, free-run from the same `locked60`-derived reset → they start
  and stay phase-locked (identical divide). No drift; `DUAL_SID_PLAN.md`'s phase caveat does
  not apply here.

`n_sids` defaults to 1 → `sid`/`sid_player`/`sid_player_sw` synthesise unchanged. Add the
`with_scope=True` flag (§6) in the same edit — mbsid uses `n_sids=2, with_scope=False`.

### 4.2 Audio routing → stereo line out

Replace the four mono jack taps (`top/sid/top.py:524-530`) for the stereo case:

| Codec channel | M2 (stereo) |
|---|---|
| `payload[0]` | **SID0 mix L** = `sid_periph_l.last_audio_left >> 8` → **LEFT out** |
| `payload[1]` | **SID1 mix R** = `sid_periph_r.last_audio_left >> 8` → **RIGHT out** |
| `payload[2]` | SID0 mix (dup) or voice tap — optional |
| `payload[3]` | SID1 mix (dup) or voice tap — optional |

(Each SID is a mono synth with a stereo *filter* output; use `.last_audio_left` per SID per
`DUAL_SID_PLAN.md`. `.last_audio_right` is equally valid if per-SID filter panning is wanted.)

**mbsid has no scope/display** — the firmware drives none (`fw/src/main.rs:13`: "NO
display/scope"). But mbsid currently *inherits* `SIDSoc`'s `ScopePeripheral` + `FramebufferPlotter`
gateware unchanged (it only overrides `__init__`), so that logic synthesizes as **dead LUTs +
a dead PSRAM master**. See §6 — removing it is the primary capacity lever for the second SID,
and is correct cleanup regardless. With the scope gone, the scope-feed wiring
(`top/sid/top.py:535-539`) goes away too; only the two SID-mix codec channels remain.

### 4.3 mbsid top

`MBSIDSoc` (`top/mbsid/top.py:28`) sets `n_sids=2`:
```python
kwargs.setdefault("mainram_size", 0x8000)
kwargs.setdefault("n_sids", 2)
```

### 4.4 PAC regen (required — this is a CSR change)

M1 needed no PAC regen; **M2 adds a CSR peripheral**, so the firmware won't see SID #1 until
the PAC is regenerated. Build with `pdm mbsid build` (full) or `pdm mbsid build --pac-only`
after the Amaranth change, then reference the regenerated `sid_periph_r` base in firmware.
(The PAC is gitignored/regenerated each build — root `CLAUDE.md`.)

---

## 5. Firmware changes (`top/mbsid/fw/src/main.rs`)

Minimal and symmetric with the existing L path:

1. **Second shadow + diff.** Add a second 32-byte shadow `shadow_r` alongside `shadow_l`.
   After `mbsid_tick`, run the *same* diff/enqueue routine on `mbsid_regs_r()` → write changed
   `(data<<5)|addr` words to `SID_PERIPH_R`'s `transaction_data`, with the same `TxnStatus`
   backpressure check as the L path. Factor the existing L diff into a helper taking
   `(regs, &mut shadow, periph)` and call it twice.
2. **Reset both SIDs** wherever the L SID is reset/initialised (mirror any `$D400-$D418` clear).
3. **No engine changes.** `mbsid_note_on/off`, `pitch_bend`, `cc`, `mbsid_tick` are all
   unchanged — they already drive all 6 oscillators internally.
4. **Cost.** +32 B `.bss` (second shadow) and up to ~25 extra FIFO writes/ms. Well within
   budget; **no `mainram_size` bump beyond M1's `0x8000`** (engine `.bss` is unchanged).

---

## 6. Capacity & timing (the real M2 risk)

The reSID filter is already at 30 MHz, so **`sync` Fmax is not the concern** — a second reSID
adds ~no `sync`-domain logic. The concern is **LUT area + `sid`-domain routing congestion**:

- `DUAL_SID_PLAN.md` estimates **~7k LUTs for 2× SID**. mbsid is leaner than `sid_player_sw`
  (no 6502 interpreter, no PSID/replay machinery), and once the dead scope gateware is stripped
  (below) it carries only the framebuffer + UI draw path.
- Single-SID mbsid currently passes at **`sync` 67.25 MHz** (`CLAUDE.md`). Adding SID #1
  mainly pressures `TRELLIS_COMB %` and routing; `sid`-domain timing has ~47% slack at 30 MHz
  (`perf_review §1`), so the new core has headroom there.

**Primary capacity lever — strip mbsid's inherited dead scope gateware.** mbsid inherits
`SIDSoc`'s `ScopePeripheral` + `FramebufferPlotter` (a PSRAM master) but its firmware drives
neither (`fw/src/main.rs:13`). Override `elaborate`/parametrize `SIDSoc` so the **scope +
plotter** are omitted when unused — this frees LUTs *and* a PSRAM arbiter port for the second
SID, and is correct cleanup independent of M2.

**Keep the framebuffer / video output AND the base draw path.** mbsid needs video (a simple
patch-browsing UI is planned post-M2). There are **two** `FramebufferPlotter`s — don't confuse
them:
- `TiliquaSoc.framebuffer_plotter` (`tiliqua_soc.py:241`) — the *base* plotter, wired to
  `pixel_plot`/`blit`/`line` (`tiliqua_soc.py:362-364`). This is the **HW text/line draw path**
  the future UI uses (firmware `draw::draw_options` → `blit`/`line` → this plotter →
  framebuffer). **KEEP.**
- `SIDSoc.plotter` (`top/sid/top.py:482`) — a *second*, scope-dedicated 4-port plotter, plus
  `ScopePeripheral` (CSR `0x1100`). **This is the dead weight. REMOVE.**

Removing the scope plotter does not orphan `fb.fbp`: that geometry/Properties bus fans out to
the base plotter + `persist_periph` too (`tiliqua_soc.py:375-376`), which stay.

**How to remove (upstream change, backward-compatible).** `SIDSoc` adds the scope
unconditionally (`__init__` `top/sid/top.py:481-489`; `elaborate` `:501-513`, `:535-539`). Add a
**`with_scope=True`** kwarg to `SIDSoc` and guard those `add()`s / submodules / connects /
pmod-scope-feed behind it. Default `True` keeps `sid` and `sid_player_sw` **bit-identical**;
mbsid passes `with_scope=False`. This rides along with the `n_sids` parametrization (§4.1) in
the same `__init__`/`elaborate` region — one coherent edit. (Alternative: override
`SIDSoc.elaborate` wholesale in `MBSIDSoc` — avoids the upstream edit but duplicates ~70 lines
of SID/pmod/MIDI wiring and drifts from `top/sid`; not recommended.)

**Plan of attack:**
1. Record single-SID mbsid post-PnR utilisation as baseline (TRELLIS_COMB, Fmax both domains).
2. Strip the dead scope/plotter (above); re-measure — establishes the real headroom.
3. Build dual-SID on the default **25F** (r5). Check post-route: `sid` Fmax ≥ 30 MHz with
   margin, `sync` Fmax ≥ 60 MHz, TRELLIS_COMB < ~90%.
4. If it still won't fit: target **LFE5U-45F** (SoldierCrab R2, ~45k LUTs — "comfortable" per
   `DUAL_SID_PLAN.md`).

Read post-route Fmax as the **second** `Max frequency for clock` occurrence per clock in
`build/mbsid-r5/top.tim` (root `CLAUDE.md`).

---

## 7. Oracle & validation — extend to the R stream

M1's oracle diffs only the **L** register stream (`DESIGN.md §6`, `host_oracle/run_oracle.sh`).
M2 must validate the **R** path before hardware:

- Extend the host harness to also dump and diff the **R** register stream (`mbsid_regs_r()` vs
  the JUCE port's second `sid_regs_t`) — byte-identical across the same ≥3 Lead patches × 2
  sequences. Result target: **both L and R 6/6 byte-identical**.
- Re-run `host_oracle/run_oracle.sh` after any shim/firmware change (it's the keystone).
- This is what actually proves "true to the design": a patch that uses oscillators 4–6 now has
  its R stream verified against the reference engine, not silently dropped.

---

## 8. Milestones (inside M2)

1. **Oracle R-stream.** Extend the harness; L **and** R bit-exact on ≥3 Lead patches. (PC only.)
2. **Gateware bring-up.** `n_sids=2` builds; PAC regen exposes `sid_periph_r`; at least one
   register write reaches reSID1 (scope-verified tone on the RIGHT channel).
3. **Stereo play.** A 6-oscillator / dual-filter Lead patch plays with audible stereo content
   (L≠R) on hardware; informal A/B against the emulator confirms both halves are present.
4. **Capacity sign-off.** Post-route utilisation + both-domain Fmax pass on the chosen device
   (25F if it fits, else 45F).

---

## 9. Risks / open items

- **25F LUT/congestion** (§6) — primary risk. Mitigations designed: strip the inherited dead
  scope gateware (the main lever); fall back to 45F. Overlaps the unresolved `sid_player_sw`
  congestion work (`[[sid-player-sw-timing-critical-path]]`).
- **Two phi2 dividers vs one.** Spec uses two (one per `SIDPeripheral`, both in `sid`,
  phase-locked from common reset). If LUTs are tight, a single shared divider feeding both SIDs
  is a ~50-LUT saving but requires refactoring `SIDPeripheral` to accept an external phi2 —
  deferred unless needed.
- **Codec L/R mapping.** Confirm `pmod0.i_cal.payload[0]`/`[1]` map to the intended physical
  L/R output jacks during bring-up (milestone 2 scope check).
- **GPL** unchanged from M1 (engine linked into firmware → GPL on distribution).

---

## 10. Reference pointers

- 30 MHz `sid` domain: `src/tiliqua/pll.py` (all three `TiliquaDomainGenerator*`);
  `top/sid/top.py` (`SID`, `SIDPeripheral` AsyncFIFO + phi2 `DomainRenamer("sid")`).
- Single→dual wiring baseline: `top/sid/top.py` `SIDSoc` (`__init__`/`elaborate`);
  `docs/DUAL_SID_PLAN.md` (stale re: firmware model & numbers — re-validate, don't trust).
- Both register images: shim `mbsid_regs_l()`/`mbsid_regs_r()` (`DESIGN.md §4`).
- Timing/congestion context: `docs/sid_player_sw_perf_review.md §1`.
- M1 spec & roadmap: `DESIGN.md` (§2 mono-collapse decision, §10 roadmap).
