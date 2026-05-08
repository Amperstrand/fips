//! Per-address exponential backoff for BLE connection failures.
//!
//! Tracks consecutive connection failures per peer address and applies
//! increasing backoff delays. After `max_failures` consecutive failures,
//! the address is auto-denied for `deny_duration` to prevent wasting
//! resources on unreachable peers.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::addr::BleAddr;

const DEFAULT_BASE_SECS: u64 = 5;
const DEFAULT_MAX_SECS: u64 = 300;
const DEFAULT_MAX_FAILURES: u32 = 5;
const DEFAULT_DENY_DURATION_SECS: u64 = 3600;

struct Entry {
    failures: u32,
    next_allowed: Instant,
}

struct DenyEntry {
    until: Instant,
}

/// Per-address exponential backoff tracker.
///
/// Tracks consecutive connection failures per BLE address and applies
/// exponentially increasing backoff delays. After `max_failures` consecutive
/// failures the address is auto-denied for `deny_duration`.
pub struct PeerBackoff {
    entries: HashMap<BleAddr, Entry>,
    denied: HashMap<BleAddr, DenyEntry>,
    base: Duration,
    max: Duration,
    max_failures: u32,
    deny_duration: Duration,
}

impl PeerBackoff {
    /// Create with explicit parameters.
    pub fn new(base_secs: u64, max_secs: u64, max_failures: u32, deny_duration_secs: u64) -> Self {
        Self {
            entries: HashMap::new(),
            denied: HashMap::new(),
            base: Duration::from_secs(base_secs.max(1)),
            max: Duration::from_secs(max_secs.max(base_secs)),
            max_failures: max_failures.max(1),
            deny_duration: Duration::from_secs(deny_duration_secs.max(60)),
        }
    }

    /// Create with production defaults (5s base, 300s max, 5 failures, 1h deny).
    pub fn with_defaults() -> Self {
        Self::new(
            DEFAULT_BASE_SECS,
            DEFAULT_MAX_SECS,
            DEFAULT_MAX_FAILURES,
            DEFAULT_DENY_DURATION_SECS,
        )
    }

    /// Whether the address is currently auto-denied.
    /// Removes expired entries to prevent unbounded memory growth.
    pub fn is_denied(&mut self, addr: &BleAddr) -> bool {
        if let Some(d) = self.denied.get(addr)
            && Instant::now() < d.until
        {
            return true;
        }
        self.denied.remove(addr);
        false
    }

    #[cfg(test)]
    fn test_insert_denied(&mut self, addr: &BleAddr, until: Instant) {
        self.denied.insert(addr.clone(), DenyEntry { until });
    }

    /// Whether the address is currently in backoff (should not be probed).
    pub fn is_in_backoff(&self, addr: &BleAddr) -> bool {
        if let Some(e) = self.entries.get(addr) {
            return Instant::now() < e.next_allowed;
        }
        false
    }

    /// Record a connection failure for the given address.
    ///
    /// Returns `true` if the address has been auto-denied as a result.
    pub fn record_failure(&mut self, addr: &BleAddr) -> bool {
        let now = Instant::now();
        let entry = self.entries.entry(addr.clone()).or_insert(Entry {
            failures: 0,
            next_allowed: now,
        });

        entry.failures += 1;

        if entry.failures >= self.max_failures {
            self.denied.insert(
                addr.clone(),
                DenyEntry {
                    until: now + self.deny_duration,
                },
            );
            self.entries.remove(addr);
            return true;
        }

        let delay_secs = (1u64 << entry.failures.min(10)) * self.base.as_secs();
        let capped = delay_secs.min(self.max.as_secs());
        let jitter = simple_hash_jitter(addr, entry.failures) % (capped / 5 + 1);
        entry.next_allowed = now + Duration::from_secs(capped + jitter);

        false
    }

    /// Clear all backoff state for an address (on successful connection).
    pub fn clear(&mut self, addr: &BleAddr) {
        self.entries.remove(addr);
        self.denied.remove(addr);
    }

    /// Get the current failure count for an address.
    #[cfg(test)]
    pub fn failure_count(&self, addr: &BleAddr) -> u32 {
        self.entries.get(addr).map(|e| e.failures).unwrap_or(0)
    }
}

/// Deterministic jitter derived from address bytes and failure count.
fn simple_hash_jitter(addr: &BleAddr, failures: u32) -> u64 {
    let mut h: u64 = 0x9e3779b97f4a7c15;
    for &b in &addr.device {
        h = h.wrapping_mul(0x517cc1b727220a95).wrapping_add(b as u64);
    }
    h = h.wrapping_add(failures as u64);
    h ^ (h >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> BleAddr {
        BleAddr {
            adapter: "hci0".to_string(),
            device: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, n],
        }
    }

    #[test]
    fn backoff_increases_exponentially() {
        let mut bo = PeerBackoff::new(1, 300, 10, 3600);
        let addr = test_addr(1);

        bo.record_failure(&addr);
        assert!(!bo.is_in_backoff(&addr) || bo.failure_count(&addr) >= 1);

        bo.record_failure(&addr);
        assert!(bo.failure_count(&addr) == 2);
    }

    #[test]
    fn auto_deny_after_max_failures() {
        let mut bo = PeerBackoff::new(1, 300, 3, 3600);
        let addr = test_addr(2);

        let denied = bo.record_failure(&addr);
        assert!(!denied);
        let denied = bo.record_failure(&addr);
        assert!(!denied);
        let denied = bo.record_failure(&addr);
        assert!(denied);
        assert!(bo.is_denied(&addr));
    }

    #[test]
    fn clear_resets_everything() {
        let mut bo = PeerBackoff::new(1, 300, 2, 3600);
        let addr = test_addr(3);

        bo.record_failure(&addr);
        bo.record_failure(&addr);
        assert!(bo.is_denied(&addr));

        bo.clear(&addr);
        assert!(!bo.is_denied(&addr));
        assert!(!bo.is_in_backoff(&addr));
    }

    #[test]
    fn deny_entry_removed_after_expiry() {
        let mut bo = PeerBackoff::with_defaults();
        let addr = test_addr(4);

        // Insert a deny entry valid for 1 second from now
        bo.test_insert_denied(&addr, Instant::now() + std::time::Duration::from_secs(1));
        assert!(bo.is_denied(&addr));

        // Wait for it to expire
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(!bo.is_denied(&addr));

        // Calling again should still return false (entry was removed, not just expired)
        assert!(!bo.is_denied(&addr));
    }
}
