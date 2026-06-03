# Tiliqua

## Build & test (pdm project lives in `gateware/`, not repo root)
- `cd gateware && pdm <target> build` — build a bitstream (targets in `gateware/pyproject.toml [tool.pdm.scripts]`, e.g. `sid_player`, `polysyn`, `bootloader`)
- `pdm test` — run all tests (`pytest -n auto tests/`); or `pdm run pytest tests/test_x.py -v`
- A failed build surfaces only as `CalledProcessError: 'build_top.sh' returned non-zero`; the real cause (e.g. yosys `found logic loop` / abc9 `no_loops`) is in stdout — grep for it.
- Generated synthesis artifacts (`cpu.v`, `top.ys`, `top.rpt`, `top.il`) land in `gateware/build/<target>-r5/`.

## Gotchas
- arlet 6502 (`cpu.v`): `RDY` combinationally selects `DIMUX` (`DIMUX = ~RDY ? DIHOLD : DI`) which feeds `AB`. A peripheral driving `cpu_RDY` combinationally from `cpu_AB` forms an unsynthesizable comb loop — drive `RDY` from registered FSM state instead.
