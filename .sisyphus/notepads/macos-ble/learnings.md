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
