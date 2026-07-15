# macOS BLE Transport Design

## Background

FIPS uses BLE L2CAP Connection-Oriented Channels (CoC) for peer
communication. The Linux implementation (`BluerIo`) uses the `bluer` crate
(BlueZ D-Bus bindings) and binds to a fixed PSM (`0x0085`). Both sides
know the PSM, so connection is straightforward: `listen(0x0085)` /
`connect(addr, 0x0085)`.

macOS supports L2CAP CoC via CoreBluetooth's `CBL2CAPChannel` (since macOS
10.13), but with a fundamental protocol difference that shapes the entire
implementation.

## The PSM problem

Linux (BlueZ) lets you bind an L2CAP listener to a specific PSM in the
dynamic range (`0x0080`–`0x00FF`). FIPS uses `0x0085`.

macOS (CoreBluetooth) **dynamically assigns** the PSM when you call
`publishL2CAPChannelWithEncryption`. You cannot choose a specific PSM. The
assigned PSM must be communicated to the connecting peer, typically via a
GATT characteristic.

### How it works now (branch `linux-ble-stability-v2`)

The solution is a GATT PSM exchange: the peripheral publishes L2CAP with a
dynamic PSM, exposes that PSM via a GATT read characteristic, and the
central discovers it before opening the L2CAP channel.

Both platforms implement the **same** GATT-first connect pattern:

#### Linux central (`BluerIo::connect`) — `io.rs:794-885`

```
1. GATT connect to peer
2. Enumerate GATT services → find FIPS_GATT_PSM_SERVICE_UUID
3. Enumerate characteristics → find FIPS_GATT_PSM_CHAR_UUID
4. Read 2-byte LE PSM value from characteristic
5. Disconnect GATT
6. Open L2CAP socket with discovered PSM
   (fallback to configured PSM 0x0085 if GATT discovery fails)
```

#### macOS central (`BluestIo::connect`) — `io_macos.rs:1047-1078`

```
1. BLE connect to peer (via bluest adapter)
2. discover_gatt_psm(): enumerate services → find PSM service → read PSM char
3. Open L2CAP channel with discovered PSM
   (fallback to configured PSM 0x0085 if GATT discovery fails)
```

#### macOS peripheral (`BluestIo::listen`) — `io_macos.rs:900-1044`

```
1. Create CBPeripheralManager on dedicated dispatch queue
2. publishL2CAPChannelWithEncryption(false) → get dynamic PSM from delegate
3. Create GATT service with FIPS_GATT_PSM_SERVICE_UUID
4. Add read-only characteristic with 2-byte LE PSM value
5. Start advertising both FIPS service UUID and GATT PSM service UUID
```

### Interoperability matrix

| Scenario         | Status | Central code path | Proof |
|------------------|--------|-------------------|-------|
| macOS → Linux    | Works  | macOS `BluestIo::connect` → GATT PSM discovery fails (Linux has no GATT PSM service) → fallback to configured PSM `0x0085` → L2CAP open succeeds | Code-level: `io_macos.rs:1053-1062` fallback path. Runtime: Linux listener on `0x0085` accepts the connection. |
| Linux → macOS    | Works  | Linux `BluerIo::connect` → GATT connect to Mac → discover PSM service → read dynamic PSM → open L2CAP with dynamic PSM | Code-level: `io.rs:794-885` complete GATT-first flow. Runtime: this was the previously-blocked direction, now fixed. |
| macOS → macOS    | Likely works | macOS `BluestIo::connect` → discover GATT PSM on peer Mac → use dynamic PSM → open L2CAP | Code-level: both sides have symmetric GATT PSM exchange. No runtime proof yet — requires two Macs. |

### Why macOS → macOS is "likely" and not "proven"

The code paths for both directions are now symmetric and structurally
identical to the Linux → macOS path that works. Specifically:

- **macOS peripheral side** publishes dynamic PSM via GATT (`io_macos.rs:962-992`)
- **macOS central side** discovers PSM via GATT before L2CAP connect (`io_macos.rs:1053-1062`)
- **Linux central side** uses the same GATT discovery pattern and it works against macOS peripherals (`io.rs:805-820`)

The only missing piece is a two-Mac hardware test. The code is wired
correctly; the question is whether CoreBluetooth allows the same device to
simultaneously act as both central and peripheral for L2CAP, which it should
per Apple documentation but which we have not yet observed on this branch.

