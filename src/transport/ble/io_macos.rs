//! macOS BLE I/O implementation via `bluest` (CoreBluetooth).
//!
//! Implements the `BleIo` trait for macOS using the `bluest` crate, which
//! wraps CoreBluetooth's L2CAP CoC support.
//!
//! Current scope: outbound connections only (macOS → Linux). The acceptor
//! and advertising are stubs — inbound connections and macOS ↔ macOS
//! support require GATT-based PSM exchange (see docs/macos-ble-design.md).

use super::*;
use crate::transport::TransportError;
use crate::transport::ble::addr::BleAddr;

use bluest::{Adapter, Device};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, trace};

/// FIPS BLE service UUID (same value as Linux — derived from
/// SHA-256("FIPS: welcome to cryptoanarchy") with UUID v4 bits).
const FIPS_SERVICE_UUID: uuid::Uuid =
    uuid::Uuid::from_u128(0x9c90_b790_2cc5_42c0_9f87_c9cc_4064_8f4c);

/// Default adapter name used on macOS (CoreBluetooth doesn't expose adapter names).
const MACOS_ADAPTER_NAME: &str = "default";

// ============================================================================
// BluestStream — wraps a split L2capChannel
// ============================================================================

/// BLE stream wrapping a bluest L2CAP channel.
///
/// Raw byte-stream — no framing is added at this layer. CoreBluetooth
/// may fragment or coalesce L2CAP SDUs across reads, so callers that
/// need message boundaries must handle reassembly (see `receive_loop`
/// and `pubkey_exchange` in mod.rs).
pub struct BluestStream {
    reader: Mutex<bluest::L2capChannelReader>,
    writer: Mutex<bluest::L2capChannelWriter>,
    remote: BleAddr,
    mtu: u16,
    rate_limiter: Option<tokio::sync::Mutex<crate::transport::ble::rate_limit::SendRateLimiter>>,
}

