//! BLE I/O abstraction layer.
//!
//! Defines the `BleIo` trait that separates transport logic from the
//! BlueZ/bluer stack. `BluerIo` (behind `cfg(bluer_available)`) provides
//! the real implementation; `MockBleIo` provides an in-memory test double.

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
    /// No-op if rate limiting is disabled.
    fn set_rate_bps(
        &self,
        _rate_bps: u64,
    ) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }

    /// Send data with higher priority (bypasses rate limiting).
    /// Default implementation delegates to `send`.
    fn send_urgent(
        &self,
        data: &[u8],
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send {
        self.send(data)
    }

    /// Whether this stream supports bidirectional pubkey exchange.
    /// Peripheral streams do not (CoreBluetooth rejects SMP pairing).
    fn supports_bidirectional_pubkey_exchange(&self) -> bool {
        false
    }
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
    fn next(&mut self) -> impl std::future::Future<Output = Option<BleAddr>> + Send;
}

/// Core BLE I/O operations.
///
/// This trait abstracts the BlueZ/bluer stack so that `BleTransport`
/// can be tested with `MockBleIo` (in-memory channels) in CI without
/// requiring Bluetooth hardware, D-Bus, or bluetoothd.
pub trait BleIo: Send + Sync + 'static {
    type Stream: BleStream + 'static;
    type Acceptor: BleAcceptor<Stream = Self::Stream> + 'static;
    type Scanner: BleScanner + 'static;

    fn listen(
        &self,
        psm: u16,
    ) -> impl std::future::Future<Output = Result<Self::Acceptor, TransportError>> + Send;

    fn connect(
        &self,
        addr: &BleAddr,
        psm: u16,
    ) -> impl std::future::Future<Output = Result<Self::Stream, TransportError>> + Send;

    fn start_advertising(
        &self,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send;

    fn stop_advertising(
        &self,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send;

    fn disconnect_device(
        &self,
        _addr: &BleAddr,
    ) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }

    fn start_scanning(
        &self,
    ) -> impl std::future::Future<Output = Result<Self::Scanner, TransportError>> + Send;

    fn local_addr(&self) -> Result<BleAddr, TransportError>;

    fn adapter_name(&self) -> &str;

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

#[cfg(all(feature = "ble", target_os = "linux"))]
fn try_take_framed_payload(
    recv_buf: &mut Vec<u8>,
    buf: &mut [u8],
    max_payload_len: usize,
) -> Result<Option<usize>, TransportError> {
    if recv_buf.len() < 2 {
        return Ok(None);
    }

    let payload_len = u16::from_be_bytes([recv_buf[0], recv_buf[1]]) as usize;
    if payload_len > max_payload_len {
        return Err(TransportError::RecvFailed(format!(
            "BLE recv: invalid frame header {} exceeds max payload {}",
            payload_len, max_payload_len
        )));
    }

    if payload_len == 0 {
        recv_buf.drain(..2);
        return Err(TransportError::RecvFailed(
            "BLE recv: framed message too short (0 bytes)".into(),
        ));
    }

    if recv_buf.len() < 2 + payload_len {
        return Ok(None);
    }

    let copy_len = payload_len.min(buf.len());
    buf[..copy_len].copy_from_slice(&recv_buf[2..2 + copy_len]);
    recv_buf.drain(..2 + payload_len);
    Ok(Some(copy_len))
}

// ============================================================================
// BluerIo — Production BLE I/O via BlueZ D-Bus
// ============================================================================

#[cfg(bluer_available)]
mod bluer_impl {
    use super::*;
    use crate::transport::TransportError;

    use bluer::l2cap::{FlowControl, SeqPacket, SeqPacketListener, Socket, SocketAddr};
    use bluer::{
        AdapterEvent, AddressType, DiscoveryFilter, DiscoveryTransport, adv::Advertisement,
    };
    use bluer::Address;
    use futures::StreamExt;
    use std::collections::{BTreeSet, HashSet};
    use std::pin::Pin;
    use tokio::sync::Mutex;
    use tokio::time::{Duration, timeout};
    use tracing::{debug, trace, warn};

    pub const FIPS_SERVICE_UUID: bluer::Uuid =
        bluer::Uuid::from_u128(0x9c90_b790_2cc5_42c0_9f87_c9cc_4064_8f4c);

    pub const FIPS_GATT_PSM_SERVICE_UUID: bluer::Uuid =
        bluer::Uuid::from_u128(0x0e2c_43b1_51b9_4667_a1d1_a95e_a79f_d19b);

    pub const FIPS_GATT_PSM_CHAR_UUID: bluer::Uuid =
        bluer::Uuid::from_u128(0x250c_88dd_3dff_4c41_83b2_f1b4_e3d8_20cc);

    fn map_err(context: &str, e: bluer::Error) -> TransportError {
        TransportError::Io(std::io::Error::other(format!("{}: {}", context, e)))
    }

    fn map_io_err(context: &str, e: std::io::Error) -> TransportError {
        TransportError::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {}", context, e),
        ))
    }

    fn local_addr_addr_type(addr: &Address) -> AddressType {
        if addr.0[0] & 0x01 == 0 {
            AddressType::LePublic
        } else {
            AddressType::LeRandom
        }
    }

    const STARTUP_RETRY_DELAY: Duration = Duration::from_millis(750);
    const STARTUP_RETRY_ATTEMPTS: usize = 3;
    const BLE_SEND_TIMEOUT: Duration = Duration::from_secs(15);

    fn is_transient_startup_error(error: &bluer::Error) -> bool {
        matches!(
            error.kind,
            bluer::ErrorKind::AuthenticationFailed
                | bluer::ErrorKind::Failed
                | bluer::ErrorKind::InProgress
                | bluer::ErrorKind::NotReady
        )
    }

    // ----------------------------------------------------------------
    // BluerStream
    // ----------------------------------------------------------------

    pub struct BluerStream {
        conn: SeqPacket,
        remote: BleAddr,
        send_mtu: u16,
        recv_mtu: u16,
        rate_limiter: Option<tokio::sync::Mutex<super::super::rate_limit::SendRateLimiter>>,
        recv_buf: tokio::sync::Mutex<Vec<u8>>,
    }

    impl BluerStream {
        pub fn new(conn: SeqPacket, remote: BleAddr, send_rate_bps: u64, send_burst_bytes: u32) -> Result<Self, TransportError> {
            let send_mtu = conn.send_mtu().map_err(|e| map_io_err("send_mtu", e))? as u16;
            let recv_mtu = conn.recv_mtu().map_err(|e| map_io_err("recv_mtu", e))? as u16;

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
                        super::super::rate_limit::SendRateLimiter::new(send_rate_bps, send_burst_bytes)
                    ))
                } else {
                    None
                },
                recv_buf: tokio::sync::Mutex::new(Vec::new()),
            })
        }
    }

    impl BleStream for BluerStream {
        async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
            let framed_len = 2 + data.len();
            if framed_len > self.send_mtu as usize {
                return Err(TransportError::MtuExceeded {
                    packet_size: framed_len,
                    mtu: self.send_mtu,
                });
            }
            if let Some(ref limiter) = self.rate_limiter {
                limiter.lock().await.acquire(framed_len).await;
            }

            let mut framed = Vec::with_capacity(framed_len);
            framed.extend_from_slice(&(data.len() as u16).to_be_bytes());
            framed.extend_from_slice(data);
            tokio::time::timeout(BLE_SEND_TIMEOUT, self.conn.send(&framed))
                .await
                .map_err(|_| {
                    warn!(len = framed.len(), timeout_secs = BLE_SEND_TIMEOUT.as_secs(), "BLE write timeout");
                    TransportError::Timeout
                })?
                .map(|_| ())
                .map_err(|e| TransportError::SendFailed(format!("{}", e)))
        }

        async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
            let max_payload_len = self.recv_mtu.saturating_sub(2) as usize;

            loop {
                {
                    let mut recv_buf = self.recv_buf.lock().await;
                    if let Some(copy_len) =
                        try_take_framed_payload(&mut recv_buf, buf, max_payload_len)?
                    {
                        trace!(
                            len = copy_len,
                            buf_remaining = recv_buf.len(),
                            addr = %self.remote,
                            "BLE linux recv frame"
                        );
                        return Ok(copy_len);
                    }
                }

                let mut chunk = vec![0u8; self.recv_mtu as usize];
                let n = self
                    .conn
                    .recv(&mut chunk)
                    .await
                    .map_err(|e| TransportError::RecvFailed(format!("{}", e)))?;

                if n == 0 {
                    return Ok(0);
                }

                trace!(raw_bytes = n, addr = %self.remote, "BLE linux recv raw");
                self.recv_buf.lock().await.extend_from_slice(&chunk[..n]);
            }
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

    pub struct BluerScanner {
        events: Pin<Box<dyn futures::Stream<Item = AdapterEvent> + Send>>,
        adapter: bluer::Adapter,
        adapter_name: String,
        initialized: bool,
    }

    impl BleScanner for BluerScanner {
        async fn next(&mut self) -> Option<BleAddr> {
            // On first call, emit FIPS peers already in BlueZ cache.
            // Devices persist in BlueZ across FIPS restarts, so DeviceAdded
            // won't fire for them. Scan the cache once to pick them up.
            if !self.initialized {
                self.initialized = true;
                if let Ok(addrs) = self.adapter.device_addresses().await {
                    for addr in addrs {
                        if let Ok(device) = self.adapter.device(addr) {
                            match device.uuids().await {
                                Ok(Some(uuids)) if uuids.contains(&FIPS_SERVICE_UUID) => {
                                    let ble_addr = BleAddr::from_bluer(addr, &self.adapter_name);
                                    debug!(addr = %ble_addr, "BLE scanner: cached FIPS peer found");
                                    return Some(ble_addr);
                                }
                                _ => continue,
                            }
                        }
                    }
                }
            }

            loop {
                match self.events.next().await {
                    Some(AdapterEvent::DeviceAdded(addr)) => {
                        if let Ok(device) = self.adapter.device(addr) {
                            match device.uuids().await {
                                Ok(Some(uuids)) if uuids.contains(&FIPS_SERVICE_UUID) => {
                                    let ble_addr = BleAddr::from_bluer(addr, &self.adapter_name);
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

    pub struct BluerIo {
        #[allow(dead_code)]
        session: bluer::Session,
        adapter: bluer::Adapter,
        adapter_name: String,
        adv_handle: Mutex<Option<bluer::adv::AdvertisementHandle>>,
        #[allow(dead_code)]
        agent_handle: bluer::agent::AgentHandle,
        mtu: u16,
        send_rate_bps: u64,
        send_burst_bytes: u32,
    }

    impl BluerIo {
        fn device_handle(&self, addr: &BleAddr) -> Result<bluer::Device, TransportError> {
            self.adapter
                .device(addr.to_bluer_address())
                .map_err(|e| map_err("device", e))
        }

        pub async fn new(adapter_name: &str, mtu: u16, send_rate_bps: u64, send_burst_bytes: u32) -> Result<Self, TransportError> {
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

            if let Err(e) = adapter.set_pairable_timeout(0).await {
                debug!(error = %e, "BLE: failed to set pairable timeout to 0");
            }

            if let Err(e) = adapter.set_pairable(true).await {
                debug!(error = %e, "BLE: failed to set adapter pairable");
            }

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
            let device = match self.device_handle(addr) {
                Ok(device) => device,
                Err(_) => return AddressType::LeRandom,
            };

            if let Ok(addr_type) = device.address_type().await {
                return addr_type;
            }

            debug!(addr = %addr, "BLE connect: device not in cache, starting discovery scan");

            let filter = DiscoveryFilter {
                transport: DiscoveryTransport::Le,
                ..Default::default()
            };

            if let Err(e) = self.adapter.set_discovery_filter(filter).await {
                debug!(addr = %addr, error = %e, "BLE connect: failed to set discovery filter");
                return AddressType::LeRandom;
            }

            match self.adapter.discover_devices_with_changes().await {
                Ok(mut events) => {
                    let scan_result = timeout(Duration::from_secs(5), async {
                        while let Some(event) = events.next().await {
                            if let AdapterEvent::DeviceAdded(found_addr) = event
                                && found_addr == bluer_addr
                                && let Ok(addr_type) = device.address_type().await
                            {
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
                        Ok(None) | Err(_) => {
                            let addr_type = device.address_type().await.unwrap_or(AddressType::LeRandom);
                            debug!(addr = %addr, addr_type = ?addr_type, "BLE connect: discovery scan cached device addr_type");
                            addr_type
                        }
                    }
                }
                Err(e) => {
                    debug!(addr = %addr, error = %e, "BLE connect: failed to start discovery scan");
                    AddressType::LeRandom
                }
            }
        }

        pub async fn discover_gatt_psm(&self, addr: &BleAddr) -> Result<u16, TransportError> {
            let discover = async {
                let _ = self.resolve_addr_type(addr).await;
                let device = self.device_handle(addr)?;

                debug!(addr = %addr, "GATT PSM discovery: connecting GATT");

                device.connect().await.map_err(|e| {
                    TransportError::Io(std::io::Error::other(format!(
                        "discover_gatt_psm: GATT connect failed for {}: {}",
                        addr, e
                    )))
                })?;

                let result = self.read_psm_from_gatt(&device, addr).await;

                if let Err(e) = device.disconnect().await {
                    debug!(addr = %addr, error = %e, "GATT PSM discovery: GATT disconnect failed (non-fatal)");
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

            debug!(addr = %addr, count = services.len(), "GATT PSM discovery: enumerated services");

            let psm_service = find_service_by_uuid(&services, FIPS_GATT_PSM_SERVICE_UUID).await;
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

            let psm_char = find_char_by_uuid(&characteristics, FIPS_GATT_PSM_CHAR_UUID).await;
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
    }

    async fn find_service_by_uuid(
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

            let socket = Socket::<SeqPacket>::new_seq_packet()
                .map_err(|e| map_io_err("new_seq_packet", e))?;

            let addr_type = local_addr_addr_type(&local_addr);
            let sa = SocketAddr::new(local_addr, addr_type, psm);
            socket.bind(sa).map_err(|e| map_io_err("bind", e))?;
            socket.set_flow_control(FlowControl::Le)
                .map_err(|e| map_io_err("set_flow_control", e))?;
            socket.set_recv_mtu(self.mtu)
                .map_err(|e| map_io_err("set_recv_mtu", e))?;
            let listener = socket.listen(1)
                .map_err(|e| map_io_err("listen", e))?;

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

        async fn connect(&self, addr: &BleAddr, psm: u16) -> Result<Self::Stream, TransportError> {
            let connect_start = std::time::Instant::now();
            let mut effective_psm = psm;
            let addr_type = self.resolve_addr_type(addr).await;
            let bluer_addr = addr.to_bluer_address();
            let device = self.device_handle(addr)?;

            debug!(addr = %addr, configured_psm = psm, addr_type = ?addr_type, "BLE connect: starting");

            // GATT connect with retry for transient errors
            const GATT_RETRY_DELAY: Duration = Duration::from_secs(1);
            const MAX_GATT_ATTEMPTS: usize = 2;
            const GATT_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

            let gatt_start = std::time::Instant::now();
            let mut gatt_connected = false;
            for attempt in 1..=MAX_GATT_ATTEMPTS {
                match timeout(GATT_CONNECT_TIMEOUT, device.connect()).await {
                    Ok(Ok(())) => {
                        debug!(
                            addr = %addr, attempt,
                            gatt_ms = gatt_start.elapsed().as_millis() as u64,
                            "BLE connect: GATT connected"
                        );
                        gatt_connected = true;
                        break;
                    }
                    Ok(Err(e)) => {
                        let err_str = format!("{e}");
                        if attempt < MAX_GATT_ATTEMPTS
                            && (err_str.contains("abort-by-local") || err_str.contains("Not Ready"))
                        {
                            debug!(
                                addr = %addr, attempt, error = %e,
                                "BLE connect: GATT connect aborted by local stack, retrying"
                            );
                            if let Err(de) = device.disconnect().await {
                                debug!(addr = %addr, error = %de, "BLE connect: GATT disconnect before retry (non-fatal)");
                            }
                            tokio::time::sleep(GATT_RETRY_DELAY).await;
                            continue;
                        }
                        debug!(addr = %addr, attempt, error = %e, "BLE connect: GATT connect failed");
                        if let Err(de) = device.disconnect().await {
                            debug!(addr = %addr, error = %de, "BLE connect: GATT disconnect after failure (non-fatal)");
                        }
                        break;
                    }
                    Err(_) => {
                        debug!(addr = %addr, attempt, "BLE connect: GATT connect timed out");
                        if let Err(de) = device.disconnect().await {
                            debug!(addr = %addr, error = %de, "BLE connect: GATT disconnect after timeout (non-fatal)");
                        }
                        break;
                    }
                }
            }

            if gatt_connected {
                let psm_start = std::time::Instant::now();
                match timeout(Duration::from_secs(5), self.read_psm_from_gatt(&device, addr)).await {
                    Ok(Ok(discovered_psm)) => {
                        effective_psm = discovered_psm;
                        debug!(
                            addr = %addr, configured_psm = psm, discovered_psm,
                            psm_discovery_ms = psm_start.elapsed().as_millis() as u64,
                            "BLE connect: using GATT-discovered PSM"
                        );
                    }
                    Ok(Err(e)) => {
                        debug!(addr = %addr, configured_psm = psm, error = %e, "BLE connect: GATT PSM discovery failed, using configured PSM");
                    }
                    Err(_) => {
                        debug!(addr = %addr, configured_psm = psm, "BLE connect: GATT PSM discovery timed out, using configured PSM");
                    }
                }
            } else {
                debug!(addr = %addr, configured_psm = psm, "BLE connect: GATT connect failed, falling back to L2CAP with configured PSM");
            }

            // Set trusted if not already
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

            let addr_type = device.address_type().await.unwrap_or(addr_type);
            debug!(addr = %addr, addr_type = ?addr_type, "BLE connect: resolved address type after GATT");

            let target_sa = SocketAddr::new(bluer_addr, addr_type, effective_psm);

            let l2cap_start = std::time::Instant::now();
            let socket = Socket::<SeqPacket>::new_seq_packet()
                .map_err(|e| map_io_err("new_seq_packet", e))?;
            socket
                .bind(SocketAddr::any_le())
                .map_err(|e| map_io_err("bind", e))?;
            socket
                .set_flow_control(FlowControl::Le)
                .map_err(|e| map_io_err("set_flow_control", e))?;
            socket
                .set_recv_mtu(self.mtu)
                .map_err(|e| map_io_err("set_recv_mtu", e))?;

            if let Err(e) = socket.set_power_forced_active(true) {
                debug!(error = %e, "BLE connect: set_power_forced_active not supported");
            }

            debug!(addr = %addr, addr_type = ?addr_type, psm = effective_psm, "BLE connect: opening LE L2CAP socket");

            let conn_result = socket.connect(target_sa).await;
            match conn_result {
                Ok(conn) => {
                    debug!(
                        addr = %addr,
                        l2cap_ms = l2cap_start.elapsed().as_millis() as u64,
                        total_ms = connect_start.elapsed().as_millis() as u64,
                        "BLE connect: L2CAP channel established"
                    );
                    let remote = addr.clone();
                    BluerStream::new(conn, remote, self.send_rate_bps, self.send_burst_bytes)
                }
                Err(e) => {
                    if let Err(e) = device.disconnect().await {
                        debug!(addr = %addr, error = %e, "BLE connect: GATT disconnect after L2CAP failure (non-fatal)");
                    }
                    Err(map_io_err("connect", e))
                }
            }
        }

        async fn start_advertising(&self) -> Result<(), TransportError> {
            for attempt in 1..=STARTUP_RETRY_ATTEMPTS {
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

                match self.adapter.advertise(adv).await {
                    Ok(handle) => {
                        *self.adv_handle.lock().await = Some(handle);
                        debug!(attempt, "BLE advertising started");
                        return Ok(());
                    }
                    Err(error)
                        if attempt < STARTUP_RETRY_ATTEMPTS
                            && is_transient_startup_error(&error) =>
                    {
                        warn!(
                            attempt,
                            retry_in_ms = STARTUP_RETRY_DELAY.as_millis(),
                            kind = ?error.kind,
                            message = %error.message,
                            "BLE advertising start hit transient controller state; retrying"
                        );
                        tokio::time::sleep(STARTUP_RETRY_DELAY).await;
                    }
                    Err(error) => return Err(map_err("advertise", error)),
                }
            }

            Err(TransportError::Io(std::io::Error::other(
                "advertise: exhausted retries",
            )))
        }

        async fn stop_advertising(&self) -> Result<(), TransportError> {
            let _ = self.adv_handle.lock().await.take();
            debug!("BLE advertising stopped");
            Ok(())
        }

        async fn disconnect_device(&self, addr: &BleAddr) {
            let device = match self.device_handle(addr) {
                Ok(d) => d,
                Err(e) => {
                    debug!(addr = %addr, error = %e, "BLE disconnect: device not found");
                    return;
                }
            };
            match device.disconnect().await {
                Ok(()) => debug!(addr = %addr, "BLE device disconnected"),
                Err(e) => debug!(addr = %addr, error = %e, "BLE disconnect failed (non-fatal)"),
            }
        }

        async fn start_scanning(&self) -> Result<Self::Scanner, TransportError> {
            if let Ok(cached) = self.adapter.device_addresses().await {
                let count = cached.len();
                for addr in cached {
                    let _ = self.adapter.remove_device(addr).await;
                }
                if count > 0 {
                    debug!(count, "BLE scanner: cleared cached devices");
                }
            }

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

            for attempt in 1..=STARTUP_RETRY_ATTEMPTS {
                match self.adapter.discover_devices().await {
                    Ok(events) => {
                        debug!(attempt, "BLE scanning started");
                        return Ok(BluerScanner {
                            events: Box::pin(events),
                            adapter: self.adapter.clone(),
                            adapter_name: self.adapter_name.clone(),
                            initialized: false,
                        });
                    }
                    Err(error)
                        if attempt < STARTUP_RETRY_ATTEMPTS
                            && is_transient_startup_error(&error) =>
                    {
                        warn!(
                            attempt,
                            retry_in_ms = STARTUP_RETRY_DELAY.as_millis(),
                            kind = ?error.kind,
                            message = %error.message,
                            "BLE scanning start hit transient controller state; retrying"
                        );
                        tokio::time::sleep(STARTUP_RETRY_DELAY).await;
                    }
                    Err(error) => return Err(map_err("discover_devices", error)),
                }
            }

            Err(TransportError::Io(std::io::Error::other(
                "discover_devices: exhausted retries",
            )))
        }

        fn local_addr(&self) -> Result<BleAddr, TransportError> {
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

    #[allow(dead_code)]
    fn _assert_bluer_io_send_sync() {
        fn require<T: Send + Sync>() {}
        require::<BluerIo>();
    }
}

#[cfg(bluer_available)]
pub use bluer_impl::{
    BluerAcceptor, BluerIo, BluerScanner, BluerStream, FIPS_SERVICE_UUID,
    FIPS_GATT_PSM_SERVICE_UUID, FIPS_GATT_PSM_CHAR_UUID,
};

// ============================================================================
// BluestIo — macOS BLE I/O via CoreBluetooth (bluest)
// ============================================================================

#[cfg(feature = "ble-macos")]
#[path = "io_macos.rs"]
mod bluest_impl;

#[cfg(feature = "ble-macos")]
pub use bluest_impl::{AnyStream, BluestAcceptor, BluestIo, BluestScanner, BluestStream};

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
    pub fn pair(addr_a: BleAddr, addr_b: BleAddr, mtu: u16) -> (Self, Self) {
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

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let mut rx = self.rx.lock().await;
        match rx.recv().await {
            Some(data) => {
                if data.len() < 2 {
                    return Err(TransportError::RecvFailed("frame too short".into()));
                }
                let payload_len = u16::from_be_bytes([data[0], data[1]]) as usize;
                if payload_len == 0 {
                    return Err(TransportError::RecvFailed(
                        "BLE recv: framed message too short (0 bytes)".into(),
                    ));
                }
                let payload = &data[2..];
                let len = payload_len.min(payload.len()).min(buf.len());
                buf[..len].copy_from_slice(&payload[..len]);
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
}
