//! RFCOMM (Bluetooth Classic) Transport Implementation
//!
//! Provides RFCOMM transport for FIPS peer communication over
//! Bluetooth Classic using direct AF_BLUETOOTH sockets.
//!
//! Bypasses the Linux TTY layer entirely — no `/dev/rfcommN` device files
//! or `rfcomm` CLI tools required. Creates RFCOMM sockets directly via
//! `AF_BLUETOOTH, SOCK_STREAM, BTPROTO_RFCOMM` and wraps them in tokio
//! for async I/O.
//!
//! ## Modes
//!
//! - **Server mode**: Binds a listening RFCOMM socket on the configured
//!   channel and accepts incoming connections.
//! - **Client mode**: Connects directly to configured peer MAC addresses.

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
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, trace, warn};

// Linux Bluetooth/RFCOMM constants
const AF_BLUETOOTH: libc::c_int = 31;
const BTPROTO_RFCOMM: libc::c_int = 3;

/// Pubkey exchange constants (matches BLE pattern).
const PUBKEY_EXCHANGE_PREFIX: u8 = 0x00;
const PUBKEY_EXCHANGE_SIZE: usize = 33;
const PUBKEY_EXCHANGE_TIMEOUT_SECS: u64 = 15;

// ============================================================================
// sockaddr_rc Helpers
// ============================================================================

/// Build a `sockaddr_rc` byte buffer for connect/bind syscalls.
///
/// `mac_le` is the Bluetooth address in little-endian byte order
/// (reversed from the string representation).
fn make_sockaddr_rc(mac_le: &[u8; 6], channel: u8) -> [u8; 10] {
    let mut addr = [0u8; 10];
    // sa_family_t (2 bytes, host byte order — LE on all targets we care about)
    addr[0] = AF_BLUETOOTH as u8;
    addr[1] = 0;
    // bdaddr_t: 6 bytes, little-endian
    addr[2..8].copy_from_slice(mac_le);
    // rc_channel: 1 byte
    addr[8] = channel;
    addr
}

/// Parse a MAC address string ("AA:BB:CC:DD:EE:FF") to little-endian bytes
/// for `sockaddr_rc`. Returns `[0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA]`.
fn parse_mac_addr(s: &str) -> Result<[u8; 6], TransportError> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return Err(TransportError::InvalidAddress(format!(
            "invalid MAC address: expected 6 octets, got {}: {}",
            parts.len(),
            s
        )));
    }
    let mut mac = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(part, 16).map_err(|e| {
            TransportError::InvalidAddress(format!("invalid MAC octet '{}': {}", part, e))
        })?;
    }
    // Reverse to little-endian for sockaddr_rc
    mac.reverse();
    Ok(mac)
}

/// Format a little-endian MAC address back to canonical string form.
fn format_mac_addr(mac_le: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac_le[5], mac_le[4], mac_le[3], mac_le[2], mac_le[1], mac_le[0]
    )
}

/// Extract the MAC address from a `sockaddr_rc` byte buffer (bytes 2..8).
fn mac_from_sockaddr(addr: &[u8]) -> [u8; 6] {
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&addr[2..8]);
    mac
}

// ============================================================================
// Socket Syscalls
// ============================================================================

/// Set a file descriptor to non-blocking mode.
fn set_nonblocking(fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Create an RFCOMM socket and connect to a peer.
///
/// Blocking — call via `spawn_blocking` to avoid stalling the tokio runtime.
fn rfcomm_socket_connect(mac_le: &[u8; 6], channel: u8) -> Result<RawFd, TransportError> {
    let fd = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_STREAM, BTPROTO_RFCOMM) };
    if fd < 0 {
        return Err(TransportError::StartFailed(format!(
            "RFCOMM socket() failed: {}",
            std::io::Error::last_os_error()
        )));
    }

    let addr = make_sockaddr_rc(mac_le, channel);
    let ret = unsafe {
        libc::connect(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            10,
        )
    };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(TransportError::StartFailed(format!(
            "RFCOMM connect() failed: {}",
            err
        )));
    }

    set_nonblocking(fd).map_err(|e| {
        unsafe { libc::close(fd) };
        TransportError::StartFailed(format!("fcntl(O_NONBLOCK) failed: {}", e))
    })?;

    Ok(fd)
}

