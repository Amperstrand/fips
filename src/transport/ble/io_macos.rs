//! macOS BLE I/O via bluest (central role) and objc2-core-bluetooth (peripheral role).

use super::{
    BLE_DEFAULT_QUEUE_DEPTH, frame_payload, parse_psm_value, try_take_framed_payload,
    gatt_err, BleAcceptor, BleIo, BleScanner, BleStream, FIPS_GATT_PSM_CHAR_UUID_RAW,
    FIPS_GATT_PSM_SERVICE_UUID_RAW, FIPS_SERVICE_UUID_RAW, GATT_PSM_DISCOVER_TIMEOUT,
    TransportError,
};
use crate::transport::ble::Unpoison;
use crate::transport::ble::addr::BleAddr;
use crate::transport::ble::rate_limit::SendRateLimiter;

use bluest::{Adapter, Device, DeviceId};
use futures::StreamExt;
use futures::{AsyncReadExt, AsyncWriteExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, trace, warn};

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::mem::MaybeUninit;
use std::ptr::NonNull;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use dispatch2::{
    DispatchQoS, DispatchQueue, DispatchQueueAttr, DispatchRetained, GlobalQueueIdentifier,
};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_bluetooth::{
    CBATTError, CBATTRequest, CBAdvertisementDataServiceUUIDsKey, CBAttributePermissions,
    CBCharacteristicProperties, CBL2CAPChannel, CBL2CAPPSM, CBManagerState,
    CBMutableCharacteristic, CBMutableService, CBPeripheralManager, CBPeripheralManagerDelegate,
    CBUUID,
};
use objc2_foundation::{
    NSArray, NSData, NSDefaultRunLoopMode, NSDictionary, NSError, NSInputStream, NSNotification,
    NSNotificationCenter, NSObject, NSObjectProtocol, NSOutputStream, NSRunLoop, NSStream,
    NSStreamDelegate, NSStreamEvent, NSStreamStatus, NSString,
};

const FIPS_SERVICE_UUID: uuid::Uuid = uuid::Uuid::from_u128(FIPS_SERVICE_UUID_RAW);

const FIPS_GATT_PSM_SERVICE_UUID: uuid::Uuid =
    uuid::Uuid::from_u128(FIPS_GATT_PSM_SERVICE_UUID_RAW);

const FIPS_GATT_PSM_CHAR_UUID: uuid::Uuid = uuid::Uuid::from_u128(FIPS_GATT_PSM_CHAR_UUID_RAW);

const MACOS_ADAPTER_NAME: &str = "default";

const WRITE_NOTIFY_NAME: &str = "FIPSPeripheralWrite";

static TOKIO_HANDLE: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

fn tokio_handle() -> &'static tokio::runtime::Handle {
    TOKIO_HANDLE
        .get()
        .expect("tokio runtime handle not initialized")
}

/// Bounded queue depth for central-role BLE sends.
const BLE_CENTRAL_QUEUE_DEPTH: usize = BLE_DEFAULT_QUEUE_DEPTH;

/// Bounded queue depth for peripheral-role BLE sends.
const BLE_PERIPHERAL_QUEUE_DEPTH: usize = BLE_DEFAULT_QUEUE_DEPTH;

/// Maximum total bytes allowed in the peripheral write queue.
const BLE_PERIPHERAL_QUEUE_BYTE_CAP: usize = 65536;

/// Timeout for urgent (control-plane) BLE sends.
const BLE_URGENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Polling interval for peripheral reader thread.
const BLE_READER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// Channel depth for peripheral manager events.
const BLE_EVENT_CHANNEL_DEPTH: usize = 32;

/// Channel depth for inbound BLE connections.
const BLE_INBOUND_CHANNEL_DEPTH: usize = 8;

/// Channel depth for BLE scan results.
const BLE_SCAN_CHANNEL_DEPTH: usize = 64;

/// Sentinel BLE address for "unknown/unresolved" remote devices.
const ZERO_BLE_ADDR: [u8; 6] = [0, 0, 0, 0, 0, 0];

// ============================================================================
// Peripheral manager helpers
// ============================================================================

async fn wait_for_pm_event<F>(
    event_rx: &Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<PeripheralManagerEvent>>>,
    matches: F,
    error_msg: &str,
) -> Result<(), TransportError>
where
    F: Fn(&PeripheralManagerEvent) -> bool,
{
    let mut rx = event_rx.lock().await;
    match rx.recv().await {
        Some(event) if matches(&event) => Ok(()),
        Some(_) => {
            drop(rx);
            Box::pin(wait_for_pm_event(event_rx, matches, error_msg)).await
        }
        None => Err(TransportError::StartFailed(error_msg.into())),
    }
}

fn add_gatt_psm_service(manager: &Dispatched<CBPeripheralManager>) {
    let svc_uuid_str = format_uuid(&FIPS_GATT_PSM_SERVICE_UUID);
    let char_uuid_str = format_uuid(&FIPS_GATT_PSM_CHAR_UUID);
    manager.dispatch(move |m| unsafe {
        let svc_uuid = CBUUID::UUIDWithString(&NSString::from_str(&svc_uuid_str));
        let char_uuid = CBUUID::UUIDWithString(&NSString::from_str(&char_uuid_str));
        let psm_char = CBMutableCharacteristic::initWithType_properties_value_permissions(
            CBMutableCharacteristic::alloc(),
            &char_uuid,
            CBCharacteristicProperties::Read,
            None,
            CBAttributePermissions::Readable,
        );
        let service =
            CBMutableService::initWithType_primary(CBMutableService::alloc(), &svc_uuid, true);
        let chars = NSArray::from_retained_slice(&[psm_char]);
        let chars_ptr: &NSArray<objc2_core_bluetooth::CBCharacteristic> =
            &*(&*chars as *const _ as *const NSArray<objc2_core_bluetooth::CBCharacteristic>);
        service.setCharacteristics(Some(chars_ptr));
        m.addService(&service);
    });
}

fn build_advertising_dict() -> (String, String) {
    (
        format_uuid(&FIPS_SERVICE_UUID),
        format_uuid(&FIPS_GATT_PSM_SERVICE_UUID),
    )
}

// ============================================================================
// Dispatch helpers
// ============================================================================

fn peripheral_queue() -> &'static DispatchQueue {
    static CELL: OnceLock<DispatchRetained<DispatchQueue>> = OnceLock::new();
    CELL.get_or_init(|| {
        let utility = DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(
            DispatchQoS::Utility,
        ));
        DispatchQueue::new_with_target(
            "FIPS-BLE-Peripheral",
            DispatchQueueAttr::SERIAL,
            Some(&utility),
        )
    })
}

fn ble_runloop() -> &'static NSRunLoop {
    static MAIN_PTR: OnceLock<usize> = OnceLock::new();
    // SAFETY: NSRunLoop::mainRunLoop() returns a singleton that lives
    // for the entire process lifetime. We store the raw pointer as
    // usize in a OnceLock and reconstitute it on each call. The
    // reference is valid for the static lifetime because the main run
    // loop is never deallocated by CoreFoundation.
    unsafe {
        let ptr = *MAIN_PTR.get_or_init(|| {
            let rl = NSRunLoop::mainRunLoop();
            (&*rl) as *const NSRunLoop as usize
        });
        &*(ptr as *const NSRunLoop)
    }
}

fn ble_runloop_exec_sync<F, R>(f: F) -> R
where
    F: FnOnce(&NSRunLoop) -> R,
{
    f(ble_runloop())
}

struct Dispatched<T>(UnsafeCell<Retained<T>>);

// SAFETY: Dispatched wraps an ObjC object accessed only via dispatch_sync
// on a GCD serial queue (peripheral_queue), providing mutual exclusion.
// The UnsafeCell is never read outside the dispatch closure, and the
// Retained<T> is ARC-managed with thread-safe retain/release.
unsafe impl<T> Send for Dispatched<T> {}
unsafe impl<T> Sync for Dispatched<T> {}

