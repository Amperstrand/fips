//! BLE L2CAP Transport Implementation
//!
//! Provides BLE-based transport for FIPS peer communication using L2CAP
//! Connection-Oriented Channels (CoC) in SeqPacket mode. While SeqPacket
//! nominally preserves message boundaries, some BLE controllers (notably
//! ESP32) may coalesce back-to-back sends into a single recv(). The
//! receive loop handles this by splitting on FMP frame boundaries.
//!
//! ## Architecture
//!
//! Transport logic (pool, discovery, lifecycle) is separated from the
//! BlueZ/bluer stack via the `BleIo` trait. `BluerIo` provides the real
//! implementation (behind `cfg(feature = "ble")`); `MockBleIo` provides
//! an in-memory test double for CI without hardware.
//!
//! ## Connection Pool
//!
//! BLE hardware limits concurrent connections (typically 4-10). The pool
//! enforces a configurable maximum (default 7) with priority eviction:
//! static (configured) peers get priority over discovered peers.

pub mod addr;
pub mod discovery;
pub mod io;
pub mod pool;
pub mod stats;


use super::{
    ConnectionState, DiscoveredPeer, PacketTx, ReceivedPacket, Transport, TransportAddr,
    TransportError, TransportId, TransportState, TransportType,
};
use crate::config::BleConfig;
use crate::identity::NodeAddr;
use addr::BleAddr;
use discovery::DiscoveryBuffer;
use io::{BleConnectionRolePolicy, BleIo, BleScanner, BleStream};
use pool::{BleConnection, ConnectionPool};
use stats::BleStats;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use secp256k1::XOnlyPublicKey;
use tracing::{debug, info, trace, warn};

/// Default FIPS L2CAP PSM (Protocol Service Multiplexer).
///
/// 0x0085 (133) is in the dynamic range (0x0080-0x00FF).
pub const DEFAULT_PSM: u16 = 0x0085;

/// Concrete BLE transport type for use in TransportHandle.
///
/// Production builds with the `ble` feature use `BluerIo` (real BlueZ stack).
/// Production builds with `ble-macos` feature use `BluestIo` (real CoreBluetooth stack).
/// Test builds and builds without BLE features use `MockBleIo`.
#[cfg(all(feature = "ble", not(test)))]
pub type DefaultBleTransport = BleTransport<io::BluerIo>;

#[cfg(all(feature = "ble-macos", not(test)))]
pub type DefaultBleTransport = BleTransport<io::BluestIo>;

#[cfg(any(all(not(feature = "ble"), not(feature = "ble-macos")), test))]
pub type DefaultBleTransport = BleTransport<io::MockBleIo>;


// ============================================================================
// BLE Transport
// ============================================================================

/// BLE transport for FIPS.
///
/// Provides connection-oriented, reliable delivery over BLE L2CAP CoC.
/// Each peer has its own L2CAP connection; the pool enforces hardware
/// connection limits with priority eviction.
pub struct BleTransport<I: BleIo> {
    /// Unique transport identifier.
    transport_id: TransportId,
    /// Optional instance name.
    name: Option<String>,
    /// Configuration.
    config: BleConfig,
    /// Current state.
    state: TransportState,
    /// BLE I/O implementation (BluerIo or MockBleIo).
    io: Arc<I>,
    /// Established connection pool.
    pool: Arc<Mutex<ConnectionPool<Arc<I::Stream>>>>,
    /// Pending connection attempts.
    connecting: Arc<Mutex<HashMap<TransportAddr, ConnectingEntry>>>,
    /// Channel for delivering received packets to Node.
    packet_tx: PacketTx,
    /// Accept loop task handle.
    accept_task: Option<JoinHandle<()>>,
    /// Combined scan + probe loop task handle.
    scan_probe_task: Option<JoinHandle<()>>,
    /// Discovery buffer for discovered peers.
    discovery_buffer: Arc<DiscoveryBuffer>,
    /// Transport statistics.
    stats: Arc<BleStats>,
    /// Our public key for pre-handshake identity exchange.
    ///
    /// BLE advertisements carry only the FIPS UUID, not the pubkey.
    /// After L2CAP connection, both sides exchange `[0x00][role:1][pubkey:32]`
    /// so the node layer can initiate the IK handshake.
    /// Temporary — removed when FMP switches to XX.
    local_pubkey: Option<[u8; 32]>,
}

/// A pending background connection attempt.
struct ConnectingEntry {
    task: JoinHandle<()>,
}

impl<I: BleIo> BleTransport<I> {
    /// Create a new BLE transport.
    pub fn new(
        transport_id: TransportId,
        name: Option<String>,
        config: BleConfig,
        io: I,
        packet_tx: PacketTx,
    ) -> Self {
        let max_conns = config.max_connections();
        Self {
            transport_id,
            name,
            config,
            state: TransportState::Configured,
            io: Arc::new(io),
            pool: Arc::new(Mutex::new(ConnectionPool::new(max_conns))),
            connecting: Arc::new(Mutex::new(HashMap::new())),
            packet_tx,
            accept_task: None,
            scan_probe_task: None,
            discovery_buffer: Arc::new(DiscoveryBuffer::new(transport_id)),
            stats: Arc::new(BleStats::new()),
            local_pubkey: None,
        }
    }

    /// Get the instance name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Get the transport statistics.
    pub fn stats(&self) -> &Arc<BleStats> {
        &self.stats
    }

    /// Get the I/O implementation (for test injection).
    pub fn io(&self) -> &Arc<I> {
        &self.io
    }

    /// Set the local public key for pre-handshake identity exchange.
    ///
    /// Must be called before `start_async()`. Without this, BLE
    /// connections skip the pubkey exchange and discovered peers
    /// won't have identity information for auto-connect.
    pub fn set_local_pubkey(&mut self, pubkey: [u8; 32]) {
        self.local_pubkey = Some(pubkey);
    }

    /// Start the transport asynchronously.
    pub async fn start_async(&mut self) -> Result<(), TransportError> {
        if !self.state.can_start() {
            return Err(TransportError::AlreadyStarted);
        }
        self.state = TransportState::Starting;

        let psm = self.config.psm();
        let adapter = self.io.adapter_name().to_string();
        let role_policy = self.io.role_policy();

        // Pre-compute local NodeAddr for cross-probe tie-breaking
        let local_node_addr = self.local_pubkey.and_then(|pk| {
            XOnlyPublicKey::from_slice(&pk)
                .ok()
                .map(|xonly| NodeAddr::from_pubkey(&xonly))
        });

        // Start L2CAP listener for inbound connections
        if self.config.accept_connections() {
            if role_policy.supports_inbound() {
                match self.io.listen(psm).await {
                    Ok(acceptor) => {
                        let pool = Arc::clone(&self.pool);
                        let packet_tx = self.packet_tx.clone();
                        let transport_id = self.transport_id;
                        let stats = Arc::clone(&self.stats);
                        let max_conns = self.config.max_connections();

                        self.accept_task = Some(tokio::spawn(accept_loop(
                            acceptor,
                            pool,
                            packet_tx,
                            transport_id,
                            stats,
                            max_conns,
                            self.local_pubkey,
                            Arc::clone(&self.discovery_buffer),
                            local_node_addr,
                            role_policy,
                        )));
                        debug!(adapter = %adapter, psm = psm, "BLE accept loop started");
                    }
                    Err(e) => {
                        warn!(adapter = %adapter, error = %e, "failed to start BLE listener");
                        if !matches!(e, TransportError::NotSupported(_)) {
                            self.state = TransportState::Failed;
                            return Err(e);
                        }
                    }
                }
            } else {
                warn!(adapter = %adapter, role_policy = ?role_policy, "BLE backend cannot accept inbound connections; skipping listen");
            }
        }

        // Start continuous advertising
        if self.config.advertise() {
            if role_policy.supports_inbound() {
                if let Err(e) = self.io.start_advertising().await {
                    warn!(adapter = %adapter, error = %e, "failed to start BLE advertising");
                } else {
                    self.stats.record_advertisement();
                    debug!(adapter = %adapter, "BLE advertising started (continuous)");
                }
            } else {
                warn!(adapter = %adapter, role_policy = ?role_policy, "BLE backend cannot complete inbound BLE role; skipping advertising");
            }
        }

        // Start combined scan + probe loop
        if self.config.scan() {
            if role_policy.supports_outbound() {
                match self.io.start_scanning().await {
                    Ok(scanner) => {
                        self.scan_probe_task = Some(tokio::spawn(scan_probe_loop::<I>(
                            scanner,
                            Arc::clone(&self.io),
                            Arc::clone(&self.pool),
                            Arc::clone(&self.discovery_buffer),
                            Arc::clone(&self.stats),
                            self.local_pubkey,
                            self.config.psm(),
                            self.config.connect_timeout_ms(),
                            self.config.probe_cooldown_secs(),
                            local_node_addr,
                            self.packet_tx.clone(),
                            self.transport_id,
                            role_policy,
                        )));
                        debug!(adapter = %adapter, "BLE scan+probe loop started");
                    }
                    Err(e) => {
                        warn!(adapter = %adapter, error = %e, "failed to start BLE scanning");
                    }
                }
            } else {
                warn!(adapter = %adapter, role_policy = ?role_policy, "BLE backend cannot initiate outbound connections; skipping scan");
            }
        }

        self.state = TransportState::Up;
        info!(adapter = %adapter, psm = psm, "BLE transport started");
        Ok(())
    }

