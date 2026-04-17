# Learnings — GATT PSM Exchange Plan

## 2026-04-13 Session Start

### Existing Code Patterns
- FIPS_SERVICE_UUID: `0x9c90_b790_2cc5_42c0_9f87_c9cc_4064_8f4c` (SHA-256("FIPS: welcome to cryptoanarchy") + v4 bits)
- UUID format: `uuid::Uuid::from_u128(0x...)` on macOS, `bluer::Uuid::from_u128(0x...)` on Linux
- PeerCapabilities is a newtype around u8 with bit flags
- linux_default() = 0x3c (L2CAP|CAN_CENTRAL|CAN_PERIPHERAL|PREFER_L2CAP)
- macos_default() = central_only() = 0x2a (L2CAP|CAN_CENTRAL|PREFER_OUTBOUND)
- GATT_SUPPORTED (0x40) already defined but unused in defaults
- macOS listen(), start_advertising(), BluestAcceptor are all stubs
- bluest has NO GATT server support — must use objc2-core-bluetooth directly

## 2026-04-13 PeerCapabilities Defaults Update

### Changes Made
- **linux_default()**: Added `GATT_SUPPORTED` flag (0x40) → new value: 0x7c (was 0x3c)
- **macos_default()**: Changed from `central_only()` to explicit combination with `GATT_SUPPORTED` and `CAN_PERIPHERAL` → new value: 0x7a (was 0x2a)
- **Test updates**: `test_peer_capabilities_defaults_and_queries` updated all assertions:
  - Byte values: 0x3c → 0x7c, 0x2a → 0x7a
  - `supports_gatt()`: `!true` → `true` (both linux and mac)
  - `can_accept_inbound()`: `false` → `true` (mac only)
  - `is_central_only()`: `true` → `false` (mac only)
- **New test added**: `test_gatt_supported_flag_encoding` validates roundtrip encoding/decoding of GATT flag

### Test Results
- All 14 BLE transport tests pass (including new test)
- Full test suite: 1017 passed, 4 ignored, 0 failed (no regressions)
- Backwards compatibility preserved: `from_byte(0x01)` still maps to `central_only()`

### Verification
- `PeerCapabilities::linux_default().supports_gatt()` → true
- `PeerCapabilities::macos_default().supports_gatt()` → true
- `PeerCapabilities::macos_default().can_accept_inbound()` → true
- `PeerCapabilities::macos_default().is_central_only()` → false (now has CAN_PERIPHERAL)

## 2026-04-13 BluerIo GATT PSM Discovery (Linux)

### bluer 0.17 GATT Client API
- `bluer::Device::connect().await` — GATT-level connect (triggers service resolution)
- `bluer::Device::disconnect().await` — GATT disconnect (independent of L2CAP)
- `bluer::Device::services().await` — returns `Vec<gatt::remote::Service>` (waits for resolution)
- `gatt::remote::Service::uuid().await` — returns `Uuid` (property, D-Bus call)
- `gatt::remote::Service::characteristics().await` — returns `Vec<Characteristic>`
- `gatt::remote::Characteristic::uuid().await` — returns `Uuid` (property, D-Bus call)
- `gatt::remote::Characteristic::read().await` — returns `Vec<u8>`
- Service/Characteristic UUID matching requires sequential `.await` per item (no batch filter)
- Full path types used inline: `bluer::gatt::remote::Service`, `bluer::gatt::remote::Characteristic`

### Implementation Pattern
- `discover_gatt_psm(&self, addr)` — public entry point, wraps in 10s timeout
- `read_psm_from_gatt(&self, device, addr)` — core GATT logic, returns PSM
- `find_service_by_uuid` / `find_char_by_uuid` — async helpers for UUID matching
- GATT connect → enumerate services → find by UUID → enumerate chars → read → disconnect
- GATT disconnect happens after read, even on error (non-fatal if disconnect fails)
- PSM parsed as `u16::from_le_bytes`, validated non-zero
- Method lives in `impl BluerIo` (NOT `impl BleIo for BluerIo`)

### Build/Test Results
- `cargo build` — clean (only pre-existing warnings in transport/mod.rs)
- `cargo test` — 1017 passed, 0 failed, 4 ignored (no regressions)
- BLE code compiles only on Linux with `--features ble`; macOS build just needs no breakage

## Task 8: Connection Decision Logic (2026-04-14)

### RPITIT trait default method pattern
- Adding a method with `impl Future<Output = ...> + Send` default body to a trait works cleanly with RPITIT (Rust 2024 edition)
- All existing impls automatically get the default — no changes needed for MockBleIo
- To override with an inherent method, explicitly define the trait method in the `impl BleIo for X` block and delegate to `X::discover_gatt_psm(self, addr)`

### Wiring pattern for inherent→trait delegation
```rust
fn discover_gatt_psm(&self, addr: &BleAddr) -> impl Future<Output = Result<u16, TransportError>> + Send {
    BluerIo::discover_gatt_psm(self, addr)  // call inherent method
}
```
- Using the fully-qualified path `TypeName::method(self, ...)` avoids ambiguity
- Both BluerIo (Linux) and BluestIo (macOS) have identical pattern

### scan_probe_loop integration
- `discover_gatt_psm` is called before L2CAP connect on every probe attempt
- Falls back to configured PSM on any error (service not found, timeout, etc.)
- Uses `trace!` for fallback to avoid log noise (most peers won't support GATT PSM yet)
- Uses `debug!` for successful discovery (noteworthy event)
- No caching — always re-reads PSM per the task requirements

### Test baseline confirmed
- 1017 passed, 0 failed, 4 ignored (matches baseline)
- `cargo build --release --features ble-macos` succeeds with no new warnings

## Task 6: MockBleIo GATT PSM Discovery Support

- Added `gatt_psm_map: std::sync::Mutex<HashMap<[u8; 6], u16>>` to MockBleIo for per-address PSM simulation
- Pattern: same `std::sync::Mutex<Option<T>>` / `std::sync::Mutex<HashMap>` approach as `connect_handler`
- `HashMap` import needed at top level of io.rs (not just in the `bluer_impl` module which is behind `cfg(feature = "ble")`)
- `discover_gatt_psm` override uses `addr.device` (`[u8; 6]`) as HashMap key — `BleAddr` doesn't implement Hash
- Lock → compute result → drop lock → async move result pattern needed because MutexGuard is not Send
- 5 tests added: success, not_found, fallback_fixed, override, set_and_clear
- All 1022 tests pass (5 new + 1017 existing), 4 ignored, 0 failures
