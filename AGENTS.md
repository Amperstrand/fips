# Agent Coordination Notes

Notes for LLM agents working on this codebase. Not user documentation.

## BLE Framing Architecture

All platforms on this branch use 2-byte BE length-prefix framing.
See `.sisyphus/notepad/ble-framing-architecture.md` for the full history.

### Current State

| Platform | Framing | Wire-compatible with upstream? |
|----------|---------|-------------------------------|
| Linux (`io.rs`) | 2-byte BE length prefix | ❌ No |
| macOS (`io_macos.rs`) | 2-byte BE length prefix | ❌ No |
| Mock (`io.rs`) | 2-byte BE length prefix | ❌ No |

### Why All Platforms Use 2-byte Prefix

- **macOS**: CoreBluetooth byte streams may coalesce/fragment SDUs. Framing is required.
- **Linux**: SeqPacket preserves boundaries, so framing is technically unnecessary.
  However, the ESP32 firmware **expects** 2-byte prefix framing. Since we need
  Linux↔ESP32 interop, Linux must also use the prefix.
- **Mock**: Tests must match production wire format (all platforms use prefix).

### Rules

1. **NEVER put transport framing logic in `receive_loop` (mod.rs).** It should
   receive one complete FMP frame per `stream.recv()` call and deliver it.

2. **Frame reassembly belongs in the BleStream implementation**
   (`io.rs` for Linux, `io_macos.rs` for macOS).

3. **All platforms use 2-byte prefix.** Do NOT remove it from Linux or Mock.
   The ESP32 firmware depends on it.

4. **Upstream (`jmcorgan/master`) does NOT use framing.** This branch is
   wire-incompatible with upstream. Acceptable — upstream BLE was not in production.

## GitHub Conventions

- **Issues**: Always create on `Amperstrand/fips`, NEVER on `jmcorgan/fips` (upstream).
- **Comments**: Same rule — only comment on `Amperstrand/fips` or the user's own repos.
- **PRs**: Target `jmcorgan/fips` only when explicitly asked to upstream a change.
- **Push**: `origin` is `Amperstrand/fips`. Upstream remote is `jmcorgan/fips`.
- When using `gh` commands, always pass `--repo Amperstrand/fips` unless upstreaming.

## Branch: linux-ble-stability-v2

Based on `jmcorgan/master` (merged at `0442117`) with BLE stability fixes and
experimental features. Upstream features now included: LAN gateway, macOS
packaging, bloom filter routing fix, MMP interval tuning, rustfmt, toolchain
1.94.1.

### Known Issues on This Branch

- `BleConnection.is_static`: Always set to `false` — unimplemented
- Leaf proxy commits (15+): Experimental feature, separate from BLE fixes
- Wire-incompatible with upstream (we use 2-byte prefix, upstream doesn't)
- xHCI controller death after `systemctl restart bluetooth` (Amperstrand/fips#63)
- CoreBluetooth peripheral mode rejects SMP pairing (Amperstrand/fips#64)
- GATT-first connect sometimes aborts on first attempt (Amperstrand/fips#65)

### PR Planning

See GitHub issue Amperstrand/fips#39 for the recommended PR split. Key point: BLE fixes should
be separate from leaf proxy feature.

## Linux Node Operations

- **SSH**: `ssh -i ~/.ssh/id_ed25519_gitlab ubuntu@192.168.13.218`
- **ALWAYS run FIPS as a systemd service**, never as a background process:
  ```bash
  sudo chattr -i /etc/fips/fips.yaml   # unlock config
  # edit config...
  sudo chattr +i /etc/fips/fips.yaml   # lock config
  sudo systemctl restart fips           # restart service
  sudo journalctl -u fips -f           # follow logs
  ```
- **Build**: `source ~/.cargo/env && CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble`
- **Install**: `sudo cp /tmp/fips-target/release/fips /usr/local/bin/fips && sudo cp /tmp/fips-target/release/fipsctl /usr/local/bin/fipsctl`
- **Config file**: `/etc/fips/fips.yaml` (often has `chattr +i` — unlock before editing)
- **Control socket**: `/run/fips/control.sock` (via `fipsctl`)
- **Ephemeral key log**: `/var/log/fips/fips-ik-ephemeral.jsonl`
- **Bluetooth adapter**: `hci0`, BD Address `14:5A:FC:49:C2:24`

## macOS Node Operations

- macOS FIPS is managed by the user (launchd or manual). Do not attempt to start/stop remotely.
- macOS BLE supports both central and peripheral roles on this branch.
- macOS BLE peripheral mode works but requires unencrypted L2CAP + dedicated NSRunLoop thread + dynamic PSM discovery via GATT (Amperstrand/fips#64).
- Mac restart script: `sudo bash /tmp/fips-mac.sh`

## ESP32-S3 Known Limitations

- BLE L2CAP MTU: 512 bytes (hardware constraint)
- FilterAnnounce (1071 bytes wire) exceeds ESP32 MTU — FMP has no fragmentation
- Bloom filter MTU skip fix deployed (`9b86483`) — ESP32 receives data but
  `their_index=00000000` (firmware never sends data back)
- See GitHub issue Amperstrand/fips#66 for ESP32 firmware action items
