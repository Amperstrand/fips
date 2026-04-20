//! macOS BLE I/O via bluest (central role) and objc2-core-bluetooth (peripheral role).

use super::*;
use crate::transport::ble::addr::BleAddr;
use crate::transport::ble::rate_limit::SendRateLimiter;
use crate::transport::TransportError;

use bluest::{Adapter, Device, DeviceId};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, trace, warn};

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::mem::MaybeUninit;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::OnceLock;
use std::sync::Mutex as StdMutex;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, Message};
use objc2_core_bluetooth::{
    CBAdvertisementDataServiceUUIDsKey, CBATTError, CBATTRequest, CBAttributePermissions,
    CBCharacteristicProperties, CBL2CAPChannel, CBL2CAPPSM,
    CBManagerState, CBMutableCharacteristic, CBMutableService, CBPeripheralManager,
    CBPeripheralManagerDelegate, CBUUID,
};
use objc2_foundation::{
    NSArray, NSData, NSDictionary, NSError, NSDefaultRunLoopMode, NSInputStream, NSNotificationCenter,
    NSNotification, NSObject, NSObjectProtocol, NSOutputStream, NSRunLoop, NSStream, NSStreamDelegate,
    NSStreamEvent, NSStreamStatus, NSString,
};
use dispatch2::{DispatchQoS, DispatchQueue, DispatchQueueAttr, DispatchRetained, GlobalQueueIdentifier};

const FIPS_SERVICE_UUID: uuid::Uuid =
    uuid::Uuid::from_u128(0x9c90_b790_2cc5_42c0_9f87_c9cc_4064_8f4c);

const FIPS_GATT_PSM_SERVICE_UUID: uuid::Uuid =
    uuid::Uuid::from_u128(0x0e2c_43b1_51b9_4667_a1d1_a95e_a79f_d19b);

const FIPS_GATT_PSM_CHAR_UUID: uuid::Uuid =
    uuid::Uuid::from_u128(0x250c_88dd_3dff_4c41_83b2_f1b4_e3d8_20cc);

const MACOS_ADAPTER_NAME: &str = "default";

const WRITE_NOTIFY_NAME: &str = "FIPSPeripheralWrite";

/// Bounded queue depth for central-role BLE sends.
/// 32 frames ≈ 64KB at average FMP frame size; drains in ~2s at 250kbps.
const BLE_CENTRAL_QUEUE_DEPTH: usize = 32;

/// Timeout for enqueuing a framed message when the bounded queue is full.
/// Matches Linux BLE send timeout; 4× safety margin over expected drain time.
const BLE_CENTRAL_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Bounded queue depth for peripheral-role BLE sends.
/// 32 frames ≈ 64KB at average FMP frame size; drains in ~2s at 250kbps.
const BLE_PERIPHERAL_QUEUE_DEPTH: usize = 32;

/// Maximum total bytes allowed in the peripheral write queue.
const BLE_PERIPHERAL_QUEUE_BYTE_CAP: usize = 65536;

/// Timeout for enqueuing when the peripheral write queue is full.
/// 4× safety margin over expected drain time (~3.4s at 150kbps).
const BLE_PERIPHERAL_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

// ============================================================================
// Dispatch helpers
// ============================================================================

fn peripheral_queue() -> &'static DispatchQueue {
    static CELL: OnceLock<DispatchRetained<DispatchQueue>> = OnceLock::new();
    CELL.get_or_init(|| {
        let utility = DispatchQueue::global_queue(
            GlobalQueueIdentifier::QualityOfService(DispatchQoS::Utility),
        );
        DispatchQueue::new_with_target(
            "FIPS-BLE-Peripheral",
            DispatchQueueAttr::SERIAL,
            Some(&utility),
        )
    })
}

fn ble_runloop() -> &'static NSRunLoop {
    static MAIN_PTR: OnceLock<usize> = OnceLock::new();
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
        unsafe { ret.assume_init() }
    }
}

impl<T: Message> Clone for Dispatched<T> {
    fn clone(&self) -> Self {
        self.dispatch(|val| Self(UnsafeCell::new(val.retain())))
    }
}

struct RunLoopDispatched<T>(UnsafeCell<Retained<T>>);

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
    remote: BleAddr,
    mtu: u16,
    recv_buf: Mutex<Vec<u8>>,
    rate_limiter: Option<Mutex<SendRateLimiter>>,
}

impl BleStream for BluestStream {
    async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
        let framed_len = 2 + data.len();
        if let Some(ref limiter) = self.rate_limiter {
            limiter.lock().await.acquire(framed_len).await;
        }

        let mut framed = Vec::with_capacity(framed_len);
        framed.extend_from_slice(&(data.len() as u16).to_be_bytes());
        framed.extend_from_slice(data);
        trace!(len = data.len(), framed_len = framed.len(), addr = %self.remote, "BLE macOS send");