### Simulated proof: Linux ↔ macOS (both directions)

Since we cannot test Mac ↔ Mac directly, the strongest available proof is
Linux ↔ macOS in both directions, which exercises the same GATT PSM exchange
that Mac ↔ Mac would use:

| Test | Direction | GATT PSM exchange | L2CAP result |
|------|-----------|-------------------|--------------|
| Linux central → macOS peripheral | Linux reads dynamic PSM from Mac's GATT service | `io.rs:809-820` discovers PSM, uses it | Works |
| macOS central → Linux peripheral | Mac's GATT read fails (no service), falls back to `0x0085` | `io_macos.rs:1058-1061` fallback | Works |

Both connect patterns exercise the full GATT PSM machinery on at least one
side. For Mac ↔ Mac, both sides would use GATT discovery — the same code
paths, just exercised from both ends simultaneously.

## Crate selection

| Crate                    | L2CAP on macOS | Notes                                              |
|--------------------------|----------------|----------------------------------------------------|
| `bluest`                 | Yes            | Async, wraps CoreBluetooth, L2CAP added June 2025  |
| `btleplug`               | No             | GATT only, maintainer confirmed no L2CAP plans     |
| `objc2-core-bluetooth`   | Yes (raw)      | Low-level Objective-C bindings, all L2CAP APIs bound|
| `core_bluetooth`          | No             | Central role only, no L2CAP, appears unmaintained   |

**Selected: `bluest`** for central role + `objc2-core-bluetooth` for peripheral role.

`bluest` is the only Rust crate with a working async L2CAP CoC
implementation over CoreBluetooth. Pre-1.0 but actively maintained (174+
commits, 140+ stars). Uses `objc2-core-bluetooth` internally. API:

- `Device::open_l2cap_channel(psm, secure)` → `L2capChannel`
- `L2capChannel` provides `read()`, `write()`, `close()`, `split()`

Note: the `bluest` docs may still say "L2CAP not supported on macOS" — this
is outdated. The implementation landed in PR #33 (June 2025).

For the peripheral side, we use `objc2-core-bluetooth` directly because
`bluest` does not expose `CBPeripheralManager` (the peripheral role manager).
This gives us `publishL2CAPChannelWithEncryption`, GATT service/characteristic
creation, and the delegate callbacks needed for inbound L2CAP channels.

## Architecture

### Dual-role implementation

The macOS BLE implementation handles **both** central (outbound) and
peripheral (inbound) roles in a single `BluestIo` struct:

- **Central** (via `bluest`): scan for peers, connect, open L2CAP channels
- **Peripheral** (via `objc2-core-bluetooth`): publish L2CAP, advertise,
  accept inbound connections, manage GATT PSM characteristic

This is different from Linux where both roles use `bluer` (BlueZ) through
SeqPacket sockets. macOS requires two different crate stacks for the two
roles.

### Trait implementation

The existing `BleIo` trait abstracts all platform-specific BLE operations:

```rust
pub trait BleIo: Send + Sync + 'static {
    type Stream: BleStream + 'static;
    type Acceptor: BleAcceptor<Stream = Self::Stream> + 'static;
    type Scanner: BleScanner + 'static;

    async fn listen(&self, psm: u16) -> Result<Self::Acceptor, TransportError>;
    async fn connect(&self, addr: &BleAddr, psm: u16) -> Result<Self::Stream, TransportError>;
    async fn start_advertising(&self) -> Result<(), TransportError>;
    async fn stop_advertising(&self) -> Result<(), TransportError>;
    async fn start_scanning(&self) -> Result<Self::Scanner, TransportError>;
    fn local_addr(&self) -> Result<BleAddr, TransportError>;
    fn adapter_name(&self) -> &str;
}
```

All higher-level logic (connection pool, discovery buffer, accept/scan loops,
pubkey exchange, cross-probe tie-breaking) remains unchanged.

### GATT PSM exchange — full protocol

Since macOS cannot listen on a fixed PSM, the implementation adds a thin GATT
layer to advertise the dynamically-assigned PSM:

**Peripheral side (`listen` + `start_advertising`):**

1. Create `CBPeripheralManager` on dedicated GCD dispatch queue
2. Call `publishL2CAPChannelWithEncryption(false)` → receive dynamic PSM via
   `peripheralManagerDidPublishL2CAPChannel` delegate callback
