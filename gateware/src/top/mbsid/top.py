# Copyright (c) 2026 Tiliqua contributors
#
# SPDX-License-Identifier: CERN-OHL-S-2.0
"""
MBSID-on-Tiliqua (M2): reuses top/sid's SoC verbatim (VexiiRiscv @ 60 MHz,
SIDPeripheral, Phi2Divider, MIDI-in CSR FIFO, Timer0). The only delta is the
firmware path — `path=this_path` points the firmware build at `mbsid/fw`, whose
build.rs cross-compiles the MBSID v3 C++ engine and links it into the ELF.
"""

import os
import sys

# This file is named top.py and is run as a script, so its own directory lands on
# sys.path[0] and shadows the `top` package (making `from top.sid...` re-import this
# file). Replace that entry with the `src` root so `top.sid.top` resolves correctly.
_this_dir = os.path.dirname(os.path.realpath(__file__))
_src_root = os.path.dirname(os.path.dirname(_this_dir))  # .../src
if sys.path and os.path.realpath(sys.path[0]) == _this_dir:
    sys.path[0] = _src_root
elif _src_root not in sys.path:
    sys.path.insert(0, _src_root)

from top.sid.top import SIDSoc
from tiliqua.build.cli import top_level_cli
from tiliqua.build.types import BitstreamHelp


class MBSIDSoc(SIDSoc):
    """top/sid's SoC, but with a larger BRAM (mainram) window.

    The MBSID v3 engine is aggregated BY VALUE inside one static MbSidEnvironment,
    so the whole engine tree's state lands in .bss (~16 KB) — it nearly fills sid's
    default 0x4000 BRAM, leaving no room for the stack. Bump to 0x8000 (32 KB):
    ~16 KB .bss + ~16 KB stack. (M1 SoC-RAM risk, DESIGN.md §8.)
    """

    bitstream_help = BitstreamHelp(
        brief="MBSID Lead: dual SID stereo synth, MIDI in.",
        io_left=['CV1 mod', 'CV2 mod', 'CV3 mod', 'CV4 mod', 'L out', 'R out', 'L+R mix', 'L+R mix'],
        io_right=['navigate menu', 'MIDI host', 'video out', '', '', 'TRS MIDI in']
    )

    def __init__(self, **kwargs):
        kwargs.setdefault("mainram_size", 0x8000)
        kwargs.setdefault("with_scope", False)
        # Freeze framebuffer rows < 320 from persist phosphor decay. mbsid has
        # no scope, so decay's only effect was slowly fading the menu text
        # (drawn on input only; menu box spans y=62..306 at MENU_X/Y in fw).
        # Same mechanism as sid_player_sw's header freeze; build-time param,
        # no CSR/PAC change. Firmware relies on this: menu redraw is a blit
        # diff with no background fill (fw/src/menu.rs Painter).
        kwargs.setdefault("persist_freeze_rows", 320)
        kwargs.setdefault("n_sids", 2)
        kwargs.setdefault("with_sysex", True)
        # M6: USB mass-storage patch load/export (M6_USB_STORAGE.md). Adds the
        # USBMSCHost + UTMI mux + usb_msc CSR block at 0x1300 (PAC regen!).
        kwargs.setdefault("with_usb_msc", True)
        # The 2026-07-15 chunk-size diagnostic sweep (64/32/31 bytes, all
        # rej=4/2/0) is settled: the "silent" drive was answering NYET (HS
        # bulk-OUT flow control) and the stock guh SIE didn't decode it —
        # fixed in the vendored SIE (src/vendor/guh_msc/sie.py, NYET=7) and
        # engine (msc.py treats NYET as ACK). Back at the default 64-byte
        # chunks (the SIE TX FIFO depth; M6_USB_STORAGE.md round four).
        # Round eight (M6_USB_STORAGE.md): force the MSC engine to Full
        # Speed. At FS the 64-byte TX chunks equal wMaxPacketSize (legal
        # packets) and the PING protocol doesn't exist — this removes both
        # critical HS violations behind the write-path drive wedges. FS
        # ~1 MB/s is ample for 512 B patch files. USB-MIDI keeps its own
        # engine/speed; only storage mode is affected.
        kwargs.setdefault("usb_msc_fullspeed_only", True)
        super().__init__(**kwargs)


if __name__ == "__main__":
    this_path = os.path.dirname(os.path.realpath(__file__))
    top_level_cli(MBSIDSoc, path=this_path,
                  archiver_callback=lambda archiver: archiver.with_option_storage())
