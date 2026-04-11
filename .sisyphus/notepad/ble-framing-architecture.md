# BLE Framing Architecture — Definitive Reference

**Date**: 2026-04-11 (revised)
**Status**: RESOLVED — framing cleanup applied, see "Resolution" section below

---

## TL;DR

- **Upstream (`jmcorgan/master`)** has NO framing layer in FIPS code. Linux uses raw
  SeqPacket reads, macOS uses channel-based delivery. Both preserve message boundaries
  without any length-prefix framing.
- **Our branch (`linux-ble-stability-v2`)** invented the 2-byte BE length prefix framing
  (commit `42d9adb`) to solve a macOS CoreBluetooth byte-stream coalescing problem.
  This is a **wire protocol change** — both peers must agree on it.
- The 2-byte prefix approach is **architecturally correct for macOS** (byte streams need
  framing) but **unnecessary for Linux** (SeqPacket preserves boundaries).
- Commits `e81d688` and `daa76f1` put FMP-level coalescing/splitting logic in `receive_loop`
  (mod.rs). This is the **wrong layer** — transport framing belongs in the stream
  implementation, not the FMP receive path.

---

## How Upstream Works (No Framing)

### Linux (`jmcorgan/master` — `BluerStream::recv()`)

```rust
// Raw pass-through — no framing, no buffering
async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
    self.conn.recv(buf).await // SeqPacket: one recv() = one L2CAP SDU
}
```

BlueZ SeqPacket sockets preserve L2CAP SDU boundaries. Each `recv()` returns exactly
one complete message. No framing is needed.

### macOS (`jmcorgan/macos-support` — `Bluestream::recv()`)

```rust
// Channel-based delivery
async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
    self.rx.recv().await // Each send() maps to one recv()
}
```

Uses a `tokio::sync::mpsc` channel. Each `write()` from the CoreBluetooth delegate
callback becomes one `rx.recv()`. Message boundaries are preserved by the channel,
not by length-prefix framing.

### Upstream `receive_loop` (`jmcorgan/macos-support` — mod.rs)

Simple pass-through: `Ok(n) => record_recv(n), deliver packet`. No frame splitting,
no coalescing logic. Receives one frame per `stream.recv()` call.

---

## What Our Branch Changed

### Commit `42d9adb` — Added 2-byte BE length prefix framing

**Why**: macOS CoreBluetooth's `NSInputStream` may coalesce L2CAP SDUs into a single
`read()` call. The upstream macOS implementation uses a channel to avoid this, but our
branch's `Bluestream` uses direct `read()` calls which DO coalesce.

**What**: All `send()` paths prepend a 2-byte big-endian length prefix. All `recv()`
paths strip it (or buffer until a complete frame is available).

```
Wire format (our branch only):
  [2-byte BE length prefix] [FMP frame]
```

**Impact**: This is a wire protocol change. Peers running upstream code cannot talk to
peers running our branch (and vice versa) because they speak different wire formats.

### Commit `e81d688` — FMP-level coalescing in receive_loop (WRONG LAYER)

Added `calculate_frame_len()` and frame-splitting logic in `receive_loop` (mod.rs)
that walks raw bytes using FMP header structure to find frame boundaries.

**Why it was added**: After the 2-byte prefix was added but `recv_buf` was removed from
`BluerStream` (between sessions), there was no buffering. If the ESP32 ever coalesced
multiple sends into one recv(), the data would be corrupted. This was a band-aid.

**Why it's wrong**: FMP layer should not know about transport framing. If coalescing
needs handling, it belongs in the stream implementation (`BluerStream::recv()`).

### Commit `daa76f1` — `calculate_frame_len()` for handshake vs established frames

Added logic to determine FMP frame length from the 4-byte prefix, handling both
handshake messages (different prefix structure) and established frames.

**Why it's wrong**: Depends on `e81d688`. Same layering violation.

### Commit `e6311be` — Fix broken match arm from partial revert

Bugfix only. During investigation of issue #42, someone partially reverted `Ok(Ok(n))`
to `Ok(n)` which broke compilation. Fixed the match arm and extra closing braces.

---

## The Correct Architecture

### For Linux (SeqPacket)

```
FMP frame: [4-byte prefix] [payload] [16-byte AEAD tag]
  ↓ raw write to SeqPacket socket
L2CAP SeqPacket socket (preserves boundaries)
```

No framing needed. Each `send()` = one SDU. Each `recv()` = one SDU.

### For macOS (byte stream)

```
FMP frame: [4-byte prefix] [payload] [16-byte AEAD tag]
  ↓ wrap with length prefix (if using direct read())
BLE frame: [2-byte BE length prefix] [FMP frame]
  ↓ write to NSOutputStream
CoreBluetooth NSOutputStream (byte stream — may coalesce/fragment)
```

