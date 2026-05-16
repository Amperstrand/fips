//! Node-level reconnection and retry logic.
//!
//! Provides exponential backoff for peer reconnection attempts following
//! link failures. Uses a separate backoff state per peer address.
//!
//! Design: Backoff follows the exponential pattern from TCP RTO (RFC 6298 §5),
//! but is applied at the mesh reconnection layer rather than the transport layer.
//! BLE transport has its own additional backoff (see `transport/ble/backoff.rs`).
//!
//! Reconnect vs. retry:
//! - **Retry**: Initial connection attempt or failure during handshake.
//!   Subject to max_retries cap.
//! - **Reconnect**: Re-establishment of a previously-active peer connection.
//!   Bypasses max_retries (a reconnecting peer should keep trying indefinitely,
//!   similar to TCP's persistent connection attempts per RFC 1122 §4.2.3.5).

use super::Node;
use crate::PeerIdentity;
use crate::config::{PeerAddress, PeerConfig};
use crate::identity::NodeAddr;
use tracing::{debug, info, warn};

// MAX_BACKOFF_MS is now derived from config: node.retry.max_backoff_secs * 1000

/// Tracks retry state for a peer across connection attempts.
pub struct RetryState {
    /// The peer config to use for initiating retries.
    /// Contains transport addresses and auto_reconnect flag.
    pub peer_config: PeerConfig,

    /// Number of retries attempted so far.
    /// Used as the exponent in backoff: delay = base * 2^retry_count.
    pub retry_count: u32,

    /// Timestamp (Unix ms) when the next retry should be attempted.
    /// Set to `now_ms + backoff_ms()` after each attempt.
    pub retry_after_ms: u64,

    /// Whether this is an auto-reconnect (unlimited retries, ignores max_retries).
    /// Reconnect entries persist until the peer connects or the entry expires.
    /// See RFC 1122 §4.2.3.5 for the TCP analogue (persistent connections).
    pub reconnect: bool,

    /// Whether this reconnect was triggered proactively (H13 SRTT threshold)
    /// vs. reactively (link failure). Proactive reconnects use lighter backoff
    /// since the link was healthy — we're cycling the channel, not recovering
    /// from a failure.
    pub proactive: bool,

    /// Optional absolute expiry for this retry entry (Unix ms).
    ///
    /// When set, retries are dropped after this point even if reconnect logic
    /// would otherwise continue. This prevents stale discovered peers from
    /// retrying forever after they have left the mesh.
    pub expires_at_ms: Option<u64>,
}

impl RetryState {
    /// Create a new retry state for a peer.
    pub fn new(peer_config: PeerConfig) -> Self {
        Self {
            peer_config,
            retry_count: 0,
            retry_after_ms: 0,
            reconnect: false,
            proactive: false,
            expires_at_ms: None,
        }
    }

    /// Calculate the backoff delay in milliseconds for the current retry count.
    ///
    /// Uses exponential backoff: `base_interval_ms * 2^retry_count`,
    /// capped at `max_backoff_ms`.
    ///
    /// This follows the same exponential pattern as TCP RTO backoff (RFC 6298 §5),
    /// where each subsequent retry doubles the wait time.
    ///
    /// Note: For proactive reconnects (H13), this compounds with any previous
    /// failure reconnects since retry_count is preserved. See issue #108 for
    /// the proposal to use a separate, lighter backoff for proactive reconnects.
    pub fn backoff_ms(
        &self,
        base_interval_ms: u64,
        max_backoff_ms: u64,
        proactive_backoff_ms: u64,
    ) -> u64 {
        if self.proactive {
            return proactive_backoff_ms;
        }
        // Normal exponential backoff for failure reconnects
        let multiplier = 1u64.checked_shl(self.retry_count).unwrap_or(u64::MAX);
        base_interval_ms
            .saturating_mul(multiplier)
            .min(max_backoff_ms)
    }
}

