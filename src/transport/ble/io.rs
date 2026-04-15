//! BLE I/O abstraction layer.
//!
//! Defines the `BleIo` trait that separates transport logic from the
//! BlueZ/bluer stack. `BluerIo` (behind `cfg(feature = "ble")`) provides
//! the real implementation; `MockBleIo` provides an in-memory test double.

use std::collections::HashMap;

use crate::transport::TransportError;

use super::addr::BleAddr;

// ============================================================================
// BLE I/O Traits
// ============================================================================

/// A connected L2CAP stream for sending and receiving data.
pub trait BleStream: Send + Sync {
    /// Send data over the L2CAP connection.
    fn send(
        &self,
        data: &[u8],
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send;

    /// Send data with priority (bypasses rate limiting).
    ///
    /// For control plane packets: handshakes, rekey, heartbeats, MMP reports.
    /// Default implementation falls through to [`send`](Self::send), which is
    /// correct for streams without rate limiting (e.g., mock streams).
    fn send_urgent(
        &self,
        data: &[u8],
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send;

    /// Receive data from the L2CAP connection.
    ///
    /// Returns the number of bytes read into `buf`.
    fn recv(
        &self,
        buf: &mut [u8],
    ) -> impl std::future::Future<Output = Result<usize, TransportError>> + Send;

    /// Get the L2CAP send MTU for this connection.
    fn send_mtu(&self) -> u16;

    /// Get the L2CAP receive MTU for this connection.
    fn recv_mtu(&self) -> u16;

    /// Get the remote device address.
    fn remote_addr(&self) -> &BleAddr;

    /// Update the send rate limiter's throughput ceiling.
    /// No-op if rate limiting is disabled (rate_bps = 0).
    fn set_rate_bps(
        &self,
        rate_bps: u64,
    ) -> impl std::future::Future<Output = ()> + Send;
}

/// An acceptor that yields inbound L2CAP connections.
pub trait BleAcceptor: Send {
    /// The concrete stream type yielded by this acceptor.
    type Stream: BleStream + 'static;

    /// Accept the next inbound connection.
    fn accept(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Self::Stream, TransportError>> + Send;
}

/// A scanner that yields discovered BLE devices advertising the FIPS UUID.
pub trait BleScanner: Send {
    /// Wait for the next discovered device.
    ///
    /// Returns `None` when scanning is stopped.
    fn next(
        &mut self,
    ) -> impl std::future::Future<Output = Option<BleAddr>> + Send;
}

/// Core BLE I/O operations.
///
/// This trait abstracts the BlueZ/bluer stack so that `BleTransport`
/// can be tested with `MockBleIo` (in-memory channels) in CI without
/// requiring Bluetooth hardware, D-Bus, or bluetoothd.
pub trait BleIo: Send + Sync + 'static {
    /// The concrete stream type returned by this I/O implementation.
    type Stream: BleStream + 'static;
    /// The concrete acceptor type.
    type Acceptor: BleAcceptor<Stream = Self::Stream> + 'static;
    /// The concrete scanner type.
    type Scanner: BleScanner + 'static;

    /// Start listening for inbound L2CAP connections on the given PSM.
    fn listen(
        &self,
        psm: u16,
    ) -> impl std::future::Future<Output = Result<Self::Acceptor, TransportError>> + Send;

    /// Connect to a remote BLE device on the given PSM.
    fn connect(
        &self,
        addr: &BleAddr,
        psm: u16,
    ) -> impl std::future::Future<Output = Result<Self::Stream, TransportError>> + Send;

    /// Start advertising the FIPS service UUID.
    fn start_advertising(
        &self,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send;

    /// Stop advertising.
    fn stop_advertising(
        &self,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send;

    /// Disconnect the underlying BLE device at the given address.
    ///
    /// On macOS, this calls `cancelPeripheralConnection:` so CoreBluetooth
    /// transitions the peripheral to Disconnected and resumes reporting it
    /// in scan results. On Linux this is a no-op (bluez manages connections
    /// via socket lifecycle).
    fn disconnect_device(
        &self,
        addr: &BleAddr,
    ) -> impl std::future::Future<Output = ()> + Send;

    /// Start passive scanning for FIPS service UUID advertisements.
    fn start_scanning(
        &self,
    ) -> impl std::future::Future<Output = Result<Self::Scanner, TransportError>> + Send;

    /// Get the adapter's BLE address.
    fn local_addr(&self) -> Result<BleAddr, TransportError>;

    /// Get the adapter name (e.g., "hci0").
    fn adapter_name(&self) -> &str;

    /// Discover the L2CAP PSM of a remote peripheral via GATT PSM exchange.
    ///
    /// Returns the dynamically-assigned PSM if the peer supports GATT PSM
    /// exchange, or an error if the service is not found or unsupported.
    /// The default implementation always returns an error (GATT not supported).
    fn discover_gatt_psm(
        &self,
        _addr: &BleAddr,
    ) -> impl std::future::Future<Output = Result<u16, TransportError>> + Send {
        async {
            Err(TransportError::Io(std::io::Error::other(
                "GATT PSM discovery not supported",
            )))
        }
    }
}

// ============================================================================
// BluerIo — Production BLE I/O via BlueZ D-Bus
// ============================================================================

#[cfg(all(feature = "ble", target_os = "linux"))]
mod bluer_impl {
    use super::*;
    use crate::transport::TransportError;

    use bluer::l2cap::{FlowControl, SeqPacket, SeqPacketListener, Socket, SocketAddr};
    use bluer::{adv::Advertisement, AdapterEvent, AddressType, DiscoveryFilter, DiscoveryTransport};
    use futures::StreamExt;
    use std::collections::{BTreeSet, HashMap, HashSet};
    use std::pin::Pin;
    use tokio::sync::Mutex;
    use tokio::time::{Duration, timeout};
    use tracing::{debug, trace, warn};

    /// FIPS BLE service UUID.
    ///
    /// Derived from SHA-256("FIPS: welcome to cryptoanarchy") with UUID v4
    /// version/variant bits applied.
    pub const FIPS_SERVICE_UUID: bluer::Uuid =
        bluer::Uuid::from_u128(0x9c90_b790_2cc5_42c0_9f87_c9cc_4064_8f4c);

    /// GATT PSM Exchange Service UUID.
    ///
    /// Derived from SHA-256("FIPS GATT PSM Exchange Service") with UUID v4
    /// version/variant bits applied. Central reads this to decide whether
    /// to discover the peer's L2CAP PSM via GATT.
    pub const FIPS_GATT_PSM_SERVICE_UUID: bluer::Uuid =
        bluer::Uuid::from_u128(0x0e2c_43b1_51b9_4667_a1d1_a95e_a79f_d19b);

    /// GATT PSM Exchange Characteristic UUID.
    ///
    /// Derived from SHA-256("FIPS GATT PSM Exchange Characteristic") with UUID v4
    /// version/variant bits applied. Contains the 2-byte LE PSM value.
    pub const FIPS_GATT_PSM_CHAR_UUID: bluer::Uuid =
        bluer::Uuid::from_u128(0x250c_88dd_3dff_4c41_83b2_f1b4_e3d8_20cc);

    /// Map a bluer error to a TransportError.
    fn map_err(context: &str, e: bluer::Error) -> TransportError {
        TransportError::Io(std::io::Error::other(format!("{}: {}", context, e)))
    }

    /// Map a std::io::Error to a TransportError.
    fn map_io_err(context: &str, e: std::io::Error) -> TransportError {
        TransportError::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {}", context, e),
        ))
    }

    // ----------------------------------------------------------------
    // BluerStream
    // ----------------------------------------------------------------

    /// BLE stream wrapping a bluer L2CAP SeqPacket connection with length-prefix framing.
    ///
    /// The ESP32 firmware adds a 2-byte big-endian length prefix to every L2CAP
    /// frame. The macOS `BluestStream` does the same for CoreBluetooth
    /// compatibility. This stream matches that framing so the daemon and ESP32
    /// can communicate.
    pub struct BluerStream {
        conn: SeqPacket,
        remote: BleAddr,
        send_mtu: u16,
        recv_mtu: u16,
        rate_limiter: Option<tokio::sync::Mutex<crate::transport::ble::rate_limit::SendRateLimiter>>,
        /// Internal buffer for stripping the 2-byte BE length prefix on recv.
        recv_buf: tokio::sync::Mutex<Vec<u8>>,
    }

    impl BluerStream {
        /// Construct from a connected SeqPacket, querying MTU values.
        pub fn new(conn: SeqPacket, remote: BleAddr, send_rate_bps: u64, send_burst_bytes: u32) -> Result<Self, TransportError> {
            let send_mtu = conn
                .send_mtu()
                .map_err(|e| map_io_err("send_mtu", e))? as u16;
            let recv_mtu = conn
                .recv_mtu()
                .map_err(|e| map_io_err("recv_mtu", e))? as u16;

            // Log negotiated PHY for diagnostics (2M vs 1M)
            match conn.as_ref().phy() {
                Ok(phy) => debug!(addr = %remote, phy, send_mtu, recv_mtu, "BLE connection established"),
                Err(_) => debug!(addr = %remote, send_mtu, recv_mtu, "BLE connection established (PHY query unsupported)"),
            }

            Ok(Self {
                conn,
                remote,
                send_mtu,
                recv_mtu,
                rate_limiter: if send_rate_bps > 0 {
                    Some(tokio::sync::Mutex::new(
                        crate::transport::ble::rate_limit::SendRateLimiter::new(send_rate_bps, send_burst_bytes)
                    ))
                } else {
                    None
                },
                recv_buf: tokio::sync::Mutex::new(vec![0u8; recv_mtu as usize]),
            })
        }
    }

    impl BleStream for BluerStream {
        async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
            let framed_len = 2 + data.len();
            if let Some(ref limiter) = self.rate_limiter {
                limiter.lock().await.acquire(framed_len).await;
            }

            if framed_len > self.send_mtu as usize {
                return Err(TransportError::MtuExceeded {
                    packet_size: framed_len,
                    mtu: self.send_mtu,
                });
            }

            let mut framed = Vec::with_capacity(framed_len);
            framed.extend_from_slice(&(data.len() as u16).to_be_bytes());
            framed.extend_from_slice(data);
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                self.conn.send(&framed),
            )
            .await
            .map_err(|_| {
                warn!(len = framed.len(), "BLE write timeout (3s)");
                TransportError::Timeout
            })?
            .map(|_| ())
            .map_err(|e| TransportError::SendFailed(format!("{}", e)))
        }

        async fn send_urgent(&self, data: &[u8]) -> Result<(), TransportError> {
            let framed_len = 2 + data.len();
            if framed_len > self.send_mtu as usize {
                return Err(TransportError::MtuExceeded {
                    packet_size: framed_len,
                    mtu: self.send_mtu,
                });
            }

            let mut framed = Vec::with_capacity(framed_len);
            framed.extend_from_slice(&(data.len() as u16).to_be_bytes());
            framed.extend_from_slice(data);
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                self.conn.send(&framed),
            )
            .await
            .map_err(|_| {
                warn!(len = framed.len(), "BLE write timeout (3s)");
                TransportError::Timeout
            })?
            .map(|_| ())
            .map_err(|e| TransportError::SendFailed(format!("{}", e)))
        }

