//! Timeout management for stale handshake connections, idle sessions,
//! and handshake message resend scheduling.

use crate::node::Node;
use crate::peer::HandshakeState;
use crate::transport::LinkId;
use tracing::{debug, info, warn};

impl Node {
    /// Check for timed-out handshake connections and clean them up.
    ///
    /// Called periodically by the RX event loop. Removes connections that have
    /// been idle longer than the configured handshake timeout or are in Failed state.
    pub(in crate::node) async fn check_timeouts(&mut self) {
        if self.connections.is_empty() {
            return;
        }

        let now_ms = Self::now_ms();
        let timeout_ms = self.config().node.rate_limit.handshake_timeout_secs * 1000;

        let stale: Vec<LinkId> = self
            .connections
            .iter()
            .filter(|(_, conn)| conn.is_timed_out(now_ms, timeout_ms) || conn.is_failed())
            .map(|(link_id, _)| *link_id)
            .collect();

        for link_id in stale {
            // Log and schedule retry before cleanup (need connection state)
            if let Some(conn) = self.connections.get(&link_id) {
                let direction = conn.direction();
                let idle_ms = conn.idle_time(now_ms);
                if conn.is_failed() {
                    debug!(
                        link_id = %link_id,
                        direction = %direction,
                        "Failed handshake connection cleaned up"
                    );
                } else {
                    debug!(
                        link_id = %link_id,
                        direction = %direction,
                        idle_secs = idle_ms / 1000,
                        "Stale handshake connection timed out"
                    );
                }

                // Schedule retry for failed outbound auto-connect peers
                if conn.is_outbound()
                    && let Some(identity) = conn.expected_identity()
                {
                    self.schedule_retry(*identity.node_addr(), now_ms);
                }
            }
            self.cleanup_stale_connection(link_id, now_ms).await;
        }
    }

    /// Remove a handshake connection and all associated state.
    ///
    /// Frees the session index, removes pending_outbound entry, closes the
    /// underlying transport connection, and cleans up the link and address
    /// mapping. Does not log — callers provide context-appropriate log
    /// messages.
    pub(in crate::node) async fn cleanup_stale_connection(
        &mut self,
        link_id: LinkId,
        _now_ms: u64,
    ) {
        let conn = match self.connections.remove(&link_id) {
            Some(c) => c,
            None => return,
        };
        let transport_id = conn.transport_id();

        // Free session index and pending_outbound if allocated
        if let Some(idx) = conn.our_index() {
            if let Some(tid) = conn.transport_id() {
                self.pending_outbound.remove(&(tid, idx.as_u32()));
            }
            let _ = self.index_allocator.free(idx);
        }

        // Tear down the transport connection, not just the node-side state.
        // A connection-oriented transport otherwise keeps the socket, its
        // pool entry and its inbound-slot accounting alive after the node
        // has forgotten the handshake that socket belonged to, so a peer
        // that sends msg1 and then stalls holds an inbound slot forever.
        // Closing twice is harmless: every `close_connection` implementation
        // is `if let Some(conn) = pool.remove(addr)` and the connectionless
        // default is a no-op, so the handshake paths that already close and
        // then drop a link cannot be disturbed by this.
        if let Some(link) = self.links.get(&link_id) {
            let tid = link.transport_id();
            let addr = link.remote_addr().clone();
            if let Some(transport) = self.transports.get(&tid) {
                transport.close_connection(&addr).await;
            }
        }

        // Remove link and addr_to_link
        self.remove_link(&link_id);
        if let Some(transport_id) = transport_id {
            self.cleanup_bootstrap_transport_if_unused(transport_id);
        }
    }

