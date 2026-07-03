/* mbsid_shim.h — extern "C" ABI for the MBSIDv3 engine shim
 *
 * All types are <stdint.h> only — no mios32 types cross this boundary.
 */

#ifndef MBSID_SHIM_H
#define MBSID_SHIM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

void           mbsid_init(void);
int            mbsid_load_patch(const uint8_t *buf512);  /* 0 = ok */
void           mbsid_program_change(uint8_t patch);     /* load factory bank slot (patch & 0x7F) via bankLoad */
uint8_t        mbsid_bank_count(void);                   /* number of valid banks (>=1) */
int            mbsid_bank_load(uint8_t bank, uint8_t patch);            /* 0 = ok */
void           mbsid_bank_patch_name_get(uint8_t bank, uint8_t patch, char *buf17); /* 16 chars + NUL */
int            mbsid_bank_patch_info(uint8_t bank, uint8_t patch,
                                     uint8_t *engine_out, uint8_t *vflags_out); /* 0 = ok, -1 = bad index */
void           mbsid_note_on (uint8_t chn, uint8_t note, uint8_t vel);
void           mbsid_note_off(uint8_t chn, uint8_t note);
void           mbsid_pitch_bend(uint8_t chn, uint16_t bend14);
void           mbsid_cc(uint8_t chn, uint8_t cc, uint8_t val);
void           mbsid_aftertouch(uint8_t chn, uint8_t val);  /* channel aftertouch */
int            mbsid_tick(uint8_t speed_factor);          /* 1 if regs changed */
const uint8_t *mbsid_regs_l(void);                        /* 32-byte image */
const uint8_t *mbsid_regs_r(void);                        /* 32-byte image, used by M2 firmware for SID_PERIPH_R */

/* M4: on-device save — copy the live patch out (raw sid_patch_t bytes). */
void mbsid_current_patch_raw(uint8_t *buf512);
/* M4: SysEx receive — feed one raw stream byte to the engine's parsers
 * (MbSidSysEx + MbSidAsid). Returns nonzero if consumed as SysEx. */
int  mbsid_sysex_byte(uint8_t b);
/* M4: abort a half-received SysEx message after an RX gap (upstream
 * MIDI-timeout hook; MbSidSysEx aborts only on port match, which DEFAULT is). */
void mbsid_sysex_timeout(void);

/* M5: modulation + on-device patch editing (M5_MENU_CARDS_CV_MOD.md §4). */
void    mbsid_knob_set(uint8_t knob, uint8_t value);    /* engine knob 0..7 (K1..K5,V,P,A) */
void    mbsid_par_set(uint8_t par, uint16_t value16);   /* parSet common block, sidlr=3, 16-bit scaled */
int     mbsid_sysex_param(uint16_t addr, uint8_t data); /* patch body write + live update; 1 = ok */
uint8_t mbsid_patch_byte(uint16_t addr);                /* raw patch body read (display) */
uint8_t mbsid_current_engine(void);                     /* patch body[0x10]: 0=Lead,1=Bass,2=Drum,3=Multi */

#ifdef __cplusplus
}
#endif

#endif /* MBSID_SHIM_H */
