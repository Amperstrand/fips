use super::*;
use crate::cache::entry::VERIFIED_TTL_MS;

fn make_node_addr(val: u8) -> NodeAddr {
    let mut bytes = [0u8; 16];
    bytes[0] = val;
    NodeAddr::from_bytes(bytes)
}

fn make_coords(ids: &[u8]) -> TreeCoordinate {
    TreeCoordinate::from_addrs(ids.iter().map(|&v| make_node_addr(v)).collect()).unwrap()
}

#[test]
fn test_coord_cache_basic() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);
    let coords = make_coords(&[1, 0]);

    let _ = cache.insert(addr, coords.clone(), 0);

    assert!(cache.contains(&addr, 0));
    assert_eq!(cache.get(&addr, 0), Some(&coords));
    assert_eq!(cache.len(), 1);
}

#[test]
fn test_coord_cache_expiry() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);
    let coords = make_coords(&[1, 0]);

    let _ = cache.insert(addr, coords, 0);

    assert!(cache.contains(&addr, 500));
    assert!(!cache.contains(&addr, 1500));
}

#[test]
fn test_coord_cache_update() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);

    let _ = cache.insert(addr, make_coords(&[1, 0]), 0);
    let _ = cache.insert(addr, make_coords(&[1, 2, 0]), 500);

    assert_eq!(cache.len(), 1);
    let coords = cache.get(&addr, 500).unwrap();
    assert_eq!(coords.depth(), 2);
}

#[test]
fn test_coord_cache_eviction() {
    let mut cache = CoordCache::new(2, 10000);

    let addr1 = make_node_addr(1);
    let addr2 = make_node_addr(2);
    let addr3 = make_node_addr(3);

    let _ = cache.insert(addr1, make_coords(&[1, 0]), 0);
    let _ = cache.insert(addr2, make_coords(&[2, 0]), 100);

    // Touch addr2 to make it more recent
    let _ = cache.get_and_touch(&addr2, 200);

    // Insert addr3, should evict addr1 (LRU)
    let _ = cache.insert(addr3, make_coords(&[3, 0]), 300);

    assert!(!cache.contains(&addr1, 300));
    assert!(cache.contains(&addr2, 300));
    assert!(cache.contains(&addr3, 300));
}

#[test]
fn test_coord_cache_evict_expired_first() {
    let mut cache = CoordCache::new(2, 100);

    let _ = cache.insert(make_node_addr(1), make_coords(&[1, 0]), 0);
    let _ = cache.insert(make_node_addr(2), make_coords(&[2, 0]), 50);

    // At time 150, addr1 is expired, addr2 is not
    let _ = cache.insert(make_node_addr(3), make_coords(&[3, 0]), 150);

    // addr1 should be evicted (expired), not addr2 (LRU but not expired)
    assert!(!cache.contains(&make_node_addr(1), 150));
    assert!(cache.contains(&make_node_addr(2), 150));
    assert!(cache.contains(&make_node_addr(3), 150));
}

#[test]
fn test_coord_cache_purge_expired() {
    let mut cache = CoordCache::new(100, 100);

    let _ = cache.insert(make_node_addr(1), make_coords(&[1, 0]), 0); // expires at 100
    let _ = cache.insert(make_node_addr(2), make_coords(&[2, 0]), 50); // expires at 150
    let _ = cache.insert(make_node_addr(3), make_coords(&[3, 0]), 200); // expires at 300

    assert_eq!(cache.len(), 3);

    let purged = cache.purge_expired(151); // both addr1 and addr2 expired

    // Entry 1 and 2 expired, entry 3 still valid
    assert_eq!(purged, 2);
    assert_eq!(cache.len(), 1);
    assert!(cache.contains(&make_node_addr(3), 151));
}

#[test]
fn test_coord_cache_stats() {
    let mut cache = CoordCache::new(100, 100);

    let _ = cache.insert(make_node_addr(1), make_coords(&[1, 0]), 0);
    let _ = cache.insert(make_node_addr(2), make_coords(&[2, 0]), 50);

    let stats = cache.stats(150);

    assert_eq!(stats.entries, 2);
    assert_eq!(stats.max_entries, 100);
    assert_eq!(stats.expired, 1); // addr1 expired
    assert!(stats.avg_age_ms > 0);
}

#[test]
fn test_coord_cache_insert_with_ttl() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);

    let _ = cache.insert_with_ttl(addr, make_coords(&[1, 0]), 0, 200);

    // Should expire at 200, not the default 1000
    assert!(cache.contains(&addr, 100));
    assert!(!cache.contains(&addr, 201));
}

