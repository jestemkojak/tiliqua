/* sweep_driver.cpp — no-crash sweep over ALL 128 factory patches via the shim.
 *
 * Loads each sid_bank_preset_0[row] through mbsid_program_change (the bankLoad
 * path), plays a short note, and ticks the engine. Asserts only that the engine
 * RUNS to completion for every patch — including the 9 non-Lead patches, which
 * dispatch into the linked Bassline/Drum/Multi SEs (verified present in the ELF,
 * 24-26 symbols each). A segfault fails the process exit code; a hang is caught
 * by the `timeout` wrapper in run_oracle.sh. Proves "non-Lead patches don't
 * freeze the SoC" entirely on PC.
 */
#include <cstdint>
#include <cstdio>
#include "mbsid_shim.h"

int main() {
    mbsid_init();
    for (int row = 0; row < 128; ++row) {
        mbsid_program_change((uint8_t)row);
        mbsid_note_on(60, 100);
        for (int t = 0; t < 16; ++t) mbsid_tick(2);
        mbsid_note_off(60);
        for (int t = 0; t < 4; ++t)  mbsid_tick(2);
    }
    printf("SWEEP OK: 128 patches\n");
    return 0;
}