        match self.tx.try_send(framed) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(framed)) => {
                trace!(addr = %self.remote, queue_depth = BLE_CENTRAL_QUEUE_DEPTH, "BLE central queue full, waiting with timeout");
                tokio::time::timeout(BLE_CENTRAL_SEND_TIMEOUT, self.tx.send(framed))
                    .await
                    .map_err(|_| {
                        warn!(addr = %self.remote, timeout_secs = BLE_CENTRAL_SEND_TIMEOUT.as_secs(), "BLE central send timeout (queue full)");
                        TransportError::Timeout
                    })?
                    .map_err(|_| TransportError::Io(std::io::Error::other("BLE central send channel closed")))
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(TransportError::Io(std::io::Error::other("BLE central send channel closed")))
            }
        }
    }

    async fn send_urgent(&self, data: &[u8]) -> Result<(), TransportError> {
        let mut framed = Vec::with_capacity(2 + data.len());
        framed.extend_from_slice(&(data.len() as u16).to_be_bytes());
        framed.extend_from_slice(data);
        trace!(len = data.len(), framed_len = framed.len(), addr = %self.remote, "BLE macOS send_urgent");

        match self.tx.try_send(framed) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(framed)) => {
                trace!(addr = %self.remote, queue_depth = BLE_CENTRAL_QUEUE_DEPTH, "BLE central queue full (urgent), waiting with timeout");
                tokio::time::timeout(BLE_CENTRAL_SEND_TIMEOUT, self.tx.send(framed))
                    .await
                    .map_err(|_| {
                        warn!(addr = %self.remote, timeout_secs = BLE_CENTRAL_SEND_TIMEOUT.as_secs(), "BLE central send_urgent timeout (queue full)");
                        TransportError::Timeout
                    })?
                    .map_err(|_| TransportError::Io(std::io::Error::other("BLE central send channel closed")))
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(TransportError::Io(std::io::Error::other("BLE central send channel closed")))
            }
        }
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        loop {
            {
                let mut recv_buf = self.recv_buf.lock().await;
                if recv_buf.len() >= 2 {
                    let payload_len = u16::from_be_bytes([recv_buf[0], recv_buf[1]]) as usize;
                    if recv_buf.len() >= 2 + payload_len {
                        let copy_len = payload_len.min(buf.len());
                        buf[..copy_len].copy_from_slice(&recv_buf[2..2 + copy_len]);
                        recv_buf.drain(..2 + payload_len);
                        trace!(
                            len = copy_len,
                            buf_remaining = recv_buf.len(),
                            addr = %self.remote,
                            "BLE macOS recv frame"
                        );
                        return Ok(copy_len);
                    }
                }
            }

            let mut tmp = [0u8; 2048];
            let n = self
                .reader
                .lock()
                .await
                .read(&mut tmp)
                .await
                .map_err(|e| TransportError::Io(std::io::Error::other(format!("BLE recv: {e}"))))?;
            if n == 0 {
                return Ok(0);
            }
            trace!(raw_bytes = n, addr = %self.remote, "BLE macOS recv raw");
            self.recv_buf.lock().await.extend_from_slice(&tmp[..n]);
        }
    }

    fn send_mtu(&self) -> u16 { self.mtu }
    fn recv_mtu(&self) -> u16 { self.mtu }
    fn remote_addr(&self) -> &BleAddr { &self.remote }

    async fn set_rate_bps(&self, rate_bps: u64) {
        if let Some(ref limiter) = self.rate_limiter {
            limiter.lock().await.set_rate_bps(rate_bps);
        }
    }

    fn supports_bidirectional_pubkey_exchange(&self) -> bool { true }
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
        match self { AnyStream::Central(s) => s.send_mtu(), AnyStream::Peripheral(s) => s.send_mtu() }
    }
    fn recv_mtu(&self) -> u16 {
        match self { AnyStream::Central(s) => s.recv_mtu(), AnyStream::Peripheral(s) => s.recv_mtu() }
    }
    fn remote_addr(&self) -> &BleAddr {
        match self { AnyStream::Central(s) => s.remote_addr(), AnyStream::Peripheral(s) => s.remote_addr() }
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
unsafe impl Send for SendableInputDelegate {}

impl SendableInputDelegate {
    fn retained(&self) -> Retained<PeripheralInputDelegate> {
        self.0.retain()
    }
}

struct SendableOutputDelegate(Retained<PeripheralOutputDelegate>);
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
                    if let Ok(mut eof) = self.ivars().eof.lock() { *eof = true; }
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
            if event_code != NSStreamEvent::HasSpaceAvailable { return; }
            let output_stream = stream.downcast_ref::<NSOutputStream>().unwrap();
            if self.ivars().output_stream.lock().ok().map_or(true, |s| s.is_none()) {
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
        if let Ok(mut guard) = self.ivars().output_stream.lock() {
            if guard.is_none() {
                debug!("BLE peripheral: eagerly storing output stream reference");
                *guard = Some(stream);
            }
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
        let mut queue = match self.ivars().write_queue.lock() { Ok(q) => q, Err(_) => return };
        let initial_len = queue.len();
        while let Some(data) = queue.pop_front() {
            let mut offset = 0;
            while offset < data.len() {
                let res = unsafe {
                    output_stream.write_maxLength(
                        NonNull::new_unchecked(data[offset..].as_ptr() as *mut u8),
                        data.len() - offset,
                    )
                };
                if res < 0 { return; }
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

    #[allow(dead_code)]
    fn queue_len(&self) -> usize {
        self.ivars().write_queue.lock().map(|q| q.len()).unwrap_or(0)
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
        self.input_stream.dispatch(|s| unsafe { s.setDelegate(None); s.close(); });
        self.output_stream.dispatch(|s| unsafe { s.setDelegate(None); s.close(); });
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
    remote: BleAddr,
    mtu: u16,
    recv_buf: Mutex<Vec<u8>>,
    rate_limiter: Option<Mutex<SendRateLimiter>>,
}

impl PeripheralStream {
    unsafe fn setup_channel(
        channel: SendableChannel,
        remote: BleAddr,
        mtu: u16,
        send_rate_bps: u64,
        send_burst_bytes: u32,
    ) -> Self {
        let input_stream = unsafe { channel.0.inputStream() }.expect("CBL2CAPChannel has no input stream");
        let output_stream = unsafe { channel.0.outputStream() }.expect("CBL2CAPChannel has no output stream");

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
                trace!("BLE peripheral: input stream scheduled and opened, status={:?}", stream.streamStatus());
            });

            let output_protocol = ProtocolObject::from_retained(output_delegate_for_setup.retained());
            output_stream_for_setup.with(|stream| {
                stream.setDelegate(Some(&output_protocol));
                stream.scheduleInRunLoop_forMode(ble_rl, NSDefaultRunLoopMode);
                stream.open();
                trace!("BLE peripheral: output stream scheduled and opened, status={:?}", stream.streamStatus());
            });
        });

        // Eagerly store the output stream reference. The main NSRunLoop is never
        // pumped in a Rust CLI app, so HasSpaceAvailable events are never delivered.
        // Without this, on_write_notify finds output_stream = None and silently drops writes.
        output_delegate.set_output_stream(output_stream.clone());

        let center = NSNotificationCenter::defaultCenter();
        let notify_name = NSString::from_str(WRITE_NOTIFY_NAME);
        unsafe { center.addObserver_selector_name_object(
            &output_delegate, objc2::sel!(onWriteNotify:), Some(&notify_name), None,
        ) };

        let input_dispatched = unsafe { RunLoopDispatched::new(input_stream.retained()) };
        let output_dispatched = unsafe { RunLoopDispatched::new(output_stream.retained()) };

        let input_stream_for_reader = input_stream.clone();
        let input_delegate_for_reader = SendableInputDelegate(input_delegate.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let res = unsafe {
                    input_stream_for_reader.with(|stream| {
                        stream.read_maxLength(
                            NonNull::new_unchecked(buf.as_mut_ptr()),
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

                let status = unsafe { input_stream_for_reader.with(|stream| stream.streamStatus()) };
                if res < 0 || matches!(status, NSStreamStatus::AtEnd | NSStreamStatus::Closed | NSStreamStatus::Error) {
                    if let Ok(mut eof) = input_delegate_for_reader.0.ivars().eof.lock() {
                        *eof = true;
                    }
                    input_delegate_for_reader.0.ivars().notify.notify_one();
                    debug!(?status, read_result = res, "PeripheralInput reader thread stopping");
                    break;
                }

                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });

        let closer = Arc::new(PeripheralCloser {
            input_stream: input_dispatched,
            output_stream: output_dispatched,
            _center_observer: output_delegate.clone(),
        });

        PeripheralStream {
            _channel: channel,
            _input_delegate: input_delegate,
            output_delegate,
            closer,
            read_notify,
            queue_space_notify,
            remote,
            mtu,
            recv_buf: Mutex::new(Vec::new()),
            rate_limiter: if send_rate_bps > 0 {
                Some(Mutex::new(SendRateLimiter::new(send_rate_bps, send_burst_bytes)))
            } else { None },
        }
    }

    fn notify_write(&self) {
        unsafe {
            let name = NSString::from_str(WRITE_NOTIFY_NAME);
            NSNotificationCenter::defaultCenter().postNotificationName_object(&name, None);
        }
    }

    async fn enqueue_with_backpressure(&self, framed: &[u8], label: &str) -> Result<(), TransportError> {
        loop {
            let notified = self.queue_space_notify.notified();
            if self.output_delegate.try_enqueue(framed) {
                self.notify_write();
                return Ok(());
            }
            trace!(addr = %self.remote, %label, queue_depth = BLE_PERIPHERAL_QUEUE_DEPTH, "BLE peripheral queue full, waiting for space");
            match tokio::time::timeout(BLE_PERIPHERAL_SEND_TIMEOUT, notified).await {
                Ok(()) => continue,
                Err(_) => {
                    warn!(addr = %self.remote, %label, timeout_secs = BLE_PERIPHERAL_SEND_TIMEOUT.as_secs(), "BLE peripheral timeout (queue full)");
                    return Err(TransportError::Timeout);
                }
            }
        }
    }
}

impl BleStream for PeripheralStream {
    async fn send(&self, data: &[u8]) -> Result<(), TransportError> {
        let framed_len = 2 + data.len();
        if let Some(ref limiter) = self.rate_limiter {
            limiter.lock().await.acquire(framed_len).await;
        }
        let framed = {
            let mut f = Vec::with_capacity(framed_len);
            f.extend_from_slice(&(data.len() as u16).to_be_bytes());
            f.extend_from_slice(data);
            f
        };
        trace!(len = data.len(), addr = %self.remote, "BLE peripheral send");
        self.enqueue_with_backpressure(&framed, "send").await
    }

    async fn send_urgent(&self, data: &[u8]) -> Result<(), TransportError> {
        let framed = {
            let mut f = Vec::with_capacity(2 + data.len());
            f.extend_from_slice(&(data.len() as u16).to_be_bytes());
            f.extend_from_slice(data);
            f
        };
        trace!(len = data.len(), addr = %self.remote, "BLE peripheral send_urgent");
        self.enqueue_with_backpressure(&framed, "send_urgent").await
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        loop {
            {
                let mut recv_buf = self.recv_buf.lock().await;
                if recv_buf.len() >= 2 {
                    let payload_len = u16::from_be_bytes([recv_buf[0], recv_buf[1]]) as usize;
                    if recv_buf.len() >= 2 + payload_len {
                        let copy_len = payload_len.min(buf.len());
                        buf[..copy_len].copy_from_slice(&recv_buf[2..2 + copy_len]);
                        recv_buf.drain(..2 + payload_len);
                        trace!("BLE peripheral recv: returned {} bytes from recv_buf", copy_len);
                        return Ok(copy_len);
                    }
                }
            }

            let bytes = self._input_delegate.take_buffer();
            if !bytes.is_empty() {
                trace!(raw_bytes = bytes.len(), addr = %self.remote, "BLE peripheral recv raw");
                self.recv_buf.lock().await.extend_from_slice(&bytes);
                continue;
            }

            if self._input_delegate.reached_eof() {
                trace!(addr = %self.remote, "BLE peripheral recv EOF");
                return Ok(0);
            }

            self.read_notify.notified().await;
        }
    }

    fn send_mtu(&self) -> u16 { self.mtu }
    fn recv_mtu(&self) -> u16 { self.mtu }
    fn remote_addr(&self) -> &BleAddr { &self.remote }
    async fn set_rate_bps(&self, rate_bps: u64) {
        if let Some(ref limiter) = self.rate_limiter {
            limiter.lock().await.set_rate_bps(rate_bps);
        }
    }

    fn supports_bidirectional_pubkey_exchange(&self) -> bool { true }
}

// ============================================================================
// PeripheralManagerDelegate
// ============================================================================

enum PeripheralManagerEvent {
    StateChanged(CBManagerState),
    L2CAPPublished { psm: u16 },
    ServiceAdded,
    L2CAPChannelOpened,
    AdvertisingStarted,
}

struct SendableChannel(Retained<CBL2CAPChannel>);
unsafe impl Send for SendableChannel {}
unsafe impl Sync for SendableChannel {}

struct SendablePeripheralStream(PeripheralStream);
unsafe impl Send for SendablePeripheralStream {}
unsafe impl Sync for SendablePeripheralStream {}

struct SendableDelegate(Retained<FipsPeripheralDelegate>);
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
            debug!("Peripheral manager state changed: {:?}", state);
            let _ = self.ivars().sender.lock()
                .map(|s| s.try_send(PeripheralManagerEvent::StateChanged(state)));
        }

        #[unsafe(method(peripheralManager:didPublishL2CAPChannel:error:))]
        fn did_publish_l2cap(&self, _peripheral: &CBPeripheralManager, psm: CBL2CAPPSM, error: Option<&NSError>) {
            if let Some(e) = error { error!("L2CAP channel publish failed: {:?}", e); return; }
            info!("L2CAP channel published with PSM: {}", psm);
            self.ivars().published_psm.store(psm, Ordering::SeqCst);
            let _ = self.ivars().sender.lock()
                .map(|s| s.try_send(PeripheralManagerEvent::L2CAPPublished { psm }));
        }

        #[unsafe(method(peripheralManager:didAddService:error:))]
        fn did_add_service(&self, _peripheral: &CBPeripheralManager, _service: &objc2_core_bluetooth::CBService, error: Option<&NSError>) {
            if let Some(e) = error { error!("GATT service add failed: {:?}", e); return; }
            debug!("GATT service added");
            let _ = self.ivars().sender.lock()
                .map(|s| s.try_send(PeripheralManagerEvent::ServiceAdded));
        }

        #[unsafe(method(peripheralManager:didReceiveReadRequest:))]
        fn did_receive_read_request(&self, peripheral: &CBPeripheralManager, request: &CBATTRequest) {
            let psm = self.ivars().published_psm.load(Ordering::SeqCst);
            let value = NSData::with_bytes(&psm.to_le_bytes());
            unsafe {
                request.setValue(Some(&value));
                peripheral.respondToRequest_withResult(request, CBATTError::Success);
            }
            trace!("Responded to read request with PSM: {}", psm);
        }

        #[unsafe(method(peripheralManager:didOpenL2CAPChannel:error:))]
        fn did_open_l2cap_channel(&self, _peripheral: &CBPeripheralManager, channel: Option<&CBL2CAPChannel>, error: Option<&NSError>) {
            if let Some(e) = error { error!("L2CAP channel open failed: {:?}", e); return; }
            if let Some(channel) = channel {
                debug!("Incoming L2CAP channel opened");
                let remote = unsafe { channel.peer() }.map(|p| {
                    let identifier = unsafe { p.identifier() };
                    let bytes = unsafe { nsuuid_to_bytes(&identifier) };
                    BleAddr { adapter: MACOS_ADAPTER_NAME.to_string(), device: bytes, rssi: None }
                }).unwrap_or_else(|| BleAddr { adapter: MACOS_ADAPTER_NAME.to_string(), device: [0, 0, 0, 0, 0, 0], rssi: None });
                let stream = unsafe {
                    PeripheralStream::setup_channel(
                        SendableChannel(channel.retain()),
                        remote,
                        self.ivars().mtu,
                        self.ivars().send_rate_bps,
                        self.ivars().send_burst_bytes,
                    )
                };
                if let Ok(mut pending) = self.ivars().pending_streams.lock() {
                    pending.push(SendablePeripheralStream(stream));
                }
                let _ = self.ivars().sender.lock()
                    .map(|s| s.try_send(PeripheralManagerEvent::L2CAPChannelOpened));
            }
        }

        #[unsafe(method(peripheralManagerDidStartAdvertising:error:))]
        fn did_start_advertising(&self, _peripheral: &CBPeripheralManager, error: Option<&NSError>) {
            if let Some(e) = error { warn!("Advertising start failed: {:?}", e); } else { debug!("Advertising started"); }
            let _ = self.ivars().sender.lock()
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
        self.rx.lock().await.recv().await
            .ok_or_else(|| TransportError::Io(std::io::Error::other("acceptor channel closed")))
    }
}

// ============================================================================
// BluestScanner
// ============================================================================

pub struct BluestScanner { rx: tokio::sync::mpsc::Receiver<BleAddr> }

impl BleScanner for BluestScanner {
    async fn next(&mut self) -> Option<BleAddr> { self.rx.recv().await }
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
    _delegate_sender: tokio::sync::mpsc::Sender<PeripheralManagerEvent>,
    was_powered_off: Arc<tokio::sync::Mutex<bool>>,
}

unsafe impl Sync for BluestIo {}
unsafe impl Send for BluestIo {}

impl BluestIo {
    pub async fn new(
        _adapter_name: &str, mtu: u16, send_rate_bps: u64, send_burst_bytes: u32,
    ) -> Result<Self, TransportError> {
        let adapter = Adapter::default().await
            .ok_or_else(|| TransportError::StartFailed("CoreBluetooth adapter not found".into()))?;
        adapter.wait_available().await
            .map_err(|e| TransportError::StartFailed(format!("Bluetooth not available: {e}")))?;
        debug!("CoreBluetooth adapter ready");
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(32);
        Ok(Self {
            adapter, mtu,
            devices: Arc::new(Mutex::new(HashMap::new())),
            send_rate_bps, send_burst_bytes,
            peripheral_manager: StdMutex::new(None),
            peripheral_delegate: Arc::new(StdMutex::new(None)),
            published_psm: Arc::new(AtomicU16::new(0)),
            event_rx: Arc::new(tokio::sync::Mutex::new(event_rx)),
            inbound_tx: StdMutex::new(None),
            _delegate_sender: event_tx,
            was_powered_off: Arc::new(tokio::sync::Mutex::new(false)),
        })
    }

    pub async fn discover_gatt_psm(&self, addr: &BleAddr) -> Result<u16, TransportError> {
        let discover = async {
            let device = { self.devices.lock().await.get(&addr.device).cloned() };
            let device = device.ok_or_else(|| {
                TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: device not found in cache: {}", addr
                )))
            })?;

            debug!(addr = %addr, "GATT PSM discovery: enumerating services");

            let services = device.services().await.map_err(|e| {
                TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: failed to enumerate services for {}: {}", addr, e
                )))
            })?;

            debug!(addr = %addr, count = services.len(), "GATT PSM discovery: enumerated services");

            let psm_service = services.iter().find(|s| s.uuid() == FIPS_GATT_PSM_SERVICE_UUID);
            let psm_service = match psm_service {
                Some(s) => s,
                None => {
                    return Err(TransportError::Io(std::io::Error::other(format!(
                        "discover_gatt_psm: FIPS GATT PSM service not found on {}", addr
                    ))));
                }
            };

            let characteristics = psm_service.characteristics().await.map_err(|e| {
                TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: failed to enumerate characteristics for {}: {}", addr, e
                )))
            })?;

            let psm_char = characteristics.iter().find(|c| c.uuid() == FIPS_GATT_PSM_CHAR_UUID);
            let psm_char = match psm_char {
                Some(c) => c,
                None => {
                    return Err(TransportError::Io(std::io::Error::other(format!(
                        "discover_gatt_psm: PSM characteristic not found on {}", addr
                    ))));
                }
            };

            let value = psm_char.read().await.map_err(|e| {
                TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: failed to read PSM characteristic on {}: {}", addr, e
                )))
            })?;

            if value.len() != 2 {
                return Err(TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: expected 2-byte PSM value, got {} bytes from {}",
                    value.len(), addr
                ))));
            }

            let psm = u16::from_le_bytes([value[0], value[1]]);

            if psm == 0 {
                return Err(TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: invalid PSM value 0 from {}", addr
                ))));
            }

            debug!(addr = %addr, psm, "GATT PSM discovery: discovered PSM");

            Ok(psm)
        };

        tokio::time::timeout(std::time::Duration::from_secs(10), discover)
            .await
            .map_err(|_| {
                TransportError::Io(std::io::Error::other(format!(
                    "discover_gatt_psm: timed out discovering PSM for {}", addr
                )))
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
    } else { [0, 0, 0, 0, 0, 0] }
}

