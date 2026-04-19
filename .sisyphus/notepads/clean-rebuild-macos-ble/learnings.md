# Mac BLE Peripheral Branch Setup

## Summary
Successfully created `macos-ble-peripheral` branch by merging `upstream/master` into `upstream/macos-ble-rebased`, added benchmark feature, and verified builds on both Mac and Linux.

## Key Findings

### 1. Dependency Issue: bluest-patched vs bluest
**Problem**: The `upstream/macos-ble-rebased` branch uses a local path dependency on `../bluest-patched` which doesn't exist on the current repo.

**Resolution**: Changed to use the regular `bluest` crate from crates.io (version 0.6) with the `l2cap` and `unstable` features.

**Files Modified**:
- `Cargo.toml`: Changed `bluest = { path = "../bluest-patched", ... }` to `bluest = { version = "0.6", features = ["l2cap", "unstable"], optional = true }`

### 2. Merge Conflicts
**Files with conflicts**:
1. `Cargo.toml` - macOS/Windows dependencies section
2. `Cargo.lock` - Dependency list (had merge conflict markers)
3. `.github/workflows/ci.yml` - Build matrix configuration
4. `src/bin/fips.rs` - Main function structure

**Resolution Strategy**:
- Kept macOS-specific BLE dependencies from `upstream/macos-ble-rebased`
- Kept Windows dependencies from `upstream/master`
- Used upstream's more maintainable `run_daemon()` structure for `src/bin/fips.rs`
- Preserved BLE-specific transport files (to be done in separate work)

### 3. Benchmark Feature
Successfully added `benchmark = []` to the `[features]` section in `Cargo.toml`.

### 4. Build Verification
**Mac Build**:
- Command: `cargo build --release --features "ble-macos benchmark"`
- Output: 3 binaries generated (7.4M fips, 1.3M fipsctl, 2.0M fipstop)
- Status: ✅ Success

**Linux Build**:
- Command: `CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble`
- Status: ✅ Success
- Output: 12M fips, 1.9M fipsctl, 2.6M fipstop

### 5. Branch Structure
```
macos-ble-peripheral (tracking upstream/macos-ble-rebased)
├── Merge commit: "Merge upstream/master for ACL and platform improvements"
└── Commit: "Fix bluest dependency path and add benchmark feature"
```

## Next Steps (Not Done)
- Verify BLE transport files (mod.rs, io.rs, io_macos.rs) are correctly based on macos-ble-rebased
- Test BLE functionality on macOS
- Consider removing or resolving experimental branches

## Linux BLE Transport Hardening (commit 19a5602)

### Changes Made (5 files, +885/-288 lines)

1. **`src/transport/ble/backoff.rs`** (NEW): `PeerBackoff` struct with per-address exponential backoff.
   - 5s base, 300s max, auto-deny after 5 consecutive failures for 1 hour
   - Deterministic jitter via FNV-like hash of device address bytes
   - `clear()` on successful connection resets all state

2. **`src/transport/ble/io.rs`**: Major rework of `bluer_impl` module.
   - `listen()`: auto-detects local adapter address type (LePublic for USB dongles, LeRandom for built-in)
   - `connect()`: GATT-first connection with retry (handles abort-by-local), PSM discovery via GATT characteristic, falls back to configured PSM
   - `disconnect_device()`: calls BlueZ Device1.Disconnect() for clean device teardown
   - `discover_gatt_psm()`: connects GATT, reads FIPS GATT PSM service/characteristic, returns dynamic PSM
   - Agent registration for BLE pairing, pairable timeout configuration
   - Startup retry logic for advertising and scanning (transient BlueZ errors)
   - `FlowControl::Le` and `set_power_forced_active` on all sockets
   - 0-byte frame rejection in BluerStream::recv (protocol error, not connection close)
   - `BleIo` trait extended: `disconnect_device` (default no-op), `discover_gatt_psm` (default error)

3. **`src/transport/ble/mod.rs`**: Scanner supervisor and backoff wiring.
   - `scan_probe_supervisor()`: wraps scan_probe_loop, auto-restarts with exponential backoff on scanner termination (handles bluetoothd restarts)
   - Backoff wired into accept_loop: denied peers dropped, failures recorded on pubkey exchange failure, cleared on success
   - Backoff wired into scan_probe_loop: denied/in-backoff peers skipped, failures recorded on connect/timeout/pubkey failure, cleared on success
   - receive_loop: 0-byte "framed message too short" errors continue loop (don't break connection)
   - `BleTransport` now has `backoff: Arc<Mutex<PeerBackoff>>` field
   - All BleConnection constructions include `on_drop` callback for disconnect_device

4. **`src/transport/ble/pool.rs`**: Connection cleanup and MTU accounting.
   - `on_drop: Option<Box<dyn FnOnce() + Send>>` on BleConnection for cleanup callbacks
   - `BLE_FRAME_PREFIX_LEN = 2` constant, subtracted from effective_mtu
   - Drop impl calls on_drop before aborting recv_task

5. **`src/transport/ble/addr.rs`**: `to_socket_addr()` changed from `LePublic` to `LeRandom`

### Key Design Decisions
- `disconnect_device` has default no-op impl on BleIo trait → io_macos.rs unchanged
- `discover_gatt_psm` has default error impl → io_macos.rs unchanged
- BleStream trait unchanged → no rate limiting additions
- BleAddr unchanged (no rssi field) → minimal diff from upstream
- Scanner supervisor uses exponential restart backoff (2s→60s) with reset on success

## Task 5: Adaptive Rate Control (2026-04-19)

### Changes Made
- Created `src/transport/ble/rate_limit.rs` with `SendRateLimiter` (token bucket) and `BleRateAdapter` (AIMD with SRTT feedback)
- Added `set_rate_bps()` to `BleStream` trait with default no-op impl
- Added `rate_limiter` field to both `BluerStream` and `BluestStream`, initialized from config
- Rate limiter acquires tokens in `send()` before writing framed data to socket
- `BleRateAdapter` added to `BleTransport` with `update_rate_from_srtt()` method
- `BleConfig` gained `send_rate_bps`, `send_burst_bytes`, `effective_send_rate_bps()`
- `BluerIo::new()` and `BluestIo::new()` now accept `send_rate_bps` + `send_burst_bytes` params

### Key Design Decisions
- Rate limiter is per-stream (not per-transport) — each connection has its own token bucket
- `send_rate_bps = 0` means unlimited (rate_limiter is `None`)
- `effective_send_rate_bps()` maps explicit `0` to 150kbps for the AIMD adapter ceiling
- Token bucket uses `framed_len = 2 + data.len()` (includes 2-byte BE prefix)
- AIMD constants tuned from real BLE experiments: RTT_LOW=200ms, RTT_HIGH=500ms, MIN=15kbps, MAX=80kbps

### Gotchas
- `BleConfig` has `#[serde(deny_unknown_fields)]` — new fields MUST be `Option<T>` with `#[serde(default)]`
- `BluerStream` and `BluestStream` both use `tokio::sync::Mutex` (not std) — required for `.await` in send path
- `BleAddr::from_bluer()` signature differs between branches — current branch doesn't take rssi parameter
- BluerAcceptor needs rate config passed through from BluerIo (new fields on acceptor struct)
