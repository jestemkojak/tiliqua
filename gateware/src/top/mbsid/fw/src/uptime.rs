//! 1 ms wall-clock uptime, ticked by the Timer0 ISR (main.rs) and readable
//! from anywhere in the main loop. riscv32im has no atomic ops at all (repo
//! CLAUDE.md), so the counter lives behind a critical_section Mutex — the
//! same ISR/main-loop sharing pattern main.rs uses for App. Host tests link
//! the critical-section `std` impl via the dev-dependency in Cargo.toml.
//! Wraps every ~49.7 days; all consumers must use deadline_expired (
//! wrapping subtraction), never a plain `>=` on now_ms values.
use core::cell::Cell;
use critical_section::Mutex;

static UPTIME_MS: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));

/// Called once per 1 ms Timer0 ISR tick. Nested critical sections are fine
/// (the ISR body already runs inside critical_section::with).
pub fn tick_1ms() {
    critical_section::with(|cs| {
        let c = UPTIME_MS.borrow(cs);
        c.set(c.get().wrapping_add(1));
    });
}

pub fn now_ms() -> u32 {
    critical_section::with(|cs| UPTIME_MS.borrow(cs).get())
}

/// Wraparound-safe deadline check.
pub fn deadline_expired(start_ms: u32, now_ms: u32, limit_ms: u32) -> bool {
    now_ms.wrapping_sub(start_ms) >= limit_ms
}

/// TTL check for an `Option<start_ms>`-tracked value (e.g. main.rs's status
/// message auto-clear): `None` (nothing set) never expires.
pub fn ttl_expired(set_at: Option<u32>, now_ms: u32, ttl_ms: u32) -> bool {
    match set_at {
        Some(start) => deadline_expired(start, now_ms, ttl_ms),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn expires_exactly_at_limit() {
        assert!(!super::deadline_expired(100, 100, 30_000));
        assert!(!super::deadline_expired(100, 30_099, 30_000));
        assert!(super::deadline_expired(100, 30_100, 30_000));
    }

    #[test]
    fn survives_u32_wraparound() {
        let start = u32::MAX - 5_000;
        assert!(!super::deadline_expired(
            start, start.wrapping_add(29_999), 30_000));
        assert!(super::deadline_expired(
            start, start.wrapping_add(30_000), 30_000));
    }

    #[test]
    fn tick_and_now_roundtrip() {
        // Also exercises the critical-section std impl linkage on host.
        let before = super::now_ms();
        super::tick_1ms();
        assert_eq!(super::now_ms(), before.wrapping_add(1));
    }

    #[test]
    fn ttl_expired_none_never_expires() {
        assert!(!super::ttl_expired(None, u32::MAX, 0));
    }

    #[test]
    fn ttl_expired_tracks_deadline() {
        assert!(!super::ttl_expired(Some(1_000), 3_999, 3_000));
        assert!(super::ttl_expired(Some(1_000), 4_000, 3_000));
    }

    #[test]
    fn ttl_expired_survives_wraparound() {
        let start = u32::MAX - 100;
        assert!(!super::ttl_expired(Some(start), start.wrapping_add(2_999), 3_000));
        assert!(super::ttl_expired(Some(start), start.wrapping_add(3_000), 3_000));
    }
}
