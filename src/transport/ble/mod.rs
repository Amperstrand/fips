//! BLE L2CAP Transport Implementation
//!
//! Provides BLE-based transport for FIPS peer communication using L2CAP
//! Connection-Oriented Channels (CoC) in SeqPacket mode. L2CAP CoC
//! preserves message boundaries (unlike TCP byte streams), so no FMP
//! framing is needed — each send/recv is one FIPS packet.
//!
//! ## Architecture
//!
//! Transport logic (pool, discovery, lifecycle) is separated from the
//! BlueZ/bluer stack via the `BleIo` trait. `BluerIo` provides the real
//! implementation (behind `cfg(bluer_available)`); `MockBleIo` provides
//! an in-memory test double for CI without hardware.
//!
//! ## Connection Pool
//!
//! BLE hardware limits concurrent connections (typically 4-10). The pool
//! enforces a configurable maximum (default 7) with priority eviction:
//! static (configured) peers get priority over discovered peers.

pub mod addr;
pub mod backoff;
pub mod capabilities;
pub mod discovery;
pub mod io;
pub mod pool;
pub mod rate_limit;
pub mod stats;

use super::{
    ConnectionState, DisconnectTx, DiscoveredPeer, PacketTx, ReceivedPacket, Transport,
    TransportAddr, TransportDisconnect, TransportError, TransportId, TransportState, TransportType,
};
use crate::config::BleConfig;
use crate::identity::NodeAddr;
use addr::BleAddr;
pub use capabilities::PeerCapabilities;
use discovery::DiscoveryBuffer;
use io::{BleIo, BleScanner, BleStream};
use pool::{BLE_FRAME_PREFIX_LEN, BleConnection, ConnectionPool};
use rate_limit::BleRateAdapter;
use stats::BleStats;

use secp256k1::XOnlyPublicKey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, trace, warn};

/// Default FIPS L2CAP PSM (Protocol Service Multiplexer).
///
/// 0x0085 (133) is in the dynamic range (0x0080-0x00FF).
pub const DEFAULT_PSM: u16 = 0x0085;

/// Concrete BLE transport type for use in TransportHandle.
///
/// Production builds on glibc-linux use `BluerIo` (real BlueZ stack).
/// Test builds, musl-linux, and non-Linux platforms use `MockBleIo`.
#[cfg(all(bluer_available, not(test)))]
pub type DefaultBleTransport = BleTransport<io::BluerIo>;

#[cfg(all(feature = "ble-macos", not(bluer_available), not(test)))]
pub type DefaultBleTransport = BleTransport<io::BluestIo>;

#[cfg(all(not(bluer_available), not(feature = "ble-macos"), not(test)))]
pub type DefaultBleTransport = BleTransport<io::MockBleIo>;

#[cfg(test)]
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
    transport_id: TransportId,
    name: Option<String>,
    config: BleConfig,
    state: TransportState,
    io: Arc<I>,
    pool: Arc<Mutex<ConnectionPool<Arc<I::Stream>>>>,
    connecting: Arc<Mutex<HashMap<TransportAddr, ConnectingEntry>>>,
    packet_tx: PacketTx,
    accept_task: Option<JoinHandle<()>>,
    scan_probe_task: Option<JoinHandle<()>>,
    discovery_buffer: Arc<DiscoveryBuffer>,
    stats: Arc<BleStats>,
    local_pubkey: Option<[u8; 32]>,
    local_capabilities: PeerCapabilities,
    rate_adapter: Arc<Mutex<BleRateAdapter>>,
    backoff: Arc<Mutex<backoff::PeerBackoff>>,
    disconnect_tx: Option<DisconnectTx>,
}

