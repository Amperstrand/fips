//! Handshake handlers and connection promotion.

use crate::NodeAddr;
use crate::PeerIdentity;
use crate::node::acl::PeerAclContext;
use crate::node::rate_limit::Msg1Class;
use crate::node::reject::{HandshakeReject, RejectReason};
use crate::node::wire::{Msg1Header, Msg2Header, build_msg2};
use crate::node::{Node, NodeError};
use crate::peer::{ActivePeer, PeerConnection, PromotionResult, cross_connection_winner};
use crate::transport::{Link, LinkDirection, LinkId, ReceivedPacket};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Minimum interval between accepted epoch changes for one peer identity,
/// and the recency threshold at which the peering an epoch change would
/// destroy still counts as live.
///
/// An epoch-mismatch msg1 is authentic but replayable: a captured one stays
/// valid indefinitely, and accepting it tears down a working peering. Both
/// conditions are receiver-local. The liveness half is the one that closes
/// the replay, since a peering under attack is by construction still
/// heartbeating; the interval half bounds the churn a peer can drive on its
/// own.
///
/// Sized against the peer's own recovery rather than against a round number:
/// a genuinely restarting peer's msg1 resends fire at roughly t+1, t+3, t+7
/// and t+15 seconds and its attempt is reaped at `handshake_timeout_secs`
/// (30), so 15 is the largest value at which a real restart still re-peers
/// inside its first handshake window with no reconnect backoff. It also sits
/// below `link_dead_timeout_secs` (30), so the liveness gate can never
/// outlive the reaper that would have removed the peering anyway.
///
/// Raising it lengthens the outage an attacker's accepted replay causes,
/// because the genuine peer's recovery msg1 hits the same arm. Lowering it
/// weakens both halves and, below the resend ladder, buys nothing.
const EPOCH_RESTART_MIN_INTERVAL_SECS: u64 = 15;

/// Why an inbound msg1 got past the `accept_connections` gate, and against
/// what identity the post-DH confirmation must check it.
///
/// Three outcomes, not two: an `Option` would conflate "no waiver was needed"
/// with "the waiver was used and nobody owns the matched address", and the
/// second of those is the case that must reject.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::node) enum Msg1Waiver {
    /// The transport accepts fresh inbound handshakes (or no transport is
    /// registered), so the address carve-out did not admit this msg1 and
    /// there is nothing to confirm.
    NotNeeded,
    /// The carve-out is what admitted this msg1, and the matched address
    /// belongs to this identity: either a promoted peer, or a handshake
    /// already in flight on the matched link whose identity is expected
    /// (outbound dial) or already learned (inbound msg1).
    Expect(NodeAddr),
    /// The carve-out is what admitted this msg1, and no identity can be
    /// attributed to the matched address. Fail closed: reject after the DH.
    Unattributed,
}

impl Node {
    /// Returns true if an inbound msg1's source matches an established
    /// link, i.e. it is rekey/restart maintenance traffic rather than a
    /// stranger's fresh handshake.
    ///
    /// This is deliberately separate from the `accept_connections` gate:
    /// it is the only half of `should_admit_msg1` that is a safe basis
    /// for exempting traffic from stranger-class treatment. Two
    /// predicates cover "established peer at this transport+addr":
    ///
    /// 1. `addr_to_link` has an entry for `(transport_id, remote_addr)`.
    ///    This is the fast path and matches when the peer registered with
    ///    the same `TransportAddr` form we observe on inbound packets
    ///    (e.g., both numeric when peer config uses a numeric IP).
    ///
    /// 2. An active peer's `current_addr()` matches `(transport_id,
    ///    remote_addr)`. `current_addr` is updated from inbound encrypted-
    ///    frame source addrs (always numeric `SocketAddr`-form), so this
    ///    catches established peers whose `addr_to_link` key is hostname-
    ///    form (because `initiate_connection` populated it from a
    ///    hostname-bearing peer config) while inbound rekey msg1 arrives
    ///    in numeric form. Without this second predicate, the carve-out
    ///    misses any deployment that combines a hostname-based peer config
    ///    with `udp.accept_connections: false` or `udp.outbound_only: true`
    ///    (the production trigger for the 2026-04-30 bug).
    ///
    /// Cost: predicate 1 is O(1), predicate 2 is O(peers). Because
    /// `handle_msg1` classifies before rate limiting, predicate 2 runs on
    /// every inbound msg1 including those about to be refused, so a msg1
    /// flood costs O(peers) per dropped packet rather than O(1). Predicate 2
    /// exists only because `addr_to_link` is keyed on the *unresolved* dial
    /// address; if that keying is corrected, this becomes a single O(1)
    /// lookup and the flood cost returns to O(1).
    pub(in crate::node) fn is_established_link_msg1(
        &self,
        transport_id: crate::transport::TransportId,
        remote_addr: &crate::transport::TransportAddr,
    ) -> bool {
        if self
            .addr_to_link
            .contains_key(&(transport_id, remote_addr.clone()))
        {
            return true;
        }
        if self.peers.values().any(|p| {
            p.transport_id() == Some(transport_id) && p.current_addr() == Some(remote_addr)
        }) {
            return true;
        }
        false
    }

    /// Classify the msg1 waiver for a source that `should_admit_msg1`
    /// admitted, so the post-DH confirmation knows whether it has an
    /// identity to check against and what to do when it has none.
    ///
    /// `established` is the caller's already-computed
    /// `is_established_link_msg1(...)`, so the O(peers) scan is not repeated
    /// on the refusal path.
    ///
    /// The two attribution limbs are composed the same way
    /// `is_established_link_msg1` composes its own: as an OR, not as an
    /// if/else. An `addr_to_link` entry that yields no identity must not
    /// short-circuit the address scan, because the two keys can be different
    /// forms of the same peer's address (the hostname-vs-numeric case that
    /// predicate 2 exists for) and the entry can outlive the link it named.
    pub(in crate::node) fn msg1_waiver(
        &self,
        established: bool,
        transport_id: crate::transport::TransportId,
        remote_addr: &crate::transport::TransportAddr,
    ) -> Msg1Waiver {
        // The carve-out only admits anything when the gate would otherwise
        // refuse, so an accepting transport has nothing to confirm.
        if self
            .transports
            .get(&transport_id)
            .is_none_or(|t| t.accept_connections())
        {
            return Msg1Waiver::NotNeeded;
        }
        if !established {
            // `should_admit_msg1` refused this msg1 and the caller returned,
            // so this arm is unreachable from the one call site. Fail closed
            // rather than skipping the check, so a second caller cannot
            // reintroduce the hole this classifier exists to close.
            return Msg1Waiver::Unattributed;
        }

        // Predicate 1: the reverse-address lookup.
        if let Some(&link_id) = self.addr_to_link.get(&(transport_id, remote_addr.clone())) {
            if let Some(peer) = self.peers.values().find(|p| p.link_id() == link_id) {
                return Msg1Waiver::Expect(*peer.node_addr());
            }
            // A link with no promoted peer: a dial in progress or an inbound
            // handshake in flight. Both register a connection carrying the
            // expected (outbound) or learned (inbound) identity.
            if let Some(id) = self
                .connections
                .get(&link_id)
                .and_then(|c| c.expected_identity())
            {
                return Msg1Waiver::Expect(*id.node_addr());
            }
            // Deliberately fall through instead of returning. The entry can
            // name a link that no longer exists — `remove_link` clears the
            // reverse lookup only under the key it rebuilds from the link's
            // own remote address, so an entry inserted under a second
            // address form for that link survives its removal. Rejecting
            // here would refuse a peer predicate 2 can still attribute, and
            // would refuse it permanently: this classifier's caller returns
            // above the insert that overwrites the stale entry, so nothing
            // downstream would ever repair the map.
        }

        // Predicate 2: the address scan over promoted peers, which always
        // yields an identity when it matches.
        self.peers
            .values()
            .find(|p| {
                p.transport_id() == Some(transport_id) && p.current_addr() == Some(remote_addr)
            })
            .map(|p| Msg1Waiver::Expect(*p.node_addr()))
            .unwrap_or(Msg1Waiver::Unattributed)
    }

