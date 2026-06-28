/* oracle.cpp — the REFERENCE side of the Task-2 equivalence check.
 *
 * Instantiates the MBSID Lead engine EXACTLY as the JUCE PluginProcessor.cpp
 * does (mbSidEnvironment.mbSid[0].init(sid, &sidRegs[L], &sidRegs[R], &clock)),
 * drives the sequence through env.mbSid[0].midiReceive*, runs one env.tick()
 * per ms-step, and emits the changed L- and R-register traces (the PluginProcessor
 * diff loop, both sides).  Uses the vendored engine source directly — NO shim.
 */
#include <mios32.h>
#include "MbSidEnvironment.h"
#include "seq.h"

// In-tree Lead preset bank: static const u8 sid_bank_preset_0[128][512].
#include "sid_bank_preset_a.inc"

namespace {
    const u8 MIDI_CHN = 0;
}

struct OracleBackend {
    MbSidEnvironment env;
    sid_regs_t sidRegs[2];   // [0]=L, [1]=R  (mirrors PluginProcessor.cpp)

    void init() {
        for (int reg = 0; reg < SID_REGS_NUM; ++reg) {
            sidRegs[0].ALL[reg] = 0;
            sidRegs[1].ALL[reg] = 0;
        }
        u8 sid = 0;
        env.mbSid[0].init(sid, &sidRegs[0], &sidRegs[1], &env.mbSidClock);
    }
    int load_patch(int row) {
        sid_patch_t p;
        memcpy(&p, sid_bank_preset_0[row], sizeof(p));
        return env.sysexSetPatch(/*sid*/0, &p, /*toBank*/false, 0, 0) ? 0 : -1;
    }
    void note_on (int note, int vel) { env.mbSid[0].midiReceiveNote(MIDI_CHN, (u8)note, (u8)vel); }
    void note_off(int note)          { env.mbSid[0].midiReceiveNote(MIDI_CHN, (u8)note, 0); }
    void cc(int num, int val)        { env.mbSid[0].midiReceiveCC(MIDI_CHN, (u8)num, (u8)val); }
    void bend(int val14)             { env.mbSid[0].midiReceivePitchBend(MIDI_CHN, (u16)val14); }
    int  tick()                      { return env.tick() ? 1 : 0; }
    const uint8_t *regs()            { return sidRegs[0].ALL; }
    const uint8_t *regs_r()          { return sidRegs[1].ALL; }
};

int main(int argc, char **argv) {
    if (argc < 2) { fprintf(stderr, "usage: %s <sequence.txt>\n", argv[0]); return 2; }
    OracleBackend be;
    run_sequence(argv[1], be, stdout);
    return 0;
}