3. Create a GATT service with UUID `FIPS_GATT_PSM_SERVICE_UUID`
4. Add a read-only characteristic (`FIPS_GATT_PSM_CHAR_UUID`) containing the
   2-byte PSM (little-endian)
5. Start advertising both the FIPS service UUID and the GATT PSM service UUID
   in the advertisement data

**Central side (`connect`):**

1. BLE connect to the peripheral
2. Enumerate GATT services → find `FIPS_GATT_PSM_SERVICE_UUID`
3. Enumerate characteristics → find `FIPS_GATT_PSM_CHAR_UUID`
4. Read 2-byte LE value → discovered PSM
5. Open L2CAP channel with discovered PSM
6. If GATT discovery fails or the peer doesn't expose the service (e.g. Linux
   with static PSM `0x0085`), fall back to the configured PSM

**Fallback behavior:**

- When connecting to a Linux peer that listens on a known static PSM, the
  GATT PSM service won't be found, and the connect path falls back to the
  configured PSM. This is expected and correct.
- When connecting to a macOS peer, the GATT PSM service must be found, or the
  connection will fail (the dynamic PSM is unknown without it).

### UUID constants

| Constant | UUID | Purpose |
|----------|------|---------|
| `FIPS_SERVICE_UUID` | `9c90b790-2cc5-42c0-9f87-c9cc40648f4c` | Main FIPS BLE service — advertised in scan response |
| `FIPS_GATT_PSM_SERVICE_UUID` | `0e2c43b1-51b9-4667-a1d1-a95ea79fd19b` | GATT service that exposes the dynamic PSM |
| `FIPS_GATT_PSM_CHAR_UUID` | `250c88dd-3dff-4c41-83b2-f1b4e3d820cc` | Read characteristic containing 2-byte LE PSM |

### Stream adapter

`bluest`'s `L2capChannel` exposes byte-stream semantics (`read`/`write`),
while `BleStream` expects datagram semantics (each `send` is one message). On
BLE L2CAP CoC, the underlying transport preserves message boundaries at the
controller level, but CoreBluetooth's NSStream abstraction may coalesce reads.

**Resolved**: The `linux-ble-stability-v2` branch implements length-prefix
framing (2-byte BE prefix) in `io_macos.rs`. Note that upstream
(`jmcorgan/macos-support`) uses a channel-based approach
(`tokio::sync::mpsc`) that avoids needing framing entirely — each
CoreBluetooth delegate callback maps to one channel message, preserving
boundaries naturally. The length-prefix approach is a **wire protocol
change** that makes this branch incompatible with upstream. See
`.sisyphus/notepad/ble-framing-architecture.md` for the full analysis.

### Peripheral stream implementation

The peripheral (inbound) side uses a different stream type than the central
(outbound) side:

- **Central**: `BluestStream` wraps `bluest::L2capChannel` halves directly
- **Peripheral**: `PeripheralStream` wraps raw `NSInputStream`/`NSOutputStream`
  from `CBL2CAPChannel`, managed through delegate callbacks on a dedicated
  NSRunLoop thread

The peripheral stream uses `PeripheralOutputDelegate` (Objective-C class
defined via `define_class!`) to receive `NSStreamEvent::HasSpaceAvailable`
notifications for backpressure-aware writes.

### Role capabilities and tie-breaking

macOS nodes advertise `PeerCapabilities::macos_default()` which includes:

- `CAN_CENTRAL` — can initiate outbound BLE connections
- `CAN_PERIPHERAL` — can accept inbound BLE connections
- `GATT_SUPPORTED` — supports GATT PSM exchange
- `PREFER_OUTBOUND` — prefers to be the initiator when both sides scan

When `accept_connections=true`, the macOS node runs both `scan_probe_loop()`
(outbound) and `accept_loop()` (inbound) simultaneously. If two macOS nodes
discover each other, the tie-breaker in `scan_probe_loop()` compares
`NodeAddr` values — the smaller address wins the outbound role, the larger
accepts inbound. This prevents duplicate connections.

When `accept_connections=false`, the macOS node runs in central-only mode
(`PeerCapabilities::central_only()`) — it can only initiate connections, not
accept them.

### Feature gating

```toml
# Cargo.toml
[features]
default = ["tui", "ble"]
ble = ["dep:bluer"]
ble-macos = ["dep:bluest"]

[target.'cfg(target_os = "linux")'.dependencies]
bluer = { version = "0.17", features = ["bluetoothd", "l2cap"], optional = true }

[target.'cfg(target_os = "macos")'.dependencies]
bluest = { version = "0.3", optional = true }
```