impl Node {
    /// Store a synthetic PeerConfig for a runtime-discovered peer if it has no
    /// static config entry. The synthetic config enables reconnect after
    /// transport disconnect. The transport_name and transport_addr are captured
    /// from the active link so the reconnect can redial the same endpoint.
    pub(in crate::node) fn store_discovered_peer_config(
        &mut self,
        node_addr: NodeAddr,
        peer_identity: &PeerIdentity,
        transport_name: &str,
        transport_addr: &crate::transport::TransportAddr,
    ) {
        let already_in_static_config = self.config.auto_connect_peers().any(|pc| {
            PeerIdentity::from_npub(&pc.npub)
                .map(|id| *id.node_addr() == node_addr)
                .unwrap_or(false)
        });

        if already_in_static_config {
            return;
        }

        let npub = peer_identity.npub();
        let peer_address = PeerAddress::new(transport_name, transport_addr.to_string());

        let peer_config = PeerConfig {
            npub,
            alias: None,
            addresses: vec![peer_address],
            auto_reconnect: true,
            connect_policy: crate::config::ConnectPolicy::AutoConnect,
            via_nostr: false,
        };

        self.discovered_peer_configs.insert(node_addr, peer_config);

        if self.discovered_peer_configs.len() > super::DISCOVERED_PEER_CONFIGS_MAX {
            let oldest_key = self.discovered_peer_configs.keys().next().copied();
            if let Some(key) = oldest_key {
                self.discovered_peer_configs.remove(&key);
            }
        }
    }

    /// Schedule an initial connection retry (not a reconnect).
    ///
    /// Unlike reconnects, retries ARE subject to max_retries cap.
    /// Used for failed handshake attempts and initial connection failures.
    ///
    /// Only schedules if the peer is an auto-connect peer and max retries
    /// have not been exhausted (unless `reconnect` is true, which retries
    /// indefinitely). Does nothing if the peer is already connected or has
    /// a connection in progress.
    pub(super) fn schedule_retry(&mut self, node_addr: NodeAddr, now_ms: u64) {
        let retry_cfg = &self.config.node.retry;
        let max_retries = retry_cfg.max_retries;
        if max_retries == 0 {
            return;
        }

        // Don't retry if peer is already connected
        if self.peers.contains_key(&node_addr) {
            return;
        }

        let base_interval_ms = retry_cfg.base_interval_secs * 1000;
        let max_backoff_ms = retry_cfg.max_backoff_secs * 1000;
        let peer_name = self.peer_display_name(&node_addr);

        if let Some(state) = self.retry_pending.get_mut(&node_addr) {
            // Already tracking — increment
            state.retry_count += 1;
            if !state.reconnect && state.retry_count > max_retries {
                info!(
                    peer = %peer_name,
                    attempts = state.retry_count,
                    "Max retries exhausted, giving up on peer"
                );
                self.retry_pending.remove(&node_addr);
                return;
            }
            let delay = state.backoff_ms(base_interval_ms, max_backoff_ms, 0);
            state.retry_after_ms = now_ms + delay;
            debug!(
                peer = %peer_name,
                retry = state.retry_count,
                reconnect = state.reconnect,
                delay_secs = delay / 1000,
                "Scheduling connection retry"
            );
        } else {
            // First failure — find the matching PeerConfig
            let peer_config = self
                .config
                .auto_connect_peers()
                .find(|pc| {
                    PeerIdentity::from_npub(&pc.npub)
                        .map(|id| *id.node_addr() == node_addr)
                        .unwrap_or(false)
                })
                .cloned()
                .or_else(|| self.discovered_peer_configs.get(&node_addr).cloned());

            if let Some(pc) = peer_config {
                let mut state = RetryState::new(pc);
                state.retry_count = 1;
                state.reconnect = true;
                let delay = state.backoff_ms(base_interval_ms, max_backoff_ms, 0);
                state.retry_after_ms = now_ms + delay;
                debug!(
                    peer = %self.peer_display_name(&node_addr),
                    delay_secs = delay / 1000,
                    "First connection attempt failed, scheduling retry"
                );
                self.retry_pending.insert(node_addr, state);
            }
            // If not found in auto_connect_peers, no retry (one-shot connection)
        }
    }

