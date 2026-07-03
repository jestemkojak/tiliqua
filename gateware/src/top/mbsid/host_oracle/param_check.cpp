/* param_check.cpp — M5: assert mbsid_sysex_param edits land in the patch body
 * that mbsid_current_patch_raw captures (the on-device-save invariant), and
 * that out-of-range addresses are rejected. Shim-side only, no reference. */
#include <mios32.h>
#include "mbsid_shim.h"
#include <cstdio>
#include <cstring>
#include "sid_bank_preset_a.inc"

int main() {
    mbsid_init();
    if (mbsid_load_patch(sid_bank_preset_0[0]) != 0) { puts("FAIL: load"); return 1; }
    // Edit: volume (0x52), filter cutoff_l L (0x55), OSC1 waveform (0x61).
    struct { uint16_t addr; uint8_t val; } edits[] =
        {{0x52, 0x0A}, {0x55, 0x33}, {0x61, 0x04}};
    for (auto &e : edits)
        if (!mbsid_sysex_param(e.addr, e.val)) { printf("FAIL: write %03x\n", e.addr); return 1; }
    for (int i = 0; i < 5; ++i) mbsid_tick(2); // engine must survive live update
    uint8_t buf[512];
    mbsid_current_patch_raw(buf);
    for (auto &e : edits) {
        if (buf[e.addr] != e.val) { printf("FAIL: body[%03x]=%02x want %02x\n", e.addr, buf[e.addr], e.val); return 1; }
        if (mbsid_patch_byte(e.addr) != e.val) { printf("FAIL: patch_byte %03x\n", e.addr); return 1; }
    }
    if (mbsid_current_engine() != 0) { puts("FAIL: engine byte"); return 1; }
    if (mbsid_sysex_param(512, 0)) { puts("FAIL: addr 512 accepted"); return 1; }
    puts("OK: sysex_param edits captured by current_patch_raw");
    return 0;
}
