//! Capacity policy for the dedup cache.
//!
//! These live beside the policy rather than in `lookup/tests/core.rs`
//! because they are the regression tests for a security finding and read
//! directly against `make_room`'s two branches.

use super::super::limits::{LookupBackoff, LookupForwardRateLimiter};
use super::super::state::MIN_RECENT_PER_PEER;
use super::*;
use crate::proto::coord::TreeCoordinate;
use crate::testutil::make_node_addr;

/// The cache bound used by the behavioural tests. Smaller than the
/// production 4096 so a saturation test stays cheap; the policy is a
/// function of the bound, not of its value.
const CACHE: usize = 128;

fn empty() -> Lookup {
    Lookup::new(
        LookupBackoff::default(),
        LookupForwardRateLimiter::default(),
    )
}

fn request(request_id: u64, target: NodeAddr) -> LookupRequest {
    let origin = make_node_addr(0xCC);
    LookupRequest::new(
        request_id,
        target,
        origin,
        TreeCoordinate::root(origin),
        5,
        0,
    )
}

/// Deliver one transit request from `from`, discarding the route
/// decision: these tests are about which entries survive, and the
/// forward limiter's verdict does not affect what is cached.
fn deliver(
    lookup: &mut Lookup,
    request_id: u64,
    from: &NodeAddr,
    my_addr: &NodeAddr,
    peer_count: usize,
) -> Classification {
    let target = make_node_addr(0xBB);
    classify_request(
        lookup,
        &request(request_id, target),
        from,
        my_addr,
        1_000,
        60_000,
        CACHE,
        peer_count,
    )
}

#[test]
fn a_peer_over_its_share_evicts_its_own_oldest_and_not_a_light_peers() {
    let mut lookup = empty();
    let heavy = make_node_addr(0x01);
    let light = make_node_addr(0x02);
    let me = make_node_addr(0x99);
    // 64 peers over a 128-entry cache: 128/64 = 2, floored to 64.
    let peer_count = 64;
    let share = Lookup::peer_share(CACHE, peer_count);

    deliver(&mut lookup, 7, &light, &me, peer_count);

    // One request past the share, so the heavy peer pays for its own
    // admission rather than the cache paying for it.
    for i in 0..=share as u64 {
        deliver(&mut lookup, 1_000 + i, &heavy, &me, peer_count);
    }

    assert!(
        lookup.recent_requests.contains_key(&7),
        "a light peer's reverse-path entry must survive a neighbour's flood"
    );
    assert!(
        !lookup.recent_requests.contains_key(&1_000),
        "the flooder's own oldest entry is what pays for its newest"
    );
    assert!(
        lookup.recent_requests.contains_key(&(1_000 + share as u64)),
        "and its newest is admitted rather than dropped"
    );
    assert_eq!(
        lookup.peer_entries(&heavy),
        share,
        "the flooder is held at its share"
    );
}

#[test]
fn the_per_peer_share_never_falls_below_the_floor_however_many_peers() {
    // A node with as many peers as cache entries would otherwise give
    // each peer a share of one, which no genuine transit burst survives.
    assert_eq!(Lookup::peer_share(CACHE, CACHE), MIN_RECENT_PER_PEER);
    assert_eq!(Lookup::peer_share(CACHE, usize::MAX), MIN_RECENT_PER_PEER);
    // Above the floor the share still tracks the peer count.
    assert_eq!(Lookup::peer_share(4096, 8), 512);
    // And a zero peer count (no links up yet) must not divide by zero.
    assert_eq!(Lookup::peer_share(CACHE, 0), CACHE);

    // Behaviourally: at the floor, entry number 64 costs the peer
    // nothing and entry number 65 costs it its oldest.
    let mut lookup = empty();
    let peer = make_node_addr(0x01);
    let me = make_node_addr(0x99);
    let peer_count = usize::MAX;
    for i in 0..MIN_RECENT_PER_PEER as u64 {
        let evicted = deliver(&mut lookup, i, &peer, &me, peer_count).evicted;
        assert!(evicted.is_none(), "nothing is evicted below the floor");
    }
    let evicted = deliver(&mut lookup, 999, &peer, &me, peer_count)
        .evicted
        .expect("the entry past the floor must evict");
    assert_eq!(evicted.request_id, 0, "the peer's oldest entry pays");
    assert_eq!(evicted.peer, peer);
    assert_eq!(evicted.share, MIN_RECENT_PER_PEER);
}

#[test]
fn a_node_whose_dedup_cache_is_saturated_still_answers_a_lookup_for_itself() {
    // The availability claim, and the actual finding: the capacity check
    // used to sit ahead of the check for whether the request names us,
    // so a peer holding the cache full made this node unresolvable.
    let mut lookup = empty();
    let flooder = make_node_addr(0x01);
    let other = make_node_addr(0x02);
    let me = make_node_addr(0x99);
    // A single link peer, so its share is the whole cache and it can
    // saturate without evicting itself.
    let peer_count = 1;

    for i in 0..CACHE as u64 {
        deliver(&mut lookup, i, &flooder, &me, peer_count);
    }
    assert_eq!(
        lookup.recent_requests.len(),
        CACHE,
        "precondition: the cache is full, or the rest observes nothing"
    );

    let classification = classify_request(
        &mut lookup,
        &request(u64::MAX, me),
        &other,
        &me,
        1_000,
        60_000,
        CACHE,
        peer_count,
    );

    assert!(
        matches!(classification.outcome, RequestOutcome::RespondAsTarget),
        "a saturated cache must not stop the node answering lookups for itself"
    );
    let evicted = classification
        .evicted
        .expect("room must have been made at capacity");
    assert_eq!(
        evicted.peer, flooder,
        "the peer holding the most entries pays, not the arriving one"
    );
    assert_eq!(evicted.request_id, 0, "and it pays with its oldest");
    assert!(
        lookup.recent_requests.contains_key(&u64::MAX),
        "the arriving request is recorded, so its response can be routed back"
    );
    assert_eq!(
        lookup.recent_requests.len(),
        CACHE,
        "the cache stays at its bound"
    );
}