```rust
// src/transport/ble/io.rs
#[cfg(all(feature = "ble", target_os = "linux"))]
pub type DefaultBleIo = BluerIo;

#[cfg(all(feature = "ble-macos", target_os = "macos"))]
pub type DefaultBleIo = BluestIo;
```

## Code reference

Key files and line ranges on `linux-ble-stability-v2`:

| File | Lines | What |
|------|-------|------|
| `src/transport/ble/io_macos.rs` | 798-876 | `BluestIo::discover_gatt_psm()` — GATT PSM discovery for macOS central |
| `src/transport/ble/io_macos.rs` | 900-1044 | `BluestIo::listen()` — peripheral manager setup, L2CAP publish, GATT service creation |
| `src/transport/ble/io_macos.rs` | 962 | `publishL2CAPChannelWithEncryption(false)` — unencrypted L2CAP publish |
| `src/transport/ble/io_macos.rs` | 977-992 | GATT PSM service and characteristic creation |
| `src/transport/ble/io_macos.rs` | 1047-1078 | `BluestIo::connect()` — central connect with GATT PSM discovery + fallback |
| `src/transport/ble/io.rs` | 612-648 | `BluerIo::discover_gatt_psm()` — GATT PSM discovery for Linux central |
| `src/transport/ble/io.rs` | 650-710 | `BluerIo::read_psm_from_gatt()` — GATT service/characteristic enumeration |
| `src/transport/ble/io.rs` | 794-885 | `BluerIo::connect()` — GATT-first connect with PSM discovery + fallback |
| `src/transport/ble/mod.rs` | 849+ | `accept_connections` gate for peripheral vs central-only mode |
| `src/node/mod.rs` | — | `PeerCapabilities::macos_default()` assignment |

## Verification

### Automated

```
$ cargo build --features ble-macos
   Finished `dev` profile [unoptimized + debuginfo] target(s)
   15 warnings (pre-existing, unrelated to GATT PSM change)

$ cargo test test_peer_capabilities_defaults_and_queries --features ble-macos
   test transport::ble::tests::test_peer_capabilities_defaults_and_queries ... ok

$ cargo test test_tiebreaker_convention --features ble-macos
   test transport::ble::tests::test_tiebreaker_convention ... ok

$ cargo test test_gatt_supported_flag_encoding --features ble-macos
   test transport::ble::tests::test_gatt_supported_flag_encoding ... ok
```

### Hardware test plan

See `testing/ble/hw-test-plan.md` for the full Mac ↔ Mac verification
matrix including: direction tests (M1/M2), dual-scan race (M3), reconnect
(M4), and large payload coalescing (M5).

## Open questions

1. **`bluest` maturity**: the L2CAP implementation is ~1 year old. Should we
   vendor/fork it, or depend on crates.io and pin the version?

2. **Stream framing** (RESOLVED): `bluest`'s `L2capChannel::read()` does NOT
   preserve L2CAP SDU boundaries — it's a byte stream. Our branch uses 2-byte
   BE length-prefix framing to handle this. Upstream avoids the problem entirely
   by using a channel-based approach. See
   `.sisyphus/notepad/ble-framing-architecture.md`.

3. **macOS ↔ macOS runtime proof**: code paths now support dynamic PSM exchange
   in both central implementations, but a dedicated two-Mac hardware run is
   still needed to prove both directions on this branch.

4. **Adapter naming**: Linux uses `hci0`, macOS uses `default` (there's
   typically one adapter). The `BleAddr` format (`adapter/AA:BB:CC:DD:EE:FF`)
   should work with `default` as the adapter name. CoreBluetooth doesn't expose
   adapter names, so this is a synthetic identifier.

5. **Testing**: macOS BLE testing requires physical hardware. CI can only verify
   compilation, not runtime behavior. The existing `MockBleIo` test
   infrastructure covers the transport logic above the I/O layer.

6. **L2CAP encryption**: macOS `publishL2CAPChannelWithEncryption(false)` uses
   unencrypted L2CAP. See Amperstrand/fips#61 and #64 for the encryption/
   pairing status.

## Upstream comparison

**Target branch**: `jmcorgan/fips` `macos-ble-rebased` (commit `0ae9e01`)

