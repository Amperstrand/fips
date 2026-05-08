# BLE Transport Testing

## Physical-device testing

Hardware-dependent BLE stability tests live in **[Amperstrand/fips-lab](https://github.com/Amperstrand/fips-lab)**, which provides:

- `ble-stability-test.sh` — automated end-to-end BLE reliability test (physical devices only)
- `ble-spike/` — standalone Rust spike for BLE L2CAP exploration
- `lab/` — Python orchestration framework (device inventory, deploy, capture, analysis)
- `scenarios/` — lab topology definitions (2-node, 3-node, microfips smoke)

```bash
git clone https://github.com/Amperstrand/fips-lab.git
cd fips-lab
pip install -r requirements.txt
python -m lab --scenario scenarios/lab-2node-ble.yaml
```

## In-tree unit tests (no hardware required)

BLE transport logic is tested via `MockBleIo` in-memory channel doubles
that run in CI without Bluetooth hardware:

```bash
cargo test --lib                     # All unit tests (includes BLE tests)
cargo test --lib -- transport::ble   # BLE-specific tests only
cargo test --lib -- transport::ble::backoff  # Single module
```

## Building with BLE support

```bash
# Linux — auto-detected when BlueZ/libdbus are installed
cargo build --release

# macOS — requires explicit feature flag
cargo build --release --features ble-macos
```

## Prerequisites for hardware testing

### Linux

- BLE adapter (built-in or USB dongle, e.g., CSR8510, Intel AX200)
- BlueZ stack: `sudo apt install bluetooth bluez`
- Verify adapter: `hciconfig hci0` or `btmgmt info`

### macOS

- Built-in Bluetooth (all Macs since 2012)

### Wireshark (optional, for protocol analysis)

- Install Wireshark with BLE capture support
- Copy `testing/chaos/wireshark/fips-dissector.lua` to your Wireshark
  plugins directory
- Capture BLE traffic: `sudo btmon -i hci0 -w capture.log` (Linux)

## Expected Results

For the `ble-smoke` scenario (2 nodes):

| Metric | Observed |
| ------ | -------- |
| BLE scan to discovery | ~200–500ms |
| GATT PSM read | ~286ms |
| L2CAP channel open | ~29ms |
| Noise handshake | ~50ms |
| **Total connect** | **~2300ms** |
| MTU | 2048/2048 (both directions) |
| Spanning tree convergence | < 5 seconds total |
| Throughput | 50–250 Kbps (limited by BLE link) |

### Hardware-validated setup

| Box | Role | Adapter |
| --- | ---- | -------- |
| macOS (arm64) | Central + Peripheral | Built-in (`default`) |
| Linux (x86_64) | Central + Peripheral | `hci0` |

BLE PSM: dynamic (GATT-advertised by macOS via UUID `9c90b790-2cc5-42c0-9f87-c9cc40648f4c`).

## Troubleshooting

### "No BLE adapter found"

- Linux: Check `hciconfig hci0` — adapter may be blocked. Run
  `sudo rfkill unblock bluetooth` and `sudo hciconfig hci0 up`.
- macOS: Check System Preferences → Bluetooth is enabled.

### "L2CAP connect failed"

- Linux: Ensure `bluetoothd` is running (`sudo systemctl status bluetooth`).
- Check PSM 0x0085 (133) is not in use: `sudo sdptool browse local`.

### macOS central role receives no data

- Fixed in the Amperstrand/bluest fork (Amperstrand/bluest#3).
- The fork is pinned in `Cargo.toml` via `rev = "f3c8d09"`.

### macOS L2CAP sends silently lose bytes

- Fixed in the Amperstrand/bluest fork (Amperstrand/bluest#2).
- Symptom: intermittent corruption in Noise handshake or AEAD decryption
  failures under sustained traffic.

### TCP over BLE is slow and bursty

- TCP works over BLE but is suboptimal due to the constrained link and
  kernel-level TCP behavior.
- UDP and ICMPv6 (ping6, iperf3 UDP mode) work reliably at up to 80 Kbps
  with zero loss. Use UDP for BLE throughput testing.

### Connection keeps dropping

- BLE connection interval may be too aggressive. Default is 30ms; some
  adapters prefer 100ms+.
- Check distance — BLE range is typically 10m (line of sight).
- USB BLE dongles may have power issues — try a powered USB hub.

### "MockBleIo" in logs

This means the transport compiled with the mock backend instead of the real
one. Ensure:
- Linux: Building with glibc (not musl) so `bluer_available` is set
- macOS: `--features ble-macos` is specified