    /// Stop the transport asynchronously.
    pub async fn stop_async(&mut self) -> Result<(), TransportError> {
        // Stop advertising
        let _ = self.io.stop_advertising().await;

        // Abort accept loop
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }

        // Abort scan+probe loop
        if let Some(task) = self.scan_probe_task.take() {
            task.abort();
        }

        // Drain connecting pool
        {
            let mut connecting = self.connecting.lock().await;
            for (_, entry) in connecting.drain() {
                entry.task.abort();
            }
        }

        // Drain established connections (recv tasks aborted via Drop)
        {
            let mut pool = self.pool.lock().await;
            for addr in pool.addrs() {
                pool.remove(&addr);
            }
        }

        self.state = TransportState::Down;
        info!("BLE transport stopped");
        Ok(())
    }

    /// Send data to a remote BLE address.
    ///
    /// If no connection exists, triggers a background connect and fails
    /// fast. The next send retry (typically 1s later for handshake msg1)
    /// will find the connection established. This avoids blocking the
    /// event loop on L2CAP connect (up to 10s).
    pub async fn send_async(
        &self,
        addr: &TransportAddr,
        data: &[u8],
    ) -> Result<usize, TransportError> {
        let pool = self.pool.lock().await;
        let conn = match pool.get(addr) {
            Some(c) => c,
            None => {
                // Drop pool lock before triggering background connect
                drop(pool);
                // Fire-and-forget: connect_async spawns a background task
                let _ = self.connect_async(addr).await;
                return Err(TransportError::SendFailed("not connected".into()));
            }
        };

        // MTU check
        let mtu = conn.effective_mtu() as usize;
        if data.len() > mtu {
            self.stats.record_mtu_exceeded();
            return Err(TransportError::MtuExceeded {
                packet_size: data.len(),
                mtu: mtu as u16,
            });
        }

        match conn.stream.send(data).await {
            Ok(()) => {
                debug!(addr = %addr, bytes = data.len(), "BLE send: packet sent");
                self.stats.record_send(data.len());
                Ok(data.len())
            }
            Err(e) => {
                self.stats.record_send_error();
                // Drop pool lock before removing to avoid deadlock
                drop(pool);
                let mut pool = self.pool.lock().await;
                pool.remove(addr);
                warn!(addr = %addr, error = %e, "BLE send failed, connection removed");
                Err(e)
            }
        }
    }

    /// Connect to a remote BLE device inline (blocking the caller).
    ///
    /// Not used in normal operation (send_async fails fast instead).
    /// Retained for manual debugging / testing scenarios.
    #[allow(dead_code)]
    async fn connect_inline(&self, addr: &TransportAddr) -> Result<(), TransportError> {
        if !self.io.role_policy().supports_outbound() {
            return Err(TransportError::NotSupported(
                "BLE backend cannot initiate outbound connections".into(),
            ));
        }

        let ble_addr = BleAddr::parse(
            addr.as_str()
                .ok_or_else(|| TransportError::InvalidAddress("not valid UTF-8".into()))?,
        )?;

        let psm = self.config.psm();
        let timeout_ms = self.config.connect_timeout_ms();

        let stream = match tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            self.io.connect(&ble_addr, psm),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                debug!(addr = %addr, error = %e, "BLE connect-on-send failed");
                return Err(TransportError::ConnectionRefused);
            }
            Err(_) => {
                self.stats.record_connect_timeout();
                debug!(addr = %addr, "BLE connect-on-send timeout");
                return Err(TransportError::Timeout);
            }
        };

        // Pre-handshake pubkey exchange (temporary, pre-XX)
        if let Some(ref our_pubkey) = self.local_pubkey {
            match pubkey_exchange(
                &stream,
                our_pubkey,
                PubkeyExchangeRole::Initiator,
                self.io.role_policy(),
            )
            .await
            {
                Ok(peer) => {
                    debug!(addr = %addr, "BLE outbound pubkey exchange complete");
                    self.discovery_buffer
                        .add_peer_with_pubkey(&ble_addr, peer.pubkey);
                }
                Err(e) => {
                    warn!(addr = %addr, error = %e, "BLE outbound pubkey exchange failed");
                    return Err(e);
                }
            }
        }

        self.promote_connection(addr, &ble_addr, stream).await
    }

    /// Promote a newly established stream into the connection pool.
    ///
    /// Spawns the receive loop and inserts into the pool with eviction.
    async fn promote_connection(
        &self,
        addr: &TransportAddr,
        ble_addr: &BleAddr,
        stream: I::Stream,
    ) -> Result<(), TransportError> {
        let send_mtu = stream.send_mtu();
        let recv_mtu = stream.recv_mtu();
        let stream = Arc::new(stream);

        let recv_task = tokio::spawn(receive_loop(
            Arc::clone(&stream),
            addr.clone(),
            Arc::clone(&self.pool),
            self.packet_tx.clone(),
            self.transport_id,
            Arc::clone(&self.stats),
            recv_mtu,
        ));

        let conn = BleConnection {
            stream,
            recv_task: Some(recv_task),
            send_mtu,
            recv_mtu,
            established_at: tokio::time::Instant::now(),
            is_static: false,
            addr: ble_addr.clone(),
        };

        let mut pool = self.pool.lock().await;
        match pool.insert(addr.clone(), conn) {
            Ok(Some(evicted)) => {
                self.stats.record_pool_eviction();
                debug!(addr = %addr, evicted = %evicted, "BLE connection established (evicted peer)");
            }
            Ok(None) => {
                debug!(addr = %addr, "BLE connection established");
            }
            Err(e) => {
                warn!(addr = %addr, error = %e, "BLE pool full, connection dropped");
                self.stats.record_connection_rejected();
                return Err(TransportError::SendFailed("pool full".into()));
            }
        }
        self.stats.record_connection_established();
        Ok(())
    }

    /// Initiate a non-blocking connection to a remote BLE device.
    ///
    /// Spawns a background task that connects with timeout and promotes
    /// to the pool on success. Poll `connection_state_sync()` to check.
    pub async fn connect_async(&self, addr: &TransportAddr) -> Result<(), TransportError> {
        if !self.io.role_policy().supports_outbound() {
            return Err(TransportError::NotSupported(
                "BLE backend cannot initiate outbound connections".into(),
            ));
        }

        // Already connected?
        {
            let pool = self.pool.lock().await;
            if pool.contains(addr) {
                return Ok(());
            }
        }

        // Already connecting?
        {
            let connecting = self.connecting.lock().await;
            if connecting.contains_key(addr) {
                return Ok(());
            }
        }

        let ble_addr = BleAddr::parse(
            addr.as_str()
                .ok_or_else(|| TransportError::InvalidAddress("not valid UTF-8".into()))?,
        )?;

        let io = Arc::clone(&self.io);
        let pool = Arc::clone(&self.pool);
        let connecting = Arc::clone(&self.connecting);
        let packet_tx = self.packet_tx.clone();
        let transport_id = self.transport_id;
        let stats = Arc::clone(&self.stats);
        let psm = self.config.psm();
        let timeout_ms = self.config.connect_timeout_ms();
        let addr_clone = addr.clone();
        let local_pubkey = self.local_pubkey;
        let discovery_buffer = Arc::clone(&self.discovery_buffer);
        let role_policy = self.io.role_policy();

        let task = tokio::spawn(async move {
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                io.connect(&ble_addr, psm),
            )
            .await;

            // Remove from connecting pool
            connecting.lock().await.remove(&addr_clone);

            match result {
                Ok(Ok(stream)) => {
                    // Pre-handshake pubkey exchange (temporary, pre-XX)
                    if let Some(ref our_pubkey) = local_pubkey {
                        match pubkey_exchange(
                            &stream,
                            our_pubkey,
                            PubkeyExchangeRole::Initiator,
                            role_policy,
                        )
                        .await
                        {
                            Ok(peer) => {
                                debug!(addr = %addr_clone, "BLE outbound pubkey exchange complete");
                                discovery_buffer
                                    .add_peer_with_pubkey(&ble_addr, peer.pubkey);
                            }
                            Err(e) => {
                                warn!(
                                    addr = %addr_clone, error = %e,
                                    "BLE outbound pubkey exchange failed"
                                );
                                return;
                            }
                        }
                    }

                    let send_mtu = stream.send_mtu();
                    let recv_mtu = stream.recv_mtu();
                    let stream = Arc::new(stream);

                    let recv_task = tokio::spawn(receive_loop(
                        Arc::clone(&stream),
                        addr_clone.clone(),
                        Arc::clone(&pool),
                        packet_tx,
                        transport_id,
                        Arc::clone(&stats),
                        recv_mtu,
                    ));

                    let conn = BleConnection {
                        stream,
                        recv_task: Some(recv_task),
                        send_mtu,
                        recv_mtu,
                        established_at: tokio::time::Instant::now(),
                        is_static: false,
                        addr: ble_addr,
                    };

                    let mut pool = pool.lock().await;
                    match pool.insert(addr_clone.clone(), conn) {
                        Ok(Some(evicted)) => {
                            stats.record_pool_eviction();
                            debug!(addr = %addr_clone, evicted = %evicted, "BLE connection established (evicted peer)");
                        }
                        Ok(None) => {
                            debug!(addr = %addr_clone, "BLE connection established");
                        }
                        Err(e) => {
                            warn!(addr = %addr_clone, error = %e, "BLE pool full, connection dropped");
                            stats.record_connection_rejected();
                            return;
                        }
                    }
                    stats.record_connection_established();
                }
                Ok(Err(e)) => {
                    debug!(addr = %addr_clone, error = %e, "BLE connect failed");
                }
                Err(_) => {
                    stats.record_connect_timeout();
                    debug!(addr = %addr_clone, "BLE connect timeout");
                }
            }
        });

        self.connecting
            .lock()
            .await
            .insert(addr.clone(), ConnectingEntry { task });

        Ok(())
    }

    /// Query the state of a connection attempt.
    pub fn connection_state_sync(&self, addr: &TransportAddr) -> ConnectionState {
        // Check established pool (try_lock to avoid blocking)
        if let Ok(pool) = self.pool.try_lock()
            && pool.contains(addr)
        {
            return ConnectionState::Connected;
        }

        // Check connecting pool
        if let Ok(connecting) = self.connecting.try_lock()
            && connecting.contains_key(addr)
        {
            return ConnectionState::Connecting;
        }

        ConnectionState::None
    }

    /// Close a specific connection.
    pub async fn close_connection_async(&self, addr: &TransportAddr) {
        let mut pool = self.pool.lock().await;
        if let Some(conn) = pool.remove(addr) {
            debug!(addr = %addr, "BLE connection closed");
            drop(conn); // recv_task aborted via Drop
        }
    }

    /// Get the link MTU for a specific address.
    pub fn link_mtu(&self, addr: &TransportAddr) -> u16 {
        if let Ok(pool) = self.pool.try_lock()
            && let Some(conn) = pool.get(addr)
        {
            return conn.effective_mtu();
        }
        self.config.mtu()
    }
}

