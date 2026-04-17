# BLE Reverse Direction: Linux (central) → Mac (peripheral)

## TL;DR

> **Quick Summary**: Test the Mac peripheral BLE path by having Linux connect to Mac as central, proving Mac-to-Mac BLE would work. Requires fixing macOS capability flags to be config-driven (matching Linux pattern) and deploying reverse-direction configs to both nodes.
> 
> **Deliverables**:
> - macOS BLE capabilities made conditional on `accept_connections` config (matching Linux pattern)
> - `macos_default()` restored with `CAN_PERIPHERAL` flag
> - Hardware test: Linux discovers Mac's GATT PSM, connects inbound, ping works
> - Original configs restored and forward direction re-verified
> 
> **Estimated Effort**: Medium
> **Parallel Execution**: YES - 3 waves
> **Critical Path**: Task 1 → Task 2+3 → Task 4 → Task 5 → Task 6

---

## Context

### Original Request
Test the BLE reverse direction — Mac as peripheral, Linux as central — to prove Mac-to-Mac BLE connectivity would work. We only have one Mac and one Linux box, so Linux stands in for the second Mac.

### Interview Summary
**Key Discussions**:
- Current direction works: Mac (central) → Linux (peripheral) — verified with 20/20 pings at 109ms avg
- The GATT PSM exchange infrastructure is fully implemented on both platforms
- macOS `listen()` already publishes L2CAP + GATT service (io_macos.rs:853-989) — the code exists
- `macos_default()` at commit `bfea948` removed `CAN_PERIPHERAL` claiming "CoreBluetooth cannot accept inbound L2CAP"
- But the code contradicts this — full `CBPeripheralManager` peripheral implementation exists
- User doesn't know why `accept_connections: false` was set — likely a workaround, not a true limitation

**Research Findings**:
- Linux capability assignment is config-driven (node/mod.rs:860-864): uses `central_only()` when `!accept_connections`
- macOS capability assignment is hardcoded (node/mod.rs:889-891): always uses `macos_default()`, ignores config
- The tie-breaker at mod.rs:1326-1332 yields if peer can't accept inbound — so without `CAN_PERIPHERAL`, Linux won't connect to Mac
- `io_macos.rs` file header says "central role AND peripheral role" — both were intended

### Metis Review
**Identified Gaps** (addressed):
- **bfea948 contradiction**: CAN_PERIPHERAL was removed but peripheral code exists and is complete. Resolution: the hardware test 2 failure ("both sides became outbound-only with no listener") was a tie-breaker logic issue, not a CoreBluetooth limitation. Making capabilities config-driven fixes both directions.
- **Tie-breaker regression risk**: Adding CAN_PERIPHERAL unconditionally to macos_default() would change behavior for ALL macOS configs. Resolution: make it conditional on `accept_connections`, matching the Linux pattern.
- **Config rollback**: Must restore original configs after test. Resolution: backup step included.
- **ESP32 disruption**: Linux changing to central-only mode means ESP32 devices can't connect. Resolution: test is temporary, restore original config after.
- **Adapter name**: Linux adapter is `hci1` (confirmed in AGENTS.md context), not `hci0`.

---

## Work Objectives

### Core Objective
Prove that macOS can accept inbound BLE L2CAP connections (peripheral role) by having Linux connect to Mac as central, using GATT PSM discovery. This validates the Mac-to-Mac BLE path.

### Concrete Deliverables
- Modified `node/mod.rs`: macOS capability selection conditional on `accept_connections` config
- Modified `mod.rs`: `macos_default()` includes `CAN_PERIPHERAL` flag
- Updated unit tests for new capability behavior
- Successful hardware test: Linux→Mac BLE ping (20/20 packets)

### Definition of Done
- [ ] `cargo test --features ble` passes (all 1029+ tests)
- [ ] `cargo build --release --features ble` succeeds on Linux
- [ ] Hardware: Linux discovers Mac GATT PSM, connects, ping 20/20
- [ ] Forward direction (Mac→Linux) still works after config rollback

