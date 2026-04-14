# BLE Hardware Test Plan — Capability Signaling

Tests the BLE peer capability signaling feature (commit `8c388cf` and
subsequent). Verifies backwards-compatible capability exchange during
pubkey exchange and correct tie-breaker behavior for all role
combinations.

## Test Matrix

| # | Name | Mac Capabilities | Linux Capabilities | Expected Outcome |
|---|------|-----------------|-------------------|------------------|
| 1 | normal | `macos_default()` (PREFER_OUTBOUND) | `linux_default()` (full, no pref) | Mac outbound wins via preference tier |
| 2 | linux-central-only | `macos_default()` | `central_only()` (accept_connections=false) | Mac outbound wins; Linux accept_loop keeps inbound from Mac |
| 3 | legacy | Sends 33-byte (no flags) | `linux_default()` | Old wire format works; Linux assumes full capability; NodeAddr decides |
| 4 | speedtest | `macos_default()` | `linux_default()` | Functional throughput test with iperf3 via TUN |

## Prerequisites

- Mac with Bluetooth (this machine) running macOS build with `--features ble-macos`
- Linux machine with BLE adapter in radio range (~10m)
- Both machines on same network for SSH access
- `iperf3` installed on both for throughput tests
- Root/sudo on both machines (for TUN interface)

## Test Execution

### Step 1: Build and Deploy

**Mac** (run from this repo):
```bash
CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble-macos
```

**Linux** (SSH into the machine):
```bash
cd /path/to/fips
git fetch origin
git checkout macos-ble
git pull
cargo build --release --features ble
sudo systemctl stop fips
sudo cp target/release/fips /usr/local/bin/fips
```

### Step 2: Run Each Test

For each test in the matrix:

1. Deploy the appropriate config to both Mac and Linux
2. Start FIPS on both (Linux: `systemctl start fips`, Mac: `sudo ./fips -c config.yaml`)
3. Wait for BLE discovery and connection (~10-30s)
4. Verify connection established:
   - `fipsctl show links` — should show BLE link
   - `fipsctl show peers` — should show authenticated peer
5. Capture debug log with ephemeral keys for offline decoding
6. Run connectivity test: `ping6 <peer_npub>.fips`
7. Run throughput test: `iperf3 -c <peer_fd00_addr>` (through TUN)
8. Stop and collect logs

### Step 3: Decode Captured Traffic

Use the debug ephemeral key log (`debug_ephemeral_key_log_path`) to
decrypt captured BLE L2CAP traffic. Verify:
- Pubkey exchange frame: 34 bytes (`[0x00][pubkey:32][flags:1]`)
- Capability flags byte matches expected value
- Noise IK handshake succeeds
- FMP data flows bidirectionally

## Config Files

See `configs/` subdirectory:
- `mac-test{N}.yaml` — Mac config for test N
- `linux-test{N}.yaml` — Linux config for test N

## Results Template

For each test, record:

```
### Test N: <name>
- Date/time:
- Mac NodeAddr:
- Linux NodeAddr:
- Connection time (from logs):
- Capability bytes exchanged (from debug log):
  - Mac sent: 0x__
  - Linux sent: 0x__
- Tie-breaker result:
  - Which side's outbound won:
  - Reason (preference/can't_accept_inbound/NodeAddr):
- Connectivity:
  - ping6: ___ms RTT, ___% loss
  - iperf3: ___ Kbps throughput
- Wire format verified:
  - Pubkey exchange frame size: 34 bytes ✓/✗
  - Flags byte correct: ✓/✗
- Issues observed:
```
