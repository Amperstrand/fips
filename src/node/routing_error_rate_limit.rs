//! Routing error signal rate limiting.
//!
//! Prevents routing error floods (CoordsRequired / PathBroken) by
//! rate-limiting error signals per destination address at transit nodes.
//!
//! The destination address is chosen by whoever sent the datagram, so this
//! gate is an aggregate suppressor during a real outage and never a bound on
//! what one sender can induce: a fresh destination is always a first sighting.
//! The bound is `PeerErrorBudget`, keyed on the authenticated link peer and
//! consulted first. What this module owes on top of its interval is that its
//! own map stays bounded and its per-admit cost stays sub-linear whatever the
//! sender does with the key.

use crate::NodeAddr;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Maximum number of destinations this limiter remembers at once.
///
/// A hard ceiling on the map an attacker can grow by varying the destination
/// address. Raising it costs one `NodeAddr` plus one `Instant` per entry and
/// buys interval suppression across more simultaneously-unroutable
/// destinations; lowering it makes admission-without-recording (see
/// [`LimitVerdict::AdmitAtCapacity`]) the common case sooner, which weakens
/// the interval gate but never the peer budget.
const MAX_ENTRIES: usize = 4096;

/// Fraction of `max_age` between amortized sweeps.
///
/// The sweep is a full-map `retain`, so running it on every admit made
/// per-packet cost linear in a map the sender sizes. Eight sweeps per entry
/// lifetime keeps expired entries from accumulating without putting the scan
/// on the per-packet path.
const SWEEPS_PER_MAX_AGE: u32 = 8;

/// What the limiter decided about one candidate error signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitVerdict {
    /// Send it; the destination was recorded.
    Admit,
    /// Send it, but the map was full so the destination was not recorded and
    /// the interval will not suppress its successor.
    ///
    /// This gate fails open deliberately. Failing closed would turn a full map
    /// into node-wide silence on error signalling, and the map is fullest
    /// exactly during partition healing, when many destinations are
    /// legitimately unroutable at once and sources most need the signal. The
    /// bound on emission is the per-peer budget, not this map.
    AdmitAtCapacity,
    /// Suppress it; an error for this destination went out within the
    /// interval.
    Suppress,
}

/// Rate limiter for routing error signals (CoordsRequired / PathBroken).
///
/// Tracks the last time a routing error was sent for each destination
/// address and enforces a minimum interval to prevent floods.
pub struct RoutingErrorRateLimiter {
    /// Maps destination NodeAddr to the last time we sent an error about it.
    last_sent: HashMap<NodeAddr, Instant>,
    /// Minimum interval between error signals for the same destination.
    min_interval: Duration,
    /// Maximum age of entries before cleanup.
    max_age: Duration,
    /// When `cleanup` last ran.
    last_sweep: Instant,
    /// Sweeps run since construction. Read by the tests that hold the
    /// amortization property: a full-map scan per admit is the denial-of-
    /// service multiplier this counter exists to catch coming back.
    sweeps: u64,
}

impl RoutingErrorRateLimiter {
    /// Create a new rate limiter.
    ///
    /// Default: max 10 errors/sec per destination (100ms interval).
    pub fn new() -> Self {
        Self {
            last_sent: HashMap::new(),
            min_interval: Duration::from_millis(100),
            max_age: Duration::from_secs(10),
            last_sweep: Instant::now(),
            sweeps: 0,
        }
    }

    /// Create a rate limiter with a custom minimum interval.
    pub fn with_interval(min_interval: Duration) -> Self {
        Self {
            last_sent: HashMap::new(),
            min_interval,
            max_age: Duration::from_secs(10),
            last_sweep: Instant::now(),
            sweeps: 0,
        }
    }

    /// Check if we should send a routing error for this destination.
    ///
    /// Returns true if enough time has passed since the last error for
    /// this destination, or if this is the first error. Updates internal
    /// state when returning true.
    pub fn should_send(&mut self, dest_addr: &NodeAddr) -> bool {
        self.check(dest_addr, Instant::now()) != LimitVerdict::Suppress
    }

    /// Decide about one error signal at an explicit `now`, reporting whether
    /// the destination could be recorded.
    ///
    /// Callers that distinguish the at-capacity admission use this; callers
    /// that only need a yes or no use [`Self::should_send`].
    pub fn check(&mut self, dest_addr: &NodeAddr, now: Instant) -> LimitVerdict {
        if let Some(&last) = self.last_sent.get(dest_addr)
            && now.saturating_duration_since(last) < self.min_interval
        {
            return LimitVerdict::Suppress;
        }

        if self.last_sent.len() >= MAX_ENTRIES && !self.last_sent.contains_key(dest_addr) {
            self.maybe_cleanup(now);
            if self.last_sent.len() >= MAX_ENTRIES {
                return LimitVerdict::AdmitAtCapacity;
            }
        }

        self.last_sent.insert(*dest_addr, now);
        self.maybe_cleanup(now);
        LimitVerdict::Admit
    }

    /// Run the sweep if one is due.
    fn maybe_cleanup(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_sweep) >= self.max_age / SWEEPS_PER_MAX_AGE {
            self.cleanup(now);
        }
    }

    /// Remove entries older than max_age.
    fn cleanup(&mut self, now: Instant) {
        self.last_sweep = now;
        self.sweeps += 1;
        self.last_sent
            .retain(|_, &mut last| now.saturating_duration_since(last) < self.max_age);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.last_sent.len()
    }

    #[cfg(test)]
    pub fn sweeps(&self) -> u64 {
        self.sweeps
    }
}

