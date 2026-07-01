/* mbsid_multi_wt.h — Multi-engine wavetable -> parameter modulation.
 *
 * Upstream mios32 MbSidSeMulti never finished wiring the WT to parameters:
 *   (1) sysexSetParameter case 0x2b drops the assign-LR bits (data >> 6) that
 *       Lead extracts, so MbSidWt::wtAssignLeftRight stays 0; and
 *   (2) MbSidSeMulti does not override MbSidSe::parSetWT (an empty virtual {}),
 *       so the per-step parSetWT() call in MbSidSeMulti::tick() does nothing.
 * Result: Multi patches that gate a filter from the WT (e.g. A107 "Poly
 * Trancegate") are silent.
 *
 * We do NOT patch the GPL engine. Instead this helper replicates the WT step
 * MbSidSeMulti::tick() (lines 178-188) performs, using MbSidSeLead::parSetWT's
 * relative/absolute math (lines 1067-1092) and the engine's OWN public
 * parSet/parGet. Call it ONCE immediately after env.tick(); it is a no-op unless
 * the active engine is Multi. wtOut is written exactly once per env.tick(), so a
 * post-tick read is lossless; the filterCutoff write lands in next tick's
 * register image (~1 ms lag, inaudible, identical on shim & oracle).
 *
 * Pinned to mios32 @ 44d8e6af. If the pin moves, re-verify parSetWT's formula
 * and the body.M.voice[i][0x2b] WT-speed byte layout (sid_se_wt_speed_par_t:
 * CLKDIV[5:0], bit6=CHANNEL_TARGET_SIDL, bit7=CHANNEL_TARGET_SIDR).
 */
#ifndef MBSID_MULTI_WT_H
#define MBSID_MULTI_WT_H

#include "MbSidEnvironment.h"   // pulls MbSid / MbSidSeMulti / MbSidWt / types

static inline void mbsid_multi_wt_fixup(MbSid &sid)
{
    if (sid.currentMbSidSePtr != &sid.mbSidSeMulti)
        return;

    MbSidSeMulti &m = sid.mbSidSeMulti;
    for (int wt = 0; wt < 6; ++wt) {
        MbSidWt &w = m.mbSidWt[wt];
        if (w.wtOut < 0 || !w.wtAssign)
            continue;

        // Recover the SID L/R target bits upstream's case 0x2b dropped, from the
        // authoritative patch byte (WT-speed byte of Multi voice `wt`).
        u8 sidlr   = m.mbSidPatchPtr->body.M.voice[wt][0x2b] >> 6;
        u8 wtValue = m.mbSidPatchPtr->body.M.wt_memory[w.wtOut & 0x7f];

        // MbSidSeLead::parSetWT math, delegating to Multi's own parSet/parGet.
        int parValue;
        if ((wtValue & (1 << 7)) == 0) {          // relative
            int diff = ((int)wtValue << 9) - 0x8000;
            if (diff == 0)
                continue;
            parValue = (int)m.parGet(w.wtAssign, sidlr, wt, /*scaleTo16bit*/true) + diff;
            if (parValue < 0) parValue = 0; else if (parValue > 0xffff) parValue = 0xffff;
        } else {                                   // absolute
            parValue = (wtValue & 0x7f) << 9;
        }
        m.parSet(w.wtAssign, (u16)parValue, sidlr, wt, /*scaleFrom16bit*/true);
    }
}

#endif /* MBSID_MULTI_WT_H */