### Must Have
- macOS capabilities conditional on `accept_connections` config
- `CAN_PERIPHERAL` in `macos_default()` 
- Backup of both configs before test
- Rollback to original configs after test
- Linux runs FIPS as systemd service (NEVER background process)

### Must NOT Have (Guardrails)
- Do NOT change the tie-breaker algorithm itself — only change which capabilities are advertised
- Do NOT change BLE framing logic (2-byte BE length prefix)
- Do NOT touch ESP32 firmware or ESP32 peer configs
- Do NOT change `central_only()` preset — it stays as-is
- Do NOT make permanent config changes — this is a test, restore after
- Do NOT add new dependencies or new files beyond test configs
- Do NOT over-engineer — this is a minimal change to enable testing

---

## Verification Strategy (MANDATORY)

> **ZERO HUMAN INTERVENTION** for verification — ALL checks are agent-executed. 
> Exception: User will manually restart Mac FIPS (documented as a user action step).

### Test Decision
- **Infrastructure exists**: YES (cargo test, 1029+ tests)
- **Automated tests**: YES (tests-after) — update existing capability tests
- **Framework**: cargo test (Rust built-in)

### QA Policy
Every task MUST include agent-executed QA scenarios.
Evidence saved to `.sisyphus/evidence/task-{N}-{scenario-slug}.{ext}`.

- **Code changes**: Use Bash (cargo test) — run tests, verify compilation
- **Hardware test**: Use Bash (SSH to Linux) — monitor journalctl logs, run fipsctl commands
- **Config changes**: Use Bash (SSH) — verify config syntax, service restart

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Start Immediately — code change):
├── Task 1: Make macOS capabilities config-driven + restore CAN_PERIPHERAL [quick]

Wave 2 (After Wave 1 — configs + build, parallel):
├── Task 2: Build on Mac (cargo build) [quick]
├── Task 3: Build on Linux + deploy binary (SSH) [quick]

Wave 3 (After Wave 2 — deploy configs + test, sequential):
├── Task 4: Deploy reverse-direction configs to both nodes [quick]
├── Task 5: Run hardware test with staged checkpoints [deep]

Wave 4 (After Wave 3 — rollback):
├── Task 6: Restore original configs and verify forward direction [quick]

Wave FINAL (After ALL tasks):
├── Task F1: Plan compliance audit (oracle)
├── Task F2: Code quality review (unspecified-high)
├── Task F3: Real manual QA (unspecified-high)
├── Task F4: Scope fidelity check (deep)
-> Present results -> Get explicit user okay