#[test]
fn test_coord_cache_insert_with_ttl_update() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);

    let _ = cache.insert_with_ttl(addr, make_coords(&[1, 0]), 0, 200);
    let _ = cache.insert_with_ttl(addr, make_coords(&[1, 2, 0]), 100, 300);

    assert_eq!(cache.len(), 1);
    let coords = cache.get(&addr, 100).unwrap();
    assert_eq!(coords.depth(), 2);
    // New TTL: 100 + 300 = 400
    assert!(cache.contains(&addr, 399));
    assert!(!cache.contains(&addr, 401));
}

#[test]
fn test_coord_cache_get_and_touch_removes_expired() {
    let mut cache = CoordCache::new(100, 100);
    let addr = make_node_addr(1);

    let _ = cache.insert(addr, make_coords(&[1, 0]), 0);
    assert_eq!(cache.len(), 1);

    // Entry expired at time 200
    let result = cache.get_and_touch(&addr, 200);
    assert!(result.is_none());
    // Entry should be removed from the map
    assert_eq!(cache.len(), 0);
}

#[test]
fn test_coord_cache_get_entry() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);

    let _ = cache.insert(addr, make_coords(&[1, 0]), 500);

    let entry = cache.get_entry(&addr).unwrap();
    assert_eq!(entry.created_at(), 500);
    assert_eq!(entry.expires_at(), 1500);

    assert!(cache.get_entry(&make_node_addr(99)).is_none());
}

#[test]
fn test_coord_cache_remove() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);

    let _ = cache.insert(addr, make_coords(&[1, 0]), 0);
    assert_eq!(cache.len(), 1);

    let removed = cache.remove(&addr);
    assert!(removed.is_some());
    assert_eq!(cache.len(), 0);

    // Removing again returns None
    assert!(cache.remove(&addr).is_none());
}

#[test]
fn test_coord_cache_clear_and_is_empty() {
    let mut cache = CoordCache::new(100, 1000);

    assert!(cache.is_empty());

    let _ = cache.insert(make_node_addr(1), make_coords(&[1, 0]), 0);
    let _ = cache.insert(make_node_addr(2), make_coords(&[2, 0]), 0);

    assert!(!cache.is_empty());

    cache.clear();
    assert!(cache.is_empty());
    assert_eq!(cache.len(), 0);
}

#[test]
fn test_coord_cache_default() {
    let cache = CoordCache::default();

    assert_eq!(cache.max_entries(), DEFAULT_COORD_CACHE_SIZE);
    assert_eq!(cache.default_ttl_ms(), DEFAULT_COORD_CACHE_TTL_MS);
    assert!(cache.is_empty());
}

#[test]
fn test_coord_cache_set_default_ttl() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);

    cache.set_default_ttl_ms(200);
    assert_eq!(cache.default_ttl_ms(), 200);

    let _ = cache.insert(addr, make_coords(&[1, 0]), 0);
    // New TTL applies: expires at 200
    assert!(cache.contains(&addr, 100));
    assert!(!cache.contains(&addr, 201));
}

#[test]
fn test_coord_cache_stats_empty() {
    let cache = CoordCache::new(100, 1000);
    let stats = cache.stats(0);

    assert_eq!(stats.entries, 0);
    assert_eq!(stats.max_entries, 100);
    assert_eq!(stats.expired, 0);
    assert_eq!(stats.avg_age_ms, 0);
}

// ===== Surgical invalidation tests =====

#[test]
fn test_invalidate_via_node_at_self_depth() {
    // Entry whose own NodeAddr (depth 0) is the invalidation target.
    let mut cache = CoordCache::new(100, 1000);
    let target = make_node_addr(1);

    let _ = cache.insert(target, make_coords(&[1, 0]), 0);
    assert_eq!(cache.len(), 1);

    let removed = cache.invalidate_via_node(&target);
    assert_eq!(removed, 1);
    assert_eq!(cache.len(), 0);
}

#[test]
fn test_invalidate_via_node_interior() {
    // Entry whose ancestry contains the target in the interior of the path.
    let mut cache = CoordCache::new(100, 1000);
    let dest = make_node_addr(5);
    // Path: 5 -> 3 -> 1 -> 0 (root). Target 3 appears at depth 1.
    let _ = cache.insert(dest, make_coords(&[5, 3, 1, 0]), 0);

    let removed = cache.invalidate_via_node(&make_node_addr(3));
    assert_eq!(removed, 1);
    assert_eq!(cache.len(), 0);
}

