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
#include "mbsid_multi_wt.h"     // mirror the firmware shim's Multi WT fixup

// In-tree Lead preset bank: static const u8 sid_bank_preset_0[128][512].
#include "sid_bank_preset_a.inc"

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
    int program_change(int patch) { return env.bankLoad(/*sid*/0, /*bank*/0, (u8)patch); }
    void note_on (int chn, int note, int vel) { env.mbSid[0].midiReceiveNote((u8)chn, (u8)note, (u8)vel); }
    void note_off(int chn, int note)          { env.mbSid[0].midiReceiveNote((u8)chn, (u8)note, 0); }
    void cc(int chn, int num, int val)        { env.mbSid[0].midiReceiveCC((u8)chn, (u8)num, (u8)val); }
    void bend(int chn, int val14)             { env.mbSid[0].midiReceivePitchBend((u8)chn, (u16)val14); }
    void aftertouch(int chn, int val)         { env.mbSid[0].midiReceiveAftertouch((u8)chn, (u8)val); }
    void sysex_byte(uint8_t b) { env.midiReceiveSysEx(DEFAULT, b); }
    void sysex_patch_dump(int row) {
        unsigned char msg[1036];
        // type 0x08 = RAM Write sid 0 (applies live, like load_patch).
        seq_encode_patch_dump(sid_bank_preset_0[row], 0x08, 0x00, 0x00, msg);
        for (int i = 0; i < 1036; ++i) sysex_byte(msg[i]);
    }
    void knob(int k, int v)   { env.mbSid[0].currentMbSidSePtr->knobSet((u8)k, (u8)v); }
    void par(int p, int v16)  { env.mbSid[0].currentMbSidSePtr->parSet((u8)p, (u16)v16, 3, 0, true); }
    int  tick() {
        int changed = env.tick() ? 1 : 0;
        mbsid_multi_wt_fixup(env.mbSid[0]);
        return changed;
    }
    const uint8_t *regs()            { return sidRegs[0].ALL; }
    const uint8_t *regs_r()          { return sidRegs[1].ALL; }
};

int main(int argc, char **argv) {
    if (argc < 2) { fprintf(stderr, "usage: %s <sequence.txt>\n", argv[0]); return 2; }
    OracleBackend be;
    run_sequence(argv[1], be, stdout);
    return 0;
}
