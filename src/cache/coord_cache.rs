//! Coordinate cache for routing decisions.
//!
//! Maps node addresses to their tree coordinates, enabling data packets
//! to be routed without carrying coordinates in every packet. Populated
//! by SessionSetup packets.

use std::collections::HashMap;

use super::CacheStats;
use super::entry::CacheEntry;
use crate::NodeAddr;
use crate::proto::stp::TreeCoordinate;

/// Default maximum entries in coordinate cache.
pub const DEFAULT_COORD_CACHE_SIZE: usize = 50_000;

/// Default TTL for coordinate cache entries (5 minutes in milliseconds).
pub const DEFAULT_COORD_CACHE_TTL_MS: u64 = 300_000;

/// What a hint write did, which is the only place the precedence rule is
/// observable.
///
/// `#[must_use]` on purpose. A hint write can be refused, and a caller that
/// drops the outcome cannot tell a stored coordinate from a rejected one. It
/// also makes the compiler, rather than review, the thing that notices when a
/// write site is left on the hint path that should have been verified.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintOutcome {
    /// No entry existed; the hint was stored.
    Inserted,
    /// An entry existed and the hint replaced it with a different value.
    ///
    /// This is the security-interesting outcome. A destination's coordinates
    /// changing is ordinary when it moves in the tree and is also exactly what
    /// a poisoning looks like, so the two are not distinguishable here and the
    /// counter is a rate to watch rather than an alarm.
    Changed,
    /// An entry existed and the hint carried the same value.
    Unchanged,
    /// An entry existed, was verified and still within its verification
    /// window, so the hint was refused.
    Rejected,
}

/// Coordinate cache for routing decisions.
///
/// Maps node addresses to their tree coordinates, enabling data packets
/// to be routed without carrying coordinates in every packet. Populated
/// by SessionSetup packets.
#[derive(Clone, Debug)]
pub struct CoordCache {
    /// NodeAddr -> coordinates mapping.
    entries: HashMap<NodeAddr, CacheEntry>,
    /// Maximum number of entries.
    max_entries: usize,
    /// Default TTL for entries (milliseconds).
    default_ttl_ms: u64,
}

impl CoordCache {
    /// Create a new coordinate cache.
    pub fn new(max_entries: usize, default_ttl_ms: u64) -> Self {
        Self {
            entries: HashMap::with_capacity(max_entries.min(1000)),
            max_entries,
            default_ttl_ms,
        }
    }