/// Create an RFCOMM listening socket bound to the given channel.
fn rfcomm_socket_listen(channel: u8) -> Result<RawFd, TransportError> {
    let fd = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_STREAM, BTPROTO_RFCOMM) };
    if fd < 0 {
        return Err(TransportError::StartFailed(format!(
            "RFCOMM socket() failed: {}",
            std::io::Error::last_os_error()
        )));
    }

    let opt: libc::c_int = 1;
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &opt as *const _ as *const _,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }

    // Bind to BDADDR_ANY on the given channel
    let addr = make_sockaddr_rc(&[0u8; 6], channel);
    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            10,
        )
    };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(TransportError::StartFailed(format!(
            "RFCOMM bind() failed: {}",
            err
        )));
    }

    let ret = unsafe { libc::listen(fd, 4) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(TransportError::StartFailed(format!(
            "RFCOMM listen() failed: {}",
            err
        )));
    }

    Ok(fd)
}

/// Accept an incoming connection on a non-blocking listen socket.
///
/// Returns `(client_fd, client_mac_le)`.
/// Returns `WouldBlock` if no pending connection is available.
fn rfcomm_socket_accept(listen_fd: RawFd) -> std::io::Result<(RawFd, [u8; 6])> {
    let mut addr = [0u8; 16];
    let mut addr_len: libc::socklen_t = 16;
    let client_fd = unsafe {
        libc::accept(
            listen_fd,
            addr.as_mut_ptr() as *mut libc::sockaddr,
            &mut addr_len,
        )
    };
    if client_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mac = mac_from_sockaddr(&addr);

    if let Err(e) = set_nonblocking(client_fd) {
        unsafe { libc::close(client_fd) };
        return Err(e);
    }

    Ok((client_fd, mac))
}

/// Wrap a raw connected socket fd as a tokio `UnixStream`.
fn fd_to_tokio_stream(fd: RawFd) -> std::io::Result<tokio::net::UnixStream> {
    // from_raw_fd takes ownership of the fd; UnixStream closes it on drop.
    let std_stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    tokio::net::UnixStream::from_std(std_stream)
}

// ============================================================================
// Connection Pool
// ============================================================================

/// State for a single RFCOMM connection.
struct RfcommConnection {
    /// Write half of the socket (shared with pubkey exchange in recv loop).
    writer: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    /// Receive task for this connection.
    recv_task: JoinHandle<()>,
}

/// Shared connection pool.
type ConnectionPool = Arc<Mutex<HashMap<TransportAddr, RfcommConnection>>>;

// ============================================================================
// RFCOMM Transport
// ============================================================================

