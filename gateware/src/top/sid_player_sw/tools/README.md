# sid_player_sw tools

## `gen_stress_sid.py` — PSID stress-test tune generator

Generates synthetic PSID tunes that hammer the SID + software 6502 every PLAY
call, to benchmark for timing issues in `sid_player_sw` (6502 throughput,
SID-write rate, and scope/PSRAM contention vs playback — audio must always win).

The 6502 is hand-assembled by a tiny two-pass assembler in the script, then run
through a minimal in-script 6502 simulator (INIT + 64 PLAY frames) that asserts
each routine **terminates** (returns via RTS), makes SID writes, only touches
`$D400–$D418`, and actually advances its sequence — so a bad opcode/branch can't
ship a tune that spins the real player. A file is only written if it validates.

No dependencies; pure Python 3.

### Usage

```bash
python3 gen_stress_sid.py [out_dir] [options]    # default out_dir = this dir
```

| Option | Meaning |
|---|---|
| `--rates R…` | PLAY rates in Hz. `0` = 50 Hz VBlank, `>0` = CIA multispeed. One `.sid` per rate. Default `0 200`. |
| `--style ensemble\|unison` | `ensemble` (default): 3 independent voices. `unison`: 3 voices on one transposed arpeggio. |
| `--arp-div N` | *(unison)* advance the arpeggio every N PLAY calls — keeps high rates musical. Default 1. |
| `--pwm` | *(unison)* also sweep each voice's pulse width (+6 SID writes/frame → ~17 total). |
| `--prefix NAME` | output filename prefix (default `stress`). |

Output names: rate `0` → `<prefix>_vblank.sid`, rate `N` → `<prefix>_<N>hz.sid`.
Each run prints bytes, code size, SID writes/frame, rate, and style.

### Styles

- **ensemble** (default, ~9 writes/frame): a pulse **bass** (slow, advances every
  8 calls), a **lead** melody (advances every 2 calls, waveform rotates
  saw→pulse→tri every 64 calls), and a **noise percussion** (gate retrigger every
  4 calls), plus a shared filter-cutoff sweep. Distinct notes / rates / waveforms
  per voice — representative of a real multi-part tune.
- **unison** (~11 writes/frame, ~17 with `--pwm`): all three voices play one
  C-minor pentatonic arpeggio transposed a third apart (a moving chord), with a
  filter sweep and a global waveform rotation. Simpler; good for isolating raw
  call-rate effects.
- **runaway** (easter egg): a single relentless sawtooth running through a 16-step
  line over a heavy resonant filter sweep, with noise hats — in the spirit of
  early-70s analog-sequencer intros. An original pattern (not a transcription of
  any recording). Try `--style runaway --rates 0 200`.

CIA multispeed tunes set PSID `speed` bit 0 and program CIA Timer A in INIT;
`play_period_cycles` then derives the rate (`timer = round(phi2_PAL/rate) - 1`,
phi2_PAL = 985248). All tunes are flagged PAL + SID model "both" (no model-
mismatch warning on either the 6581 or 8580 firmware build).

### Generating the standard set

```bash
G=gen_stress_sid.py; OUT=../../../../../docs   # adjust to taste
python3 $G $OUT --rates 0 100 200 300 400 --prefix stress              # ensemble rate sweep
python3 $G $OUT --style unison --rates 0 200 400 --prefix uni          # unison
python3 $G $OUT --style unison --pwm --rates 200 400 --prefix unipwm   # +PWM (heavier writes)
python3 $G $OUT --style unison --pwm --arp-div 8 --rates 400 --prefix uniarp
```

### Reading the benchmark

1. Copy the `.sid` files to the USB stick, flash the build, and select each.
2. **Ensemble rate sweep** (`stress_vblank → stress_400hz`): the rate at which it
   starts to glitch/drag is the 6502-throughput ceiling for this build.
3. **`unipwm_*` vs `uni_*`** at the same rate isolates SID-write throughput
   (17 vs 11 writes/frame) from raw call-rate.
4. Toggle the scope (busy timebase / high intensity vs idle) to see the
   scope/PSRAM contribution; if a rate glitches only with a busy scope, it's
   contention, not the 6502 alone.

Generated `.sid` files are build artifacts (kept out of git, like the other test
tunes); regenerate them from this script.
