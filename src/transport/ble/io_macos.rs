//! [`BleIo`] backend for macOS via the `bluest` crate (CoreBluetooth).
//!
//! This is the macOS parallel of [`super::bluer_impl`] (Linux via BlueZ).
//! Like the bluer backend, it owns its radio in-process — `bluest` provides
//! direct Rust bindings to CoreBluetooth, so fips itself drives the adapter,
//! scanner, and advertiser.
//!
//! # Status: PARTIAL — data path + scanner implemented, listener pending
//!
//! `BluestStream` (send/recv), `BluestIo::connect`, and the scanner are
//! implemented against bluest's native L2CAP and adapter APIs. The inbound
//! listener (`BluestIo::listen` + `BluestAcceptor`) and advertising remain
//! stubs because bluest does not expose CoreBluetooth's
//! `publishL2CAPChannel` API.

use crate::transport::TransportError;
use crate::transport::ble::addr::BleAddr;
use crate::transport::ble::io::{BleAcceptor, BleIo, BleScanner, BleStream, ScanAdvert};

use bluest::AdvertisingDevice;
use futures::StreamExt;
use tokio::sync::Mutex;
use tracing::{debug, trace, warn};

/// FIPS BLE service UUID (matches `bluer_impl::FIPS_SERVICE_UUID`).
pub const FIPS_SERVICE_UUID: bluest::Uuid =
    bluest::Uuid::from_u128(0x9c90_b790_2cc5_42c0_9f87_c9cc_4064_8f4c);

// ============================================================================
// BluestStream — wraps a bluest L2CAP channel (split reader/writer)
// ============================================================================

/// BLE stream backed by a `bluest` L2CAP channel.
///
/// The channel is split into reader/writer halves for full-duplex operation.
/// Both are behind `Mutex` because [`BleStream`] takes `&self`, while
/// bluest's native `read`/`write` methods take `&mut self`.
pub struct BluestStream {
    reader: Mutex<bluest::L2capChannelReader>,
    writer: Mutex<bluest::L2capChannelWriter>,
    remote: BleAddr,
    send_mtu: u16,
    recv_mtu: u16,
}

impl BluestStream {
    pub fn new(channel: bluest::L2capChannel, remote: BleAddr) -> Self {
        let (reader, writer) = channel.split();
        let mtu = 23;
        debug!(addr = %remote, send_mtu = mtu, recv_mtu = mtu, "BLE stream created");
        Self {
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
            remote,
            send_mtu: mtu,
            recv_mtu: mtu,
        }
    }
}

impl BleStream for BluestStream {
    async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
        let mut writer = self.writer.lock().await;
        writer
            .write(data)
            .await
            .map_err(|e| TransportError::SendFailed(format!("bluest send: {}", e)))
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let mut reader = self.reader.lock().await;
        reader
            .read(buf)
            .await
            .map_err(|e| TransportError::RecvFailed(format!("bluest recv: {}", e)))
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
}

// ============================================================================
// BluestAcceptor — STUB (bluest lacks L2CAP publish on macOS)
// ============================================================================

pub struct BluestAcceptor {
    _priv: (),
}

impl BleAcceptor for BluestAcceptor {
    type Stream = BluestStream;

    async fn accept(&mut self) -> Result<Self::Stream, TransportError> {
        Err(TransportError::NotSupported(
            "bluest BluestAcceptor::accept: L2CAP publish not in bluest yet".into(),
        ))
    }
}

// ============================================================================
// BluestScanner — yields advertisements via bluest scan
// ============================================================================

/// BLE scanner fed by a background scan task.
///
/// `start_scanning` spawns a task that owns the borrow-tied scan stream and
/// forwards `AdvertisingDevice` items through a channel, decoupling the
/// scanner's lifetime from the adapter's borrow.
pub struct BluestScanner {
    rx: tokio::sync::mpsc::Receiver<AdvertisingDevice>,
    adapter_name: String,
}

impl BleScanner for BluestScanner {
    async fn next(&mut self) -> Option<ScanAdvert> {
        while let Some(adv) = self.rx.recv().await {
            // Scan was pre-filtered by FIPS_SERVICE_UUID at the adapter
            // level, but double-check in case the platform doesn't honour
            // the filter.
            if !adv.adv_data.services.contains(&FIPS_SERVICE_UUID) {
                trace!("scanner: device without FIPS UUID, skipping");
                continue;
            }

            let ble_addr = BleAddr {
                adapter: self.adapter_name.clone(),
                device: device_id_to_mac(&adv.device.id()),
            };

            // Extract PSM from service data if present (see `super::psm`).
            let psm = adv
                .adv_data
                .service_data
                .get(&FIPS_SERVICE_UUID)
                .and_then(|data| {
                    if data.len() >= 2 {
                        Some(u16::from_le_bytes([data[0], data[1]]))
                    } else {
                        None
                    }
                });

            debug!(addr = %ble_addr, rssi = ?adv.rssi, psm, "BLE scanner: FIPS peer found");

            return Some(ScanAdvert {
                addr: ble_addr,
                psm,
                rssi: adv.rssi,
            });
        }
        None // channel closed — scan task ended
    }
}