impl BleStream for BluestStream {
    async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
        let framed_len = data.len();
        if let Some(ref limiter) = self.rate_limiter {
            limiter.lock().await.acquire(framed_len).await;
        }
        trace!(len = data.len(), framed_len, addr = %self.remote, "BLE macOS send");
        self.writer
            .lock()
            .await
            .write(data)
            .await
            .map_err(|e| TransportError::Io(std::io::Error::other(format!("BLE send: {e}"))))
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let n = self
            .reader
            .lock()
            .await
            .read(buf)
            .await
            .map_err(|e| TransportError::Io(std::io::Error::other(format!("BLE recv: {e}"))))?;
        trace!(len = n, addr = %self.remote, "BLE macOS recv");
        Ok(n)
    }

    fn send_mtu(&self) -> u16 {
        self.mtu
    }

    fn recv_mtu(&self) -> u16 {
        self.mtu
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

// ============================================================================
// BluestAcceptor — stub (macOS inbound not yet supported)
// ============================================================================

/// Stub acceptor that never accepts.
///
/// macOS inbound L2CAP connections require GATT-based PSM exchange,
/// which is not yet implemented. This acceptor blocks forever.
pub struct BluestAcceptor;

impl BleAcceptor for BluestAcceptor {
    type Stream = BluestStream;

    async fn accept(&mut self) -> Result<BluestStream, TransportError> {
        // Block forever — no inbound connections on macOS yet
        std::future::pending().await
    }
}

// ============================================================================
// BluestScanner — wraps bluest scan stream
// ============================================================================

/// Scanner that yields discovered BLE devices advertising the FIPS UUID.
pub struct BluestScanner {
    rx: tokio::sync::mpsc::Receiver<BleAddr>,
}

impl BleScanner for BluestScanner {
    async fn next(&mut self) -> Option<BleAddr> {
        self.rx.recv().await
    }
}

// ============================================================================
// BluestIo — macOS BLE I/O implementation
// ============================================================================

/// macOS BLE I/O using bluest (CoreBluetooth).
pub struct BluestIo {
    adapter: Adapter,
    mtu: u16,
    send_rate_bps: u64,
    send_burst_bytes: u32,
    devices: Arc<Mutex<HashMap<[u8; 6], Device>>>,
}

impl BluestIo {
    pub async fn new(_adapter_name: &str, mtu: u16, send_rate_bps: u64, send_burst_bytes: u32) -> Result<Self, TransportError> {
        let adapter = Adapter::default()
            .await
            .ok_or_else(|| TransportError::StartFailed("CoreBluetooth adapter not found".into()))?;

        adapter
            .wait_available()
            .await
            .map_err(|e| TransportError::StartFailed(format!("Bluetooth not available: {e}")))?;

        debug!("CoreBluetooth adapter ready");

        Ok(Self {
            adapter,
            mtu,
            send_rate_bps,
            send_burst_bytes,
            devices: Arc::new(Mutex::new(HashMap::new())),
        })
    }
}

/// Global counter for generating synthetic 6-byte pseudo-addresses.
///
/// CoreBluetooth doesn't expose real Bluetooth MAC addresses, and
/// `Device::id()` panics on current objc2-foundation (0.3.x) due to an
/// NSUUID type-encoding bug. We use a monotonic counter instead — the
/// addresses are only meaningful within this process lifetime.
static DEVICE_COUNTER: AtomicU32 = AtomicU32::new(1);

/// Generate a unique 6-byte pseudo-address for a discovered device.
fn next_device_addr() -> [u8; 6] {
    let n = DEVICE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let b = n.to_be_bytes();
    // Prefix with 0xFE 0xCA to make these visually distinct from real MACs
    [0xFE, 0xCA, b[0], b[1], b[2], b[3]]
}

impl BleIo for BluestIo {
    type Stream = BluestStream;
    type Acceptor = BluestAcceptor;
    type Scanner = BluestScanner;

    async fn listen(&self, _psm: u16) -> Result<BluestAcceptor, TransportError> {
        // macOS inbound requires GATT-based PSM exchange — not yet implemented.
        // Return a stub acceptor that blocks forever (accept_loop will just idle).
        debug!("BLE listen: macOS inbound not supported, acceptor will idle");
        Ok(BluestAcceptor)
    }

    async fn connect(&self, addr: &BleAddr, psm: u16) -> Result<BluestStream, TransportError> {
        let device = {
            let devices = self.devices.lock().await;
            devices.get(&addr.device).cloned()
        };

        let device = device.ok_or_else(|| {
            TransportError::Io(std::io::Error::other(format!(
                "BLE device not found in cache: {addr}"
            )))
        })?;

        // Ensure the device is connected at GATT level (required by CoreBluetooth
        // before opening an L2CAP channel).
        self.adapter.connect_device(&device).await.map_err(|e| {
            TransportError::Io(std::io::Error::other(format!("BLE connect {addr}: {e}")))
        })?;

        debug!(addr = %addr, psm = psm, "Opening L2CAP channel");

        let channel = device.open_l2cap_channel(psm, false).await.map_err(|e| {
            TransportError::Io(std::io::Error::other(format!(
                "L2CAP open {addr} PSM {psm}: {e}"
            )))
        })?;

        let (reader, writer) = channel.split();

        debug!(addr = %addr, psm = psm, "L2CAP channel open");

        Ok(BluestStream {
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
            remote: addr.clone(),
            mtu: self.mtu,
            rate_limiter: if self.send_rate_bps > 0 {
                Some(tokio::sync::Mutex::new(
                    crate::transport::ble::rate_limit::SendRateLimiter::new(
                        self.send_rate_bps,
                        self.send_burst_bytes,
                    )
                ))
            } else {
                None
            },
        })
    }

    async fn start_advertising(&self) -> Result<(), TransportError> {
        // macOS advertising requires GATT PSM service — not yet implemented.
        debug!("BLE advertising: macOS not yet supported (outbound only)");
        Ok(())
    }

    async fn stop_advertising(&self) -> Result<(), TransportError> {
        Ok(())
    }

    async fn start_scanning(&self) -> Result<BluestScanner, TransportError> {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let devices = self.devices.clone();
        let adapter = self.adapter.clone();

        // Spawn a task that owns the adapter clone and drives the scan stream.
        // We must call adapter.scan() inside the task because the returned stream
        // borrows the adapter (lifetime-tied), so it can't cross a spawn boundary.
        tokio::spawn(async move {
            let scan_stream = match adapter.scan(&[FIPS_SERVICE_UUID]).await {
                Ok(s) => s,
                Err(e) => {
                    debug!(error = %e, "BLE scan failed to start");
                    return;
                }
            };

            futures::pin_mut!(scan_stream);
            // Dedup repeated advertisements from the same physical
            // peripheral. Device derives Eq+Hash so we can key on it.
            let mut seen: HashMap<Device, [u8; 6]> = HashMap::new();
            while let Some(discovered) = scan_stream.next().await {
                let device = discovered.device;

                if let Some(&existing) = seen.get(&device) {
                    // Same device — report the stable address so the
                    // probe cooldown handles it.
                    let addr = BleAddr {
                        adapter: MACOS_ADAPTER_NAME.to_string(),
                        device: existing,
                    };
                    if tx.send(addr).await.is_err() {
                        break;
                    }
                    continue;
                }

                let bytes = next_device_addr();
                seen.insert(device.clone(), bytes);

                let name = discovered
                    .adv_data
                    .local_name
                    .as_deref()
                    .unwrap_or("unknown");
                debug!(
                    name = name,
                    addr = format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]),
                    "Discovered FIPS BLE device"
                );

                // Cache the Device for later connect()
                devices.lock().await.insert(bytes, device);

                let addr = BleAddr {
                    adapter: MACOS_ADAPTER_NAME.to_string(),
                    device: bytes,
                };

                if tx.send(addr).await.is_err() {
                    break; // Scanner dropped
                }
            }
            trace!("BLE scan stream ended");
        });

        Ok(BluestScanner { rx })
    }

    fn local_addr(&self) -> Result<BleAddr, TransportError> {
        // CoreBluetooth doesn't expose the local adapter address.
        // Return a synthetic address for API compatibility.
        Ok(BleAddr {
            adapter: MACOS_ADAPTER_NAME.to_string(),
            device: [0, 0, 0, 0, 0, 0],
        })
    }

    fn adapter_name(&self) -> &str {
        MACOS_ADAPTER_NAME
    }
}