    /// Create a cache with default parameters.
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_COORD_CACHE_SIZE, DEFAULT_COORD_CACHE_TTL_MS)
    }

    /// Get the maximum capacity.
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Get the default TTL.
    pub fn default_ttl_ms(&self) -> u64 {
        self.default_ttl_ms
    }

    /// Set the default TTL.
    pub fn set_default_ttl_ms(&mut self, ttl_ms: u64) {
        self.default_ttl_ms = ttl_ms;
    }

    /// Insert or update a cache entry from an unauthenticated hint.
    ///
    /// **This is the only way to write a coordinate learned off the wire, and
    /// it is deliberately the obvious name.** A hint never displaces an entry
    /// that a verified lookup established and whose verification has not yet
    /// aged out; see [`CacheEntry::is_verified`]. Conferring trust requires
    /// asking for it by name, with [`CoordCache::insert_verified`].
    pub fn insert(
        &mut self,
        addr: NodeAddr,
        coords: TreeCoordinate,
        current_time_ms: u64,
    ) -> HintOutcome {
        self.insert_hint_with_ttl(addr, coords, current_time_ms, self.default_ttl_ms)
    }

    /// Insert or update a cache entry from a hint, with an explicit TTL.
    fn insert_hint_with_ttl(
        &mut self,
        addr: NodeAddr,
        coords: TreeCoordinate,
        current_time_ms: u64,
        ttl_ms: u64,
    ) -> HintOutcome {
        if let Some(entry) = self.entries.get_mut(&addr) {
            if entry.is_verified(current_time_ms) {
                return HintOutcome::Rejected;
            }
            let changed = entry.coords() != &coords;
            entry.update(coords, current_time_ms, ttl_ms);
            return if changed {
                HintOutcome::Changed
            } else {
                HintOutcome::Unchanged
            };
        }

        // Evict if at capacity
        if self.entries.len() >= self.max_entries {
            self.evict_one(current_time_ms);
        }

        // Eviction can decline to free a slot when every entry is a live
        // verified one, which is the case the hint must not be allowed to
        // force. Refuse rather than grow past the cap.
        if self.entries.len() >= self.max_entries {
            return HintOutcome::Rejected;
        }

        let entry = CacheEntry::new(coords, current_time_ms, ttl_ms);
        self.entries.insert(addr, entry);
        HintOutcome::Inserted
    }

    /// Insert or update a cache entry from a lookup whose proof was verified.
    ///
    /// Unconditional: a verified value displaces whatever was there, which is
    /// the point — it is how a poisoned entry gets corrected.
    pub fn insert_verified(
        &mut self,
        addr: NodeAddr,
        coords: TreeCoordinate,
        current_time_ms: u64,
    ) {
        if let Some(entry) = self.entries.get_mut(&addr) {
            entry.update_verified(coords, current_time_ms, self.default_ttl_ms);
            return;
        }

        if self.entries.len() >= self.max_entries {
            self.evict_one(current_time_ms);
        }

        let entry = CacheEntry::new_verified(coords, current_time_ms, self.default_ttl_ms);
        self.entries.insert(addr, entry);
    }

    /// Insert or update a verified cache entry with path MTU information.
    ///
    /// Used by discovery response handling to store the discovered path MTU
    /// alongside the target's coordinates. Verified for the same reason
    /// [`CoordCache::insert_verified`] is: the caller checked the proof.
    pub fn insert_verified_with_path_mtu(
        &mut self,
        addr: NodeAddr,
        coords: TreeCoordinate,
        current_time_ms: u64,
        path_mtu: u16,
    ) {
        if let Some(entry) = self.entries.get_mut(&addr) {
            entry.update_verified(coords, current_time_ms, self.default_ttl_ms);
            entry.set_path_mtu(path_mtu);
            return;
        }

        if self.entries.len() >= self.max_entries {
            self.evict_one(current_time_ms);
        }

        let mut entry = CacheEntry::new_verified(coords, current_time_ms, self.default_ttl_ms);
        entry.set_path_mtu(path_mtu);
        self.entries.insert(addr, entry);
    }

    /// Insert with a custom TTL.
    pub fn insert_with_ttl(
        &mut self,
        addr: NodeAddr,
        coords: TreeCoordinate,
        current_time_ms: u64,
        ttl_ms: u64,
    ) -> HintOutcome {
        self.insert_hint_with_ttl(addr, coords, current_time_ms, ttl_ms)
    }

    /// Look up coordinates for an address (without touching).
    pub fn get(&self, addr: &NodeAddr, current_time_ms: u64) -> Option<&TreeCoordinate> {
        self.entries.get(addr).and_then(|entry| {
            if entry.is_expired(current_time_ms) {
                None
            } else {
                Some(entry.coords())
            }
        })
    }

    /// Look up coordinates and refresh (update last_used and extend TTL).
    pub fn get_and_touch(
        &mut self,
        addr: &NodeAddr,
        current_time_ms: u64,
    ) -> Option<&TreeCoordinate> {
        // Check and remove if expired
        if let Some(entry) = self.entries.get(addr)
            && entry.is_expired(current_time_ms)
        {
            self.entries.remove(addr);
            return None;
        }

        // Refresh TTL and return
        if let Some(entry) = self.entries.get_mut(addr) {
            entry.refresh(current_time_ms, self.default_ttl_ms);
            Some(entry.coords())
        } else {
            None
        }
    }

    /// Get the full cache entry.
    pub fn get_entry(&self, addr: &NodeAddr) -> Option<&CacheEntry> {
        self.entries.get(addr)
    }

    /// Remove an entry.
    pub fn remove(&mut self, addr: &NodeAddr) -> Option<CacheEntry> {
        self.entries.remove(addr)
    }

    /// Demote an entry to an unverified hint, keeping its coordinates and TTL.
    ///
    /// The entry goes on routing, but a hint may now replace it. Returns
    /// whether an entry existed.
    pub fn demote(&mut self, addr: &NodeAddr) -> bool {
        match self.entries.get_mut(addr) {
            Some(entry) => {
                entry.mark_hint();
                true
            }
            None => false,
        }
    }

    /// Forget the path MTU stored with an entry, keeping the entry.
    pub fn clear_path_mtu(&mut self, addr: &NodeAddr) {
        if let Some(entry) = self.entries.get_mut(addr) {
            entry.clear_path_mtu();
        }
    }

    /// Check if an address is cached (and not expired).
    pub fn contains(&self, addr: &NodeAddr, current_time_ms: u64) -> bool {
        self.get(addr, current_time_ms).is_some()
    }

    /// Number of entries (including expired).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over non-expired entries.
    pub fn iter(&self, current_time_ms: u64) -> impl Iterator<Item = (&NodeAddr, &CacheEntry)> {
        self.entries
            .iter()
            .filter(move |(_, entry)| !entry.is_expired(current_time_ms))
    }

    /// Remove all expired entries.
    pub fn purge_expired(&mut self, current_time_ms: u64) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|_, entry| !entry.is_expired(current_time_ms));
        before - self.entries.len()
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Drop entries whose cached destination ancestry contains the given
    /// `NodeAddr`.
    ///
    /// Used at parent-position-change sites: when our own position in the
    /// tree changes, destinations downstream of us (whose cached coordinates
    /// embed our previous prefix) have stale path information and must be
    /// re-learned. Entries whose ancestry does not include `node_addr` are
    /// unaffected by the local position change and are retained.
    ///
    /// Returns the count of entries removed.
    pub fn invalidate_via_node(&mut self, node_addr: &NodeAddr) -> usize {
        let len_before = self.entries.len();
        self.entries
            .retain(|_, entry| !entry.coords().contains(node_addr));
        len_before - self.entries.len()
    }

    /// Drop entries whose cached destination `root_id` differs from
    /// `current_root`.
    ///
    /// Used at root-change sites (become_root, root handover via
    /// TreeAnnounce). `find_next_hop` returns `None` for any destination
    /// whose root does not match the local root, so entries from a stale
    /// root cannot route and would otherwise occupy cache slots until
    /// TTL expiry.
    ///
    /// Returns the count of entries removed.
    pub fn invalidate_other_roots(&mut self, current_root: &NodeAddr) -> usize {
        let len_before = self.entries.len();
        self.entries
            .retain(|_, entry| entry.coords().root_id() == current_root);
        len_before - self.entries.len()
    }

    /// Evict one entry (expired first, then LRU).
    fn evict_one(&mut self, current_time_ms: u64) {
        // First try to evict an expired entry
        let expired_key = self
            .entries
            .iter()
            .find(|(_, e)| e.is_expired(current_time_ms))
            .map(|(k, _)| *k);

        if let Some(key) = expired_key {
            self.entries.remove(&key);
            return;
        }

        // Otherwise evict the LRU among entries that are not live-verified.
        //
        // Restricting the victim pool is what stops a hint flood from
        // manufacturing the empty slot the precedence rule depends on: without
        // it, an attacker fills the cache with hints until a verified entry
        // becomes the LRU, evicts it, and then plants into a slot that is now
        // empty and so accepts an ordinary first write. Declining to evict is
        // the correct outcome when every entry is live-verified; the caller
        // refuses the hint rather than growing past the cap.
        let lru_key = self
            .entries
            .iter()
            .filter(|(_, e)| !e.is_verified(current_time_ms))
            .max_by_key(|(_, e)| e.idle_time(current_time_ms))
            .map(|(k, _)| *k);

        if let Some(key) = lru_key {
            self.entries.remove(&key);
        }
    }

    /// Get cache statistics.
    pub fn stats(&self, current_time_ms: u64) -> CacheStats {
        let mut expired = 0;
        let mut total_age = 0u64;

        for entry in self.entries.values() {
            if entry.is_expired(current_time_ms) {
                expired += 1;
            }
            total_age += entry.age(current_time_ms);
        }

        CacheStats {
            entries: self.entries.len(),
            max_entries: self.max_entries,
            expired,
            avg_age_ms: if self.entries.is_empty() {
                0
            } else {
                total_age / self.entries.len() as u64
            },
        }
    }
}

impl Default for CoordCache {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
#[path = "tests/coord_cache.rs"]
mod tests;
