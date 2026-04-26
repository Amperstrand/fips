# AGENTS.md — Developer Reference

## Build Commands

```bash
# Standard build (Linux, no BLE feature flag needed — auto-detected)
cargo build --release

# macOS with BLE support
cargo build --release --features ble-macos

# Build without BLE (any platform)
cargo build --release --no-default-features --features tui
```

## Lint & Check

```bash
cargo clippy --all -- -D warnings
cargo check
cargo fmt --check
```

## Test

```bash
# Unit tests
cargo test --lib

# Unit tests with macOS BLE
cargo test --lib --features ble-macos

# Local CI
./testing/ci-local.sh --test-only
```

## Feature Flags

| Feature | Purpose | Platforms |
|---------|---------|-----------|
| `ble-macos` | BLE via CoreBluetooth | macOS only |
| `tui` | Terminal UI | All |
| (none) | Linux BLE auto-detected via `build.rs` | glibc Linux |

## BLE Hardware Testing

See `testing/ble/README.md` for two-box BLE test setup (macOS ↔ Linux).

Key config fields for BLE transport:
```yaml
transports:
  ble:
    adapter: "hci0"        # Linux: "hci0", macOS: "default"
    mtu: 2048
    advertise: true
    scan: true
    auto_connect: true
    accept_connections: true
    send_rate_bps: 200000
    send_burst_bytes: 2048
```

## Architecture

- `src/transport/ble/` — Platform-independent BLE transport logic
- `src/transport/ble/io.rs` — Linux BLE (bluer/BlueZ)
- `src/transport/ble/io_macos.rs` — macOS BLE (bluest/CoreBluetooth)
- `src/transport/ble/io_macos.rs` — Dual role: central (bluest) + peripheral (objc2-core-bluetooth)
- `src/noise/` — Noise protocol (XK pattern, ChaCha20-Poly1305)
- `src/node/` — Mesh node core (handshake, rekey, sessions, tree, bloom)

## Key Dependencies

- `bluest` (macOS BLE): pinned to Amperstrand fork, rev `f3c8d09` — fixes NSOutputStream partial write and NSRunLoop scheduling
- `bluer` (Linux BLE): upstream, auto-detected by build.rs on glibc Linux
- Rust toolchain: 1.94.1 (pinned in rust-toolchain.toml)
