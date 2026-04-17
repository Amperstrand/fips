# objc2-core-bluetooth API Research Summary

**Task**: macOS BLE Peripheral Role Implementation
**Version**: objc2-core-bluetooth 0.3.2
**Date**: 2026-04-13
**Status**: Research Only - No Implementation Code

---

## Table of Contents

1. [Overview](#overview)
2. [CBPeripheralManager](#cbperipheralmanager)
3. [L2CAP Channel Publishing](#l2cap-channel-publishing)
4. [GATT Service & Characteristic](#gatt-service--characteristic)
5. [Advertising](#advertising)
6. [Incoming L2CAP Connections](#incoming-l2cap-connections)
7. [Thread Safety & Gotchas](#thread-safety--gotchas)
8. [Memory Management](#memory-management)
9. [Run Loop Requirements](#run-loop-requirements)

---

## Overview

This research documents the objc2-core-bluetooth crate APIs needed to implement the macOS BLE peripheral role (CBPeripheralManager). The crate provides safe Rust bindings to Apple's CoreBluetooth framework.

**Key Findings:**
- objc2-core-bluetooth version 0.3.2 is available as a transitive dependency
- bluest wraps CoreBluetooth for the central role only (no GATT server support)
- Direct objc2 usage is required for peripheral implementation
- CoreBluetooth delegate callbacks arrive on the main thread with CFRunLoop
- Memory management uses ARC through objc2's `Retained<T>` smart pointers

---

## CBPeripheralManager

### Creation & Initialization

**Two available init methods:**

1. **`initWithDelegate_queue_options`** (Recommended)
   - **Signature**: `fn initWithDelegate_queue_options(
       this: Allocated<Self>,
       delegate: Option<&ProtocolObject<dyn CBPeripheralManagerDelegate>>,
       queue: Option<&DispatchQueue>,
       options: Option<&NSDictionary<NSString, AnyObject>>,
   ) -> Retained<Self>`

2. **`initWithDelegate_queue`**
   - **Signature**: `fn initWithDelegate_queue(
       this: Allocated<Self>,
       delegate: Option<&ProtocolObject<dyn CBPeripheralManagerDelegate>>,
       queue: Option<&DispatchQueue>,
   ) -> Retained<Self>`

**Parameters:**
- `delegate`: The delegate that will receive peripheral role events (weak property)
- `queue`: Dispatch queue for events (nil = main queue)
- `options`: Optional dictionary for manager options

**Important Notes:**
- Delegate is a **weak property** - must ensure delegate object lives long enough
- If queue is nil, the main queue is used
- The FIPS binary already runs `CFRunLoopRun()` on the main thread

### State Management

**Required delegate method:**
```rust
unsafe fn peripheralManagerDidUpdateState(
    &self,
    peripheral: &CBPeripheralManager,
)
```

**State transitions:**
1. `CBManagerStateUnknown` → initial state
2. `CBManagerStateResetting` → Bluetooth hardware resetting
3. `CBManagerStateUnsupported` → Hardware doesn't support BLE
4. `CBManagerStateUnauthorized` → Bluetooth access denied
5. `CBManagerStatePoweredOn` → **Only call commands when in this state**

**Command restrictions:**
- All peripheral commands should only be issued when state is `PoweredOn`
- If state moves below `PoweredOn`, advertisement pauses and connected centrals disconnect
- If state becomes `PoweredOff`, must restart advertisement and re-add all services

### Core Properties

```rust
// Weak delegate property
delegate(&self) -> Option<Retained<ProtocolObject<dyn CBPeripheralManagerDelegate>>>

// Weak setter
setDelegate(&self, delegate: Option<&ProtocolObject<dyn CBPeripheralManagerDelegate>>)

// State query
state(&self) -> CBManagerState

// Current authorization status
authorization(&self) -> CBManagerAuthorization

// Whether currently advertising
isAdvertising(&self) -> bool
```

---

## L2CAP Channel Publishing

### Publishing L2CAP Channel

**Method:**
```rust
pub unsafe fn publishL2CAPChannelWithEncryption(
    &self,
    encryption_required: bool,
)
```

**Parameters:**
- `encryption_required`: `true` if link must be encrypted, `false` if unsecured link allowed

**Behavior:**
- System determines an unused PSM (Protocol/Service Multiplexer)
- PSM is returned via delegate callback (not assigned beforehand)
- L2CAP channels are **not discoverable** by themselves
- Application must handle PSM discovery on the client side

**Delegate callback for published channel:**
```rust
unsafe fn peripheralManager_didPublishL2CAPChannel_error(
    &self,
    peripheral: &CBPeripheralManager,
    psm: CBL2CAPPSM,
    error: Option<&NSError>,
)
```

### PSM Type

**Type definition:**
```rust
pub type CBL2CAPPSM = u16;
```

**Apple documentation:** `CBL2CAPPSM` is the 16-bit LE PSM value assigned by the system

### Unpublishing

**Method:**
```rust
pub unsafe fn unpublishL2CAPChannel(
    &self,
    psm: CBL2CAPPSM,
)
```

**Behavior:**
- Removes the published PSM from the system
- No new connections for this PSM will be accepted
- Any existing L2CAP channels using this PSM will be closed

---

## GATT Service & Characteristic

### CBMutableService Creation

**Primary constructor:**
```rust
pub unsafe fn initWithType_primary(
    this: Allocated<Self>,
    uuid: &CBUUID,
    is_primary: bool,
) -> Retained<Self>
```

**Parameters:**
- `uuid`: Bluetooth UUID of the service (16-bit, 32-bit, or 128-bit)
- `is_primary`: `true` for primary service, `false` for secondary

**Methods to set characteristics:**
```rust
// Get existing characteristics
characteristics(&self) -> Option<Retained<NSArray<CBCharacteristic>>>

// Set characteristics (must be done before adding service)
setCharacteristics(
    &self,
    characteristics: Option<&NSArray<CBCharacteristic>>,
)
```

**Methods to set included services:**
```rust
includedServices(&self) -> Option<Retained<NSArray<CBService>>>

setIncludedServices(
    &self,
    included_services: Option<&NSArray<CBService>>,
)
```

### CBMutableCharacteristic Creation

**Primary constructor:**
```rust
pub unsafe fn initWithType_properties_value_permissions(
    this: Allocated<Self>,
    uuid: &CBUUID,
    properties: CBCharacteristicProperties,
    value: Option<&NSData>,
    permissions: CBAttributePermissions,
) -> Retained<Self>
```

**Parameters:**
- `uuid`: Bluetooth UUID of the characteristic
- `properties`: Characteristic properties (bitmask)
- `value`: Cached value or `None` for dynamic values
- `permissions`: Read/write permissions

**Example usage:**
```rust
use objc2_core_bluetooth::{
    CBMutableCharacteristic, CBUUID, CBCharacteristicProperties,
    CBAttributePermissions, NSData,
};
use objc2_foundation::NSData;

// For FIPS: Create a characteristic with read permission
let uuid = CBUUID::from_u16(0x250c); // FIPS GATT PSM Exchange Characteristic
let properties = CBCharacteristicProperties::READ; // Only readable
let value = None; // Dynamic value - will be requested on-demand
let permissions = CBAttributePermissions::READABLE;

let characteristic = unsafe {
    CBMutableCharacteristic::initWithType_properties_value_permissions(
        Allocated::new().unwrap(),
        &uuid,
        properties,
        value,
        permissions,
    )
};
```

### Adding Services to Database

**Method:**
```rust
pub unsafe fn addService(&self, service: &CBMutableService)
```

**Delegate callback:**
```rust
unsafe fn peripheralManager_didAddService_error(
    &self,
    peripheral: &CBPeripheralManager,
    service: &CBService,
    error: Option<&NSError>,
)
```

**Important Notes:**
- Service is cached in the database after adding
- Published service can no longer be changed
- If service contains included services, they must be added first
- All centrals can now access this service

### Handling Read Requests

**Delegate method for read requests:**
```rust
unsafe fn peripheralManager_didReceiveReadRequest(
    &self,
    peripheral: &CBPeripheralManager,
    request: &CBATTRequest,
)
```

**Parameters:**
- `request`: The ATT read request object

**To respond:**
```rust
pub unsafe fn respondToRequest_withResult(
    &self,
    request: &CBATTRequest,
    result: CBATTError,
)
```

**Parameters:**
- `request`: The original request
- `result`: Success or error result

### Handling Write Requests

**Delegate method for write requests:**
```rust
unsafe fn peripheralManager_didReceiveWriteRequests(
    &self,
    peripheral: &CBPeripheralManager,
    requests: &NSArray<CBATTRequest>,
)
```

**Parameters:**
- `requests`: List of one or more ATT request/command objects

**Important:**
- Must call `respondToRequest_withResult` exactly once per invocation
- If multiple requests are present, they must be treated as an atomic unit
- If one request would cause failure, none should execute and error should be returned

---

## Advertising

### Starting Advertising

**Method:**
```rust
pub unsafe fn startAdvertising(
    &self,
    advertisement_data: Option<&NSDictionary<NSString, AnyObject>>,
)
```

**Parameters:**
- `advertisement_data`: Dictionary containing advertising data

**Supported keys (from objc2-core-bluetooth):**
- `CBAdvertisementDataLocalNameKey`: Local name (NSString)
- `CBAdvertisementDataServiceUUIDsKey`: Array of service UUIDs (NSArray<CBUUID>)

**Apple documentation limits:**
- Foreground: 28 bytes in initial advertisement data
- Foreground: 10 bytes in scan response (local name only)
- Background: No local name used, all service UUIDs in "overflow" area
- Background: Apps not in "bluetooth-peripheral" background mode cannot advertise

**Delegate callback:**
```rust
unsafe fn peripheralManagerDidStartAdvertising_error(
    &self,
    peripheral: &CBPeripheralManager,
    error: Option<&NSError>,
)
```

**Important:**
- This method returns the result of `startAdvertising`
- If advertising could not be started, the error is detailed in the error parameter
- Use `isAdvertising()` to check current advertising state

### Stopping Advertising

**Method:**
```rust
pub unsafe fn stopAdvertising(&self)
```

---

## Incoming L2CAP Connections

### Callback for Incoming Connections

**Delegate method:**
```rust
unsafe fn peripheralManager_didOpenL2CAPChannel_error(
    &self,
    peripheral: &CBPeripheralManager,
    channel: Option<&CBL2CAPChannel>,
    error: Option<&NSError>,
)
```

**Parameters:**
- `channel`: A `CBL2CAPChannel` object representing the live connection
- `error`: If an error occurred

### CBL2CAPChannel Properties

**Methods to access channel information:**

```rust
// Get the peer (remote central)
peer(&self) -> Option<Retained<CBCentral>>

// Get PSM of this channel
PSM(&self) -> CBL2CAPPSM

// Get input stream for reading from remote peer
inputStream(&self) -> Option<Retained<NSInputStream>>

// Get output stream for writing to remote peer
outputStream(&self) -> Option<Retained<NSOutputStream>>
```

**Stream types:**
- `NSInputStream` / `NSOutputStream` from objc2-foundation
- These are CFReadStream / CFWriteStream under the hood
- Byte streams may coalesce or fragment L2CAP SDUs

### Bridging Streams to Rust

**For FIPS implementation:**
1. Create a `tokio::sync::Mutex` wrapper around `NSInputStream`
2. Use `NSInputStream` methods to read bytes:
   ```rust
   // Read into a byte buffer
   let mut buffer = vec![0u8; 4096];
   stream.read(&mut buffer)?;
   ```
3. For asynchronous I/O, spawn a tokio task that reads from the NSInputStream
4. The FIPS binary already has a running CFRunLoop on the main thread
5. Stream reading should happen on the main thread due to NSStream requirements

---

## Thread Safety & Gotchas

### Thread Safety Rules

**CoreBluetooth limitations:**
- CoreBluetooth is **main-thread only**
- All delegate callbacks arrive on the main thread
- NSInputStream/NSOutputStream methods must be called on the main thread
- CFRunLoopRun() is already running on the main thread in the FIPS binary

**objc2-core-bluetooth thread safety:**
- Objects marked with `AnyThread` are safe from any thread
- Most CoreBluetooth objects are `AnyThread` (check ClassType::ThreadKind)
- However, NSInputStream/NSOutputStream require main thread access

### Gotchas

1. **Delegate retain cycle**
   - Delegate is a **weak property**
   - Ensure the Rust object implementing `CBPeripheralManagerDelegate` lives long enough
   - The delegate needs to keep a strong reference to the PeripheralManager instance

2. **Main thread requirement**
   - All CoreBluetooth delegate callbacks are main-thread only
   - All NSInputStream/NSOutputStream operations must be main-thread
   - **Do NOT** use `tokio::spawn` to run CoreBluetooth code on background threads
   - If you need async behavior, bridge to tokio from the main thread

3. **State transitions**
   - Never call commands when state is not `PoweredOn`
   - Listen for `peripheralManagerDidUpdateState` and buffer commands
   - When state becomes `PoweredOn`, execute buffered commands

4. **Service publishing order**
   - Must add included services before adding services that include them
   - Services are cached after adding and cannot be modified

5. **Characteristic values**
   - If value is specified at creation, it's cached and read-only
   - If value is `None`, it's dynamic and will be requested on-demand
   - For FIPS PSM exchange characteristic: use `None` (dynamic) since PSM changes

6. **Advertising data limits**
   - Respect the 28-byte / 10-byte limits
   - Service UUIDs that don't fit go to overflow area (only discovered with explicit scanning)
   - Background apps have even stricter limits

7. **L2CAP PSM discovery**
   - System assigns PSM at publish time (not predetermined)
   - Client must discover PSM through GATT characteristic
   - L2CAP channel is not discoverable by itself

---

## Memory Management

### Retained Objects

**objc2 uses ARC (Automatic Reference Counting):**

- All CoreBluetooth objects are managed by `Retained<T>`
- `Retained` implements `Deref` to the inner type
- When `Retained` goes out of scope, ARC decrements the reference count
- Object is deallocated when reference count reaches zero

**Creating objects:**
```rust
// Using constructors that return Retained
let service = unsafe {
    CBMutableService::initWithType_primary(
        Allocated::new().unwrap(),
        &uuid,
        true,
    )
};

// From Rust byte array
use objc2_foundation::NSData;
let data = NSData::with_bytes(&[0x01, 0x02, 0x03]);
```

### Weak Properties

**`CBPeripheralManager.delegate` is a weak property:**
- Weak means it doesn't keep the delegate alive
- If delegate is deallocated, `setDelegate(None)` is called automatically
- You must ensure your delegate object lives as long as the PeripheralManager

**Solution:**
```rust
struct PeripheralDelegate;
unsafe impl CBPeripheralManagerDelegate for PeripheralDelegate {}

// Create delegate object first
let delegate = Retained::new(PeripheralDelegate);

// Create manager with delegate (transitively retains delegate via ProtocolObject)
let manager = unsafe {
    CBPeripheralManager::initWithDelegate_queue_options(
        Allocated::new().unwrap(),
        Some(&ProtocolObject::new(delegate)),
        Some(&main_queue),
        None,
    )
};

// Now manager.delegate() returns Some(&delegate)
```

### Protocol Objects

**`ProtocolObject<T>` wraps a delegate trait object:**
```rust
use objc2::runtime::ProtocolObject;

let delegate: Retained<dyn CBPeripheralManagerDelegate> =
    Retained::new(PeripheralDelegate);
let delegate_obj = ProtocolObject::new(delegate);

// Pass to CBPeripheralManager
manager.setDelegate(Some(&delegate_obj));
```

---

## Run Loop Requirements

### FIPS Binary State

**Already running CFRunLoopRun() on main thread** (as per context):
- This is perfect for CoreBluetooth
- No additional run loop setup needed

### NSStream Requirements

**NSInputStream/NSOutputStream behavior:**
- These are NSStreams (Objective-C objects)
- They typically run on the main thread's run loop
- Reading/writing should be done from the main thread

**Suggested pattern:**
```rust
// On main thread, create streams
let (input_stream, output_stream) = channel.split();

// Spawn tokio task to read from stream
tokio::spawn(async move {
    let mut buffer = vec![0u8; 4096];
    loop {
        // Read from NSInputStream
        let n = input_stream.read(&mut buffer).await?;
        if n == 0 { break; }
        // Process bytes...
    }
});
```

**Important:** The tokio task reads from NSInputStream, but the NSInputStream methods are called from the main thread. Use proper synchronization (Mutex, channels) to coordinate between threads.

---

## Code Snippets: objc2 Calling Patterns

### 1. Creating a PeripheralManager

```rust
use objc2_core_bluetooth::{
    CBPeripheralManager, CBPeripheralManagerDelegate, CBL2CAPPSM,
};
use objc2_foundation::{NSArray, NSDictionary, NSString};
use objc2::runtime::ProtocolObject;
use objc2_dispatch::DispatchQueue;

#[derive(Debug)]
struct MyPeripheralDelegate;

unsafe impl CBPeripheralManagerDelegate for MyPeripheralDelegate {
    unsafe fn peripheralManagerDidUpdateState(
        &self,
        peripheral: &CBPeripheralManager,
    ) {
        // Handle state changes
    }

    unsafe fn peripheralManager_didPublishL2CAPChannel_error(
        &self,
        peripheral: &CBPeripheralManager,
        psm: CBL2CAPPSM,
        error: Option<&objc2_foundation::NSError>,
    ) {
        // PSM received
    }

    unsafe fn peripheralManager_didOpenL2CAPChannel_error(
        &self,
        peripheral: &CBPeripheralManager,
        channel: Option<&objc2_core_bluetooth::CBL2CAPChannel>,
        error: Option<&objc2_foundation::NSError>,
    ) {
        // Connection established
        if let Some(channel) = channel {
            let psm = channel.PSM();
            println!("L2CAP channel opened with PSM: {}", psm);
        }
    }

    unsafe fn peripheralManager_didReceiveReadRequest(
        &self,
        peripheral: &CBPeripheralManager,
        request: &objc2_core_bluetooth::CBATTRequest,
    ) {
        // Handle read request
    }

    unsafe fn peripheralManager_didReceiveWriteRequests(
        &self,
        peripheral: &CBPeripheralManager,
        requests: &objc2_foundation::NSArray<objc2_core_bluetooth::CBATTRequest>,
    ) {
        // Handle write request
    }
}

// Create delegate and manager
let delegate = Retained::new(MyPeripheralDelegate);
let delegate_obj = ProtocolObject::new(delegate);

let main_queue = DispatchQueue::main();
let manager = unsafe {
    CBPeripheralManager::initWithDelegate_queue_options(
        Allocated::new().unwrap(),
        Some(&delegate_obj),
        Some(&main_queue),
        None,
    )
};
```

### 2. Creating GATT Service and Characteristic

```rust
use objc2_core_bluetooth::{
    CBMutableService, CBMutableCharacteristic, CBUUID,
    CBCharacteristicProperties, CBAttributePermissions,
};
use objc2_foundation::{NSArray, NSData};

// FIPS service UUID
const FIPS_SERVICE_UUID: u128 = 0x9c90_b790_2cc5_42c0_9f87_c9cc_4064_8f4c;
let service_uuid = CBUUID::from_u128(FIPS_SERVICE_UUID);

// Create service (primary)
let service = unsafe {
    CBMutableService::initWithType_primary(
        Allocated::new().unwrap(),
        &service_uuid,
        true, // is_primary
    )
};

// Create PSM exchange characteristic (dynamic value)
const PSM_CHAR_UUID: u128 = 0x250c_88dd_3dff_4c41_83b2_f1b4_e3d8_20cc;
let psm_char_uuid = CBUUID::from_u128(PSM_CHAR_UUID);

let psm_char = unsafe {
    CBMutableCharacteristic::initWithType_properties_value_permissions(
        Allocated::new().unwrap(),
        &psm_char_uuid,
        CBCharacteristicProperties::READ, // Read-only
        None, // Dynamic value
        CBAttributePermissions::READABLE,
    )
};

// Add characteristic to service
service.setCharacteristics(Some(&NSArray::from_slice(&[psm_char])));
```

### 3. Publishing Service and Starting Advertising

```rust
// Add service to manager
manager.addService(&service);

// Create advertising data
use objc2_foundation::{NSString, NSData};
let service_uuid_data = NSData::with_bytes(&service_uuid.to_le_bytes());

let advertising_data = NSDictionary::from_entries(&[
    (
        NSString::from_str("CBAdvertisementDataServiceUUIDsKey"),
        NSArray::from_slice(&[service_uuid]),
    ),
]);

// Start advertising
manager.startAdvertising(Some(&advertising_data));
```

### 4. Publishing L2CAP Channel

```rust
// Publish L2CAP channel (encryption required)
manager.publishL2CAPChannelWithEncryption(true);
```

### 5. Handling Read Requests

```rust
unsafe impl CBPeripheralManagerDelegate for MyPeripheralDelegate {
    unsafe fn peripheralManager_didReceiveReadRequest(
        &self,
        peripheral: &CBPeripheralManager,
        request: &CBATTRequest,
    ) {
        // Get characteristic value from Rust state
        let value = get_psm_value();

        // Respond with success and value
        use objc2_core_bluetooth::CBATTError;
        use objc2_foundation::NSData;

        let value_data = NSData::with_bytes(&value);
        request.setValue(Some(&value_data));
        peripheral.respondToRequest_withResult(request, CBATTError::Success);
    }
}
```

### 6. Bridging NSInputStream to Tokio

```rust
use tokio::sync::Mutex;
use objc2_foundation::NSInputStream;
use std::sync::Arc;

struct StreamReader {
    input_stream: Arc<Mutex<NSInputStream>>,
    buffer: Vec<u8>,
}

impl StreamReader {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        // Acquire lock on NSInputStream
        let mut stream = self.input_stream.lock().await;

        // Read from stream (main thread required)
        let n = stream.read(buf).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::Other, e)
        })?;

        Ok(n)
    }
}
```

---

## Summary Checklist for Implementation

### Before Starting Implementation

- [ ] Create a Rust struct implementing `CBPeripheralManagerDelegate`
- [ ] Wrap in `Retained<T>` and `ProtocolObject<T>` pattern
- [ ] Create `CBMutableService` with FIPS UUID
- [ ] Create `CBMutableCharacteristic` for PSM exchange (dynamic value)
- [ ] Set up main thread CFRunLoop (already present in FIPS)
- [ ] Create `CBPeripheralManager` with delegate

### During Initialization

- [ ] Wait for `CBManagerStatePoweredOn` in delegate
- [ ] Add service to manager
- [ ] Start advertising with FIPS UUID
- [ ] Publish L2CAP channel

### During Operation

- [ ] Handle `peripheralManager_didPublishL2CAPChannel_error` to get PSM
- [ ] Write PSM to characteristic when published
- [ ] Handle `peripheralManager_didReceiveReadRequest` for PSM reads
- [ ] Handle `peripheralManager_didReceiveWriteRequests` for connections
- [ ] Handle `peripheralManager_didOpenL2CAPChannel_error` to get streams
- [ ] Bridge NSInputStream/NSOutputStream to tokio for async I/O

### Important Constraints

- [ ] All CoreBluetooth operations on main thread
- [ ] Delegate must stay alive as long as PeripheralManager
- [ ] Services cannot be modified after publishing
- [ ] Respect advertising data size limits
- [ ] Handle state transitions properly

---

## References

- **Crate Version**: objc2-core-bluetooth 0.3.2
- **Documentation**: https://docs.rs/objc2-core-bluetooth/0.3.2
- **objc2 Documentation**: https://docs.rs/objc2/
- **objc2-foundation**: https://docs.rs/objc2-foundation/0.3.2
- **Apple CoreBluetooth Docs**: https://developer.apple.com/documentation/corebluetooth/
- **bluest source** (for context on bluest usage): /Users/macbook/src/fips/src/transport/ble/io_macos.rs
