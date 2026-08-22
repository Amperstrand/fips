//! LookupRequest/LookupResponse discovery protocol handlers.
//!
//! Handles coordinate discovery via bloom-filter-guided tree routing.
//! Requests are forwarded only to tree peers (parent + children) whose
//! bloom filter contains the target. TTL and request_id dedup provide
//! safety bounds.

use crate::node::reject::DiscoveryReject;
use crate::node::{Node, RecentRequest};
use crate::protocol::{LookupRequest, LookupResponse};
use crate::transport::{TransportAddr, TransportId};
use crate::{NodeAddr, PeerIdentity};
use tracing::{debug, info, trace, warn};

/// Cap on the discovery request dedup cache, which is also the reverse-path
/// table for responses in flight.
pub(in crate::node) const MAX_RECENT_DISCOVERY_REQUESTS: usize = 4096;

/// Floor under one link peer's share of the dedup cache.
///
/// A peer's share is the cache divided by the current link-peer count, and
/// this is what stops that share collapsing to nothing on a node with very
/// many links. It is a cap and not a reservation: shares can sum past the
/// cache size, in which case the peer holding the most entries pays for the
/// next admission. Raising it lets one busy neighbour hold more of the
/// cache; lowering it clips a genuine transit burst.
pub(in crate::node) const MIN_RECENT_PER_PEER: usize = 64;

impl Node {
    /// Handle an incoming LookupRequest from a peer.
    ///
    /// Processing steps:
    /// 1. Decode and validate
    /// 2. Check request_id for duplicates (dedup / reverse-path routing)
    /// 3. Record request for reverse-path forwarding
    /// 4. Lazy purge expired entries
    /// 5. If we're the target, generate and send response
    /// 6. If TTL > 0, forward to tree peers whose bloom filter matches
    pub(in crate::node) async fn handle_lookup_request(&mut self, from: &NodeAddr, payload: &[u8]) {
        self.metrics().discovery.req_received.inc();

        let request = match LookupRequest::decode(payload) {
            Ok(req) => req,
            Err(e) => {
                self.metrics()
                    .discovery
                    .record_reject(DiscoveryReject::ReqDecodeError);
                debug!(from = %self.peer_display_name(from), error = %e, "Malformed LookupRequest");
                return;
            }
        };

        let now_ms = Self::now_ms();
        self.purge_expired_requests(now_ms);

        // Dedup: drop if we've already seen this request_id.
        // Also serves as loop protection — tree routing is loop-free,
        // but request_id dedup catches edge cases during tree restructuring.
        if self.recent_requests.contains_key(&request.request_id) {
            self.metrics()
                .discovery
                .record_reject(DiscoveryReject::ReqDuplicate);
            debug!(
                request_id = request.request_id,
                from = %self.peer_display_name(from),
                "Duplicate LookupRequest, dropping"
            );
            return;
        }

        // A full cache evicts rather than refuses. Refusing meant one peer
        // could fill the cache with fresh request_ids and stop this node
        // answering lookups for itself and forwarding anyone else's, which
        // is a denial of the service the cache exists to protect. The
        // eviction is charged to the peer that filled the cache: over its
        // own share it pays for itself, and at global capacity the peer
        // holding the most entries pays, so extra identities buy a flooder
        // proportionally less and a light peer's reverse path survives.
        self.make_room_for_request(from);

        // Record for reverse-path forwarding and dedup
        self.recent_requests
            .insert(request.request_id, RecentRequest::new(*from, now_ms));
        self.recent_by_peer
            .entry(*from)
            .or_default()
            .push_back(request.request_id);

        // Are we the target?
        if request.target == *self.node_addr() {
            // Answering costs a fresh Schnorr signature every time: the
            // proof is bound to the requester's request_id, so it cannot be
            // cached or served twice. Meter that per link peer, or a
            // neighbour generating request_ids sets this node's signing
            // rate. The dedup entry above stays regardless, so a refused
            // request still occupies its id and a retry, which carries a
            // fresh id, is unaffected.
            if !self.discovery_sign_limiter.should_sign(from) {
                self.metrics()
                    .discovery
                    .record_reject(DiscoveryReject::ReqSignRateLimited);
                debug!(
                    request_id = request.request_id,
                    from = %self.peer_display_name(from),
                    "Lookup signing budget spent for this peer, not answering"
                );
                return;
            }
            self.metrics().discovery.req_target_is_us.inc();
            debug!(
                request_id = request.request_id,
                origin = %self.peer_display_name(&request.origin),
                "We are the lookup target, generating response"
            );
            self.send_lookup_response(&request).await;
            return;
        }

        // Forward if TTL permits
        if request.can_forward() {
            // Transit-side rate limit: collapse rapid-fire lookups for the
            // same target from misbehaving nodes generating fresh request_ids.
            if !self
                .discovery_forward_limiter
                .should_forward(&request.target)
            {
                self.metrics().discovery.req_forward_rate_limited.inc();
                debug!(
                    request_id = request.request_id,
                    target = %self.peer_display_name(&request.target),
                    "Forward rate limited, suppressing LookupRequest"
                );
                return;
            }
            self.metrics().discovery.req_forwarded.inc();
            self.forward_lookup_request(request).await;
        } else {
            self.metrics()
                .discovery
                .record_reject(DiscoveryReject::ReqTtlExhausted);
            debug!(
                request_id = request.request_id,
                target = %self.peer_display_name(&request.target),
                "LookupRequest TTL exhausted"
            );
        }
    }