#[test]
fn test_invalidate_via_node_absent() {
    // Entry whose ancestry does NOT contain the target must be retained.
    let mut cache = CoordCache::new(100, 1000);
    let dest = make_node_addr(5);
    let _ = cache.insert(dest, make_coords(&[5, 3, 1, 0]), 0);

    let removed = cache.invalidate_via_node(&make_node_addr(99));
    assert_eq!(removed, 0);
    assert_eq!(cache.len(), 1);
    assert!(cache.contains(&dest, 0));
}

#[test]
fn test_invalidate_via_node_empty_cache() {
    let mut cache = CoordCache::new(100, 1000);
    let removed = cache.invalidate_via_node(&make_node_addr(1));
    assert_eq!(removed, 0);
    assert_eq!(cache.len(), 0);
}

#[test]
fn test_invalidate_other_roots_current_root_kept() {
    let mut cache = CoordCache::new(100, 1000);
    // Entries rooted at addr(0)
    let _ = cache.insert(make_node_addr(1), make_coords(&[1, 0]), 0);
    let _ = cache.insert(make_node_addr(2), make_coords(&[2, 0]), 0);

    let removed = cache.invalidate_other_roots(&make_node_addr(0));
    assert_eq!(removed, 0);
    assert_eq!(cache.len(), 2);
}

#[test]
fn test_invalidate_other_roots_different_root_dropped() {
    let mut cache = CoordCache::new(100, 1000);
    // Three entries rooted at addr(0), one rooted at addr(9)
    let _ = cache.insert(make_node_addr(1), make_coords(&[1, 0]), 0);
    let _ = cache.insert(make_node_addr(2), make_coords(&[2, 0]), 0);
    let _ = cache.insert(make_node_addr(3), make_coords(&[3, 0]), 0);
    let _ = cache.insert(make_node_addr(4), make_coords(&[4, 9]), 0);

    let removed = cache.invalidate_other_roots(&make_node_addr(0));
    assert_eq!(removed, 1);
    assert_eq!(cache.len(), 3);
    assert!(!cache.contains(&make_node_addr(4), 0));
    assert!(cache.contains(&make_node_addr(1), 0));
}

#[test]
fn test_invalidate_other_roots_all_match() {
    let mut cache = CoordCache::new(100, 1000);
    let _ = cache.insert(make_node_addr(1), make_coords(&[1, 0]), 0);
    let _ = cache.insert(make_node_addr(2), make_coords(&[2, 0]), 0);

    let removed = cache.invalidate_other_roots(&make_node_addr(0));
    assert_eq!(removed, 0);
    assert_eq!(cache.len(), 2);
}

#[test]
fn test_invalidate_other_roots_empty_cache() {
    let mut cache = CoordCache::new(100, 1000);
    let removed = cache.invalidate_other_roots(&make_node_addr(0));
    assert_eq!(removed, 0);
    assert_eq!(cache.len(), 0);
}

#[test]
fn a_hint_does_not_displace_a_live_verified_entry() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);
    let good = make_coords(&[1, 0]);
    let forged = make_coords(&[1, 2, 0]);

    cache.insert_verified(addr, good.clone(), 0);
    assert_eq!(cache.insert(addr, forged, 10), HintOutcome::Rejected);
    assert_eq!(
        cache.get(&addr, 10),
        Some(&good),
        "the verified value must survive the hint"
    );
}

#[test]
fn a_verified_write_displaces_a_hint() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);
    let hint = make_coords(&[1, 2, 0]);
    let good = make_coords(&[1, 0]);

    assert_eq!(cache.insert(addr, hint, 0), HintOutcome::Inserted);
    cache.insert_verified(addr, good.clone(), 10);
    assert_eq!(
        cache.get(&addr, 10),
        Some(&good),
        "a proof must be able to correct a poisoned entry"
    );
}

#[test]
fn verification_ages_out_so_a_stale_verified_entry_stops_refusing_hints() {
    let mut cache = CoordCache::new(100, u64::MAX / 4);
    let addr = make_node_addr(1);
    cache.insert_verified(addr, make_coords(&[1, 0]), 0);

    // Inside the window: refused.
    assert_eq!(
        cache.insert(addr, make_coords(&[1, 2, 0]), VERIFIED_TTL_MS),
        HintOutcome::Rejected
    );
    // One millisecond past it: accepted, so a destination that genuinely
    // moved is not locked out forever by a verification nobody renews.
    let moved = make_coords(&[1, 3, 0]);
    assert_eq!(
        cache.insert(addr, moved.clone(), VERIFIED_TTL_MS + 1),
        HintOutcome::Changed
    );
    assert_eq!(cache.get(&addr, VERIFIED_TTL_MS + 1), Some(&moved));
}

