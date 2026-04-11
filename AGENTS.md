# Agent Coordination Notes

Notes for LLM agents working on this codebase. Not user documentation.

## BLE Framing Architecture — RESOLVED

The framing issues that caused bugs across multiple sessions have been fixed.
See `.sisyphus/notepad/ble-framing-architecture.md` for the full history.

### Current State

| Platform | Framing | Wire-compatible with upstream? |
|----------|---------|-------------------------------|
| Linux (`io.rs`) | **None** — raw SeqPacket pass-through | ✅ Yes |
| macOS (`io_macos.rs`) | 2-byte BE length prefix (byte streams need it) | ❌ No |
| Mock (`io.rs`) | **None** — raw channel pass-through | ✅ Yes |

### What Was Fixed

- Removed FMP-level coalescing from `receive_loop` (was `e81d688` + `daa76f1`)
- Removed 2-byte BE length prefix from Linux `BluerStream` (was unnecessary for SeqPacket)
- Removed 2-byte prefix from `MockBleStream` (tests should match production Linux behavior)
- macOS `Bluestream` keeps its 2-byte prefix (CoreBluetooth byte streams require framing)

### Rules

1. **NEVER put transport framing logic in `receive_loop` (mod.rs).** It should
   receive one complete FMP frame per `stream.recv()` call and deliver it.

2. **Frame reassembly (if needed) belongs in the BleStream implementation**
   (`io.rs` for Linux, `io_macos.rs` for macOS).

3. **Linux SeqPacket preserves message boundaries.** No framing needed. Do not add
   length-prefix framing to `BluerStream`.

4. **macOS byte streams need framing.** The 2-byte prefix in `io_macos.rs` is correct
   and necessary.

## Branch: linux-ble-stability-v2

Based on `jmcorgan/master` with BLE stability fixes and experimental features.

### Known Issues on This Branch

- `BleConnection.is_static`: Always set to `false` — unimplemented
- Leaf proxy commits (15+): Experimental feature, separate from BLE fixes
- macOS framing: Wire-incompatible with upstream (macOS uses channels upstream,
  we use 2-byte prefix). Acceptable since BLE transport was not in production.

### PR Planning

See GitHub issue #39 for the recommended PR split. Key point: BLE fixes should
be separate from leaf proxy feature. Framing cleanup is done.

## ESP32-S3 Known Limitations

- BLE L2CAP MTU: 512 bytes (hardware constraint)
- FilterAnnounce (1071 bytes wire) exceeds ESP32 MTU — FMP has no fragmentation
- Bloom filter MTU skip fix deployed (`9b86483`) — ESP32 receives data but
  `their_index=00000000` (firmware never sends data back)
- See GitHub issue #43 for ESP32 firmware action items
