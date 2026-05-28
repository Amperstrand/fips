//! RFCOMM (Bluetooth Classic Serial) Transport Implementation
//!
//! Provides RFCOMM serial transport for FIPS peer communication over
//! Bluetooth Classic. Designed for AR3012 chips on OpenWrt routers
//! running musl libc.
//!
//! ## Architecture
//!
//! Operates on `/dev/rfcommN` device files created externally by procd
//! init scripts (e.g., `rfcomm connect`, `rfcomm watch`). FIPS opens
//! these serial port files and uses length-prefix framing (2-byte BE
//! length + payload) to recover packet boundaries from the byte stream.
//!
//! ## Modes
//!
//! - **Server mode**: Polls for new `/dev/rfcommN` device files appearing
//!   (created externally when peers connect via `rfcomm watch`).
//! - **Client mode**: Opens a configured `/dev/rfcommN` device file
//!   (created externally via `rfcomm connect`).

pub mod framing;
pub mod stats;

use super::{
    ConnectionState, DiscoveredPeer, PacketTx, ReceivedPacket, Transport, TransportAddr,
    TransportError, TransportId, TransportState, TransportType,
};
use crate::config::RfcommConfig;
use framing::{read_framed_packet, write_framed_packet};
use stats::RfcommStats;

use secp256k1::XOnlyPublicKey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::fs::OpenOptions;
use tokio::io::BufReader;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, trace, warn};

/// Pubkey exchange constants (matches BLE pattern).
const PUBKEY_EXCHANGE_PREFIX: u8 = 0x00;
const PUBKEY_EXCHANGE_SIZE: usize = 33; // prefix(1) + pubkey(32)
const PUBKEY_EXCHANGE_TIMEOUT_SECS: u64 = 15;

// ============================================================================
// Connection Pool
// ============================================================================

/// State for a single RFCOMM serial connection.
struct RfcommConnection {
    /// Write half of the serial port file.
    writer: Arc<Mutex<tokio::fs::File>>,
    /// Receive task for this connection.
    recv_task: JoinHandle<()>,
}

/// Shared connection pool.
type ConnectionPool = Arc<Mutex<HashMap<TransportAddr, RfcommConnection>>>;

// ============================================================================
// RFCOMM Transport
// ============================================================================

/// RFCOMM serial transport for FIPS.
///
/// Provides connection-oriented, reliable byte stream delivery over
/// Bluetooth Classic RFCOMM serial ports. Uses length-prefix framing
/// to recover packet boundaries.
pub struct RfcommTransport {
    /// Unique transport identifier.
    transport_id: TransportId,
    /// Optional instance name (for named instances in config).
    name: Option<String>,
    /// Configuration.
    config: RfcommConfig,
    /// Current state.
    state: TransportState,
    /// Connection pool: addr -> established connections.
    pool: ConnectionPool,
    /// Channel for delivering received packets to Node.
    packet_tx: PacketTx,
    /// Server polling task handle (if in server mode).
    poll_task: Option<JoinHandle<()>>,
    /// Transport statistics.
    stats: Arc<RfcommStats>,
    /// Local node's Nostr public key (for pubkey exchange).
    local_pubkey: Option<[u8; 32]>,
}

impl RfcommTransport {
    /// Create a new RFCOMM transport.
    pub fn new(
        transport_id: TransportId,
        name: Option<String>,
        config: RfcommConfig,
        packet_tx: PacketTx,
    ) -> Self {
        Self {
            transport_id,
            name,
            config,
            state: TransportState::Configured,
            pool: Arc::new(Mutex::new(HashMap::new())),
            packet_tx,
            poll_task: None,
            stats: Arc::new(RfcommStats::new()),
            local_pubkey: None,
        }
    }

    /// Get the instance name (if configured as a named instance).
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Get the transport statistics.
    pub fn stats(&self) -> &Arc<RfcommStats> {
        &self.stats
    }

    /// Set the local node's public key (for pubkey exchange).
    pub fn set_local_pubkey(&mut self, pubkey: [u8; 32]) {
        self.local_pubkey = Some(pubkey);
    }