    /// Handle an incoming LookupResponse from a peer.
    ///
    /// Processing steps:
    /// 1. Decode and validate
    /// 2. Check recent_requests to determine if we originated or are forwarding
    /// 3. If originator: verify proof signature, then cache target_coords and path_mtu in coord_cache
    /// 4. If transit: apply path_mtu min(outgoing_link_mtu), reverse-path forward to from_peer
    pub(in crate::node) async fn handle_lookup_response(
        &mut self,
        from: &NodeAddr,
        payload: &[u8],
    ) {
        self.metrics().discovery.resp_received.inc();

        let mut response = match LookupResponse::decode(payload) {
            Ok(resp) => resp,
            Err(e) => {
                self.metrics()
                    .discovery
                    .record_reject(DiscoveryReject::RespDecodeError);
                debug!(from = %self.peer_display_name(from), error = %e, "Malformed LookupResponse");
                return;
            }
        };

        let now_ms = Self::now_ms();

        // Check if we forwarded this request (transit node) or originated it
        if let Some(recent) = self.recent_requests.get_mut(&response.request_id) {
            // Already forwarded a response for this request — drop to
            // prevent response routing loops.
            if recent.response_forwarded {
                debug!(
                    request_id = response.request_id,
                    target = %self.peer_display_name(&response.target),
                    "Response already forwarded for this request, dropping"
                );
                return;
            }
            recent.response_forwarded = true;

            // Transit node: reverse-path forward
            let from_peer = recent.from_peer;
            self.metrics().discovery.resp_forwarded.inc();

            // Apply path_mtu min() from the outgoing link's transport MTU
            self.apply_outgoing_link_mtu_to_response(&mut response, &from_peer);

            debug!(
                request_id = response.request_id,
                target = %self.peer_display_name(&response.target),
                next_hop = %self.peer_display_name(&from_peer),
                path_mtu = response.path_mtu,
                "Reverse-path forwarding LookupResponse"
            );

            let encoded = response.encode();
            if let Err(e) = self.send_encrypted_link_message(&from_peer, &encoded).await {
                debug!(
                    next_hop = %self.peer_display_name(&from_peer),
                    error = %e,
                    "Failed to forward LookupResponse"
                );
            }
        } else {
            // We originated this request — verify proof before caching
            let target = response.target;
            let path_mtu = response.path_mtu;

            // Correlate against our own outstanding lookups first. The
            // request_id is fresh 64-bit randomness we drew per attempt and
            // the target signs over it, so requiring the response to carry
            // one we issued for this target is what makes this path
            // solicited: an unsolicited or replayed response is dropped
            // here, before the identity resolve and before the signature
            // verify, and so cannot clear pending state, record a backoff
            // success, refresh the coordinate cache, or flush queued
            // packets. A duplicate of a response already accepted lands
            // here too, which is ordinary and is why this is debug level.
            let solicited = self
                .pending_lookups
                .get(&target)
                .is_some_and(|pending| pending.matches(response.request_id));
            if !solicited {
                self.metrics()
                    .discovery
                    .record_reject(DiscoveryReject::RespUnsolicited);
                debug!(
                    request_id = response.request_id,
                    target = %self.peer_display_name(&target),
                    "LookupResponse does not match an outstanding request, dropping"
                );
                return;
            }

            // Look up the target's public key from identity_cache
            let mut prefix = [0u8; 15];
            prefix.copy_from_slice(&target.as_bytes()[0..15]);
            let target_pubkey = match self.lookup_by_fips_prefix(&prefix) {
                Some((_addr, pubkey)) => pubkey,
                None => {
                    self.metrics()
                        .discovery
                        .record_reject(DiscoveryReject::RespIdentityMiss);
                    warn!(
                        request_id = response.request_id,
                        target = %self.peer_display_name(&target),
                        "identity_cache miss for lookup target, cannot verify proof"
                    );
                    return;
                }
            };

            // Verify the proof signature
            let (xonly, _parity) = target_pubkey.x_only_public_key();
            let peer_id = PeerIdentity::from_pubkey(xonly);
            let proof_data =
                LookupResponse::proof_bytes(response.request_id, &target, &response.target_coords);
            if !peer_id.verify(&proof_data, &response.proof) {
                self.metrics()
                    .discovery
                    .record_reject(DiscoveryReject::RespProofFailed);
                warn!(
                    request_id = response.request_id,
                    target = %self.peer_display_name(&target),
                    "LookupResponse proof verification failed, discarding"
                );
                return;
            }

            self.metrics().discovery.resp_accepted.inc();

            // Clear backoff on success — target is reachable
            self.discovery_backoff.record_success(&target);

            info!(
                request_id = response.request_id,
                target = %self.peer_display_name(&target),
                depth = response.target_coords.depth(),
                path_mtu = path_mtu,
                "Discovery succeeded, proof verified, route cached"
            );

            // The annotation is unsigned and accumulates hop by hop, so any
            // forwarder on the reverse path can lower it. A value below the
            // actionable floor cannot describe a usable path, so treat it as
            // absent: cache the coordinates, which are what the proof covers,
            // and store no path MTU from this response at all.
            let path_mtu_actionable = path_mtu >= crate::upper::icmp::MIN_ACTIONABLE_PATH_MTU;
            if path_mtu_actionable {
                self.coord_cache.insert_with_path_mtu(
                    target,
                    response.target_coords,
                    now_ms,
                    path_mtu,
                );
            } else {
                warn!(
                    request_id = response.request_id,
                    target = %self.peer_display_name(&target),
                    path_mtu = path_mtu,
                    floor = crate::upper::icmp::MIN_ACTIONABLE_PATH_MTU,
                    "LookupResponse carries a path MTU below the actionable floor; \
                     caching coordinates without it"
                );
                self.metrics().errors.lookup_resp_mtu_below_floor.inc();
                self.coord_cache
                    .insert(target, response.target_coords, now_ms);
            }

            // Mirror path_mtu into the FipsAddress-keyed read-only lookup
            // map used by the TUN reader/writer at TCP MSS clamp time.
            let fips_addr = crate::FipsAddress::from_node_addr(&target);
            if path_mtu_actionable {
                match self.path_mtu_lookup.write() {
                    Ok(mut map) => match map.get(&fips_addr).copied() {
                        Some(existing) if existing.mtu <= path_mtu => {
                            // Keep the tighter learned value; never loosen the
                            // clamp. A reactive MtuExceeded or PathMtuNotification
                            // tighten takes precedence over a looser discovery
                            // estimate (cross-carrier keep-tighter).
                            //
                            // This arm deliberately leaves `learned_ms` alone.
                            // That is what bounds a replayed response: the
                            // replay of a value already stored takes this arm,
                            // so the entry still expires at first-write plus
                            // the TTL rather than being pushed out again on
                            // every injection. Refreshing the stamp here would
                            // read as a tidy-up and would silently restore
                            // indefinite pinning.
                            debug!(
                                target = %self.peer_display_name(&target),
                                fips_addr = %fips_addr,
                                path_mtu = path_mtu,
                                existing = existing.mtu,
                                "LookupResponse: keeping tighter existing path_mtu_lookup value"
                            );
                        }
                        other => {
                            // The one carrier with no release path, so this is
                            // the one write that carries a deadline.
                            map.insert(
                                fips_addr,
                                crate::upper::tun::PathMtuEntry::learned(path_mtu, now_ms),
                            );
                            debug!(
                                target = %self.peer_display_name(&target),
                                fips_addr = %fips_addr,
                                path_mtu = path_mtu,
                                prior = ?other,
                                map_len = map.len(),
                                "Wrote path_mtu_lookup from discovery LookupResponse"
                            );
                        }
                    },
                    Err(e) => {
                        warn!(
                            target = %self.peer_display_name(&target),
                            fips_addr = %fips_addr,
                            path_mtu = path_mtu,
                            error = %e,
                            "path_mtu_lookup write lock poisoned; clamp will not see this update"
                        );
                    }
                }
            }

            // Clean up pending lookup tracking
            self.pending_lookups.remove(&target);

            // If an established session exists, reset the warmup counter.
            let n = self.config().node.session.coords_warmup_packets;
            if let Some(entry) = self.sessions.get_mut(&target)
                && entry.is_established()
            {
                entry.set_coords_warmup_remaining(n);
                debug!(
                    dest = %self.peer_display_name(&target),
                    warmup_packets = n,
                    "Reset coords warmup after discovery for existing session"
                );
            }

            // If we have pending TUN packets for this target, retry session
            // initiation. The coord_cache now has coords, so find_next_hop()
            // should succeed.
            if let Some(packets) = self.pending_tun_packets.get(&target) {
                debug!(
                    dest = %self.peer_display_name(&target),
                    queued_packets = packets.len(),
                    "Retrying queued packets after discovery"
                );
                self.retry_session_after_discovery(target).await;
            }
        }
    }