Critical Path: Task 1 → Task 2+3 → Task 4 → Task 5 → Task 6 → F1-F4
Parallel Speedup: Wave 2 runs 2 tasks in parallel
```

### Dependency Matrix

| Task | Depends On | Blocks | Wave |
|------|-----------|--------|------|
| 1 | — | 2, 3, 4, 5, 6 | 1 |
| 2 | 1 | 4, 5 | 2 |
| 3 | 1 | 4, 5 | 2 |
| 4 | 2, 3 | 5 | 3 |
| 5 | 4 | 6 | 3 |
| 6 | 5 | F1-F4 | 4 |

### Agent Dispatch Summary

- **Wave 1**: 1 task — T1 → `quick`
- **Wave 2**: 2 tasks — T2 → `quick`, T3 → `quick`
- **Wave 3**: 2 tasks — T4 → `quick`, T5 → `deep`
- **Wave 4**: 1 task — T6 → `quick`
- **FINAL**: 4 tasks — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [x] 1. Make macOS BLE capabilities conditional on `accept_connections` config

  **What to do**:
  - In `src/transport/ble/mod.rs`, add `CAN_PERIPHERAL` back to `macos_default()`:
    ```rust
    pub fn macos_default() -> Self {
        Self(
            Self::L2CAP_SUPPORTED
                | Self::CAN_CENTRAL
                | Self::CAN_PERIPHERAL  // ← ADD THIS BACK
                | Self::GATT_SUPPORTED
                | Self::PREFER_OUTBOUND,
        )
    }
    ```
  - Update the comment above `macos_default()` (lines 847-850) to reflect that macOS CAN accept inbound L2CAP via `publishL2CAPChannel` / `CBPeripheralManager`. The old comment said "cannot accept inbound L2CAP" which is wrong — the code at `io_macos.rs:853-989` proves it can.
  - In `src/node/mod.rs`, modify the macOS BLE path (lines 873-892) to conditionally set capabilities based on `accept_connections` config, matching the Linux pattern at lines 860-864:
    ```rust
    #[cfg(all(feature = "ble-macos", not(test)))]
    for (name, ble_config) in ble_instances {
        let transport_id = self.allocate_transport_id();
        let adapter = ble_config.adapter().to_string();
        let mtu = ble_config.mtu();
        let accept_connections = ble_config.accept_connections();  // ← ADD
        match crate::transport::ble::io::BluestIo::new(&adapter, mtu, ble_config.send_rate_bps(), ble_config.send_burst_bytes()).await {
            Ok(io) => {
                let mut ble = crate::transport::ble::BleTransport::new(
                    transport_id, name, ble_config, io, packet_tx.clone(),
                );
                ble.set_disconnect_tx(disconnect_tx.clone());
                ble.set_local_pubkey(self.identity.pubkey().serialize());
                if !accept_connections {                              // ← ADD
                    ble.set_local_capabilities(                       // ← ADD
                        crate::transport::ble::PeerCapabilities::central_only(), // ← ADD
                    );                                                // ← ADD
                } else {                                              // ← ADD
                    ble.set_local_capabilities(                       // ← ADD
                        crate::transport::ble::PeerCapabilities::macos_default(), // ← ADD
                    );                                                // ← ADD
                }                                                     // ← ADD
                transports.push(TransportHandle::Ble(ble));
            }
            ...
        }
    }
    ```
  - Update the `test_tiebreaker_central_only_overrides_node_addr` test (around line 1808) and `test_gatt_supported_flag_encoding` (around line 1499) to reflect that `macos_default()` now includes `CAN_PERIPHERAL`. Specifically:
    - `macos_default().can_accept_inbound()` should now return `true`
    - `macos_default().is_central_only()` should now return `false`
    - The byte value changes from `0x6a` to `0x7a` (adding `CAN_PERIPHERAL = 0x10`)
  - Run `cargo test --features ble` to verify all tests pass

  **Must NOT do**:
  - Do NOT change the tie-breaker algorithm (`should_we_connect` logic or `accept_loop` tie-breaker)
  - Do NOT change `central_only()` preset
  - Do NOT change `linux_default()` preset
  - Do NOT change BLE framing
  - Do NOT change the Linux capability path (lines 843-865)

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Single focused code change across 2 files, following an existing pattern
  - **Skills**: [`git-master`]
    - `git-master`: Clean atomic commit with proper message

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 1 (solo)
  - **Blocks**: Tasks 2, 3, 4, 5, 6
  - **Blocked By**: None (can start immediately)

  **References** (CRITICAL):

  **Pattern References**:
  - `src/node/mod.rs:843-865` — Linux BLE capability conditional pattern — COPY THIS EXACT PATTERN for macOS. Lines 848 reads `accept_connections`, lines 860-864 conditionally set `central_only()`.
  - `src/node/mod.rs:873-892` — macOS BLE path TO MODIFY — currently hardcodes `macos_default()` at line 889-891

  **API/Type References**:
  - `src/transport/ble/mod.rs:846-857` — `macos_default()` definition TO MODIFY — add `CAN_PERIPHERAL` flag
  - `src/transport/ble/mod.rs:817-827` — `PeerCapabilities` flag constants — `CAN_PERIPHERAL = 0x10`
  - `src/transport/ble/mod.rs:859-862` — `is_central_only()` and `can_accept_inbound()` — these will change behavior for `macos_default()`
  - `src/config/transport.rs:604-606` — `ble_config.accept_connections()` method to call

  **Test References**:
  - `src/transport/ble/mod.rs:1499-1510` — `test_gatt_supported_flag_encoding` — update expected values for `macos_default()`
  - `src/transport/ble/mod.rs:1808-1852` — `test_tiebreaker_central_only_overrides_node_addr` — update assertions since `macos_default()` is no longer central-only

  **WHY Each Reference Matters**:
  - `node/mod.rs:843-865` — This IS the pattern to copy. Linux reads config, conditionally sets capabilities. macOS must do the same.
  - `mod.rs:846-857` — The actual function to modify. Adding one constant changes the wire-visible capability byte.
  - Test files — Assertions about `macos_default()` properties will fail if not updated.

  **Acceptance Criteria**:

  - [ ] `cargo test --features ble` → PASS (all tests pass)
  - [ ] `macos_default().to_byte()` returns `0x7a` (was `0x6a`)
  - [ ] `macos_default().can_accept_inbound()` returns `true`
  - [ ] `macos_default().is_central_only()` returns `false`

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: macos_default includes CAN_PERIPHERAL
    Tool: Bash (cargo test)
    Preconditions: Code changes applied to mod.rs and node/mod.rs
    Steps:
      1. Run: cargo test --features ble test_gatt_supported_flag_encoding
      2. Verify test passes with macos_default().to_byte() == 0x7a
      3. Run: cargo test --features ble test_tiebreaker
      4. Verify all tie-breaker tests pass
    Expected Result: All tests pass, 0 failures
    Failure Indicators: Test assertion failures mentioning 0x6a or is_central_only
    Evidence: .sisyphus/evidence/task-1-capability-tests.txt

  Scenario: Full test suite passes
    Tool: Bash (cargo test)
    Preconditions: All code changes applied
    Steps:
      1. Run: cargo test --features ble 2>&1
      2. Count total tests, verify 0 failures
    Expected Result: 1029+ tests pass, 0 failures
    Failure Indicators: "FAILED" in output, non-zero exit code
    Evidence: .sisyphus/evidence/task-1-full-test-suite.txt

  Scenario: Conditional capability for accept_connections=false
    Tool: Bash (grep)
    Preconditions: node/mod.rs modified
    Steps:
      1. Verify node/mod.rs macOS path reads accept_connections from ble_config
      2. Verify conditional: if !accept_connections → central_only()
      3. Verify else branch → macos_default()
    Expected Result: Pattern matches Linux path at lines 860-864
    Failure Indicators: Hardcoded macos_default() without conditional
    Evidence: .sisyphus/evidence/task-1-conditional-check.txt
  ```

  **Commit**: YES
  - Message: `fix(ble): make macOS capabilities conditional on accept_connections config`
  - Files: `src/node/mod.rs`, `src/transport/ble/mod.rs`
  - Pre-commit: `cargo test --features ble`

