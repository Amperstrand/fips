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
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, trace};

/// FIPS BLE service UUID.
pub const FIPS_SERVICE_UUID: uuid::Uuid = uuid::uuid!("9c90b790-2cc5-42c0-9f87-c9cc40648f4c");

type DeviceCache = Arc<Mutex<HashMap<[u8; 16], bluest::Device>>>;

/// BLE stream wrapping a bluest L2CAP channel.
pub struct BluestStream {
    reader: tokio::sync::Mutex<bluest::L2capChannelReader>,
    writer: tokio::sync::Mutex<bluest::L2capChannelWriter>,
    remote: BleAddr,
    mtu: u16,
}

impl BluestStream {
    pub fn new(channel: bluest::L2capChannel, remote: BleAddr, mtu: u16) -> Self {
        let (reader, writer) = channel.split();
        Self {
            reader: tokio::sync::Mutex::new(reader),
            writer: tokio::sync::Mutex::new(writer),
            remote,
            mtu,
        }
    }
}

impl BleStream for BluestStream {
    async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
        let mut writer = self.writer.lock().await;
        writer.write(data).await.map_err(|e| TransportError::SendFailed(format!("{}", e)))
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let mut reader = self.reader.lock().await;
        reader.read(buf).await.map_err(|e| TransportError::RecvFailed(format!("{}", e)))
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
}

#[derive(Debug)]
pub struct BluestAcceptor;

impl BleAcceptor for BluestAcceptor {
    type Stream = BluestStream;

    async fn accept(&mut self) -> Result<BluestStream, TransportError> {
        Err(TransportError::NotSupported(
            "BLE peripheral role not supported on macOS (bluest is central-only)".into(),
        ))
    }
}
pub struct BluestScanner {
    receiver: mpsc::Receiver<BleAddr>,
}

impl BleScanner for BluestScanner {
    fn next(&mut self) -> impl std::future::Future<Output = Option<BleAddr>> + Send {
        async move { self.receiver.recv().await }
    }
}
async fn find_device_by_uuid(
    adapter: &bluest::Adapter,
    target_uuid: &[u8; 16],
    timeout_secs: u64,
) -> Result<bluest::Device, TransportError> {
    let mut scan = adapter
        .scan(&[FIPS_SERVICE_UUID])
        .await
        .map_err(|e| {
            TransportError::Io(std::io::Error::other(format!("Failed to start scan: {}", e)))
        })?;

    let timeout = tokio::time::Duration::from_secs(timeout_secs);
    let target_uuid_obj = uuid::Uuid::from_bytes_ref(target_uuid);
    match tokio::time::timeout(timeout, async {
        while let Some(discovered) = scan.next().await {
            let device_id = discovered.device.id();
            let device_id_str = device_id.to_string();
            if let Ok(discovered_uuid_obj) = uuid::Uuid::parse_str(&device_id_str) {
                if discovered_uuid_obj == *target_uuid_obj {
                    return Some(discovered.device);
                }
            }
        }
        None
    }).await {
        Ok(Some(device)) => Ok(device),
        Ok(None) => Err(TransportError::LinkFailed("BLE device not found during scan".into())),
        Err(_) => Err(TransportError::LinkFailed(format!("BLE device discovery timed out after {}s", timeout_secs))),
    }
}
pub struct BluestIo {
    adapter: bluest::Adapter,
    adapter_name: String,
    mtu: u16,
    device_cache: DeviceCache,
}
impl BluestIo {
    pub async fn new(adapter_name: &str, mtu: u16) -> Result<Self, TransportError> {
        let adapter = bluest::Adapter::default()
            .await
            .ok_or_else(|| TransportError::Io(std::io::Error::other("Bluetooth adapter not found")))?;

        adapter
            .wait_available()
            .await
            .map_err(|e| {
                TransportError::Io(std::io::Error::other(format!("Adapter not available: {}", e)))
            })?;

        Ok(Self {
            adapter,
            adapter_name: adapter_name.to_string(),
            mtu,
            device_cache: Arc::new(Mutex::new(HashMap::new())),
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

    async fn connect(&self, addr: &BleAddr, psm: u16) -> Result<Self::Stream, TransportError> {
        let device_uuid = match &addr.device {
            BleDeviceAddr::Uuid(bytes) => *bytes,
            BleDeviceAddr::Mac(_) => {
                return Err(TransportError::InvalidAddress(
                    "macOS BLE connect requires UUID-based device address".into(),
                ));
            }
        };

        let device = {
            let cache = self.device_cache.lock().await;
            if let Some(device) = cache.get(&device_uuid) {
                debug!(uuid = %uuid::Uuid::from_bytes_ref(&device_uuid), "BLE connect: using cached device");
                device.clone()
            } else {
                drop(cache);
                debug!(uuid = %uuid::Uuid::from_bytes_ref(&device_uuid), "BLE connect: device not cached, discovering...");
                find_device_by_uuid(&self.adapter, &device_uuid, 30).await?
            }
        };

        self.adapter.connect_device(&device).await.map_err(|e| {
            TransportError::LinkFailed(format!("Failed to connect to BLE device: {}", e))
        })?;

        let channel = device.open_l2cap_channel(psm, true).await.map_err(|e| {
            TransportError::Io(std::io::Error::other(format!("Failed to open L2CAP channel: {}", e)))
        })?;

        debug!(addr = %addr, psm, mtu = self.mtu, "BLE L2CAP channel established");
        Ok(BluestStream::new(channel, addr.clone(), self.mtu))
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
        let device_cache = self.device_cache.clone();

        tokio::spawn(async move {
            match adapter.scan(&[FIPS_SERVICE_UUID]).await {
                Ok(mut scan_stream) => {
                    debug!("BLE scanning started (macOS/bluest)");
                    while let Some(discovered) = scan_stream.next().await {
                        let device_id = discovered.device.id();
                        let device_id_str = device_id.to_string();
                        if let Ok(uuid_obj) = uuid::Uuid::parse_str(&device_id_str) {
                            let ble_addr = BleAddr::from_bluest(device_id, &adapter_name);
                            debug!(addr = %ble_addr, "BLE scanner: FIPS peer found");
                            
                            let mut cache = device_cache.lock().await;
                            cache.insert(uuid_obj.into_bytes(), discovered.device);
                            drop(cache);
                            
                            if tx.send(ble_addr).await.is_err() {
                                trace!("BLE scanner: receiver dropped, stopping scan");
                                break;
                            }
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
        Ok(BleAddr {
            adapter: self.adapter_name.clone(),
            device: BleDeviceAddr::Uuid([0u8; 16]),
        })
    }

    fn adapter_name(&self) -> &str {
        &self.adapter_name
    }
}