    /// Generate and send a LookupResponse when we are the target.
    async fn send_lookup_response(&mut self, request: &LookupRequest) {
        let our_coords = self.tree_state().my_coords().clone();

        // Sign proof: Identity::sign hashes with SHA-256 internally
        let proof_data =
            LookupResponse::proof_bytes(request.request_id, &request.target, &our_coords);
        let proof = self.identity().sign(&proof_data);

        let mut response =
            LookupResponse::new(request.request_id, request.target, our_coords, proof);

        // Route toward origin via reverse path.
        let next_hop_addr = if let Some(recent) = self.recent_requests.get(&request.request_id) {
            recent.from_peer
        } else {
            // Fallback: try greedy tree routing toward origin
            match self.find_next_hop(&request.origin) {
                Some(peer) => *peer.node_addr(),
                None => {
                    debug!(
                        origin = %self.peer_display_name(&request.origin),
                        "Cannot route LookupResponse: no reverse path or tree route to origin"
                    );
                    self.metrics()
                        .discovery
                        .record_reject(DiscoveryReject::RespNoRoute);
                    return;
                }
            }
        };

        // Fold our outgoing-link MTU into path_mtu so the target-edge link
        // appears in the bottleneck calculation. Without this, the response
        // leaves the target with path_mtu = u16::MAX and only intermediate
        // transits min-fold; the target's first reverse-path hop is missed.
        self.apply_outgoing_link_mtu_to_response(&mut response, &next_hop_addr);

        debug!(
            request_id = request.request_id,
            origin = %self.peer_display_name(&request.origin),
            next_hop = %self.peer_display_name(&next_hop_addr),
            path_mtu = response.path_mtu,
            "Sending LookupResponse"
        );

        let encoded = response.encode();
        if let Err(e) = self
            .send_encrypted_link_message(&next_hop_addr, &encoded)
            .await
        {
            debug!(
                next_hop = %self.peer_display_name(&next_hop_addr),
                error = %e,
                "Failed to send LookupResponse"
            );
        }
    }

