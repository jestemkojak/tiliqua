/* mbsid_shim.cpp — flat extern "C" wrapper around one MbSidEnvironment.
 *
 * `env` is a C++ global whose constructor (MbSidEnvironment::MbSidEnvironment,
 * and recursively every member ctor) establishes essential state: speed factor
 * = 2, RNG seed 0xcafebabe, clock BPM=120 / mode AUTO, per-voice/LFO/arp init,
 * etc.  On the bare-metal riscv target NOTHING runs that ctor (riscv-rt has no
 * __libc_init_array), so mbsid_init() runs all static ctors itself first — see
 * mbsid_run_static_ctors() below.  On the host oracle, libc already ran the
 * ctors before main(), so the runner is a no-op there (kept width/behaviour
 * identical between host and target).
 *
 * mbsid_tick()'s speed_factor arg is accepted for ABI stability but is NOT wired
 * into env.tick() — the engine uses its internal factor.
 */

#include "mbsid_shim.h"
#include "MbSidEnvironment.h"   // pulls sid_regs_t, sid_patch_t via mios32 facade
#include "mbsid_multi_wt.h"     // Multi WT->parameter modulation (upstream stubbed)

static_assert(sizeof(sid_patch_t) == 512, "patch must be 512 bytes");

namespace {
    MbSidEnvironment env;       // .bss until its ctor runs (see mbsid_run_static_ctors)
    sid_regs_t       regL;      // .bss, zero-init
    sid_regs_t       regR;      // .bss, zero-init
    struct { uint8_t bank, patch, engine, vflags; bool valid; } g_patch_cache = {};
}

/* Run every C++ static constructor (.init_array) exactly as a hosted libc would.
 * On the target the bounds come from fw/init_array.x; on the host libc already
 * ran them, so this is compiled out (avoids an undefined-symbol link error and
 * a redundant re-run). */
#if defined(__riscv)
extern "C" {
    extern void (*__init_array_start[])(void);
    extern void (*__init_array_end[])(void);
}
extern "C" void mbsid_run_static_ctors(void) {
    for (void (**p)(void) = __init_array_start; p != __init_array_end; ++p)
        (*p)();
}
#else
extern "C" void mbsid_run_static_ctors(void) { /* host libc ran ctors before main() */ }
#endif

extern "C" void mbsid_init(void) {
    mbsid_run_static_ctors();   // construct `env` (and all members) before first use
    for (int r = 0; r < SID_REGS_NUM; ++r) { regL.ALL[r] = 0; regR.ALL[r] = 0; }
    env.mbSid[0].init(/*sidNum*/0, &regL, &regR, &env.mbSidClock);
}

extern "C" int mbsid_load_patch(const uint8_t *buf512) {
    sid_patch_t p;
    for (unsigned i = 0; i < sizeof(p); ++i) ((uint8_t*)&p)[i] = buf512[i];
    return env.sysexSetPatch(/*sid*/0, &p, /*toBank*/false, 0, 0) ? 0 : -1;
}

extern "C" void mbsid_program_change(uint8_t patch) {
    // Route through mbsid_bank_load so the patch cache is updated.
    mbsid_bank_load(/*bank*/0, patch & 0x7F);
}

// Number of valid banks. SID_BANK_NUM is private to MbSidEnvironment.cpp, so
// probe via the side-effect-free bankPatchNameGet (returns <0 on invalid bank).
extern "C" uint8_t mbsid_bank_count(void) {
    char tmp[20];
    uint8_t n = 0;
    while (n < 255 && env.bankPatchNameGet(n, /*patch*/0, tmp) >= 0)
        ++n;
    return n;
}