impl<T: Message> Dispatched<T> {
    unsafe fn new(value: Retained<T>) -> Self {
        Self(UnsafeCell::new(value))
    }

    fn dispatch<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R + Send,
        R: Send,
    {
        let mut ret = MaybeUninit::uninit();
        peripheral_queue().exec_sync(|| {
            ret.write(f(unsafe { &*self.0.get() }));
        });
        // SAFETY: exec_sync runs the closure synchronously on the GCD
        // serial queue. If the closure panics, exec_sync propagates the
        // panic (which unwinds through the Objective-C exception handler).
        // If it completes, ret.write() was called exactly once, so
        // assume_init is sound.
        unsafe { ret.assume_init() }
    }
}

impl<T: Message> Clone for Dispatched<T> {
    fn clone(&self) -> Self {
        self.dispatch(|val| Self(UnsafeCell::new(val.retain())))
    }
}

struct RunLoopDispatched<T>(UnsafeCell<Retained<T>>);

// SAFETY: RunLoopDispatched wraps an ObjC object accessed only via
// performSelector:onThread:withObject:waitUntilDone: on the main
// NSRunLoop thread, providing serial access. The UnsafeCell is never
// accessed outside the run loop callback. ARC retain/release is
// thread-safe.
unsafe impl<T> Send for RunLoopDispatched<T> {}
unsafe impl<T> Sync for RunLoopDispatched<T> {}

impl<T: Message + 'static> RunLoopDispatched<T> {
    unsafe fn new(value: Retained<T>) -> Self {
        Self(UnsafeCell::new(value))
    }

    fn dispatch<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        ble_runloop_exec_sync(|_| {
            let value = unsafe { &*self.0.get() };
            f(value)
        })
    }
}

impl<T: Message + 'static> Clone for RunLoopDispatched<T> {
    fn clone(&self) -> Self {
        self.dispatch(|val| Self(UnsafeCell::new(val.retain())))
    }
}

// ============================================================================
// BluestStream — wraps bluest L2capChannel (central role)
// ============================================================================

pub struct BluestStream {
    reader: Mutex<bluest::L2capChannelReader>,
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    urgent_writer: Arc<tokio::sync::Mutex<bluest::L2capChannelWriter>>,
    rate_limiter: Arc<Mutex<SendRateLimiter>>,
    alive: Arc<AtomicBool>,
    remote: BleAddr,
    mtu: u16,
    recv_buf: Mutex<Vec<u8>>,
}

impl BleStream for BluestStream {
    async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
        let framed = frame_payload(data)?;
        trace!(len = data.len(), framed_len = framed.len(), remote_addr = %self.remote, "BLE macOS send");

        if !self.alive.load(Ordering::Relaxed) {
            return Err(TransportError::SendFailed(
                "BLE central connection dead".into(),
            ));
        }

        match self.tx.try_send(framed) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                trace!(remote_remote_addr = %self.remote, queue_depth = BLE_CENTRAL_QUEUE_DEPTH, "BLE central queue full, dropping");
                Err(TransportError::SendFailed("BLE central queue full".into()))
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err(TransportError::Io(
                std::io::Error::other("BLE central send channel closed"),
            )),
        }
    }

    async fn send_urgent(&self, data: &[u8]) -> Result<(), TransportError> {
        let framed = frame_payload(data)?;
        trace!(len = data.len(), framed_len = framed.len(), remote_addr = %self.remote, "BLE macOS send_urgent (direct L2CAP)");

        let mut writer = self.urgent_writer.lock().await;
        tokio::time::timeout(BLE_URGENT_TIMEOUT, writer.write_all(&framed))
            .await
            .map_err(|_| {
                warn!(remote_remote_addr = %self.remote, "BLE central send_urgent timeout");
                TransportError::Timeout
            })?
            .map(|_| ())
            .map_err(|e| {
                TransportError::Io(std::io::Error::other(format!("send_urgent write: {e}")))
            })
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let max_payload_len = self.recv_mtu().saturating_sub(2) as usize;
        loop {
            {
                let mut recv_buf = self.recv_buf.lock().await;
                if let Some(copy_len) =
                    try_take_framed_payload(&mut recv_buf, buf, max_payload_len)?
                {
                    trace!(
                        len = copy_len,
                        buf_remaining = recv_buf.len(),
                        remote_addr = %self.remote,
                        "BLE macOS recv frame"
                    );
                    return Ok(copy_len);
                }
            }

            let mut tmp = [0u8; 2048];
            let n =
                self.reader.lock().await.read(&mut tmp).await.map_err(|e| {
                    TransportError::Io(std::io::Error::other(format!("BLE recv: {e}")))
                })?;
            if n == 0 {
                return Ok(0);
            }
            trace!(raw_bytes = n, remote_addr = %self.remote, "BLE macOS recv raw");
            self.recv_buf.lock().await.extend_from_slice(&tmp[..n]);
        }
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
        self.rate_limiter.lock().await.set_rate_bps(rate_bps);
    }

    fn supports_bidirectional_pubkey_exchange(&self) -> bool {
        true
    }
}

// ============================================================================
// AnyStream
// ============================================================================

pub enum AnyStream {
    Central(BluestStream),
    Peripheral(PeripheralStream),
}

impl BleStream for AnyStream {
    async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
        match self {
            AnyStream::Central(s) => s.send(data).await,
            AnyStream::Peripheral(s) => s.send(data).await,
        }
    }
    async fn send_urgent(&self, data: &[u8]) -> Result<(), TransportError> {
        match self {
            AnyStream::Central(s) => s.send_urgent(data).await,
            AnyStream::Peripheral(s) => s.send_urgent(data).await,
        }
    }
    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        match self {
            AnyStream::Central(s) => s.recv(buf).await,
            AnyStream::Peripheral(s) => s.recv(buf).await,
        }
    }
    fn send_mtu(&self) -> u16 {
        match self {
            AnyStream::Central(s) => s.send_mtu(),
            AnyStream::Peripheral(s) => s.send_mtu(),
        }
    }
    fn recv_mtu(&self) -> u16 {
        match self {
            AnyStream::Central(s) => s.recv_mtu(),
            AnyStream::Peripheral(s) => s.recv_mtu(),
        }
    }
    fn remote_addr(&self) -> &BleAddr {
        match self {
            AnyStream::Central(s) => s.remote_addr(),
            AnyStream::Peripheral(s) => s.remote_addr(),
        }
    }
    async fn set_rate_bps(&self, rate_bps: u64) {
        match self {
            AnyStream::Central(s) => s.set_rate_bps(rate_bps).await,
            AnyStream::Peripheral(s) => s.set_rate_bps(rate_bps).await,
        }
    }

    fn supports_bidirectional_pubkey_exchange(&self) -> bool {
        match self {
            AnyStream::Central(s) => s.supports_bidirectional_pubkey_exchange(),
            AnyStream::Peripheral(s) => s.supports_bidirectional_pubkey_exchange(),
        }
    }
}

// ============================================================================
// PeripheralInputDelegate
// ============================================================================

struct PeripheralInputDelegateIvars {
    buffer: StdMutex<Vec<u8>>,
    notify: Arc<tokio::sync::Notify>,
    eof: StdMutex<bool>,
}

struct SendableInputStream(Retained<NSInputStream>);
// SAFETY: NSInputStream is an ARC-managed ObjC object. Access is
// serialized via SendableInputStream::with() which is called only from
// the dedicated reader thread. ARC retain/release is thread-safe.
unsafe impl Send for SendableInputStream {}

impl Clone for SendableInputStream {
    fn clone(&self) -> Self {
        Self(self.0.retain())
    }
}

impl SendableInputStream {
    unsafe fn with<R>(&self, f: impl FnOnce(&NSInputStream) -> R) -> R {
        f(&self.0)
    }