fn format_uuid(uuid: &uuid::Uuid) -> String {
    uuid.hyphenated().to_string().to_uppercase()
}

impl BleIo for BluestIo {
    type Stream = AnyStream;
    type Acceptor = BluestAcceptor;
    type Scanner = BluestScanner;

    async fn listen(&self, _psm: u16) -> Result<BluestAcceptor, TransportError> {
        if self.peripheral_manager.lock().unwrap().is_some() {
            let (_, inbound_rx) = tokio::sync::mpsc::channel(8);
            return Ok(BluestAcceptor { rx: tokio::sync::Mutex::new(inbound_rx) });
        }

        let (event_tx, new_event_rx) = tokio::sync::mpsc::channel(32);
        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(8);

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

            *self.peripheral_manager.lock().unwrap() = Some(mgr.clone());
            *self.peripheral_delegate.lock().unwrap() = Some(SendableDelegate(delegate));
            *self.inbound_tx.lock().unwrap() = Some(inbound_tx.clone());
            mgr
        };

        *self.event_rx.lock().await = new_event_rx;

        // Wait for PoweredOn or detect sleep/wake cycle
        loop {
            let mut rx = self.event_rx.lock().await;
            match rx.recv().await {
                Some(PeripheralManagerEvent::StateChanged(state)) => {
                    if state == CBManagerState::PoweredOff {
                        warn!("Peripheral manager powered off (sleep)");
                        *self.was_powered_off.lock().await = true;
                        continue;
                    }
                    if state == CBManagerState::PoweredOn {
                        if *self.was_powered_off.lock().await {
                            warn!("Recovering from sleep/wake: Peripheral manager powered on");
                            *self.was_powered_off.lock().await = false;
                            break;
                        }
                        debug!("Peripheral manager powered on");
                        break;
                    }
                    if state == CBManagerState::Unsupported || state == CBManagerState::Unauthorized {
                        return Err(TransportError::StartFailed(format!("Bluetooth not available: state {:?}", state)));
                    }
                }
                Some(_) => {}
                None => return Err(TransportError::StartFailed("Peripheral manager event channel closed".into())),
            }
        }