#[test]
fn ordinary_traffic_does_not_extend_the_verification_window() {
    // The entry TTL has to be long enough that the touches below keep the
    // entry alive; the test is about the verification clock, not expiry.
    let mut cache = CoordCache::new(100, VERIFIED_TTL_MS);
    let addr = make_node_addr(1);
    cache.insert_verified(addr, make_coords(&[1, 0]), 0);

    // Touch it repeatedly the way forwarding does, right up to the edge.
    for t in [100, 1000, 100_000, VERIFIED_TTL_MS] {
        let _ = cache.get_and_touch(&addr, t);
    }

    // The entry is alive but its verification has aged out on its own
    // clock, which is the whole point of keeping the two clocks separate.
    assert_eq!(
        cache.insert(addr, make_coords(&[1, 2, 0]), VERIFIED_TTL_MS + 1),
        HintOutcome::Changed,
        "refresh must not carry the verification forward"
    );
}

#[test]
fn eviction_prefers_an_unverified_victim_over_a_verified_one() {
    let mut cache = CoordCache::new(2, 1_000_000);
    let verified = make_node_addr(1);
    let hint = make_node_addr(2);
    let newcomer = make_node_addr(3);

    // The verified entry is the least recently used, so an unrestricted
    // LRU would take it. That is exactly the eviction an attacker would
    // drive to manufacture an empty slot.
    cache.insert_verified(verified, make_coords(&[1, 0]), 0);
    assert_eq!(
        cache.insert(hint, make_coords(&[2, 0]), 100),
        HintOutcome::Inserted
    );
    assert_eq!(
        cache.insert(newcomer, make_coords(&[3, 0]), 200),
        HintOutcome::Inserted
    );

    assert!(
        cache.contains(&verified, 200),
        "the verified entry must not be the eviction victim"
    );
    assert!(
        !cache.contains(&hint, 200),
        "the unverified entry should have been evicted instead"
    );
}

#[test]
fn a_cache_full_of_verified_entries_refuses_a_hint_rather_than_evicting_one() {
    let mut cache = CoordCache::new(2, 1_000_000);
    cache.insert_verified(make_node_addr(1), make_coords(&[1, 0]), 0);
    cache.insert_verified(make_node_addr(2), make_coords(&[2, 0]), 0);

    assert_eq!(
        cache.insert(make_node_addr(3), make_coords(&[3, 0]), 10),
        HintOutcome::Rejected
    );
    assert!(cache.contains(&make_node_addr(1), 10));
    assert!(cache.contains(&make_node_addr(2), 10));
}

#[test]
fn demote_keeps_the_value_and_lets_a_hint_replace_it() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);
    let real = make_coords(&[1, 0]);
    cache.insert_verified_with_path_mtu(addr, real.clone(), 10, 1400);
    assert_eq!(
        cache.insert(addr, make_coords(&[1, 2, 0]), 10),
        HintOutcome::Rejected
    );

    assert!(cache.demote(&addr));

    let entry = cache.get_entry(&addr).unwrap();
    assert_eq!(entry.coords(), &real);
    assert_eq!(entry.source(), crate::cache::CoordSource::Hint);
    assert!(!entry.is_verified(10));
    assert_eq!(
        entry.path_mtu(),
        Some(1400),
        "demote leaves the path MTU to its caller"
    );
    assert_eq!(
        cache.insert(addr, make_coords(&[1, 2, 0]), 11),
        HintOutcome::Changed
    );
}

#[test]
fn demote_of_an_absent_entry_reports_none() {
    let mut cache = CoordCache::new(100, 1000);
    assert!(!cache.demote(&make_node_addr(1)));
    assert!(cache.is_empty());
}

#[test]
fn clear_path_mtu_keeps_the_entry() {
    let mut cache = CoordCache::new(100, 1000);
    let addr = make_node_addr(1);
    cache.insert_verified_with_path_mtu(addr, make_coords(&[1, 0]), 10, 1400);

    cache.clear_path_mtu(&addr);

    let entry = cache.get_entry(&addr).unwrap();
    assert_eq!(entry.path_mtu(), None);
    assert!(entry.is_verified(10));
}
