# SID Player SW

Software 6502 (`mos6502` crate, NMOS variant) runs the PSID tune on the VexiiRiscv;
memory is the 64 KB image in PSRAM at `0x20800000` via the RISC-V D-cache
(`fw/src/player.rs` `PsidBus`); `$D400-$D41F` writes are redirected to the
`SIDPeripheral` CSR (`(val<<5)|reg` → `transaction_data` register); `play()` is
called by polling `mcycle` against `psid::play_period_cycles`. No gateware
6502/bridge/play-timer. The bus/driver logic is host-tested
(`cargo test --target x86_64-unknown-linux-gnu --lib`).

**Limitation:** `mos6502` has no illegal/undocumented opcodes (none of the test
tunes use them); revisit if a future tune requires them.

## Architecture

- `fw/src/player.rs` — `PsidBus` (`Bus` impl), `call()` (run-until-RTS sentinel),
  `init()`, `sid_txn()`. Host-testable.
- `fw/src/main.rs` — CPU constructed over `&'static mut [u8; 0x10000]` at
  `0x20800000`; INIT called once after load; main loop polls `mcycle` and calls
  `player::call(play_addr)` at the tune's rate. File/subtune changes re-fill
  `cpu.memory.mem` and re-run INIT.

## Play rate (VBlank / CIA multispeed)
- Rate computed by `psid::play_period_cycles` from `hdr.clock()` / `hdr.is_cia(subtune)`.
  PSID `speed` (offset $12) is VBI(0) vs CIA(1) — **not** PAL/NTSC.
- CIA timer value is read from `cpu.memory.mem[0xDC04/0xDC05]` directly after INIT
  (the RISC-V is the only PSRAM master of the image, so no cache-thrash needed).

## Firmware host tests (`fw/`)
- Run: `cd fw && cargo test --target x86_64-unknown-linux-gnu --lib` (default
  target is `riscv32im`, so the host triple must be explicit).
- Host-testable modules listed in `fw/src/lib.rs` without `#[cfg(not(test))]`:
  `partition`, `psid`, `sid_scan`, `player`. `usb_msc`/`fat` are hardware-bound.
- The pac CSR asm (`pac/src/macros.rs`) is `target_arch`-gated; fix it in the
  template (`src/rs/template/pac/`), not just the generated copy.
