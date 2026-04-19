//! BLE L2CAP Transport Implementation
//!
//! Provides BLE-based transport for FIPS peer communication using L2CAP
//! Connection-Oriented Channels (CoC) in SeqPacket mode.
//!
//! ## Framing Note
//!
//! This branch uses a 2-byte BE length prefix on all BLE sends/recvs (added in
//! commit `42d9adb`). **This is NOT upstream behavior** — upstream has no framing
//! layer. The prefix was added to handle macOS CoreBluetooth byte-stream coalescing.
//! On Linux, SeqPacket preserves boundaries so the framing is unnecessary but harmless.
//!
//! **WARNING**: Commits `e81d688` and `daa76f1` added FMP-level coalescing logic in
//! this file's receive_loop. This is the WRONG layer for transport framing. See
//! `.sisyphus/notepad/ble-framing-architecture.md` for details.
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
pub mod rate_limit;
pub mod stats;

use super::{
    ConnectionState, DisconnectTx, DiscoveredPeer, PacketTx, ReceivedPacket, Transport,
    TransportAddr, TransportDisconnect, TransportError, TransportId, TransportState,
    TransportType,
};
use crate::config::BleConfig;
use crate::identity::NodeAddr;
use addr::BleAddr;
use discovery::DiscoveryBuffer;
use io::{BleIo, BleScanner, BleStream};
use pool::{BLE_FRAME_PREFIX_LEN, BleConnection, ConnectionPool};
use rate_limit::BleRateAdapter;
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
/// Test builds and builds without `ble` use `MockBleIo`.
#[cfg(all(feature = "ble", target_os = "linux", not(test)))]
pub type DefaultBleTransport = BleTransport<io::BluerIo>;

#[cfg(all(feature = "ble-macos", not(test)))]
pub type DefaultBleTransport = BleTransport<io::BluestIo>;