    /// Returns true if an inbound msg1 should be admitted past the
    /// `accept_connections` gate.
    ///
    /// Rekey/restart msg1 from an established peer is always admitted (the
    /// gate is meant to filter fresh handshakes from strangers, not
    /// maintenance traffic on established sessions).
    ///
    /// Otherwise the transport's `accept_connections` config decides;
    /// absence of a registered transport admits (no gate to apply).
    pub(in crate::node) fn should_admit_msg1(
        &self,
        transport_id: crate::transport::TransportId,
        remote_addr: &crate::transport::TransportAddr,
    ) -> bool {
        if self.is_established_link_msg1(transport_id, remote_addr) {
            return true;
        }
        self.transports
            .get(&transport_id)
            .is_none_or(|t| t.accept_connections())
    }

    /// Handle handshake message 1 (phase 0x1).
    ///
    /// This creates a new inbound connection. Rate limiting is applied
    /// before any expensive crypto operations.
    ///
    /// Classifying the source costs no crypto (two map/scan lookups), so it
    /// happens first and selects which bucket the msg1 draws on: rekey and
    /// restart traffic from an established link stops competing with
    /// stranger admission, while still being metered.
    pub(in crate::node) async fn handle_msg1(&mut self, packet: ReceivedPacket) {
        // === CLASSIFY, THEN RATE LIMIT (both before any crypto) ===
        // Classification is two map lookups; the second is O(peers) and now
        // runs on every inbound msg1, including refused ones. See the
        // `is_established_link_msg1` doc comment for why the scan is still
        // needed and what would retire it.
        let established = self.is_established_link_msg1(packet.transport_id, &packet.remote_addr);
        let class = if established {
            Msg1Class::EstablishedLink
        } else {
            Msg1Class::Stranger
        };
        let _slot = match self.msg1_rate_limiter.start_handshake(class) {
            Ok(slot) => slot,
            Err(reason) => {
                debug!(
                    transport_id = %packet.transport_id,
                    remote_addr = %packet.remote_addr,
                    refused_by = %reason,
                    "Msg1 rate limited"
                );
                return;
            }
        };

        // accept_connections gate. Rekey/restart msg1 on an existing link
        // is always admitted; the gate only filters truly-fresh connections
        // from strangers. Without this carve-out, the dual-init tie-breaker
        // deadlocks when the larger-NodeAddr side has accept_connections=false.
        //
        // `!established &&` is not a behaviour change: `should_admit_msg1`
        // is `is_established_link_msg1() || accept_connections()`, so the
        // short-circuit only skips a second evaluation of the `peers` scan
        // on the hot path. The call is left in place so the two predicates
        // cannot drift apart.
        if !established && !self.should_admit_msg1(packet.transport_id, &packet.remote_addr) {
            self.stats_mut()
                .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
            return;
        }

        // Snapshot which identity, if any, the address carve-out attributed
        // this source to. Taken here rather than after the DH so the answer
        // is the one the gate acted on. On an accepting transport this is one
        // map lookup and a return.
        let waiver = self.msg1_waiver(established, packet.transport_id, &packet.remote_addr);

        // Parse header
        let header = match Msg1Header::parse(&packet.data) {
            Some(h) => h,
            None => {
                debug!("Invalid msg1 header");
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                return;
            }
        };

        // Check for existing connection from this address.
        //
        // If we already have an *inbound* link from this address, this could be:
        // 1. A duplicate msg1 (our msg2 was lost) — resend msg2
        // 2. A restarted peer (different epoch) — tear down and reprocess
        //
        // If we have an *outbound* link to this address (we initiated to them
        // AND they initiated to us), this is a cross-connection — allow it.
        //
        // Epoch-based restart detection: if the sender already has an inbound
        // link AND is an active peer in self.peers, fall through to decrypt
        // the msg1 and check the epoch. Otherwise, treat as duplicate.
        let addr_key = (packet.transport_id, packet.remote_addr.clone());
        let mut possible_restart = false;
        if let Some(&existing_link_id) = self.addr_to_link.get(&addr_key)
            && let Some(link) = self.links.get(&existing_link_id)
        {
            if link.direction() == LinkDirection::Inbound {
                // Check if this link belongs to an already-promoted active peer
                let is_active_peer = self.peers.values().any(|p| p.link_id() == existing_link_id);

                if is_active_peer {
                    // Possible restart — fall through to decrypt and check epoch
                    possible_restart = true;
                } else {
                    // Genuinely pending handshake — resend msg2
                    let msg2_bytes = self.find_stored_msg2(existing_link_id);
                    if let Some(msg2) = msg2_bytes {
                        if let Some(transport) = self.transports.get(&packet.transport_id) {
                            match transport.send(&packet.remote_addr, &msg2).await {
                                Ok(_) => debug!(
                                    remote_addr = %packet.remote_addr,
                                    "Resent msg2 for duplicate msg1"
                                ),
                                Err(e) => debug!(
                                    remote_addr = %packet.remote_addr,
                                    error = %e,
                                    "Failed to resend msg2"
                                ),
                            }
                        }
                    } else {
                        debug!(
                            remote_addr = %packet.remote_addr,
                            "Duplicate msg1 but no stored msg2 to resend"
                        );
                        self.stats_mut().record_reject(RejectReason::Handshake(
                            HandshakeReject::UnknownConnection,
                        ));
                    }
                    return;
                }
            } else {
                // Outbound link to this address. If it belongs to an active
                // peer, this may be a rekey msg1 (same epoch) or a
                // restart (different epoch). Set possible_restart to enable
                // the epoch/rekey check below.
                let is_active_peer = self.peers.values().any(|p| p.link_id() == existing_link_id);
                if is_active_peer {
                    possible_restart = true;
                } else {
                    debug!(
                        transport_id = %packet.transport_id,
                        remote_addr = %packet.remote_addr,
                        existing_link_id = %existing_link_id,
                        "Cross-connection detected: have outbound, received inbound msg1"
                    );
                }
            }
        }

        // === CRYPTO COST PAID HERE ===
        let link_id = self.allocate_link_id();
        let mut conn = PeerConnection::inbound_with_transport(
            link_id,
            packet.transport_id,
            packet.remote_addr.clone(),
            packet.timestamp_ms,
        );

        // This frame's own copy of the node's long-term private key; the
        // handshake state keeps its own and clears that on drop.
        let mut our_keypair = self.identity().keypair();
        let noise_msg1 = &packet.data[header.noise_msg1_offset..];
        let init_result = conn.receive_handshake_init(
            our_keypair,
            self.startup_epoch(),
            noise_msg1,
            packet.timestamp_ms,
        );
        our_keypair.non_secure_erase();
        let msg2_response = match init_result {
            Ok(m) => m,
            Err(e) => {
                debug!(
                    error = %e,
                    "Failed to process msg1"
                );
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                return;
            }
        };

        // Learn peer identity from msg1
        let peer_identity = match conn.expected_identity() {
            Some(id) => *id,
            None => {
                warn!("Identity not learned from msg1");
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                return;
            }
        };

        let peer_node_addr = *peer_identity.node_addr();

        // The address carve-out admitted this msg1 past a refusing gate on
        // the strength of the source address alone. Now that the DH has
        // revealed the initiator's static, confirm it belongs to the party
        // that address is attributed to; an off-path party sourcing from an
        // established peer's address gets no further than here. Cheap
        // rejection is unchanged: a stranger under accept_connections=false
        // is still refused above, having paid nothing.
        match waiver {
            Msg1Waiver::NotNeeded => {}
            Msg1Waiver::Expect(expected) if expected == peer_node_addr => {}
            Msg1Waiver::Expect(expected) => {
                warn!(
                    expected = %self.peer_display_name(&expected),
                    actual = %self.peer_display_name(&peer_node_addr),
                    transport_id = %packet.transport_id,
                    "Msg1 admitted by the established-address waiver carries a different identity, dropping"
                );
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                return;
            }
            Msg1Waiver::Unattributed => {
                warn!(
                    actual = %self.peer_display_name(&peer_node_addr),
                    transport_id = %packet.transport_id,
                    "Msg1 admitted by the established-address waiver, but no identity owns that address, dropping"
                );
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                return;
            }
        }

        // Identity-based restart/rekey detection: if the peer is already
        // active but addr_to_link didn't match (different source address, e.g.,
        // TCP from a different port), we still need to check for restart/rekey.
        if !possible_restart && self.peers.contains_key(&peer_node_addr) {
            possible_restart = true;
        }

        // Early cap check: at max_peers and this is a net-new identity?
        // Bypass for known peers (reconnect / cross-connection) — admitting
        // them doesn't grow peers.len(). This silent-drops the Msg1 before
        // the Msg2 build/send and index allocation, avoiding wasted wire
        // bytes and giving the peer cleaner semantics (no fake-completed
        // handshake whose data frames subsequently fail decryption here).
        // The late cap check inside promote_connection() is intentionally
        // left in place as defense-in-depth.
        if self.max_peers() > 0 && self.peers.len() >= self.max_peers() {
            let is_known_active = self.peers.contains_key(&peer_node_addr);
            let is_pending_outbound = self.connections.iter().any(|(_, conn)| {
                conn.expected_identity()
                    .map(|id| *id.node_addr() == peer_node_addr)
                    .unwrap_or(false)
            });
            if !is_known_active && !is_pending_outbound {
                debug!(
                    peer = %self.peer_display_name(&peer_node_addr),
                    max = self.max_peers(),
                    "Silent-dropping Msg1 at max_peers cap (early gate; no Msg2 sent)"
                );
                // `link_id` was allocated above but `conn` is still a local
                // (not yet inserted into self.connections / self.links /
                // self.addr_to_link), so the local drop suffices.
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                return;
            }
        }

        // Epoch-based restart detection and duplicate msg1 handling.
        //
        // If we fell through from the addr_to_link check above with
        // possible_restart=true, we now have the decrypted epoch from msg1.
        // Compare it against the stored epoch for this peer.
        if possible_restart && let Some(existing_peer) = self.peers.get(&peer_node_addr) {
            let new_epoch = conn.remote_epoch();
            let existing_epoch = existing_peer.remote_epoch();
            let now_ms = Self::now_ms();
            // How long the peering this msg1 would destroy has gone without
            // authenticated inbound traffic. `last_seen` moves only on a
            // successful decrypt, so nothing an unauthenticated sender emits
            // can refresh it.
            let peering_idle_ms = existing_peer.idle_time(now_ms);

            match (existing_epoch, new_epoch) {
                (Some(existing), Some(new)) if existing != new => {
                    // Epoch mismatch — peer restarted. Tear down stale session.
                    //
                    // Two receiver-local conditions have to hold first. The
                    // epoch is sealed, so this msg1 is authentic, but a
                    // captured one stays authentic forever and replaying it
                    // destroys a working peering. Refuse while the peering is
                    // still carrying authenticated traffic — a peer that has
                    // genuinely restarted stopped feeding `last_seen` when it
                    // died, so this self-clears — and refuse a second epoch
                    // change inside the dampening interval, which bounds the
                    // churn a peer can drive on its own.
                    let dampened = self
                        .restart_dampener
                        .get(&peer_node_addr)
                        .is_some_and(|t| t.elapsed().as_secs() < EPOCH_RESTART_MIN_INTERVAL_SECS);
                    let peering_is_live = peering_idle_ms < EPOCH_RESTART_MIN_INTERVAL_SECS * 1000;
                    if peering_is_live || dampened {
                        debug!(
                            peer = %self.peer_display_name(&peer_node_addr),
                            idle_ms = peering_idle_ms,
                            dampened,
                            "Epoch mismatch dampened, dropping msg1"
                        );
                        // No msg2 is sent: the stored msg2 is bound to the
                        // original msg1's ephemeral, and answering an address
                        // the sender chose is free amplification.
                        self.connections.remove(&link_id);
                        self.links.remove(&link_id);
                        self.stats_mut()
                            .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                        return;
                    }
                    debug!(
                        peer = %self.peer_display_name(&peer_node_addr),
                        "Peer restart detected (epoch mismatch), removing stale session"
                    );
                    self.remove_active_peer(&peer_node_addr);
                    // Stamped on acceptance only. A refusal that slid the
                    // window would let a sustained replay starve a genuinely
                    // restarting peer for as long as it kept sending.
                    let cutoff = Duration::from_secs(EPOCH_RESTART_MIN_INTERVAL_SECS);
                    self.restart_dampener.retain(|_, t| t.elapsed() < cutoff);
                    self.restart_dampener.insert(peer_node_addr, Instant::now());
                    self.schedule_reconnect(peer_node_addr, now_ms);
                    // Fall through to process as new connection
                }
                _ => {
                    // Same epoch (or no epoch stored).
                    // If the peer has an active session and rekey is enabled,
                    // this is a rekey msg1 (not a duplicate initial msg1).
                    // Guard: the session must be at least 30s old to avoid
                    // misidentifying a cross-connection msg1 as a rekey.
                    // During simultaneous connection, both sides promote
                    // within the same tick and the peer's msg1 arrives
                    // immediately — a genuine rekey can't fire that fast.
                    let session_age_secs =
                        existing_peer.session_established_at().elapsed().as_secs();
                    if self.config().node.rekey.enabled
                        && existing_peer.has_session()
                        && existing_peer.is_healthy()
                        && session_age_secs >= 30
                    {
                        // Guard: already have a pending session from a completed
                        // rekey (waiting for K-bit cutover). Don't overwrite it
                        // with a new handshake — drop this msg1.
                        if existing_peer.pending_new_session().is_some() {
                            debug!(
                                peer = %self.peer_display_name(&peer_node_addr),
                                "Rekey msg1 received but already have pending session, dropping"
                            );
                            self.connections.remove(&link_id);
                            self.links.remove(&link_id);
                            self.stats_mut()
                                .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                            return;
                        }

                        // Dual-initiation detection: both sides sent msg1
                        // simultaneously. Apply tie-breaker — smaller NodeAddr
                        // wins as initiator (same as cross-connection resolution).
                        if existing_peer.rekey_in_progress() {
                            let our_addr = self.identity().node_addr();
                            if our_addr < &peer_node_addr {
                                // We win as initiator — drop their msg1.
                                // Our msg2 will arrive at peer, who completes
                                // as our responder.
                                debug!(
                                    peer = %self.peer_display_name(&peer_node_addr),
                                    "Dual rekey initiation: we win (smaller addr), dropping their msg1"
                                );
                                self.connections.remove(&link_id);
                                self.links.remove(&link_id);
                                self.stats_mut().record_reject(RejectReason::Handshake(
                                    HandshakeReject::BadState,
                                ));
                                return;
                            }
                            // We lose — abandon our rekey, become responder below.
                            debug!(
                                peer = %self.peer_display_name(&peer_node_addr),
                                "Dual rekey initiation: we lose (larger addr), abandoning ours"
                            );
                            if let Some(peer) = self.peers.get_mut(&peer_node_addr)
                                && let Some(idx) = peer.abandon_rekey()
                            {
                                if let Some(tid) = peer.transport_id() {
                                    self.peers_by_index.remove(&(tid, idx.as_u32()));
                                    self.pending_outbound.remove(&(tid, idx.as_u32()));
                                }
                                let _ = self.index_allocator.free(idx);
                            }
                            // Fall through to respond as responder
                        }

                        // Rekey: process as responder, store new session as pending
                        let noise_session = conn.take_session();
                        let our_new_index = match self.index_allocator.allocate() {
                            Ok(idx) => idx,
                            Err(e) => {
                                warn!(error = %e, "Failed to allocate index for rekey");
                                self.stats_mut().record_reject(RejectReason::Handshake(
                                    HandshakeReject::BadState,
                                ));
                                return;
                            }
                        };

                        let noise_session = match noise_session {
                            Some(s) => s,
                            None => {
                                warn!("Rekey msg1: no session from handshake");
                                let _ = self.index_allocator.free(our_new_index);
                                self.stats_mut().record_reject(RejectReason::Handshake(
                                    HandshakeReject::BadState,
                                ));
                                return;
                            }
                        };

                        // Send msg2 response using the new handshake
                        let wire_msg2 =
                            build_msg2(our_new_index, header.sender_idx, &msg2_response);
                        if let Some(transport) = self.transports.get(&packet.transport_id) {
                            match transport.send(&packet.remote_addr, &wire_msg2).await {
                                Ok(_) => {
                                    debug!(
                                        peer = %self.peer_display_name(&peer_node_addr),
                                        new_our_index = %our_new_index,
                                        "Sent rekey msg2 response"
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        peer = %self.peer_display_name(&peer_node_addr),
                                        error = %e,
                                        "Failed to send rekey msg2"
                                    );
                                    let _ = self.index_allocator.free(our_new_index);
                                    self.stats_mut().record_reject(RejectReason::Handshake(
                                        HandshakeReject::BadState,
                                    ));
                                    return;
                                }
                            }
                        }

                        // Store pending session on the existing peer
                        if let Some(peer) = self.peers.get_mut(&peer_node_addr) {
                            peer.set_pending_session(
                                noise_session,
                                our_new_index,
                                header.sender_idx,
                            );
                            peer.record_peer_rekey();
                        }

                        // Register new index in peers_by_index
                        self.peers_by_index.insert(
                            (packet.transport_id, our_new_index.as_u32()),
                            peer_node_addr,
                        );

                        // Clean up: remove the temporary connection/link we created.
                        // Do NOT remove addr_to_link — the entry must remain pointing
                        // to the original link so future msg1s from this address are
                        // recognized as rekeys (not new connections).
                        self.connections.remove(&link_id);
                        self.links.remove(&link_id);

                        return;
                    }

                    // Not a rekey — duplicate msg1. Resend stored msg2.
                    if let Some(msg2) = existing_peer.handshake_msg2().map(|m| m.to_vec())
                        && let Some(transport) = self.transports.get(&packet.transport_id)
                    {
                        match transport.send(&packet.remote_addr, &msg2).await {
                            Ok(_) => debug!(
                                peer = %self.peer_display_name(&peer_node_addr),
                                "Resent msg2 for duplicate msg1 (same epoch)"
                            ),
                            Err(e) => debug!(
                                peer = %self.peer_display_name(&peer_node_addr),
                                error = %e,
                                "Failed to resend msg2"
                            ),
                        }
                    }
                    return;
                }
            }
        }
        // If possible_restart was true but peer is no longer in self.peers
        // (removed by another path), fall through to process as new connection.

        if self
            .authorize_peer(
                &peer_identity,
                PeerAclContext::InboundHandshake,
                packet.transport_id,
                &packet.remote_addr,
            )
            .is_err()
        {
            self.stats_mut()
                .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
            return;
        }

        // Note: we don't early-return if peer is already in self.peers here.
        // promote_connection handles cross-connection resolution via tie-breaker.

        // Allocate our session index
        let our_index = match self.index_allocator.allocate() {
            Ok(idx) => idx,
            Err(e) => {
                warn!(error = %e, "Failed to allocate session index for inbound");
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                return;
            }
        };

        conn.set_our_index(our_index);
        conn.set_their_index(header.sender_idx);

        // Create link
        let link = Link::connectionless(
            link_id,
            packet.transport_id,
            packet.remote_addr.clone(),
            LinkDirection::Inbound,
            Duration::from_millis(self.config().node.base_rtt_ms),
        );

        self.links.insert(link_id, link);
        self.addr_to_link.insert(addr_key, link_id);
        self.connections.insert(link_id, conn);

        // Build and send msg2 response, storing for potential resend
        let wire_msg2 = build_msg2(our_index, header.sender_idx, &msg2_response);
        if let Some(conn) = self.connections.get_mut(&link_id) {
            conn.set_handshake_msg2(wire_msg2.clone());
        }

        if let Some(transport) = self.transports.get(&packet.transport_id) {
            match transport.send(&packet.remote_addr, &wire_msg2).await {
                Ok(bytes) => {
                    debug!(
                        link_id = %link_id,
                        our_index = %our_index,
                        their_index = %header.sender_idx,
                        bytes,
                        "Sent msg2 response"
                    );
                }
                Err(e) => {
                    warn!(
                        link_id = %link_id,
                        error = %e,
                        "Failed to send msg2"
                    );
                    // Clean up on failure
                    self.connections.remove(&link_id);
                    self.links.remove(&link_id);
                    self.addr_to_link
                        .remove(&(packet.transport_id, packet.remote_addr));
                    let _ = self.index_allocator.free(our_index);
                    self.stats_mut()
                        .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                    return;
                }
            }
        }

        // Responder handshake is complete after receive_handshake_init (Noise IK
        // pattern: responder processes msg1 and generates msg2 in one step).
        // Promote the connection to active peer now.
        match self.promote_connection(link_id, peer_identity, packet.timestamp_ms) {
            Ok(result) => {
                match result {
                    PromotionResult::Promoted(node_addr) => {
                        // Store msg2 on peer for resend on duplicate msg1
                        if let Some(peer) = self.peers.get_mut(&node_addr) {
                            peer.set_handshake_msg2(wire_msg2.clone());
                        }
                        // Promotion is logged once by `promote_connection`
                        // ("Connection promoted to active peer"); no separate
                        // inbound-path line.
                        // Send initial tree announce to new peer
                        if let Err(e) = self.send_tree_announce_to_peer(&node_addr).await {
                            debug!(peer = %self.peer_display_name(&node_addr), error = %e, "Failed to send initial TreeAnnounce");
                        }
                        // Schedule filter announce (sent on next tick via debounce)
                        self.bloom_state.mark_update_needed(node_addr);
                        self.reset_discovery_backoff();
                    }
                    PromotionResult::CrossConnectionWon {
                        loser_link_id,
                        node_addr,
                    } => {
                        // Store msg2 on peer for resend on duplicate msg1
                        if let Some(peer) = self.peers.get_mut(&node_addr) {
                            peer.set_handshake_msg2(wire_msg2.clone());
                        }
                        // Close the losing TCP connection (no-op for connectionless)
                        if let Some(loser_link) = self.links.get(&loser_link_id) {
                            let loser_tid = loser_link.transport_id();
                            let loser_addr = loser_link.remote_addr().clone();
                            if let Some(transport) = self.transports.get(&loser_tid) {
                                transport.close_connection(&loser_addr).await;
                            }
                        }
                        // Clean up the losing connection's link
                        self.remove_link(&loser_link_id);
                        debug!(
                            peer = %self.peer_display_name(&node_addr),
                            loser_link_id = %loser_link_id,
                            "Inbound cross-connection won, loser link cleaned up"
                        );
                        // Send initial tree announce to peer (new or reconnected)
                        if let Err(e) = self.send_tree_announce_to_peer(&node_addr).await {
                            debug!(peer = %self.peer_display_name(&node_addr), error = %e, "Failed to send initial TreeAnnounce");
                        }
                        // Schedule filter announce (sent on next tick via debounce)
                        self.bloom_state.mark_update_needed(node_addr);
                        self.reset_discovery_backoff();
                    }
                    PromotionResult::CrossConnectionLost { winner_link_id } => {
                        // Close the losing TCP connection (no-op for connectionless)
                        if let Some(transport) = self.transports.get(&packet.transport_id) {
                            transport.close_connection(&packet.remote_addr).await;
                        }
                        // This connection lost — clean up its link
                        self.remove_link(&link_id);
                        // Restore addr_to_link for the winner's link
                        self.addr_to_link.insert(
                            (packet.transport_id, packet.remote_addr.clone()),
                            winner_link_id,
                        );
                        debug!(
                            winner_link_id = %winner_link_id,
                            "Inbound cross-connection lost, keeping existing"
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    link_id = %link_id,
                    error = %e,
                    "Failed to promote inbound connection"
                );
                // Clean up on promotion failure
                self.remove_link(&link_id);
                let _ = self.index_allocator.free(our_index);
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
            }
        }
    }

    /// Find stored msg2 bytes for a given link (pre- or post-promotion).
    ///
    /// Checks the PeerConnection (if still pending) and then the ActivePeer
    /// (if already promoted).
    fn find_stored_msg2(&self, link_id: LinkId) -> Option<Vec<u8>> {
        // Check pending connection first
        if let Some(conn) = self.connections.get(&link_id)
            && let Some(msg2) = conn.handshake_msg2()
        {
            return Some(msg2.to_vec());
        }
        // Check promoted peer
        for peer in self.peers.values() {
            if peer.link_id() == link_id
                && let Some(msg2) = peer.handshake_msg2()
            {
                return Some(msg2.to_vec());
            }
        }
        None
    }

    /// Handle handshake message 2 (phase 0x2).
    ///
    /// This completes an outbound handshake we initiated.
    pub(in crate::node) async fn handle_msg2(&mut self, packet: ReceivedPacket) {
        // Parse header
        let header = match Msg2Header::parse(&packet.data) {
            Some(h) => h,
            None => {
                debug!("Invalid msg2 header");
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                return;
            }
        };

        // Look up our pending handshake by our sender_idx (receiver_idx in msg2)
        let key = (packet.transport_id, header.receiver_idx.as_u32());
        let link_id = match self.pending_outbound.get(&key) {
            Some(id) => *id,
            None => {
                debug!(
                    receiver_idx = %header.receiver_idx,
                    "No pending outbound handshake for index"
                );
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::UnknownConnection));
                return;
            }
        };

        // Check if this is a rekey msg2: the handshake state is on the
        // ActivePeer (not a PeerConnection), so self.connections won't have it.
        // Look for a peer with matching rekey_our_index.
        if !self.connections.contains_key(&link_id) {
            let noise_msg2 = &packet.data[header.noise_msg2_offset..];

            // Find peer with rekey in progress for this index
            let peer_addr = self.peers.iter().find_map(|(addr, peer)| {
                if peer.rekey_in_progress() && peer.rekey_our_index() == Some(header.receiver_idx) {
                    Some(*addr)
                } else {
                    None
                }
            });

            if let Some(peer_node_addr) = peer_addr {
                let display_name = self.peer_display_name(&peer_node_addr);

                // Complete the rekey handshake on the ActivePeer
                if let Some(peer) = self.peers.get_mut(&peer_node_addr) {
                    match peer.complete_rekey_msg2(noise_msg2) {
                        Ok((session, remote_epoch)) => {
                            let our_index = peer.rekey_our_index().unwrap_or(header.receiver_idx);
                            let remote_epoch_changed = matches!(
                                (peer.remote_epoch(), remote_epoch),
                                (Some(old), Some(new)) if old != new
                            );
                            if remote_epoch.is_some() {
                                peer.set_remote_epoch(remote_epoch);
                            }
                            peer.set_pending_session(session, our_index, header.sender_idx);

                            if let Some(transport_id) = peer.transport_id() {
                                self.peers_by_index
                                    .insert((transport_id, our_index.as_u32()), peer_node_addr);
                            }

                            if remote_epoch_changed {
                                if self.sessions.remove(&peer_node_addr).is_some() {
                                    debug!(
                                        peer = %display_name,
                                        "Cleared stale FSP session after peer restart during FMP rekey"
                                    );
                                }
                                info!(
                                    peer = %display_name,
                                    "Peer restart detected during FMP rekey, replacing stale endpoint session"
                                );
                            }

                            debug!(
                                peer = %display_name,
                                new_our_index = %our_index,
                                new_their_index = %header.sender_idx,
                                "Rekey completed (initiator), pending K-bit cutover"
                            );
                        }
                        Err(e) => {
                            warn!(
                                peer = %display_name,
                                error = %e,
                                "Rekey msg2 processing failed"
                            );
                            if let Some(idx) = peer.abandon_rekey() {
                                if let Some(tid) = peer.transport_id() {
                                    self.peers_by_index.remove(&(tid, idx.as_u32()));
                                }
                                let _ = self.index_allocator.free(idx);
                            }
                            self.stats_mut()
                                .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                        }
                    }
                }

                self.pending_outbound.remove(&key);
                return;
            }

            // Not a rekey — stale pending_outbound entry pointing at a
            // removed connection and no rekey-in-progress peer claims the
            // receiver_idx. State-machine inconsistency, not a fresh
            // lookup miss.
            self.pending_outbound.remove(&key);
            self.stats_mut()
                .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
            return;
        }

        let (peer_identity, our_index) = {
            let conn = self.connections.get_mut(&link_id).unwrap();

            let noise_msg2 = &packet.data[header.noise_msg2_offset..];
            if let Err(e) = conn.complete_handshake(noise_msg2, packet.timestamp_ms) {
                warn!(
                    link_id = %link_id,
                    error = %e,
                    "Handshake completion failed"
                );
                conn.mark_failed();
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                return;
            }

            conn.set_their_index(header.sender_idx);
            conn.set_source_addr(packet.remote_addr.clone());

            let peer_identity = match conn.expected_identity() {
                Some(id) => *id,
                None => {
                    warn!(link_id = %link_id, "No identity after handshake");
                    self.stats_mut()
                        .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                    return;
                }
            };

            (peer_identity, conn.our_index())
        };

        if self
            .authorize_peer(
                &peer_identity,
                PeerAclContext::OutboundHandshake,
                packet.transport_id,
                &packet.remote_addr,
            )
            .is_err()
        {
            self.pending_outbound.remove(&key);
            if let Some(link) = self.links.get(&link_id) {
                let tid = link.transport_id();
                let addr = link.remote_addr().clone();
                if let Some(transport) = self.transports.get(&tid) {
                    transport.close_connection(&addr).await;
                }
            }
            self.connections.remove(&link_id);
            self.remove_link(&link_id);
            if let Some(idx) = our_index {
                let _ = self.index_allocator.free(idx);
            }
            self.stats_mut()
                .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
            return;
        }

        let peer_node_addr = *peer_identity.node_addr();

        debug!(
            peer = %self.peer_display_name(&peer_node_addr),
            link_id = %link_id,
            their_index = %header.sender_idx,
            "Outbound handshake completed"
        );

        // Cross-connection resolution: if the peer was already promoted via
        // our inbound handshake (we processed their msg1), both nodes initially
        // use mismatched sessions. The tie-breaker determines which handshake
        // wins: smaller node_addr's outbound.
        //
        // - Winner (smaller node): swap to outbound session + outbound indices
        // - Loser (larger node): keep inbound session + original their_index
        //
        // This ensures both nodes use the same Noise handshake (the winner's
        // outbound = the loser's inbound).
        if self.peers.contains_key(&peer_node_addr) {
            let our_outbound_wins = cross_connection_winner(
                self.identity().node_addr(),
                &peer_node_addr,
                true, // this IS our outbound
            );

            // Extract the outbound connection
            let mut conn = match self.connections.remove(&link_id) {
                Some(c) => c,
                None => {
                    self.pending_outbound.remove(&key);
                    self.stats_mut()
                        .record_reject(RejectReason::Handshake(HandshakeReject::UnknownConnection));
                    return;
                }
            };

            if our_outbound_wins {
                // We're the smaller node. Swap to outbound session + indices.
                // The peer will keep their inbound session (complement of ours).
                let outbound_our_index = conn.our_index();
                let outbound_session = conn.take_session();

                let (outbound_session, outbound_our_index) = match (
                    outbound_session,
                    outbound_our_index,
                ) {
                    (Some(s), Some(idx)) => (s, idx),
                    _ => {
                        warn!(peer = %self.peer_display_name(&peer_node_addr), "Incomplete outbound connection");
                        self.pending_outbound.remove(&key);
                        self.stats_mut()
                            .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
                        return;
                    }
                };

                if let Some(peer) = self.peers.get_mut(&peer_node_addr) {
                    let suppressed = peer.replay_suppressed_count();
                    let old_our_index = peer.replace_session(
                        outbound_session,
                        outbound_our_index,
                        header.sender_idx,
                    );

                    // Update peers_by_index: remove old inbound index, add outbound
                    let transport_id = peer.transport_id().unwrap();
                    if let Some(old_idx) = old_our_index {
                        self.peers_by_index
                            .remove(&(transport_id, old_idx.as_u32()));
                        let _ = self.index_allocator.free(old_idx);
                    }
                    self.peers_by_index
                        .insert((transport_id, outbound_our_index.as_u32()), peer_node_addr);

                    if suppressed > 0 {
                        debug!(
                            peer = %self.peer_display_name(&peer_node_addr),
                            count = suppressed,
                            "Suppressed replay detections during link transition"
                        );
                    }

                    debug!(
                        peer = %self.peer_display_name(&peer_node_addr),
                        new_our_index = %outbound_our_index,
                        new_their_index = %header.sender_idx,
                        "Cross-connection: swapped to outbound session (our outbound wins)"
                    );
                }
            } else {
                // We're the larger node. Keep our inbound session (it pairs
                // with the peer's outbound, which is the winning handshake).
                //
                // Do NOT update their_index here. Our their_index was set during
                // promote_connection() from the peer's msg1 sender_idx, which is
                // the peer's outbound our_index. After the peer (winner) swaps to
                // their outbound session, that index is exactly what they'll use.
                // The msg2 sender_idx we see here is the peer's INBOUND our_index,
                // which becomes stale after the peer swaps.
                let outbound_our_index = conn.our_index();

                if let Some(peer) = self.peers.get(&peer_node_addr) {
                    debug!(
                        peer = %self.peer_display_name(&peer_node_addr),
                        kept_their_index = ?peer.their_index(),
                        "Cross-connection: keeping inbound session and original their_index (peer outbound wins)"
                    );
                }

                // Free the outbound's session index since we're not using it
                if let Some(idx) = outbound_our_index {
                    let _ = self.index_allocator.free(idx);
                }
            }

            // Clean up outbound connection state
            self.pending_outbound.remove(&key);
            // Close the losing TCP connection (no-op for connectionless)
            if let Some(link) = self.links.get(&link_id) {
                let tid = link.transport_id();
                let addr = link.remote_addr().clone();
                if let Some(transport) = self.transports.get(&tid) {
                    transport.close_connection(&addr).await;
                }
            }
            self.remove_link(&link_id);

            // Send TreeAnnounce now that sessions are aligned
            if let Err(e) = self.send_tree_announce_to_peer(&peer_node_addr).await {
                debug!(peer = %self.peer_display_name(&peer_node_addr), error = %e, "Failed to send TreeAnnounce after cross-connection resolution");
            }
            // Schedule filter announce (sent on next tick via debounce)
            self.bloom_state.mark_update_needed(peer_node_addr);
            self.reset_discovery_backoff();
            return;
        }

        // Normal path: promote to active peer
        match self.promote_connection(link_id, peer_identity, packet.timestamp_ms) {
            Ok(result) => {
                // Clean up pending_outbound
                self.pending_outbound.remove(&key);

                match result {
                    PromotionResult::Promoted(node_addr) => {
                        info!(
                            peer = %self.peer_display_name(&node_addr),
                            "Peer promoted to active"
                        );
                        // Send initial tree announce to new peer
                        if let Err(e) = self.send_tree_announce_to_peer(&node_addr).await {
                            debug!(peer = %self.peer_display_name(&node_addr), error = %e, "Failed to send initial TreeAnnounce");
                        }
                        // Schedule filter announce (sent on next tick via debounce)
                        self.bloom_state.mark_update_needed(node_addr);
                        self.reset_discovery_backoff();
                    }
                    PromotionResult::CrossConnectionWon {
                        loser_link_id,
                        node_addr,
                    } => {
                        // Close the losing TCP connection (no-op for connectionless)
                        if let Some(loser_link) = self.links.get(&loser_link_id) {
                            let loser_tid = loser_link.transport_id();
                            let loser_addr = loser_link.remote_addr().clone();
                            if let Some(transport) = self.transports.get(&loser_tid) {
                                transport.close_connection(&loser_addr).await;
                            }
                        }
                        // Clean up the losing connection's link
                        self.remove_link(&loser_link_id);
                        // Ensure addr_to_link points to the winning link
                        self.addr_to_link
                            .insert((packet.transport_id, packet.remote_addr.clone()), link_id);
                        debug!(
                            peer = %self.peer_display_name(&node_addr),
                            loser_link_id = %loser_link_id,
                            "Outbound cross-connection won, loser link cleaned up"
                        );
                        // Send initial tree announce to peer (new or reconnected)
                        if let Err(e) = self.send_tree_announce_to_peer(&node_addr).await {
                            debug!(peer = %self.peer_display_name(&node_addr), error = %e, "Failed to send initial TreeAnnounce");
                        }
                        // Schedule filter announce (sent on next tick via debounce)
                        self.bloom_state.mark_update_needed(node_addr);
                        self.reset_discovery_backoff();
                    }
                    PromotionResult::CrossConnectionLost { winner_link_id } => {
                        // Close the losing TCP connection (no-op for connectionless)
                        if let Some(transport) = self.transports.get(&packet.transport_id) {
                            transport.close_connection(&packet.remote_addr).await;
                        }
                        // This connection lost — clean up its link
                        self.remove_link(&link_id);
                        // Ensure addr_to_link points to the winner's link
                        self.addr_to_link.insert(
                            (packet.transport_id, packet.remote_addr.clone()),
                            winner_link_id,
                        );
                        debug!(
                            winner_link_id = %winner_link_id,
                            "Outbound cross-connection lost, keeping existing"
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    link_id = %link_id,
                    error = %e,
                    "Failed to promote connection"
                );
                self.stats_mut()
                    .record_reject(RejectReason::Handshake(HandshakeReject::BadState));
            }
        }
    }

    /// Promote a connection to active peer after successful authentication.
    ///
    /// Handles cross-connection detection and resolution using tie-breaker rules.
    pub(in crate::node) fn promote_connection(
        &mut self,
        link_id: LinkId,
        verified_identity: PeerIdentity,
        current_time_ms: u64,
    ) -> Result<PromotionResult, NodeError> {
        // Remove the connection from pending
        let mut connection = self
            .connections
            .remove(&link_id)
            .ok_or(NodeError::ConnectionNotFound(link_id))?;

        // Verify handshake is complete and extract session
        if !connection.has_session() {
            return Err(NodeError::HandshakeIncomplete(link_id));
        }

        let noise_session = connection
            .take_session()
            .ok_or(NodeError::NoSession(link_id))?;

        let our_index = connection
            .our_index()
            .ok_or_else(|| NodeError::PromotionFailed {
                link_id,
                reason: "missing our_index".into(),
            })?;
        let their_index = connection
            .their_index()
            .ok_or_else(|| NodeError::PromotionFailed {
                link_id,
                reason: "missing their_index".into(),
            })?;
        let transport_id = connection
            .transport_id()
            .ok_or_else(|| NodeError::PromotionFailed {
                link_id,
                reason: "missing transport_id".into(),
            })?;
        let current_addr = connection
            .source_addr()
            .ok_or_else(|| NodeError::PromotionFailed {
                link_id,
                reason: "missing source_addr".into(),
            })?
            .clone();
        let link_stats = connection.link_stats().clone();
        let remote_epoch = connection.remote_epoch();

        let peer_node_addr = *verified_identity.node_addr();
        let is_outbound = connection.is_outbound();

        // Check for cross-connection
        if let Some(existing_peer) = self.peers.get(&peer_node_addr) {
            let existing_link_id = existing_peer.link_id();

            let remote_epoch_changed = matches!((existing_peer.remote_epoch(), remote_epoch), (Some(old), Some(new)) if old != new);

            // Determine which connection wins. A peer restart (different
            // startup epoch) is not a normal cross-connection: the old link
            // and FSP sessions are cryptographically stale, so the freshly
            // authenticated connection must replace them regardless of the
            // tie-breaker direction.
            let this_wins = remote_epoch_changed
                || cross_connection_winner(
                    self.identity().node_addr(),
                    &peer_node_addr,
                    is_outbound,
                );

            if this_wins {
                // This connection wins, replace the existing peer
                let old_peer = self.peers.remove(&peer_node_addr).unwrap();
                let loser_link_id = old_peer.link_id();

                // Clean up old peer's index from peers_by_index
                if let (Some(old_tid), Some(old_idx)) =
                    (old_peer.transport_id(), old_peer.our_index())
                {
                    self.peers_by_index.remove(&(old_tid, old_idx.as_u32()));
                    // Unregister the OLD cache_key from the decrypt
                    // worker pool BEFORE freeing the index for reuse.
                    // Otherwise the worker's per-shard HashMap retains a
                    // stale entry pointing at the removed peer's session;
                    // if the index allocator later recycles old_idx to a
                    // different peer, the new register call overwrites
                    // the stale entry — but until that point, decrypt
                    // jobs that land at the recycled cache_key resolve
                    // to the wrong session and AEAD silently fails.
                    #[cfg(unix)]
                    self.unregister_decrypt_worker_session((old_tid, old_idx.as_u32()));
                    let _ = self.index_allocator.free(old_idx);
                }

                if remote_epoch_changed {
                    if self.sessions.remove(&peer_node_addr).is_some() {
                        debug!(
                            peer = %self.peer_display_name(&peer_node_addr),
                            "Cleared stale FSP session after peer restart during promotion"
                        );
                    }
                    info!(
                        peer = %self.peer_display_name(&peer_node_addr),
                        winner_link = %link_id,
                        loser_link = %loser_link_id,
                        "Peer restart detected during promotion, replacing stale active peer"
                    );
                }

                self.seed_path_mtu_for_link_peer(&peer_node_addr, transport_id, &current_addr);

                let mut new_peer = ActivePeer::with_session(
                    verified_identity,
                    link_id,
                    current_time_ms,
                    noise_session,
                    our_index,
                    their_index,
                    transport_id,
                    current_addr,
                    link_stats,
                    is_outbound,
                    &self.config().node.mmp,
                    remote_epoch,
                );
                new_peer.set_tree_announce_min_interval_ms(
                    self.config().node.tree.announce_min_interval_ms,
                );

                self.peers.insert(peer_node_addr, new_peer);
                self.peers_by_index
                    .insert((transport_id, our_index.as_u32()), peer_node_addr);
                self.retry_pending.remove(&peer_node_addr);
                self.register_identity(peer_node_addr, verified_identity.pubkey_full());

                debug!(
                    peer = %self.peer_display_name(&peer_node_addr),
                    winner_link = %link_id,
                    loser_link = %loser_link_id,
                    "Cross-connection resolved: this connection won"
                );

                // Hand the FMP recv cipher + replay window to the
                // decrypt shard worker. (Same as normal-promotion tail
                // below.)
                #[cfg(unix)]
                self.register_decrypt_worker_session(&peer_node_addr);

                Ok(PromotionResult::CrossConnectionWon {
                    loser_link_id,
                    node_addr: peer_node_addr,
                })
            } else {
                // This connection loses, keep existing
                // Free the index we allocated
                let _ = self.index_allocator.free(our_index);

                debug!(
                    peer = %self.peer_display_name(&peer_node_addr),
                    winner_link = %existing_link_id,
                    loser_link = %link_id,
                    "Cross-connection resolved: this connection lost"
                );

                Ok(PromotionResult::CrossConnectionLost {
                    winner_link_id: existing_link_id,
                })
            }
        } else {
            // No existing promoted peer. There may be a pending outbound
            // connection to the same peer (cross-connection in progress).
            // Do NOT clean it up yet — we need the outbound to stay alive
            // so that when the peer's msg2 arrives, we can learn the peer's
            // inbound session index and update their_index on the promoted
            // peer. The outbound will be cleaned up in handle_msg2 or by
            // the 30s handshake timeout.
            let pending_to_same_peer: Vec<LinkId> = self
                .connections
                .iter()
                .filter(|(_, conn)| {
                    conn.expected_identity()
                        .map(|id| *id.node_addr() == peer_node_addr)
                        .unwrap_or(false)
                })
                .map(|(lid, _)| *lid)
                .collect();

            for pending_link_id in &pending_to_same_peer {
                debug!(
                    peer = %self.peer_display_name(&peer_node_addr),
                    pending_link_id = %pending_link_id,
                    promoted_link_id = %link_id,
                    "Deferring cleanup of pending outbound (awaiting msg2 for index update)"
                );
            }

            // Normal promotion
            if self.max_peers() > 0 && self.peers.len() >= self.max_peers() {
                let _ = self.index_allocator.free(our_index);
                return Err(NodeError::MaxPeersExceeded {
                    max: self.max_peers(),
                });
            }

            // Preserve tree announce rate-limit state from old peer (if reconnecting).
            // Without this, reconnection resets the rate limit window to zero,
            // allowing an immediate announce that can feed an announce loop.
            let old_announce_ts = self
                .peers
                .get(&peer_node_addr)
                .map(|p| p.last_tree_announce_sent_ms());

            self.seed_path_mtu_for_link_peer(&peer_node_addr, transport_id, &current_addr);

            let mut new_peer = ActivePeer::with_session(
                verified_identity,
                link_id,
                current_time_ms,
                noise_session,
                our_index,
                their_index,
                transport_id,
                current_addr,
                link_stats,
                is_outbound,
                &self.config().node.mmp,
                remote_epoch,
            );
            new_peer.set_tree_announce_min_interval_ms(
                self.config().node.tree.announce_min_interval_ms,
            );
            if let Some(ts) = old_announce_ts {
                new_peer.set_last_tree_announce_sent_ms(ts);
            }

            self.peers.insert(peer_node_addr, new_peer);
            self.peers_by_index
                .insert((transport_id, our_index.as_u32()), peer_node_addr);
            self.retry_pending.remove(&peer_node_addr);
            self.register_identity(peer_node_addr, verified_identity.pubkey_full());

            debug!(
                peer = %self.peer_display_name(&peer_node_addr),
                link_id = %link_id,
                our_index = %our_index,
                their_index = %their_index,
                "Connection promoted to active peer"
            );

            // Hand the FMP recv cipher + replay window to the
            // decrypt shard worker. From this point on the worker
            // is the sole authority on FMP replay protection for
            // this session. No-op when the worker pool isn't
            // spawned (unit-test path or `FIPS_DECRYPT_WORKERS=0`).
            #[cfg(unix)]
            self.register_decrypt_worker_session(&peer_node_addr);

            Ok(PromotionResult::Promoted(peer_node_addr))
        }
    }
}