    /// Forward a LookupRequest to eligible peers.
    ///
    /// Primary path: tree peers (parent + children) whose bloom filter
    /// contains the target. Restricting to tree peers follows the spanning
    /// tree partition, producing a single directed path.
    ///
    /// Fallback: if no tree peer's bloom matches, try non-tree peers whose
    /// bloom contains the target. This recovers from dead ends caused by
    /// stale bloom filters, tree restructuring, or transit node failures.
    async fn forward_lookup_request(&mut self, mut request: LookupRequest) {
        if !request.forward() {
            return;
        }

        // Collect tree peers whose bloom filter contains the target
        let forward_to: Vec<NodeAddr> = self
            .peers
            .iter()
            .filter(|(addr, peer)| self.is_tree_peer(addr) && peer.may_reach(&request.target))
            .map(|(addr, _)| *addr)
            .collect();

        // Fallback: if no tree peer matches, try non-tree bloom-matching peers
        let (forward_to, used_fallback) = if forward_to.is_empty() {
            let fallback: Vec<NodeAddr> = self
                .peers
                .iter()
                .filter(|(addr, peer)| !self.is_tree_peer(addr) && peer.may_reach(&request.target))
                .map(|(addr, _)| *addr)
                .collect();
            if fallback.is_empty() {
                self.metrics().discovery.req_no_tree_peer.inc();
                trace!(
                    request_id = request.request_id,
                    "No eligible peers to forward LookupRequest"
                );
                return;
            }
            (fallback, true)
        } else {
            (forward_to, false)
        };

        if used_fallback {
            self.metrics().discovery.req_fallback_forwarded.inc();
            debug!(
                request_id = request.request_id,
                target = %self.peer_display_name(&request.target),
                ttl = request.ttl,
                peer_count = forward_to.len(),
                "Forwarding LookupRequest via non-tree fallback"
            );
        } else {
            debug!(
                request_id = request.request_id,
                target = %self.peer_display_name(&request.target),
                ttl = request.ttl,
                peer_count = forward_to.len(),
                "Forwarding LookupRequest"
            );
        }

        let encoded = request.encode();

        for peer_addr in forward_to {
            if let Err(e) = self.send_encrypted_link_message(&peer_addr, &encoded).await {
                debug!(
                    peer = %self.peer_display_name(&peer_addr),
                    error = %e,
                    "Failed to forward LookupRequest to peer"
                );
            }
        }
    }