        async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
            let mut internal = self.recv_buf.lock().await;
            let n = self
                .conn
                .recv(&mut internal[..])
                .await
                .map_err(|e| TransportError::RecvFailed(format!("{}", e)))?;

            if n < 2 {
                return Err(TransportError::RecvFailed(format!(
                    "BLE recv: framed message too short ({} bytes)",
                    n
                )));
            }

            let payload_len = u16::from_be_bytes([internal[0], internal[1]]) as usize;
            if n < 2 + payload_len {
                return Err(TransportError::RecvFailed(format!(
                    "BLE recv: frame header says {} bytes but only {} available",
                    payload_len,
                    n - 2
                )));
            }

            let copy_len = payload_len.min(buf.len());
            buf[..copy_len].copy_from_slice(&internal[2..2 + copy_len]);
            Ok(copy_len)
        }

        fn send_mtu(&self) -> u16 {
            self.send_mtu
        }

        fn recv_mtu(&self) -> u16 {
            self.recv_mtu
        }

        fn remote_addr(&self) -> &BleAddr {
            &self.remote
        }

        async fn set_rate_bps(&self, rate_bps: u64) {
            if let Some(ref limiter) = self.rate_limiter {
                limiter.lock().await.set_rate_bps(rate_bps);
            }
        }
    }

    // ----------------------------------------------------------------
    // BluerAcceptor
    // ----------------------------------------------------------------

    /// Acceptor wrapping a bluer L2CAP SeqPacketListener.
    pub struct BluerAcceptor {
        listener: SeqPacketListener,
        adapter_name: String,
        send_rate_bps: u64,
        send_burst_bytes: u32,
    }

    impl BleAcceptor for BluerAcceptor {
        type Stream = BluerStream;

        async fn accept(&mut self) -> Result<BluerStream, TransportError> {
            let (conn, peer_sa) = self
                .listener
                .accept()
                .await
                .map_err(|e| map_io_err("accept", e))?;

            let remote = BleAddr::from_bluer(peer_sa.addr, &self.adapter_name);
            BluerStream::new(conn, remote, self.send_rate_bps, self.send_burst_bytes)
        }
    }

    // ----------------------------------------------------------------
    // BluerScanner
    // ----------------------------------------------------------------

    /// Scanner wrapping a bluer discovery event stream.
    pub struct BluerScanner {
        events: Pin<Box<dyn futures::Stream<Item = AdapterEvent> + Send>>,
        adapter: bluer::Adapter,
        adapter_name: String,
    }

    impl BleScanner for BluerScanner {
        async fn next(&mut self) -> Option<BleAddr> {
            loop {
                match self.events.next().await {
                    Some(AdapterEvent::DeviceAdded(addr)) => {
                        // Check if device advertises FIPS UUID
                        if let Ok(device) = self.adapter.device(addr) {
                            match device.uuids().await {
                                Ok(Some(uuids)) if uuids.contains(&FIPS_SERVICE_UUID) => {
                                    let ble_addr =
                                        BleAddr::from_bluer(addr, &self.adapter_name);
                                    debug!(addr = %ble_addr, "BLE scanner: FIPS peer found");
                                    return Some(ble_addr);
                                }
                                Ok(_) => {
                                    trace!(addr = %addr, "BLE scanner: device without FIPS UUID");
                                }
                                Err(e) => {
                                    trace!(addr = %addr, error = %e, "BLE scanner: failed to read UUIDs");
                                }
                            }
                        }
                    }
                    Some(_) => continue,
                    None => return None,
                }
            }
        }
    }

    // ----------------------------------------------------------------
    // BluerIo
    // ----------------------------------------------------------------

    /// Production BLE I/O implementation via BlueZ D-Bus (bluer crate).
    pub struct BluerIo {
        #[allow(dead_code)] // Session must be kept alive for the adapter.
        session: bluer::Session,
        adapter: bluer::Adapter,
        adapter_name: String,
        adv_handle: Mutex<Option<bluer::adv::AdvertisementHandle>>,
        #[allow(dead_code)] // Handle must be kept alive for agent to stay registered.
        agent_handle: bluer::agent::AgentHandle,
        mtu: u16,
        send_rate_bps: u64,
        send_burst_bytes: u32,
    }

    impl BluerIo {
        /// Create a new BluerIo for the given adapter.
        ///
        /// Connects to BlueZ via D-Bus and powers on the adapter.
        /// When `le_only` is true, disables BR/EDR via btmgmt to prevent
        /// CTKD LinkKey bits in SMP pairing (required for CoreBluetooth).
        pub async fn new(adapter_name: &str, mtu: u16, send_rate_bps: u64, send_burst_bytes: u32, le_only: bool) -> Result<Self, TransportError> {
            if le_only {
                let mgmt_adapter = if adapter_name == "default" {
                    "hci0".to_string()
                } else {
                    adapter_name.to_string()
                };
                let output = tokio::process::Command::new("btmgmt")
                    .args(["bredr", "off"])
                    .output()
                    .await;
                match output {
                    Ok(out) if out.status.success() => {
                        debug!(adapter = %mgmt_adapter, "BLE: BR/EDR disabled (le_only mode)");
                    }
                    Ok(out) => {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        warn!(adapter = %mgmt_adapter, stderr = %stderr, "BLE: btmgmt bredr off failed (non-fatal, SMP pairing may fail)");
                    }
                    Err(e) => {
                        warn!(adapter = %mgmt_adapter, error = %e, "BLE: btmgmt not found (non-fatal, SMP pairing may fail)");
                    }
                }
            }

            let session = bluer::Session::new()
                .await
                .map_err(|e| map_err("Session::new", e))?;

            let adapter = if adapter_name == "default" {
                session
                    .default_adapter()
                    .await
                    .map_err(|e| map_err("default_adapter", e))?
            } else {
                session
                    .adapter(adapter_name)
                    .map_err(|e| map_err("adapter", e))?
            };

            adapter
                .set_powered(true)
                .await
                .map_err(|e| map_err("set_powered", e))?;

            let name = adapter.name().to_string();

            let agent = bluer::agent::Agent {
                request_default: true,
                ..Default::default()
            };
            let agent_handle = session
                .register_agent(agent)
                .await
                .map_err(|e| map_err("register_agent", e))?;

            debug!(adapter = %name, "BluerIo initialized");

            Ok(Self {
                session,
                adapter,
                adapter_name: name,
                adv_handle: Mutex::new(None),
                agent_handle,
                mtu,
                send_rate_bps,
                send_burst_bytes,
            })
        }

        async fn resolve_addr_type(&self, addr: &BleAddr) -> AddressType {
            let bluer_addr = addr.to_bluer_address();

            match self.adapter.device(bluer_addr) {
                Ok(device) => device.address_type().await.unwrap_or(AddressType::LeRandom),
                Err(_) => {
                    debug!(addr = %addr, "BLE connect: device not in cache, starting discovery scan");

                    let filter = DiscoveryFilter {
                        transport: DiscoveryTransport::Le,
                        ..Default::default()
                    };

                    if let Err(e) = self.adapter.set_discovery_filter(filter).await {
                        debug!(addr = %addr, error = %e, "BLE connect: failed to set discovery filter");
                        return AddressType::LeRandom;
                    }

                    match self.adapter.discover_devices().await {
                        Ok(mut events) => {
                            let scan_result = timeout(Duration::from_secs(5), async {
                                while let Some(event) = events.next().await {
                                    if let AdapterEvent::DeviceAdded(found_addr) = event
                                        && found_addr == bluer_addr {
                                            let addr_type = match self.adapter.device(found_addr) {
                                                Ok(device) => {
                                                    device.address_type().await.unwrap_or(AddressType::LeRandom)
                                                }
                                                Err(_) => AddressType::LeRandom,
                                            };
                                            debug!(addr = %addr, addr_type = ?addr_type, "BLE connect: discovery scan found device addr_type");
                                            return Some(addr_type);
                                        }
                                }
                                None
                            })
                            .await;

                            drop(events);

                            match scan_result {
                                Ok(Some(addr_type)) => addr_type,
                                Ok(None) | Err(_) => match self.adapter.device(bluer_addr) {
                                    Ok(device) => {
                                        let addr_type = device.address_type().await.unwrap_or(AddressType::LeRandom);
                                        debug!(addr = %addr, addr_type = ?addr_type, "BLE connect: discovery scan cached device addr_type");
                                        addr_type
                                    }
                                    Err(_) => AddressType::LeRandom,
                                },
                            }
                        }
                        Err(e) => {
                            debug!(addr = %addr, error = %e, "BLE connect: failed to start discovery scan");
                            AddressType::LeRandom
                        }
                    }
                }
            }
        }

        /// Discover the L2CAP PSM of a remote peripheral via GATT.
        ///
        /// Connects at GATT level, finds the FIPS GATT PSM Exchange Service,
        /// reads the PSM characteristic, and returns the 2-byte LE-encoded PSM.
        /// Disconnects GATT after reading (L2CAP connection is independent).
        ///
        /// Returns an error if the service or characteristic is not found,
        /// or if the PSM value is invalid.
        pub async fn discover_gatt_psm(&self, addr: &BleAddr) -> Result<u16, TransportError> {
            let discover = async {
                let bluer_addr = addr.to_bluer_address();
                let device = self.adapter.device(bluer_addr).map_err(|e| {
                    TransportError::Io(std::io::Error::other(format!(
                        "discover_gatt_psm: device not found for {}: {}",
                        addr, e
                    )))
                })?;

                debug!(addr = %addr, "GATT PSM discovery: connecting GATT");

                device.connect().await.map_err(|e| {
                    TransportError::Io(std::io::Error::other(format!(
                        "discover_gatt_psm: GATT connect failed for {}: {}",
                        addr, e
                    )))
                })?;

                let result = self
                    .read_psm_from_gatt(&device, addr)
                    .await;

                if let Err(e) = device.disconnect().await {
                    debug!(
                        addr = %addr, error = %e,
                        "GATT PSM discovery: GATT disconnect failed (non-fatal)"
                    );
                }

                result
            };

            timeout(Duration::from_secs(10), discover)
                .await
                .map_err(|_| {
                    TransportError::Io(std::io::Error::other(format!(
                        "discover_gatt_psm: timed out discovering PSM for {}",
                        addr
                    )))
                })?
        }

        async fn read_psm_from_gatt(
            &self,
            device: &bluer::Device,
            addr: &BleAddr,
        ) -> Result<u16, TransportError> {
            let services = device.services().await.map_err(|e| {
                TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: failed to enumerate services for {}: {}",
                    addr, e
                )))
            })?;

            debug!(
                addr = %addr, count = services.len(),
                "GATT PSM discovery: enumerated services"
            );

            let psm_service = self.find_service_by_uuid(&services, FIPS_GATT_PSM_SERVICE_UUID).await;
            let psm_service = match psm_service {
                Some(s) => s,
                None => {
                    return Err(TransportError::Io(std::io::Error::other(format!(
                        "discover_gatt_psm: FIPS GATT PSM service not found on {}",
                        addr
                    ))));
                }
            };

            let characteristics = psm_service.characteristics().await.map_err(|e| {
                TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: failed to enumerate characteristics for {}: {}",
                    addr, e
                )))
            })?;

            let psm_char = self.find_char_by_uuid(&characteristics, FIPS_GATT_PSM_CHAR_UUID).await;
            let psm_char = match psm_char {
                Some(c) => c,
                None => {
                    return Err(TransportError::Io(std::io::Error::other(format!(
                        "discover_gatt_psm: PSM characteristic not found on {}",
                        addr
                    ))));
                }
            };

            let value = psm_char.read().await.map_err(|e| {
                TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: failed to read PSM characteristic on {}: {}",
                    addr, e
                )))
            })?;

            if value.len() != 2 {
                return Err(TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: expected 2-byte PSM value, got {} bytes from {}",
                    value.len(),
                    addr
                ))));
            }

            let psm = u16::from_le_bytes([value[0], value[1]]);

            if psm == 0 {
                return Err(TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: invalid PSM value 0 from {}",
                    addr
                ))));
            }

            debug!(addr = %addr, psm, "GATT PSM discovery: discovered PSM");

            Ok(psm)
        }

        async fn find_service_by_uuid(
            &self,
            services: &[bluer::gatt::remote::Service],
            target: bluer::Uuid,
        ) -> Option<bluer::gatt::remote::Service> {
            for svc in services {
                if let Ok(uuid) = svc.uuid().await {
                    if uuid == target {
                        return Some(svc.clone());
                    }
                }
            }
            None
        }

        async fn find_char_by_uuid(
            &self,
            chars: &[bluer::gatt::remote::Characteristic],
            target: bluer::Uuid,
        ) -> Option<bluer::gatt::remote::Characteristic> {
            for ch in chars {
                if let Ok(uuid) = ch.uuid().await {
                    if uuid == target {
                        return Some(ch.clone());
                    }
                }
            }
            None
        }
    }

    impl BleIo for BluerIo {
        type Stream = BluerStream;
        type Acceptor = BluerAcceptor;
        type Scanner = BluerScanner;

        async fn listen(&self, psm: u16) -> Result<Self::Acceptor, TransportError> {
            let local_addr = self
                .adapter
                .address()
                .await
                .map_err(|e| map_err("address", e))?;

            let sa = SocketAddr::new(local_addr, AddressType::LePublic, psm);
            let socket = Socket::<SeqPacket>::new_seq_packet()
                .map_err(|e| map_io_err("new_seq_packet", e))?;
            socket.bind(sa).map_err(|e| map_io_err("bind", e))?;
            socket.set_flow_control(FlowControl::Le)
                .map_err(|e| map_io_err("set_flow_control", e))?;
            socket.set_recv_mtu(self.mtu)
                .map_err(|e| map_io_err("set_recv_mtu", e))?;
            let listener = socket.listen(1)
                .map_err(|e| map_io_err("listen", e))?;

            // Prevent sniff mode to reduce latency during data transfer
            if let Err(e) = listener.as_ref().set_power_forced_active(true) {
                debug!(error = %e, "BLE listener: set_power_forced_active not supported");
            }

            debug!(psm, mtu = self.mtu, "BLE listener bound");

            Ok(BluerAcceptor {
                listener,
                adapter_name: self.adapter_name.clone(),
                send_rate_bps: self.send_rate_bps,
                send_burst_bytes: self.send_burst_bytes,
            })
        }

        async fn connect(
            &self,
            addr: &BleAddr,
            psm: u16,
        ) -> Result<Self::Stream, TransportError> {
            let bluer_addr = addr.to_bluer_address();

            let device = self.adapter.device(bluer_addr)
                .map_err(|e| map_err("device not found", e))?;

            debug!(addr = %addr, "BLE connect: GATT-first connect");
            match device.connect().await {
                Ok(()) => {
                    debug!(addr = %addr, "BLE connect: GATT connected");
                }
                Err(e) => {
                    debug!(addr = %addr, error = %e, "BLE connect: GATT connect failed, trying direct L2CAP");
                }
            }

            match device.is_paired().await {
                Ok(true) => {
                    debug!(addr = %addr, "BLE connect: device already paired");
                }
                Ok(false) => {
                    debug!(addr = %addr, "BLE connect: explicit pair() starting");
                    device
                        .pair()
                        .await
                        .map_err(|e| map_err("pair", e))?;
                    debug!(addr = %addr, "BLE connect: explicit pair() complete");
                }
                Err(e) => {
                    debug!(addr = %addr, error = %e, "BLE connect: failed to query paired state, attempting pair()");
                    device
                        .pair()
                        .await
                        .map_err(|e| map_err("pair", e))?;
                    debug!(addr = %addr, "BLE connect: explicit pair() complete after paired-state query failure");
                }
            }

            match device.is_trusted().await {
                Ok(true) => {}
                Ok(false) => {
                    if let Err(e) = device.set_trusted(true).await {
                        debug!(addr = %addr, error = %e, "BLE connect: set_trusted(true) failed (non-fatal)");
                    }
                }
                Err(e) => {
                    debug!(addr = %addr, error = %e, "BLE connect: failed to query trusted state");
                }
            }

            let addr_type = device.address_type().await.unwrap_or(AddressType::LeRandom);
            debug!(addr = %addr, addr_type = ?addr_type, "BLE connect: resolved address type after GATT");

            let target_sa = SocketAddr::new(bluer_addr, addr_type, psm);

            let socket = Socket::<SeqPacket>::new_seq_packet()
                .map_err(|e| map_io_err("new_seq_packet", e))?;
            socket
                .bind(SocketAddr::any_le())
                .map_err(|e| map_io_err("bind", e))?;

            // SOCK_SEQPACKET defaults to ERTM mode, which is not supported
            // for LE L2CAP CoC. Must explicitly set LE flow control mode
            // before connecting or the kernel returns ENOSYS.
            socket
                .set_flow_control(FlowControl::Le)
                .map_err(|e| map_io_err("set_flow_control", e))?;

            socket
                .set_recv_mtu(self.mtu)
                .map_err(|e| map_io_err("set_recv_mtu", e))?;

            if let Err(e) = socket.set_power_forced_active(true) {
                debug!(error = %e, "BLE connect: set_power_forced_active not supported");
            }

            let conn = socket.connect(target_sa).await
                .map_err(|e| map_io_err("connect", e))?;

            let remote = addr.clone();
            BluerStream::new(conn, remote, self.send_rate_bps, self.send_burst_bytes)
        }

        async fn start_advertising(&self) -> Result<(), TransportError> {
            let adv = Advertisement {
                advertisement_type: bluer::adv::Type::Peripheral,
                service_uuids: {
                    let mut s = BTreeSet::new();
                    s.insert(FIPS_SERVICE_UUID);
                    s
                },
                local_name: Some("fips".to_string()),
                min_interval: Some(std::time::Duration::from_millis(400)),
                max_interval: Some(std::time::Duration::from_millis(600)),
                ..Default::default()
            };

            let handle = self
                .adapter
                .advertise(adv)
                .await
                .map_err(|e| map_err("advertise", e))?;

            *self.adv_handle.lock().await = Some(handle);
            debug!("BLE advertising started");
            Ok(())
        }

        async fn stop_advertising(&self) -> Result<(), TransportError> {
            let _ = self.adv_handle.lock().await.take();
            debug!("BLE advertising stopped");
            Ok(())
        }

        async fn disconnect_device(&self, _addr: &BleAddr) {}

        async fn start_scanning(&self) -> Result<Self::Scanner, TransportError> {
            // Clear cached devices so BlueZ fires DeviceAdded for every
            // advertisement. Without this, already-known devices only
            // produce PropertyChanged events (which bluer doesn't expose
            // at the device level), causing the scanner to miss peers
            // after a daemon restart.
            if let Ok(cached) = self.adapter.device_addresses().await {
                let count = cached.len();
                for addr in cached {
                    let _ = self.adapter.remove_device(addr).await;
                }
                if count > 0 {
                    debug!(count, "BLE scanner: cleared cached devices");
                }
            }

            // Set discovery filter for LE transport with FIPS UUID
            let filter = DiscoveryFilter {
                transport: DiscoveryTransport::Le,
                uuids: {
                    let mut s = HashSet::new();
                    s.insert(FIPS_SERVICE_UUID);
                    s
                },
                ..Default::default()
            };

            self.adapter
                .set_discovery_filter(filter)
                .await
                .map_err(|e| map_err("set_discovery_filter", e))?;

            let events = self
                .adapter
                .discover_devices()
                .await
                .map_err(|e| map_err("discover_devices", e))?;

            debug!("BLE scanning started");

            Ok(BluerScanner {
                events: Box::pin(events),
                adapter: self.adapter.clone(),
                adapter_name: self.adapter_name.clone(),
            })
        }

        fn local_addr(&self) -> Result<BleAddr, TransportError> {
            // Use futures::executor::block_on since this is a sync method
            // but needs an async call. The adapter address is cached so
            // the D-Bus call is fast.
            let addr = futures::executor::block_on(self.adapter.address())
                .map_err(|e| map_err("address", e))?;
            Ok(BleAddr::from_bluer(addr, &self.adapter_name))
        }

        fn adapter_name(&self) -> &str {
            &self.adapter_name
        }

        fn discover_gatt_psm(
            &self,
            addr: &BleAddr,
        ) -> impl std::future::Future<Output = Result<u16, TransportError>> + Send {
            BluerIo::discover_gatt_psm(self, addr)
        }
    }

    // Compile-time assertion that BluerIo satisfies Send + Sync.
    #[allow(dead_code)]
    fn _assert_bluer_io_send_sync() {
        fn require<T: Send + Sync>() {}
        require::<BluerIo>();
    }
}

