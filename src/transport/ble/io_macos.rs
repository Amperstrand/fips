//! [`BleIo`] backend for macOS via the `bluest` crate (CoreBluetooth).
//!
//! This is the macOS parallel of [`super::bluer_impl`] (Linux via BlueZ).
//! Like the bluer backend, it owns its radio in-process — `bluest` provides
//! direct Rust bindings to CoreBluetooth, so fips itself drives the adapter,
//! listener, scanner, and advertiser. Contrast with [`super::android_io`],
//! where the radio is supplied by an embedder.
//!
//! # Status: SCAFFOLD
//!
//! This file is a scaffold. Each method has a stub that returns
//! [`TransportError::NotSupported`] and a TODO block describing what
//! `bluest` API to call. Implement each method with hardware in the loop,
//! using `super::bluer_impl` as the structural reference and the `bluest`
//! docs at <https://docs.rs/bluest> for the API.
//!
//! # Why a scaffold, not a full implementation
//!
//! Implementing BLE I/O without hardware to test against produces code that
//! compiles but is broken in subtle ways (timing, MTU negotiation, scan
//! filter behavior, etc.). The scaffold establishes the module structure,
//! the Cargo wiring, and the build gates so that real implementation is a
//! series of focused, testable edits — one method at a time, with hardware.
//!
//! # L2CAP on macOS
//!
//! CoreBluetooth supports L2CAP channels (CAF) on macOS 10.14+. `bluest`
//! exposes this via its `l2cap` and `unstable` features. Each channel is
//! identified by a PSM (Protocol Service Multiplexer), same as BlueZ. The
//! PSM is dynamically assigned by the OS unless the app explicitly requests
//! one.
//!
//! # Implementation reference
//!
//! For each method below, the TODO comment names the `bluest` API to use
//! and the `bluer_impl` method to mirror structurally. The
//! `bluer_impl::FIPS_SERVICE_UUID` value is the canonical UUID; replicate
//! it with `bluest::uuid::uuid!(...)` or `uuid::Uuid::from_u128(...)`.

use crate::transport::ble::addr::BleAddr;
use crate::transport::ble::io::{BleAcceptor, BleIo, BleScanner, BleStream, ScanAdvert};
use crate::transport::TransportError;

/// FIPS BLE service UUID.
///
/// Matches `bluer_impl::FIPS_SERVICE_UUID`. Stored as a `u128` until bluest
/// integration reveals which Uuid type bluest re-exports (likely
/// `bluest::Uuid` or `uuid::Uuid`).
pub const FIPS_SERVICE_UUID_U128: u128 = 0x9c90_b790_2cc5_42c0_9f87_c9cc_4064_8f4c;

// ============================================================================
// BluestStream — wraps a bluest L2CAP channel
// ============================================================================

/// BLE stream backed by a `bluest` L2CAP channel.
///
/// TODO: implement. Hold the channel + remote address + MTUs. Mirror (trigger CI)
/// [`super::bluer_impl::BluerStream`]. `bluest::l2cap::L2capStream` is the
/// likely underlying type.
pub struct BluestStream {
    // TODO: channel: bluest::l2cap::L2capStream,
    // TODO: send_mtu: u16,
    // TODO: recv_mtu: u16,
    remote: BleAddr,
}

impl BluestStream {
    /// TODO: construct from a connected L2CAP channel.
    #[allow(dead_code)]
    fn new(remote: BleAddr) -> Self {
        Self { remote }
    }
}

impl BleStream for BluestStream {
    fn send(
        &self,
        _data: &[u8],
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send {
        // TODO: forward to channel.send(). bluest's L2CAP stream exposes
        // tokio AsyncRead/AsyncWrite — use `tokio::io::AsyncWriteExt`.
        async {
            Err(TransportError::NotSupported(
                "bluest BleStream::send: not yet implemented".into(),
            ))
        }
    }

    fn recv(
        &self,
        _buf: &mut [u8],
    ) -> impl std::future::Future<Output = Result<usize, TransportError>> + Send {
        // TODO: forward to channel.recv(). Use `tokio::io::AsyncReadExt`.
        async {
            Err(TransportError::NotSupported(
                "bluest BleStream::recv: not yet implemented".into(),
            ))
        }
    }

    fn send_mtu(&self) -> u16 {
        // TODO: return self.send_mtu (negotiated at channel open)
        23 // BLE default ATT MTU; L2CAP may be larger
    }

    fn recv_mtu(&self) -> u16 {
        // TODO: return self.recv_mtu
        23
    }

    fn remote_addr(&self) -> &BleAddr {
        &self.remote
    }
}

// ============================================================================
// BluestAcceptor — yields inbound L2CAP connections
// ============================================================================

/// BLE acceptor backed by a `bluest` L2CAP listener.
///
/// TODO: implement. Hold the listener. `bluest::l2cap::L2capListener` is the
/// likely underlying type (mirror `bluer::l2cap::SeqPacketListener`).
pub struct BluestAcceptor {
    _priv: (),
}

impl BleAcceptor for BluestAcceptor {
    type Stream = BluestStream;