    /// Initiate a discovery lookup for a target node.
    ///
    /// Creates a LookupRequest and sends it to tree peers whose bloom
    /// filters contain the target. Returns the number of peers sent to.
    /// The originator does NOT record the request_id in recent_requests,
    /// so when the response arrives, it's recognized as "our request".
    /// It records the id on the target's pending entry instead, which is
    /// what the response path correlates against; recording it here rather
    /// than in the callers keeps "if a request went out, its id is
    /// recorded" true for every caller.
    pub(in crate::node) async fn initiate_lookup(&mut self, target: &NodeAddr, ttl: u8) -> usize {
        self.metrics().discovery.req_initiated.inc();

        let origin = *self.node_addr();
        let origin_coords = self.tree_state().my_coords().clone();
        let request = LookupRequest::generate(*target, origin, origin_coords, ttl, 0);

        let now_ms = Self::now_ms();
        self.pending_lookups
            .entry(*target)
            .or_insert_with(|| PendingLookup::new(now_ms))
            .record(request.request_id);

        // Send only to tree peers whose bloom filter contains the target
        let peer_addrs: Vec<NodeAddr> = self
            .peers
            .iter()
            .filter(|(addr, peer)| self.is_tree_peer(addr) && peer.may_reach(target))
            .map(|(addr, _)| *addr)
            .collect();

        let peer_count = peer_addrs.len();

        debug!(
            request_id = request.request_id,
            target = %self.peer_display_name(target),
            ttl = ttl,
            peer_count = peer_count,
            total_peers = self.peers.len(),
            "Discovery lookup initiated"
        );

        if peer_count == 0 {
            return 0;
        }

        let encoded = request.encode();

        for peer_addr in peer_addrs {
            if let Err(e) = self.send_encrypted_link_message(&peer_addr, &encoded).await {
                debug!(
                    peer = %self.peer_display_name(&peer_addr),
                    error = %e,
                    "Failed to send LookupRequest to peer"
                );
            }
        }

        peer_count
    }