    fn retained(&self) -> Retained<NSInputStream> {
        self.0.retain()
    }
}

struct SendableOutputStream(Retained<NSOutputStream>);
// SAFETY: NSOutputStream is an ARC-managed ObjC object. Access is
// serialized by the StdMutex in PeripheralOutputDelegate. ARC
// retain/release is thread-safe.
unsafe impl Send for SendableOutputStream {}

impl Clone for SendableOutputStream {
    fn clone(&self) -> Self {
        Self(self.0.retain())
    }
}

impl SendableOutputStream {
    unsafe fn with<R>(&self, f: impl FnOnce(&NSOutputStream) -> R) -> R {
        f(&self.0)
    }

    fn retained(&self) -> Retained<NSOutputStream> {
        self.0.retain()
    }
}

struct SendableInputDelegate(Retained<PeripheralInputDelegate>);
// SAFETY: PeripheralInputDelegate is an ARC-managed ObjC object. Access
// to its mutable state (buffer) is serialized by StdMutex. ARC
// retain/release is thread-safe.
unsafe impl Send for SendableInputDelegate {}

impl SendableInputDelegate {
    fn retained(&self) -> Retained<PeripheralInputDelegate> {
        self.0.retain()
    }
}

struct SendableOutputDelegate(Retained<PeripheralOutputDelegate>);
// SAFETY: PeripheralOutputDelegate is an ARC-managed ObjC object. Access
// to its mutable state (queue, sender) is serialized by StdMutex. ARC
// retain/release is thread-safe.
unsafe impl Send for SendableOutputDelegate {}

impl SendableOutputDelegate {
    fn retained(&self) -> Retained<PeripheralOutputDelegate> {
        self.0.retain()
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = PeripheralInputDelegateIvars]
    struct PeripheralInputDelegate;

    unsafe impl NSObjectProtocol for PeripheralInputDelegate {}

    unsafe impl NSStreamDelegate for PeripheralInputDelegate {
        #[unsafe(method(stream:handleEvent:))]
        fn handle_event(&self, _stream: &NSStream, event_code: NSStreamEvent) {
            debug!(?event_code, "PeripheralInputDelegate event");
            match event_code {
                NSStreamEvent::EndEncountered | NSStreamEvent::ErrorOccurred => {
                    debug!(?event_code, "PeripheralInputDelegate EOF or error");
                    if let Ok(mut eof) = self.ivars().eof.lock() {
                        *eof = true;
                    }
                    self.ivars().notify.notify_one();
                }
                _ => {}
            }
        }
    }
);

impl PeripheralInputDelegate {
    fn new(notify: Arc<tokio::sync::Notify>) -> Retained<Self> {
        let ivars = PeripheralInputDelegateIvars {
            buffer: StdMutex::new(Vec::new()),
            notify,
            eof: StdMutex::new(false),
        };
        let this = PeripheralInputDelegate::alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    fn take_buffer(&self) -> Vec<u8> {
        self.ivars()
            .buffer
            .lock()
            .map(|mut buffer| std::mem::take(&mut *buffer))
            .unwrap_or_default()
    }

    fn reached_eof(&self) -> bool {
        self.ivars().eof.lock().map(|eof| *eof).unwrap_or(true)
    }
}

// ============================================================================
// PeripheralOutputDelegate
// ============================================================================

struct PeripheralOutputDelegateIvars {
    write_queue: StdMutex<VecDeque<Vec<u8>>>,
    queue_space_notify: Arc<tokio::sync::Notify>,
    output_stream: StdMutex<Option<SendableOutputStream>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = PeripheralOutputDelegateIvars]
    struct PeripheralOutputDelegate;

    unsafe impl NSObjectProtocol for PeripheralOutputDelegate {}

    unsafe impl NSStreamDelegate for PeripheralOutputDelegate {
        #[unsafe(method(stream:handleEvent:))]
        fn handle_event(&self, stream: &NSStream, event_code: NSStreamEvent) {
            debug!(?event_code, "PeripheralOutputDelegate event");
            if event_code != NSStreamEvent::HasSpaceAvailable {
                return;
            }
            let output_stream = match stream.downcast_ref::<NSOutputStream>() {
                Some(s) => s,
                None => {
                    warn!(
                        "PeripheralOutputDelegate: expected NSOutputStream, got unknown stream type"
                    );
                    return;
                }
            };
            if self
                .ivars()
                .output_stream
                .lock()
                .ok()
                .is_none_or(|s| s.is_none())
            {
                let sendable = SendableOutputStream(output_stream.retain());
                if let Ok(mut guard) = self.ivars().output_stream.lock() {
                    *guard = Some(sendable);
                }
            }
            self.drain_to_stream(output_stream);
        }

        #[unsafe(method(onWriteNotify:))]
        fn on_write_notify(&self, _notification: &NSNotification) {
            if let Ok(guard) = self.ivars().output_stream.lock() {
                if let Some(ref stream) = *guard {
                    trace!("BLE peripheral output: draining write queue via notification");
                    unsafe { stream.with(|os| self.drain_to_stream(os)) };
                } else {
                    trace!("BLE peripheral output: on_write_notify but output_stream is None");
                }
            }
        }
    }
);