impl<I: BleIo> Transport for BleTransport<I> {
    fn transport_id(&self) -> TransportId {
        self.transport_id
    }

    fn transport_type(&self) -> &TransportType {
        &TransportType::BLE
    }

    fn state(&self) -> TransportState {
        self.state
    }

    fn mtu(&self) -> u16 {
        self.config.mtu()
    }

    fn link_mtu(&self, addr: &TransportAddr) -> u16 {
        self.link_mtu(addr)
    }

    fn start(&mut self) -> Result<(), TransportError> {
        Err(TransportError::NotSupported(
            "use start_async() for BLE transport".into(),
        ))
    }

    fn stop(&mut self) -> Result<(), TransportError> {
        Err(TransportError::NotSupported(
            "use stop_async() for BLE transport".into(),
        ))
    }

    fn send(&self, _addr: &TransportAddr, _data: &[u8]) -> Result<(), TransportError> {
        Err(TransportError::NotSupported(
            "use send_async() for BLE transport".into(),
        ))
    }

    fn discover(&self) -> Result<Vec<DiscoveredPeer>, TransportError> {
        Ok(self.discovery_buffer.take())
    }

    fn auto_connect(&self) -> bool {
        self.config.auto_connect()
    }

    fn accept_connections(&self) -> bool {
        self.config.accept_connections()
    }

    fn close_connection(&self, _addr: &TransportAddr) {
        // use close_connection_async()
    }
}

// ============================================================================
// Background Tasks
// ============================================================================

/// Pre-handshake pubkey exchange prefix byte.
///
/// Distinguishes the identity exchange from FMP packets (version ≥ 0x01).
/// Temporary — removed when FMP switches from IK to XX handshake.
const PUBKEY_EXCHANGE_PREFIX: u8 = 0x00;

/// Pre-handshake pubkey exchange message size: `[0x00][role:1][pubkey:32]`.
const PUBKEY_EXCHANGE_SIZE: usize = 34;
const PUBKEY_EXCHANGE_ROLE_INDEX: usize = 1;
const PUBKEY_EXCHANGE_PUBKEY_START: usize = 2;

/// Timeout for pubkey exchange recv (seconds).
///
/// The peer should respond in milliseconds; 5s is generous. Without this,
/// a peer that connects but never sends its pubkey blocks the calling task
/// forever — killing scan_probe_loop, accept_loop, or the event loop.
const PUBKEY_EXCHANGE_TIMEOUT_SECS: u64 = 5;

#[cfg(feature = "ble-macos")]
const ALLOW_OUTBOUND_PROBE_TIMEOUT_PROMOTION: bool = false;

#[cfg(not(feature = "ble-macos"))]
const ALLOW_OUTBOUND_PROBE_TIMEOUT_PROMOTION: bool = false;

const ROLE_MISMATCH_BACKOFF_SECS: u64 = 600;

async fn recv_pubkey_frame<S: BleStream>(
    stream: &S,
    buf: &mut [u8; PUBKEY_EXCHANGE_SIZE],
    role: PubkeyExchangeRole,
) -> Result<(), TransportError> {
    let timeout = std::time::Duration::from_secs(PUBKEY_EXCHANGE_TIMEOUT_SECS);
    let role_name = match role {
        PubkeyExchangeRole::Initiator => "initiator",
        PubkeyExchangeRole::Responder => "responder",
    };

    tokio::time::timeout(timeout, async {
        let mut offset = 0;
        while offset < PUBKEY_EXCHANGE_SIZE {
            debug!(
                role = role_name,
                read_offset = offset,
                remaining = PUBKEY_EXCHANGE_SIZE - offset,
                "BLE pubkey exchange recv start"
            );

            let n = stream.recv(&mut buf[offset..]).await?;

            debug!(
                role = role_name,
                read_offset = offset,
                bytes_read = n,
                "BLE pubkey exchange recv complete"
            );

            if n == 0 {
                return Err(TransportError::RecvFailed(
                    "pubkey exchange: peer closed before full frame".into(),
                ));
            }

            offset += n;
        }

        Ok(())
    })
    .await
    .map_err(|_| TransportError::Timeout)?
}