    /// Resend handshake messages for pending connections.
    ///
    /// For outbound connections in SentMsg1 state, resends the stored msg1
    /// with exponential backoff. Called periodically from the RX event loop.
    pub(in crate::node) async fn resend_pending_handshakes(&mut self, now_ms: u64) {
        if self.connections.is_empty() {
            return;
        }

        let max_resends = self.config().node.rate_limit.handshake_max_resends;
        let interval_ms = self.config().node.rate_limit.handshake_resend_interval_ms;
        let backoff = self.config().node.rate_limit.handshake_resend_backoff;

        // Collect resend candidates: outbound, in SentMsg1, with stored msg1,
        // under max resends, and past the scheduled time.
        let candidates: Vec<(LinkId, Vec<u8>)> = self
            .connections
            .iter()
            .filter(|(_, conn)| {
                conn.is_outbound()
                    && conn.handshake_state() == HandshakeState::SentMsg1
                    && conn.resend_count() < max_resends
                    && conn.next_resend_at_ms() > 0
                    && now_ms >= conn.next_resend_at_ms()
            })
            .filter_map(|(link_id, conn)| {
                conn.handshake_msg1().map(|msg1| (*link_id, msg1.to_vec()))
            })
            .collect();

        for (link_id, msg1_bytes) in candidates {
            // Get transport and address info from the connection
            let (transport_id, remote_addr) = match self.connections.get(&link_id) {
                Some(conn) => match (conn.transport_id(), conn.source_addr()) {
                    (Some(tid), Some(addr)) => (tid, addr.clone()),
                    _ => continue,
                },
                None => continue,
            };

            // Send the stored msg1
            let sent = if let Some(transport) = self.transports.get(&transport_id) {
                match transport.send(&remote_addr, &msg1_bytes).await {
                    Ok(_) => true,
                    Err(e) => {
                        debug!(
                            link_id = %link_id,
                            error = %e,
                            "Handshake msg1 resend failed"
                        );
                        false
                    }
                }
            } else {
                false
            };

            if sent && let Some(conn) = self.connections.get_mut(&link_id) {
                let count = conn.resend_count() + 1;
                let next = now_ms + (interval_ms as f64 * backoff.powi(count as i32)) as u64;
                conn.record_resend(next);
                debug!(
                    link_id = %link_id,
                    resend = count,
                    "Resent handshake msg1"
                );
            }
        }
    }

    /// Resend session-layer handshake messages and timeout stale handshakes.
    ///
    /// For sessions in Initiating or AwaitingMsg3 state:
    /// - If the handshake has exceeded the timeout window, remove the session.
    /// - If a resend is due and under max resends, resend the stored payload
    ///   wrapped in a fresh SessionDatagram (so routing can adapt).
    pub(in crate::node) async fn resend_pending_session_handshakes(&mut self, now_ms: u64) {
        if self.sessions.is_empty() {
            return;
        }

        let timeout_ms = self.config().node.rate_limit.handshake_timeout_secs * 1000;
        let max_resends = self.config().node.rate_limit.handshake_max_resends;
        let interval_ms = self.config().node.rate_limit.handshake_resend_interval_ms;
        let backoff = self.config().node.rate_limit.handshake_resend_backoff;
        let ttl = self.config().node.session.default_ttl;

        // First pass: find timed-out sessions to remove
        let timed_out: Vec<crate::NodeAddr> = self
            .sessions
            .iter()
            .filter(|(_, entry)| {
                !entry.is_established() && now_ms.saturating_sub(entry.last_activity()) > timeout_ms
            })
            .map(|(addr, _)| *addr)
            .collect();

        for addr in &timed_out {
            let name = self.peer_display_name(addr);
            info!(dest = %name, "Session handshake timed out, removing");
            self.sessions.remove(addr);
            self.pending_tun_packets.remove(addr);
            self.path_mtu_lookup_release(addr);
        }

        // Second pass: collect resend candidates
        let my_addr = *self.node_addr();
        let candidates: Vec<(crate::NodeAddr, Vec<u8>)> = self
            .sessions
            .iter()
            .filter(|(_, entry)| {
                !entry.is_established()
                    && entry.handshake_payload().is_some()
                    && entry.resend_count() < max_resends
                    && entry.next_resend_at_ms() > 0
                    && now_ms >= entry.next_resend_at_ms()
            })
            .map(|(addr, entry)| (*addr, entry.handshake_payload().unwrap().to_vec()))
            .collect();

        for (dest_addr, payload) in candidates {
            use crate::protocol::SessionDatagram;

            let mut datagram = SessionDatagram::new(my_addr, dest_addr, payload).with_ttl(ttl);
            let sent = match self.send_session_datagram(&mut datagram).await {
                Ok(_) => true,
                Err(e) => {
                    debug!(
                        dest = %self.peer_display_name(&dest_addr),
                        error = %e,
                        "Session handshake resend failed"
                    );
                    false
                }
            };

            if sent && let Some(entry) = self.sessions.get_mut(&dest_addr) {
                let count = entry.resend_count() + 1;
                let next = now_ms + (interval_ms as f64 * backoff.powi(count as i32)) as u64;
                entry.record_resend(next);
                debug!(
                    dest = %self.peer_display_name(&dest_addr),
                    resend = count,
                    "Resent session handshake"
                );
            }
        }
    }

