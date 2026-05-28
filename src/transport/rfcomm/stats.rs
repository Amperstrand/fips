//! RFCOMM transport statistics.

use portable_atomic::{AtomicU64, Ordering};
use serde::Serialize;

/// Statistics for an RFCOMM transport instance.
///
/// Uses atomic counters for lock-free updates from per-connection
/// receive loops and the send path concurrently.
pub struct RfcommStats {
    pub connections_established: AtomicU64,
    pub connections_accepted: AtomicU64,
    pub connections_closed: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_recv: AtomicU64,
    pub send_errors: AtomicU64,
    pub recv_errors: AtomicU64,
    pub framing_errors: AtomicU64,
}

impl RfcommStats {
    /// Create a new stats instance with all counters at zero.
    pub fn new() -> Self {
        Self {
            connections_established: AtomicU64::new(0),
            connections_accepted: AtomicU64::new(0),
            connections_closed: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_recv: AtomicU64::new(0),
            send_errors: AtomicU64::new(0),
            recv_errors: AtomicU64::new(0),
            framing_errors: AtomicU64::new(0),
        }
    }

    /// Record a successful outbound connection.
    pub fn record_connection_established(&self) {
        self.connections_established
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a successful inbound connection.
    pub fn record_connection_accepted(&self) {
        self.connections_accepted.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a connection closure.
    pub fn record_connection_closed(&self) {
        self.connections_closed.fetch_add(1, Ordering::Relaxed);
    }

    /// Record bytes sent.
    pub fn record_send(&self, bytes: usize) {
        self.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record bytes received.
    pub fn record_recv(&self, bytes: usize) {
        self.bytes_recv.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record a send error.
    pub fn record_send_error(&self) {
        self.send_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a receive error.
    pub fn record_recv_error(&self) {
        self.recv_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a framing error.
    pub fn record_framing_error(&self) {
        self.framing_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Take a snapshot of all counters.
    pub fn snapshot(&self) -> RfcommStatsSnapshot {
        RfcommStatsSnapshot {
            connections_established: self.connections_established.load(Ordering::Relaxed),
            connections_accepted: self.connections_accepted.load(Ordering::Relaxed),
            connections_closed: self.connections_closed.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bytes_recv: self.bytes_recv.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            recv_errors: self.recv_errors.load(Ordering::Relaxed),
            framing_errors: self.framing_errors.load(Ordering::Relaxed),
        }
    }
}

impl Default for RfcommStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Point-in-time snapshot of RFCOMM stats (non-atomic, copyable).
#[derive(Clone, Debug, Default, Serialize)]
pub struct RfcommStatsSnapshot {
    pub connections_established: u64,
    pub connections_accepted: u64,
    pub connections_closed: u64,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub send_errors: u64,
    pub recv_errors: u64,
    pub framing_errors: u64,
}