async fn send_pubkey_frame<S: BleStream>(
    stream: &S,
    local_pubkey: &[u8; 32],
    role: PubkeyExchangeRole,
    role_policy: BleConnectionRolePolicy,
) -> Result<(), TransportError> {
    let role_name = match role {
        PubkeyExchangeRole::Initiator => "initiator",
        PubkeyExchangeRole::Responder => "responder",
    };

    let mut msg = [0u8; PUBKEY_EXCHANGE_SIZE];
    msg[0] = PUBKEY_EXCHANGE_PREFIX;
    msg[PUBKEY_EXCHANGE_ROLE_INDEX] = role_policy.to_wire();
    msg[PUBKEY_EXCHANGE_PUBKEY_START..].copy_from_slice(local_pubkey);

    debug!(role = role_name, bytes = msg.len(), "BLE pubkey exchange send start");
    stream.send(&msg).await?;
    debug!(role = role_name, bytes = msg.len(), "BLE pubkey exchange send complete");
    Ok(())
}

/// Role in the pubkey exchange handshake.
///
/// Used to prevent race conditions in BLE L2CAP where both sides might
/// send simultaneously before either starts receiving. The initiator
/// (connector) sends first; the responder (acceptor) receives first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PubkeyExchangeRole {
    /// Initiator: send pubkey first, then receive peer's pubkey.
    /// Used by outbound connections (scan_probe_loop).
    Initiator,
    /// Responder: receive peer's pubkey first, then send ours.
    /// Used by inbound connections (accept_loop).
    Responder,
}

impl PubkeyExchangeRole {
    const fn opposite(self) -> Self {
        match self {
            Self::Initiator => Self::Responder,
            Self::Responder => Self::Initiator,
        }
    }
}