        // Recovery: Re-publish L2CAP channel
        if *self.was_powered_off.lock().await {
            warn!("Recovering from sleep/wake: re-publishing L2CAP channel");
            *self.was_powered_off.lock().await = false;
            manager.dispatch(|m| unsafe { m.publishL2CAPChannelWithEncryption(false); });

            // Wait for L2CAP published
            loop {
                let mut rx = self.event_rx.lock().await;
                match rx.recv().await {
                    Some(PeripheralManagerEvent::L2CAPPublished { psm }) => {
                        info!("Recovery: L2CAP published with PSM {}", psm);
                        break;
                    }
                    Some(_) => {}
                    None => return Err(TransportError::StartFailed("L2CAP publish event channel closed".into())),
                }
            }

            // Recovery: Re-add GATT service
            warn!("Recovering from sleep/wake: re-adding GATT service");
            let svc_uuid_str = format_uuid(&FIPS_GATT_PSM_SERVICE_UUID);
            let char_uuid_str = format_uuid(&FIPS_GATT_PSM_CHAR_UUID);
            manager.dispatch(move |m| unsafe {
                let svc_uuid = CBUUID::UUIDWithString(&NSString::from_str(&svc_uuid_str));
                let char_uuid = CBUUID::UUIDWithString(&NSString::from_str(&char_uuid_str));
                let psm_char = CBMutableCharacteristic::initWithType_properties_value_permissions(
                    CBMutableCharacteristic::alloc(), &char_uuid,
                    CBCharacteristicProperties::Read, None, CBAttributePermissions::Readable,
                );
                let service = CBMutableService::initWithType_primary(CBMutableService::alloc(), &svc_uuid, true);
                let chars = NSArray::from_retained_slice(&[psm_char]);
                let chars_ptr: &NSArray<objc2_core_bluetooth::CBCharacteristic> =
                    &*(&*chars as *const _ as *const NSArray<objc2_core_bluetooth::CBCharacteristic>);
                service.setCharacteristics(Some(chars_ptr));
                m.addService(&service);
            });

            // Wait for service added
            loop {
                let mut rx = self.event_rx.lock().await;
                match rx.recv().await {
                    Some(PeripheralManagerEvent::ServiceAdded) => {
                        info!("Recovery: GATT service re-added");
                        break;
                    }
                    Some(_) => {}
                    None => return Err(TransportError::StartFailed("Service add event channel closed".into())),
                }
            }
        }

