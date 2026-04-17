# BLE Reverse Direction Debugging: Linux Central → Mac Peripheral

Date: 2026-04-15

## Goal

Prove BLE reverse direction works: **Linux as central initiating L2CAP → Mac as peripheral accepting L2CAP**.
This demonstrates Mac-to-Mac BLE would work (Linux stands in for a second Mac).
The forward direction (Mac central → Linux peripheral) already works.

## Architecture

```
Linux (central)                         Mac (peripheral)
  scan ──LE Create Connection──►  advertise
  GATT connect ─────────────────►  GATT server
  L2CAP socket connect (PSM) ───►  L2CAP publish (PSM)
  Noise IK handshake ───────────►  Noise IK responder
  FMP frames ◄──────────────────►  FMP frames
```

## Hypotheses Tested (H1–H14)

### H1: ECRED mode + LeRandom address
- **Tried**: Use L2CAP ECRED (enhanced credit) mode with LeRandom target
- **Result**: No improvement. ECRED not widely supported.

### H2–H3: Various HCI parameter combinations
- **Tried**: Different HCI parameters for connection
- **Result**: Connection failures

### H4: LeRandom target with LE_FLOWCTL (revert ECRED)
- **Tried**: Standard LE flow control mode with random address
- **Result**: Progress but still no L2CAP

### H5–H6: GATT-first connect, PSM discovery
- **Tried**: Connect GATT first, discover PSM, then open L2CAP
- **Result**: GATT connect goes over BR/EDR for LePublic addresses

### H7–H8: Force LE transport
- **Tried**: `adapter.connect_device()` with LE-specific params
- **Result**: BlueZ `device.connect()` sends BR/EDR Create Connection for LePublic addresses

### H9: AddressType setting
- **Tried**: Set AddressType on device before connect
- **Result**: Partial — helps but doesn't solve BR/EDR fallback

### H10–H12: HCI-level address type fixes
- **Tried**: Various low-level fixes for address type handling
- **Result**: Incremental progress

### H13: Direct L2CAP socket connect with FlowControl::Le ✅
- **Tried**: Skip GATT entirely. Create L2CAP socket with `FlowControl::Le` mode set, connect directly to PSM
- **Result**: Kernel creates LE ACL via HCI LE Create Connection. **This works for establishing the LE link.**
- **Commit**: `0ace30d`

### H14: L2CAP without encryption ❌
- **Tried**: Publish L2CAP on Mac without encryption requirement
- **Result**: CoreBluetooth accepts the channel then immediately sends L2CAP Disconnection Request 2µs after Connection Response. **Encryption is required** for functional L2CAP CoC on macOS.
- **Commit**: `5720547`

### Revert: Re-enable encryption — SMP pairing blocker 🔴
- **Tried**: Re-enable encryption on Mac side
- **Result**: SMP pairing fails. BlueZ kernel adds `SMP_DIST_LINK_KEY` to key distribution because the adapter is dual-mode (BR/EDR + LE) with SSP support. CoreBluetooth rejects Pairing Request with reason 0x08 (Unspecified).
- **Root cause**: CTKD (Cross-Transport Key Derivation) LinkKey bits. BlueZ kernel auto-adds them for dual-mode adapters. Cannot be disabled via `main.conf`.
- **Commit**: `5be5c05`

## Current Blocker: SMP Pairing CTKD LinkKey Bits

### The Problem
```
Linux (dual-mode adapter, BR/EDR + LE)
  └─ BlueZ kernel sees dual-mode adapter
     └─ Adds SMP_DIST_LINK_KEY to SMP key distribution
        └─ CoreBluetooth rejects Pairing Request (reason 0x08)
           └─ L2CAP encryption fails
              └─ No L2CAP CoC
```

### Why Dual-Mode Matters
- Linux BT adapter (hci0, BD `14:5A:FC:49:C2:24`) is dual-mode: supports both BR/EDR and LE
- For dual-mode adapters with SSP (Secure Simple Pairing), BlueZ kernel automatically sets CTKD bits
- This tells the peer "I want to derive a BR/EDR LinkKey from this LE pairing"
- CoreBluetooth (macOS) rejects this — likely because the Mac has no BR/EDR pairing context for this device

### Proposed Fix: Force LE-Only Mode
If we make the Linux adapter appear as **LE-only** (not dual-mode), BlueZ won't add CTKD LinkKey bits.

Options:
1. **Disable BR/EDR on adapter** (`hciconfig hci0 noscan` for BR/EDR, or `btmgmt`)
2. **Use LE random address** instead of public address
3. **Kernel patch** to clear CTKD bits (too invasive)

## H15 — Force LE-Only on Linux (IMPLEMENTED)

### Strategy
1. Add `le_only: true` config option to BLE transport config
2. When enabled, `BluerIo::new()` runs `btmgmt bredr off <adapter>` before powering on
3. This makes the kernel treat the adapter as LE-only, preventing CTKD LinkKey bits
4. SMP pairing should succeed → L2CAP encryption → L2CAP CoC → FIPS session

### Code Changes
- `src/config/transport.rs`: Added `le_only: Option<bool>` field to `BleConfig`
- `src/transport/ble/io.rs`: `BluerIo::new()` takes `le_only` param, runs `btmgmt bredr off`
- `src/node/mod.rs`: Passes `le_only` from config to `BluerIo::new()`

### Linux Config (when node comes back up)
```yaml
transports:
  ble:
    adapter: "hci0"
    psm: 133
    le_only: true
    scan: true
    advertise: false
    accept_connections: false
    auto_connect: true
```

### Mac Config (peripheral, accepting)
```yaml
transports:
  ble:
    adapter: "default"
    psm: 133
    scan: false
    advertise: true
    accept_connections: true
    auto_connect: false
```