    /// Start the transport asynchronously.
    pub async fn start_async(&mut self) -> Result<(), TransportError> {
        if !self.state.can_start() {
            return Err(TransportError::AlreadyStarted);
        }

        self.state = TransportState::Starting;

        // In client mode, open the configured device immediately
        if self.config.mode() == "client" {
            if let Some(ref device) = self.config.device {
                match self.open_device(device).await {
                    Ok(()) => {
                        debug!(
                            transport_id = %self.transport_id,
                            device = %device,
                            "RFCOMM client: opened device"
                        );
                    }
                    Err(e) => {
                        warn!(
                            transport_id = %self.transport_id,
                            device = %device,
                            error = %e,
                            "RFCOMM client: failed to open device (will retry)"
                        );
                    }
                }
            }
        }

        // In server mode, start polling for new /dev/rfcommN devices
        if self.config.mode() == "server" {
            let transport_id = self.transport_id;
            let packet_tx = self.packet_tx.clone();
            let pool = self.pool.clone();
            let stats = self.stats.clone();
            let local_pubkey = self.local_pubkey;
            let poll_interval = std::time::Duration::from_secs(2);
            let known_devices = self.collect_known_devices().await;

            let poll_task = tokio::spawn(async move {
                server_poll_loop(
                    transport_id,
                    packet_tx,
                    pool,
                    stats,
                    local_pubkey,
                    poll_interval,
                    known_devices,
                )
                .await;
            });
            self.poll_task = Some(poll_task);
        }

        self.state = TransportState::Up;

        if let Some(ref name) = self.name {
            info!(
                name = %name,
                mode = %self.config.mode(),
                mtu = self.config.mtu(),
                "RFCOMM transport started"
            );
        } else {
            info!(
                mode = %self.config.mode(),
                mtu = self.config.mtu(),
                "RFCOMM transport started"
            );
        }

        Ok(())
    }

    /// Stop the transport asynchronously.
    pub async fn stop_async(&mut self) -> Result<(), TransportError> {
        if !self.state.is_operational() {
            return Err(TransportError::NotStarted);
        }

        // Abort server poll task
        if let Some(task) = self.poll_task.take() {
            task.abort();
            let _ = task.await;
        }

        // Close all established connections
        let mut pool = self.pool.lock().await;
        for (addr, conn) in pool.drain() {
            conn.recv_task.abort();
            let _ = conn.recv_task.await;
            debug!(
                transport_id = %self.transport_id,
                remote_addr = %addr,
                "RFCOMM connection closed (transport stopping)"
            );
        }
        drop(pool);

        self.state = TransportState::Down;

        info!(
            transport_id = %self.transport_id,
            "RFCOMM transport stopped"
        );

        Ok(())
    }

    /// Send a packet asynchronously.
    pub async fn send_async(
        &self,
        addr: &TransportAddr,
        data: &[u8],
    ) -> Result<usize, TransportError> {
        if !self.state.is_operational() {
            return Err(TransportError::NotStarted);
        }

        // MTU check
        let mtu = self.config.mtu() as usize;
        if data.len() > mtu {
            return Err(TransportError::MtuExceeded {
                packet_size: data.len(),
                mtu: self.config.mtu(),
            });
        }

        // Get connection writer
        let writer = {
            let pool = self.pool.lock().await;
            pool.get(addr)
                .map(|c| c.writer.clone())
                .ok_or_else(|| TransportError::SendFailed("no connection to address".into()))?
        };

        // Write framed packet
        let mut w = writer.lock().await;
        match write_framed_packet(&mut *w, data).await {
            Ok(()) => {
                self.stats.record_send(data.len());
                trace!(
                    transport_id = %self.transport_id,
                    remote_addr = %addr,
                    bytes = data.len(),
                    "RFCOMM packet sent"
                );
                Ok(data.len())
            }
            Err(e) => {
                self.stats.record_send_error();
                drop(w);
                // Remove failed connection from pool
                let mut pool = self.pool.lock().await;
                if let Some(conn) = pool.remove(addr) {
                    conn.recv_task.abort();
                }
                Err(TransportError::SendFailed(format!("{}", e)))
            }
        }
    }