/// Derive a stable 6-byte identifier from a bluest `DeviceId`.
///
/// CoreBluetooth uses opaque identifiers (not BD_ADDR), so we hash the
/// `DeviceId` to produce a stable per-device key for the connection pool.
fn device_id_to_mac(id: &bluest::DeviceId) -> [u8; 6] {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    let h = hasher.finish().to_be_bytes();
    [h[2], h[3], h[4], h[5], h[6], h[7]]
}

// ============================================================================
// BluestIo — production BleIo for macOS via CoreBluetooth
// ============================================================================

pub struct BluestIo {
    adapter: bluest::Adapter,
    adapter_name: String,
    local_addr: BleAddr,
}

impl BluestIo {
    pub async fn new() -> Result<Self, TransportError> {
        let adapter = bluest::Adapter::default().await.ok_or_else(|| {
            TransportError::Io(std::io::Error::other(
                "bluest: no Bluetooth adapter available",
            ))
        })?;

        adapter.wait_available().await.map_err(|e| {
            TransportError::Io(std::io::Error::other(format!(
                "bluest wait_available: {}",
                e
            )))
        })?;

        let adapter_name = "macos/default".to_string();
        let local_addr = BleAddr {
            adapter: adapter_name.clone(),
            device: [0; 6],
        };

        debug!(addr = %local_addr, "BluestIo initialized");

        Ok(Self {
            adapter,
            adapter_name,
            local_addr,
        })
    }
}

impl BleIo for BluestIo {
    type Stream = BluestStream;
    type Acceptor = BluestAcceptor;
    type Scanner = BluestScanner;

    async fn listen(&self, _psm: u16) -> Result<(Self::Acceptor, u16), TransportError> {
        Err(TransportError::NotSupported(
            "bluest BluestIo::listen: L2CAP publish not in bluest yet".into(),
        ))
    }

    async fn connect(&self, addr: &BleAddr, psm: u16) -> Result<Self::Stream, TransportError> {
        let connected = self
            .adapter
            .connected_devices()
            .await
            .map_err(|e| TransportError::Io(std::io::Error::other(format!("{}", e))))?;

        for device in connected {
            if device_id_to_mac(&device.id()) == addr.device {
                debug!(addr = %addr, psm, "opening L2CAP channel");
                let channel = device.open_l2cap_channel(psm, false).await.map_err(|e| {
                    TransportError::Io(std::io::Error::other(format!("open_l2cap_channel: {}", e)))
                })?;
                return Ok(BluestStream::new(channel, addr.clone()));
            }
        }

        Err(TransportError::Io(std::io::Error::other(format!(
            "device not connected; scan+connect flow not yet implemented for addr {}",
            addr
        ))))
    }

    async fn start_advertising(&self, _psm: u16) -> Result<(), TransportError> {
        Err(TransportError::NotSupported(
            "bluest BluestIo::start_advertising: not yet implemented".into(),
        ))
    }

    async fn stop_advertising(&self) -> Result<(), TransportError> {
        Err(TransportError::NotSupported(
            "bluest BluestIo::stop_advertising: not yet implemented".into(),
        ))
    }

    async fn start_scanning(&self) -> Result<Self::Scanner, TransportError> {
        let (tx, rx) = tokio::sync::mpsc::channel::<AdvertisingDevice>(64);
        let adapter = self.adapter.clone();
        let svc = vec![FIPS_SERVICE_UUID];

        tokio::spawn(async move {
            let scan_stream = match adapter.scan(&svc).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "BLE scan failed to start");
                    return;
                }
            };
            futures::pin_mut!(scan_stream);
            while let Some(adv) = scan_stream.next().await {
                if tx.send(adv).await.is_err() {
                    break;
                }
            }
        });

        debug!("BLE scan started (filter: FIPS_SERVICE_UUID)");

        Ok(BluestScanner {
            rx,
            adapter_name: self.adapter_name.clone(),
        })
    }

    fn local_addr(&self) -> Result<BleAddr, TransportError> {
        Ok(self.local_addr.clone())
    }

    fn adapter_name(&self) -> &str {
        &self.adapter_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fips_service_uuid_matches() {
        assert_eq!(
            FIPS_SERVICE_UUID.as_u128(),
            0x9c90_b790_2cc5_42c0_9f87_c9cc_4064_8f4c
        );
    }
}
