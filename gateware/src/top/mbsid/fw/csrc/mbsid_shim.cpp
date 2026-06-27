/* mbsid_shim.cpp — flat extern "C" wrapper around one MbSidEnvironment.
 *
 * One MbSidEnvironment lives in .bss (zero-initialised).  The ctor sets
 * updateSpeedFactorSet(2) internally; mbsid_tick()'s speed_factor arg is
 * accepted for ABI stability but is NOT wired into env.tick() — the engine
 * uses its internal factor.  If Task 2 reveals the oracle drives the factor
 * dynamically, add env.updateSpeedFactorSet(speed_factor) before env.tick().
 */

#include "mbsid_shim.h"
#include "MbSidEnvironment.h"   // pulls sid_regs_t, sid_patch_t via mios32 facade

static_assert(sizeof(sid_patch_t) == 512, "patch must be 512 bytes");

namespace {
    MbSidEnvironment env;       // .bss: engine + clock; ctor sets updateSpeedFactor=2
    sid_regs_t       regL;      // .bss, zero-init
    sid_regs_t       regR;      // .bss, zero-init
    const uint8_t    MIDI_CHN = 0;
}

extern "C" void mbsid_init(void) {
    for (int r = 0; r < SID_REGS_NUM; ++r) { regL.ALL[r] = 0; regR.ALL[r] = 0; }
    env.mbSid[0].init(/*sidNum*/0, &regL, &regR, &env.mbSidClock);
}

extern "C" int mbsid_load_patch(const uint8_t *buf512) {
    sid_patch_t p;
    for (unsigned i = 0; i < sizeof(p); ++i) ((uint8_t*)&p)[i] = buf512[i];
    return env.sysexSetPatch(/*sid*/0, &p, /*toBank*/false, 0, 0) ? 0 : -1;
}

extern "C" void mbsid_note_on (uint8_t note, uint8_t vel) { env.mbSid[0].midiReceiveNote(MIDI_CHN, note, vel); }
extern "C" void mbsid_note_off(uint8_t note)              { env.mbSid[0].midiReceiveNote(MIDI_CHN, note, 0); }
extern "C" void mbsid_pitch_bend(uint16_t bend14)         { env.mbSid[0].midiReceivePitchBend(MIDI_CHN, bend14); }
extern "C" void mbsid_cc(uint8_t cc, uint8_t val)         { env.mbSid[0].midiReceiveCC(MIDI_CHN, cc, val); }

extern "C" int  mbsid_tick(uint8_t /*speed_factor*/)      { return env.tick() ? 1 : 0; }
extern "C" const uint8_t *mbsid_regs_l(void)              { return regL.ALL; }
extern "C" const uint8_t *mbsid_regs_r(void)              { return regR.ALL; }