// Bank-aware load (generalizes the bank-0-only mbsid_program_change). Returns
// the engine bankLoad status (0 = ok). Mutates engine state (updatePatch): the
// Rust caller MUST guard this with a critical section vs the 1 kHz tick ISR.
// Also caches engine type and voice-flags from the loaded patch so that
// mbsid_bank_patch_info() can answer without touching sid_bank_preset_0
// (which has internal linkage in MbSidEnvironment.cpp).
extern "C" int mbsid_bank_load(uint8_t bank, uint8_t patch) {
    int r = env.bankLoad(/*sid*/0, bank, patch);
    if (r == 0) {
        const uint8_t *raw = (const uint8_t *)&env.mbSid[0].mbSidPatch.body;
        g_patch_cache = { bank, patch, raw[0x10], raw[0x50], true };
    }
    return r;
}

// Read engine type (byte 0x10) and voice-flags (byte 0x50) from the last
// successfully loaded patch. Returns 0 on success, -1 if the requested
// bank/patch was not the last loaded patch (or no patch has been loaded).
// Safe to call read-only at any time (does not mutate engine state).
extern "C" int mbsid_bank_patch_info(uint8_t bank, uint8_t patch,
                                      uint8_t *engine_out, uint8_t *vflags_out) {
    // Cache-match is the sole authority: g_patch_cache is populated only by a
    // successful env.bankLoad(), so a match implies a valid (bank, patch) for
    // however many banks the engine supports. No separate bounds guard needed.
    if (!g_patch_cache.valid ||
        g_patch_cache.bank != bank || g_patch_cache.patch != patch) return -1;
    *engine_out = g_patch_cache.engine;
    *vflags_out = g_patch_cache.vflags;
    return 0;
}

// Fill buf17 (>=17 bytes) with the 16-char patch name + NUL. Read-only.
extern "C" void mbsid_bank_patch_name_get(uint8_t bank, uint8_t patch, char *buf17) {
    env.bankPatchNameGet(bank, patch, buf17);
}

extern "C" void mbsid_note_on (uint8_t chn, uint8_t note, uint8_t vel) { env.mbSid[0].midiReceiveNote(chn, note, vel); }
extern "C" void mbsid_note_off(uint8_t chn, uint8_t note)             { env.mbSid[0].midiReceiveNote(chn, note, 0); }
extern "C" void mbsid_pitch_bend(uint8_t chn, uint16_t bend14)        { env.mbSid[0].midiReceivePitchBend(chn, bend14); }
extern "C" void mbsid_cc(uint8_t chn, uint8_t cc, uint8_t val)        { env.mbSid[0].midiReceiveCC(chn, cc, val); }
extern "C" void mbsid_aftertouch(uint8_t chn, uint8_t val)            { env.mbSid[0].midiReceiveAftertouch(chn, val); }

extern "C" int  mbsid_tick(uint8_t /*speed_factor*/) {
    int changed = env.tick() ? 1 : 0;
    mbsid_multi_wt_fixup(env.mbSid[0]);
    return changed;
}
extern "C" const uint8_t *mbsid_regs_l(void)              { return regL.ALL; }
extern "C" const uint8_t *mbsid_regs_r(void)              { return regR.ALL; }

// M4: copy the live patch out. Mirror of the raw read mbsid_bank_load already
// does (env.mbSid[0].mbSidPatch.body); the save direction of the same bytes.
extern "C" void mbsid_current_patch_raw(uint8_t *buf512) {
    const uint8_t *raw = (const uint8_t *)&env.mbSid[0].mbSidPatch.body;
    for (unsigned i = 0; i < 512; ++i) buf512[i] = raw[i];
}

// M4: SysEx byte in. RAM Writes apply live inside the engine (checksum and
// all); Bank Writes no-op upstream (bankSave stub returns -2, ignored by
// sysexSetPatch) and are persisted by the firmware-side SysexCapture instead.
extern "C" int mbsid_sysex_byte(uint8_t b) {
    return (int)env.midiReceiveSysEx(DEFAULT, b);
}

extern "C" void mbsid_sysex_timeout(void) {
    env.midiTimeOut(DEFAULT);
}