    /// Remove established sessions that have been idle too long.
    ///
    /// Only targets sessions in the Established state. Initiating/AwaitingMsg3
    /// sessions are handled by the handshake timeout.
    pub(in crate::node) fn purge_idle_sessions(&mut self, now_ms: u64) {
        let timeout_ms = self.config().node.session.idle_timeout_secs * 1000;
        if timeout_ms == 0 {
            return; // disabled
        }

        let idle: Vec<_> = self
            .sessions
            .iter()
            .filter(|(_, entry)| {
                entry.is_established() && now_ms.saturating_sub(entry.last_activity()) > timeout_ms
            })
            .map(|(addr, _)| *addr)
            .collect();

        for addr in idle {
            // Compute display name before removing the session
            let name = self.peer_display_name(&addr);

            // Log MMP teardown metrics before removing the session
            if let Some(entry) = self.sessions.get(&addr)
                && let Some(mmp) = entry.mmp()
            {
                Self::log_session_mmp_teardown(&name, mmp);
            }
            self.sessions.remove(&addr);
            self.pending_tun_packets.remove(&addr);
            self.path_mtu_lookup_release(&addr);
            debug!(
                dest = %name,
                idle_secs = timeout_ms / 1000,
                "Idle session removed (no application data)"
            );
        }
    }

    /// Expire `path_mtu_lookup` entries that nothing else will ever release.
    ///
    /// The three callers of `path_mtu_lookup_release` all fire on session
    /// state, so an entry written by the discovery `LookupResponse` carrier
    /// for a destination this node never opens a session with has no release
    /// path at all. Keep-tighter then makes one such response permanent: a
    /// `path_mtu` of 256 pins that destination's SYN-time MSS clamp at 119
    /// bytes until the process restarts. Only those entries carry a
    /// `learned_ms`, and only they are expired here.
    ///
    /// The deadline is the coordinate cache's own TTL, because the same
    /// `LookupResponse` writes both stores: the clamp cannot outlive the
    /// route it was learned with, and shortening `node.cache.coord_ttl_secs`
    /// shortens this with it. A TTL of zero disables the pass, matching
    /// `purge_idle_sessions`.
    pub(in crate::node) fn purge_expired_path_mtu(&mut self, now_ms: u64) {
        use crate::upper::tun::PathMtuEntry;

        let ttl_ms = self.config().node.cache.coord_ttl_secs * 1000;
        if ttl_ms == 0 {
            return; // disabled
        }
        let stale = |e: &PathMtuEntry| {
            e.learned_ms
                .is_some_and(|at| now_ms.saturating_sub(at) >= ttl_ms)
        };

        // Read-scan first: an ordinary tick expires nothing, and the TUN
        // reader and writer take this lock on every packet.
        let expired: Vec<crate::FipsAddress> = match self.path_mtu_lookup.read() {
            Ok(map) => map
                .iter()
                .filter(|(_, e)| stale(e))
                .map(|(a, _)| *a)
                .collect(),
            Err(e) => {
                warn!(error = %e, "path_mtu_lookup read lock poisoned; expiry pass skipped");
                return;
            }
        };
        if expired.is_empty() {
            return;
        }

        match self.path_mtu_lookup.write() {
            Ok(mut map) => {
                for addr in &expired {
                    // Re-test under the write lock. The read guard was dropped
                    // before this one was taken, so a fresh value may have
                    // landed in between; without this the pass would delete a
                    // value that was just learned.
                    if map.get(addr).is_some_and(stale) {
                        map.remove(addr);
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "path_mtu_lookup write lock poisoned; entries not expired");
                return;
            }
        }

        // Restore what local configuration knows, the same way
        // `path_mtu_lookup_release` does. A tighter remote claim overwrites a
        // direct peer's link MTU under keep-tighter, so an expired entry may
        // be sitting on top of a seed, and a bare removal would drop that peer
        // to the conservative ceiling until its link re-handshakes.
        let gone: std::collections::HashSet<crate::FipsAddress> = expired.iter().copied().collect();
        let seeds: Vec<(
            crate::NodeAddr,
            crate::transport::TransportId,
            crate::transport::TransportAddr,
        )> = self
            .peers
            .iter()
            .filter(|(addr, _)| gone.contains(&crate::FipsAddress::from_node_addr(addr)))
            .filter_map(|(addr, p)| Some((*addr, p.transport_id()?, p.current_addr()?.clone())))
            .collect();
        for (addr, tid, taddr) in seeds {
            self.seed_path_mtu_for_link_peer(&addr, tid, &taddr);
        }

        debug!(
            expired = expired.len(),
            "Expired remote-learned path_mtu_lookup entries"
        );
    }
}