impl BleConnectionRolePolicy {
    const fn supports(self, role: PubkeyExchangeRole) -> bool {
        match role {
            PubkeyExchangeRole::Initiator => self.supports_outbound(),
            PubkeyExchangeRole::Responder => self.supports_inbound(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PubkeyExchangePeer {
    pubkey: XOnlyPublicKey,
    role_policy: BleConnectionRolePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleResolution {
    Keep,
    DropTieBreaker,
    DropPolicyMismatch,
}

fn resolve_connection_role(
    local_role_policy: BleConnectionRolePolicy,
    peer_role_policy: BleConnectionRolePolicy,
    local_role: PubkeyExchangeRole,
    local_node_addr: Option<&NodeAddr>,
    peer_pubkey: &XOnlyPublicKey,
) -> RoleResolution {
    if !local_role_policy.supports(local_role)
        || !peer_role_policy.supports(local_role.opposite())
    {
        return RoleResolution::DropPolicyMismatch;
    }

    if local_role_policy == BleConnectionRolePolicy::Flexible
        && peer_role_policy == BleConnectionRolePolicy::Flexible
        && let Some(our_addr) = local_node_addr
    {
        let peer_addr = NodeAddr::from_pubkey(peer_pubkey);
        match local_role {
            PubkeyExchangeRole::Initiator if our_addr >= &peer_addr => {
                return RoleResolution::DropTieBreaker;
            }
            PubkeyExchangeRole::Responder if our_addr < &peer_addr => {
                return RoleResolution::DropTieBreaker;
            }
            _ => {}
        }
    }

    RoleResolution::Keep
}

fn remove_pending_addr(pending_addrs: &mut Vec<BleAddr>, addr: &BleAddr) {
    pending_addrs.retain(|a| a != addr);
}

/// Exchange public keys over a newly established L2CAP connection.
///
/// Uses role-based asymmetric handshake to prevent race conditions:
/// - Initiator (connector) sends first, then receives
/// - Responder (acceptor) receives first, then sends
///
/// Returns the peer's XOnlyPublicKey on success.
async fn pubkey_exchange<S: BleStream>(
    stream: &S,
    local_pubkey: &[u8; 32],
    role: PubkeyExchangeRole,
    role_policy: BleConnectionRolePolicy,
) -> Result<PubkeyExchangePeer, TransportError> {
    let mut buf = [0u8; PUBKEY_EXCHANGE_SIZE];
    let role_name = match role {
        PubkeyExchangeRole::Initiator => "initiator",
        PubkeyExchangeRole::Responder => "responder",
    };

    let start = std::time::Instant::now();
    debug!(role = role_name, "BLE pubkey exchange start");

    match role {
        PubkeyExchangeRole::Initiator => {
            debug!(role = role_name, "BLE pubkey exchange: sending our pubkey");
            send_pubkey_frame(stream, local_pubkey, role, role_policy).await?;
            debug!(role = role_name, elapsed_ms = start.elapsed().as_millis() as u64, "BLE pubkey exchange: sent, now receiving peer pubkey");
            recv_pubkey_frame(stream, &mut buf, role).await?;
        }
        PubkeyExchangeRole::Responder => {
            debug!(role = role_name, "BLE pubkey exchange: receiving peer pubkey");
            recv_pubkey_frame(stream, &mut buf, role).await?;
            debug!(role = role_name, elapsed_ms = start.elapsed().as_millis() as u64, "BLE pubkey exchange: received, now sending our pubkey");
            send_pubkey_frame(stream, local_pubkey, role, role_policy).await?;
        }
    }

    debug!(role = role_name, prefix = buf[0], "BLE pubkey exchange frame complete");

    if buf[0] != PUBKEY_EXCHANGE_PREFIX {
        return Err(TransportError::RecvFailed(format!(
            "pubkey exchange: bad prefix 0x{:02X}",
            buf[0]
        )));
    }

    let peer_role_policy = BleConnectionRolePolicy::from_wire(buf[PUBKEY_EXCHANGE_ROLE_INDEX])?;
    let peer_pubkey = XOnlyPublicKey::from_slice(&buf[PUBKEY_EXCHANGE_PUBKEY_START..])
        .map_err(|e| TransportError::RecvFailed(format!("pubkey exchange: invalid key: {}", e)))?;
    
    debug!(role = role_name, total_ms = start.elapsed().as_millis() as u64, "BLE pubkey exchange complete");
    Ok(PubkeyExchangePeer {
        pubkey: peer_pubkey,
        role_policy: peer_role_policy,
    })
}

// Beacon loop removed — advertising is now continuous (started once
// in start_async, stopped in stop_async). BLE advertising overhead
// is negligible (~0.15% duty cycle on advertising channels).

/// Accept loop: accepts inbound L2CAP connections, exchanges pubkeys,
/// and adds to pool.
#[allow(clippy::too_many_arguments)]
async fn accept_loop<A>(
    mut acceptor: A,
    pool: Arc<Mutex<ConnectionPool<Arc<A::Stream>>>>,
    packet_tx: PacketTx,
    transport_id: TransportId,
    stats: Arc<BleStats>,
    _max_conns: usize,
    local_pubkey: Option<[u8; 32]>,
    discovery_buffer: Arc<DiscoveryBuffer>,
    local_node_addr: Option<NodeAddr>,
    local_role_policy: BleConnectionRolePolicy,
) where
    A: io::BleAcceptor,
    A::Stream: 'static,
{
    loop {
        match acceptor.accept().await {
            Ok(stream) => {
                let addr = stream.remote_addr().clone();
                let ta = addr.to_transport_addr();

                // Skip if already connected (outbound won the race)
                {
                    let pool_guard = pool.lock().await;
                    if pool_guard.contains(&ta) {
                        debug!(addr = %ta, "BLE inbound: already connected, skipping");
                        continue;
                    }
                }

                let send_mtu = stream.send_mtu();
                let recv_mtu = stream.recv_mtu();

                // Pre-handshake pubkey exchange (temporary, pre-XX)
                if let Some(ref our_pubkey) = local_pubkey {
                    match pubkey_exchange(
                        &stream,
                        our_pubkey,
                        PubkeyExchangeRole::Responder,
                        local_role_policy,
                    )
                    .await
                    {
                        Ok(peer) => {
                            debug!(addr = %ta, "BLE inbound pubkey exchange complete");
                            discovery_buffer.add_peer_with_pubkey(&addr, peer.pubkey);

                            match resolve_connection_role(
                                local_role_policy,
                                peer.role_policy,
                                PubkeyExchangeRole::Responder,
                                local_node_addr.as_ref(),
                                &peer.pubkey,
                            ) {
                                RoleResolution::Keep => {}
                                RoleResolution::DropTieBreaker => {
                                    debug!(
                                        addr = %ta,
                                        "BLE inbound tie-breaker: dropping (our addr < peer, outbound wins)"
                                    );
                                    continue;
                                }
                                RoleResolution::DropPolicyMismatch => {
                                    warn!(
                                        addr = %ta,
                                        local_role_policy = ?local_role_policy,
                                        peer_role_policy = ?peer.role_policy,
                                        "BLE inbound role policy mismatch; dropping connection"
                                    );
                                    continue;
                                }
                            }
                        }
                        Err(e) => {
                            debug!(addr = %ta, error = %e, "BLE inbound pubkey exchange failed");
                            continue;
                        }
                    }
                }

                let stream = Arc::new(stream);

                // Spawn receive loop
                let recv_task = tokio::spawn(receive_loop(
                    Arc::clone(&stream),
                    ta.clone(),
                    Arc::clone(&pool),
                    packet_tx.clone(),
                    transport_id,
                    Arc::clone(&stats),
                    recv_mtu,
                ));

                let conn = BleConnection {
                    stream,
                    recv_task: Some(recv_task),
                    send_mtu,
                    recv_mtu,
                    established_at: tokio::time::Instant::now(),
                    is_static: false,
                    addr,
                };

                let mut pool_guard = pool.lock().await;
                match pool_guard.insert(ta.clone(), conn) {
                    Ok(Some(evicted)) => {
                        stats.record_pool_eviction();
                        info!(addr = %ta, evicted = %evicted, "BLE inbound accepted (evicted peer)");
                    }
                    Ok(None) => {
                        info!(addr = %ta, send_mtu, recv_mtu, "BLE inbound connection accepted");
                    }
                    Err(e) => {
                        warn!(addr = %ta, error = %e, "BLE pool full, inbound connection rejected");
                        stats.record_connection_rejected();
                        continue;
                    }
                }
                stats.record_connection_accepted();
            }
            Err(e) => {
                warn!(error = %e, "BLE accept error");
                break;
            }
        }
    }
}

/// Receive loop: reads packets from a BLE stream and delivers to node.
///
/// Some BLE controllers coalesce back-to-back sends into a single recv()
/// Calculate the total frame length from a frame prefix.
///
/// # Arguments
/// * `prefix` - The first 4 bytes of an FMP frame
///
/// # Returns
/// * `Some(frame_len)` if the prefix is valid
/// * `None` if the prefix is too short
///
/// # Wire Format
/// - Bytes [0,1]: version/phase + flags
/// - Bytes [2,3]: payload_len (little-endian u16)
/// - For established frames (phase 0x0): header(16) + payload_len + tag(16)
/// - For handshake frames (phase 0x1/0x2): prefix(4) + payload_len
fn calculate_frame_len(prefix: &[u8]) -> Option<usize> {
    if prefix.len() < crate::node::wire::COMMON_PREFIX_SIZE {
        return None;
    }

    let phase = prefix[0] & 0x0F;
    let payload_len = u16::from_le_bytes([prefix[2], prefix[3]]) as usize;

    let frame_len = if phase == crate::node::wire::PHASE_ESTABLISHED {
        crate::node::wire::ESTABLISHED_HEADER_SIZE + payload_len + crate::noise::TAG_SIZE
    } else {
        crate::node::wire::COMMON_PREFIX_SIZE + payload_len
    };

    Some(frame_len)
}

/// call despite SeqPacket semantics. This loop splits coalesced data on
/// FMP frame boundaries using the payload_len field in each 4-byte prefix.
async fn receive_loop<S: BleStream>(
    stream: Arc<S>,
    addr: TransportAddr,
    pool: Arc<Mutex<ConnectionPool<Arc<S>>>>,
    packet_tx: PacketTx,
    transport_id: TransportId,
    stats: Arc<BleStats>,
    recv_mtu: u16,
) {
    let mut buf = vec![0u8; recv_mtu as usize];
    loop {
        match stream.recv(&mut buf).await {
            Ok(0) => {
                debug!(addr = %addr, "BLE connection closed by peer");
                break;
            }
            Ok(n) => {
                let mut remaining = &buf[..n];
                let mut frame_count = 0u32;
                while !remaining.is_empty() {
                    let frame_len = match calculate_frame_len(remaining) {
                        Some(len) => len,
                        None => {
                            debug!(
                                addr = %addr,
                                leftover = remaining.len(),
                                "BLE receive: leftover bytes shorter than FMP prefix, discarding"
                            );
                            break;
                        }
                    };

                    if frame_len < crate::node::wire::COMMON_PREFIX_SIZE || frame_len > remaining.len() {
                        debug!(
                            addr = %addr,
                            frame_len,
                            remaining = remaining.len(),
                            "BLE receive: invalid or incomplete frame, discarding rest"
                        );
                        break;
                    }
                    let frame_data = remaining[..frame_len].to_vec();
                    if frame_count == 0 {
                        stats.record_recv(n);
                    } else {
                        stats.record_recv(frame_len);
                    }
                    frame_count += 1;
                    let packet = ReceivedPacket::new(transport_id, addr.clone(), frame_data);
                    if packet_tx.send(packet).await.is_err() {
                        trace!("BLE packet_tx closed, stopping receive loop");
                        break;
                    }
                    remaining = &remaining[frame_len..];
                }
                if frame_count > 1 {
                    debug!(addr = %addr, frames = frame_count, "BLE receive: split coalesced frames");
                }
            }
            Err(e) => {
                debug!(addr = %addr, error = %e, "BLE receive error");
                stats.record_recv_error();
                break;
            }
        }
    }

    // Remove from pool
    let mut pool = pool.lock().await;
    pool.remove(&addr);
}

async fn promote_probe_connection<S: BleStream + 'static>(
    stream: S,
    addr: BleAddr,
    pool: Arc<Mutex<ConnectionPool<Arc<S>>>>,
    packet_tx: PacketTx,
    transport_id: TransportId,
    stats: Arc<BleStats>,
) {
    let ta = addr.to_transport_addr();
    let send_mtu = stream.send_mtu();
    let recv_mtu = stream.recv_mtu();
    let stream = Arc::new(stream);

    let recv_task = tokio::spawn(receive_loop(
        Arc::clone(&stream),
        ta.clone(),
        Arc::clone(&pool),
        packet_tx,
        transport_id,
        Arc::clone(&stats),
        recv_mtu,
    ));

    let conn = BleConnection {
        stream,
        recv_task: Some(recv_task),
        send_mtu,
        recv_mtu,
        established_at: tokio::time::Instant::now(),
        is_static: false,
        addr,
    };

    let mut pool_guard = pool.lock().await;
    match pool_guard.insert(ta.clone(), conn) {
        Ok(Some(evicted)) => {
            stats.record_pool_eviction();
            debug!(addr = %ta, evicted = %evicted, "BLE probe promoted (evicted peer)");
        }
        Ok(None) => {
            debug!(addr = %ta, "BLE probe promoted to pool");
        }
        Err(e) => {
            warn!(addr = %ta, error = %e, "BLE pool full, probe connection dropped");
            stats.record_connection_rejected();
            return;
        }
    }
    drop(pool_guard);
    stats.record_connection_established();
}

/// Combined scan + probe loop.
///
/// Scanner events arrive continuously (both sides advertise continuously).
/// Each scan result is probed immediately unless the address is in cooldown
/// (recently probed) or already connected. On successful probe, the
/// connection is promoted directly into the pool (no second L2CAP connect
/// needed) and the peer is reported to the discovery buffer for the node
/// layer to auto-connect.
///
/// Cooldown prevents rapid re-probing of the same address: after any probe
/// attempt (success or failure), the address is suppressed for
/// `cooldown_secs`. Connected peers are filtered by pool membership.
#[allow(clippy::too_many_arguments)]
async fn scan_probe_loop<I: io::BleIo>(
    mut scanner: I::Scanner,
    io: Arc<I>,
    pool: Arc<Mutex<ConnectionPool<Arc<I::Stream>>>>,
    buffer: Arc<DiscoveryBuffer>,
    stats: Arc<BleStats>,
    local_pubkey: Option<[u8; 32]>,
    psm: u16,
    connect_timeout_ms: u64,
    cooldown_secs: u64,
    local_node_addr: Option<NodeAddr>,
    packet_tx: PacketTx,
    transport_id: TransportId,
    local_role_policy: BleConnectionRolePolicy,
) {
    // Track last probe time per address for cooldown
    let mut last_probed: HashMap<BleAddr, tokio::time::Instant> = HashMap::new();
    let mut role_mismatch_until: HashMap<BleAddr, tokio::time::Instant> = HashMap::new();
    // Addresses discovered but not yet connected — retried after cooldown
    // even if the scanner doesn't fire again (BlueZ deduplicates).
    let mut pending_addrs: Vec<BleAddr> = Vec::new();
    let cooldown = std::time::Duration::from_secs(cooldown_secs);
    let retry_interval = tokio::time::interval(std::time::Duration::from_secs(cooldown_secs));
    tokio::pin!(retry_interval);
    retry_interval.tick().await; // consume initial tick

    loop {
        // Either a scanner event or the retry timer fires
        let addr = tokio::select! {
            result = scanner.next() => {
                match result {
                    Some(a) => a,
                    None => {
                        debug!("BLE scanner ended");
                        break;
                    }
                }
            }
            _ = retry_interval.tick() => {
                // Re-probe pending addresses that aren't connected
                let pool_guard = pool.lock().await;
                pending_addrs.retain(|a| !pool_guard.contains(&a.to_transport_addr()));
                drop(pool_guard);
                if let Some(a) = pending_addrs.first().cloned() {
                    a
                } else {
                    continue;
                }
            }
        };

        let now = tokio::time::Instant::now();
        role_mismatch_until.retain(|_, until| *until > now);

        trace!(addr = %addr, "BLE scan result");
        stats.record_scan_result();

        // Skip if already connected
        {
            let pool_guard = pool.lock().await;
            if pool_guard.contains(&addr.to_transport_addr()) {
                remove_pending_addr(&mut pending_addrs, &addr);
                continue;
            }
        }

        // Track for retry in case probe fails and scanner doesn't re-fire
        if !pending_addrs.contains(&addr) {
            pending_addrs.push(addr.clone());
        }

        // Skip if in cooldown
        if last_probed
            .get(&addr)
            .is_some_and(|last| last.elapsed() < cooldown)
        {
            continue;
        }

        if role_mismatch_until
            .get(&addr)
            .is_some_and(|until| *until > tokio::time::Instant::now())
        {
            trace!(addr = %addr, "BLE scan result suppressed by role mismatch backoff");
            continue;
        }

        // Record probe time (before attempt, so cooldown applies on failure too)
        last_probed.insert(addr.clone(), tokio::time::Instant::now());

        // Need pubkey for probe
        let our_pubkey = match local_pubkey {
            Some(pk) => pk,
            None => {
                buffer.add_peer(&addr);
                continue;
            }
        };

        // L2CAP connect
        let stream = match tokio::time::timeout(
            std::time::Duration::from_millis(connect_timeout_ms),
            io.connect(&addr, psm),
        )
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                debug!(addr = %addr, error = %e, "BLE probe connect failed");
                continue;
            }
            Err(_) => {
                debug!(addr = %addr, "BLE probe connect timeout");
                stats.record_connect_timeout();
                continue;
            }
        };

        // Pubkey exchange, then promote connection to pool
        if ALLOW_OUTBOUND_PROBE_TIMEOUT_PROMOTION {
            match send_pubkey_frame(
                &stream,
                &our_pubkey,
                PubkeyExchangeRole::Initiator,
                local_role_policy,
            )
            .await
            {
                Ok(()) => {
                    promote_probe_connection(
                        stream,
                        addr.clone(),
                        Arc::clone(&pool),
                        packet_tx.clone(),
                        transport_id,
                        Arc::clone(&stats),
                    )
                    .await;
                    remove_pending_addr(&mut pending_addrs, &addr);
                    continue;
                }
                Err(e) => {
                    debug!(addr = %addr, error = %e, "BLE probe pubkey send failed");
                    continue;
                }
            }
        }

        match pubkey_exchange(
            &stream,
            &our_pubkey,
            PubkeyExchangeRole::Initiator,
            local_role_policy,
        )
        .await
        {
            Ok(peer) => {
                debug!(addr = %addr, "BLE probe complete");

                match resolve_connection_role(
                    local_role_policy,
                    peer.role_policy,
                    PubkeyExchangeRole::Initiator,
                    local_node_addr.as_ref(),
                    &peer.pubkey,
                ) {
                    RoleResolution::Keep => {}
                    RoleResolution::DropTieBreaker => {
                        debug!(
                            addr = %addr,
                            "BLE probe tie-breaker: yielding to peer's outbound"
                        );
                        buffer.add_peer_with_pubkey(&addr, peer.pubkey);
                        continue;
                    }
                    RoleResolution::DropPolicyMismatch => {
                        warn!(
                            addr = %addr,
                            local_role_policy = ?local_role_policy,
                            peer_role_policy = ?peer.role_policy,
                            backoff_secs = ROLE_MISMATCH_BACKOFF_SECS,
                            "BLE probe role policy mismatch; backing off"
                        );
                        role_mismatch_until
                            .insert(
                                addr.clone(),
                                tokio::time::Instant::now()
                                    + std::time::Duration::from_secs(ROLE_MISMATCH_BACKOFF_SECS),
                            );
                        remove_pending_addr(&mut pending_addrs, &addr);
                        continue;
                    }
                }

                // Promote connection to pool — no second L2CAP connect needed
                promote_probe_connection(
                    stream,
                    addr.clone(),
                    Arc::clone(&pool),
                    packet_tx.clone(),
                    transport_id,
                    Arc::clone(&stats),
                )
                .await;
                remove_pending_addr(&mut pending_addrs, &addr);

                // Report to node layer for auto-connect / handshake
                buffer.add_peer_with_pubkey(&addr, peer.pubkey);
            }
            Err(e) => {
                if matches!(e, TransportError::Timeout) && ALLOW_OUTBOUND_PROBE_TIMEOUT_PROMOTION {
                    debug!(
                        addr = %addr,
                        "BLE probe pubkey exchange timed out; promoting connection for inbound handshake fallback"
                    );
                    promote_probe_connection(
                        stream,
                        addr.clone(),
                        Arc::clone(&pool),
                        packet_tx.clone(),
                        transport_id,
                        Arc::clone(&stats),
                    )
                    .await;
                    remove_pending_addr(&mut pending_addrs, &addr);
                    continue;
                }
                debug!(addr = %addr, error = %e, "BLE probe pubkey exchange failed");
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use super::addr::BleDeviceAddr;
    use io::{MockBleIo, MockBleStream};
    use std::collections::VecDeque;
    use tokio::sync::Mutex as TokioMutex;

    fn test_addr(n: u8) -> BleAddr {
        BleAddr {
            adapter: "hci0".to_string(),
            device: BleDeviceAddr::Mac([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, n]),
        }
    }

    fn make_transport(
        io: MockBleIo,
    ) -> (BleTransport<MockBleIo>, tokio::sync::mpsc::Receiver<ReceivedPacket>) {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let config = BleConfig::default();
        let transport = BleTransport::new(
            TransportId::new(1),
            None,
            config,
            io,
            tx,
        );
        (transport, rx)
    }

    fn test_pubkey(seed: u8) -> [u8; 32] {
        let mut secret = [0u8; 32];
        secret.fill(seed);
        secret[31] = seed.saturating_add(1);
        let secp = secp256k1::Secp256k1::new();
        let secret_key = secp256k1::SecretKey::from_slice(&secret).unwrap();
        let (pubkey, _) = secret_key.public_key(&secp).x_only_public_key();
        pubkey.serialize()
    }

    struct FragmentingBleStream {
        addr: BleAddr,
        send_mtu: u16,
        recv_mtu: u16,
        rx: TokioMutex<VecDeque<Vec<u8>>>,
        sent: TokioMutex<Vec<Vec<u8>>>,
    }

    impl FragmentingBleStream {
        fn new(addr: BleAddr, chunks: Vec<Vec<u8>>) -> Self {
            Self {
                addr,
                send_mtu: 2048,
                recv_mtu: 2048,
                rx: TokioMutex::new(VecDeque::from(chunks)),
                sent: TokioMutex::new(Vec::new()),
            }
        }

        async fn sent_frames(&self) -> Vec<Vec<u8>> {
            self.sent.lock().await.clone()
        }
    }

    impl BleStream for FragmentingBleStream {
        async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
            self.sent.lock().await.push(data.to_vec());
            Ok(())
        }

        async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
            let mut rx = self.rx.lock().await;
            match rx.pop_front() {
                Some(chunk) => {
                    let len = chunk.len().min(buf.len());
                    buf[..len].copy_from_slice(&chunk[..len]);
                    Ok(len)
                }
                None => Ok(0),
            }
        }

        fn send_mtu(&self) -> u16 {
            self.send_mtu
        }

        fn recv_mtu(&self) -> u16 {
            self.recv_mtu
        }

        fn remote_addr(&self) -> &BleAddr {
            &self.addr
        }
    }

    #[test]
    fn test_transport_type() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (transport, _rx) = make_transport(io);
        assert_eq!(transport.transport_type().name, "ble");
        assert!(transport.transport_type().connection_oriented);
        assert!(transport.transport_type().reliable);
    }

    #[test]
    fn test_transport_initial_state() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (transport, _rx) = make_transport(io);
        assert_eq!(transport.state(), TransportState::Configured);
    }

    #[test]
    fn test_transport_default_mtu() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (transport, _rx) = make_transport(io);
        assert_eq!(transport.mtu(), 2048);
    }