- [x] 2. Build on Mac

  **What to do**:
  - Pull latest commit from git (the one from Task 1)
  - Run: `cargo build --release --features ble-macos`
  - Verify binary exists at `target/release/fips`

  **Must NOT do**:
  - Do NOT install the binary yet (Task 4 handles deployment)
  - Do NOT modify any code

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Single build command
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Task 3)
  - **Blocks**: Tasks 4, 5
  - **Blocked By**: Task 1

  **References**:
  - AGENTS.md — Build instructions for macOS

  **Acceptance Criteria**:
  - [ ] `cargo build --release --features ble-macos` exits 0
  - [ ] `target/release/fips` binary exists

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Mac build succeeds
    Tool: Bash
    Preconditions: Task 1 commit pulled
    Steps:
      1. Run: git pull origin linux-ble-stability-v2
      2. Run: cargo build --release --features ble-macos
      3. Verify: ls -la target/release/fips
    Expected Result: Binary exists, non-zero size, recent timestamp
    Failure Indicators: Compilation errors, missing binary
    Evidence: .sisyphus/evidence/task-2-mac-build.txt
  ```

  **Commit**: NO

- [x] 3. Build and install on Linux (via SSH)

  **What to do**:
  - SSH to Linux: `ssh -i ~/.ssh/id_ed25519_gitlab ubuntu@192.168.13.218`
  - Pull latest commit: `cd ~/src/fips && git pull origin linux-ble-stability-v2`
  - Build: `source ~/.cargo/env && CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble`
  - Install: `sudo cp /tmp/fips-target/release/fips /usr/local/bin/fips && sudo cp /tmp/fips-target/release/fipsctl /usr/local/bin/fipsctl`
  - Do NOT restart service yet (Task 4 handles that)

  **Must NOT do**:
  - Do NOT restart the fips service yet
  - Do NOT modify config yet

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Standard build + install via SSH
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Task 2)
  - **Blocks**: Tasks 4, 5
  - **Blocked By**: Task 1

  **References**:
  - AGENTS.md — Linux Node Operations section: build command, install command, SSH access

  **Acceptance Criteria**:
  - [ ] `cargo build --release --features ble` exits 0 on Linux
  - [ ] `/usr/local/bin/fips` updated with new binary

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Linux build and install
    Tool: Bash (SSH)
    Preconditions: Task 1 commit available on remote
    Steps:
      1. SSH to Linux
      2. cd ~/src/fips && git pull origin linux-ble-stability-v2
      3. source ~/.cargo/env && CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble
      4. sudo cp /tmp/fips-target/release/fips /usr/local/bin/fips
      5. Verify: /usr/local/bin/fips --version
    Expected Result: Build succeeds, binary installed
    Failure Indicators: Compilation errors, permission denied
    Evidence: .sisyphus/evidence/task-3-linux-build.txt
  ```

  **Commit**: NO