    /// Initiate a discovery lookup if one is not already pending for this target.
    ///
    /// Checks: pending dedup, post-failure backoff (off by default), bloom
    /// filter pre-check. If all pass, sends the first attempt's LookupRequest.
    /// Subsequent attempts (with fresh request_ids) are scheduled by
    /// [`Self::check_pending_lookups`] when each attempt's per-attempt timeout
    /// expires, using the sequence in `node.discovery.attempt_timeouts_secs`.
    pub(in crate::node) async fn maybe_initiate_lookup(&mut self, dest: &NodeAddr) {
        let now_ms = Self::now_ms();

        // Dedup: any pending lookup means we are already trying.
        if self.pending_lookups.contains_key(dest) {
            self.metrics().discovery.req_deduplicated.inc();
            debug!(
                target_node = %self.peer_display_name(dest),
                "Discovery lookup deduplicated, already pending"
            );
            return;
        }

        // Optional post-failure suppression. Defaults are 0/0 (inert);
        // operators can opt in by setting `node.discovery.backoff_*_secs`.
        if self.discovery_backoff.is_suppressed(dest) {
            self.metrics().discovery.req_backoff_suppressed.inc();
            debug!(
                target_node = %self.peer_display_name(dest),
                failures = self.discovery_backoff.failure_count(dest),
                "Discovery lookup suppressed by backoff"
            );
            return;
        }

        // Bloom filter pre-check: if no peer's filter contains the target,
        // it's not in the mesh — skip the lookup and record as failure.
        let reachable = self.peers.values().any(|peer| peer.may_reach(dest));
        if !reachable {
            self.metrics().discovery.req_bloom_miss.inc();
            self.discovery_backoff.record_failure(dest);
            debug!(
                target_node = %self.peer_display_name(dest),
                "Discovery skipped, target not in any peer bloom filter"
            );
            return;
        }

        self.pending_lookups
            .insert(*dest, PendingLookup::new(now_ms));
        let ttl = self.config().node.discovery.ttl;
        let sent = self.initiate_lookup(dest, ttl).await;

        // If no tree peers had the target, fail immediately
        if sent == 0 {
            self.pending_lookups.remove(dest);
            self.discovery_backoff.record_failure(dest);
            debug!(
                target_node = %self.peer_display_name(dest),
                "Discovery failed, no tree peers with bloom match"
            );
        }
    }

    /// Check pending lookups for next-attempt or final timeout.
    ///
    /// Called periodically from the tick handler. The lookup state machine
    /// runs through `node.discovery.attempt_timeouts_secs` (default
    /// `[1, 2, 4, 8]`): each entry is the deadline for one attempt. When the
    /// current attempt's deadline elapses:
    /// - If more entries remain: send the next attempt with a fresh
    ///   `request_id`.
    /// - Otherwise: declare the destination unreachable, drop queued packets,
    ///   and emit ICMPv6 destination-unreachable for each.
    pub(in crate::node) async fn check_pending_lookups(&mut self, now_ms: u64) {
        let timeouts = self.config().node.discovery.attempt_timeouts_secs.clone();
        let max_attempts = timeouts.len() as u8;

        // Collect targets needing action
        let mut to_retry: Vec<NodeAddr> = Vec::new();
        let mut to_timeout: Vec<NodeAddr> = Vec::new();

        for (&target, entry) in &self.pending_lookups {
            let attempt_idx = (entry.attempt as usize).saturating_sub(1);
            let attempt_timeout_ms = timeouts.get(attempt_idx).copied().unwrap_or(0) * 1000;
            if now_ms.saturating_sub(entry.last_sent_ms) >= attempt_timeout_ms {
                if entry.attempt >= max_attempts {
                    to_timeout.push(target);
                } else {
                    to_retry.push(target);
                }
            }
        }

        // Process retries
        for target in to_retry {
            if let Some(entry) = self.pending_lookups.get_mut(&target) {
                entry.attempt += 1;
                entry.last_sent_ms = now_ms;
                let attempt = entry.attempt;

                let ttl = self.config().node.discovery.ttl;
                let sent = self.initiate_lookup(&target, ttl).await;
                if sent > 0 {
                    debug!(
                        target_node = %self.peer_display_name(&target),
                        attempt = attempt,
                        "Discovery retry sent"
                    );
                }
            }
        }

        // Process timeouts
        for addr in to_timeout {
            self.metrics().discovery.resp_timed_out.inc();
            self.pending_lookups.remove(&addr);

            // Record failure for optional backoff
            self.discovery_backoff.record_failure(&addr);
            let failures = self.discovery_backoff.failure_count(&addr);

            let queued = self.pending_tun_packets.remove(&addr);
            let pkt_count = queued.as_ref().map_or(0, |p| p.len());
            info!(
                target_node = %self.peer_display_name(&addr),
                queued_packets = pkt_count,
                failures = failures,
                "Discovery lookup timed out, destination unreachable"
            );
            if let Some(packets) = queued {
                for pkt in &packets {
                    self.send_icmpv6_dest_unreachable(pkt);
                }
            }
        }
    }

    /// Reset discovery backoff on topology changes.
    pub(in crate::node) fn reset_discovery_backoff(&mut self) {
        if !self.discovery_backoff.is_empty() {
            debug!(
                entries = self.discovery_backoff.entry_count(),
                "Resetting discovery backoff on topology change"
            );
            self.discovery_backoff.reset_all();
        }
    }