### What upstream already has (316-line `io_macos.rs`)

- `bluest`-based central role (scan, connect, open L2CAP)
- `BluestStream` with raw byte-stream reads (no framing — reassembly in `receive_loop`)
- Stub `BluestAcceptor` that blocks forever (inbound not supported)
- Stub `start_advertising()` (no advertising)
- `connect()` uses configured PSM directly (no GATT discovery)
- `MockBleStream` uses 2-byte length-prefix framing (channel-based)

### What our branch adds (~1160-line `io_macos.rs`)

| Feature | Lines | Status |
|---------|-------|--------|
| Full peripheral role (`CBPeripheralManager` + ObjC delegates) | ~500 | New |
| GATT PSM service (publish dynamic PSM, advertise via characteristic) | ~120 | New |
| `BluestIo::discover_gatt_psm()` — central discovers PSM before L2CAP | ~80 | New |
| `BluestIo::listen()` — real peripheral manager, L2CAP publish, inbound | ~140 | New |
| NSRunLoop-based `PeripheralStream` with delegate callbacks | ~200 | New |
| 2-byte BE length-prefix framing on all streams | ~60 | Wire change |
| `PeerCapabilities` + role negotiation + tie-breaking | ~200 (mod.rs) | Protocol change |
| Send rate limiting (`rate_limit.rs`) | ~237 | New |
| Linux stability fixes (pairing agent, btmgmt, LE-only) | ~100 (io.rs) | Linux-only |

### Framing divergence

| Approach | Branch | How | Mock match? |
|----------|--------|-----|-------------|
| 2-byte BE length prefix in stream | `linux-ble-stability-v2` | `BluestStream`/`PeripheralStream` add prefix | Yes — mock uses same framing |
| FMP-aware reassembly in receive_loop | `macos-ble-rebased` | Raw reads, reassembler parses FMP headers | Yes — mock uses channel framing |

Upstream commit `794edbf` explicitly removed length-prefix framing from
`BluestStream`. Our branch adds it back. This is a **wire protocol
incompatibility** that must be resolved when preparing upstream PRs.

### Proposed PR split (4 PRs against `macos-ble-rebased`)

| # | What | Scope | Risk | Depends |
|---|------|-------|------|---------|
| 1 | GATT PSM discovery in central `connect()` | ~80 lines `io_macos.rs` | Low | Nothing |
| 2 | Peripheral role + `CBPeripheralManager` + GATT PSM service | ~500 lines `io_macos.rs` | Medium | PR 1 |
| 3 | `PeerCapabilities` + tie-break + scan dedup | ~200 lines `mod.rs` | Medium | PR 2 |
| 4 | Linux stability (pairing agent, btmgmt, LE-only, GATT-first connect) | ~100 lines `io.rs` | Low | Nothing |

PRs 1 and 4 are independent and can land first. PR 2 builds on 1. PR 3 builds on 2.

**Framing decision**: We need to either adopt upstream's FMP-aware reassembly
approach (removing length-prefix from streams) or propose the length-prefix
as a deliberate wire protocol change. This affects all 4 PRs.

## Test campaign plan

Goal: prove at commit X that Linux ↔ macOS BLE works in both directions,
with captured/decrypted traffic, trace logs, and performance numbers.

### Configuration reference

BLE transport config options (`transports.ble` in `fips.yaml`):

| Option | Default | Purpose |
|--------|---------|---------|
| `adapter` | `hci0` | Bluetooth adapter name |
| `psm` | `0x0085` | L2CAP PSM (Linux static; macOS ignores) |
| `mtu` | `2048` | Maximum transmission unit |
| `max_connections` | `7` | BLE hardware connection limit |
| `connect_timeout_ms` | `10000` | L2CAP connect timeout |
| `advertise` | `true` | Advertise BLE presence |
| `scan` | `true` | Scan for BLE peers |
| `auto_connect` | `false` | Auto-connect to discovered peers |
| `accept_connections` | `true` | Accept inbound L2CAP |
| `probe_cooldown_secs` | `30` | Re-probe cooldown |
| `send_rate_bps` | `150000` | Send rate limit (0 = unlimited) |
| `send_burst_bytes` | `4096` | Burst size |
| `le_only` | `false` | Disable BR/EDR (needed for Linux→Mac) |
| `debug_ephemeral_key_log_path` | `None` | JSONL path for Noise IK key material |

