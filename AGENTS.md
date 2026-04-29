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

### Stability test (automated)

```bash
# 20-minute default
./testing/ble/ble-stability-test.sh

# Custom duration + iperf3 throughput test
./testing/ble/ble-stability-test.sh -d 60 --iperf

# With BLE traffic capture
./testing/ble/ble-stability-test.sh --capture -v
```

Results are saved to `testing/ble/results/<timestamp>/`.

### Key config fields for BLE transport
```yaml
transports:
  ble:
    adapter: "hci0"        # Linux: "hci0", macOS: "default"
    mtu: 2048
    advertise: true
    scan: true
    auto_connect: true
    accept_connections: true
    send_rate_bps: 80000     # Initial rate; AIMD adapter probes within MAX_RATE_BPS (80 Kbps)
    send_burst_bytes: 2048
```

## Architecture

- `src/transport/ble/` — Platform-independent BLE transport logic
- `src/transport/ble/io.rs` — Linux BLE (bluer/BlueZ) with drain task send architecture
- `src/transport/ble/io_macos.rs` — macOS BLE, dual role: central (bluest) + peripheral (objc2-core-bluetooth)
- `src/transport/ble/rate_limit.rs` — `SendRateLimiter` (token bucket) + `BleRateAdapter` (AIMD)
- `src/transport/ble/backoff.rs` — `PeerBackoff` exponential backoff + auto-deny of failing peers
- `src/transport/ble/capabilities.rs` — `PeerCapabilities` for BLE role negotiation
- `src/upper/tun.rs` — `TunPacer` (token bucket), blocks TUN reader at configured rate
- `src/upper/tcp_mss.rs` — `MAX_BLE_TCP_WINDOW = 2920` (2 MSS), strips Window Scale for BLE links
- `src/noise/` — Noise protocol (IK for link, XK for session, ChaCha20-Poly1305)
- `src/node/` — Mesh node core (handshake, rekey, sessions, tree, bloom)

## BLE Reliability Architecture

Three-layer backpressure keeps BLE alive under sustained TCP load:

### Layer 1: TUN pacer (`src/upper/tun.rs`)
Token bucket that blocks the TUN reader at `initial_stream_rate_bps` (clamped to 80 Kbps max).
Prevents the kernel from flooding the mesh with packets faster than BLE can drain.
Configured via `transports.ble.send_rate_bps` and `send_burst_bytes`.

### Layer 2: BLE drain tasks (platform-specific)
All three BLE send paths use the same architecture: `try_send()` into `mpsc::channel(32)` (non-blocking),
with a dedicated spawned task doing blocking `acquire()` + `conn.send()`.

- **Linux** (`io.rs`): `BluerStream` sends via `drain_tx.try_send()`, drain task calls `SendRateLimiter::acquire()` then `conn.send()`
- **macOS central** (`io_macos.rs`): drain task calls `SendRateLimiter::acquire()` then `write_all()`
- **macOS peripheral** (`io_macos.rs`): pacer task calls `SendRateLimiter::acquire()` then `try_enqueue()`

If the channel is full, `try_send()` returns `TrySendError::Full` → `TransportError::SendFailed`.
This is congestion, not a dead connection — the connection stays in the pool.

### Urgent sends (`send_urgent_async`)
Control-plane traffic (Noise handshake, rekey) bypasses the rate-limited drain queue.
On Linux, sends directly via the L2CAP socket. On macOS central, uses the urgent
writer directly. On macOS peripheral, enqueues with backpressure (best-effort).
This prevents control-plane stalls when the data plane is congested.

### Layer 3: Backpressure flag (`src/node/handlers/rx_loop.rs`)
`Arc<AtomicBool>` on `BleTransport`, set on `SendFailed`, cleared on send success.
The RX loop's `select!` skips the TUN outbound branch while `ble_congested` is true,
allowing the drain task to catch up before new packets enter the pipeline.

### TCP window clamping (`src/upper/tcp_mss.rs`)
`MAX_BLE_TCP_WINDOW = 2920` (2 × BLE L2CAP MTU of 1460). TCP Window Scale option is stripped
for packets routed over BLE links. This prevents the kernel from advertising large windows
that would cause burst-flood-drop cycles on the constrained link.

### TUN channel sizing
The TUN outbound channel is sized at 16 (default). This gives ~24KB of kernel buffer
at typical BLE frame sizes, which fills in ~2.4s at 80 Kbps — enough to signal TCP
backoff without unbounded queue growth.

### 60-minute test baseline (2026-04-29)

| Metric | Value |
|--------|-------|
| Duration | 61 min |
| MMP loss | 0.0% (119 samples) |
| RTT range | 25–51 ms |
| Rekeys | 60/60 |
| Disconnects | 0 |
| TCP -w 8K (30s) | 249 KB received |
| UDP 50K | 0% loss (52/52) |
| UDP 100K | 0% loss (83/83) |
| Memory | macOS 2.4 MB, Linux 13.2 MB (stable) |

TCP default socket: 128 KB burst in first second then 0 (known kernel cwnd limitation).
TCP -w 8K: delivers in ~15s retransmission bursts, sustained over 30s. Functional but not smooth.

## Key Dependencies

- `bluest` (macOS BLE): pinned to Amperstrand/bluest, rev `f3c8d09` — fixes NSOutputStream partial write and NSRunLoop scheduling
- `bluer` (Linux BLE): upstream, auto-detected by build.rs on glibc Linux
- Rust toolchain: 1.94.1 (pinned in rust-toolchain.toml)