    #[tokio::test]
    async fn test_transport_start_stop() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (mut transport, _rx) = make_transport(io);
        transport.start_async().await.unwrap();
        assert_eq!(transport.state(), TransportState::Up);

        transport.stop_async().await.unwrap();
        assert_eq!(transport.state(), TransportState::Down);
    }

    #[tokio::test]
    async fn test_outbound_only_backend_skips_inbound_tasks() {
        let io = MockBleIo::new("hci0", test_addr(1))
            .with_role_policy(BleConnectionRolePolicy::OutboundOnly);
        let (mut transport, _rx) = make_transport(io);

        transport.start_async().await.unwrap();

        assert!(transport.accept_task.is_none());
        assert!(transport.scan_probe_task.is_some());
    }

    #[tokio::test]
    async fn test_inbound_only_backend_skips_scan_task() {
        let io = MockBleIo::new("hci0", test_addr(1))
            .with_role_policy(BleConnectionRolePolicy::InboundOnly);
        let (mut transport, _rx) = make_transport(io);

        transport.start_async().await.unwrap();

        assert!(transport.accept_task.is_some());
        assert!(transport.scan_probe_task.is_none());
    }

    #[tokio::test]
    async fn test_pubkey_exchange_completes_for_role_asymmetry() {
        let (stream_a, stream_b) = MockBleStream::pair(test_addr(1), test_addr(2), 2048);
        let pubkey_a = test_pubkey(3);
        let pubkey_b = test_pubkey(9);

        let initiator = pubkey_exchange(
            &stream_a,
            &pubkey_a,
            PubkeyExchangeRole::Initiator,
            BleConnectionRolePolicy::Flexible,
        );
        let responder = pubkey_exchange(
            &stream_b,
            &pubkey_b,
            PubkeyExchangeRole::Responder,
            BleConnectionRolePolicy::Flexible,
        );
        let (peer_from_initiator, peer_from_responder) = tokio::join!(initiator, responder);

        let peer_from_initiator = peer_from_initiator.unwrap();
        let peer_from_responder = peer_from_responder.unwrap();
        assert_eq!(peer_from_initiator.pubkey.serialize(), pubkey_b);
        assert_eq!(peer_from_responder.pubkey.serialize(), pubkey_a);
        assert_eq!(peer_from_initiator.role_policy, BleConnectionRolePolicy::Flexible);
        assert_eq!(peer_from_responder.role_policy, BleConnectionRolePolicy::Flexible);
    }

    #[tokio::test]
    async fn test_pubkey_exchange_initiator_accepts_fragmented_frame() {
        let peer_pubkey = test_pubkey(21);
        let mut frame = [0u8; PUBKEY_EXCHANGE_SIZE];
        frame[0] = PUBKEY_EXCHANGE_PREFIX;
        frame[PUBKEY_EXCHANGE_ROLE_INDEX] = BleConnectionRolePolicy::OutboundOnly.to_wire();
        frame[PUBKEY_EXCHANGE_PUBKEY_START..].copy_from_slice(&peer_pubkey);

        let stream = FragmentingBleStream::new(
            test_addr(2),
            vec![frame[..7].to_vec(), frame[7..19].to_vec(), frame[19..].to_vec()],
        );

        let received = pubkey_exchange(
            &stream,
            &test_pubkey(11),
            PubkeyExchangeRole::Initiator,
            BleConnectionRolePolicy::Flexible,
        )
        .await
        .unwrap();

        assert_eq!(received.pubkey.serialize(), peer_pubkey);
        assert_eq!(received.role_policy, BleConnectionRolePolicy::OutboundOnly);
        let sent = stream.sent_frames().await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].len(), PUBKEY_EXCHANGE_SIZE);
    }

    #[tokio::test]
    async fn test_pubkey_exchange_responder_accepts_fragmented_frame() {
        let peer_pubkey = test_pubkey(31);
        let local_pubkey = test_pubkey(32);
        let mut frame = [0u8; PUBKEY_EXCHANGE_SIZE];
        frame[0] = PUBKEY_EXCHANGE_PREFIX;
        frame[PUBKEY_EXCHANGE_ROLE_INDEX] = BleConnectionRolePolicy::Flexible.to_wire();
        frame[PUBKEY_EXCHANGE_PUBKEY_START..].copy_from_slice(&peer_pubkey);

        let stream = FragmentingBleStream::new(
            test_addr(3),
            vec![frame[..1].to_vec(), frame[1..16].to_vec(), frame[16..].to_vec()],
        );

        let received = pubkey_exchange(
            &stream,
            &local_pubkey,
            PubkeyExchangeRole::Responder,
            BleConnectionRolePolicy::InboundOnly,
        )
        .await
        .unwrap();

        assert_eq!(received.pubkey.serialize(), peer_pubkey);
        assert_eq!(received.role_policy, BleConnectionRolePolicy::Flexible);

        let sent = stream.sent_frames().await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0][0], PUBKEY_EXCHANGE_PREFIX);
        assert_eq!(sent[0][PUBKEY_EXCHANGE_ROLE_INDEX], BleConnectionRolePolicy::InboundOnly.to_wire());
        assert_eq!(&sent[0][PUBKEY_EXCHANGE_PUBKEY_START..], &local_pubkey);
    }

    #[tokio::test(start_paused = true)]
    async fn test_scan_discovers_peers() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (mut transport, _rx) = make_transport(io);
        transport.start_async().await.unwrap();

        // Inject scan results via the I/O mock
        transport.io.inject_scan_result(test_addr(2)).await;
        transport.io.inject_scan_result(test_addr(3)).await;

        // Let scan_probe_loop pick up results and schedule jitter
        tokio::task::yield_now().await;
        // Advance past max jitter (5s) so probes fire
        tokio::time::advance(std::time::Duration::from_secs(6)).await;
        // Let the expired entries get processed
        tokio::task::yield_now().await;

        // Without pubkey set, scan results go to discovery buffer as bare MACs
        let peers = transport.discovery_buffer.take();
        assert_eq!(peers.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_scan_deduplicates() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (mut transport, _rx) = make_transport(io);
        transport.start_async().await.unwrap();

        // Same address twice
        transport.io.inject_scan_result(test_addr(2)).await;
        transport.io.inject_scan_result(test_addr(2)).await;

        // Let scan_probe_loop pick up results
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(6)).await;
        tokio::task::yield_now().await;

        let peers = transport.discovery_buffer.take();
        assert_eq!(peers.len(), 1);
    }

    #[test]
    fn test_transport_auto_connect_default() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (transport, _rx) = make_transport(io);
        assert!(!transport.auto_connect());
    }

    #[test]
    fn test_connection_state_none() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (transport, _rx) = make_transport(io);
        let addr = test_addr(2).to_transport_addr();
        assert_eq!(transport.connection_state_sync(&addr), ConnectionState::None);
    }

    /// Verify that the cross-probe tie-breaker follows the same convention
    /// as `cross_connection_winner`: smaller NodeAddr's outbound wins.
    #[test]
    fn test_tiebreaker_convention() {
        use secp256k1::{Secp256k1, SecretKey};

        let secp = Secp256k1::new();
        let sk_a = SecretKey::from_slice(&[1u8; 32]).unwrap();
        let sk_b = SecretKey::from_slice(&[2u8; 32]).unwrap();
        let (pk_a, _) = sk_a.public_key(&secp).x_only_public_key();
        let (pk_b, _) = sk_b.public_key(&secp).x_only_public_key();

        let addr_a = NodeAddr::from_pubkey(&pk_a);
        let addr_b = NodeAddr::from_pubkey(&pk_b);

        // Determine which is smaller
        let (smaller, larger) = if addr_a < addr_b {
            (addr_a, addr_b)
        } else {
            (addr_b, addr_a)
        };

        // scan_loop (outbound): promotes when our_addr < peer_addr
        // Smaller node scanning larger → our_addr < peer_addr → promote (win)
        assert!(smaller < larger, "test setup: smaller < larger");

        // accept_loop (inbound): drops when our_addr < peer_addr
        // Smaller node accepting from larger → drops inbound (outbound wins)
        // This means: smaller always uses outbound, larger always uses inbound
    }

    #[test]
    fn test_role_resolution_respects_asymmetric_policies() {
        let local_pubkey = XOnlyPublicKey::from_slice(&test_pubkey(1)).unwrap();
        let peer_pubkey = XOnlyPublicKey::from_slice(&test_pubkey(2)).unwrap();
        let local_addr = NodeAddr::from_pubkey(&local_pubkey);

        assert_eq!(
            resolve_connection_role(
                BleConnectionRolePolicy::OutboundOnly,
                BleConnectionRolePolicy::Flexible,
                PubkeyExchangeRole::Initiator,
                Some(&local_addr),
                &peer_pubkey,
            ),
            RoleResolution::Keep
        );

        assert_eq!(
            resolve_connection_role(
                BleConnectionRolePolicy::Flexible,
                BleConnectionRolePolicy::OutboundOnly,
                PubkeyExchangeRole::Initiator,
                Some(&local_addr),
                &peer_pubkey,
            ),
            RoleResolution::DropPolicyMismatch
        );

        assert_eq!(
            resolve_connection_role(
                BleConnectionRolePolicy::OutboundOnly,
                BleConnectionRolePolicy::InboundOnly,
                PubkeyExchangeRole::Initiator,
                Some(&local_addr),
                &peer_pubkey,
            ),
            RoleResolution::Keep
        );
    }

    #[test]
    fn test_calculate_frame_len_prefix_too_short() {
        // Less than 4 bytes should return None
        assert!(calculate_frame_len(&[]).is_none());
        assert!(calculate_frame_len(&[0x00]).is_none());
        assert!(calculate_frame_len(&[0x00, 0x00]).is_none());
        assert!(calculate_frame_len(&[0x00, 0x00, 0x00]).is_none());
    }

    #[test]
    fn test_calculate_frame_len_established_frame() {
        // Established frame (phase 0x0): header(16) + payload_len + tag(16)
        // payload_len = 0 (minimum)
        let prefix = [0x00, 0x00, 0x00, 0x00]; // phase=0, payload_len=0
        let frame_len = calculate_frame_len(&prefix).unwrap();
        assert_eq!(frame_len, 16 + 0 + 16); // = 32 bytes total

        // payload_len = 100
        let prefix = [0x00, 0x00, 0x64, 0x00]; // phase=0, payload_len=100
        let frame_len = calculate_frame_len(&prefix).unwrap();
        assert_eq!(frame_len, 16 + 100 + 16); // = 132 bytes total

        // payload_len = 1280 (max MTU)
        let prefix = [0x00, 0x00, 0x00, 0x05]; // phase=0, payload_len=1280 (0x0500 LE)
        let frame_len = calculate_frame_len(&prefix).unwrap();
        assert_eq!(frame_len, 16 + 1280 + 16); // = 1312 bytes total
    }

    #[test]
    fn test_calculate_frame_len_handshake_msg1() {
        // Handshake msg1 (phase 0x1): prefix(4) + payload_len
        let prefix = [0x01, 0x00, 0x20, 0x00]; // phase=1, payload_len=32
        let frame_len = calculate_frame_len(&prefix).unwrap();
        assert_eq!(frame_len, 4 + 32); // = 36 bytes total
    }

    #[test]
    fn test_calculate_frame_len_handshake_msg2() {
        // Handshake msg2 (phase 0x2): prefix(4) + payload_len
        let prefix = [0x02, 0x00, 0x30, 0x00]; // phase=2, payload_len=48
        let frame_len = calculate_frame_len(&prefix).unwrap();
        assert_eq!(frame_len, 4 + 48); // = 52 bytes total
    }

    #[test]
    fn test_calculate_frame_len_phase_with_flags() {
        // Phase can have flags in high nibble of first byte
        // Established with flags (0x10 = phase 0, flag bit 4 set)
        let prefix = [0x10, 0x00, 0x40, 0x00]; // phase=0 (0x10 & 0x0F), payload_len=64
        let frame_len = calculate_frame_len(&prefix).unwrap();
        assert_eq!(frame_len, 16 + 64 + 16); // = 96 bytes total (established)

        // Handshake msg1 with flags (0x11 = phase 1, flag bit 4 set)
        let prefix = [0x11, 0x00, 0x20, 0x00]; // phase=1 (0x11 & 0x0F), payload_len=32
        let frame_len = calculate_frame_len(&prefix).unwrap();
        assert_eq!(frame_len, 4 + 32); // = 36 bytes total (handshake)
    }

    #[test]
    fn test_calculate_frame_len_various_payload_sizes() {
        // Test various payload sizes for established frames
        for payload_len in [0, 1, 16, 64, 128, 256, 512, 1000, 1280] {
            let prefix = [
                0x00, // phase 0 (established)
                0x00,
                (payload_len & 0xFF) as u8,
                ((payload_len >> 8) & 0xFF) as u8,
            ];
            let frame_len = calculate_frame_len(&prefix).unwrap();
            assert_eq!(frame_len, 16 + payload_len as usize + 16);
        }

        // Test various payload sizes for handshake frames
        for payload_len in [0, 1, 16, 32, 48, 64, 128] {
            let prefix = [
                0x01, // phase 1 (handshake msg1)
                0x00,
                (payload_len & 0xFF) as u8,
                ((payload_len >> 8) & 0xFF) as u8,
            ];
            let frame_len = calculate_frame_len(&prefix).unwrap();
            assert_eq!(frame_len, 4 + payload_len as usize);
        }
    }
}