    /// Initiate a non-blocking connection to a remote address.
    ///
    /// For RFCOMM, this opens the configured device file and starts
    /// the read loop. Returns Ok immediately if already connected.
    pub async fn connect_async(&self, addr: &TransportAddr) -> Result<(), TransportError> {
        if !self.state.is_operational() {
            return Err(TransportError::NotStarted);
        }

        // Already connected?
        {
            let pool = self.pool.lock().await;
            if pool.contains_key(addr) {
                return Ok(());
            }
        }

        // For RFCOMM, the device file path IS the address
        let device_path = addr
            .as_str()
            .ok_or_else(|| TransportError::InvalidAddress("not valid UTF-8".into()))?;

        self.open_and_register_device(device_path, addr.clone()).await
    }

    /// Query the state of a connection to a remote address.
    pub fn connection_state_sync(&self, addr: &TransportAddr) -> ConnectionState {
        if let Ok(pool) = self.pool.try_lock() {
            if pool.contains_key(addr) {
                return ConnectionState::Connected;
            }
        }
        ConnectionState::None
    }

    /// Close a specific connection asynchronously.
    pub async fn close_connection_async(&self, addr: &TransportAddr) {
        let mut pool = self.pool.lock().await;
        if let Some(conn) = pool.remove(addr) {
            conn.recv_task.abort();
            self.stats.record_connection_closed();
            debug!(
                transport_id = %self.transport_id,
                remote_addr = %addr,
                "RFCOMM connection closed"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Open a device file and register it in the connection pool.
    async fn open_device(&self, device_path: &str) -> Result<(), TransportError> {
        let addr = TransportAddr::from_string(device_path);
        self.open_and_register_device(device_path, addr).await
    }

    /// Open a device, start read loop, and insert into pool.
    async fn open_and_register_device(
        &self,
        device_path: &str,
        addr: TransportAddr,
    ) -> Result<(), TransportError> {
        // Check if already connected
        {
            let pool = self.pool.lock().await;
            if pool.contains_key(&addr) {
                return Ok(());
            }
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(device_path)
            .await
            .map_err(|e| {
                TransportError::StartFailed(format!("failed to open {}: {}", device_path, e))
            })?;

        // Split into reader and writer using separate file handles
        // We need two file handles since tokio::fs::File doesn't support split.
        // Instead, we clone the file for writing and use the original for reading.
        let file_std = file.into_std().await;

        let writer_std = file_std
            .try_clone()
            .map_err(|e| TransportError::StartFailed(format!("clone fd: {}", e)))?;

        let reader = tokio::fs::File::from_std(file_std);
        let writer = tokio::fs::File::from_std(writer_std);

        let writer = Arc::new(Mutex::new(writer));

        let transport_id = self.transport_id;
        let packet_tx = self.packet_tx.clone();
        let pool = self.pool.clone();
        let stats = self.stats.clone();
        let remote_addr = addr.clone();
        let local_pubkey = self.local_pubkey;

        let recv_writer = writer.clone();
        let recv_task = tokio::spawn(async move {
            rfcomm_receive_loop(
                reader,
                recv_writer,
                transport_id,
                remote_addr.clone(),
                packet_tx,
                pool,
                stats,
                local_pubkey,
            )
            .await;
        });

        let conn = RfcommConnection {
            writer,
            recv_task,
        };

        let mut pool = self.pool.lock().await;
        pool.insert(addr, conn);

        self.stats.record_connection_established();

        debug!(
            transport_id = %self.transport_id,
            device = %device_path,
            "RFCOMM connection established"
        );

        Ok(())
    }

    /// Collect currently known device files from the pool (for server poll dedup).
    async fn collect_known_devices(&self) -> Vec<String> {
        let pool = self.pool.lock().await;
        pool.keys()
            .filter_map(|addr| addr.as_str().map(|s| s.to_string()))
            .collect()
    }
}

impl Transport for RfcommTransport {
    fn transport_id(&self) -> TransportId {
        self.transport_id
    }

    fn transport_type(&self) -> &TransportType {
        &TransportType::SERIAL
    }

    fn state(&self) -> TransportState {
        self.state
    }

    fn mtu(&self) -> u16 {
        self.config.mtu()
    }

    fn start(&mut self) -> Result<(), TransportError> {
        Err(TransportError::NotSupported(
            "use start_async() for RFCOMM transport".into(),
        ))
    }

    fn stop(&mut self) -> Result<(), TransportError> {
        Err(TransportError::NotSupported(
            "use stop_async() for RFCOMM transport".into(),
        ))
    }

    fn send(&self, _addr: &TransportAddr, _data: &[u8]) -> Result<(), TransportError> {
        Err(TransportError::NotSupported(
            "use send_async() for RFCOMM transport".into(),
        ))
    }

    fn discover(&self) -> Result<Vec<DiscoveredPeer>, TransportError> {
        // RFCOMM has no discovery mechanism — devices are configured or
        // appear via external rfcomm commands.
        Ok(Vec::new())
    }

    fn auto_connect(&self) -> bool {
        self.config.auto_connect()
    }

    fn accept_connections(&self) -> bool {
        self.config.accept_connections()
    }
}

// ============================================================================
// Server Poll Loop
// ============================================================================

/// Server mode: periodically scan for new `/dev/rfcommN` device files.
///
/// When a new device appears, opens it and starts a receive loop.
/// This handles the case where `rfcomm watch` creates device files
/// externally when Bluetooth peers connect.
async fn server_poll_loop(
    transport_id: TransportId,
    packet_tx: PacketTx,
    pool: ConnectionPool,
    stats: Arc<RfcommStats>,
    local_pubkey: Option<[u8; 32]>,
    poll_interval: std::time::Duration,
    mut known_devices: Vec<String>,
) {
    debug!(transport_id = %transport_id, "RFCOMM server poll loop starting");

    loop {
        tokio::time::sleep(poll_interval).await;

        // Scan /dev for rfcommN device files
        let mut current_devices = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/dev") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("rfcomm") {
                    let path = format!("/dev/{}", name_str);
                    current_devices.push(path);
                }
            }
        }

        // Find new devices
        for device in &current_devices {
            if !known_devices.contains(device) {
                debug!(
                    transport_id = %transport_id,
                    device = %device,
                    "RFCOMM server: new device detected"
                );

                let addr = TransportAddr::from_string(device);

                // Check if already in pool
                {
                    let pool_guard = pool.lock().await;
                    if pool_guard.contains_key(&addr) {
                        continue;
                    }
                }

                // Open device
                let file = match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(device)
                    .await
                {
                    Ok(f) => f,
                    Err(e) => {
                        warn!(
                            transport_id = %transport_id,
                            device = %device,
                            error = %e,
                            "RFCOMM server: failed to open device"
                        );
                        continue;
                    }
                };

                let file_std = file.into_std().await;

                let writer_std = match file_std.try_clone() {
                    Ok(f) => f,
                    Err(e) => {
                        warn!(
                            transport_id = %transport_id,
                            device = %device,
                            error = %e,
                            "RFCOMM server: failed to clone fd"
                        );
                        continue;
                    }
                };

                let reader = tokio::fs::File::from_std(file_std);
                let writer = Arc::new(Mutex::new(tokio::fs::File::from_std(writer_std)));

                let remote_addr = addr.clone();
                let recv_stats = stats.clone();
                let recv_packet_tx = packet_tx.clone();
                let recv_pool = pool.clone();
                let recv_pubkey = local_pubkey;

                let recv_writer = writer.clone();
                let recv_task = tokio::spawn(async move {
                    rfcomm_receive_loop(
                        reader,
                        recv_writer,
                        transport_id,
                        remote_addr.clone(),
                        recv_packet_tx,
                        recv_pool,
                        recv_stats,
                        recv_pubkey,
                    )
                    .await;
                });

                let conn = RfcommConnection {
                    writer,
                    recv_task,
                };

                let mut pool_guard = pool.lock().await;
                pool_guard.insert(addr, conn);

                stats.record_connection_accepted();

                debug!(
                    transport_id = %transport_id,
                    device = %device,
                    "RFCOMM server: accepted connection"
                );
            }
        }

        known_devices = current_devices;
    }
}

// ============================================================================
// Receive Loop (per-connection)
// ============================================================================

/// Per-connection RFCOMM receive loop.
///
/// Reads framed packets from the serial port, optionally performs pubkey
/// exchange, and delivers packets to the node via the packet channel.
/// On error or EOF, removes the connection from the pool and exits.
async fn rfcomm_receive_loop(
    reader: tokio::fs::File,
    writer: Arc<Mutex<tokio::fs::File>>,
    transport_id: TransportId,
    remote_addr: TransportAddr,
    packet_tx: PacketTx,
    pool: ConnectionPool,
    stats: Arc<RfcommStats>,
    local_pubkey: Option<[u8; 32]>,
) {
    debug!(
        transport_id = %transport_id,
        remote_addr = %remote_addr,
        "RFCOMM receive loop starting"
    );

    let mut buf_reader = BufReader::new(reader);

    // Pubkey exchange: if we have a local pubkey, send it and wait for peer's
    if let Some(pubkey) = local_pubkey {
        match pubkey_exchange(&mut buf_reader, &writer, &pubkey).await {
            Ok(_peer_pubkey) => {
                debug!(
                    transport_id = %transport_id,
                    remote_addr = %remote_addr,
                    "RFCOMM pubkey exchange complete"
                );
            }
            Err(e) => {
                warn!(
                    transport_id = %transport_id,
                    remote_addr = %remote_addr,
                    error = %e,
                    "RFCOMM pubkey exchange failed"
                );
                // Continue anyway — pubkey exchange is best-effort for RFCOMM
            }
        }
    }

    loop {
        match read_framed_packet(&mut buf_reader).await {
            Ok(data) => {
                stats.record_recv(data.len());

                trace!(
                    transport_id = %transport_id,
                    remote_addr = %remote_addr,
                    bytes = data.len(),
                    "RFCOMM packet received"
                );

                let packet = ReceivedPacket::new(transport_id, remote_addr.clone(), data);

                if packet_tx.send(packet).await.is_err() {
                    debug!(
                        transport_id = %transport_id,
                        "Packet channel closed, stopping RFCOMM receive loop"
                    );
                    break;
                }
            }
            Err(e) => {
                stats.record_recv_error();
                stats.record_framing_error();
                debug!(
                    transport_id = %transport_id,
                    remote_addr = %remote_addr,
                    error = %e,
                    "RFCOMM receive error, removing connection"
                );
                break;
            }
        }
    }

    // Clean up: remove ourselves from the pool
    let mut pool_guard = pool.lock().await;
    pool_guard.remove(&remote_addr);
    stats.record_connection_closed();

    debug!(
        transport_id = %transport_id,
        remote_addr = %remote_addr,
        "RFCOMM receive loop stopped"
    );
}

// ============================================================================
// Pubkey Exchange
// ============================================================================

/// Result of a successful pubkey exchange.
struct PubkeyExchangeResult {
    #[allow(dead_code)]
    peer_pubkey: XOnlyPublicKey,
}

/// Perform pubkey exchange over the RFCOMM serial connection.
///
/// Sends our 33-byte pubkey announcement (prefix 0x00 + 32-byte pubkey)
/// and waits for the peer's response. Uses length-prefix framing for
/// the exchange messages.
async fn pubkey_exchange<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    writer: &Arc<Mutex<tokio::fs::File>>,
    local_pubkey: &[u8; 32],
) -> Result<PubkeyExchangeResult, TransportError> {
    let mut our_announce = vec![PUBKEY_EXCHANGE_PREFIX];
    our_announce.extend_from_slice(local_pubkey);

    {
        let mut w = writer.lock().await;
        write_framed_packet(&mut *w, &our_announce)
            .await
            .map_err(|e| TransportError::SendFailed(format!("pubkey exchange send: {}", e)))?;
    }

    let timeout = std::time::Duration::from_secs(PUBKEY_EXCHANGE_TIMEOUT_SECS);

    let data = tokio::time::timeout(timeout, read_framed_packet(reader))
        .await
        .map_err(|_| TransportError::Timeout)?
        .map_err(|e| TransportError::RecvFailed(format!("pubkey exchange read: {}", e)))?;

    if data.len() < PUBKEY_EXCHANGE_SIZE {
        return Err(TransportError::RecvFailed(format!(
            "pubkey exchange: expected at least {} bytes, got {}",
            PUBKEY_EXCHANGE_SIZE,
            data.len()
        )));
    }

    if data[0] != PUBKEY_EXCHANGE_PREFIX {
        return Err(TransportError::RecvFailed(format!(
            "pubkey exchange: bad prefix 0x{:02X}",
            data[0]
        )));
    }

    let peer_pubkey = XOnlyPublicKey::from_slice(&data[1..33]).map_err(|e| {
        TransportError::RecvFailed(format!("pubkey exchange: invalid key: {}", e))
    })?;

    Ok(PubkeyExchangeResult { peer_pubkey })
}