    /// Remove expired entries from the recent_requests cache.
    pub(in crate::node) fn purge_expired_requests(&mut self, current_time_ms: u64) {
        let expiry_ms = self.config().node.discovery.recent_expiry_secs * 1000;
        let recent = &mut self.recent_requests;
        recent.retain(|_, entry| !entry.is_expired(current_time_ms, expiry_ms));
        self.recent_by_peer.retain(|_, ids| {
            ids.retain(|id| recent.contains_key(id));
            !ids.is_empty()
        });
    }

    /// Evict from the dedup cache if admitting one more request would put
    /// this peer over its share, or the cache over its capacity.
    ///
    /// The share is the cache divided by the current link-peer count, with
    /// [`MIN_RECENT_PER_PEER`] as a floor, so it tracks the peer count
    /// instead of being pinned to a number that a many-peer node outgrows.
    fn make_room_for_request(&mut self, from: &NodeAddr) {
        let share =
            (MAX_RECENT_DISCOVERY_REQUESTS / self.peers.len().max(1)).max(MIN_RECENT_PER_PEER);

        let over_share = self
            .recent_by_peer
            .get(from)
            .is_some_and(|ids| ids.len() >= share);
        let victim = if over_share {
            Some(*from)
        } else if self.recent_requests.len() >= MAX_RECENT_DISCOVERY_REQUESTS {
            // Never take from a peer under its share: charge the fattest.
            self.recent_by_peer
                .iter()
                .max_by_key(|(_, ids)| ids.len())
                .map(|(peer, _)| *peer)
        } else {
            return;
        };

        let Some(victim) = victim else { return };
        let Some(ids) = self.recent_by_peer.get_mut(&victim) else {
            return;
        };
        let Some(evicted) = ids.pop_front() else {
            return;
        };
        if ids.is_empty() {
            self.recent_by_peer.remove(&victim);
        }
        self.recent_requests.remove(&evicted);
        self.metrics().discovery.req_dedup_evicted.inc();
        debug!(
            request_id = evicted,
            evicted_from = %self.peer_display_name(&victim),
            admitting = %self.peer_display_name(from),
            share = share,
            "Discovery dedup cache full, evicting the oldest entry to make room"
        );
    }

    /// Min-fold our outgoing-link MTU into a LookupResponse's `path_mtu`.
    ///
    /// Used at both transit-side reverse-path forward and at the target's
    /// own send_lookup_response. The link MTU we apply is the MTU of the
    /// transport+addr we'll use to deliver the response toward `next_hop`.
    /// No-op when `next_hop` is not a directly-connected peer or its
    /// transport is not registered.
    pub(in crate::node) fn apply_outgoing_link_mtu_to_response(
        &self,
        response: &mut LookupResponse,
        next_hop: &NodeAddr,
    ) {
        if let Some(peer) = self.peers.get(next_hop)
            && let Some(tid) = peer.transport_id()
            && let Some(transport) = self.transports.get(&tid)
        {
            let link_mtu = if let Some(addr) = peer.current_addr() {
                transport.link_mtu(addr)
            } else {
                transport.mtu()
            };
            response.path_mtu = response.path_mtu.min(link_mtu);
        }
    }

