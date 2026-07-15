---
title: "USB storage: drive not re-detected after hotplug — needs bitstream restart"
status: needs-triage
created: 2026-07-15
---

## Description

Reported during M6b round-five hardware testing (2026-07-15): after unplugging
a USB drive in Storage mode, plugging one back in is not detected — the menu
stays at "No drive" until the bitstream is restarted. User: "something broke
recently."

Supporting evidence already in the round-five UART logs (8GB MBR stick, after
the failed export's watchdog reset):

```
usb: conn=0 rdy=0 bs=512 spd=3 t=45475ms   <- device gone (spd=3 = no device)
usb: conn=1 rdy=0 bs=512 spd=0 t=45638ms   <- re-enumerated at HS, but never READY
usb: conn=0 rdy=0 bs=512 spd=3 t=55635ms   <- 10 s later: watchdog reset again
```

So re-enumeration *starts* (conn=1) but the MSC init (TEST UNIT READY / READ
CAPACITY) apparently never completes -> ready never asserts -> 10 s watchdog
loop. This may share a root cause with the round-five write failures (a drive
left mid-BOT-command by a failed write may STALL/NAK the init commands; the
engine has no BOT Reset Recovery — no Bulk-Only Mass Storage Reset class
request). Worth re-testing AFTER the clear-halt recovery round lands, on a
clean boot with no failed write beforehand, to separate "hotplug broke in
general" from "hotplug broke after a wedged write."

Things that changed recently in this area (bisect candidates):
- vendored SIE swap (`SCSIBulkHost.__init__` replaces `enumerator.sie`) — NYET fix
- CBW NAK retry + CSW/DATA-RX Default arms in the engine
- firmware idle keepalive (LBA 0 read every 2 s while ready, commit 7f31ac6)
- REQUEST SENSE auto-issue after failed writes

## Acceptance criteria

- [ ] Determine whether hotplug re-detection fails on a clean boot (no failed
      write first) or only after a wedged/failed write
- [ ] Root-cause: enumerator, MSC init FSM (init_retry exhaustion?), or
      firmware state machine
- [ ] Hotplug (unplug -> replug, and cold plug after boot) re-detects within
      ~10 s without bitstream restart
- [ ] Consider implementing BOT Reset Recovery (class request 0xFF + clear
      both halts) as the standard "drive is confused" hammer
