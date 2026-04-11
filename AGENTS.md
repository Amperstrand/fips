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

## Branch: linux-ble-stability-v2

Based on `jmcorgan/master` (merged at `0442117`) with BLE stability fixes and
experimental features. Upstream features now included: LAN gateway, macOS
packaging, bloom filter routing fix, MMP interval tuning, rustfmt, toolchain
1.94.1.

### Known Issues on This Branch

- `BleConnection.is_static`: Always set to `false` — unimplemented
- Leaf proxy commits (15+): Experimental feature, separate from BLE fixes
- Wire-incompatible with upstream (we use 2-byte prefix, upstream doesn't)

### PR Planning

See GitHub issue #39 for the recommended PR split. Key point: BLE fixes should
be separate from leaf proxy feature.

## ESP32-S3 Known Limitations

- BLE L2CAP MTU: 512 bytes (hardware constraint)
- FilterAnnounce (1071 bytes wire) exceeds ESP32 MTU — FMP has no fragmentation
- Bloom filter MTU skip fix deployed (`9b86483`) — ESP32 receives data but
  `their_index=00000000` (firmware never sends data back)
- See GitHub issue #43 for ESP32 firmware action items