        manager.dispatch(|m| unsafe { m.publishL2CAPChannelWithEncryption(false); });

        // Wait for PSM
        loop {
            let mut rx = self.event_rx.lock().await;
            match rx.recv().await {
                Some(PeripheralManagerEvent::L2CAPPublished { psm }) => { info!("L2CAP published with PSM {}", psm); break; }
                Some(_) => {}
                None => return Err(TransportError::StartFailed("L2CAP publish event channel closed".into())),
            }
        }

        // Create GATT service with PSM characteristic
        let svc_uuid_str = format_uuid(&FIPS_GATT_PSM_SERVICE_UUID);
        let char_uuid_str = format_uuid(&FIPS_GATT_PSM_CHAR_UUID);
        manager.dispatch(move |m| unsafe {
            let svc_uuid = CBUUID::UUIDWithString(&NSString::from_str(&svc_uuid_str));
            let char_uuid = CBUUID::UUIDWithString(&NSString::from_str(&char_uuid_str));
            let psm_char = CBMutableCharacteristic::initWithType_properties_value_permissions(
                CBMutableCharacteristic::alloc(), &char_uuid,
                CBCharacteristicProperties::Read, None, CBAttributePermissions::Readable,
            );
            let service = CBMutableService::initWithType_primary(CBMutableService::alloc(), &svc_uuid, true);
            let chars = NSArray::from_retained_slice(&[psm_char]);
            let chars_ptr: &NSArray<objc2_core_bluetooth::CBCharacteristic> =
                &*(&*chars as *const _ as *const NSArray<objc2_core_bluetooth::CBCharacteristic>);
            service.setCharacteristics(Some(chars_ptr));
            m.addService(&service);
        });