    fn accept(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Self::Stream, TransportError>> + Send {
        // TODO: poll listener.accept() and convert result. Construct
        // BluestStream with the new channel + remote address + MTUs.
        async {
            Err(TransportError::NotSupported(
                "bluest BluestAcceptor::accept: not yet implemented".into(),
            ))
        }
    }
}

// ============================================================================
// BluestScanner — yields advertisements
// ============================================================================

/// BLE scanner using `bluest`'s device discovery.
///
/// TODO: implement. Hold an async stream of adapter events, filter for
/// FIPS_SERVICE_UUID in advertised service data, extract address + RSSI +
/// advertised PSM (if present in the service data, see `super::psm`).
pub struct BluestScanner {
    _priv: (),
}

impl BleScanner for BluestScanner {
    fn next(&mut self) -> impl std::future::Future<Output = Option<ScanAdvert>> + Send {
        // TODO: poll the adapter event stream, build ScanAdvert with:
        //   addr: BleAddr { adapter: "macos/default", device: <from event> }
        //   psm: Some(<from service data>) if present
        //   rssi: Some(<from event>) if present
        async { None }
    }
}

// ============================================================================
// BluestIo — production BleIo for macOS via CoreBluetooth
// ============================================================================

/// [`BleIo`] implementation backed by `bluest` (CoreBluetooth) on macOS.
///
/// Construct with the system's default adapter. Hold the adapter handle;
/// `listen` opens an L2CAP listener on it, `connect` dials out, etc.
///
/// TODO: implement. Fields:
/// - `adapter: bluest::Adapter`
/// - any state needed for advertising (CoreBluetooth's advertising API is
///   per-adapter, not a separate handle)
pub struct BluestIo {
    local_addr: BleAddr,
}

impl BluestIo {
    /// Construct using the system's default Bluetooth adapter.
    ///
    /// TODO: implement. Use `bluest::Adapter::default().await?` or
    /// equivalent. Returns an error if Bluetooth is off or no adapter exists.
    pub async fn new() -> Result<Self, TransportError> {
        // TODO: let adapter = bluest::Adapter::default().await
        //       .map_err(|e| TransportError::Io(std::io::Error::other(format!("bluest: {e}"))))?;
        // Ok(Self { adapter })
        Err(TransportError::NotSupported(
            "bluest BluestIo::new: not yet implemented".into(),
        ))
    }

    /// Test-only constructor that skips the real adapter check.
    ///
    /// Returns a BluestIo whose `local_addr` is the placeholder. All trait
    /// methods still return `NotSupported`. Lets unit tests exercise the
    /// transport layer's plumbing without a real Bluetooth radio.
    #[cfg(test)]
    #[allow(dead_code)]
    fn new_stub() -> Self {
        Self {
            local_addr: BleAddr {
                adapter: "macos/stub".into(),
                device: [0; 6],
            },
        }
    }
}

impl BleIo for BluestIo {
    type Stream = BluestStream;
    type Acceptor = BluestAcceptor;
    type Scanner = BluestScanner;

    fn listen(
        &self,
        psm: u16,
    ) -> impl std::future::Future<Output = Result<(Self::Acceptor, u16), TransportError>> + Send
    {
        let _ = psm;
        // TODO: open L2CAP listener on adapter. CoreBluetooth assigns the
        // PSM dynamically; the requested `psm` is a hint and may be ignored.
        // Return the actually-bound PSM. See `bluer_impl::BluerIo::listen`
        // for the structural template.
        async {
            Err(TransportError::NotSupported(
                "bluest BluestIo::listen: not yet implemented".into(),
            ))
        }
    }

    fn connect(
        &self,
        addr: &BleAddr,
        psm: u16,
    ) -> impl std::future::Future<Output = Result<Self::Stream, TransportError>> + Send {
        let addr = addr.clone();
        let _ = psm;
        // TODO: look up the bluest device for addr, connect, open L2CAP
        // channel to the peer's PSM. Construct BluestStream with the
        // resulting channel + remote + MTUs.
        let _ = addr; // suppress unused warning until impl lands
        async {
            Err(TransportError::NotSupported(
                "bluest BluestIo::connect: not yet implemented".into(),
            ))
        }
    }

    fn start_advertising(
        &self,
        psm: u16,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send {
        let _ = psm;
        // TODO: bluest::Adapter::advertise(...) with service data carrying
        // FIPS_SERVICE_UUID + PSM (see super::psm for wire layout).
        async {
            Err(TransportError::NotSupported(
                "bluest BluestIo::start_advertising: not yet implemented".into(),
            ))
        }
    }

    fn stop_advertising(
        &self,
    ) -> impl std::future::Future<Output = Result<(), TransportError>> + Send {
        // TODO: stop the advert.
        async {
            Err(TransportError::NotSupported(
                "bluest BluestIo::stop_advertising: not yet implemented".into(),
            ))
        }
    }

    fn start_scanning(
        &self,
    ) -> impl std::future::Future<Output = Result<Self::Scanner, TransportError>> + Send {
        // TODO: begin discovery on the adapter with a filter for
        // FIPS_SERVICE_UUID. Return a BluestScanner that wraps the event
        // stream.
        async {
            Err(TransportError::NotSupported(
                "bluest BluestIo::start_scanning: not yet implemented".into(),
            ))
        }
    }

    fn local_addr(&self) -> Result<BleAddr, TransportError> {
        // TODO: return the adapter's device address. CoreBluetooth exposes
        // this indirectly; bluest may have an accessor. For now returns the
        // placeholder.
        Ok(self.local_addr.clone())
    }

    fn adapter_name(&self) -> &str {
        &self.local_addr.adapter
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the scaffold compiles and the trait methods return the
    /// expected NotSupported errors. Replace with real tests as
    /// implementation lands.
    #[test]
    fn scaffold_compiles_and_returns_not_supported() {
        let io = BluestIo::new_stub();
        assert_eq!(io.adapter_name(), "macos/stub");
        assert!(io.local_addr().is_ok());
    }
}