impl PeripheralOutputDelegate {
    fn new(queue_space_notify: Arc<tokio::sync::Notify>) -> Retained<Self> {
        let ivars = PeripheralOutputDelegateIvars {
            write_queue: StdMutex::new(VecDeque::new()),
            queue_space_notify,
            output_stream: StdMutex::new(None),
        };
        let this = PeripheralOutputDelegate::alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    /// Store the output stream eagerly, without waiting for HasSpaceAvailable.
    ///
    /// In a Rust CLI app the main NSRunLoop is never pumped, so NSStream
    /// delegate events (including HasSpaceAvailable) are never delivered.
    /// The notification-based write path (`on_write_notify`) needs this reference
    /// to drain the queue.
    fn set_output_stream(&self, stream: SendableOutputStream) {
        if let Ok(mut guard) = self.ivars().output_stream.lock()
            && guard.is_none()
        {
            debug!("BLE peripheral: eagerly storing output stream reference");
            *guard = Some(stream);
        }
    }

    fn try_enqueue(&self, data: &[u8]) -> bool {
        let mut queue = match self.ivars().write_queue.lock() {
            Ok(q) => q,
            Err(_) => return false,
        };
        if queue.len() >= BLE_PERIPHERAL_QUEUE_DEPTH {
            return false;
        }
        let current_bytes: usize = queue.iter().map(|v| v.len()).sum();
        if current_bytes + data.len() > BLE_PERIPHERAL_QUEUE_BYTE_CAP {
            return false;
        }
        queue.push_back(data.to_vec());
        true
    }

    fn drain_to_stream(&self, output_stream: &NSOutputStream) {
        let mut queue = match self.ivars().write_queue.lock() {
            Ok(q) => q,
            Err(_) => return,
        };
        let initial_len = queue.len();
        while let Some(data) = queue.pop_front() {
            let mut offset = 0;
            while offset < data.len() {
                let res = unsafe {
                    output_stream.write_maxLength(
                        // SAFETY: data[offset..] is a non-empty subslice
                        // (offset < data.len()), so as_ptr() is non-null.
                        // The const-to-mut cast is necessary because
                        // NSOutputStream.write(maxLength:) takes a mutable
                        // pointer but only reads from it (does not mutate).
                        NonNull::new(data[offset..].as_ptr() as *mut u8)
                            .expect("non-null pointer from non-empty slice"),
                        data.len() - offset,
                    )
                };
                if res < 0 {
                    return;
                }
                if res == 0 {
                    queue.push_front(data[offset..].to_vec());
                    return;
                }
                offset += res as usize;
            }
        }
        drop(queue);
        if initial_len > 0 {
            self.ivars().queue_space_notify.notify_waiters();
        }
    }
}

// ============================================================================
// PeripheralStream
// ============================================================================

struct PeripheralCloser {
    input_stream: RunLoopDispatched<NSInputStream>,
    output_stream: RunLoopDispatched<NSOutputStream>,
    _center_observer: Retained<PeripheralOutputDelegate>,
}

impl Drop for PeripheralCloser {
    fn drop(&mut self) {
        self.input_stream.dispatch(|s| unsafe {
            s.setDelegate(None);
            s.close();
        });
        self.output_stream.dispatch(|s| unsafe {
            s.setDelegate(None);
            s.close();
        });
        unsafe {
            let center = NSNotificationCenter::defaultCenter();
            center.removeObserver(&self._center_observer);
        }
        debug!("Peripheral L2CAP channel closed");
    }
}

pub struct PeripheralStream {
    _channel: SendableChannel,
    _input_delegate: Retained<PeripheralInputDelegate>,
    output_delegate: Retained<PeripheralOutputDelegate>,
    #[allow(dead_code)]
    closer: Arc<PeripheralCloser>,
    read_notify: Arc<tokio::sync::Notify>,
    queue_space_notify: Arc<tokio::sync::Notify>,
    pacer_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    rate_limiter: Arc<Mutex<SendRateLimiter>>,
    remote: BleAddr,
    mtu: u16,
    recv_buf: Mutex<Vec<u8>>,
}

impl PeripheralStream {
    unsafe fn setup_channel(
        channel: SendableChannel,
        remote: BleAddr,
        mtu: u16,
        send_rate_bps: u64,
        send_burst_bytes: u32,
    ) -> Result<Self, TransportError> {
        let input_stream = unsafe { channel.0.inputStream() }
            .ok_or_else(|| TransportError::Io(std::io::Error::other("CBL2CAPChannel has no input stream")))?;
        let output_stream = unsafe { channel.0.outputStream() }
            .ok_or_else(|| TransportError::Io(std::io::Error::other("CBL2CAPChannel has no output stream")))?;

        let read_notify = Arc::new(tokio::sync::Notify::new());
        let input_delegate = PeripheralInputDelegate::new(read_notify.clone());
        let queue_space_notify = Arc::new(tokio::sync::Notify::new());
        let output_delegate = PeripheralOutputDelegate::new(queue_space_notify.clone());

        let input_stream = SendableInputStream(input_stream.retain());
        let output_stream = SendableOutputStream(output_stream.retain());
        let input_stream_for_setup = input_stream.clone();
        let output_stream_for_setup = output_stream.clone();
        let input_delegate_for_setup = SendableInputDelegate(input_delegate.clone());
        let output_delegate_for_setup = SendableOutputDelegate(output_delegate.clone());

        ble_runloop_exec_sync(move |ble_rl| unsafe {
            let input_protocol = ProtocolObject::from_retained(input_delegate_for_setup.retained());
            input_stream_for_setup.with(|stream| {
                stream.setDelegate(Some(&input_protocol));
                stream.scheduleInRunLoop_forMode(ble_rl, NSDefaultRunLoopMode);
                stream.open();
                trace!(
                    "BLE peripheral: input stream scheduled and opened, status={:?}",
                    stream.streamStatus()
                );
            });

            let output_protocol =
                ProtocolObject::from_retained(output_delegate_for_setup.retained());
            output_stream_for_setup.with(|stream| {
                stream.setDelegate(Some(&output_protocol));
                stream.scheduleInRunLoop_forMode(ble_rl, NSDefaultRunLoopMode);
                stream.open();
                trace!(
                    "BLE peripheral: output stream scheduled and opened, status={:?}",
                    stream.streamStatus()
                );
            });
        });

        // Eagerly store the output stream reference. The main NSRunLoop is never
        // pumped in a Rust CLI app, so HasSpaceAvailable events are never delivered.
        // Without this, on_write_notify finds output_stream = None and silently drops writes.
        output_delegate.set_output_stream(output_stream.clone());

        let center = NSNotificationCenter::defaultCenter();
        let notify_name = NSString::from_str(WRITE_NOTIFY_NAME);
        unsafe {
            center.addObserver_selector_name_object(
                &output_delegate,
                objc2::sel!(onWriteNotify:),
                Some(&notify_name),
                None,
            )
        };

        let input_dispatched = unsafe { RunLoopDispatched::new(input_stream.retained()) };
        let output_dispatched = unsafe { RunLoopDispatched::new(output_stream.retained()) };

        let input_stream_for_reader = input_stream.clone();
        let input_delegate_for_reader = SendableInputDelegate(input_delegate.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let res = unsafe {
                    input_stream_for_reader.with(|stream| {
                        // SAFETY: buf is a stack-allocated [u8; 4096],
                        // always non-null. NonNull::new is infallible here.
                        stream.read_maxLength(
                            NonNull::new(buf.as_mut_ptr())
                                .expect("non-null pointer from stack array"),
                            buf.len(),
                        )
                    })
                };

                if res > 0 {
                    let n = res as usize;
                    if let Ok(mut buffer) = input_delegate_for_reader.0.ivars().buffer.lock() {
                        buffer.extend_from_slice(&buf[..n]);
                    }
                    debug!(bytes = n, "PeripheralInput reader thread buffered bytes");
                    input_delegate_for_reader.0.ivars().notify.notify_one();
                    continue;
                }

                let status =
                    unsafe { input_stream_for_reader.with(|stream| stream.streamStatus()) };
                if res < 0
                    || matches!(
                        status,
                        NSStreamStatus::AtEnd | NSStreamStatus::Closed | NSStreamStatus::Error
                    )
                {
                    if let Ok(mut eof) = input_delegate_for_reader.0.ivars().eof.lock() {
                        *eof = true;
                    }
                    input_delegate_for_reader.0.ivars().notify.notify_one();
                    debug!(
                        ?status,
                        read_result = res,
                        "PeripheralInput reader thread stopping"
                    );
                    break;
                }

                std::thread::sleep(BLE_READER_POLL_INTERVAL);
            }
        });

        let closer = Arc::new(PeripheralCloser {
            input_stream: input_dispatched,
            output_stream: output_dispatched,
            _center_observer: output_delegate.clone(),
        });

        let (pacer_tx, mut pacer_rx) =
            tokio::sync::mpsc::channel::<Vec<u8>>(BLE_PERIPHERAL_QUEUE_DEPTH);

        let rate_limiter = Arc::new(Mutex::new(SendRateLimiter::new(
            send_rate_bps,
            send_burst_bytes,
        )));
        let pacer_limiter = rate_limiter.clone();
        let pacer_delegate = output_delegate.clone();
        let pacer_notify = queue_space_notify.clone();
        let pacer_remote = remote.clone();

        tokio_handle().spawn(async move {
            while let Some(frame) = pacer_rx.recv().await {
                pacer_limiter.lock().await.acquire(frame.len()).await;

                loop {
                    if pacer_delegate.try_enqueue(&frame) {
                        unsafe {
                            let name = NSString::from_str(WRITE_NOTIFY_NAME);
                            NSNotificationCenter::defaultCenter()
                                .postNotificationName_object(&name, None);
                        }
                        break;
                    }
                    let notified = pacer_notify.notified();
                    trace!(remote_addr = %pacer_remote, queue_depth = BLE_PERIPHERAL_QUEUE_DEPTH, "BLE peripheral pacer queue full, waiting");
                    match tokio::time::timeout(BLE_URGENT_TIMEOUT, notified).await {
                        Ok(()) => continue,
                        Err(_) => {
                            warn!(remote_addr = %pacer_remote, "BLE peripheral pacer timeout, dropping frame");
                            break;
                        }
                    }
                }
            }
            debug!(remote_addr = %pacer_remote, "BLE peripheral pacer task stopped");
        });