const RECV_TIMEOUT_SECS: u64 = 30;

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
            accept_task: None,
            scan_probe_task: None,
            discovery_buffer: Arc::new(DiscoveryBuffer::new(transport_id)),
            stats: Arc::new(BleStats::new()),
            local_pubkey: None,
            local_capabilities: PeerCapabilities::linux_default(),
            rate_adapter: Arc::new(Mutex::new(BleRateAdapter::new(initial_rate_bps))),
            backoff: Arc::new(Mutex::new(backoff::PeerBackoff::with_defaults())),
            disconnect_tx: None,
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn stats(&self) -> &Arc<BleStats> {
        &self.stats
    }

    pub fn io(&self) -> &Arc<I> {
        &self.io
    }

    pub fn set_local_pubkey(&mut self, pubkey: [u8; 32]) {
        self.local_pubkey = Some(pubkey);
    }

    pub fn set_local_capabilities(&mut self, caps: PeerCapabilities) {
        self.local_capabilities = caps;
    }

    pub fn set_disconnect_tx(&mut self, tx: DisconnectTx) {
        self.disconnect_tx = Some(tx);
    }

    /// Start the transport asynchronously.
    pub async fn start_async(&mut self) -> Result<(), TransportError> {
        if !self.state.can_start() {
            return Err(TransportError::AlreadyStarted);
        }
        self.state = TransportState::Starting;

        // Warn about contradictory BLE config combinations.
        let scan = self.config.scan();
        let advertise = self.config.advertise();
        let auto_connect = self.config.auto_connect();
        let accept = self.config.accept_connections();

        if !scan && !advertise && !accept {
            warn!(
                "BLE config: scan=false, advertise=false, accept_connections=false — transport will do nothing useful"
            );
        }
        if auto_connect && !scan {
            warn!(
                "BLE config: auto_connect=true but scan=false — auto-connect requires scanning to discover peers"
            );
        }
        if !scan && !advertise {
            warn!(
                "BLE config: scan=false and advertise=false — peers cannot discover or connect to this node"
            );
        }

        let psm = self.config.psm();
        let adapter = self.io.adapter_name().to_string();

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
                    let transport_id = self.transport_id;
                    let stats = Arc::clone(&self.stats);
                    let max_conns = self.config.max_connections();

                    self.accept_task = Some(tokio::spawn(accept_loop(
                        acceptor,
                        pool,
                        packet_tx,
                        self.disconnect_tx.clone(),
                        transport_id,
                        stats,
                        max_conns,
                        self.local_pubkey,
                        Arc::clone(&self.discovery_buffer),
                        local_node_addr,
                        self.local_capabilities,
                        Arc::clone(&self.backoff),
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

        // Start combined scan + probe supervisor
        if self.config.scan() {
            match self.io.start_scanning().await {
                Ok(scanner) => {
                    self.scan_probe_task = Some(tokio::spawn(scan_probe_supervisor::<I>(
                        scanner,
                        Arc::clone(&self.io),
                        Arc::clone(&self.pool),
                        Arc::clone(&self.discovery_buffer),
                        Arc::clone(&self.stats),
                        self.local_pubkey,
                        self.config.psm(),
                        self.config.connect_timeout_ms(),
                        self.config.probe_cooldown_secs(),
                        RECV_TIMEOUT_SECS,
                        local_node_addr,
                        self.packet_tx.clone(),
                        self.disconnect_tx.clone(),
                        self.transport_id,
                        self.local_capabilities,
                        Arc::clone(&self.backoff),
                    )));
                    debug!(adapter = %adapter, "BLE scan+probe supervisor started");
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
        let _ = self.io.stop_advertising().await;

        if let Some(task) = self.accept_task.take() {
            task.abort();
        }

        if let Some(task) = self.scan_probe_task.take() {
            task.abort();
        }

        {
            let mut connecting = self.connecting.lock().await;
            for (_, entry) in connecting.drain() {
                entry.task.abort();
            }
        }

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
    pub async fn send_async(
        &self,
        addr: &TransportAddr,
        data: &[u8],
    ) -> Result<usize, TransportError> {
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

        match conn.stream.send(data).await {
            Ok(()) => {
                self.stats.record_send(data.len());
                Ok(data.len())
            }
            Err(e) => {
                self.stats.record_send_error();
                drop(pool);
                let mut pool = self.pool.lock().await;
                pool.remove(addr);
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

    /// Connect to a remote BLE device inline (blocking the caller).
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

        // Pre-handshake pubkey exchange
        if let Some(ref our_pubkey) = self.local_pubkey {
            match pubkey_exchange(&stream, our_pubkey, self.local_capabilities).await {
                Ok(result) => {
                    debug!(addr = %addr, "BLE outbound pubkey exchange complete");
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
            self.disconnect_tx.clone(),
            self.transport_id,
            Arc::clone(&self.stats),
            recv_mtu,
            RECV_TIMEOUT_SECS,
        ));

        let io = Arc::clone(&self.io);
        let drop_addr = ble_addr.clone();
        let conn = BleConnection {
            stream,
            recv_task: Some(recv_task),
            send_mtu,
            recv_mtu,
            established_at: tokio::time::Instant::now(),
            is_static: false,
            addr: ble_addr.clone(),
            on_drop: Some(Box::new(move || {
                let io = io.clone();
                let addr = drop_addr;
                tokio::spawn(async move {
                    io.disconnect_device(&addr).await;
                });
            })),
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
    pub async fn connect_async(&self, addr: &TransportAddr) -> Result<(), TransportError> {
        {
            let pool = self.pool.lock().await;
            if pool.contains(addr) {
                return Ok(());
            }
        }

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
        let local_capabilities = self.local_capabilities;
        let discovery_buffer = Arc::clone(&self.discovery_buffer);
        let disconnect_tx = self.disconnect_tx.clone();

        let task = tokio::spawn(async move {
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                io.connect(&ble_addr, psm),
            )
            .await;

            match result {
                Ok(Ok(stream)) => {
                    if let Some(ref our_pubkey) = local_pubkey {
                        match pubkey_exchange(&stream, our_pubkey, local_capabilities).await {
                            Ok(result) => {
                                debug!(addr = %addr_clone, "BLE outbound pubkey exchange complete");
                                discovery_buffer
                                    .add_peer_with_pubkey(&ble_addr, result.peer_pubkey);
                            }
                            Err(e) => {
                                warn!(addr = %addr_clone, error = %e, "BLE outbound pubkey exchange failed");
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
                        packet_tx.clone(),
                        disconnect_tx.clone(),
                        transport_id,
                        Arc::clone(&stats),
                        recv_mtu,
                        RECV_TIMEOUT_SECS,
                    ));

                    let drop_addr = ble_addr.clone();
                    let conn = BleConnection {
                        stream,
                        recv_task: Some(recv_task),
                        send_mtu,
                        recv_mtu,
                        established_at: tokio::time::Instant::now(),
                        is_static: false,
                        addr: ble_addr,
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
        if let Ok(pool) = self.pool.try_lock()
            && pool.contains(addr)
        {
            return ConnectionState::Connected;
        }

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
            drop(conn);
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

    fn close_connection(&self, _addr: &TransportAddr) {}
}

// ============================================================================
// Background Tasks
// ============================================================================

const PUBKEY_EXCHANGE_PREFIX: u8 = 0x00;
const PUBKEY_EXCHANGE_SIZE: usize = 33;
const PUBKEY_EXCHANGE_SIZE_EXTENDED: usize = PUBKEY_EXCHANGE_SIZE + 1;
const PUBKEY_EXCHANGE_TIMEOUT_SECS: u64 = 5;
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

/// Exchange public keys over a newly established L2CAP connection.
///
struct PubkeyExchangeResult {
    peer_pubkey: XOnlyPublicKey,
    peer_capabilities: PeerCapabilities,
}

async fn pubkey_exchange<S: BleStream>(
    stream: &S,
    local_pubkey: &[u8; 32],
    local_capabilities: PeerCapabilities,
) -> Result<PubkeyExchangeResult, TransportError> {
    let mut msg = [0u8; PUBKEY_EXCHANGE_SIZE_EXTENDED];
    msg[0] = PUBKEY_EXCHANGE_PREFIX;
    msg[1..33].copy_from_slice(local_pubkey);
    msg[33] = local_capabilities.to_byte();
    stream.send(&msg).await?;

    // Receive peer's pubkey (with timeout to prevent indefinite blocking)
    let mut buf = [0u8; PUBKEY_EXCHANGE_SIZE_EXTENDED];
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

    let peer_capabilities = if n >= PUBKEY_EXCHANGE_SIZE_EXTENDED {
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
    backoff: Arc<Mutex<backoff::PeerBackoff>>,
) where
    A: io::BleAcceptor,
    A::Stream: 'static,
{
    loop {
        match acceptor.accept().await {
            Ok(stream) => {
                let addr = stream.remote_addr().clone();
                let ta = addr.to_transport_addr();

                if backoff.lock().await.is_denied(&addr) {
                    debug!(addr = %ta, "BLE inbound: denied, dropping");
                    continue;
                }

                {
                    let pool_guard = pool.lock().await;
                    if pool_guard.contains(&ta) {
                        debug!(addr = %ta, "BLE inbound: already connected, skipping");
                        continue;
                    }
                }

                let send_mtu = stream.send_mtu();
                let recv_mtu = stream.recv_mtu();

                if let Some(ref our_pubkey) = local_pubkey {
                    if stream.supports_bidirectional_pubkey_exchange() {
                        match pubkey_exchange(&stream, our_pubkey, local_capabilities).await {
                            Ok(result) => {
                                debug!(addr = %ta, "BLE inbound pubkey exchange complete");
                                discovery_buffer.add_peer_with_pubkey(&addr, result.peer_pubkey);

                                let peer_capabilities = result.peer_capabilities;
                                if !peer_capabilities.can_accept_inbound() {
                                    debug!(addr = %ta, "BLE inbound: peer is central-only, accepting inbound connection anyway");
                                } else if peer_capabilities.prefers_outbound()
                                    && !local_capabilities.prefers_outbound()
                                {
                                    debug!(addr = %ta, "BLE inbound: peer prefers outbound, keeping connection");
                                } else if let Some(ref our_addr) = local_node_addr {
                                    let peer_addr = NodeAddr::from_pubkey(&result.peer_pubkey);
                                    if our_addr < &peer_addr {
                                        debug!(addr = %ta, "BLE inbound tie-breaker: dropping (our addr < peer, outbound wins)");
                                        backoff.lock().await.clear(&addr);
                                        continue;
                                    }
                                }
                            }
                            Err(e) => {
                                debug!(addr = %ta, error = %e, "BLE inbound pubkey exchange failed");
                                let denied = backoff.lock().await.record_failure(&addr);
                                if denied {
                                    warn!(addr = %ta, "BLE inbound: auto-denied after repeated failures");
                                }
                                continue;
                            }
                        }
                    } else if let Err(e) =
                        send_pubkey_announcement(&stream, our_pubkey, local_capabilities).await
                    {
                        debug!(addr = %ta, error = %e, "BLE inbound pubkey announcement failed");
                        continue;
                    } else {
                        debug!(addr = %ta, "BLE inbound pubkey announcement sent; deferring peer identity to Noise");
                    }
                }

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
                    RECV_TIMEOUT_SECS,
                ));

                let backoff_addr = addr.clone();
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
                backoff.lock().await.clear(&backoff_addr);
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
/// Each recv returns one complete FIPS packet.
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
                debug!(addr = %addr, timeout_secs = recv_timeout_secs, "BLE recv timeout — link may be silently dead");
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

    let mut pool = pool.lock().await;
    pool.remove(&addr);
}

/// macOS byte-stream receive loop — uses the FMP common header to
/// reassemble complete packets from CoreBluetooth's L2CAP byte stream.
///
/// FMP prefix: `[ver+phase:1][flags:1][payload_len:2 LE]`
/// Phase determines remaining byte count after the 4-byte prefix.
#[cfg(target_os = "macos")]
async fn receive_loop_fmp<S: BleStream>(
    stream: &S,
    addr: &TransportAddr,
    packet_tx: &PacketTx,
    transport_id: TransportId,
    stats: &BleStats,
    recv_mtu: u16,
) {
    const FMP_PREFIX: usize = 4;
    const PHASE_ESTABLISHED: u8 = 0x0;
    const PHASE_MSG1: u8 = 0x1;
    const PHASE_MSG2: u8 = 0x2;
    const MSG1_WIRE_SIZE: usize = 114;
    const MSG2_WIRE_SIZE: usize = 69;
    const ESTABLISHED_REMAINING_HEADER: usize = 12;
    const AEAD_TAG_SIZE: usize = 16;

    let mut accum: Vec<u8> = Vec::with_capacity(recv_mtu as usize);
    let mut tmp = vec![0u8; recv_mtu as usize];

    async fn fill<S: BleStream>(
        stream: &S,
        accum: &mut Vec<u8>,
        need: usize,
        tmp: &mut [u8],
        addr: &TransportAddr,
        stats: &BleStats,
    ) -> bool {
        while accum.len() < need {
            match stream.recv(tmp).await {
                Ok(0) => {
                    debug!(addr = %addr, "BLE connection closed by peer");
                    return false;
                }
                Ok(n) => accum.extend_from_slice(&tmp[..n]),
                Err(e) => {
                    debug!(addr = %addr, error = %e, "BLE receive error");
                    stats.record_recv_error();
                    return false;
                }
            }
        }
        true
    }

    loop {
        if !fill(stream, &mut accum, FMP_PREFIX, &mut tmp, addr, stats).await {
            return;
        }

        let version = accum[0] >> 4;
        let phase = accum[0] & 0x0F;
        let payload_len = u16::from_le_bytes([accum[2], accum[3]]) as usize;

        if version != 0 {
            debug!(addr = %addr, version, "BLE FMP unknown version, dropping");
            stats.record_recv_error();
            return;
        }

        let total = match phase {
            PHASE_MSG1 => MSG1_WIRE_SIZE,
            PHASE_MSG2 => MSG2_WIRE_SIZE,
            PHASE_ESTABLISHED => {
                FMP_PREFIX + ESTABLISHED_REMAINING_HEADER + payload_len + AEAD_TAG_SIZE
            }
            _ => {
                debug!(addr = %addr, phase, "BLE FMP unknown phase, dropping");
                stats.record_recv_error();
                return;
            }
        };

        if !fill(stream, &mut accum, total, &mut tmp, addr, stats).await {
            return;
        }

        let packet_data: Vec<u8> = accum.drain(..total).collect();
        stats.record_recv(packet_data.len());
        let packet = ReceivedPacket::new(transport_id, addr.clone(), packet_data);
        if packet_tx.send(packet).await.is_err() {
            trace!("BLE packet_tx closed, stopping receive loop");
            return;
        }
    }
}

/// Scanner supervisor: wraps scan_probe_loop and auto-restarts on
/// bluetoothd restart or scanner stream termination.
#[allow(clippy::too_many_arguments)]
async fn scan_probe_supervisor<I: io::BleIo>(
    scanner: I::Scanner,
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
    backoff: Arc<Mutex<backoff::PeerBackoff>>,
) {
    let mut scanner = Some(scanner);
    let mut restart_backoff = tokio::time::Duration::from_secs(2);
    const MAX_RESTART_BACKOFF: tokio::time::Duration = tokio::time::Duration::from_secs(60);

    loop {
        if let Some(s) = scanner.take() {
            scan_probe_loop::<I>(
                s,
                Arc::clone(&io),
                Arc::clone(&pool),
                Arc::clone(&buffer),
                Arc::clone(&stats),
                local_pubkey,
                psm,
                connect_timeout_ms,
                cooldown_secs,
                recv_timeout_secs,
                local_node_addr,
                packet_tx.clone(),
                disconnect_tx.clone(),
                transport_id,
                local_capabilities,
                Arc::clone(&backoff),
            )
            .await;
        }

        warn!(
            adapter = io.adapter_name(),
            restart_after = ?restart_backoff,
            "BLE scanner ended, restarting"
        );

        tokio::time::sleep(restart_backoff).await;
        restart_backoff = (restart_backoff * 2).min(MAX_RESTART_BACKOFF);

        match io.start_scanning().await {
            Ok(s) => {
                info!(adapter = io.adapter_name(), "BLE scanner restarted");
                scanner = Some(s);
                restart_backoff = tokio::time::Duration::from_secs(2);
            }
            Err(e) => {
                warn!(
                    adapter = io.adapter_name(),
                    error = %e,
                    "BLE scanner restart failed, will retry"
                );
            }
        }
    }
}

/// Combined scan + probe loop.
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
    backoff: Arc<Mutex<backoff::PeerBackoff>>,
) {
    let mut last_probed: HashMap<BleAddr, tokio::time::Instant> = HashMap::new();
    let mut pending_addrs: Vec<BleAddr> = Vec::new();
    let cooldown = std::time::Duration::from_secs(cooldown_secs);
    let retry_interval = tokio::time::interval(std::time::Duration::from_secs(cooldown_secs));
    tokio::pin!(retry_interval);
    retry_interval.tick().await;

    loop {
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

        trace!(addr = %addr, "BLE scan result");
        stats.record_scan_result();

        if backoff.lock().await.is_denied(&addr) {
            trace!(addr = %addr, "BLE scan result: denied, skipping");
            continue;
        }

        if backoff.lock().await.is_in_backoff(&addr) {
            continue;
        }

        {
            let pool_guard = pool.lock().await;
            if pool_guard.contains(&addr.to_transport_addr()) {
                pending_addrs.retain(|a| a != &addr);
                continue;
            }
        }

        if !pending_addrs.contains(&addr) {
            pending_addrs.push(addr.clone());
        }

        if last_probed
            .get(&addr)
            .is_some_and(|last| last.elapsed() < cooldown)
        {
            continue;
        }

        last_probed.insert(addr.clone(), tokio::time::Instant::now());

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
                let denied = backoff.lock().await.record_failure(&addr);
                if denied {
                    warn!(addr = %addr, "BLE probe: auto-denied after repeated connect failures");
                    pending_addrs.retain(|a| a != &addr);
                }
                continue;
            }
            Err(_) => {
                debug!(addr = %addr, "BLE probe connect timeout");
                stats.record_connect_timeout();
                let denied = backoff.lock().await.record_failure(&addr);
                if denied {
                    warn!(addr = %addr, "BLE probe: auto-denied after repeated timeouts");
                    pending_addrs.retain(|a| a != &addr);
                }
                continue;
            }
        };

        let ta = addr.to_transport_addr();
        wait_before_outbound_pubkey_exchange().await;
        match pubkey_exchange(&stream, &our_pubkey, local_capabilities).await {
            Ok(result) => {
                debug!(addr = %addr, "BLE probe complete");

                let peer_capabilities = result.peer_capabilities;
                if !peer_capabilities.can_accept_inbound() {
                    debug!(addr = %addr, "BLE probe: peer cannot accept inbound, yielding to peer's outbound");
                    buffer.add_peer_with_pubkey(&addr, result.peer_pubkey);
                    continue;
                }

                if peer_capabilities.prefers_outbound() && !local_capabilities.prefers_outbound() {
                    debug!(addr = %addr, "BLE probe: peer prefers outbound, yielding to peer's outbound");
                    buffer.add_peer_with_pubkey(&addr, result.peer_pubkey);
                    continue;
                }

                if let Some(ref our_addr) = local_node_addr {
                    let peer_addr = NodeAddr::from_pubkey(&result.peer_pubkey);
                    if peer_capabilities.can_initiate_outbound() && our_addr >= &peer_addr {
                        debug!(addr = %addr, "BLE probe tie-breaker: yielding to peer's outbound");
                        buffer.add_peer_with_pubkey(&addr, result.peer_pubkey);
                        continue;
                    }
                }

                let peer_pubkey = result.peer_pubkey;

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
                backoff.lock().await.clear(&addr);

                buffer.add_peer_with_pubkey(&addr, peer_pubkey);
            }
            Err(e) => {
                debug!(addr = %addr, error = %e, "BLE probe pubkey exchange failed");
                let denied = backoff.lock().await.record_failure(&addr);
                if denied {
                    warn!(addr = %addr, "BLE probe: auto-denied after repeated pubkey failures");
                    pending_addrs.retain(|a| a != &addr);
                }
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
    use io::MockBleIo;
    use pool::BleConnection;

    fn test_addr(n: u8) -> BleAddr {
        BleAddr {
            adapter: "hci0".to_string(),
            device: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, n],
        }
    }

    fn make_transport(
        io: MockBleIo,
    ) -> (
        BleTransport<MockBleIo>,
        tokio::sync::mpsc::Receiver<ReceivedPacket>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let config = BleConfig::default();
        let transport = BleTransport::new(TransportId::new(1), None, config, io, tx);
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

        transport.io.inject_scan_result(test_addr(2)).await;
        transport.io.inject_scan_result(test_addr(3)).await;

        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(6)).await;
        tokio::task::yield_now().await;

        let peers = transport.discovery_buffer.take();
        assert_eq!(peers.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_scan_deduplicates() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (mut transport, _rx) = make_transport(io);
        transport.start_async().await.unwrap();

        transport.io.inject_scan_result(test_addr(2)).await;
        transport.io.inject_scan_result(test_addr(2)).await;

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
        assert_eq!(
            transport.connection_state_sync(&addr),
            ConnectionState::None
        );
    }

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

        let (smaller, larger) = if addr_a < addr_b {
            (addr_a, addr_b)
        } else {
            (addr_b, addr_a)
        };

        assert!(smaller < larger, "test setup: smaller < larger");
    }

    #[tokio::test]
    async fn test_receive_loop_emits_disconnect_event_on_close() {
        let (stream_a, stream_b) = io::MockBleStream::pair(test_addr(1), test_addr(2), 2048);
        let remote_addr = stream_a.remote_addr().to_transport_addr();
        let pool = Arc::new(Mutex::new(ConnectionPool::new(4)));
        let (packet_tx, _packet_rx) = tokio::sync::mpsc::channel(4);
        let (disconnect_tx, mut disconnect_rx) = crate::transport::disconnect_channel(4);
        let stats = Arc::new(BleStats::new());

        let task = tokio::spawn(receive_loop(
            Arc::new(stream_a),
            remote_addr.clone(),
            Arc::clone(&pool),
            packet_tx,
            Some(disconnect_tx),
            TransportId::new(1),
            stats,
            2048,
            30,
        ));

        drop(stream_b);
        task.await.unwrap();

        let disconnect = disconnect_rx
            .try_recv()
            .expect("disconnect event should be emitted");
        assert_eq!(disconnect.transport_id, TransportId::new(1));
        assert_eq!(disconnect.remote_addr, remote_addr);
    }

    #[tokio::test]
    async fn test_send_async_emits_disconnect_event_on_send_failure() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let (mut transport, _rx) = make_transport(io);
        let remote_addr = test_addr(2).to_transport_addr();
        let (stream_a, stream_b) = io::MockBleStream::pair(test_addr(1), test_addr(2), 2048);
        let (disconnect_tx, mut disconnect_rx) = crate::transport::disconnect_channel(4);

        {
            let mut pool = transport.pool.lock().await;
            pool.insert(
                remote_addr.clone(),
                BleConnection {
                    stream: Arc::new(stream_a),
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
        drop(stream_b);

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
        assert!(
            !transport
                .pool
                .lock()
                .await
                .contains(&disconnect.remote_addr)
        );
    }
}