impl Default for RoutingErrorRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn addr(val: u8) -> NodeAddr {
        let mut bytes = [0u8; 16];
        bytes[0] = val;
        NodeAddr::from_bytes(bytes)
    }

    /// A distinct destination address per index, standing for the fresh
    /// `dest_addr` a flooding sender puts on every datagram.
    fn minted_addr(val: u32) -> NodeAddr {
        let mut bytes = [0u8; 16];
        bytes[..4].copy_from_slice(&val.to_le_bytes());
        bytes[15] = 0xff;
        NodeAddr::from_bytes(bytes)
    }

    #[test]
    fn test_first_send_allowed() {
        let mut limiter = RoutingErrorRateLimiter::new();
        assert!(limiter.should_send(&addr(1)));
    }

    #[test]
    fn test_rapid_sends_rate_limited() {
        let mut limiter = RoutingErrorRateLimiter::new();
        assert!(limiter.should_send(&addr(1)));
        assert!(!limiter.should_send(&addr(1)));
        assert!(!limiter.should_send(&addr(1)));
    }

    #[test]
    fn test_different_destinations_independent() {
        let mut limiter = RoutingErrorRateLimiter::new();
        assert!(limiter.should_send(&addr(1)));
        assert!(limiter.should_send(&addr(2)));
        assert!(!limiter.should_send(&addr(1)));
        assert!(!limiter.should_send(&addr(2)));
    }

    #[test]
    fn test_send_allowed_after_interval() {
        let mut limiter = RoutingErrorRateLimiter::new();
        assert!(limiter.should_send(&addr(1)));

        thread::sleep(Duration::from_millis(110));

        assert!(limiter.should_send(&addr(1)));
    }

    #[test]
    fn test_cleanup_removes_old_entries() {
        let mut limiter = RoutingErrorRateLimiter::new();
        assert!(limiter.should_send(&addr(1)));
        assert!(limiter.should_send(&addr(2)));
        assert_eq!(limiter.len(), 2);

        let future = Instant::now() + Duration::from_secs(11);
        limiter.cleanup(future);
        assert_eq!(limiter.len(), 0);
    }

    #[test]
    fn test_cleanup_preserves_recent_entries() {
        let mut limiter = RoutingErrorRateLimiter::new();
        assert!(limiter.should_send(&addr(1)));
        assert_eq!(limiter.len(), 1);

        limiter.cleanup(Instant::now());
        assert_eq!(limiter.len(), 1);
    }

    #[test]
    fn the_map_stays_bounded_when_a_sender_mints_distinct_destination_keys() {
        let mut limiter = RoutingErrorRateLimiter::new();
        let now = Instant::now();

        for i in 0..100_000u32 {
            limiter.check(&minted_addr(i), now);
        }

        assert!(
            limiter.len() <= MAX_ENTRIES,
            "limiter held {} entries, above the {MAX_ENTRIES} ceiling",
            limiter.len()
        );
    }

    #[test]
    fn an_admission_at_capacity_still_sends_rather_than_going_silent() {
        let mut limiter = RoutingErrorRateLimiter::new();
        let now = Instant::now();

        for i in 0..MAX_ENTRIES as u32 {
            assert_eq!(limiter.check(&minted_addr(i), now), LimitVerdict::Admit);
        }

        // The map is full and nothing in it is old enough to evict, so the
        // next distinct destination cannot be recorded. It must still be sent.
        assert_eq!(
            limiter.check(&minted_addr(MAX_ENTRIES as u32), now),
            LimitVerdict::AdmitAtCapacity
        );
    }

    #[test]
    fn the_map_scan_does_not_run_once_per_admitted_destination() {
        let mut limiter = RoutingErrorRateLimiter::new();
        let now = Instant::now();
        let before = limiter.sweeps();

        for i in 0..1_000u32 {
            limiter.check(&minted_addr(i), now);
        }

        assert_eq!(
            limiter.sweeps() - before,
            0,
            "the full-map scan ran inside a single sweep interval"
        );
    }

    #[test]
    fn the_map_scan_still_runs_once_a_sweep_interval_has_passed() {
        let mut limiter = RoutingErrorRateLimiter::new();
        let start = Instant::now();
        limiter.check(&minted_addr(0), start);
        let before = limiter.sweeps();

        let later = start + Duration::from_secs(11);
        limiter.check(&minted_addr(1), later);

        assert_eq!(limiter.sweeps() - before, 1);
        // The first destination aged out, so the sweep did its job.
        assert_eq!(limiter.len(), 1);
    }

    #[test]
    fn test_with_interval_custom_rate() {
        let mut limiter = RoutingErrorRateLimiter::with_interval(Duration::from_millis(500));
        assert!(limiter.should_send(&addr(1)));
        assert!(!limiter.should_send(&addr(1)));

        // Still rate-limited after 200ms (would pass with default 100ms)
        thread::sleep(Duration::from_millis(200));
        assert!(!limiter.should_send(&addr(1)));

        // Allowed after 500ms total
        thread::sleep(Duration::from_millis(350));
        assert!(limiter.should_send(&addr(1)));
    }
}
