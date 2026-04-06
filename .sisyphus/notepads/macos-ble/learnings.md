# Learnings from Wave 1

## BleDeviceAddr Pattern
- Added `BleDeviceAddr` enum with `Mac([u8; 6])` and `Uuid([u8; 16])` variants
- Changed `BleAddr.device` field from `[u8; 6]` to `BleDeviceAddr`
- UUID format: 8-4-4-4-12 hex chars (standard UUID format)

## Test Helper Updates
- Updated 5 test helper functions to use `BleDeviceAddr::Mac([...])`:
  - `src/node/tests/ble.rs`
  - `src/transport/ble/mod.rs` (tests module)
  - `src/transport/ble/pool.rs` (tests module)
  - `src/transport/ble/discovery.rs` (tests module)
  - `src/transport/ble/io.rs` (tests module)

## Import Pattern
- Files using `BleDeviceAddr` need to import it from `addr` module:
  - `use super::addr::{BleAddr, BleDeviceAddr};` in test files
  - `use super::addr::BleDeviceAddr;` in mod.rs tests module

## UUID Format String
- Fixed format string to output correct 8-4-4-4-12 hex groups
- Format: `{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}`

## All tests pass (938 total)

# Learnings from Hardware Testing (2026-04-06)

## Mac is Central-Only — This is the Hard Constraint

CoreBluetooth does not support the peripheral (acceptor) role at all:
- `listen()` → `NotSupported`
- `start_advertising()` → `NotSupported`
- `accept()` → `NotSupported`

See `src/transport/ble/bluest.rs` lines 1-6. This means:
- Mac can ONLY scan, connect outbound, and open L2CAP channels
- Linux MUST be the peripheral (advertise, accept inbound)
- The connection flow is strictly asymmetric: Mac=initiator, Linux=responder

## Cross-Probe Tie-Breaker is Critical for Mac↔Linux

The tie-breaker in `mod.rs` determines which side's BLE connection survives when both
probe simultaneously:
- `scan_probe_loop`: if `our_addr >= peer_addr` → drop outbound (yield)
- `accept_loop`: if `our_addr < peer_addr` → drop inbound (our outbound wins)

For Mac↔Linux to work correctly, Linux must ALWAYS yield to Mac's outbound probe
(because Mac can only initiate). This requires `linux_node_addr >= mac_node_addr`.

## ESP32 Cross-Probe Creates Handshake Confusion

When two ESP32s probe each other AND Linux simultaneously, both BLE connections
for the same peer can end up in the pool. The node layer detects "Cross-connection
detected: have outbound, received inbound msg1" but both sides still try to initiate
Noise IK independently → msg3 never arrives → handshake hangs.

The 37-byte packets seen every ~10s in logs are Noise IK msg1 retransmissions,
not keepalives — the peer keeps retrying because it never sees msg2.

## L2CAP SeqPacket Does NOT Guarantee Message Boundaries

On some platforms, a single `recv()` call may return a partial pubkey frame (33 bytes).
The old code assumed one recv = one frame and failed with "expected 33 bytes, got N".
The `macos-support` branch has `recv_pubkey_frame()` that loops to reassemble fragments.

## Borrow Checker in encrypted.rs

The AAD logging commit (`6768f55`) introduced a mutable/immutable borrow conflict:
calling `self.peer_display_name(&node_addr)` while `peer` was mutably borrowed.
Fix: compute `peer_display` before the mutable borrow, restructure error path to drop
borrow before calling `self.log_decrypt_failure()` / `self.handle_decrypt_failure()`.
This fix is local-only (uncommitted, dirty tree).

## Mac Never Appeared in Linux Logs

The Mac BLE address `14:7D:DA:7D:4C:31` was never seen in `/tmp/fips-ble.log`.
Only two ESP32 peers were observed. The "Mac drops after pubkey exchange" issue from
an earlier session may have been from a rotated log, or the Mac hasn't been running
with the FIPS node active during our testing.