    /// Schedule a reconnection attempt for a peer.
    ///
    /// If an entry already exists in retry_pending, increments retry_count and
    /// preserves the existing backoff (no reset). This means proactive reconnects
    /// (H13) compound with any previous failure history.
    ///
    /// The `reconnect` flag indicates this was an established peer that lost
    /// connectivity. Reconnect entries bypass max_retries — we keep trying
    /// indefinitely, similar to TCP persistent connections (RFC 1122 §4.2.3.5).
    ///
    /// Looks up the peer in auto-connect config and checks `auto_reconnect`.
    /// If enabled, feeds the peer into the retry system with unlimited retries.
    ///
    /// If a retry entry already exists (e.g. from a previous failed handshake
    /// attempt during an earlier reconnect cycle), the existing retry count is
    /// preserved and incremented rather than reset to zero. This ensures
    /// exponential backoff accumulates across repeated link-dead events instead
    /// of resetting to the base interval on every peer removal.
    pub(super) fn schedule_reconnect(&mut self, node_addr: NodeAddr, now_ms: u64) {
        let peer_config = self
            .config
            .auto_connect_peers()
            .find(|pc| {
                PeerIdentity::from_npub(&pc.npub)
                    .map(|id| *id.node_addr() == node_addr)
                    .unwrap_or(false)
            })
            .cloned()
            .or_else(|| self.discovered_peer_configs.get(&node_addr).cloned());

        let Some(pc) = peer_config else {
            return; // Not an auto-connect peer, no reconnect
        };

        if !pc.auto_reconnect {
            debug!(
                peer = %self.peer_display_name(&node_addr),
                "Auto-reconnect disabled for peer, skipping"
            );
            return;
        }

        let base_interval_ms = self.config.node.retry.base_interval_secs * 1000;
        let max_backoff_ms = self.config.node.retry.max_backoff_secs * 1000;
        let peer_name = self.peer_display_name(&node_addr);

        // If we already have accumulated backoff from previous failed attempts,
        // preserve and bump it rather than resetting to zero. This prevents the
        // exponential backoff from being discarded on each link-dead cycle.
        if let Some(state) = self.retry_pending.get_mut(&node_addr) {
            state.reconnect = true;
            state.retry_count += 1;
            let delay = state.backoff_ms(base_interval_ms, max_backoff_ms, 0);
            state.retry_after_ms = now_ms + delay;
            debug!(
                peer = %peer_name,
                retry = state.retry_count,
                delay_secs = delay / 1000,
                "Scheduling auto-reconnect after link-dead removal (backoff preserved)"
            );
            return;
        }

        let mut state = RetryState::new(pc);
        state.reconnect = true;
        let delay = state.backoff_ms(base_interval_ms, max_backoff_ms, 0);
        state.retry_after_ms = now_ms + delay;

        debug!(
            peer = %peer_name,
            delay_secs = delay / 1000,
            "Scheduling auto-reconnect after link-dead removal"
        );

        self.retry_pending.insert(node_addr, state);
    }

