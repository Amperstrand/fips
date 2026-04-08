# Session Handoff: macOS BLE FIPS Implementation

**Date**: 2026-04-08
**Branch**: `integrate/macos-linux-sync`
**Latest Commit**: `38e2473 fix(ble-macos): gate Linux bluer paths by target OS`

---

## Executive Summary

macOS BLE transport implementation is **complete** and **functional at the L2CAP layer**. Mac can:
- Scan for FIPS peers ✅
- Discover Linux nodes ✅  
- Establish L2CAP channels ✅
- Complete pubkey exchange ✅

**Blocker**: Peers don't persist in the mesh. Connection closes after probe completes with "yielding to peer's outbound" message. No authenticated peers appear in `fipsctl show peers`.

---

## Current State

### What Works

| Component | Status | Evidence |
|-----------|--------|----------|
| BLE scanning | ✅ | Discovers Linux UUID `B7E8F33F-D2BB-7EAF-3263-8E214B2DB79B` |
| L2CAP connection | ✅ | PSM 133, MTU 2048 established |
| Pubkey exchange | ✅ | 33 bytes sent/received, ~97-140ms |
| TUN interface | ✅ | `utun4/5` created with IPv6 fd00::/8 |
| DNS responder | ✅ | Port 5354 listening |
| Control socket | ⚠️ | Works with sudo only |

### What Doesn't Work

| Issue | Symptom | Likely Cause |
|-------|---------|--------------|
| No persistent peers | `fipsctl show peers` = 0 | Connection closed after probe |
| Yielding loop | "BLE probe tie-breaker: yielding to peer's outbound" | Race condition in cross-connection detection |
| Session never established | No Noise IK handshake | Node layer rejects connection after probe |

---

## Architecture Reality

### Platform Constraints (Immutable)

**macOS (CoreBluetooth via bluest)**:
- ✅ Central role: scan, connect, L2CAP client
- ❌ Peripheral role: NO advertise, NO listen, NO accept
- Uses UUID-based addresses (privacy feature)
- Single adapter ("default")

**Linux (BlueZ via bluer)**:
- ✅ Central + Peripheral roles
- Uses MAC addresses
- Multiple adapters (hci0, hci1, ...)

### Required Flow for Mac ↔ Linux

```
Mac (Central)                      Linux (Peripheral)
     |  1. Scan for FIPS UUID           |
     |---------------------------------->|
     |  2. Discover Linux               |
     |  3. Connect outbound ONLY        |
     |---------------------------------->|
     |  4. L2CAP PSM 133                |
     |  5. Pubkey exchange (Mac first)  |
     |---------------------------------->|
     |  6. Noise IK handshake           |
     |<--------------------------------->|
     |  7. Authenticated peer           |
```

---

## Test Files & Configs

### Test Scripts
- `/tmp/run-fips-ble.sh` - Run Mac FIPS with sudo (needs TUN permissions)
- `/tmp/fips-ble.log` - Log output (sudo tee)
- `/tmp/linuxprompt.md` - Instructions for Linux side

### Config Files (in repo)
- `ble-macos-full.yaml` - Mac with TUN+DNS+BLE (scan, auto_connect)
- `ble-macos-connect.yaml` - Mac BLE-only
- `ble-linux-full.yaml` - Linux with TUN+DNS+BLE (advertise, accept)
- `ble-linux-advertise.yaml` - Linux BLE-only

### Test Commands

```bash
# On Mac (with sudo)
sudo /tmp/run-fips-ble.sh

# Check status (needs sudo)
sudo /tmp/fips-target/release/fipsctl show peers
sudo /tmp/fips-target/release/fipsctl show links
sudo /tmp/fips-target/release/fipsctl show status

# Check logs
tail -f /tmp/fips-ble.log | grep -E "(peer|Link|auth|complete)"
```

---

## Recent Fixes Pulled

```
38e2473 fix(ble-macos): gate Linux bluer paths by target OS
4cb3807 fix(macos): link CoreFoundation for BLE run loop
808214c fix(ble): add per-peer competing MSG1 rate limit
516fe87 fix(ble): notify Node on L2CAP disconnect for immediate session reset
72979b8 fix(node): detect and remove stale peers before re-handshake
d032728 fix(ble): block outbound pool insert after cross-connection resolution
ea375c7 fix(ble): close stale outbound transport socket on cross-connection detection
```

These fix cross-connection races and add proper cleanup, but **persistent peer issue remains**.

---

## Key Files to Understand

| File | Lines | Purpose |
|------|-------|---------|
| `src/transport/ble/bluest.rs` | ~250 | macOS BLE I/O (central-only) |
| `src/transport/ble/mod.rs` | ~1559 | BLE transport logic, pubkey exchange, probe/accept loops |
| `src/transport/ble/addr.rs` | - | BleDeviceAddr (Mac vs UUID) |
| `src/node/tests/ble_macos.rs` | 75 | 7 integration tests |
| `.sisyphus/plans/macos-ble-status.md` | 401 | Detailed status doc |
| `.sisyphus/plans/macos-ble.md` | 1235 | Full implementation plan |