### Debug ephemeral key log

When `debug_ephemeral_key_log_path` is set, every Noise IK handshake writes
a JSONL record with enough key material to decrypt captured traffic:

```json
{
  "version": 1,
  "tool": "fips-ik-ephemeral-dump",
  "event": "msg1" | "msg2",
  "role": "initiator" | "responder",
  "local_static_pubkey": "hex",
  "remote_static_pubkey": "hex",
  "local_ephemeral_pubkey": "hex",
  "local_ephemeral_privkey": "hex",
  "remote_ephemeral_pubkey": "hex",
  "local_epoch": "hex",
  "remote_epoch": "hex",
  "handshake_hash": "hex"
}
```

⚠️ **Security warning**: This weakens forward secrecy for logged sessions.
Only enable in lab/debug environments.

### Logging

FIPS uses `tracing`. Set via `RUST_LOG` or `log_level` in config:

| Level | BLE coverage |
|-------|-------------|
| `error` | Fatal BLE failures only |
| `warn` | Non-fatal BLE errors (connect failures, pool full) |
| `info` | Connection established/accepted, advertising started |
| `debug` | GATT PSM discovery, connect/listen lifecycle, L2CAP open/close |
| `trace` | Per-packet send/recv, stream read/write, delegate events |

For test campaign, use `log_level: debug` (or `RUST_LOG=fips=debug`).

### Test matrix

#### Test A: Linux outbound → macOS inbound (Mac peripheral)

**Linux config**: `accept_connections: false`, `scan: true`, `advertise: true`
**Mac config**: `accept_connections: true`, `scan: false` (or both true for tie-break)

1. Start Mac FIPS with `accept_connections: true` and `debug_ephemeral_key_log_path`
2. Start Linux FIPS with `accept_connections: false`, `debug_ephemeral_key_log_path`, `le_only: true`
3. Verify: `fipsctl show peers` on both sides shows connected BLE peer
4. Capture: `sudo btmon -i hci0` on Linux during connection
5. Decrypt: use ephemeral key log + Wireshark to verify Noise IK handshake
6. Performance: `iperf3 -c <mac_fd00_addr> -6 -t 30` through TUN

#### Test B: macOS outbound → Linux inbound (Mac central)

**Mac config**: `accept_connections: false`, `scan: true`, `advertise: true`
**Linux config**: `accept_connections: true`, `scan: false`

1. Start Linux FIPS with `accept_connections: true`
2. Start Mac FIPS with `accept_connections: false`
3. Verify: Mac discovers Linux via scan, connects, peer shows connected
4. Capture: `btmon` on Linux shows inbound L2CAP
5. Performance: `iperf3 -c <linux_fd00_addr> -6 -t 30` through TUN

#### Test C: Both directions, both roles (tie-break)

**Both configs**: `accept_connections: true`, `scan: true`, `advertise: true`

1. Start both with full dual-role configs
2. Verify: exactly one BLE link forms (tie-breaker picks winner)
3. Verify: `fipsctl show links` shows `transport_type: ble`
4. Verify: tree announce, filter announce, MMP traffic flows
5. Reconnect: kill and restart one side, verify link re-establishes
6. Sustained traffic: 5-minute iperf run with `iperf3 -c <peer> -6 -t 300`

#### Test D: Large payload / framing verification

1. Establish link (either direction)
2. Generate burst traffic: `ping6 -s 1200 <peer_fd00_addr>` (near-MTU)
3. Verify: no framing errors, no AEAD errors in logs
4. Verify: `fipsctl show peers` remains connected throughout

### BLE traffic capture (Linux)

```bash
# Real-time monitoring
sudo btmon -i hci0

# Capture to file for Wireshark
sudo btmon -i hci0 -w /tmp/fips-ble-capture.pklg

# Alternative: hcidump
sudo hcidump -i hci0 -w /tmp/fips-ble-dump.btsnoop

# Wireshark analysis
wireshark /tmp/fips-ble-capture.pklg
# Filter: btl2cap.cid == 0x0040 (LE L2CAP CoC)
```

### Runtime inspection

```bash
# Peer status
fipsctl show peers
fipsctl show links
fipsctl show transports

# Live metrics (MMP)
fipsctl show status

# Full log tail
sudo journalctl -u fips -f    # Linux
sudo tail -f /usr/local/var/log/fips/fips.log   # macOS
```