OR (upstream's approach, which avoids the problem entirely):

```
FMP frame: [4-byte prefix] [payload] [16-byte AEAD tag]
  ↓ send through mpsc channel
CoreBluetooth delegate callback → channel → recv()
```

The channel approach avoids needing length-prefix framing because each delegate
callback delivers one SDU, and the channel preserves that one-to-one mapping.

### The Key Insight

Upstream's macOS implementation uses a **channel** to bridge CoreBluetooth's delegate
callbacks to async Rust, and this naturally preserves message boundaries. Our branch's
macOS implementation uses direct `read()` on the `NSInputStream`, which does NOT
preserve boundaries — hence the need for length-prefix framing.

The 2-byte prefix is not inherently wrong for macOS. It's the correct solution for
byte-stream transports. But it's a **wire protocol change** that makes our branch
incompatible with upstream.

---

## Current State of the Code

| File | What it does | Correct? |
|------|-------------|----------|
| `io.rs` BluerStream::send() | Prepends 2-byte BE length prefix | Required for ESP32 interop |
| `io.rs` BluerStream::recv() | Has recv_buf, strips 2-byte prefix | Correct — handles potential coalescing |
| `io_macos.rs` Bluestream::send() | Prepends 2-byte BE length prefix | Correct for byte streams |
| `io_macos.rs` Bluestream::recv() | Buffers and reassembles on 2-byte prefix | Correct for byte streams |
| `mod.rs` receive_loop | Simple pass-through (one recv = one frame) | ✅ Correct — FMP-level coalescing removed |

---

## What Should Happen (Long-Term Solution)

### Option A: Revert to upstream approach (recommended)

Remove the 2-byte length prefix entirely. On Linux, SeqPacket preserves boundaries.
On macOS, use the channel-based approach from upstream's `jmcorgan/macos-support`.

**Changes needed:**
1. Revert 2-byte prefix from all send/recv paths (io.rs, io_macos.rs)
2. Revert `e81d688` + `daa76f1` (FMP-level coalescing in receive_loop)
3. Align macOS implementation with upstream's channel-based approach
4. Result: wire-compatible with upstream, no framing confusion

### Option B: Keep 2-byte prefix but fix layering

If there's a reason to keep the prefix (e.g., future non-SeqPacket transports),
keep the send/recv framing but move ALL framing logic to the stream layer.

**Changes needed:**
1. Keep 2-byte prefix in send/recv paths
2. Revert `e81d688` + `daa76f1` (remove FMP-level coalescing)
3. Add recv_buf back to BluerStream::recv() for safety (handles edge cases)
4. Result: framing is correct layering, but wire-incompatible with upstream

### Decision criteria

- If goal is to merge upstream → Option A
- If goal is independent transport evolution → Option B

---

## Rules for Future Agents

1. **NEVER put transport framing logic in mod.rs receive_loop.** The receive_loop
   should receive one complete FMP frame per `stream.recv()` call and deliver it.

2. **Frame reassembly (if needed) belongs in the BleStream implementation** (io.rs
   for Linux, io_macos.rs for macOS).

3. **The FMP layer should not know about transport framing.** FMP frames are
   self-describing (4-byte prefix), but receive_loop should not parse them for
   framing purposes.

4. **If you see "split coalesced frames" in receive_loop logs**, that logic should
   not be there. It was added in `e81d688` as a workaround and should be removed.

5. **The 2-byte BE length prefix is our branch's invention, not upstream's.**
   Upstream has no framing layer. This is a wire protocol change that affects
   interoperability.

---

## Resolution (2026-04-11)

### What was done

1. **Removed FMP-level coalescing** from `receive_loop` (mod.rs). Commits `e81d688`
   and `daa76f1` put frame-splitting logic in the wrong layer. This was removed and
   receive_loop now does simple pass-through (one recv = one delivered frame).
2. **Restored 2-byte BE length prefix on all platforms** (commit `349ad15`).
   Linux was briefly switched to no-prefix, but the ESP32 firmware requires it.
3. **Added recv_buf to Linux BluerStream** for safety — handles potential coalescing
   from BLE controllers.

### Why the 2-byte prefix was kept on Linux

The original plan was to remove it (SeqPacket preserves boundaries, so it's
unnecessary). But the ESP32 firmware **always** expects and sends 2-byte prefix
framing on all L2CAP data (pubkey exchange, handshake, FMP frames). Without
framing on the Linux side, ESP32 can never parse incoming data.

Since Linux↔ESP32 interop is a primary use case, all platforms now use the prefix.
This makes the branch wire-incompatible with upstream, but that's acceptable:
- Upstream BLE was not in production
- The ESP32 firmware can't be changed to remove the prefix easily
- macOS needs it anyway (byte streams)

### Decision: Modified Option B

Original Option B was "keep prefix but fix layering." That's what we did:
- ✅ 2-byte prefix on all platforms (Linux, macOS, Mock)
- ✅ FMP-level coalescing removed from receive_loop
- ✅ recv_buf added to Linux BluerStream for safety
- ❌ Wire-incompatible with upstream (accepted tradeoff)