---

## Known Issues

### 1. Yielding Loop

**Symptom**:
```
BLE probe complete addr=hci0/B7E8F33F-...
BLE probe tie-breaker: yielding to peer's outbound
<connection closes>
<repeats every 5 seconds>
```

**Analysis**: 
- Mac completes probe successfully
- Tie-breaker decides to yield
- Connection closes instead of promoting to authenticated peer
- Linux may also be probing Mac, causing race

**Fix Needed**: Investigate why yielded connection doesn't result in inbound connection being kept

### 2. Control Socket Permissions

**Symptom**: `fipsctl` fails with "Permission denied"

**Workaround**: Run with sudo

**Fix**: Change socket ownership or use group permissions

### 3. No Inbound Connections (Mac)

**Constraint**: CoreBluetooth limitation, not fixable

**Implication**: Mac can NEVER accept connections, only initiate

---

## Next Steps for Future Sessions

### Immediate Debugging

1. **Check Linux side**:
   - Is Linux also probing Mac?
   - What do Linux logs show when Mac connects?
   - Is Linux's tie-breaker logic complementary?

2. **Add more logging**:
   ```rust
   // In mod.rs, after tie-breaker decision
   debug!(
       addr = %addr,
       our_addr = ?our_node_addr,
       peer_addr = ?peer_node_addr,
       decision = ?decision,
       "BLE probe tie-breaker decision"
   );
   ```

3. **Test with manual peer addition**:
   ```bash
   # Add Linux as static peer on Mac
   fipsctl peer add npub1... --transport ble --addr hci0/B7E8F33F-...
   ```

### Medium-Term

4. **Fix the yielding issue**:
   - File: `src/transport/ble/mod.rs` 
   - Lines: ~700-850 (probe loop, tie-breaker)
   - Issue: Connection not promoted after yield

5. **Test full mesh connectivity**:
   - After peers persist: `ping6 <npub>.fips`
   - Then: `ssh -6 <npub>.fips`

6. **Document the platform asymmetry**:
   - Update README with "Mac = central only" note
   - Add architecture diagram

### Long-Term

7. **ESP32 cross-probe issue** (see status doc lines 218-224)
8. **Investigate CoreBluetooth L2CAP flakiness**
9. **Consider alternative: TCP over BLE (if L2CAP unreliable)**

---

## How to Resume This Work

### Quick Start

```bash
# 1. Pull latest
git pull origin integrate/macos-linux-sync

# 2. Rebuild
CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble-macos

# 3. Run (needs sudo for TUN)
sudo /tmp/run-fips-ble.sh

# 4. In another terminal, check logs
tail -f /tmp/fips-ble.log | grep -E "(peer|complete|yield)"

# 5. Check peers (needs sudo)
sudo /tmp/fips-target/release/fipsctl show peers
```

### Linux Side Setup

Give the Linux LLM `/tmp/linuxprompt.md` which has:
- Required commit hash
- Config file contents
- Run commands
- Debug checklist

---

## Valuable Context Files

### Already Committed
- `.sisyphus/plans/macos-ble-status.md` - Full status (401 lines)
- `.sisyphus/plans/macos-ble.md` - Implementation plan (1235 lines)
- `ble-macos-*.yaml` - Config files
- `ble-linux-*.yaml` - Config files

### Should Commit
- This handoff document (move to `.sisyphus/notepads/`)

### Temporary (don't commit)
- `/tmp/run-fips-ble.sh` - Recreate as needed
- `/tmp/fips-ble.log` - Log file
- `/tmp/linuxprompt.md` - Regenerate from configs

---

## Critical Insights

1. **Mac = Central Only**: This is a hard constraint. All designs must account for this.
2. **Tie-breaker is key**: Mac↔Linux communication depends on correct asymmetric yielding
3. **L2CAP works**: The transport layer is solid; issue is in node/peer management
4. **Pubkey exchange works**: 33-byte frames exchange correctly
5. **Close but not quite**: One fix away from working mesh peering

---

## Questions for Next Session

1. Why does yielded connection close instead of waiting for inbound?
2. Is Linux's tie-breaker logic checking the same comparison (>=)?
3. Should Mac even run tie-breaker if it can never accept inbound?
4. Would static peer configuration bypass the probe/yield issue?

---

## Related Issues

- ESP32 cross-probe handshake confusion (status doc line 218)
- microfips issue #63 (ESP32 leaf node identification)
- CoreBluetooth L2CAP CoC reliability

---

**Status**: 🔶 L2CAP works, mesh peering blocked by tie-breaker/yield issue  
**Confidence**: High - transport layer is solid, issue is in peer state machine  
**Next Action**: Debug tie-breaker logic on both Mac and Linux sides simultaneously