        // Wait for service added
        loop {
            let mut rx = self.event_rx.lock().await;
            match rx.recv().await {
                Some(PeripheralManagerEvent::ServiceAdded) => break,
                Some(_) => {}
                None => return Err(TransportError::StartFailed("Service add event channel closed".into())),
            }
        }

        // Bridge incoming L2CAP channels to acceptor
        let event_rx_bridge = self.event_rx.clone();
        let delegate_arc = self.peripheral_delegate.clone();
        tokio::spawn(async move {
            loop {
                let mut rx = event_rx_bridge.lock().await;
                match rx.recv().await {
                    Some(PeripheralManagerEvent::L2CAPChannelOpened) => {
                        let streams: Vec<SendablePeripheralStream> = {
                            let guard = delegate_arc.lock().unwrap();
                            let Some(d) = guard.as_ref() else { return };
                            let mut pending = d.0.ivars().pending_streams.lock().unwrap();
                            std::mem::take(&mut *pending)
                        };
                        for stream in streams {
                            if inbound_tx.send(AnyStream::Peripheral(stream.0)).await.is_err() { return; }
                        }
                    }
                    Some(_) => {}
                    None => return,
                }
            }
        });

        debug!("BLE listen: macOS peripheral acceptor ready");
        Ok(BluestAcceptor { rx: tokio::sync::Mutex::new(inbound_rx) })
    }

    async fn connect(&self, addr: &BleAddr, psm: u16) -> Result<AnyStream, TransportError> {
        let device = { self.devices.lock().await.get(&addr.device).cloned() };
        let device = device.ok_or_else(|| TransportError::Io(std::io::Error::other(format!("BLE device not found in cache: {addr}"))))?;

        self.adapter.connect_device(&device).await
            .map_err(|e| TransportError::Io(std::io::Error::other(format!("BLE connect {addr}: {e}"))))?;
        let effective_psm = match self.discover_gatt_psm(addr).await {
            Ok(discovered_psm) => {
                debug!(addr = %addr, configured_psm = psm, discovered_psm, "BLE connect: using GATT-discovered PSM");
                discovered_psm
            }
            Err(e) => {
                debug!(addr = %addr, configured_psm = psm, error = %e, "BLE connect: GATT PSM discovery unavailable, using configured PSM");
                psm
            }
        };
        debug!(addr = %addr, psm = effective_psm, "Opening L2CAP channel");

        let channel = device.open_l2cap_channel(effective_psm, false).await
            .map_err(|e| TransportError::Io(std::io::Error::other(format!("L2CAP open {addr} PSM {effective_psm}: {e}"))))?;
        let (reader, mut writer) = channel.split();
        debug!(addr = %addr, psm = effective_psm, "L2CAP channel open");

        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(BLE_CENTRAL_QUEUE_DEPTH);
        let drain_addr = addr.clone();
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if let Err(e) = writer.write(&frame).await {
                    warn!(addr = %drain_addr, error = %e, "BLE central drain task write error, stopping");
                    break;
                }
            }
            debug!(addr = %drain_addr, "BLE central drain task stopped");
        });

        Ok(AnyStream::Central(BluestStream {
            reader: Mutex::new(reader), tx,
            remote: addr.clone(), mtu: self.mtu,
            recv_buf: Mutex::new(Vec::new()),
            rate_limiter: if self.send_rate_bps > 0 {
                Some(Mutex::new(SendRateLimiter::new(self.send_rate_bps, self.send_burst_bytes)))
            } else { None },
        }))
    }

    async fn start_advertising(&self) -> Result<(), TransportError> {
        let guard = self.peripheral_manager.lock().unwrap();
        let manager = match guard.as_ref() {
            Some(m) => m.clone(),
            None => { debug!("BLE advertising: no peripheral manager"); return Ok(()); }
        };
        drop(guard);
        let fips_str = format_uuid(&FIPS_SERVICE_UUID);
        let psm_str = format_uuid(&FIPS_GATT_PSM_SERVICE_UUID);
        manager.dispatch(move |m: &CBPeripheralManager| unsafe {
            let fips_uuid = CBUUID::UUIDWithString(&NSString::from_str(&fips_str));
            let psm_uuid = CBUUID::UUIDWithString(&NSString::from_str(&psm_str));
            let uuids = NSArray::from_retained_slice(&[fips_uuid, psm_uuid]);
            let ad = NSDictionary::from_retained_objects(&[CBAdvertisementDataServiceUUIDsKey], &[uuids.into()]);
            m.startAdvertising(Some(&ad));
        });
        debug!("BLE advertising: started");
        Ok(())
    }

    async fn stop_advertising(&self) -> Result<(), TransportError> {
        let guard = self.peripheral_manager.lock().unwrap();
        if let Some(manager) = guard.as_ref() {
            manager.dispatch(|m: &CBPeripheralManager| unsafe { m.stopAdvertising(); });
            debug!("BLE advertising: stopped");
        }
        Ok(())
    }

    async fn disconnect_device(&self, addr: &BleAddr) {
        let device = { self.devices.lock().await.get(&addr.device).cloned() };
        if let Some(device) = device {
            match self.adapter.disconnect_device(&device).await {
                Ok(()) => debug!(addr = %addr, "Disconnected CoreBluetooth peripheral"),
                Err(e) => debug!(addr = %addr, error = %e, "Failed to disconnect peripheral"),
            }
        }
    }

    async fn start_scanning(&self) -> Result<BluestScanner, TransportError> {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
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
            let mut seen: HashMap<Device, [u8; 6]> = HashMap::new();
            while let Some(discovered) = scan_stream.next().await {
                let device = discovered.device;

                if let Some(&existing) = seen.get(&device) {
                    let addr = BleAddr {
                        adapter: MACOS_ADAPTER_NAME.to_string(),
                        device: existing,
                        rssi: None,
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
                    addr = format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]),
                    "Discovered FIPS BLE device"
                );

                devices.lock().await.insert(bytes, device);

                let addr = BleAddr {
                    adapter: MACOS_ADAPTER_NAME.to_string(),
                    device: bytes,
                    rssi: None,
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
            device: [0, 0, 0, 0, 0, 0],
            rssi: None,
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
