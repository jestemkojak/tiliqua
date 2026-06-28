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
void           mbsid_note_on (uint8_t note, uint8_t vel);
void           mbsid_note_off(uint8_t note);
void           mbsid_pitch_bend(uint16_t bend14);
void           mbsid_cc(uint8_t cc, uint8_t val);
int            mbsid_tick(uint8_t speed_factor);          /* 1 if regs changed */
const uint8_t *mbsid_regs_l(void);                        /* 32-byte image */
const uint8_t *mbsid_regs_r(void);                        /* 32-byte image, used by M2 firmware for SID_PERIPH_R */

#ifdef __cplusplus
}
#endif

#endif /* MBSID_SHIM_H */
