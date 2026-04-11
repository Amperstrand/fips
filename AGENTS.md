# Agent Coordination Notes

Notes for LLM agents working on this codebase. Not user documentation.

## BLE Framing Architecture

**WARNING**: Multiple sessions have introduced bugs by putting frame reassembly
logic in the wrong layer. Read `.sisyphus/notepad/ble-framing-architecture.md`
before touching any BLE receive path code.

### Quick Reference

The framing stack has two layers. Each has its own responsibility:

```
FMP frame: [4-byte prefix] [payload] [16-byte AEAD tag]
  ↓ wrapped in BLE framing
BLE frame: [2-byte BE length prefix] [FMP frame]
  ↓ sent over L2CAP SeqPacket
```

| Layer | Boundary marker | Reassembly owner | File |
|-------|----------------|-----------------|------|
| BLE framing | 2-byte BE length prefix | `BluerStream::recv()` | `io.rs` / `io_macos.rs` |
| FMP framing | 4-byte FMP prefix | Node layer (after recv) | `mod.rs` receive_loop |

### Rules

1. Frame reassembly belongs in `BluerStream` / `Bluestream` (`io.rs` / `io_macos.rs`),
   NOT in `receive_loop` in `mod.rs`.

2. `receive_loop` should receive one complete FMP frame per `stream.recv()` call.
   If it needs to split coalesced frames, the bug is in the stream layer.

3. The `e81d688` and `daa76f1` commits on `linux-ble-stability-v2` put coalescing
   logic in the wrong layer (FMP receive_loop instead of BLE stream). Do NOT
   extend or depend on this pattern.

## Branch: linux-ble-stability-v2

Based on `jmcorgan/master` with BLE stability fixes and experimental features.

### Known Issues on This Branch

- `e81d688` + `daa76f1`: Coalescing logic in wrong layer (see above)
- `BleConnection.is_static`: Always set to `false` — unimplemented
- Leaf proxy commits (15+): Experimental feature, separate from BLE fixes

### PR Planning

See GitHub issue #39 for the recommended PR split. Key point: BLE fixes should
be separate from leaf proxy feature. Coalescing commits need rework before upstream.

## ESP32-S3 Known Limitations

- BLE L2CAP MTU: 512 bytes (hardware constraint)
- FilterAnnounce (1071 bytes wire) exceeds ESP32 MTU — FMP has no fragmentation
- Bloom filter MTU skip fix deployed (`9b86483`) — ESP32 receives data but
  `their_index=00000000` (firmware never sends data back)
- See GitHub issue #43 for ESP32 firmware action items