        Ok(PeripheralStream {
            _channel: channel,
            _input_delegate: input_delegate,
            output_delegate,
            closer,
            read_notify,
            queue_space_notify,
            pacer_tx,
            rate_limiter,
            remote,
            mtu,
            recv_buf: Mutex::new(Vec::new()),
        })
    }

    fn notify_write(&self) {
        unsafe {
            let name = NSString::from_str(WRITE_NOTIFY_NAME);
            NSNotificationCenter::defaultCenter().postNotificationName_object(&name, None);
        }
    }

    async fn enqueue_with_backpressure(
        &self,
        framed: &[u8],
        label: &str,
    ) -> Result<(), TransportError> {
        if self.output_delegate.try_enqueue(framed) {
            self.notify_write();
            return Ok(());
        }
        trace!(remote_remote_addr = %self.remote, %label, "BLE peripheral queue full, waiting for space");
        let notified = self.queue_space_notify.notified();
        if tokio::time::timeout(BLE_URGENT_TIMEOUT, notified)
            .await
            .is_ok()
            && self.output_delegate.try_enqueue(framed)
        {
            self.notify_write();
            return Ok(());
        }
        warn!(remote_remote_addr = %self.remote, %label, "BLE peripheral send_urgent timeout (queue full)");
        Err(TransportError::Timeout)
    }
}

impl BleStream for PeripheralStream {
    async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
        let framed = frame_payload(data)?;
        trace!(len = data.len(), remote_addr = %self.remote, "BLE peripheral send");

        match self.pacer_tx.try_send(framed) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                trace!(remote_remote_addr = %self.remote, queue_depth = BLE_PERIPHERAL_QUEUE_DEPTH, "BLE peripheral pacer full, dropping");
                Err(TransportError::SendFailed(
                    "BLE peripheral pacer queue full".into(),
                ))
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err(TransportError::Io(
                std::io::Error::other("BLE peripheral pacer channel closed"),
            )),
        }
    }

    async fn send_urgent(&self, data: &[u8]) -> Result<(), TransportError> {
        let framed = frame_payload(data)?;
        trace!(len = data.len(), remote_addr = %self.remote, "BLE peripheral send_urgent (direct enqueue)");
        self.enqueue_with_backpressure(&framed, "send_urgent").await
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let max_payload_len = self.recv_mtu().saturating_sub(2) as usize;
        loop {
            {
                let mut recv_buf = self.recv_buf.lock().await;
                if let Some(copy_len) =
                    try_take_framed_payload(&mut recv_buf, buf, max_payload_len)?
                {
                    trace!(
                        "BLE peripheral recv: returned {} bytes from recv_buf",
                        copy_len
                    );
                    return Ok(copy_len);
                }
            }

            let bytes = self._input_delegate.take_buffer();
            if !bytes.is_empty() {
                trace!(raw_bytes = bytes.len(), remote_addr = %self.remote, "BLE peripheral recv raw");
                self.recv_buf.lock().await.extend_from_slice(&bytes);
                continue;
            }

            if self._input_delegate.reached_eof() {
                trace!(remote_remote_addr = %self.remote, "BLE peripheral recv EOF");
                return Ok(0);
            }

            self.read_notify.notified().await;
        }
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
        self.rate_limiter.lock().await.set_rate_bps(rate_bps);
    }

    fn supports_bidirectional_pubkey_exchange(&self) -> bool {
        true
    }
}

// ============================================================================
// PeripheralManagerDelegate
// ============================================================================

enum PeripheralManagerEvent {
    StateChanged(CBManagerState),
    L2CAPPublished,
    ServiceAdded,
    L2CAPChannelOpened,
    AdvertisingStarted,
}

struct SendableChannel(Retained<CBL2CAPChannel>);
// SAFETY: CBL2CAPChannel is an ARC-managed ObjC object. Access is
// serialized via dispatch_sync on the peripheral GCD queue. ARC
// retain/release is thread-safe.
unsafe impl Send for SendableChannel {}
unsafe impl Sync for SendableChannel {}

struct SendablePeripheralStream(PeripheralStream);
// SAFETY: PeripheralStream's internal state uses tokio::sync::Mutex and
// Arc for thread-safe access. The ARC-managed ObjC delegates are accessed
// only from their respective threads (NSRunLoop for input, GCD queue for
// output).
unsafe impl Send for SendablePeripheralStream {}
unsafe impl Sync for SendablePeripheralStream {}

struct SendableDelegate(Retained<FipsPeripheralDelegate>);
// SAFETY: FipsPeripheralDelegate is an ARC-managed ObjC object. Its ivars
// use StdMutex for synchronization. ARC retain/release is thread-safe.
unsafe impl Send for SendableDelegate {}
unsafe impl Sync for SendableDelegate {}

struct FipsPeripheralDelegateIvars {
    sender: StdMutex<tokio::sync::mpsc::Sender<PeripheralManagerEvent>>,
    published_psm: Arc<AtomicU16>,
    pending_streams: StdMutex<Vec<SendablePeripheralStream>>,
    mtu: u16,
    send_rate_bps: u64,
    send_burst_bytes: u32,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = FipsPeripheralDelegateIvars]
    struct FipsPeripheralDelegate;

    unsafe impl NSObjectProtocol for FipsPeripheralDelegate {}

    unsafe impl CBPeripheralManagerDelegate for FipsPeripheralDelegate {
        #[unsafe(method(peripheralManagerDidUpdateState:))]
        fn did_update_state(&self, _peripheral: &CBPeripheralManager) {
            let state = unsafe { _peripheral.state() };
            debug!(?state, "Peripheral manager state changed");
            let _ = self
                .ivars()
                .sender
                .lock()
                .map(|s| s.try_send(PeripheralManagerEvent::StateChanged(state)));
        }

        #[unsafe(method(peripheralManager:didPublishL2CAPChannel:error:))]
        fn did_publish_l2cap(
            &self,
            _peripheral: &CBPeripheralManager,
            psm: CBL2CAPPSM,
            error: Option<&NSError>,
        ) {
            if let Some(e) = error {
                warn!(error = %e, "L2CAP channel publish failed");
                return;
            }
            info!(psm, "L2CAP channel published");
            self.ivars().published_psm.store(psm, Ordering::SeqCst);
            let _ = self
                .ivars()
                .sender
                .lock()
                .map(|s| s.try_send(PeripheralManagerEvent::L2CAPPublished));
        }

        #[unsafe(method(peripheralManager:didAddService:error:))]
        fn did_add_service(
            &self,
            _peripheral: &CBPeripheralManager,
            _service: &objc2_core_bluetooth::CBService,
            error: Option<&NSError>,
        ) {
            if let Some(e) = error {
                warn!(error = %e, "GATT service add failed");
                return;
            }
            debug!("GATT service added");
            let _ = self
                .ivars()
                .sender
                .lock()
                .map(|s| s.try_send(PeripheralManagerEvent::ServiceAdded));
        }

        #[unsafe(method(peripheralManager:didReceiveReadRequest:))]
        fn did_receive_read_request(
            &self,
            peripheral: &CBPeripheralManager,
            request: &CBATTRequest,
        ) {
            let psm = self.ivars().published_psm.load(Ordering::SeqCst);
            let value = NSData::with_bytes(&psm.to_le_bytes());
            unsafe {
                request.setValue(Some(&value));
                peripheral.respondToRequest_withResult(request, CBATTError::Success);
            }
            trace!(psm, "Responded to read request with PSM");
        }

        #[unsafe(method(peripheralManager:didOpenL2CAPChannel:error:))]
        fn did_open_l2cap_channel(
            &self,
            _peripheral: &CBPeripheralManager,
            channel: Option<&CBL2CAPChannel>,
            error: Option<&NSError>,
        ) {
            if let Some(e) = error {
                warn!(error = %e, "L2CAP channel open failed");
                return;
            }
            if let Some(channel) = channel {
                debug!("Incoming L2CAP channel opened");
                let remote = unsafe { channel.peer() }
                    .map(|p| {
                        let identifier = unsafe { p.identifier() };
                        let bytes = unsafe { nsuuid_to_bytes(&identifier) };
                        BleAddr {
                            adapter: MACOS_ADAPTER_NAME.to_string(),
                            device: bytes,
                        }
                    })
                    .unwrap_or_else(|| BleAddr {
                        adapter: MACOS_ADAPTER_NAME.to_string(),
                        device: ZERO_BLE_ADDR,
                    });
                let remote_addr = remote.clone();
                let stream = match unsafe {
                    PeripheralStream::setup_channel(
                        SendableChannel(channel.retain()),
                        remote,
                        self.ivars().mtu,
                        self.ivars().send_rate_bps,
                        self.ivars().send_burst_bytes,
                    )
                } {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(remote_addr = %remote_addr, error = %e, "BLE peripheral: failed to setup L2CAP channel");
                        return;
                    }
                };
                 if let Ok(mut pending) = self.ivars().pending_streams.lock() {
                    pending.push(SendablePeripheralStream(stream));
                }
                let _ = self
                    .ivars()
                    .sender
                    .lock()
                    .map(|s| s.try_send(PeripheralManagerEvent::L2CAPChannelOpened));
            }
        }

        #[unsafe(method(peripheralManagerDidStartAdvertising:error:))]
        fn did_start_advertising(
            &self,
            _peripheral: &CBPeripheralManager,
            error: Option<&NSError>,
        ) {
            if let Some(e) = error {
                warn!(error = %e, "Advertising start failed");
            } else {
                debug!("Advertising started");
            }
            let _ = self
                .ivars()
                .sender
                .lock()
                .map(|s| s.try_send(PeripheralManagerEvent::AdvertisingStarted));
        }
    }
);

