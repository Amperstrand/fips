//! BLE I/O implementation for macOS using bluest (CoreBluetooth).
//!
//! This module provides `BluestIo`, a BLE central-role implementation for
//! macOS using the `bluest` crate. macOS CoreBluetooth does not support
//! the peripheral role (L2CAP server, advertising), so those methods
//! return `TransportError::NotSupported`.

use crate::transport::TransportError;

use super::super::addr::{BleAddr, BleDeviceAddr};
use super::{BleIo, BleStream, BleAcceptor, BleScanner};
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, trace};

/// FIPS BLE service UUID.
///
/// Derived from SHA-256("FIPS: welcome to cryptoanarchy") with UUID v4
/// version/variant bits applied. Must match the BlueZ implementation.
pub const FIPS_SERVICE_UUID: uuid::Uuid = uuid::uuid!("9c90b790-2cc5-42c0-9f87-c9cc40648f4c");

/// Map a bluest error to a TransportError.
fn map_err(context: &str, e: bluest::Error) -> TransportError {
    TransportError::Io(std::io::Error::other(format!("{}: {}", context, e)))
}

// ============================================================================
// Placeholder types (to be implemented in later tasks)
// ============================================================================

/// Placeholder for BLE stream (to be implemented).
///
/// Will wrap a bluest GATT characteristic subscription for receiving data
/// and characteristic write for sending data.
pub struct BluestStream;

impl BleStream for BluestStream {
    async fn send(&self, _data: &[u8]) -> Result<(), TransportError> {
        todo!("send() will be implemented in Tasks 6-8")
    }

    async fn recv(&self, _buf: &mut [u8]) -> Result<usize, TransportError> {
        todo!("recv() will be implemented in Tasks 6-8")
    }

    fn send_mtu(&self) -> u16 {
        0
    }

    fn recv_mtu(&self) -> u16 {
        0
    }

    fn remote_addr(&self) -> &BleAddr {
        todo!("remote_addr() will be implemented in Tasks 6-8")
    }
}

/// Placeholder for BLE acceptor (peripheral role - not supported on macOS).
///
/// macOS CoreBluetooth does not support L2CAP server sockets. This type
/// exists only to satisfy the BleAcceptor trait's associated type.
pub struct BluestAcceptor;

impl BleAcceptor for BluestAcceptor {
    type Stream = BluestStream;

    async fn accept(&mut self) -> Result<BluestStream, TransportError> {
        Err(TransportError::NotSupported(
            "BLE peripheral role not supported on macOS (bluest is central-only)".into(),
        ))
    }
}

/// BLE scanner that receives discovered devices via a channel.
///
/// Uses a background task to own the scan stream (which borrows from the
/// adapter) and sends discovered devices over an mpsc channel. This decouples
/// the scan stream's lifetime from the scanner struct.
pub struct BluestScanner {
    receiver: mpsc::Receiver<BleAddr>,
}

impl BleScanner for BluestScanner {
    fn next(
        &mut self,
    ) -> impl std::future::Future<Output = Option<BleAddr>> + Send {
        async move {
            self.receiver.recv().await
        }
    }
}

// ============================================================================
// BluestIo
// ============================================================================

/// BLE I/O implementation for macOS using bluest (CoreBluetooth).
///
/// This is a central-only implementation. macOS CoreBluetooth does not
/// support the peripheral role, so:
/// - `listen()` returns `NotSupported`
/// - `start_advertising()` returns `NotSupported`
/// - `stop_advertising()` returns `NotSupported`
///
/// The bluest::Adapter is clonable and Send+Sync, making it safe to share
/// across async tasks.
pub struct BluestIo {
    adapter: bluest::Adapter,
    adapter_name: String,
    mtu: u16,
}

impl BluestIo {
    /// Create a new BluestIo for the default adapter.
    ///
    /// Connects to CoreBluetooth and waits for the adapter to become available.
    pub async fn new(adapter_name: &str, mtu: u16) -> Result<Self, TransportError> {
        let adapter = bluest::Adapter::default()
            .await
            .ok_or_else(|| TransportError::Io(std::io::Error::other("Bluetooth adapter not found")))?;

        adapter
            .wait_available()
            .await
            .map_err(|e| TransportError::Io(std::io::Error::other(format!("Adapter not available: {}", e))))?;

        Ok(Self {
            adapter,
            adapter_name: adapter_name.to_string(),
            mtu,
        })
    }
}

impl BleIo for BluestIo {
    type Stream = BluestStream;
    type Acceptor = BluestAcceptor;
    type Scanner = BluestScanner;

    async fn listen(&self, _psm: u16) -> Result<Self::Acceptor, TransportError> {
        Err(TransportError::NotSupported(
            "BLE peripheral role not supported on macOS (bluest is central-only)".into(),
        ))
    }

    async fn connect(&self, _addr: &BleAddr, _psm: u16) -> Result<Self::Stream, TransportError> {
        todo!("connect() will be implemented in Tasks 6-8")
    }

    async fn start_advertising(&self) -> Result<(), TransportError> {
        Err(TransportError::NotSupported(
            "BLE advertising not supported on macOS (bluest is central-only)".into(),
        ))
    }

    async fn stop_advertising(&self) -> Result<(), TransportError> {
        Err(TransportError::NotSupported(
            "BLE advertising not supported on macOS (bluest is central-only)".into(),
        ))
    }

    async fn start_scanning(&self) -> Result<Self::Scanner, TransportError> {
        let (tx, rx) = mpsc::channel(16);
        let adapter = self.adapter.clone();
        let adapter_name = self.adapter_name.clone();

        tokio::spawn(async move {
            match adapter.scan(&[FIPS_SERVICE_UUID]).await {
                Ok(mut scan_stream) => {
                    debug!("BLE scanning started (macOS/bluest)");
                    while let Some(discovered) = scan_stream.next().await {
                        let device_id = discovered.device.id();
                        let ble_addr = BleAddr::from_bluest(device_id, &adapter_name);
                        debug!(addr = %ble_addr, "BLE scanner: FIPS peer found");
                        if tx.send(ble_addr).await.is_err() {
                            trace!("BLE scanner: receiver dropped, stopping scan");
                            break;
                        }
                    }
                }
                Err(e) => {
                    debug!("BLE scanner failed: {}", e);
                }
            }
        });

        Ok(BluestScanner { receiver: rx })
    }

    fn local_addr(&self) -> Result<BleAddr, TransportError> {
        // macOS CoreBluetooth doesn't expose a MAC address. Use a UUID-based
        // device identifier. For now, return a placeholder UUID.
        // TODO: Get actual device identifier from bluest adapter.
        Ok(BleAddr {
            adapter: self.adapter_name.clone(),
            device: BleDeviceAddr::Uuid([0u8; 16]),
        })
    }

    fn adapter_name(&self) -> &str {
        &self.adapter_name
    }
}

// Compile-time assertion that BluestIo satisfies Send + Sync.
#[allow(dead_code)]
fn _assert_bluest_io_send_sync() {
    fn require<T: Send + Sync>() {}
    require::<BluestIo>();
}