    /// Seed `path_mtu_lookup` for a directly-connected peer.
    ///
    /// Called when an FMP link-layer peer is promoted to active. The seed
    /// value is the local outgoing-link MTU on the peer's transport, which
    /// is the actual link constraint for direct-link traffic. Stored only
    /// when no tighter value exists: discovery's reverse-path bottleneck
    /// or MMP `MtuExceeded` reactive learning take precedence when smaller.
    ///
    /// Without this seed, configured/auto-connect peers (which establish
    /// sessions without going through the discovery Lookup flow) leave
    /// `path_mtu_lookup` empty for their FipsAddress, causing
    /// `per_flow_max_mss` to fall back to the global ceiling and the
    /// SYN-time TCP MSS clamp to over-estimate the effective path.
    pub(in crate::node) fn seed_path_mtu_for_link_peer(
        &self,
        peer_addr: &NodeAddr,
        transport_id: TransportId,
        addr: &TransportAddr,
    ) {
        let Some(transport) = self.transports.get(&transport_id) else {
            debug!(
                peer = %self.peer_display_name(peer_addr),
                transport_id = %transport_id,
                "seed_path_mtu_for_link_peer: transport not registered, skipping seed"
            );
            return;
        };
        let link_mtu = transport.link_mtu(addr);
        // A locally derived MTU is deliberately exempt from the actionable
        // floor, so this seeds the value either way, and a narrow link is not
        // by itself worth reporting: BLE negotiates its MTU per connection and
        // lands below the floor routinely, where the tight clamp the seed
        // produces is exactly what the flow needs. Warn only where the link
        // admits no TCP payload byte at all, since there the SYN-time clamp
        // has nothing usable to derive and drops the peer onto the
        // conservative fallback ceiling for as long as the link stands.
        if crate::upper::icmp::mss_ceiling(link_mtu) == 0 {
            warn!(
                peer = %self.peer_display_name(peer_addr),
                link_mtu = link_mtu,
                "Link MTU leaves no room for a TCP payload byte; TCP to this peer \
                 will not work until the link or the transport's mtu setting changes"
            );
        }
        let fips_addr = crate::FipsAddress::from_node_addr(peer_addr);
        let Ok(mut map) = self.path_mtu_lookup.write() else {
            warn!(
                peer = %self.peer_display_name(peer_addr),
                "seed_path_mtu_for_link_peer: path_mtu_lookup write lock poisoned"
            );
            return;
        };
        match map.get(&fips_addr).copied() {
            Some(existing) if existing.mtu <= link_mtu => {
                // Keep the tighter learned value; never loosen the clamp.
                debug!(
                    peer = %self.peer_display_name(peer_addr),
                    fips_addr = %fips_addr,
                    link_mtu = link_mtu,
                    existing = existing.mtu,
                    "seed_path_mtu_for_link_peer: keeping tighter existing value"
                );
            }
            other => {
                // Held, not expiring: this describes a link this node can see
                // for itself, and it is released when the link goes.
                map.insert(fips_addr, crate::upper::tun::PathMtuEntry::held(link_mtu));
                debug!(
                    peer = %self.peer_display_name(peer_addr),
                    fips_addr = %fips_addr,
                    link_mtu = link_mtu,
                    prior = ?other,
                    map_len = map.len(),
                    "seed_path_mtu_for_link_peer: wrote link MTU"
                );
            }
        }
    }
}

/// How many outstanding `request_id`s one pending lookup remembers.
///
/// Bounds the per-target correlator at eight u64s. The retry ladder
/// (`node.discovery.attempt_timeouts_secs`) is operator configuration and
/// can be longer than this, so the recorder evicts the oldest id rather
/// than refusing the newest: dropping the newest would discard the id most
/// likely to be answered and fail a healthy lookup. Raising this costs
/// eight bytes per extra attempt on every pending target and widens the
/// set of ids a late response may still match; lowering it means a reply
/// to an early attempt on a long ladder is dropped as unsolicited.
const MAX_RECORDED_IDS: usize = 8;

/// Tracks a pending discovery lookup with retry state.
pub struct PendingLookup {
    /// When the lookup was first initiated.
    pub initiated_ms: u64,
    /// When the last attempt was sent.
    pub last_sent_ms: u64,
    /// Current attempt number (1 = initial, 2 = first retry, ...).
    pub attempt: u8,
    /// `request_id`s issued for this target, oldest first, capped at
    /// [`MAX_RECORDED_IDS`]. A response is only acted on when it carries
    /// one of these, which is what makes the accept path solicited. The
    /// entry itself is dropped at ladder timeout, so this set needs no
    /// expiry of its own.
    pub ids: Vec<u64>,
}

impl PendingLookup {
    pub fn new(now_ms: u64) -> Self {
        Self {
            initiated_ms: now_ms,
            last_sent_ms: now_ms,
            attempt: 1,
            ids: Vec::new(),
        }
    }

    /// Remember a `request_id` we just put on the wire for this target.
    pub fn record(&mut self, request_id: u64) {
        if self.ids.contains(&request_id) {
            return;
        }
        if self.ids.len() >= MAX_RECORDED_IDS {
            self.ids.remove(0);
        }
        self.ids.push(request_id);
    }

    /// Whether `request_id` is one this node issued for this target.
    pub fn matches(&self, request_id: u64) -> bool {
        self.ids.contains(&request_id)
    }
}