- [x] 4. Deploy reverse-direction configs to both nodes

  **What to do**:
  - **Backup Linux config**: `ssh ubuntu@... 'sudo cp /etc/fips/fips.yaml /etc/fips/fips.yaml.bak-forward'`
  - **Deploy Linux central-only config** via SSH:
    - `sudo chattr -i /etc/fips/fips.yaml`
    - Edit config to set: `accept_connections: false`, `scan: true`, `auto_connect: true`, `advertise: false`, `adapter: hci1`
    - Keep existing peers (macos, esp32s3, lilys3) and leaf_proxies
    - `sudo chattr +i /etc/fips/fips.yaml`
    - `sudo systemctl restart fips`
  - **Tell user to update Mac config** with:
    - `accept_connections: true` (this enables listen() → publishes L2CAP + GATT)
    - `scan: false` (don't initiate outbound)
    - `auto_connect: false` (passive mode)
    - `advertise: true` (be discoverable)
    - Keep existing peers and identity
  - **Tell user to restart Mac FIPS** (user handles Mac restarts)
  - Wait for user confirmation that Mac FIPS is running

  **Must NOT do**:
  - Do NOT run FIPS as background process on Linux — always systemd service
  - Do NOT change ESP32 peer entries or leaf_proxies on Linux
  - Do NOT change identity keys on either side

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Config edits + service restart
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (sequential with Task 5)
  - **Blocks**: Task 5
  - **Blocked By**: Tasks 2, 3

  **References**:

  **Pattern References**:
  - `/etc/fips/fips.yaml` on Linux — current production config (backup before modifying)
  - `/tmp/fips-test-macos/config.yaml` on Mac — current Mac config

  **External References**:
  - AGENTS.md — Linux Node Operations: chattr unlock, edit, lock, restart sequence

  **WHY Each Reference Matters**:
  - Linux config must be unlocked (`chattr -i`) before editing and re-locked after
  - Mac config location depends on how user runs FIPS

  **Acceptance Criteria**:
  - [ ] Linux config has `accept_connections: false`, `scan: true`, `advertise: false`
  - [ ] Linux FIPS restarted as systemd service
  - [ ] Linux logs show "BLE scan+probe loop started" but NOT "BLE accept loop started"
  - [ ] User confirms Mac FIPS restarted with `accept_connections: true`

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Linux config deployed correctly
    Tool: Bash (SSH)
    Preconditions: Task 3 binary installed
    Steps:
      1. SSH to Linux
      2. Run: sudo cat /etc/fips/fips.yaml | grep -E 'accept_connections|scan:|auto_connect|advertise'
      3. Assert: accept_connections: false, scan: true, auto_connect: true, advertise: false
      4. Run: sudo journalctl -u fips --since "1 minute ago" | grep -E 'scan.*started|accept.*started'
      5. Assert: "scan+probe loop started" present, "accept loop started" absent
    Expected Result: Linux is in central-only mode, scanning but not listening
    Failure Indicators: "accept loop started" in logs, config shows accept_connections: true
    Evidence: .sisyphus/evidence/task-4-linux-config.txt

  Scenario: Mac is advertising GATT PSM service
    Tool: Bash (SSH to Linux)
    Preconditions: User has restarted Mac FIPS with accept_connections: true
    Steps:
      1. SSH to Linux
      2. Run: sudo journalctl -u fips -f --since "1 minute ago"
      3. Wait up to 60s for scan results mentioning Mac's BLE address
      4. Look for GATT PSM discovery attempt in logs
    Expected Result: Linux sees Mac advertising and attempts GATT PSM discovery
    Failure Indicators: No scan results for Mac address, "device not found" errors
    Evidence: .sisyphus/evidence/task-4-mac-advertising.txt
  ```

  **Commit**: NO

- [ ] 5. Run hardware test with staged checkpoints

  **What to do**:
  Run the reverse-direction hardware test with 6 staged checkpoints. Monitor Linux logs via SSH and Mac logs via local terminal.

  **Checkpoint 1 — Mac Listen**: Mac logs show `"L2CAP published with PSM <N>"` and `"GATT service added"` and `"BLE accept loop started"`
  **Checkpoint 2 — Linux Scan**: Linux logs show Mac's BLE address in scan results
  **Checkpoint 3 — GATT Discovery**: Linux logs show `"GATT PSM discovery: discovered PSM"` with Mac's dynamic PSM
  **Checkpoint 4 — L2CAP Connect**: Linux logs show `"L2CAP channel open"` or `"BLE probe complete"`
  **Checkpoint 5 — Pubkey Exchange**: Both sides log pubkey exchange with correct capability bytes (Mac: `0x7a`, Linux: `0x2c` for central_only or `0x7c` for linux_default — depends on central-only config). Actually for Linux with `accept_connections: false`, it will use `central_only()` = `0x29` (L2CAP_SUPPORTED | CAN_CENTRAL | PREFER_OUTBOUND).
  **Checkpoint 6 — Ping**: Run `sudo fipsctl ping <mac-npub>` from Linux — expect 20/20 pings

  **Diagnostic steps for each failure**:
  - Checkpoint 1 fails: Check Mac logs for `CBManagerState::Unsupported` or `Unauthorized`. Check if Mac binary was actually rebuilt with Task 1 changes.
  - Checkpoint 2 fails: Run `sudo bluetoothctl scan le` on Linux to check if Mac is advertising at all. Check if `advertise: true` is set on Mac.
  - Checkpoint 3 fails: Mac may not be publishing GATT service. Check Mac logs for `"GATT service added"`. Check if `accept_connections: true` was set.
  - Checkpoint 4 fails: PSM mismatch? Check if discovered PSM matches published PSM. Check L2CAP connection errors.
  - Checkpoint 5 fails: Capability byte mismatch. Check hex values in debug log.
  - Checkpoint 6 fails: Connection established but data path broken. Check for the same degradation pattern from issue #50.

  **Must NOT do**:
  - Do NOT attempt to fix bugs found during testing (document them, don't fix)
  - Do NOT change configs mid-test
  - Do NOT restart services mid-test (unless specifically needed for a retry)

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Multi-step hardware test requiring log monitoring, SSH, and staged verification
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (sequential after Task 4)
  - **Blocks**: Task 6
  - **Blocked By**: Task 4

  **References**:

  **Pattern References**:
  - `src/transport/ble/io_macos.rs:906-912` — L2CAP publish + PSM log message format
  - `src/transport/ble/io_macos.rs:918-935` — GATT service creation log messages
  - `src/transport/ble/mod.rs:1287-1297` — GATT PSM discovery log messages in scan_probe_loop
  - `src/transport/ble/mod.rs:1015` — Pubkey exchange complete log with capability hex

  **WHY Each Reference Matters**:
  - These are the exact log messages to grep for at each checkpoint
  - Capability hex values in the pubkey exchange log confirm correct flag encoding

  **Acceptance Criteria**:
  - [ ] All 6 checkpoints pass
  - [ ] 20/20 pings from Linux to Mac
  - [ ] Capability bytes correct: Mac advertises `0x7a`, Linux reads it correctly

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Full reverse-direction connection (happy path)
    Tool: Bash (SSH to Linux) + local terminal for Mac
    Preconditions: Task 4 configs deployed, both services running
    Steps:
      1. On Linux: sudo journalctl -u fips -f | tee /tmp/reverse-test.log
      2. Wait up to 120s for scan discovery of Mac
      3. Grep log for "GATT PSM discovery: discovered PSM" — record PSM value
      4. Grep log for "BLE probe complete" or "L2CAP channel open"
      5. Grep log for "pubkey exchange complete" — verify peer_caps contains 0x7a
      6. Run: sudo fipsctl ping npub1dvfy5yd2qxz49amhu5yuvycjpmt5a8kmam2wldcem885xm0rfeps0v0pu0 -c 20
      7. Assert: 20/20 packets, 0% loss
    Expected Result: Linux discovers Mac GATT PSM, connects, establishes BLE link, ping works
    Failure Indicators: Timeout at any checkpoint, 0x6a in peer_caps (old value), ping loss > 0%
    Evidence: .sisyphus/evidence/task-5-reverse-direction-test.log

  Scenario: Mac accept_loop handles inbound correctly
    Tool: Bash (Mac local terminal)
    Preconditions: Mac FIPS running with accept_connections: true
    Steps:
      1. Grep Mac log for "L2CAP published with PSM"
      2. Grep Mac log for "GATT service added"
      3. Grep Mac log for "BLE accept loop started"
      4. After Linux connects: grep for "BLE inbound pubkey exchange complete"
      5. Verify local_caps shows 0x7a in Mac log
    Expected Result: Mac publishes L2CAP + GATT, accepts inbound from Linux
    Failure Indicators: "CBManagerState::Unsupported", "GATT service add failed", no inbound log
    Evidence: .sisyphus/evidence/task-5-mac-peripheral-log.txt
  ```

  **Commit**: NO

- [ ] 6. Restore original configs and verify forward direction

  **What to do**:
  - **Restore Linux config**: `ssh ubuntu@... 'sudo chattr -i /etc/fips/fips.yaml && sudo cp /etc/fips/fips.yaml.bak-forward /etc/fips/fips.yaml && sudo chattr +i /etc/fips/fips.yaml && sudo systemctl restart fips'`
  - **Tell user to restore Mac config**: Set `accept_connections: false`, `scan: true`, `auto_connect: true`
  - **Tell user to restart Mac FIPS**
  - **Verify forward direction**: Wait for Mac→Linux connection, run ping from Mac

  **Must NOT do**:
  - Do NOT leave test configs in place — must restore
  - Do NOT delete the backup file (keep for reference)

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Config restore + service restart + ping test
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 4 (solo)
  - **Blocks**: F1-F4
  - **Blocked By**: Task 5

  **References**:
  - `/etc/fips/fips.yaml.bak-forward` — backup created in Task 4
  - AGENTS.md — Linux Node Operations

  **Acceptance Criteria**:
  - [ ] Linux config restored to original
  - [ ] Mac config restored to original
  - [ ] Forward direction (Mac→Linux) BLE connection established
  - [ ] Ping from Mac to Linux works

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Forward direction restored
    Tool: Bash (SSH to Linux)
    Preconditions: Task 5 complete, configs being restored
    Steps:
      1. Restore Linux config from backup
      2. sudo systemctl restart fips
      3. Wait for user to restart Mac FIPS
      4. Wait up to 120s for connection
      5. Verify: sudo fipsctl show peers — Mac peer visible
      6. Run ping from Linux: sudo fipsctl ping <mac-npub> -c 5
    Expected Result: Forward direction works as before, 5/5 pings
    Failure Indicators: Peer not visible, ping timeout
    Evidence: .sisyphus/evidence/task-6-forward-restored.txt

  Scenario: Linux config matches original
    Tool: Bash (SSH)
    Preconditions: Config restored from backup
    Steps:
      1. Run: diff /etc/fips/fips.yaml /etc/fips/fips.yaml.bak-forward
      2. Assert: no differences (exit code 0)
    Expected Result: Config identical to backup
    Failure Indicators: Diff shows differences
    Evidence: .sisyphus/evidence/task-6-config-diff.txt
  ```

  **Commit**: NO

---

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [ ] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, run command). For each "Must NOT Have": search codebase for forbidden patterns — reject with file:line if found. Check evidence files exist in .sisyphus/evidence/. Compare deliverables against plan.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [ ] F2. **Code Quality Review** — `unspecified-high`
  Run `cargo test --features ble` + `cargo clippy --features ble`. Review all changed files for: `as any`, unsafe blocks without safety comments, dead code, unused imports. Check that the macOS conditional follows the exact same pattern as Linux.
  Output: `Build [PASS/FAIL] | Tests [N pass/N fail] | Clippy [PASS/FAIL] | VERDICT`

- [ ] F3. **Real Manual QA** — `unspecified-high`
  Verify that the forward direction (Mac→Linux) still works after rollback. Check that `macos_default()` includes CAN_PERIPHERAL. Check that node/mod.rs macOS path reads `accept_connections` from config. Verify all 6 hardware checkpoints were met.
  Output: `Checkpoints [N/6 pass] | Forward direction [PASS/FAIL] | VERDICT`

- [ ] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual diff (git log/diff). Verify 1:1 — everything in spec was built, nothing beyond spec was built. Check "Must NOT do" compliance. Flag unaccounted changes.
  Output: `Tasks [N/N compliant] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

| Commit | Message | Files | Pre-commit |
|--------|---------|-------|------------|
| 1 | `fix(ble): make macOS capabilities conditional on accept_connections config` | `src/node/mod.rs`, `src/transport/ble/mod.rs` | `cargo test --features ble` |

---

## Success Criteria

### Verification Commands
```bash
cargo test --features ble          # Expected: all tests pass
cargo build --release --features ble  # Expected: compiles on Linux
# On Linux via SSH:
sudo journalctl -u fips -f        # Expected: GATT PSM discovery + L2CAP connect logs
sudo fipsctl ping <mac-npub>      # Expected: 20/20 pings
```

### Final Checklist
- [ ] macOS capabilities are config-driven (conditional on `accept_connections`)
- [ ] `macos_default()` includes `CAN_PERIPHERAL`
- [ ] Linux→Mac BLE connection established via GATT PSM discovery
- [ ] 20/20 pings from Linux to Mac
- [ ] Forward direction (Mac→Linux) restored and working
- [ ] All tests pass
- [ ] No changes to tie-breaker algorithm
- [ ] No changes to BLE framing
- [ ] No ESP32 changes
- [ ] Original configs restored on both nodes