#[test]
fn a_duplicate_is_not_indexed_twice_and_evicts_nothing() {
    // The index is a second container over the same entries, so the
    // maintenance risk is drift: everything the policy decides reads it.
    let mut lookup = empty();
    let peer = make_node_addr(0x01);
    let me = make_node_addr(0x99);

    deliver(&mut lookup, 42, &peer, &me, 1);
    let repeat = deliver(&mut lookup, 42, &peer, &me, 1);

    assert!(matches!(repeat.outcome, RequestOutcome::Duplicate));
    assert!(repeat.evicted.is_none(), "a duplicate makes no room");
    assert_eq!(lookup.peer_entries(&peer), 1);
    assert_eq!(lookup.recent_requests.len(), 1);
}

#[test]
fn purging_expired_entries_leaves_the_index_level_with_the_cache() {
    let mut lookup = empty();
    let a = make_node_addr(0x01);
    let b = make_node_addr(0x02);
    let me = make_node_addr(0x99);

    for i in 0..5u64 {
        deliver(&mut lookup, i, &a, &me, 2);
    }
    for i in 100..103u64 {
        deliver(&mut lookup, i, &b, &me, 2);
    }
    let indexed: usize = lookup.recent_by_peer.values().map(|ids| ids.len()).sum();
    assert_eq!(
        indexed,
        lookup.recent_requests.len(),
        "every cached request is indexed exactly once"
    );

    // Entries were stamped at 1_000 with a 60s window; age them out.
    lookup.purge_recent(1_000 + 60_000 + 1, 60_000);
    assert!(lookup.recent_requests.is_empty());
    assert!(
        lookup.recent_by_peer.is_empty(),
        "the index must not keep entries the cache no longer holds"
    );
}

/// Deliver one transit request from `from` at `now_ms`, with a share at
/// the floor (one peer per floor's worth of cache) and a long window.
fn deliver_at(
    lookup: &mut Lookup,
    request_id: u64,
    from: &NodeAddr,
    now_ms: u64,
) -> Classification {
    classify_request(
        lookup,
        &request(request_id, make_node_addr(0xBB)),
        from,
        &make_node_addr(0x99),
        now_ms,
        600_000,
        CACHE,
        usize::MAX,
    )
}

#[test]
fn an_eviction_carries_the_evicted_entrys_age_and_whether_it_was_answered() {
    let mut lookup = empty();
    let peer = make_node_addr(0x01);
    // Request 0 arrives at 1_000, the rest of the floor later.
    deliver_at(&mut lookup, 0, &peer, 1_000);
    for i in 1..MIN_RECENT_PER_PEER as u64 {
        deliver_at(&mut lookup, i, &peer, 2_000);
    }
    lookup
        .recent_requests
        .get_mut(&1)
        .unwrap()
        .response_forwarded = true;

    let first = deliver_at(&mut lookup, 500, &peer, 5_500)
        .evicted
        .expect("the entry past the floor evicts");
    assert_eq!(first.request_id, 0);
    assert_eq!(first.age_ms, Some(4_500), "arrival to eviction");
    assert_eq!(first.answered, Some(false));

    let second = deliver_at(&mut lookup, 501, &peer, 7_000)
        .evicted
        .expect("and the next one evicts again");
    assert_eq!(second.request_id, 1);
    assert_eq!(second.age_ms, Some(5_000));
    assert_eq!(second.answered, Some(true), "a response went back on it");
}

#[test]
fn evicted_ids_are_remembered_up_to_the_ring_size_and_the_oldest_forgotten() {
    use super::super::state::EVICTED_KEPT;
    let mut lookup = empty();
    let peer = make_node_addr(0x01);
    let floor = MIN_RECENT_PER_PEER as u64;
    // Fill the floor, then evict one entry per further arrival.
    for i in 0..floor + EVICTED_KEPT as u64 + 1 {
        deliver_at(&mut lookup, i, &peer, 1_000);
    }
    // Ids 0 to EVICTED_KEPT were evicted; the ring holds the last
    // EVICTED_KEPT of them.
    assert!(!lookup.was_evicted(0), "the oldest eviction is forgotten");
    assert!(lookup.was_evicted(1));
    assert!(lookup.was_evicted(EVICTED_KEPT as u64));
    assert!(
        !lookup.was_evicted(EVICTED_KEPT as u64 + 1),
        "an id still cached was not evicted"
    );
}

#[test]
fn an_expiry_purge_is_not_recorded_as_an_eviction() {
    let mut lookup = empty();
    let peer = make_node_addr(0x01);
    deliver_at(&mut lookup, 7, &peer, 1_000);
    lookup.purge_recent(1_000 + 600_001, 600_000);
    assert!(
        lookup.recent_requests.is_empty(),
        "setup: request 7 expired"
    );
    assert!(!lookup.was_evicted(7));
}