#[cfg(all(feature = "ble", target_os = "linux"))]
pub use bluer_impl::{BluerAcceptor, BluerIo, BluerScanner, BluerStream, FIPS_SERVICE_UUID};

// ============================================================================
// BluestIo — macOS BLE I/O via CoreBluetooth (bluest)
// ============================================================================

#[cfg(feature = "ble-macos")]
#[path = "io_macos.rs"]
mod bluest_impl;

#[cfg(feature = "ble-macos")]
pub use bluest_impl::{BluestAcceptor, BluestIo, BluestScanner, BluestStream};

// ============================================================================
// Mock BLE I/O (for testing without hardware)
// ============================================================================

/// Mock BLE stream backed by tokio channels.
pub struct MockBleStream {
    addr: BleAddr,
    send_mtu: u16,
    recv_mtu: u16,
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
}

impl MockBleStream {
    /// Create a linked pair of mock streams simulating an L2CAP connection.
    pub fn pair(
        addr_a: BleAddr,
        addr_b: BleAddr,
        mtu: u16,
    ) -> (Self, Self) {
        let (tx_a, rx_a) = tokio::sync::mpsc::channel(64);
        let (tx_b, rx_b) = tokio::sync::mpsc::channel(64);
        let stream_a = Self {
            addr: addr_b.clone(),
            send_mtu: mtu,
            recv_mtu: mtu,
            tx: tx_a,
            rx: tokio::sync::Mutex::new(rx_b),
        };
        let stream_b = Self {
            addr: addr_a,
            send_mtu: mtu,
            recv_mtu: mtu,
            tx: tx_b,
            rx: tokio::sync::Mutex::new(rx_a),
        };
        (stream_a, stream_b)
    }
}