/// RFCOMM transport for FIPS.
///
/// Provides connection-oriented, reliable byte stream delivery over
/// Bluetooth Classic RFCOMM sockets. Uses length-prefix framing
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
    /// Server accept loop task handle (if in server mode).
    accept_task: Option<JoinHandle<()>>,
    /// Listen socket fd (server mode) — closed on stop.
    listen_fd: Option<RawFd>,
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
            accept_task: None,
            listen_fd: None,
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

        if self.config.mode() == "client" {
            self.start_client().await;
        }

        if self.config.mode() == "server" {
            self.start_server().await?;
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

    /// Client mode: connect to all configured peers.
    async fn start_client(&self) {
        let channel = self.config.channel();
        if self.config.peers.is_empty() {
            debug!(
                transport_id = %self.transport_id,
                "RFCOMM client: no peers configured"
            );
            return;
        }

        for peer in &self.config.peers {
            match parse_mac_addr(&peer.mac) {
                Ok(mac_le) => {
                    let mac_str = format_mac_addr(&mac_le);
                    match self.connect_to_peer(&mac_le, channel).await {
                        Ok(()) => {
                            debug!(
                                transport_id = %self.transport_id,
                                mac = %mac_str,
                                channel,
                                "RFCOMM client: connected to peer"
                            );
                        }
                        Err(e) => {
                            warn!(
                                transport_id = %self.transport_id,
                                mac = %mac_str,
                                error = %e,
                                "RFCOMM client: failed to connect to peer"
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        transport_id = %self.transport_id,
                        mac = %peer.mac,
                        error = %e,
                        "RFCOMM client: invalid MAC address in config"
                    );
                }
            }
        }
    }

    /// Connect to a single peer and register the connection in the pool.
    async fn connect_to_peer(&self, mac_le: &[u8; 6], channel: u8) -> Result<(), TransportError> {
        let mac_str = format_mac_addr(mac_le);
        let addr = TransportAddr::from_string(&mac_str);

        {
            let pool = self.pool.lock().await;
            if pool.contains_key(&addr) {
                return Ok(());
            }
        }

        let mac_copy = *mac_le;
        let fd = tokio::task::spawn_blocking(move || rfcomm_socket_connect(&mac_copy, channel))
            .await
            .map_err(|e| TransportError::StartFailed(format!("connect task join: {}", e)))??;

        self.register_socket(fd, addr).await
    }

    /// Wrap a connected socket fd and register it in the pool.
    async fn register_socket(
        &self,
        fd: RawFd,
        addr: TransportAddr,
    ) -> Result<(), TransportError> {
        // Double-check pool under lock to prevent duplicate registration
        {
            let pool = self.pool.lock().await;
            if pool.contains_key(&addr) {
                unsafe { libc::close(fd) };
                return Ok(());
            }
        }

        let stream = fd_to_tokio_stream(fd).map_err(|e| {
            // fd is now owned by fd_to_tokio_stream; on error it closes the fd.
            TransportError::StartFailed(format!("tokio UnixStream::from_std: {}", e))
        })?;

        let (read_half, write_half) = stream.into_split();
        let writer = Arc::new(Mutex::new(write_half));

        let transport_id = self.transport_id;
        let packet_tx = self.packet_tx.clone();
        let pool = self.pool.clone();
        let stats = self.stats.clone();
        let remote_addr = addr.clone();
        let local_pubkey = self.local_pubkey;

        let recv_writer = writer.clone();
        let recv_task = tokio::spawn(async move {
            rfcomm_receive_loop(
                read_half,
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
        if let Some(old) = pool.insert(addr, conn) {
            old.recv_task.abort();
        }

        self.stats.record_connection_established();

        Ok(())
    }

    /// Server mode: bind, listen, and spawn accept loop.
    async fn start_server(&mut self) -> Result<(), TransportError> {
        let channel = self.config.channel();
        let listen_fd = rfcomm_socket_listen(channel)?;
        self.listen_fd = Some(listen_fd);

        let transport_id = self.transport_id;
        let packet_tx = self.packet_tx.clone();
        let pool = self.pool.clone();
        let stats = self.stats.clone();
        let local_pubkey = self.local_pubkey;

        let accept_task = tokio::spawn(async move {
            server_accept_loop(
                listen_fd,
                transport_id,
                packet_tx,
                pool,
                stats,
                local_pubkey,
            )
            .await;
        });
        self.accept_task = Some(accept_task);

        info!(
            transport_id = %self.transport_id,
            channel,
            "RFCOMM server: listening on channel"
        );

        Ok(())
    }

    /// Stop the transport asynchronously.
    pub async fn stop_async(&mut self) -> Result<(), TransportError> {
        if !self.state.is_operational() {
            return Err(TransportError::NotStarted);
        }

        // Abort accept loop first so it stops using the listen fd
        if let Some(task) = self.accept_task.take() {
            task.abort();
            let _ = task.await;
        }

        // Close listen socket
        if let Some(fd) = self.listen_fd.take() {
            unsafe { libc::close(fd) };
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

        let mtu = self.config.mtu() as usize;
        if data.len() > mtu {
            return Err(TransportError::MtuExceeded {
                packet_size: data.len(),
                mtu: self.config.mtu(),
            });
        }

        let writer = {
            let pool = self.pool.lock().await;
            pool.get(addr)
                .map(|c| c.writer.clone())
                .ok_or_else(|| TransportError::SendFailed("no connection to address".into()))?
        };

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
    /// The address must be a MAC address string ("AA:BB:CC:DD:EE:FF").
    /// Returns Ok immediately if already connected.
    pub async fn connect_async(&self, addr: &TransportAddr) -> Result<(), TransportError> {
        if !self.state.is_operational() {
            return Err(TransportError::NotStarted);
        }

        {
            let pool = self.pool.lock().await;
            if pool.contains_key(addr) {
                return Ok(());
            }
        }

        let mac_str = addr
            .as_str()
            .ok_or_else(|| TransportError::InvalidAddress("not valid UTF-8".into()))?;
        let mac_le = parse_mac_addr(mac_str)?;
        let channel = self.config.channel();
        self.connect_to_peer(&mac_le, channel).await
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
// Server Accept Loop
// ============================================================================

/// Blocking accept via `spawn_blocking` — `AsyncFd`/`epoll` does not work with
/// Bluetooth RFCOMM sockets. Accepted fds are sent to the async runtime.
async fn server_accept_loop(
    listen_fd: RawFd,
    transport_id: TransportId,
    packet_tx: PacketTx,
    pool: ConnectionPool,
    stats: Arc<RfcommStats>,
    local_pubkey: Option<[u8; 32]>,
) {
    info!(transport_id = %transport_id, "RFCOMM server accept loop starting");

    let (accept_tx, mut accept_rx) = tokio::sync::mpsc::unbounded_channel();

    let blocking_transport_id = transport_id;
    let blocking_handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
        loop {
            match rfcomm_socket_accept(listen_fd) {
                Ok((client_fd, client_mac)) => {
                    if accept_tx.send((client_fd, client_mac)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        transport_id = %blocking_transport_id,
                        error = %e,
                        "RFCOMM server: blocking accept() error"
                    );
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
    });

    while let Some((client_fd, client_mac)) = accept_rx.recv().await {
        let mac_str = format_mac_addr(&client_mac);
        let addr = TransportAddr::from_string(&mac_str);

        // Check if already connected
        {
            let pool_guard = pool.lock().await;
            if pool_guard.contains_key(&addr) {
                debug!(
                    transport_id = %transport_id,
                    mac = %mac_str,
                    "RFCOMM server: duplicate connection, closing"
                );
                unsafe { libc::close(client_fd) };
                continue;
            }
        }

        if let Err(e) = set_nonblocking(client_fd) {
            warn!(
                transport_id = %transport_id,
                mac = %mac_str,
                error = %e,
                "RFCOMM server: failed to set client fd non-blocking"
            );
            unsafe { libc::close(client_fd) };
            continue;
        }

        let stream = match fd_to_tokio_stream(client_fd) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    transport_id = %transport_id,
                    mac = %mac_str,
                    error = %e,
                    "RFCOMM server: failed to wrap client fd"
                );
                continue;
            }
        };

        let (read_half, write_half) = stream.into_split();
        let writer = Arc::new(Mutex::new(write_half));

        let recv_writer = writer.clone();
        let recv_packet_tx = packet_tx.clone();
        let recv_pool = pool.clone();
        let recv_stats = stats.clone();
        let recv_pubkey = local_pubkey;
        let remote_addr = addr.clone();

        let recv_task = tokio::spawn(async move {
            rfcomm_receive_loop(
                read_half,
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
        if let Some(old) = pool_guard.insert(addr, conn) {
            old.recv_task.abort();
        }

        stats.record_connection_accepted();

        info!(
            transport_id = %transport_id,
            mac = %mac_str,
            "RFCOMM server: accepted connection"
        );
    }

    blocking_handle.abort();
    info!(transport_id = %transport_id, "RFCOMM server accept loop stopped");
}

// ============================================================================
// Receive Loop (per-connection)
// ============================================================================

/// Per-connection RFCOMM receive loop.
///
/// Reads framed packets from the socket, optionally performs pubkey exchange,
/// and delivers packets to the node via the packet channel.
/// On error or EOF, removes the connection from the pool and exits.
async fn rfcomm_receive_loop(
    reader: tokio::net::unix::OwnedReadHalf,
    writer: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
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
        let mut w = writer.lock().await;
        match pubkey_exchange(&mut buf_reader, &mut *w, &pubkey).await {
            Ok(_peer_pubkey) => {
                info!(
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
                // Continue anyway — pubkey exchange is best-effort
            }
        }
        drop(w);
    }

    debug!(
        transport_id = %transport_id,
        remote_addr = %remote_addr,
        "RFCOMM receive loop entering main read loop"
    );

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

/// Perform pubkey exchange over the RFCOMM connection.
///
/// Sends our 33-byte pubkey announcement (prefix 0x00 + 32-byte pubkey)
/// and waits for the peer's response. Uses length-prefix framing for
/// the exchange messages.
async fn pubkey_exchange<R, W>(
    reader: &mut R,
    writer: &mut W,
    local_pubkey: &[u8; 32],
) -> Result<PubkeyExchangeResult, TransportError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut our_announce = vec![PUBKEY_EXCHANGE_PREFIX];
    our_announce.extend_from_slice(local_pubkey);

    write_framed_packet(writer, &our_announce)
        .await
        .map_err(|e| TransportError::SendFailed(format!("pubkey exchange send: {}", e)))?;

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
