/* shim_driver.cpp — the SHIM side of the Task-2 equivalence check.
 *
 * Drives the flat extern "C" mbsid_* ABI (fw/csrc/mbsid_shim.{h,cpp}) over the
 * SAME sequence and emits the SAME L- and R-register traces.  If this diverges
 * from oracle.cpp the shim mis-wraps the engine — fix the shim, never the trace.
 *
 * mios32.h is included ONLY to obtain `u8` for the preset-bank .inc; the driver
 * itself touches no mios32 types — everything crosses via <stdint.h>.
 */
#include <mios32.h>          // for u8 (preset bank below)
#include "mbsid_shim.h"
#include "seq.h"

#include "sid_bank_preset_a.inc"   // static const u8 sid_bank_preset_0[128][512]

struct ShimBackend {
    void init()                      { mbsid_init(); }
    int  load_patch(int row)         { return mbsid_load_patch(sid_bank_preset_0[row]); }
    void note_on (int note, int vel) { mbsid_note_on((uint8_t)note, (uint8_t)vel); }
    void note_off(int note)          { mbsid_note_off((uint8_t)note); }
    void cc(int num, int val)        { mbsid_cc((uint8_t)num, (uint8_t)val); }
    void bend(int val14)             { mbsid_pitch_bend((uint16_t)val14); }
    int  tick()                      { return mbsid_tick(2); }
    const uint8_t *regs()            { return mbsid_regs_l(); }
    const uint8_t *regs_r()          { return mbsid_regs_r(); }
};

int main(int argc, char **argv) {
    if (argc < 2) { fprintf(stderr, "usage: %s <sequence.txt>\n", argv[0]); return 2; }
    ShimBackend be;
    run_sequence(argv[1], be, stdout);
    return 0;
}