impl FipsPeripheralDelegate {
    fn new(
        sender: tokio::sync::mpsc::Sender<PeripheralManagerEvent>,
        published_psm: Arc<AtomicU16>,
        mtu: u16,
        send_rate_bps: u64,
        send_burst_bytes: u32,
    ) -> Retained<Self> {
        let ivars = FipsPeripheralDelegateIvars {
            sender: StdMutex::new(sender),
            published_psm,
            pending_streams: StdMutex::new(Vec::new()),
            mtu,
            send_rate_bps,
            send_burst_bytes,
        };
        let this = FipsPeripheralDelegate::alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }
}

// ============================================================================
// BluestAcceptor
// ============================================================================

pub struct BluestAcceptor {
    rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<AnyStream>>,
}

impl BleAcceptor for BluestAcceptor {
    type Stream = AnyStream;
    async fn accept(&mut self) -> Result<AnyStream, TransportError> {
        self.rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| TransportError::Io(std::io::Error::other("acceptor channel closed")))
    }
}

// ============================================================================
// BluestScanner
// ============================================================================

pub struct BluestScanner {
    rx: tokio::sync::mpsc::Receiver<BleAddr>,
}

impl BleScanner for BluestScanner {
    async fn next(&mut self) -> Option<BleAddr> {
        self.rx.recv().await
    }
}

// ============================================================================
// BluestIo
// ============================================================================

pub struct BluestIo {
    adapter: Adapter,
    mtu: u16,
    devices: Arc<Mutex<HashMap<[u8; 6], Device>>>,
    send_rate_bps: u64,
    send_burst_bytes: u32,
    peripheral_manager: StdMutex<Option<Dispatched<CBPeripheralManager>>>,
    peripheral_delegate: Arc<StdMutex<Option<SendableDelegate>>>,
    published_psm: Arc<AtomicU16>,
    event_rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<PeripheralManagerEvent>>>,
    inbound_tx: StdMutex<Option<tokio::sync::mpsc::Sender<AnyStream>>>,
    advertising_enabled: StdMutex<bool>,
    _delegate_sender: tokio::sync::mpsc::Sender<PeripheralManagerEvent>,
    was_powered_off: Arc<tokio::sync::Mutex<bool>>,
}

// SAFETY: BluestIo's fields use interior mutability patterns
// (StdMutex, Arc<tokio::sync::Mutex>, Arc<AtomicBool>) for thread-safe
// access. The ARC-managed ObjC objects (adapter, peripheral_manager)
// are accessed only via Dispatched/RunLoopDispatched which serialize
// access through GCD serial queues and the main NSRunLoop.
unsafe impl Sync for BluestIo {}
unsafe impl Send for BluestIo {}

impl BluestIo {
    pub async fn new(
        _adapter_name: &str,
        mtu: u16,
        send_rate_bps: u64,
        send_burst_bytes: u32,
    ) -> Result<Self, TransportError> {
        let adapter = Adapter::default().await.map_err(|e| {
            TransportError::StartFailed(format!("CoreBluetooth adapter not found: {e}"))
        })?;
        adapter
            .wait_available()
            .await
            .map_err(|e| TransportError::StartFailed(format!("Bluetooth not available: {e}")))?;
        debug!("CoreBluetooth adapter ready");
        let _ = TOKIO_HANDLE.set(tokio::runtime::Handle::current());
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(BLE_EVENT_CHANNEL_DEPTH);
        Ok(Self {
            adapter,
            mtu,
            devices: Arc::new(Mutex::new(HashMap::new())),
            send_rate_bps,
            send_burst_bytes,
            peripheral_manager: StdMutex::new(None),
            peripheral_delegate: Arc::new(StdMutex::new(None)),
            published_psm: Arc::new(AtomicU16::new(0)),
            event_rx: Arc::new(tokio::sync::Mutex::new(event_rx)),
            inbound_tx: StdMutex::new(None),
            advertising_enabled: StdMutex::new(false),
            _delegate_sender: event_tx,
            was_powered_off: Arc::new(tokio::sync::Mutex::new(false)),
        })
    }

    async fn recover_peripheral_manager(
        manager: &Dispatched<CBPeripheralManager>,
        event_rx: &Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<PeripheralManagerEvent>>>,
        advertising_enabled: bool,
    ) -> Result<(), TransportError> {
        warn!("Recovering from sleep/wake: re-publishing L2CAP channel");
        manager.dispatch(|m| unsafe {
            m.publishL2CAPChannelWithEncryption(false);
        });

        wait_for_pm_event(
            event_rx,
            |e| matches!(e, PeripheralManagerEvent::L2CAPPublished { .. }),
            "L2CAP publish event channel closed",
        )
        .await?;
        info!("Recovery: L2CAP published");

        warn!("Recovering from sleep/wake: re-adding GATT service");
        add_gatt_psm_service(&manager);

        wait_for_pm_event(
            event_rx,
            |e| matches!(e, PeripheralManagerEvent::ServiceAdded),
            "Service add event channel closed",
        )
        .await?;
        info!("Recovery: GATT service re-added");

        if advertising_enabled {
            warn!("Recovering from sleep/wake: restarting advertising");
            let (fips_str, psm_str) = build_advertising_dict();
            manager.dispatch(move |m: &CBPeripheralManager| unsafe {
                let fips_uuid = CBUUID::UUIDWithString(&NSString::from_str(&fips_str));
                let psm_uuid = CBUUID::UUIDWithString(&NSString::from_str(&psm_str));
                let uuids = NSArray::from_retained_slice(&[fips_uuid, psm_uuid]);
                let ad = NSDictionary::from_retained_objects(
                    &[CBAdvertisementDataServiceUUIDsKey],
                    &[uuids.into()],
                );
                m.startAdvertising(Some(&ad));
            });

            wait_for_pm_event(
                event_rx,
                |e| matches!(e, PeripheralManagerEvent::AdvertisingStarted),
                "Advertising event channel closed",
            )
            .await?;
            info!("Recovery: advertising restarted");
        }

        Ok(())
    }