impl BleStream for MockBleStream {
    async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
        self.tx
            .send(data.to_vec())
            .await
            .map_err(|_| TransportError::SendFailed("channel closed".into()))
    }

    async fn send_urgent(&self, data: &[u8]) -> Result<(), TransportError> {
        self.send(data).await
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let mut rx = self.rx.lock().await;
        match rx.recv().await {
            Some(data) => {
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
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

    async fn set_rate_bps(&self, _rate_bps: u64) {}
}

/// Mock BLE acceptor backed by a channel of pre-connected streams.
pub struct MockBleAcceptor {
    rx: tokio::sync::mpsc::Receiver<MockBleStream>,
}

impl BleAcceptor for MockBleAcceptor {
    type Stream = MockBleStream;

    async fn accept(&mut self) -> Result<MockBleStream, TransportError> {
        self.rx
            .recv()
            .await
            .ok_or(TransportError::RecvFailed("acceptor channel closed".into()))
    }
}

/// Mock BLE scanner backed by a channel of discovered addresses.
pub struct MockBleScanner {
    rx: tokio::sync::mpsc::Receiver<BleAddr>,
}

impl BleScanner for MockBleScanner {
    async fn next(&mut self) -> Option<BleAddr> {
        self.rx.recv().await
    }
}

/// Handler type for outbound mock connections.
type ConnectHandler =
    Box<dyn Fn(&BleAddr, u16) -> Result<MockBleStream, TransportError> + Send + Sync>;

/// Mock BLE I/O for testing without hardware.
///
/// Create with `MockBleIo::new()`, then use `inject_*` methods to
/// feed connections and scan results into the transport under test.
pub struct MockBleIo {
    adapter: String,
    local_addr: BleAddr,
    accept_tx: tokio::sync::mpsc::Sender<MockBleStream>,
    accept_rx: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<MockBleStream>>>,
    scan_tx: tokio::sync::mpsc::Sender<BleAddr>,
    scan_rx: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<BleAddr>>>,
    connect_handler: std::sync::Mutex<Option<ConnectHandler>>,
    gatt_psm_map: std::sync::Mutex<HashMap<[u8; 6], u16>>,
}

impl MockBleIo {
    /// Create a new mock BLE I/O with the given adapter name and address.
    pub fn new(adapter: &str, local_addr: BleAddr) -> Self {
        let (accept_tx, accept_rx) = tokio::sync::mpsc::channel(16);
        let (scan_tx, scan_rx) = tokio::sync::mpsc::channel(64);
        Self {
            adapter: adapter.to_string(),
            local_addr,
            accept_tx,
            accept_rx: std::sync::Mutex::new(Some(accept_rx)),
            scan_tx,
            scan_rx: std::sync::Mutex::new(Some(scan_rx)),
            connect_handler: std::sync::Mutex::new(None),
            gatt_psm_map: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Inject an inbound connection (simulates a remote device connecting).
    pub async fn inject_inbound(&self, stream: MockBleStream) {
        let _ = self.accept_tx.send(stream).await;
    }

    /// Inject a scan result (simulates discovering a remote device).
    pub async fn inject_scan_result(&self, addr: BleAddr) {
        let _ = self.scan_tx.send(addr).await;
    }

    /// Set a handler for outbound connect calls.
    pub fn set_connect_handler<F>(&self, handler: F)
    where
        F: Fn(&BleAddr, u16) -> Result<MockBleStream, TransportError> + Send + Sync + 'static,
    {
        *self.connect_handler.lock().unwrap() = Some(Box::new(handler));
    }

    /// Set the GATT PSM for a remote device address.
    pub fn set_gatt_psm(&self, addr: &BleAddr, psm: u16) {
        self.gatt_psm_map.lock().unwrap().insert(addr.device, psm);
    }

    /// Remove GATT PSM mapping for a remote device address.
    pub fn set_no_gatt(&self, addr: &BleAddr) {
        self.gatt_psm_map.lock().unwrap().remove(&addr.device);
    }
}

impl BleIo for MockBleIo {
    type Stream = MockBleStream;
    type Acceptor = MockBleAcceptor;
    type Scanner = MockBleScanner;

    async fn listen(&self, _psm: u16) -> Result<Self::Acceptor, TransportError> {
        let rx = self
            .accept_rx
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| TransportError::NotSupported("acceptor already taken".into()))?;
        Ok(MockBleAcceptor { rx })
    }

    async fn connect(&self, addr: &BleAddr, psm: u16) -> Result<Self::Stream, TransportError> {
        let handler = self.connect_handler.lock().unwrap();
        match handler.as_ref() {
            Some(f) => f(addr, psm),
            None => Err(TransportError::ConnectionRefused),
        }
    }

    async fn start_advertising(&self) -> Result<(), TransportError> {
        Ok(())
    }

    async fn stop_advertising(&self) -> Result<(), TransportError> {
        Ok(())
    }

    async fn disconnect_device(&self, _addr: &BleAddr) {}

    async fn start_scanning(&self) -> Result<Self::Scanner, TransportError> {
        let rx = self
            .scan_rx
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| TransportError::NotSupported("scanner already taken".into()))?;
        Ok(MockBleScanner { rx })
    }

    fn local_addr(&self) -> Result<BleAddr, TransportError> {
        Ok(self.local_addr.clone())
    }

    fn adapter_name(&self) -> &str {
        &self.adapter
    }

    fn discover_gatt_psm(
        &self,
        addr: &BleAddr,
    ) -> impl std::future::Future<Output = Result<u16, TransportError>> + Send {
        let device = addr.device;
        let map = self.gatt_psm_map.lock().unwrap();
        let result = map.get(&device).copied().ok_or_else(|| {
            TransportError::Io(std::io::Error::other(format!(
                "discover_gatt_psm: GATT PSM service not found on {}",
                addr
            )))
        });
        async move { result }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> BleAddr {
        BleAddr {
            adapter: "hci0".to_string(),
            device: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, n],
        }
    }

    #[tokio::test]
    async fn test_mock_stream_pair_send_recv() {
        let (a, b) = MockBleStream::pair(test_addr(1), test_addr(2), 2048);

        a.send(b"hello").await.unwrap();
        let mut buf = [0u8; 64];
        let n = b.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");

        b.send(b"world").await.unwrap();
        let n = a.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"world");
    }

    #[tokio::test]
    async fn test_mock_stream_mtu() {
        let (a, b) = MockBleStream::pair(test_addr(1), test_addr(2), 512);
        assert_eq!(a.send_mtu(), 512);
        assert_eq!(a.recv_mtu(), 512);
        assert_eq!(b.send_mtu(), 512);
        assert_eq!(b.recv_mtu(), 512);
    }

    #[tokio::test]
    async fn test_mock_stream_remote_addr() {
        let (a, b) = MockBleStream::pair(test_addr(1), test_addr(2), 2048);
        assert_eq!(a.remote_addr(), &test_addr(2));
        assert_eq!(b.remote_addr(), &test_addr(1));
    }

    #[tokio::test]
    async fn test_mock_io_listen_accept() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let mut acceptor = io.listen(0x0085).await.unwrap();

        let (stream_a, _stream_b) = MockBleStream::pair(test_addr(1), test_addr(2), 2048);
        io.inject_inbound(stream_a).await;

        let accepted = acceptor.accept().await.unwrap();
        // stream_a's remote_addr is addr_b (test_addr(2))
        assert_eq!(accepted.remote_addr(), &test_addr(2));
    }

    #[tokio::test]
    async fn test_mock_io_connect() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let local = test_addr(1);
        io.set_connect_handler(move |addr, _psm| {
            let (stream, _peer) = MockBleStream::pair(local.clone(), addr.clone(), 2048);
            Ok(stream)
        });

        let stream = io.connect(&test_addr(2), 0x0085).await.unwrap();
        assert_eq!(stream.remote_addr(), &test_addr(2));
    }

    #[tokio::test]
    async fn test_mock_io_connect_no_handler() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let result = io.connect(&test_addr(2), 0x0085).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_io_scan() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let mut scanner = io.start_scanning().await.unwrap();

        io.inject_scan_result(test_addr(2)).await;
        io.inject_scan_result(test_addr(3)).await;

        assert_eq!(scanner.next().await, Some(test_addr(2)));
        assert_eq!(scanner.next().await, Some(test_addr(3)));
    }

    #[tokio::test]
    async fn test_mock_io_local_addr() {
        let io = MockBleIo::new("hci0", test_addr(1));
        assert_eq!(io.local_addr().unwrap(), test_addr(1));
        assert_eq!(io.adapter_name(), "hci0");
    }

    #[tokio::test]
    async fn test_mock_io_advertising_noop() {
        let io = MockBleIo::new("hci0", test_addr(1));
        io.start_advertising().await.unwrap();
        io.stop_advertising().await.unwrap();
    }

    #[tokio::test]
    async fn test_mock_io_listen_twice_fails() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let _acceptor = io.listen(0x0085).await.unwrap();
        assert!(io.listen(0x0085).await.is_err());
    }

    #[tokio::test]
    async fn test_gatt_psm_discovery_success() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let peer = test_addr(2);
        io.set_gatt_psm(&peer, 0x0099);

        let psm = io.discover_gatt_psm(&peer).await.unwrap();
        assert_eq!(psm, 0x0099);
    }

    #[tokio::test]
    async fn test_gatt_psm_discovery_not_found() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let peer = test_addr(2);

        let result = io.discover_gatt_psm(&peer).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_gatt_psm_discovery_fallback_fixed() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let peer = test_addr(2);
        assert!(io.discover_gatt_psm(&peer).await.is_err());

        io.set_connect_handler(move |addr, psm| {
            assert_eq!(psm, 0x0085);
            let (stream, _) = MockBleStream::pair(test_addr(1), addr.clone(), 2048);
            Ok(stream)
        });
        let stream = io.connect(&peer, 0x0085).await.unwrap();
        assert_eq!(stream.remote_addr(), &peer);
    }

    #[tokio::test]
    async fn test_gatt_psm_discovery_override() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let peer = test_addr(2);

        io.set_gatt_psm(&peer, 0x0099);
        assert_eq!(io.discover_gatt_psm(&peer).await.unwrap(), 0x0099);

        io.set_gatt_psm(&peer, 0x00AA);
        assert_eq!(io.discover_gatt_psm(&peer).await.unwrap(), 0x00AA);
    }

    #[tokio::test]
    async fn test_gatt_psm_set_and_clear() {
        let io = MockBleIo::new("hci0", test_addr(1));
        let peer = test_addr(2);

        io.set_gatt_psm(&peer, 0x0099);
        assert!(io.discover_gatt_psm(&peer).await.is_ok());

        io.set_no_gatt(&peer);
        assert!(io.discover_gatt_psm(&peer).await.is_err());
    }
}