    /// Schedule a proactive reconnection attempt for a peer.
    ///
    /// Similar to schedule_reconnect but sets the `proactive` flag for lighter
    /// backoff. Used by H13 SRTT threshold reconnect — the link was healthy,
    /// we're cycling the BLE channel to get fresh L2CAP credits.
    pub(super) fn schedule_proactive_reconnect(
        &mut self,
        node_addr: NodeAddr,
        now_ms: u64,
        proactive_backoff_ms: u64,
    ) {
        let peer_config = self
            .config
            .auto_connect_peers()
            .find(|pc| {
                PeerIdentity::from_npub(&pc.npub)
                    .map(|id| *id.node_addr() == node_addr)
                    .unwrap_or(false)
            })
            .cloned()
            .or_else(|| self.discovered_peer_configs.get(&node_addr).cloned());

        let Some(pc) = peer_config else {
            return;
        };

        if !pc.auto_reconnect {
            return;
        }

        let base_interval_ms = self.config.node.retry.base_interval_secs * 1000;
        let max_backoff_ms = self.config.node.retry.max_backoff_secs * 1000;
        let peer_name = self.peer_display_name(&node_addr);

        if let Some(state) = self.retry_pending.get_mut(&node_addr) {
            // Existing entry — mark as proactive for lighter backoff
            state.reconnect = true;
            state.proactive = true;
            // Don't increment retry_count for proactive — it wasn't a failure
            let delay = state.backoff_ms(base_interval_ms, max_backoff_ms, proactive_backoff_ms);
            state.retry_after_ms = now_ms + delay;
            debug!(
                peer = %peer_name,
                delay_secs = delay / 1000,
                "Scheduling proactive reconnect (lighter backoff)"
            );
            return;
        }

        let mut state = RetryState::new(pc);
        state.reconnect = true;
        state.proactive = true;
        let delay = state.backoff_ms(base_interval_ms, max_backoff_ms, proactive_backoff_ms);
        state.retry_after_ms = now_ms + delay;

        debug!(
            peer = %peer_name,
            delay_secs = delay / 1000,
            "Scheduling proactive reconnect"
        );

        self.retry_pending.insert(node_addr, state);
    }

