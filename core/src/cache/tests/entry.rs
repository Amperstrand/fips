use super::*;
use crate::identity::NodeAddr;

fn make_node_addr(val: u8) -> NodeAddr {
    let mut bytes = [0u8; 16];
    bytes[0] = val;
    NodeAddr::from_bytes(bytes)
}

fn make_coords(ids: &[u8]) -> TreeCoordinate {
    TreeCoordinate::from_addrs(ids.iter().map(|&v| make_node_addr(v)).collect()).unwrap()
}

#[test]
fn test_cache_entry_expiry() {
    let coords = make_coords(&[1, 0]);
    let entry = CacheEntry::new(coords, 1000, 500);

    assert!(!entry.is_expired(1000));
    assert!(!entry.is_expired(1500)); // expires_at = 1500, not yet expired
    assert!(entry.is_expired(1501)); // one ms after expiry
    assert!(entry.is_expired(2000));
}

#[test]
fn test_cache_entry_refresh() {
    let coords = make_coords(&[1, 0]);
    let mut entry = CacheEntry::new(coords, 1000, 500);

    assert!(entry.is_expired(1501)); // expires_at = 1500

    entry.refresh(1400, 500); // new expires_at = 1900

    assert!(!entry.is_expired(1600));
    assert!(!entry.is_expired(1900)); // at exactly expiry, not expired
    assert!(entry.is_expired(1901)); // one ms after expiry
}

#[test]
fn test_cache_entry_times() {
    let coords = make_coords(&[1, 0]);
    let entry = CacheEntry::new(coords, 1000, 500);

    assert_eq!(entry.created_at(), 1000);
    assert_eq!(entry.last_used(), 1000);
    assert_eq!(entry.expires_at(), 1500);
    assert_eq!(entry.age(1200), 200);
    assert_eq!(entry.idle_time(1200), 200);
    assert_eq!(entry.time_to_expiry(1200), 300);
    assert_eq!(entry.time_to_expiry(1600), 0);
}

#[test]
fn test_cache_entry_touch() {
    let coords = make_coords(&[1, 0]);
    let mut entry = CacheEntry::new(coords, 1000, 500);

    assert_eq!(entry.last_used(), 1000);
    entry.touch(1300);
    assert_eq!(entry.last_used(), 1300);
    // Touch doesn't affect expiry
    assert_eq!(entry.expires_at(), 1500);
}

#[test]
fn test_cache_entry_update() {
    let mut entry = CacheEntry::new(make_coords(&[1, 0]), 1000, 500);

    let new_coords = make_coords(&[1, 2, 0]);
    entry.update(new_coords.clone(), 2000, 600);

    assert_eq!(entry.coords(), &new_coords);
    assert_eq!(entry.last_used(), 2000);
    assert_eq!(entry.expires_at(), 2600);
    // created_at is unchanged
    assert_eq!(entry.created_at(), 1000);
}

#[test]
fn test_cache_entry_saturating_add() {
    let coords = make_coords(&[1, 0]);
    let entry = CacheEntry::new(coords, u64::MAX - 10, 100);

    // Should saturate to u64::MAX rather than overflow
    assert_eq!(entry.expires_at(), u64::MAX);
}
