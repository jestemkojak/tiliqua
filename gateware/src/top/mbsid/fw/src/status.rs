//! The menu status line: its state (text + when it was set, always updated
//! together) and the pure constructors for every message main.rs can show.
//!
//! Split out of main.rs because the `status = Some(x); status_set_at =
//! Some(now)` pair appeared at six sites and desyncing the two was a live
//! bug class (see commit 565023fe). `Status::set` writes both or neither.

use core::fmt::Write;
use heapless::String;

use crate::bank_import::ImportOutcome;
use crate::uptime;

/// How long a status message stays on screen before auto-clearing. Was
/// `STATUS_TTL_MS` in main.rs.
pub const TTL_MS: u32 = 3000;

/// The menu's transient status line. `text` and `set_at` are only ever
/// written together, which is the whole reason this type exists.
#[derive(Default)]
pub struct Status {
    text: Option<String<24>>,
    set_at: Option<u32>,
}

impl Status {
    pub fn new() -> Self {
        Self::default()
    }

    /// Show `s` starting at `now` (a `uptime::now_ms()` reading). Restarts
    /// the TTL. `now` is a parameter rather than read internally so this
    /// type stays pure and host-testable.
    pub fn set(&mut self, s: String<24>, now: u32) {
        self.text = Some(s);
        self.set_at = Some(now);
    }

    pub fn clear(&mut self) {
        self.text = None;
        self.set_at = None;
    }

    /// Auto-clear a stale message. Returns `true` exactly once per expiry —
    /// the caller uses it as a redraw-dirty flag, so repeating `true` would
    /// repaint forever.
    pub fn expire(&mut self, now: u32) -> bool {
        if uptime::ttl_expired(self.set_at, now, TTL_MS) {
            self.clear();
            true
        } else {
            false
        }
    }

    pub fn as_deref(&self) -> Option<&str> {
        self.text.as_deref()
    }
}

/// On-device save to user slot `slot`.
pub fn saved(ok: bool, slot: u8) -> String<24> {
    let mut s = String::new();
    let _ = if ok {
        write!(s, "Saved U{:03}", slot)
    } else {
        write!(s, "Save FAILED U{:03}", slot)
    };
    s
}

/// The USB patch file could not be read or parsed.
pub fn usb_load_failed() -> String<24> {
    let mut s = String::new();
    let _ = write!(s, "USB load FAILED");
    s
}

/// A USB patch file loaded into the engine. `slot` is `Some(n)` when the
/// load was also persisted to user slot `n`, in which case `saved_ok`
/// reports whether that flash write succeeded.
pub fn usb_loaded(slot: Option<u8>, saved_ok: bool) -> String<24> {
    let mut s = String::new();
    let _ = match slot {
        Some(n) if saved_ok => write!(s, "Loaded -> U{:03}", n),
        Some(n) => write!(s, "Save FAILED U{:03}", n),
        None => write!(s, "Loaded (audition)"),
    };
    s
}

/// Whole-bank import from /MBSID/BANK.SYX. `None` = the drive would not mount.
pub fn imported(outcome: Option<ImportOutcome>) -> String<24> {
    let mut s = String::new();
    let _ = match outcome {
        None => write!(s, "USB mount FAILED"),
        Some(ImportOutcome::BadFile) => write!(s, "No/bad BANK.SYX"),
        Some(ImportOutcome::Failed) => write!(s, "Import FAILED"),
        Some(ImportOutcome::Imported(n)) => write!(s, "Imported {} patches", n),
    };
    s
}

/// Patch export to the drive.
pub fn exported(ok: bool, fname: &str) -> String<24> {
    let mut s = String::new();
    let _ = if ok {
        write!(s, "Exported {}", fname)
    } else {
        write!(s, "Export FAILED")
    };
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bank_import::ImportOutcome;

    #[test]
    fn saved_strings_match_legacy_wording() {
        assert_eq!(saved(true, 7).as_str(), "Saved U007");
        assert_eq!(saved(false, 7).as_str(), "Save FAILED U007");
        assert_eq!(saved(true, 123).as_str(), "Saved U123");
    }

    #[test]
    fn usb_load_strings_match_legacy_wording() {
        assert_eq!(usb_load_failed().as_str(), "USB load FAILED");
        assert_eq!(usb_loaded(None, true).as_str(), "Loaded (audition)");
        assert_eq!(usb_loaded(Some(42), true).as_str(), "Loaded -> U042");
        assert_eq!(usb_loaded(Some(42), false).as_str(), "Save FAILED U042");
    }

    #[test]
    fn usb_loaded_ignores_saved_ok_when_auditioning() {
        // No slot => nothing was saved, so saved_ok is irrelevant.
        assert_eq!(usb_loaded(None, false).as_str(), "Loaded (audition)");
    }

    #[test]
    fn import_strings_match_legacy_wording() {
        assert_eq!(imported(None).as_str(), "USB mount FAILED");
        assert_eq!(imported(Some(ImportOutcome::BadFile)).as_str(), "No/bad BANK.SYX");
        assert_eq!(imported(Some(ImportOutcome::Failed)).as_str(), "Import FAILED");
        assert_eq!(
            imported(Some(ImportOutcome::Imported(12))).as_str(),
            "Imported 12 patches"
        );
    }

    #[test]
    fn export_strings_match_legacy_wording() {
        assert_eq!(exported(true, "EDIT.SYX").as_str(), "Exported EDIT.SYX");
        assert_eq!(exported(false, "EDIT.SYX").as_str(), "Export FAILED");
    }

    #[test]
    fn set_writes_both_fields_and_clear_clears_both() {
        let mut s = Status::new();
        assert_eq!(s.as_deref(), None);
        s.set(saved(true, 1), 1000);
        assert_eq!(s.as_deref(), Some("Saved U001"));
        // Not yet expired one tick before the TTL.
        assert!(!s.expire(1000 + TTL_MS - 1));
        assert_eq!(s.as_deref(), Some("Saved U001"));
        s.clear();
        assert_eq!(s.as_deref(), None);
        // A cleared Status never expires again (no set_at).
        assert!(!s.expire(9_999_999));
    }

    #[test]
    fn expire_clears_once_and_reports_dirty_once() {
        let mut s = Status::new();
        s.set(saved(true, 1), 1000);
        assert!(s.expire(1000 + TTL_MS));
        assert_eq!(s.as_deref(), None);
        // Second call must not report dirty again — that would repaint forever.
        assert!(!s.expire(1000 + TTL_MS));
    }

    #[test]
    fn expire_is_wraparound_safe() {
        let mut s = Status::new();
        s.set(saved(true, 1), u32::MAX - 10);
        // now wraps past zero; elapsed = 10 + 5 = 15 ms, well under the TTL.
        assert!(!s.expire(4));
        assert_eq!(s.as_deref(), Some("Saved U001"));
        assert!(s.expire(TTL_MS));
    }

    #[test]
    fn set_replaces_text_and_restarts_the_ttl() {
        let mut s = Status::new();
        s.set(saved(true, 1), 1000);
        s.set(saved(false, 2), 1000 + TTL_MS - 1);
        assert_eq!(s.as_deref(), Some("Save FAILED U002"));
        // The TTL restarted at the second set, so the first deadline passing
        // must not clear it.
        assert!(!s.expire(1000 + TTL_MS));
    }
}