    /// Process pending retries whose time has arrived.
    ///
    /// Called every node.tick_interval_secs (default: 1s) from the RX loop.
    /// Checks if any pending retry has expired and initiates reconnection.
    /// If a peer connects inbound while pending, the stale entry is removed.
    ///
    /// For each due retry, initiates a fresh connection attempt. The retry
    /// entry stays in `retry_pending` until the connection succeeds (cleared
    /// in `promote_connection`) or max retries are exhausted (cleared in
    /// `schedule_retry`).
    pub(super) async fn process_pending_retries(&mut self, now_ms: u64) {
        if self.retry_pending.is_empty() {
            return;
        }

        let expired: Vec<NodeAddr> = self
            .retry_pending
            .iter()
            .filter_map(|(addr, state)| {
                state
                    .expires_at_ms
                    .filter(|expires_at_ms| now_ms >= *expires_at_ms)
                    .map(|_| *addr)
            })
            .collect();
        for node_addr in expired {
            self.retry_pending.remove(&node_addr);
            info!(
                peer = %self.peer_display_name(&node_addr),
                "Retry window expired, dropping pending retry state"
            );
        }
        if self.retry_pending.is_empty() {
            return;
        }

        // Collect retries that are due
        let due: Vec<NodeAddr> = self
            .retry_pending
            .iter()
            .filter(|(_, state)| now_ms >= state.retry_after_ms)
            .map(|(addr, _)| *addr)
            .collect();

        for node_addr in due {
            // Peer may have connected inbound while we waited
            if self.peers.contains_key(&node_addr) {
                self.retry_pending.remove(&node_addr);
                continue;
            }

            let state = match self.retry_pending.get(&node_addr) {
                Some(s) => s,
                None => continue,
            };

            debug!(
                peer = %self.peer_display_name(&node_addr),
                retry = state.retry_count,
                "Attempting connection retry"
            );

            let peer_config = state.peer_config.clone();

            match self.initiate_peer_connection(&peer_config).await {
                Ok(()) => {
                    // Push retry_after_ms past the handshake timeout window so
                    // we don't re-fire on the next tick. If the handshake
                    // succeeds, promote_connection() clears retry_pending. If
                    // it times out, check_timeouts() calls schedule_retry()
                    // which bumps the counter and applies proper backoff.
                    let hs_timeout_ms = self.config.node.rate_limit.handshake_timeout_secs * 1000;
                    if let Some(state) = self.retry_pending.get_mut(&node_addr) {
                        state.retry_after_ms = now_ms + hs_timeout_ms;
                    }
                    debug!(
                        peer = %self.peer_display_name(&node_addr),
                        "Retry connection initiated, suppressing re-fire for {}s",
                        self.config.node.rate_limit.handshake_timeout_secs,
                    );
                }
                Err(e) => {
                    warn!(
                        peer = %self.peer_display_name(&node_addr),
                        error = %e,
                        "Retry connection initiation failed"
                    );
                    // Immediate failure counts as an attempt — schedule next retry
                    // (reconnect flag is preserved on existing retry_pending entry)
                    self.schedule_retry(node_addr, now_ms);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PeerConfig;

    const TEST_MAX_BACKOFF_MS: u64 = 300_000;

    #[test]
    fn test_backoff_exponential() {
        let state = RetryState {
            peer_config: PeerConfig::default(),
            retry_count: 0,
            retry_after_ms: 0,
            reconnect: false,
            proactive: false,
            expires_at_ms: None,
        };
        // base = 5000ms
        assert_eq!(state.backoff_ms(5000, TEST_MAX_BACKOFF_MS, 0), 5000); // 5s * 2^0

        let state = RetryState {
            retry_count: 1,
            ..state
        };
        assert_eq!(state.backoff_ms(5000, TEST_MAX_BACKOFF_MS, 0), 10_000); // 5s * 2^1

        let state = RetryState {
            retry_count: 2,
            ..state
        };
        assert_eq!(state.backoff_ms(5000, TEST_MAX_BACKOFF_MS, 0), 20_000); // 5s * 2^2

        let state = RetryState {
            retry_count: 3,
            ..state
        };
        assert_eq!(state.backoff_ms(5000, TEST_MAX_BACKOFF_MS, 0), 40_000); // 5s * 2^3

        let state = RetryState {
            retry_count: 4,
            ..state
        };
        assert_eq!(state.backoff_ms(5000, TEST_MAX_BACKOFF_MS, 0), 80_000); // 5s * 2^4
    }

    #[test]
    fn test_backoff_cap() {
        let state = RetryState {
            peer_config: PeerConfig::default(),
            retry_count: 20, // 2^20 * 5000 would be huge
            retry_after_ms: 0,
            reconnect: false,
            proactive: false,
            expires_at_ms: None,
        };
        assert_eq!(
            state.backoff_ms(5000, TEST_MAX_BACKOFF_MS, 0),
            TEST_MAX_BACKOFF_MS
        );
    }

    #[test]
    fn test_backoff_zero_base() {
        let state = RetryState {
            peer_config: PeerConfig::default(),
            retry_count: 3,
            retry_after_ms: 0,
            reconnect: false,
            proactive: false,
            expires_at_ms: None,
        };
        assert_eq!(state.backoff_ms(0, TEST_MAX_BACKOFF_MS, 0), 0);
    }

    #[test]
    fn test_backoff_proactive() {
        let state = RetryState {
            peer_config: PeerConfig::default(),
            retry_count: 5,
            retry_after_ms: 0,
            reconnect: true,
            proactive: true,
            expires_at_ms: None,
        };
        // Proactive: returns the configured proactive_backoff_ms regardless of retry_count
        assert_eq!(state.backoff_ms(5000, TEST_MAX_BACKOFF_MS, 10_000), 10_000);

        // Proactive with different configured value
        let state = RetryState {
            peer_config: PeerConfig::default(),
            retry_count: 5,
            retry_after_ms: 0,
            reconnect: true,
            proactive: true,
            expires_at_ms: None,
        };
        assert_eq!(state.backoff_ms(5000, TEST_MAX_BACKOFF_MS, 5_000), 5_000);
    }
}
