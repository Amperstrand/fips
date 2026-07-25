//! Rekey storm regression tests for issue #103.
//!
//! Two bugs caused a rekey "storm" — tight retry loops that congested the
//! drain queue and blocked all outbound traffic including heartbeats:
//!
//! 1. **Hammering**: `initiate_rekey()` freed the session index and returned
//!    early on `transport.send()` failure WITHOUT storing rekey state on the
//!    peer. This left `rekey_in_progress()` false, so `check_rekey()` would
//!    re-trigger `initiate_rekey()` every tick (1s), permanently congesting
//!    the drain queue with fresh Noise handshakes.
//!
//! 2. **Timer stall**: `resend_pending_rekeys()` only advanced the resend
//!    timer when `transport.send()` succeeded. On failure, the timer never
//!    advanced, so `needs_msg1_resend()` returned true every tick, creating
//!    a tight retry loop with no backoff.
//!
//! These tests simulate transport send failure by removing the peer's entry
//! from the loopback registry, then verify the rekey state machine handles
//! the failure correctly.

use super::spanning_tree::*;
use super::*;

/// Remove a peer's loopback receiver so all sends to it fail with
/// `TransportError::SendFailed("no loopback route to ...")`.
fn break_route_to(addr: &TransportAddr) {
    super::spanning_tree::LOOPBACK_REGISTRY
        .lock()
        .unwrap()
        .remove(addr);
}

/// Set `rekey.after_secs` and `rekey.after_messages` on an already-constructed
/// node via the copy-on-write context swap (same pattern as
/// `set_link_dead_timeout` in `heartbeat.rs`).
fn set_rekey_config(node: &mut Node, after_secs: u64, after_messages: u64) {
    node.replace_context(|ctx| {
        let mut cfg = (*ctx.config).clone();
        cfg.node.rekey.after_secs = after_secs;
        cfg.node.rekey.after_messages = after_messages;
        ctx.config = std::sync::Arc::new(cfg);
    });
}

/// Arm a real (initiator) FMP rekey on the peer the given node holds for
/// `peer_addr`, so the msg1 resend budget can be exercised.
fn arm_rekey(node: &mut Node, peer_addr: &NodeAddr) {
    let remote = Identity::generate();
    let local = Identity::generate();
    let hs = crate::noise::HandshakeState::new_initiator(local.keypair(), remote.pubkey_full());
    let peer = node.get_peer_mut(peer_addr).expect("peer present");
    peer.set_rekey_state(hs, SessionIndex::new(7), vec![0xAB; 64], 0);
}

// ============================================================================
// Bug 1: Rekey state not stored on send failure (hammering)
// ============================================================================

/// When `transport.send()` fails during `initiate_rekey`, the rekey state
/// MUST still be stored on the peer so `rekey_in_progress()` returns true.
/// Without this, `check_rekey()` re-triggers `initiate_rekey()` every tick.
#[tokio::test]
async fn rekey_state_stored_on_send_failure() {
    let mut nodes = run_tree_test(2, &[(0, 1)], false).await;
    verify_tree_convergence(&nodes);

    let addr_1 = nodes[1].addr.clone();
    assert!(nodes[0].node.get_peer(&nodes[1].node.node_addr()).is_some());

    // Break the route so all sends fail.
    break_route_to(&addr_1);

    // Force the rekey trigger: after_secs=0 means the time threshold is
    // always met (modulo jitter, which can add at most REKEY_JITTER_SECS).
    // after_messages=0 means the counter threshold is also always met.
    set_rekey_config(&mut nodes[0].node, 0, 0);

    nodes[0].node.check_rekey().await;

    let peer_addr = *nodes[1].node.node_addr();
    let peer = nodes[0]
        .node
        .get_peer(&peer_addr)
        .expect("peer should still exist");

    assert!(
        peer.rekey_in_progress(),
        "rekey state must be stored even when transport.send fails — \
         without this, check_rekey re-triggers every tick (issue #103)"
    );

    cleanup_nodes(&mut nodes).await;
}

/// Calling `check_rekey()` again must NOT re-trigger `initiate_rekey()` when
/// a rekey is already in progress. This is the direct regression for the
/// hammering bug: without the state-storing fix, every tick would allocate
/// a new session index and send a fresh msg1.
#[tokio::test]
async fn rekey_not_retriggered_when_in_progress() {
    let mut nodes = run_tree_test(2, &[(0, 1)], false).await;
    verify_tree_convergence(&nodes);

    let addr_1 = nodes[1].addr.clone();
    let peer_addr = *nodes[1].node.node_addr();

    break_route_to(&addr_1);
    set_rekey_config(&mut nodes[0].node, 0, 0);

    // First trigger — should initiate rekey and store state.
    nodes[0].node.check_rekey().await;

    let peer = nodes[0].node.get_peer(&peer_addr).unwrap();
    assert!(peer.rekey_in_progress(), "rekey should be in progress after first check");

    // Record the initial msg1 payload so we can detect a second initiation.
    let initial_msg1 = peer
        .rekey_msg1()
        .expect("msg1 should be stored")
        .to_vec();

    // Second trigger — must NOT re-initiate because rekey is already in flight.
    nodes[0].node.check_rekey().await;

    let peer = nodes[0].node.get_peer(&peer_addr).unwrap();
    assert!(
        peer.rekey_in_progress(),
        "rekey should still be in progress"
    );

    // The msg1 payload should be IDENTICAL — no second initiation happened.
    let current_msg1 = peer
        .rekey_msg1()
        .expect("msg1 should still be stored")
        .to_vec();
    assert_eq!(
        current_msg1, initial_msg1,
        "msg1 must not change on second check_rekey — if it did, a second \
         initiate_rekey fired despite an in-flight rekey (issue #103 hammering)"
    );

    cleanup_nodes(&mut nodes).await;
}

// ============================================================================
// Bug 2: Resend timer not advancing on send failure (timer stall)
// ============================================================================

/// When `transport.send()` fails during `resend_pending_rekeys`, the resend
/// timer MUST still advance so the next attempt uses the exponential backoff
/// schedule. Without this, `needs_msg1_resend()` returns true every tick,
/// creating a tight retry loop.
#[tokio::test]
async fn resend_timer_advances_on_send_failure() {
    let mut nodes = run_tree_test(2, &[(0, 1)], false).await;
    verify_tree_convergence(&nodes);

    let addr_1 = nodes[1].addr.clone();
    let peer_addr = *nodes[1].node.node_addr();

    // Break the route so all resends fail.
    break_route_to(&addr_1);

    // Arm a rekey with resend deadline at 0 (immediately due).
    arm_rekey(&mut nodes[0].node, &peer_addr);

    let peer = nodes[0].node.get_peer(&peer_addr).unwrap();
    assert!(peer.rekey_in_progress());
    assert_eq!(peer.rekey_msg1_resend_count(), 0, "initial resend count should be 0");

    // Call resend with now_ms=1 (past the deadline of 0).
    nodes[0].node.resend_pending_rekeys(1).await;

    let peer = nodes[0].node.get_peer(&peer_addr).unwrap();
    assert!(
        peer.rekey_msg1_resend_count() >= 1,
        "resend count must advance even when send fails — \
         without this, needs_msg1_resend() returns true every tick \
         creating a tight retry loop (issue #103 timer stall). \
         Got count: {}",
        peer.rekey_msg1_resend_count()
    );

    cleanup_nodes(&mut nodes).await;
}
