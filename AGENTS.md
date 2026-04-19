# Agent Coordination Notes

Notes for LLM agents working on this codebase. Not user documentation.

## Protocol Implementation Reference

Sufficient context to build a third-party FIPS implementation (ESPHome, microfips,
etc.) from the code + linked issues. For formal protocol specs, see `docs/design/`.

### FMP Wire Format

Source: `src/node/wire.rs`

Every FMP frame starts with a 4-byte common prefix:

```
[0]      version (currently 0)
[1]      phase: 0x0=established, 0x1=msg1, 0x2=msg2
[2..3]   payload_len (u16 BE)
```

**Critical: `payload_len` has phase-dependent meaning** (caused 3 independent bugs — see Amperstrand/fips#21, #22):

| Phase | payload_len means | Total wire size |
|-------|-------------------|-----------------|
| 0x0 (established) | Inner plaintext only (excludes 16-byte AEAD tag) | 16 + payload_len + 16 |
| 0x1 (msg1) | Total post-prefix payload | 4 + payload_len (= `MSG1_WIRE_SIZE` = 110) |
| 0x2 (msg2) | Total post-prefix payload | 4 + payload_len (= `MSG2_WIRE_SIZE` = 61) |

After the prefix, established frames have a 12-byte inner header:
```
[0..3]  sender_idx (u32 BE) — our session index
[4..7]  receiver_idx (u32 BE) — peer's session index
[8..11] flags + sequence
```

Code reference: `build_established_header()`, `calculate_frame_len()` in `src/node/wire.rs`.

### Noise Protocol (IK and XK patterns)

Source: `src/noise/handshake.rs`, `src/noise/mod.rs`

FIPS uses Noise IK for link-layer handshake and Noise XK for session-layer rekey.
The IK implementation has **four deviations from the standard Noise specification**
that third-party implementations MUST match:

#### D1: Empty AAD during handshake

Standard Noise uses the running hash `h` as Additional Authenticated Data.
FIPS passes **empty AAD (0 bytes)** during the handshake phase.

Post-handshake: the 16-byte FMP outer header is used as AAD.

Code: `encrypt_and_hash()` / `decrypt_and_hash()` in `src/noise/handshake.rs`.

#### D2: `se` token uses ephemeral, not static

Standard Noise IK defines `se` as `DH(s_initiator, re_responder)`.
FIPS replaces this with `DH(e_initiator, rs_responder)`.

Both sides still agree because:
- Responder computes: `DH(s_responder, e_initiator)` — line ~682
- Initiator computes: `DH(e_initiator, s_responder)` — line ~735

Code: `write_message_2()` and `read_message_2()` in `src/noise/handshake.rs`.

#### D3: SHA-256 of ECDH x-coordinate

Standard Noise uses raw ECDH output. FIPS hashes only the x-coordinate:
`SHA256(shared_x_coordinate)`. This is necessary because Nostr npubs encode
x-only keys without parity — both `P` and `-P` must produce the same shared secret.

Code: `HandshakeState::ecdh()` in `src/noise/handshake.rs`.

#### Parity normalization

Pre-message public key hashing forces the `0x02` (even parity) prefix regardless
of the key's actual parity. This ensures both sides produce matching hash chains
even when the initiator doesn't know the responder's actual parity.

Code: `HandshakeState::normalize_for_premessage()` in `src/noise/handshake.rs`.

See also: Amperstrand/fips#22, `src/noise/tests.rs` (tests `test_handshake_with_odd_parity_responder`, `test_xk_with_odd_parity_responder`).

### Noise IK Handshake Flow

```
Initiator                              Responder
   |                                      |
   |  knows responder's static pubkey     |  (no pre-knowledge)
   |                                      |
   |--- MSG1 (106 bytes) ---------------->|
   |  e (33) + enc(s, 49) + enc(epoch,24) |
   |                                      |
   |<-- MSG2 (57 bytes) -----------------|
   |  e (33) + enc(epoch, 24)             |
   |                                      |
   Both derive session keys, transition to encrypted phase.
```

Key sizes: pubkey=33 (compressed secp256k1), epoch=8, AEAD tag=16.
MSG1 total: 33 + (33+16) + (8+16) = 106 bytes.
MSG2 total: 33 + (8+16) = 57 bytes.

Responder processes MSG1 and generates MSG2 in one step (`receive_handshake_init` in `src/peer/connection.rs`).

### NodeAddr Derivation and Tie-Breaker

Source: `src/peer/mod.rs`, `src/node/local_endpoint.rs`

```
NodeAddr = SHA256(x_only_pubkey)[0..16]
```

Where `x_only_pubkey` is the 32-byte x-coordinate of the secp256k1 public key
(without the 0x02/0x03 parity prefix).

Cross-connection tie-breaker (`cross_connection_winner` in `src/peer/mod.rs`):
- The node with the **smaller** NodeAddr keeps its **outbound** connection.
- Deterministic and symmetric — both nodes reach the same conclusion.
- Applied in BLE `accept_loop` and `scan_probe_loop` in `src/transport/ble/mod.rs`.

See also: Amperstrand/fips#8, #22.

### BLE Pubkey Exchange Protocol

Source: `src/transport/ble/mod.rs` (`pubkey_exchange()`)

Before the Noise handshake, both sides exchange BLE pubkeys over L2CAP:

```
Format: [prefix:1][pubkey:32][flags:1] = 34 bytes total
```

- `prefix`: always `0x00` (`PUBKEY_EXCHANGE_PREFIX`)
- `pubkey`: 32-byte x-only secp256k1 public key
- `flags`: `PeerCapabilities` bitmask (see below)

**Legacy compatibility**: Old nodes send 33 bytes (no flags byte). If the received
message is exactly `PUBKEY_EXCHANGE_SIZE` (33) bytes, assume **full capabilities**.
If 34+ bytes, read capability flags at byte offset 33.

### BLE Peer Capabilities (flags byte)

Source: `src/transport/ble/mod.rs` (`PeerCapabilities`)

The capability flags byte controls BLE role negotiation and tie-breaking:

| Bit | Name | Meaning |
|-----|------|---------|
| 0 | `can_central` | Can act as BLE central (scan + initiate connections) |
| 1 | `can_peripheral` | Can act as BLE peripheral (advertise + accept connections) |
| 2 | `prefer_outbound` | Prefers outbound (central-initiated) connection |
| 3 | `central_only` | Can ONLY act as central — reject inbound connections |

Tie-breaking rules in `accept_loop` and `scan_probe_loop`:
1. If peer is `central_only` → keep connection anyway (it can't accept inbound).
2. If peer `prefer_outbound` and we don't → yield (let peer's outbound win).
3. Otherwise → use NodeAddr comparison (smaller NodeAddr's outbound wins).

### BLE L2CAP Connection Sequence

Source: `src/transport/ble/io.rs`

The Linux BLE connect sequence is:

```
1. BlueZ device.connect()         — GATT-level connect (may retry once on abort-by-local)
2. Discover dynamic PSM via GATT  — read 16-bit LE PSM from service characteristic
3. L2CAP connect to PSM           — open L2CAP CoC channel
4. Pubkey exchange                — 34-byte exchange described above
5. Connection enters pool         — receive_loop starts, available for Noise handshake
```

The macOS BLE connect sequence differs:
- CoreBluetooth publishes a dynamic PSM (e.g., 192) via GATT service
- FIPS discovers this PSM via GATT characteristic read
- Then opens L2CAP CoC to that PSM

**GATT PSM discovery is mandatory** — static PSM (133) is a dead-code fallback
that was removed. ESP32 firmware MUST advertise the GATT PSM service.

See also: Amperstrand/fips#52 (PSM discovery), #65 (GATT retry), #64 (macOS pairing).

### Session Key Derivation After Noise IK

Source: `src/peer/connection.rs` (`receive_handshake_init`)

After `HandshakeState::into_session()`, the session has two derived keys (k1, k2).
FIPS assigns them based on **role**, not order:

- **Initiator**: `send_key = k1, recv_key = k2`
- **Responder**: `send_key = k2, recv_key = k1`

This is the opposite of what standard Noise implementations expect. If you get
this wrong, encrypted frames will fail decryption.

Code: `NoiseSession::new()` in `src/noise/session.rs` and `receive_handshake_init`
in `src/peer/connection.rs:484`.

### MSG1 Processing and Rate Limiting

Source: `src/node/handlers/handshake.rs`

Inbound MSG1 processing has multiple layers of protection:

1. **Global rate limiter**: Token bucket (burst=100, refill=10/s) — limits total
   concurrent handshakes. Code: `HandshakeRateLimiter` in `src/node/rate_limit.rs`.

2. **Per-peer competing MSG1 limit**: `MAX_COMPETING_MSG1_PER_PEER = 5` — each
   remote address can send at most 5 MSG1 without completing the handshake before
   being dropped. Counter resets on successful promotion.

3. **ACL verification**: After Noise decryption reveals the initiator's static
   pubkey, `authorize_peer()` checks against configured allow/deny lists.
   Code: `src/node/mod.rs:1104`.

4. **Epoch-based restart detection**: If the peer already has an active session
   but the decrypted epoch differs, the stale session is torn down and the new
   connection is processed. Same epoch with rekey enabled → rekey flow.

### FSP Port Assignments

Source: `src/protocol/session.rs`

| Port | Purpose |
|------|---------|
| 256 | IPv6 shim — compressed header, dispatches by dst_port |
| Other | End-to-end session data |

### Configuration Defaults Affecting Wire Behavior

Source: `src/config/`, `src/transport/ble/rate_limit.rs`

| Parameter | Default | Where Used |
|-----------|---------|------------|
| `send_rate_bps` | 80,000 | BLE token bucket rate limiter |
| `send_burst_bytes` | 2,048 | BLE burst capacity |
| `mtu` | 1,024 | BLE frame MTU |
| `MAX_RATE_BPS` | 80,000 | AIMD rate ceiling |
| `MIN_RATE_BPS` | 15,000 | AIMD rate floor |
| `MAX_BLE_TCP_WINDOW` | 8,192 | TCP window clamp for BLE paths |
| heartbeat interval | configurable | Link liveness detection |
| link dead timeout | 30s | Remove peer after no heartbeat |

### ESP32 Firmware Requirements

For an ESP32 to interop with this FIPS branch, it MUST:

1. **Accept 2-byte BE length-prefix framing** on L2CAP (send and receive).
2. **Advertise a GATT service** with a PSM characteristic so FIPS can discover
   the dynamic L2CAP PSM (see #52 for the service UUID and characteristic).
3. **Exchange BLE pubkeys** (34-byte format) before the Noise handshake.
4. **Implement Noise IK with D1/D2/D3 deviations** (empty AAD, ephemeral se, SHA256 x-coord).
5. **Handle `payload_len` phase-dependent meaning** (plaintext-only for established frames).
6. **Support 512-byte L2CAP MTU** (hardware constraint on ESP32-S3) — frames >512 can't be received.

See also: Amperstrand/fips#22, #52, #66.

### Design Documentation Index

Formal protocol specs live in `docs/design/`:

| File | Layer | What It Describes |
|------|-------|-------------------|
| `fips-intro.md` | All | Protocol overview and layer model |
| `fips-wire-formats.md` | Wire | FMP frame format tables |
| `fips-transport-layer.md` | Transport | How datagrams are delivered |
| `fips-mesh-layer.md` | Mesh (FMP) | Peer auth, forwarding, routing |
| `fips-session-layer.md` | Session (FSP) | End-to-end sessions |
| `fips-ipv6-adapter.md` | IPv6 | TUN/NPUB mapping, MTU |
| `fips-bloom-filters.md` | Routing | Bloom filter math and routing |
| `fips-spanning-tree.md` | Routing | Spanning tree, root discovery |
| `fips-mesh-operation.md` | Mesh | Routing, discovery, error handling |
| `fips-gateway.md` | Gateway | LAN gateway integration |
| `fips-configuration.md` | Config | YAML config reference |

Start with `fips-intro.md`, then read by layer.

### Notepad References

Internal design notes in `.sisyphus/notepad/`:

| File | What It Documents |
|------|-------------------|
| `ble-framing-architecture.md` | Definitive history of 2-byte BE framing decision |
| `reverse-ble-debugging.md` | BLE reverse-direction debugging |
| `macos-ble-connection.md` | macOS↔Linux BLE connectivity success report |
| `hardcoded-epoch-bug.md` | Epoch handling bug and fixes |

### Key Issue References for Protocol Behaviors

| Issue | Protocol Behavior | Status |
|-------|-------------------|--------|
| Amperstrand/fips#22 | ESPHome findings: payload_len, D1/D2/D3, NodeAddr, MSG1 flood | Closed — all documented in code |
| Amperstrand/fips#21 | payload_len ambiguity (caused 3 independent bugs) | Closed — documented in `wire.rs` |
| Amperstrand/fips#8 | Tie-breaker needs NodeAddr comparison | Closed — implemented |
| Amperstrand/fips#52 | BLE GATT PSM discovery protocol needs documentation | Open |
| Amperstrand/fips#66 | ESP32 MTU limitation (512B), bloom filter skip | Open — firmware blocked |
| Amperstrand/fips#64 | CoreBluetooth peripheral rejects SMP pairing | Open — OS limitation, not a blocker |
| Amperstrand/fips#65 | GATT-first connect abort-on-local retry | Closed — fixed with retry |
| Amperstrand/fips#63 | xHCI controller death after bluetooth restart | Open — kernel/hardware issue, not reproducible |
| Amperstrand/fips#55 | Dual-role tie-breaker deadlock | Closed — yield path fixed |
| Amperstrand/fips#50 | TCP burst-stall over BLE | Closed — WS stripping + window clamp |
| Amperstrand/fips#24 | BLE L2CAP framing retrospective | Closed — documented |
| Amperstrand/fips#7 | LePublic → LeRandom address type | Closed — auto-detect for local, LeRandom for remote |

### BLE Adapter Recovery Playbook (Linux)

If the BLE adapter becomes unresponsive (HCI commands timeout, `btmgmt` returns
`Authentication Failed`, or `dmesg` shows `Opcode 0xNNNN failed: -110`):

| Level | Method | Command |
|-------|--------|---------|
| 1 | Daemon restart (safest) | `sudo pkill -9 bluetoothd` |
| 2 | HCI reset | `sudo hciconfig hci0 down && sleep 2 && sudo hciconfig hci0 up` |
| 3 | USB authorized toggle | `echo 0 \| sudo tee /sys/bus/usb/devices/1-10/authorized; sleep 3; echo 1 \| sudo tee /sys/bus/usb/devices/1-10/authorized` |
| 4 | PCI FLR reset | `echo 1 \| sudo tee /sys/bus/pci/devices/0000:07:00.3/reset` |
| 5 | Reboot | `sudo reboot` |

**Do NOT** use `systemctl restart bluetooth` while FIPS BLE is active — while it
usually works fine, it has caused xHCI controller death once (Amperstrand/fips#63).
Use `sudo systemctl stop fips` first, then restart bluetooth, then `sudo systemctl start fips`.

---

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
- xHCI controller death after `systemctl restart bluetooth` (Amperstrand/fips#63) — **not reproducible** after 30+ attempts across 3 sessions. Documented recovery playbook in the issue. Treat as kernel/hardware issue, not FIPS bug.
- CoreBluetooth peripheral mode rejects SMP pairing (Amperstrand/fips#64)
- ~~GATT-first connect sometimes aborts on first attempt (Amperstrand/fips#65)~~ Fixed — retry added

### Issues Closed on This Branch

| Issue | Root Cause | Commit |
|-------|-----------|--------|
| #50 | TCP Window Scale burst-stall + rate limiter disabled | `206908c` |
| #65 | Dead-code static-PSM fallback + no GATT retry | `25a69d3` |
| #14 | Same as #50 (SSH over BLE timeout) | `206908c` |
| #69 | BLE throughput (WS burst-stall) | `206908c` |
| #7 | `LePublic` → `LeRandom` everywhere (BLE socket addresses) | `60e2dc9`, `7448b63` |
| #10 | Unused `BleDeviceAddr` imports already cleaned | prior commit |
 | #22 | Protocol documentation gaps (payload_len, D1/D2/D3, NodeAddr, MSG1 flood) | `0f5e2c0` |
| #24 | BLE L2CAP framing retrospective (2-byte BE prefix history + downstream fixes) | multiple |

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