#[cfg(any(
    not(any(
        all(feature = "ble", target_os = "linux"),
        feature = "ble-macos",
    )),
    test
))]
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
    /// Channel for notifying Node when a live connection disappears.
    disconnect_tx: Option<DisconnectTx>,
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
    /// After L2CAP connection, both sides exchange `[0x00][pubkey:32]`
    /// so the node layer can initiate the IK handshake.
    /// Temporary — removed when FMP switches to XX.
    local_pubkey: Option<[u8; 32]>,
    local_capabilities: PeerCapabilities,
    /// Adaptive rate controller using MMP SRTT feedback.
    rate_adapter: Arc<Mutex<BleRateAdapter>>,
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
        let initial_rate_bps = config.effective_send_rate_bps();
        Self {
            transport_id,
            name,
            config,
            state: TransportState::Configured,
            io: Arc::new(io),
            pool: Arc::new(Mutex::new(ConnectionPool::new(max_conns))),
            connecting: Arc::new(Mutex::new(HashMap::new())),
            packet_tx,
            disconnect_tx: None,
            accept_task: None,
            scan_probe_task: None,
            discovery_buffer: Arc::new(DiscoveryBuffer::new(transport_id)),
            stats: Arc::new(BleStats::new()),
            local_pubkey: None,
            local_capabilities: PeerCapabilities::linux_default(),
            rate_adapter: Arc::new(Mutex::new(BleRateAdapter::new(initial_rate_bps))),
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

    /// Set the disconnect notification channel used for immediate peer cleanup.
    pub fn set_disconnect_tx(&mut self, tx: DisconnectTx) {
        self.disconnect_tx = Some(tx);
    }

    pub fn set_local_capabilities(&mut self, caps: PeerCapabilities) {
        self.local_capabilities = caps;
    }

    /// Start the transport asynchronously.
    pub async fn start_async(&mut self) -> Result<(), TransportError> {
        if !self.state.can_start() {
            return Err(TransportError::AlreadyStarted);
        }
        self.state = TransportState::Starting;

        let psm = self.config.psm();
        let adapter = self.io.adapter_name().to_string();

        // Pre-compute local NodeAddr for cross-probe tie-breaking
        let local_node_addr = self.local_pubkey.and_then(|pk| {
            XOnlyPublicKey::from_slice(&pk)
                .ok()
                .map(|xonly| NodeAddr::from_pubkey(&xonly))
        });

        // Start L2CAP listener for inbound connections
        if self.config.accept_connections() {
            match self.io.listen(psm).await {
                Ok(acceptor) => {
                    let pool = Arc::clone(&self.pool);
                    let packet_tx = self.packet_tx.clone();
                    let disconnect_tx = self.disconnect_tx.clone();
                    let transport_id = self.transport_id;
                    let stats = Arc::clone(&self.stats);
                    let max_conns = self.config.max_connections();
                    let recv_timeout_secs = self.config.recv_timeout_secs();

                    self.accept_task = Some(tokio::spawn(accept_loop(
                        acceptor,
                        pool,
                        packet_tx,
                        disconnect_tx,
                        transport_id,
                        stats,
                        max_conns,
                        self.local_pubkey,
                        Arc::clone(&self.discovery_buffer),
                        local_node_addr,
                        self.local_capabilities,
                        recv_timeout_secs,
                    )));
                    debug!(adapter = %adapter, psm = psm, "BLE accept loop started");
                }
                Err(e) => {
                    warn!(adapter = %adapter, error = %e, "failed to start BLE listener");
                    self.state = TransportState::Failed;
                    return Err(e);
                }
            }
        }

        // Start continuous advertising
        if self.config.advertise() {
            if let Err(e) = self.io.start_advertising().await {
                warn!(adapter = %adapter, error = %e, "failed to start BLE advertising");
            } else {
                self.stats.record_advertisement();
                debug!(adapter = %adapter, "BLE advertising started (continuous)");
            }
        }

        // Start combined scan + probe loop
        if self.config.scan() {
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
                        self.config.recv_timeout_secs(),
                        local_node_addr,
                        self.packet_tx.clone(),
                        self.disconnect_tx.clone(),
                        self.transport_id,
                        self.local_capabilities,
                    )));
                    debug!(adapter = %adapter, "BLE scan+probe loop started");
                }
                Err(e) => {
                    warn!(adapter = %adapter, error = %e, "failed to start BLE scanning");
                }
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
        // Clone stream Arc and drop pool lock BEFORE the send (which may
        // sleep for rate limiting). Holding the pool lock during send blocks
        // all other BLE operations for the sleep duration.
        let stream = {
            let pool = self.pool.lock().await;
            let conn = match pool.get(addr) {
                Some(c) => c,
                None => {
                    drop(pool);
                    let _ = self.connect_async(addr).await;
                    return Err(TransportError::SendFailed("not connected".into()));
                }
            };

            let mtu = conn.effective_mtu() as usize;
            if data.len() > mtu {
                self.stats.record_mtu_exceeded();
                return Err(TransportError::MtuExceeded {
                    packet_size: data.len(),
                    mtu: mtu as u16,
                });
            }

            Arc::clone(&conn.stream)
        }; // pool lock dropped here

        match stream.send(data).await {
            Ok(()) => {
                self.stats.record_send(data.len());
                Ok(data.len())
            }
            Err(e) => {
                self.stats.record_send_error();
                let mut pool = self.pool.lock().await;
                pool.remove(addr);
                drop(pool);
                if let Some(tx) = &self.disconnect_tx {
                    let _ = tx.try_send(TransportDisconnect {
                        transport_id: self.transport_id,
                        remote_addr: addr.clone(),
                    });
                }
                warn!(addr = %addr, error = %e, "BLE send failed, connection removed");
                Err(e)
            }
        }
    }

    /// Send data with priority (bypasses rate limiter) to a remote BLE address.
    ///
    /// For control plane packets: handshakes, rekey, heartbeats, MMP reports.
    pub async fn send_urgent_async(
        &self,
        addr: &TransportAddr,
        data: &[u8],
    ) -> Result<usize, TransportError> {
        let stream = {
            let pool = self.pool.lock().await;
            let conn = match pool.get(addr) {
                Some(c) => c,
                None => return Err(TransportError::SendFailed("not connected".into())),
            };

            let mtu = conn.effective_mtu() as usize;
            if data.len() > mtu {
                return Err(TransportError::MtuExceeded {
                    packet_size: data.len(),
                    mtu: mtu as u16,
                });
            }

            Arc::clone(&conn.stream)
        };

        match stream.send_urgent(data).await {
            Ok(()) => {
                self.stats.record_send(data.len());
                Ok(data.len())
            }
            Err(e) => {
                self.stats.record_send_error();
                let mut pool = self.pool.lock().await;
                pool.remove(addr);
                drop(pool);
                if let Some(tx) = &self.disconnect_tx {
                    let _ = tx.try_send(TransportDisconnect {
                        transport_id: self.transport_id,
                        remote_addr: addr.clone(),
                    });
                }
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

        wait_before_outbound_pubkey_exchange().await;

        // Pre-handshake pubkey exchange (temporary, pre-XX)
        if let Some(ref our_pubkey) = self.local_pubkey {
            match pubkey_exchange(&stream, our_pubkey, self.local_capabilities).await {
                Ok(result) => {
                    debug!(addr = %addr, peer_caps = format!("0x{:02x}", result.peer_capabilities.to_byte()), local_caps = format!("0x{:02x}", self.local_capabilities.to_byte()), "BLE outbound pubkey exchange complete");
                    self.discovery_buffer
                        .add_peer_with_pubkey(&ble_addr, result.peer_pubkey);
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
        let recv_timeout_secs = self.config.recv_timeout_secs();
        let stream = Arc::new(stream);

        let recv_task = tokio::spawn(receive_loop(
            Arc::clone(&stream),
            addr.clone(),
            Arc::clone(&self.pool),
            self.packet_tx.clone(),
            self.disconnect_tx.clone(),
            self.transport_id,
            Arc::clone(&self.stats),
            recv_mtu,
            recv_timeout_secs,
        ));

        let io = Arc::clone(&self.io);
        let drop_addr = ble_addr.clone();
        let on_drop: Option<Box<dyn FnOnce() + Send>> = Some(Box::new(move || {
            let io = io.clone();
            let addr = drop_addr;
            tokio::spawn(async move {
                io.disconnect_device(&addr).await;
            });
        }));

        let conn = BleConnection {
            stream,
            recv_task: Some(recv_task),
            send_mtu,
            recv_mtu,
            established_at: tokio::time::Instant::now(),
            is_static: false,
            addr: ble_addr.clone(),
            on_drop,
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
        let disconnect_tx = self.disconnect_tx.clone();
        let transport_id = self.transport_id;
        let stats = Arc::clone(&self.stats);
        let psm = self.config.psm();
        let timeout_ms = self.config.connect_timeout_ms();
        let recv_timeout_secs = self.config.recv_timeout_secs();
        let addr_clone = addr.clone();
        let local_pubkey = self.local_pubkey;
        let local_capabilities = self.local_capabilities;
        let discovery_buffer = Arc::clone(&self.discovery_buffer);

        let task = tokio::spawn(async move {
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                io.connect(&ble_addr, psm),
            )
            .await;

            match result {
                Ok(Ok(stream)) => {
                    wait_before_outbound_pubkey_exchange().await;

                    // Pre-handshake pubkey exchange (temporary, pre-XX)
                    if let Some(ref our_pubkey) = local_pubkey {
                        match pubkey_exchange(&stream, our_pubkey, local_capabilities).await {
                            Ok(result) => {
                                debug!(addr = %addr_clone, peer_caps = format!("0x{:02x}", result.peer_capabilities.to_byte()), local_caps = format!("0x{:02x}", local_capabilities.to_byte()), "BLE outbound pubkey exchange complete");
                                discovery_buffer
                                    .add_peer_with_pubkey(&ble_addr, result.peer_pubkey);
                            }
                            Err(e) => {
                                warn!(
                                    addr = %addr_clone, error = %e,
                                    "BLE outbound pubkey exchange failed"
                                );
                                connecting.lock().await.remove(&addr_clone);
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
                        disconnect_tx,
                        transport_id,
                        Arc::clone(&stats),
                        recv_mtu,
                        recv_timeout_secs,
                    ));

                    let conn = BleConnection {
                        stream,
                        recv_task: Some(recv_task),
                        send_mtu,
                        recv_mtu,
                        established_at: tokio::time::Instant::now(),
                        is_static: false,
                        addr: ble_addr.clone(),
                        on_drop: Some(Box::new({
                            let io = io.clone();
                            let addr = ble_addr;
                            move || {
                                let io = io.clone();
                                tokio::spawn(async move {
                                    io.disconnect_device(&addr).await;
                                });
                            }
                        })),
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
                            connecting.lock().await.remove(&addr_clone);
                            return;
                        }
                    }
                    connecting.lock().await.remove(&addr_clone);
                    stats.record_connection_established();
                }
                Ok(Err(e)) => {
                    connecting.lock().await.remove(&addr_clone);
                    debug!(addr = %addr_clone, error = %e, "BLE connect failed");
                }
                Err(_) => {
                    stats.record_connect_timeout();
                    connecting.lock().await.remove(&addr_clone);
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

    /// Query transport-local congestion indicators.
    pub fn congestion(&self) -> super::TransportCongestion {
        let snap = self.stats.snapshot();
        super::TransportCongestion {
            recv_drops: if snap.recv_errors > 0 {
                Some(snap.recv_errors)
            } else {
                None
            },
        }
    }

    /// Feed MMP SRTT measurement to the adaptive rate controller.
    ///
    /// Returns the new rate in bps after AIMD adjustment.
    /// Updates the rate limiter on the specific connection's stream.
    pub async fn update_rate_from_srtt(&self, addr: &TransportAddr, srtt_ms: f64) -> u64 {
        let new_rate = self.rate_adapter.lock().await.update(srtt_ms);

        let pool = self.pool.lock().await;
        if let Some(conn) = pool.get(addr) {
            conn.stream.set_rate_bps(new_rate).await;
        }

        new_rate
    }

    /// Get the link MTU for a specific address.
    pub fn link_mtu(&self, addr: &TransportAddr) -> u16 {
        if let Ok(pool) = self.pool.try_lock()
            && let Some(conn) = pool.get(addr)
        {
            return conn.effective_mtu();
        }
        self.config.mtu().saturating_sub(BLE_FRAME_PREFIX_LEN)
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
        self.config.mtu().saturating_sub(BLE_FRAME_PREFIX_LEN)
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

/// Pre-handshake pubkey exchange message size: `[0x00][pubkey:32]`.
const PUBKEY_EXCHANGE_SIZE: usize = 33;

/// Timeout for pubkey exchange recv (seconds).
///
/// The peer should respond in milliseconds; 5s is generous. Without this,
/// a peer that connects but never sends its pubkey blocks the calling task
/// forever — killing scan_probe_loop, accept_loop, or the event loop.
const PUBKEY_EXCHANGE_TIMEOUT_SECS: u64 = 5;

/// BLE peer capability flags exchanged during pubkey exchange.
///
/// Backwards compatible: old nodes send only 33 bytes (no flags).
/// New nodes send 34 bytes (`[0x00][pubkey:32][flags:1]`).
/// Old nodes ignore the trailing byte; new nodes read it if present.
#[derive(Debug, Clone, Copy, Default)]
pub struct PeerCapabilities(u8);

impl PeerCapabilities {
    const LEGACY_CENTRAL_ONLY: u8 = 0x01;
    const PREFER_OUTBOUND: u8 = 0x02;
    const PREFER_L2CAP: u8 = 0x04;
    const CAN_CENTRAL: u8 = 0x08;
    const CAN_PERIPHERAL: u8 = 0x10;
    const L2CAP_SUPPORTED: u8 = 0x20;
    const GATT_SUPPORTED: u8 = 0x40;

    pub fn none() -> Self {
        Self(0)
    }

    pub fn linux_default() -> Self {
        Self(
            Self::L2CAP_SUPPORTED
                | Self::CAN_CENTRAL
                | Self::CAN_PERIPHERAL
                | Self::GATT_SUPPORTED
                | Self::PREFER_L2CAP,
        )
    }

    pub fn central_only() -> Self {
        Self(Self::L2CAP_SUPPORTED | Self::CAN_CENTRAL | Self::PREFER_OUTBOUND)
    }

    pub fn peripheral_only() -> Self {
        Self(Self::L2CAP_SUPPORTED | Self::CAN_PERIPHERAL | Self::GATT_SUPPORTED)
    }

    pub fn macos_default() -> Self {
        // macOS CoreBluetooth supports both central and peripheral BLE roles.
        // When `accept_connections` is true, the peripheral role is activated via
        // CBPeripheralManager (publishL2CAPChannel + GATT service). When false,
        // only the central role is used.
        Self(
            Self::L2CAP_SUPPORTED
                | Self::CAN_CENTRAL
                | Self::CAN_PERIPHERAL
                | Self::GATT_SUPPORTED
                | Self::PREFER_OUTBOUND,
        )
    }

    /// Whether the peer can only act as BLE central (cannot accept inbound).
    pub fn is_central_only(&self) -> bool {
        self.can_initiate_outbound() && !self.can_accept_inbound()
    }

    pub fn can_accept_inbound(&self) -> bool {
        self.is_legacy_unrestricted() || (self.0 & Self::CAN_PERIPHERAL != 0)
    }

    pub fn can_initiate_outbound(&self) -> bool {
        self.is_legacy_unrestricted() || (self.0 & Self::CAN_CENTRAL != 0)
    }

    pub fn supports_l2cap(&self) -> bool {
        self.is_legacy_unrestricted() || (self.0 & Self::L2CAP_SUPPORTED != 0)
    }

    pub fn supports_gatt(&self) -> bool {
        self.0 & Self::GATT_SUPPORTED != 0
    }

    pub fn prefers_l2cap(&self) -> bool {
        self.0 & Self::PREFER_L2CAP != 0
    }

    pub fn prefers_outbound(&self) -> bool {
        self.0 & Self::PREFER_OUTBOUND != 0
    }

    /// Encode as a single byte.
    pub fn to_byte(self) -> u8 {
        self.0
    }

    /// Decode from a single byte.
    pub fn from_byte(byte: u8) -> Self {
        if byte == Self::LEGACY_CENTRAL_ONLY {
            return Self::central_only();
        }
        Self(byte)
    }

    fn is_legacy_unrestricted(&self) -> bool {
        self.0 == 0
    }
}

/// Result of pubkey exchange including peer capabilities.
struct PubkeyExchangeResult {
    peer_pubkey: XOnlyPublicKey,
    peer_capabilities: PeerCapabilities,
}

const OUTBOUND_PUBKEY_EXCHANGE_SETTLE_MS: u64 = 250;

async fn wait_before_outbound_pubkey_exchange() {
    tokio::time::sleep(std::time::Duration::from_millis(
        OUTBOUND_PUBKEY_EXCHANGE_SETTLE_MS,
    ))
    .await;
}

async fn send_pubkey_announcement<S: BleStream>(
    stream: &S,
    local_pubkey: &[u8; 32],
    local_capabilities: PeerCapabilities,
) -> Result<(), TransportError> {
    let mut msg = [0u8; PUBKEY_EXCHANGE_SIZE + 1];
    msg[0] = PUBKEY_EXCHANGE_PREFIX;
    msg[1..33].copy_from_slice(local_pubkey);
    msg[33] = local_capabilities.to_byte();
    stream.send(&msg).await
}

/// Exchange public keys and capabilities over a newly established L2CAP connection.
///
/// Both sides send `[0x00][our_pubkey:32][flags:1]` (34 bytes) and receive
/// the peer's. Old nodes that send only 33 bytes are detected and treated
/// as having no capability flags (full capability).
///
/// Returns the peer's public key and declared capabilities on success.
async fn pubkey_exchange<S: BleStream>(
    stream: &S,
    local_pubkey: &[u8; 32],
    local_capabilities: PeerCapabilities,
) -> Result<PubkeyExchangeResult, TransportError> {
    // Send our pubkey + capability flags
    let mut msg = [0u8; PUBKEY_EXCHANGE_SIZE + 1];
    msg[0] = PUBKEY_EXCHANGE_PREFIX;
    msg[1..33].copy_from_slice(local_pubkey);
    msg[33] = local_capabilities.to_byte();
    stream.send(&msg).await?;

    // Receive peer's pubkey (with timeout to prevent indefinite blocking)
    // Accept 33 bytes (old peer, no flags) or 34 bytes (new peer, with flags).
    let mut buf = [0u8; PUBKEY_EXCHANGE_SIZE + 1];
    let timeout = std::time::Duration::from_secs(PUBKEY_EXCHANGE_TIMEOUT_SECS);
    let n = match tokio::time::timeout(timeout, stream.recv(&mut buf)).await {
        Ok(result) => result?,
        Err(_) => return Err(TransportError::Timeout),
    };
    if n < PUBKEY_EXCHANGE_SIZE {
        return Err(TransportError::RecvFailed(format!(
            "pubkey exchange: expected at least {} bytes, got {}",
            PUBKEY_EXCHANGE_SIZE, n
        )));
    }
    if buf[0] != PUBKEY_EXCHANGE_PREFIX {
        return Err(TransportError::RecvFailed(format!(
            "pubkey exchange: bad prefix 0x{:02X}",
            buf[0]
        )));
    }

    let peer_pubkey = XOnlyPublicKey::from_slice(&buf[1..33])
        .map_err(|e| TransportError::RecvFailed(format!("pubkey exchange: invalid key: {}", e)))?;

    // If peer sent 34+ bytes, read capability flags; otherwise assume full capability
    let peer_capabilities = if n > PUBKEY_EXCHANGE_SIZE {
        PeerCapabilities::from_byte(buf[33])
    } else {
        PeerCapabilities::none()
    };

    Ok(PubkeyExchangeResult {
        peer_pubkey,
        peer_capabilities,
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
    disconnect_tx: Option<DisconnectTx>,
    transport_id: TransportId,
    stats: Arc<BleStats>,
    _max_conns: usize,
    local_pubkey: Option<[u8; 32]>,
    discovery_buffer: Arc<DiscoveryBuffer>,
    local_node_addr: Option<NodeAddr>,
    local_capabilities: PeerCapabilities,
    recv_timeout_secs: u64,
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
                    if stream.supports_bidirectional_pubkey_exchange() {
                        match pubkey_exchange(&stream, our_pubkey, local_capabilities).await {
                            Ok(result) => {
                                let peer_pubkey = result.peer_pubkey;
                                let peer_capabilities = result.peer_capabilities;
                                debug!(addr = %ta, peer_caps = format!("0x{:02x}", peer_capabilities.to_byte()), local_caps = format!("0x{:02x}", local_capabilities.to_byte()), "BLE inbound pubkey exchange complete");
                                discovery_buffer.add_peer_with_pubkey(&addr, peer_pubkey);

                                if !peer_capabilities.can_accept_inbound() {
                                    debug!(
                                        addr = %ta,
                                        "BLE inbound: peer is central-only, accepting inbound connection anyway"
                                    );
                                } else if peer_capabilities.prefers_outbound()
                                    && !local_capabilities.prefers_outbound()
                                {
                                    debug!(
                                        addr = %ta,
                                        "BLE inbound: peer prefers outbound, keeping connection"
                                    );
                                } else if let Some(ref our_addr) = local_node_addr {
                                    let peer_addr = NodeAddr::from_pubkey(&peer_pubkey);
                                    if our_addr < &peer_addr {
                                        debug!(
                                            addr = %ta,
                                            "BLE inbound tie-breaker: dropping (our addr < peer, outbound wins)"
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
                    } else if let Err(e) = send_pubkey_announcement(&stream, our_pubkey, local_capabilities).await {
                        debug!(addr = %ta, error = %e, "BLE inbound pubkey announcement failed");
                        continue;
                    } else {
                        debug!(addr = %ta, local_caps = format!("0x{:02x}", local_capabilities.to_byte()), "BLE inbound pubkey announcement sent; deferring peer identity to Noise");
                    }
                }

                let stream = Arc::new(stream);

                // Spawn receive loop
                let recv_task = tokio::spawn(receive_loop(
                    Arc::clone(&stream),
                    ta.clone(),
                    Arc::clone(&pool),
                    packet_tx.clone(),
                    disconnect_tx.clone(),
                    transport_id,
                    Arc::clone(&stats),
                    recv_mtu,
                    recv_timeout_secs,
                ));

                let conn = BleConnection {
                    stream,
                    recv_task: Some(recv_task),
                    send_mtu,
                    recv_mtu,
                    established_at: tokio::time::Instant::now(),
                    is_static: false,
                    addr,
                    on_drop: None,
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
/// Expects `stream.recv()` to return one complete FMP frame per call.
/// Transport framing (if any) is handled by the stream implementation,
/// not here.
async fn receive_loop<S: BleStream>(
    stream: Arc<S>,
    addr: TransportAddr,
    pool: Arc<Mutex<ConnectionPool<Arc<S>>>>,
    packet_tx: PacketTx,
    disconnect_tx: Option<DisconnectTx>,
    transport_id: TransportId,
    stats: Arc<BleStats>,
    recv_mtu: u16,
    recv_timeout_secs: u64,
) {
    let mut buf = vec![0u8; recv_mtu as usize];
    loop {
        let recv_result = tokio::time::timeout(
            std::time::Duration::from_secs(recv_timeout_secs),
            stream.recv(&mut buf),
        )
        .await;

        match recv_result {
            Ok(Ok(0)) => {
                debug!(addr = %addr, "BLE connection closed by peer");
                break;
            }
            Ok(Ok(n)) => {
                stats.record_recv(n);
                let frame_data = buf[..n].to_vec();
                let packet = ReceivedPacket::new(transport_id, addr.clone(), frame_data);
                if packet_tx.send(packet).await.is_err() {
                    trace!("BLE packet_tx closed, stopping receive loop");
                    break;
                }
            }
            Ok(Err(e)) => {
                let err_str = format!("{e}");
                if err_str.contains("framed message too short") {
                    stats.record_recv_error();
                    continue;
                }
                debug!(addr = %addr, error = %e, "BLE receive error");
                stats.record_recv_error();
                break;
            }
            Err(_) => {
                debug!(
                    addr = %addr,
                    timeout_secs = recv_timeout_secs,
                    "BLE recv timeout — link may be silently dead (bluest L2CAP stall)"
                );
                stats.record_recv_error();
                break;
            }
        }
    }

    if let Some(tx) = disconnect_tx {
        let _ = tx.try_send(TransportDisconnect {
            transport_id,
            remote_addr: addr.clone(),
        });
    }

    // Remove from pool
    let mut pool = pool.lock().await;
    pool.remove(&addr);
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
    recv_timeout_secs: u64,
    local_node_addr: Option<NodeAddr>,
    packet_tx: PacketTx,
    disconnect_tx: Option<DisconnectTx>,
    transport_id: TransportId,
    local_capabilities: PeerCapabilities,
) {
    // Track last probe time per address for cooldown
    let mut last_probed: HashMap<BleAddr, tokio::time::Instant> = HashMap::new();
    let mut yielded_at: HashMap<BleAddr, tokio::time::Instant> = HashMap::new();
    let yield_cooldown = std::time::Duration::from_secs(cooldown_secs * 3);
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
                pending_addrs.sort_by_key(|a| std::cmp::Reverse(a.rssi.unwrap_or(i16::MIN)));
                if let Some(a) = pending_addrs.first().cloned() {
                    a
                } else {
                    continue;
                }
            }
        };

        trace!(addr = %addr, "BLE scan result");
        stats.record_scan_result();

        // Skip if already connected
        {
            let pool_guard = pool.lock().await;
            if pool_guard.contains(&addr.to_transport_addr()) {
                pending_addrs.retain(|a| a != &addr);
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

        if yielded_at
            .get(&addr)
            .is_some_and(|last| last.elapsed() < yield_cooldown)
        {
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

        // connect() now handles GATT-first for CoreBluetooth peers and
        // discovers the dynamic PSM. Pass the configured PSM as fallback.
        let effective_psm = psm;

        // L2CAP connect
        let stream = match tokio::time::timeout(
            std::time::Duration::from_millis(connect_timeout_ms),
            io.connect(&addr, effective_psm),
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

        wait_before_outbound_pubkey_exchange().await;

        // Pubkey exchange, then promote connection to pool
        let ta = addr.to_transport_addr();
        match pubkey_exchange(&stream, &our_pubkey, local_capabilities).await {
            Ok(result) => {
                let peer_pubkey = result.peer_pubkey;
                let peer_capabilities = result.peer_capabilities;
                debug!(addr = %addr, "BLE probe complete");

                if !peer_capabilities.can_accept_inbound() {
                    debug!(
                        addr = %addr,
                        "BLE probe: peer cannot accept inbound, yielding to peer's outbound"
                    );
                    buffer.add_peer_with_pubkey(&addr, peer_pubkey);
                    yielded_at.insert(addr.clone(), tokio::time::Instant::now());
                    drop(stream);
                    // NOTE: do NOT call disconnect_device() here — it tears down
                    // the entire BLE ACL link via BlueZ Device1.Disconnect(), which
                    // kills any L2CAP that auto_connect opened in the interim.
                    // drop(stream) closes only the L2CAP channel.
                    continue;
                }

                if peer_capabilities.prefers_outbound() && !local_capabilities.prefers_outbound() {
                    debug!(
                        addr = %addr,
                        "BLE probe: peer prefers outbound, yielding to peer's outbound"
                    );
                    buffer.add_peer_with_pubkey(&addr, peer_pubkey);
                    yielded_at.insert(addr.clone(), tokio::time::Instant::now());
                    drop(stream);
                    // Do NOT call disconnect_device() — see comment above.
                    continue;
                }

                // Cross-probe tie-breaker: smaller NodeAddr's outbound wins.
                if let Some(ref our_addr) = local_node_addr {
                    let peer_addr = NodeAddr::from_pubkey(&peer_pubkey);
                    if peer_capabilities.can_initiate_outbound() && our_addr >= &peer_addr {
                        debug!(
                            addr = %addr,
                            "BLE probe tie-breaker: yielding to peer's outbound"
                        );
                        buffer.add_peer_with_pubkey(&addr, peer_pubkey);
                        yielded_at.insert(addr.clone(), tokio::time::Instant::now());
                        drop(stream);
                        // Do NOT call disconnect_device() — see comment above.
                        continue;
                    }
                }

                // Promote connection to pool — no second L2CAP connect needed
                let send_mtu = stream.send_mtu();
                let recv_mtu = stream.recv_mtu();
                let stream = Arc::new(stream);

                let recv_task = tokio::spawn(receive_loop(
                    Arc::clone(&stream),
                    ta.clone(),
                    Arc::clone(&pool),
                    packet_tx.clone(),
                    disconnect_tx.clone(),
                    transport_id,
                    Arc::clone(&stats),
                    recv_mtu,
                    recv_timeout_secs,
                ));

                 let drop_addr = addr.clone();
                 let conn = BleConnection {
                     stream,
                     recv_task: Some(recv_task),
                     send_mtu,
                     recv_mtu,
                     established_at: tokio::time::Instant::now(),
                     is_static: false,
                     addr: addr.clone(),
                     on_drop: Some(Box::new({
                         let io = io.clone();
                         move || {
                             let io = io.clone();
                             tokio::spawn(async move {
                                 io.disconnect_device(&drop_addr).await;
                             });
                         }
                     })),
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
                    }
                }
                drop(pool_guard);
                stats.record_connection_established();
                pending_addrs.retain(|a| a != &addr);

                // Report to node layer for auto-connect / handshake
                buffer.add_peer_with_pubkey(&addr, peer_pubkey);
            }
            Err(e) => {
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
    use io::{MockBleIo, MockBleStream};

    fn test_addr(n: u8) -> BleAddr {
        BleAddr {
            adapter: "hci0".to_string(),
            device: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, n],
            rssi: None,
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
    fn test_peer_capabilities_defaults_and_queries() {
        let linux = PeerCapabilities::linux_default();
        assert_eq!(linux.to_byte(), 0x7c);
        assert!(linux.supports_l2cap());
        assert!(linux.supports_gatt());
        assert!(linux.can_accept_inbound());
        assert!(linux.can_initiate_outbound());
        assert!(linux.prefers_l2cap());
        assert!(!linux.prefers_outbound());

        let mac = PeerCapabilities::macos_default();
        assert_eq!(mac.to_byte(), 0x7a);
        assert!(mac.supports_l2cap());
        assert!(mac.supports_gatt());
        assert!(mac.can_accept_inbound());
        assert!(mac.can_initiate_outbound());
        assert!(mac.prefers_outbound());
        assert!(!mac.is_central_only());
    }

    #[test]
    fn test_peer_capabilities_legacy_compatibility() {
        let legacy_none = PeerCapabilities::none();
        assert_eq!(legacy_none.to_byte(), 0x00);
        assert!(legacy_none.supports_l2cap());
        assert!(legacy_none.can_accept_inbound());
        assert!(legacy_none.can_initiate_outbound());
        assert!(!legacy_none.prefers_outbound());

        let legacy_central_only = PeerCapabilities::from_byte(0x01);
        assert_eq!(legacy_central_only.to_byte(), 0x2a);
        assert!(legacy_central_only.supports_l2cap());
        assert!(!legacy_central_only.can_accept_inbound());
        assert!(legacy_central_only.can_initiate_outbound());
        assert!(legacy_central_only.prefers_outbound());
    }

    #[test]
    fn test_gatt_supported_flag_encoding() {
        let linux = PeerCapabilities::linux_default();
        assert!(linux.supports_gatt());
        let roundtrip = PeerCapabilities::from_byte(linux.to_byte());
        assert!(roundtrip.supports_gatt());
        assert_eq!(roundtrip.to_byte(), linux.to_byte());

        let mac = PeerCapabilities::macos_default();
        assert!(mac.supports_gatt());
        assert!(mac.can_accept_inbound()); // macOS can accept inbound when accept_connections=true
        let roundtrip = PeerCapabilities::from_byte(mac.to_byte());
        assert!(roundtrip.supports_gatt());
        assert!(roundtrip.can_accept_inbound()); // roundtrip preserves CAN_PERIPHERAL bit (0x7a)
    }

    #[test]
    fn test_peer_outbound_preference() {
        let linux = PeerCapabilities::linux_default();
        let mac = PeerCapabilities::macos_default();

        assert!(mac.prefers_outbound() && !linux.prefers_outbound());
        assert!(!(linux.prefers_outbound() && !mac.prefers_outbound()));
    }

    #[test]
    fn test_transport_default_mtu() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (transport, _rx) = make_transport(io);
        assert_eq!(transport.mtu(), 2046);
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

    #[tokio::test]
    async fn test_receive_loop_emits_disconnect_event_on_close() {
        let (stream, peer_stream) = MockBleStream::pair(test_addr(1), test_addr(2), 2048);
        let remote_addr = stream.remote_addr().to_transport_addr();
        let pool = Arc::new(Mutex::new(ConnectionPool::new(4)));
        let (packet_tx, _packet_rx) = tokio::sync::mpsc::channel(4);
        let (disconnect_tx, mut disconnect_rx) = crate::transport::disconnect_channel(4);
        let stats = Arc::new(BleStats::new());

        let task = tokio::spawn(receive_loop(
            Arc::new(stream),
            remote_addr.clone(),
            Arc::clone(&pool),
            packet_tx,
            Some(disconnect_tx),
            TransportId::new(1),
            stats,
            2048,
            45,
        ));

        drop(peer_stream);
        task.await.unwrap();

        let disconnect = disconnect_rx.try_recv().expect("disconnect event should be emitted");
        assert_eq!(disconnect.transport_id, TransportId::new(1));
        assert_eq!(disconnect.remote_addr, remote_addr);
    }

    #[tokio::test]
    async fn test_send_async_emits_disconnect_event_on_send_failure() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (mut transport, _rx) = make_transport(io);
        let remote_addr = test_addr(2).to_transport_addr();
        let (stream, peer_stream) = MockBleStream::pair(test_addr(1), test_addr(2), 2048);
        let (disconnect_tx, mut disconnect_rx) = crate::transport::disconnect_channel(4);

        {
            let mut pool = transport.pool.lock().await;
            pool.insert(
                remote_addr.clone(),
                BleConnection {
                    stream: Arc::new(stream),
                    recv_task: None,
                    send_mtu: 2048,
                    recv_mtu: 2048,
                    established_at: tokio::time::Instant::now(),
                    is_static: false,
                    addr: test_addr(2),
                    on_drop: None,
                },
            )
            .unwrap();
        }

        transport.set_disconnect_tx(disconnect_tx);
        drop(peer_stream);

        let err = transport
            .send_async(&remote_addr, b"hello")
            .await
            .expect_err("send should fail when peer side is gone");
        assert!(matches!(err, TransportError::SendFailed(_)));

        let disconnect = disconnect_rx
            .try_recv()
            .expect("disconnect event should be emitted on send failure");
        assert_eq!(disconnect.transport_id, TransportId::new(1));
        assert_eq!(disconnect.remote_addr, remote_addr);
        assert!(!transport.pool.lock().await.contains(&disconnect.remote_addr));
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

    // ========================================================================
    // BLE capability signaling & pubkey exchange tests
    // ========================================================================

    #[tokio::test]
    async fn test_pubkey_exchange_new_format() {
        let secp = secp256k1::Secp256k1::new();
        let sk_a = secp256k1::SecretKey::from_slice(&[1u8; 32]).unwrap();
        let sk_b = secp256k1::SecretKey::from_slice(&[2u8; 32]).unwrap();
        let (pk_a, _) = sk_a.public_key(&secp).x_only_public_key();
        let (pk_b, _) = sk_b.public_key(&secp).x_only_public_key();
        let pubkey_bytes_a: [u8; 32] = pk_a.serialize();
        let pubkey_bytes_b: [u8; 32] = pk_b.serialize();

        let (stream_a, stream_b) = MockBleStream::pair(test_addr(1), test_addr(2), 2048);
        let caps_a = PeerCapabilities::linux_default();
        let caps_b = PeerCapabilities::central_only();

        let (result_a, result_b) = tokio::join!(
            pubkey_exchange(&stream_a, &pubkey_bytes_a, caps_a),
            pubkey_exchange(&stream_b, &pubkey_bytes_b, caps_b),
        );

        let res_a = result_a.expect("exchange on A should succeed");
        let res_b = result_b.expect("exchange on B should succeed");

        // A received B's pubkey and capabilities
        assert_eq!(res_a.peer_pubkey.serialize(), pubkey_bytes_b);
        assert_eq!(res_a.peer_capabilities.to_byte(), caps_b.to_byte());

        // B received A's pubkey and capabilities
        assert_eq!(res_b.peer_pubkey.serialize(), pubkey_bytes_a);
        assert_eq!(res_b.peer_capabilities.to_byte(), caps_a.to_byte());
    }

    #[tokio::test]
    async fn test_pubkey_exchange_legacy_peer() {
        let secp = secp256k1::Secp256k1::new();
        let sk_a = secp256k1::SecretKey::from_slice(&[1u8; 32]).unwrap();
        let sk_b = secp256k1::SecretKey::from_slice(&[2u8; 32]).unwrap();
        let (pk_a, _) = sk_a.public_key(&secp).x_only_public_key();
        let (pk_b, _) = sk_b.public_key(&secp).x_only_public_key();
        let pubkey_bytes_a: [u8; 32] = pk_a.serialize();
        let pubkey_bytes_b: [u8; 32] = pk_b.serialize();

        let (stream_a, stream_b) = MockBleStream::pair(test_addr(1), test_addr(2), 2048);

        // B manually sends 33-byte legacy frame (no capability byte)
        let mut legacy_msg = [0u8; 33];
        legacy_msg[0] = 0x00; // prefix
        legacy_msg[1..33].copy_from_slice(&pubkey_bytes_b);
        stream_b.send(&legacy_msg).await.unwrap();

        // A runs pubkey_exchange (sends 34 bytes with flags)
        let result_a = pubkey_exchange(&stream_a, &pubkey_bytes_a, PeerCapabilities::linux_default()).await;

        let res_a = result_a.expect("exchange should succeed with legacy peer");

        // A should see B's pubkey correctly
        assert_eq!(res_a.peer_pubkey.serialize(), pubkey_bytes_b);

        // A should see PeerCapabilities::none() because legacy peer sent no flags byte
        // (none() means full capability / legacy unrestricted)
        assert_eq!(res_a.peer_capabilities.to_byte(), 0);
        assert!(res_a.peer_capabilities.can_accept_inbound());
        assert!(res_a.peer_capabilities.can_initiate_outbound());
    }

    #[tokio::test]
    async fn test_pubkey_exchange_bad_prefix() {
        let secp = secp256k1::Secp256k1::new();
        let sk_a = secp256k1::SecretKey::from_slice(&[1u8; 32]).unwrap();
        let (pk_a, _) = sk_a.public_key(&secp).x_only_public_key();
        let pubkey_bytes_a: [u8; 32] = pk_a.serialize();

        let (stream_a, stream_b) = MockBleStream::pair(test_addr(1), test_addr(2), 2048);

        // B sends frame with wrong prefix (0xFF)
        let mut bad_msg = [0u8; 33];
        bad_msg[0] = 0xFF;
        bad_msg[1..33].copy_from_slice(&pubkey_bytes_a);
        stream_b.send(&bad_msg).await.unwrap();

        let result = pubkey_exchange(&stream_a, &pubkey_bytes_a, PeerCapabilities::linux_default()).await;
        match result {
            Err(TransportError::RecvFailed(msg)) => {
                assert!(msg.contains("bad prefix"), "error should mention bad prefix, got: {msg}");
            }
            Err(other) => panic!("expected RecvFailed, got: {other:?}"),
            Ok(_) => panic!("should fail with bad prefix"),
        }
    }

    #[tokio::test]
    async fn test_pubkey_exchange_too_short() {
        let secp = secp256k1::Secp256k1::new();
        let sk_a = secp256k1::SecretKey::from_slice(&[1u8; 32]).unwrap();
        let (pk_a, _) = sk_a.public_key(&secp).x_only_public_key();
        let pubkey_bytes_a: [u8; 32] = pk_a.serialize();

        let (stream_a, stream_b) = MockBleStream::pair(test_addr(1), test_addr(2), 2048);

        // B sends only 10 bytes (too short)
        let short_msg = [0xAAu8; 10];
        stream_b.send(&short_msg).await.unwrap();

        let result = pubkey_exchange(&stream_a, &pubkey_bytes_a, PeerCapabilities::linux_default()).await;
        match result {
            Err(TransportError::RecvFailed(msg)) => {
                assert!(
                    msg.contains("expected at least 33"),
                    "error should mention expected size, got: {msg}"
                );
            }
            Err(other) => panic!("expected RecvFailed, got: {other:?}"),
            Ok(_) => panic!("should fail with too-short message"),
        }
    }

    #[test]
    fn test_peer_capabilities_central_only_detection() {
        // central_only: can initiate outbound, cannot accept inbound
        let co = PeerCapabilities::central_only();
        assert!(co.is_central_only());
        assert!(!co.can_accept_inbound());
        assert!(co.can_initiate_outbound());

        // none (legacy): full capability, not central_only
        let none = PeerCapabilities::none();
        assert!(!none.is_central_only());
        assert!(none.can_accept_inbound());
        assert!(none.can_initiate_outbound());

        // linux_default: full symmetric capability
        let linux = PeerCapabilities::linux_default();
        assert!(!linux.is_central_only());
        assert!(linux.can_accept_inbound());
        assert!(linux.can_initiate_outbound());
    }

    #[test]
    fn test_set_local_capabilities() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (mut transport, _rx) = make_transport(io);

        // Should not panic — setter accepts PeerCapabilities
        transport.set_local_capabilities(PeerCapabilities::central_only());
        transport.set_local_capabilities(PeerCapabilities::linux_default());
        transport.set_local_capabilities(PeerCapabilities::none());
    }

    #[test]
    fn test_tiebreaker_central_only_overrides_node_addr() {
        use secp256k1::{Secp256k1, SecretKey};

        let secp = Secp256k1::new();
        let sk_a = SecretKey::from_slice(&[1u8; 32]).unwrap();
        let sk_b = SecretKey::from_slice(&[2u8; 32]).unwrap();
        let (pk_a, _) = sk_a.public_key(&secp).x_only_public_key();
        let (pk_b, _) = sk_b.public_key(&secp).x_only_public_key();

        let addr_a = NodeAddr::from_pubkey(&pk_a);
        let addr_b = NodeAddr::from_pubkey(&pk_b);

        let peer_caps = PeerCapabilities::central_only();

        // peer_caps.can_accept_inbound() == false means the peer can only
        // do outbound. In scan_probe_loop, we should always yield (let
        // the peer's outbound win) regardless of NodeAddr comparison.
        // In accept_loop, we should always keep (their outbound always
        // beats our inbound).

        // Direction 1: A is scanning B (B is central_only)
        // Even if A < B in NodeAddr (A would normally win outbound),
        // B cannot accept inbound, so A must yield.
        let a_should_yield = !peer_caps.can_accept_inbound();
        assert!(a_should_yield, "scan_loop: must yield to central_only peer");

        // Direction 2: A is accepting from B (B is central_only)
        // B can only do outbound, so A must keep regardless of NodeAddr.
        let a_should_keep = !peer_caps.can_accept_inbound();
        assert!(a_should_keep, "accept_loop: must keep connection from central_only peer");

        // Verify with both orderings
        if addr_a < addr_b {
            // A < B: normally A wins outbound, but central_only overrides
            assert!(a_should_yield, "A must yield to B even though A < B");
        } else {
            // B < A: B already wins by NodeAddr, central_only also forces it
            assert!(a_should_yield, "A must yield to B, B is both smaller and central_only");
        }

        // When peer has full capabilities, tie-breaker uses NodeAddr as normal
        let full_caps = PeerCapabilities::linux_default();
        assert!(full_caps.can_accept_inbound());
        assert!(full_caps.can_initiate_outbound());
        // No override needed — normal tie-breaker logic applies
    }
}
