/* init_array.x — collect C++ static constructors and expose their bounds.
 *
 * riscv-rt's link.x defines NO `.init_array` output section and provides no
 * `__init_array_start`/`__init_array_end` symbols, and its reset path never
 * calls `__libc_init_array`. So the MBSID engine's global constructors (the one
 * `.init_array` entry for the shim's `MbSidEnvironment env` — which recursively
 * sets the speed factor, seeds the RNG, sets BPM=120/clock mode, etc.) would
 * NEVER run on the target. We collect `.init_array` into a known section and
 * expose its bounds; `mbsid_run_static_ctors()` (csrc/mbsid_shim.cpp) walks them
 * at firmware startup. KEEP guards the entries against --gc-sections.
 *
 * Injected (riscv only) via build.rs: `cargo:rustc-link-arg=-Tinit_array.x`.
 */
SECTIONS {
  .init_array : ALIGN(4)
  {
    __init_array_start = .;
    KEEP(*(SORT(.init_array.*)));
    KEEP(*(.init_array));
    __init_array_end = .;
  } > REGION_RODATA
} INSERT AFTER .rodata;