    pub async fn discover_gatt_psm(&self, addr: &BleAddr) -> Result<u16, TransportError> {
        let discover = async {
            let device = { self.devices.lock().await.get(&addr.device).cloned() };
            let device = device.ok_or_else(|| {
                TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: device not found in cache: {}",
                    addr
                )))
            })?;

            debug!(remote_addr = %addr, "GATT PSM discovery: discovering FIPS service");

            let services = device
                .discover_services_with_uuid(FIPS_GATT_PSM_SERVICE_UUID)
                .await
                .map_err(|e| {
                    TransportError::Io(std::io::Error::other(
                        gatt_err::enum_services(addr, &e),
                    ))
                })?;

            debug!(remote_addr = %addr, count = services.len(), "GATT PSM discovery: enumerated services");

            let psm_service = services
                .iter()
                .find(|s| s.uuid() == FIPS_GATT_PSM_SERVICE_UUID);
            let psm_service = match psm_service {
                Some(s) => s,
                None => {
                    return Err(TransportError::Io(std::io::Error::other(
                        gatt_err::service_not_found(addr),
                    )));
                }
            };

            let characteristics = psm_service.characteristics().await.map_err(|e| {
                TransportError::Io(std::io::Error::other(
                    gatt_err::enum_chars(addr, &e),
                ))
            })?;

            let psm_char = characteristics
                .iter()
                .find(|c| c.uuid() == FIPS_GATT_PSM_CHAR_UUID);
            let psm_char = match psm_char {
                Some(c) => c,
                None => {
                    return Err(TransportError::Io(std::io::Error::other(
                        gatt_err::char_not_found(addr),
                    )));
                }
            };

            let value = psm_char.read().await.map_err(|e| {
                TransportError::Io(std::io::Error::other(
                    gatt_err::read_psm(addr, &e),
                ))
            })?;

            let psm = parse_psm_value(&value, addr)?;

            debug!(remote_addr = %addr, psm, "GATT PSM discovery: discovered PSM");

            Ok(psm)
        };

        tokio::time::timeout(GATT_PSM_DISCOVER_TIMEOUT, discover)
            .await
            .map_err(|_| {
                TransportError::Io(std::io::Error::other(
                    gatt_err::timeout(addr),
                ))
            })?
    }
}

fn device_id_to_bytes(id: &DeviceId) -> [u8; 6] {
    let s = format!("{id}");
    if let Ok(uuid) = uuid::Uuid::parse_str(&s) {
        let b = uuid.as_bytes();
        [b[0], b[1], b[2], b[3], b[4], b[5]]
    } else {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut hasher);
        let h = hasher.finish().to_le_bytes();
        [h[0], h[1], h[2], h[3], h[4], h[5]]
    }
}

unsafe fn nsuuid_to_bytes(uuid: &objc2_foundation::NSUUID) -> [u8; 6] {
    let s = uuid.UUIDString();
    if let Ok(parsed) = uuid::Uuid::parse_str(&s.to_string()) {
        let b = parsed.as_bytes();
        [b[0], b[1], b[2], b[3], b[4], b[5]]
    } else {
        [0, 0, 0, 0, 0, 0]
    }
}

fn format_uuid(uuid: &uuid::Uuid) -> String {
    uuid.hyphenated().to_string().to_uppercase()
}

impl BleIo for BluestIo {
    type Stream = AnyStream;
    type Acceptor = BluestAcceptor;
    type Scanner = BluestScanner;

    async fn listen(&self, _psm: u16) -> Result<BluestAcceptor, TransportError> {
        if self.peripheral_manager.lock().unpoison().is_some() {
            return Err(TransportError::StartFailed(
                "BLE peripheral already listening".into(),
            ));
        }

        let (event_tx, new_event_rx) = tokio::sync::mpsc::channel(BLE_EVENT_CHANNEL_DEPTH);
        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(BLE_INBOUND_CHANNEL_DEPTH);

        let manager = {
            let delegate = FipsPeripheralDelegate::new(
                event_tx,
                self.published_psm.clone(),
                self.mtu,
                self.send_rate_bps,
                self.send_burst_bytes,
            );
            let protocol = ProtocolObject::from_retained(delegate.clone());

            let mgr = unsafe {
                CBPeripheralManager::initWithDelegate_queue_options(
                    CBPeripheralManager::alloc(),
                    Some(&protocol),
                    Some(peripheral_queue()),
                    None,
                )
            };
            let mgr = unsafe { Dispatched::new(mgr) };

            *self.peripheral_manager.lock().unpoison() = Some(mgr.clone());
            *self.peripheral_delegate.lock().unpoison() = Some(SendableDelegate(delegate));
            *self.inbound_tx.lock().unpoison() = Some(inbound_tx.clone());
            mgr
        };

        *self.event_rx.lock().await = new_event_rx;

        // Wait for PoweredOn or detect an immediate sleep/wake cycle.
        loop {
            let mut rx = self.event_rx.lock().await;
            match rx.recv().await {
                Some(PeripheralManagerEvent::StateChanged(state)) => {
                    if state == CBManagerState::PoweredOff {
                        warn!("Peripheral manager powered off during listen startup");
                        *self.was_powered_off.lock().await = true;
                        continue;
                    }
                    if state == CBManagerState::PoweredOn {
                        if *self.was_powered_off.lock().await {
                            warn!(
                                "Recovering from sleep/wake: Peripheral manager powered on during startup"
                            );
                        } else {
                            debug!("Peripheral manager powered on");
                        }
                        break;
                    }
                    if state == CBManagerState::Unsupported || state == CBManagerState::Unauthorized
                    {
                        return Err(TransportError::StartFailed(format!(
                            "Bluetooth not available: state {:?}",
                            state
                        )));
                    }
                }
                Some(_) => {}
                None => {
                    return Err(TransportError::StartFailed(
                        "Peripheral manager event channel closed".into(),
                    ));
                }
            }
        }

        if *self.was_powered_off.lock().await {
            let advertising_enabled = *self.advertising_enabled.lock().unpoison();
            Self::recover_peripheral_manager(&manager, &self.event_rx, advertising_enabled).await?;
            *self.was_powered_off.lock().await = false;
        }

        manager.dispatch(|m| unsafe {
            m.publishL2CAPChannelWithEncryption(false);
        });

        // Wait for PSM
        wait_for_pm_event(
            &self.event_rx,
            |e| matches!(e, PeripheralManagerEvent::L2CAPPublished { .. }),
            "L2CAP publish event channel closed",
        )
        .await?;
        info!("L2CAP published");

        // Create GATT service with PSM characteristic
        add_gatt_psm_service(&manager);

        // Wait for service added
        wait_for_pm_event(
            &self.event_rx,
            |e| matches!(e, PeripheralManagerEvent::ServiceAdded),
            "Service add event channel closed",
        )
        .await?;

        // Bridge incoming L2CAP channels to acceptor
        let event_rx_bridge = self.event_rx.clone();
        let delegate_arc = self.peripheral_delegate.clone();
        let manager_for_bridge = manager.clone();
        let was_powered_off = Arc::clone(&self.was_powered_off);
        let advertising_enabled = *self.advertising_enabled.lock().unpoison();
        tokio::spawn(async move {
            loop {
                let event = {
                    let mut rx = event_rx_bridge.lock().await;
                    rx.recv().await
                };
                match event {
                    Some(PeripheralManagerEvent::StateChanged(CBManagerState::PoweredOff)) => {
                        warn!("Peripheral manager powered off (sleep)");
                        *was_powered_off.lock().await = true;
                    }
                    Some(PeripheralManagerEvent::StateChanged(CBManagerState::PoweredOn)) => {
                        let mut powered_off = was_powered_off.lock().await;
                        if *powered_off {
                            warn!("Recovering from sleep/wake: Peripheral manager powered on");
                            *powered_off = false;
                            drop(powered_off);
                            if let Err(error) = BluestIo::recover_peripheral_manager(
                                &manager_for_bridge,
                                &event_rx_bridge,
                                advertising_enabled,
                            )
                            .await
                            {
                                warn!(error = %error, "BLE peripheral manager recovery failed");
                                return;
                            }
                        }
                    }
                    Some(PeripheralManagerEvent::L2CAPChannelOpened) => {
                        let streams: Vec<SendablePeripheralStream> = {
                            let guard = delegate_arc.lock().unpoison();
                            let Some(d) = guard.as_ref() else { return };
                            let mut pending = d.0.ivars().pending_streams.lock().unpoison();
                            std::mem::take(&mut *pending)
                        };
                        for stream in streams {
                            if inbound_tx
                                .send(AnyStream::Peripheral(stream.0))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    Some(_) => {}
                    None => return,
                }
            }
        });

        debug!("BLE listen: macOS peripheral acceptor ready");
        Ok(BluestAcceptor {
            rx: tokio::sync::Mutex::new(inbound_rx),
        })
    }

    async fn connect(&self, addr: &BleAddr, psm: u16) -> Result<AnyStream, TransportError> {
        let device = { self.devices.lock().await.get(&addr.device).cloned() };
        let device = device.ok_or_else(|| {
            TransportError::Io(std::io::Error::other(format!(
                "BLE device not found in cache: {addr}"
            )))
        })?;

        self.adapter.connect_device(&device).await.map_err(|e| {
            TransportError::Io(std::io::Error::other(format!("BLE connect {addr}: {e}")))
        })?;
        let effective_psm = match self.discover_gatt_psm(addr).await {
            Ok(discovered_psm) => {
                debug!(remote_addr = %addr, configured_psm = psm, discovered_psm, "BLE connect: using GATT-discovered PSM");
                discovered_psm
            }
            Err(e) => {
                debug!(remote_addr = %addr, configured_psm = psm, error = %e, "BLE connect: GATT PSM discovery unavailable, using configured PSM");
                psm
            }
        };
        debug!(remote_addr = %addr, psm = effective_psm, "Opening L2CAP channel");

        let channel = device
            .open_l2cap_channel(effective_psm, false)
            .await
            .map_err(|e| {
                TransportError::Io(std::io::Error::other(format!(
                    "L2CAP open {addr} PSM {effective_psm}: {e}"
                )))
            })?;
        let (reader, writer) = channel.split();
        debug!(remote_addr = %addr, psm = effective_psm, "L2CAP channel open");

        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(BLE_CENTRAL_QUEUE_DEPTH);

        let shared_writer = Arc::new(tokio::sync::Mutex::new(writer));
        let drain_writer = shared_writer.clone();

        let rate_limiter = Arc::new(Mutex::new(SendRateLimiter::new(
            self.send_rate_bps,
            self.send_burst_bytes,
        )));
        let drain_limiter = rate_limiter.clone();

        let alive = Arc::new(AtomicBool::new(true));
        let drain_alive = alive.clone();
        let drain_addr = addr.clone();
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                drain_limiter.lock().await.acquire(frame.len()).await;
                let mut writer = drain_writer.lock().await;
                if let Err(e) = writer.write_all(&frame).await {
                    warn!(remote_addr = %drain_addr, error = %e, "BLE central drain task write error, stopping");
                    drain_alive.store(false, Ordering::Relaxed);
                    break;
                }
            }
            debug!(remote_addr = %drain_addr, "BLE central drain task stopped");
        });

        Ok(AnyStream::Central(BluestStream {
            reader: Mutex::new(reader),
            tx,
            urgent_writer: shared_writer,
            rate_limiter,
            alive,
            remote: addr.clone(),
            mtu: self.mtu,
            recv_buf: Mutex::new(Vec::new()),
        }))
    }

    async fn start_advertising(&self) -> Result<(), TransportError> {
        *self.advertising_enabled.lock().unpoison() = true;
        let guard = self.peripheral_manager.lock().unpoison();
        let manager = match guard.as_ref() {
            Some(m) => m.clone(),
            None => {
                debug!("BLE advertising: no peripheral manager");
                return Ok(());
            }
        };
        drop(guard);
        let (fips_str, psm_str) = build_advertising_dict();
        manager.dispatch(move |m: &CBPeripheralManager| unsafe {
            let fips_uuid = CBUUID::UUIDWithString(&NSString::from_str(&fips_str));
            let psm_uuid = CBUUID::UUIDWithString(&NSString::from_str(&psm_str));
            let uuids = NSArray::from_retained_slice(&[fips_uuid, psm_uuid]);
            let ad = NSDictionary::from_retained_objects(
                &[CBAdvertisementDataServiceUUIDsKey],
                &[uuids.into()],
            );
            m.startAdvertising(Some(&ad));
        });
        debug!("BLE advertising: started");
        Ok(())
    }

    async fn stop_advertising(&self) -> Result<(), TransportError> {
        *self.advertising_enabled.lock().unpoison() = false;
        let guard = self.peripheral_manager.lock().unpoison();
        if let Some(manager) = guard.as_ref() {
            manager.dispatch(|m: &CBPeripheralManager| unsafe {
                m.stopAdvertising();
            });
            debug!("BLE advertising: stopped");
        }
        Ok(())
    }

    async fn disconnect_device(&self, addr: &BleAddr) {
        let device = { self.devices.lock().await.get(&addr.device).cloned() };
        if let Some(device) = device {
            match self.adapter.disconnect_device(&device).await {
                Ok(()) => debug!(remote_addr = %addr, "Disconnected CoreBluetooth peripheral"),
                Err(e) => {
                    debug!(remote_addr = %addr, error = %e, "Failed to disconnect peripheral")
                }
            }
        }
    }

    async fn start_scanning(&self) -> Result<BluestScanner, TransportError> {
        let (tx, rx) = tokio::sync::mpsc::channel(BLE_SCAN_CHANNEL_DEPTH);
        let devices = self.devices.clone();
        let adapter = self.adapter.clone();

        tokio::spawn(async move {
            let scan_stream = match adapter.scan(&[FIPS_SERVICE_UUID]).await {
                Ok(s) => s,
                Err(e) => {
                    debug!(error = %e, "BLE scan failed to start");
                    return;
                }
            };

            futures::pin_mut!(scan_stream);
            #[allow(clippy::mutable_key_type)]
            let mut seen: HashMap<Device, [u8; 6]> = HashMap::new();
            while let Some(discovered) = scan_stream.next().await {
                let device = discovered.device;

                if let Some(&existing) = seen.get(&device) {
                    let addr = BleAddr {
                        adapter: MACOS_ADAPTER_NAME.to_string(),
                        device: existing,
                    };
                    if tx.send(addr).await.is_err() {
                        break;
                    }
                    continue;
                }

                let bytes = device_id_to_bytes(&device.id());
                seen.insert(device.clone(), bytes);

                let name = discovered
                    .adv_data
                    .local_name
                    .as_deref()
                    .unwrap_or("unknown");
                debug!(
                    name = name,
                    addr = format!(
                        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
                    ),
                    "Discovered FIPS BLE device"
                );

                devices.lock().await.insert(bytes, device);

                let addr = BleAddr {
                    adapter: MACOS_ADAPTER_NAME.to_string(),
                    device: bytes,
                };

                if tx.send(addr).await.is_err() {
                    break;
                }
            }
            trace!("BLE scan stream ended");
        });

        Ok(BluestScanner { rx })
    }

    fn local_addr(&self) -> Result<BleAddr, TransportError> {
        Ok(BleAddr {
            adapter: MACOS_ADAPTER_NAME.to_string(),
            device: ZERO_BLE_ADDR,
        })
    }

    fn adapter_name(&self) -> &str {
        MACOS_ADAPTER_NAME
    }

    fn discover_gatt_psm(
        &self,
        addr: &BleAddr,
    ) -> impl std::future::Future<Output = Result<u16, TransportError>> + Send {
        BluestIo::discover_gatt_psm(self, addr)
    }
}
