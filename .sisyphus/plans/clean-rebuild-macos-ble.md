# Clean Rebuild: macOS BLE Peripheral + Linux BLE Stability

## TL;DR

> **Quick Summary**: Rebuild our 137-commit experimental branch as 5-6 clean, well-structured commits on top of `upstream/macos-ble-rebased`. Each commit is tested on both Mac and Linux with results documented on issue #78.
>
> **Deliverables**:
> - New branch `macos-ble-peripheral` based on `upstream/macos-ble-rebased`
> - 5-6 clean commits capturing all our unique work
> - Test evidence for each commit on both platforms
> - Issue #78 updated with test results per commit
>
> **Estimated Effort**: XL (5-6 commits, each with cross-platform testing)
> **Parallel Execution**: YES - within each commit, Mac and Linux builds run in parallel
> **Critical Path**: Prep → Commit 1 → Commit 2 → Commit 3 → Commit 4 → Commit 5 → Commit 6 → Final

---

## Context

### Original Request
User wants to clean up 137 messy experimental commits on `linux-ble-stability-v2` into 5-6 clean commits on top of `upstream/macos-ble-rebased`. The upstream maintainer asked us to build on this branch. Each commit must be tested on both Mac and Linux, with results documented on issue #78.

### Interview Summary
**Key Discussions**:
- Our branch (`linux-ble-stability-v2`) has 137 commits, many hypothesis-test-revert cycles
- Upstream `macos-ble-rebased` has 4 unique commits (macOS BLE central foundation)
- Upstream `master` has ACL (PR #50) and 25 unique commits we want
- The two upstream branches have diverged — need to merge master into our base
- User is open to rebasing off master or macos-ble-rebased; prefers macos-ble-rebased per maintainer request
- Linux 2-byte framing was for ESP32 interop — may need platform-specific handling

**Research Findings**:
- Upstream macos-ble-rebased has: FMP byte-stream reassembly (`receive_loop_fmp`), 316-line central-only `io_macos.rs`, NO peripheral role
- Upstream master has: ACL enforcement (PR #50), disconnect state tracking, basic BLE
- Neither upstream branch has: peer capabilities, rate control, peripheral role, sleep/wake recovery, Linux BLE hardening
- Upstream ACL is nearly identical to ours — use theirs
- Both branches use `LePublic` for Linux — we need to change to `LeRandom`

### Branch Topology
```
0d4ffc6  ← merge-base of our branch and macos-ble-rebased
7494ed0  ← merge-base of our branch and master

macos-ble-rebased: 4 unique commits (macOS BLE central)
master:           25 unique commits (ACL, stats, DNS, gateway fixes)
our branch:       137 unique commits (everything we built)
```

---

## Work Objectives

### Core Objective
Create a clean, reviewable branch with all our BLE work as well-structured commits, each tested on both platforms.

### Concrete Deliverables
- Branch `macos-ble-peripheral` on `origin` (Amperstrand/fips)
- 5 commits (6th optional) with clear scope and documentation
- Test evidence on issue #78 for each commit

### Definition of Done
- [ ] All 5 core commits pushed to `origin/macos-ble-peripheral`
- [ ] Each commit builds on both Mac (`--features "ble-macos benchmark"`) and Linux (`--features "ble"`)
- [ ] Each commit tested: FIPS starts, peers connect over BLE
- [ ] Issue #78 has test results comment for each commit
- [ ] No `as any`, no error suppression, no commented-out code
- [ ] `cargo build --release` succeeds on both platforms

### Must Have
- Peer capability signaling with legacy compat
- Linux LeRandom, GATT PSM discovery, backoff, scanner supervisor
- Rate control (token bucket + AIMD)
- macOS peripheral role (CBPeripheralManager, GATT, L2CAP, advertising)
- Sleep/wake recovery (PoweredOff→PoweredOn state machine)

### Must NOT Have (Guardrails)
- NO leaf proxy code (separate PR, separate branch)
- NO ephemeral key dump (debug-only)
- NO `.sisyphus/` or `AGENTS.md` content in commits
- NO asymmetric framing — both platforms MUST use the same wire format (2-byte BE prefix)
- NO separate `receive_loop_fmp` / `receive_loop_seqpacket` — single `receive_loop` with framing in BleStream
- NO assumption that our branch's messy history matters — we write clean code from scratch

### Framing Decision (Critical)

Upstream `macos-ble-rebased` has an **asymmetric framing** bug:
- **macOS `BluestStream`**: Sends RAW bytes (no prefix), receives RAW bytes — relies on `receive_loop_fmp` to parse FMP headers for reassembly
- **Linux `BluerStream`**: Sends with 2-byte BE prefix, receives and strips 2-byte prefix — uses SeqPacket
- **Mock**: Same as Linux (2-byte prefix)

This means **macOS↔Linux is wire-incompatible** in upstream! Mac sends raw, Linux expects prefix. Linux sends prefixed, Mac reads raw.

**Our approach**: Both platforms use **2-byte BE length prefix** consistently:
- **macOS `BluestStream`**: Add 2-byte prefix on send, strip on recv with byte accumulation buffer (handles CoreBluetooth fragmentation/coalescing)
- **Linux `BluerStream`**: Keep upstream's existing 2-byte prefix (already correct!)
- **Result**: Wire-compatible, single `receive_loop`, ESP32 compatible

This eliminates the need for upstream's separate `receive_loop_fmp` and `receive_loop_seqpacket`. The framing is handled in BleStream where it belongs.

**Commit 7 (peripheral role)** must include fixing the macOS central `BluestStream` send/recv to add 2-byte prefix framing.

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES (upstream has `cargo test`, we add `benchmark` feature)
- **Automated tests**: Tests-after (each commit gets tests where applicable)
- **Framework**: `cargo test` / `cargo build`

### QA Policy
Every task includes agent-executed QA scenarios. Evidence saved to `.sisyphus/evidence/`.

- **Build verification**: `cargo build --release --features "ble-macos benchmark"` on Mac, `cargo build --release --features "ble"` on Linux (via SSH)
- **Runtime verification**: Start FIPS on both platforms, check `fipsctl show peers` shows connection
- **Issue documentation**: `gh issue comment` on #78 with build output and peer status

---

## Execution Strategy

### Sequential Commit Chain

```
Step 0 (Preparation):
└── Task 0: Create branch + merge master + verify build [quick]

Step 1 (Commit 1 — peer capabilities):
├── Task 1: Implement peer capability signaling [deep]
└── Task 2: Test commit 1 on Mac + Linux, document on #78 [unspecified-high]

Step 2 (Commit 2 — Linux BLE robustness):
├── Task 3: Implement Linux BLE transport hardening [deep]
└── Task 4: Test commit 2 on Mac + Linux, document on #78 [unspecified-high]

Step 3 (Commit 3 — rate control):
├── Task 5: Implement adaptive rate control [deep]
└── Task 6: Test commit 3 on Mac + Linux, document on #78 [unspecified-high]

Step 4 (Commit 4 — macOS peripheral):
├── Task 7: Implement macOS BLE peripheral role [deep]
└── Task 8: Test commit 4 on Mac + Linux, document on #78 [unspecified-high]

Step 5 (Commit 5 — sleep/wake):
├── Task 9: Implement sleep/wake recovery [deep]
└── Task 10: Test commit 5 on Mac + Linux, document on #78 [unspecified-high]

Step 6 (Commit 6 — benchmark, OPTIONAL):
├── Task 11: Implement benchmark feature [deep]
└── Task 12: Test commit 6 on Mac + Linux, document on #78 [unspecified-high]

Wave FINAL (After ALL tasks — 4 parallel reviews):
├── F1: Plan compliance audit (oracle)
├── F2: Code quality review (unspecified-high)
├── F3: Real cross-platform QA (unspecified-high)
└── F4: Scope fidelity check (deep)
→ Present results → Get explicit user okay

Critical Path: Task 0 → Task 1 → Task 3 → Task 5 → Task 7 → Task 9 → Task 11 → F1-F4
Total Tasks: 12 implementation + 4 final = 16
```

### Dependency Matrix

| Task | Depends On | Blocks | Wave |
|------|-----------|--------|------|
| 0 | None | 1, 2 | Prep |
| 1 | 0 | 2 | Step 1 |
| 2 | 1 | 3 | Step 1 |
| 3 | 2 | 4 | Step 2 |
| 4 | 3 | 5 | Step 2 |
| 5 | 4 | 6 | Step 3 |
| 6 | 5 | 7 | Step 3 |
| 7 | 6 | 8 | Step 4 |
| 8 | 7 | 9 | Step 4 |
| 9 | 8 | 10 | Step 5 |
| 10 | 9 | 11 | Step 5 |
| 11 | 10 | 12 | Step 6 |
| 12 | 11 | F1-F4 | Step 6 |
| F1-F4 | 12 | User okay | Final |

### Agent Dispatch Summary

- **Prep**: 1 — T0 → `quick`
- **Step 1**: 2 — T1 → `deep`, T2 → `unspecified-high`
- **Step 2**: 2 — T3 → `deep`, T4 → `unspecified-high`
- **Step 3**: 2 — T5 → `deep`, T6 → `unspecified-high`
- **Step 4**: 2 — T7 → `deep`, T8 → `unspecified-high`
- **Step 5**: 2 — T9 → `deep`, T10 → `unspecified-high`
- **Step 6**: 2 — T11 → `deep`, T12 → `unspecified-high`
- **FINAL**: 4 — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [x] 0. **Create branch + merge master + verify build**

  **What to do**:
  - Create new branch `macos-ble-peripheral` based on `upstream/macos-ble-rebased`:
    ```bash
    git fetch upstream
    git checkout -b macos-ble-peripheral upstream/macos-ble-rebased
    ```
  - Merge `upstream/master` to incorporate ACL and all master improvements:
    ```bash
    git merge upstream/master -m "Merge upstream/master for ACL and platform improvements"
    ```
  - Resolve any merge conflicts (expected: minimal, since macos-ble-rebased branched recently from master)
  - Add `benchmark` feature to `Cargo.toml` `[features]` section:
    ```toml
    benchmark = []
    ```
  - Verify the build compiles:
    ```bash
    cargo build --release --features "ble-macos benchmark"
    ```
  - Push to origin:
    ```bash
    git push -u origin macos-ble-peripheral
    ```
  - Also build on Linux (via SSH):
    ```bash
    # Copy Cargo.toml and Cargo.lock changes to Linux
    ssh 218 "source ~/.cargo/env && cd /home/ubuntu/fips && git fetch origin && git checkout macos-ble-peripheral && CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble"
    ```

  **Must NOT do**:
  - Do NOT cherry-pick any of our old commits — this is a clean start
  - Do NOT include leaf proxy, benchmark code, or .sisyphus files
  - Do NOT modify any BLE transport code yet

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: [`git-master`]

  **Parallelization**:
  - **Can Run In Parallel**: NO (foundation for everything)
  - **Parallel Group**: Sequential — must complete first
  - **Blocks**: Tasks 1-12, F1-F4
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `Cargo.toml` on our branch (at `origin/linux-ble-stability-v2:Cargo.toml`) — shows the `benchmark = []` feature definition. Look at the `[features]` section to see exact placement.
  - `Cargo.toml` on `upstream/macos-ble-rebased` — the base we're building on. Has: `default = ["tui", "ble"]`, `ble`, `ble-macos`, `gateway`. We add `benchmark = []`.

  **API/Type References**:
  - `upstream/macos-ble-rebased:src/transport/ble/io_macos.rs` — 316 lines, central-only. This is our starting point.
  - `upstream/macos-ble-rebased:src/transport/ble/mod.rs` — has `receive_loop_fmp` and `receive_loop_seqpacket`. Our starting point.
  - `upstream/master:src/node/acl.rs` — the ACL system we're merging in.

  **Acceptance Criteria**:
  - [ ] Branch `macos-ble-peripheral` exists on `origin`
  - [ ] `git log --oneline -5` shows: merge commit from master + 4 macos-ble-rebased commits
  - [ ] `cargo build --release --features "ble-macos benchmark"` exits 0 on Mac
  - [ ] `cargo build --release --features "ble"` exits 0 on Linux (via SSH)

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Build succeeds on Mac
    Tool: Bash
    Preconditions: Branch checked out, dependencies installed
    Steps:
      1. Run: cargo build --release --features "ble-macos benchmark"
      2. Check exit code is 0
      3. Verify binary exists: ls -la /tmp/fips-target/release/fips (or target/release/fips)
    Expected Result: Build completes with 0 errors, binary exists
    Failure Indicators: Compilation errors, missing features, unresolved imports
    Evidence: .sisyphus/evidence/task-0-mac-build.txt

  Scenario: Build succeeds on Linux
    Tool: Bash (ssh 218)
    Preconditions: Branch pushed to origin, Linux has cargo installed
    Steps:
      1. ssh 218 "source ~/.cargo.env && cd /home/ubuntu/fips && git fetch origin && git checkout macos-ble-peripheral && CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble"
      2. Check exit code is 0
    Expected Result: Build completes with 0 errors
    Failure Indicators: Compilation errors, merge conflict markers in files
    Evidence: .sisyphus/evidence/task-0-linux-build.txt
  ```

  **Commit**: YES
  - Message: `Merge upstream/master for ACL and platform improvements`
  - Files: Merge commit (no individual files)
  - Pre-commit: `cargo build --release --features "ble-macos benchmark"`

- [x] 1. **Implement peer capability signaling**

  **What to do**:
  - Create `src/transport/ble/capabilities.rs` with `PeerCapabilities` bitflags:
    ```rust
    bitflags::bitflags! {
        pub struct PeerCapabilities: u8 {
            const CAN_CENTRAL = 0x01;
            const CAN_PERIPHERAL = 0x02;
            const PREFER_OUTBOUND = 0x04;
            const CENTRAL_ONLY = 0x08;
        }
    }
    ```
  - Add platform default capability constructors:
    - `macos_default()` — `CAN_CENTRAL` (and `CAN_PERIPHERAL` if `accept_connections` config is true)
    - `linux_default()` — `CAN_CENTRAL | CAN_PERIPHERAL` (Linux can do both)
  - Modify the BLE pubkey exchange in `src/transport/ble/mod.rs` to send 34 bytes instead of 33:
    - Byte 0: `0x00` prefix
    - Bytes 1-32: x-only pubkey
    - Byte 33: capability flags
  - Handle legacy 33-byte exchanges (no flags byte → assume full capabilities)
  - Add capability-aware tie-breaking in `accept_loop` and `scan_probe_loop`:
    - If peer is `CENTRAL_ONLY` → keep connection (peer can't accept inbound)
    - If peer `PREFER_OUTBOUND` and we don't → yield (let peer's outbound win)
    - Otherwise → NodeAddr comparison (smaller NodeAddr's outbound wins)
  - Add unit tests for capability signaling and tie-breaking logic
  - Commit with message: `feat(ble): peer capability signaling for BLE role negotiation`

  **Must NOT do**:
  - Do NOT modify Linux BLE connect logic (that's commit 2)
  - Do NOT modify macOS io_macos.rs (that's commit 4)
  - Do NOT add rate limiting (that's commit 3)

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []
    - Reason: Complex logic with multiple interacting components, needs deep understanding of BLE handshake flow and tie-breaking semantics

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on Task 0)
  - **Parallel Group**: Sequential
  - **Blocks**: Task 2
  - **Blocked By**: Task 0

  **References**:

  **Pattern References**:
  - `origin/linux-ble-stability-v2:src/transport/ble/mod.rs` — our existing implementation of `PeerCapabilities`, pubkey exchange, and capability-aware tie-breaking. Search for `PeerCapabilities`, `PUBKEY_EXCHANGE_SIZE`, `cross_connection_winner`, and the tie-breaking logic in `accept_loop` and `scan_probe_loop`. This is the reference implementation to adapt.
  - `origin/linux-ble-stability-v2:src/transport/ble/mod.rs:scan_probe_loop` — shows how `prefer_outbound` and `central_only` flags are used during connection setup to decide whether to yield
  - `origin/linux-ble-stability-v2:src/transport/ble/mod.rs:accept_loop` — shows the inbound side of tie-breaking

  **API/Type References**:
  - `upstream/macos-ble-rebased:src/transport/ble/mod.rs:pubkey_exchange()` — the current 33-byte exchange function. Must be extended to 34 bytes.
  - `upstream/macos-ble-rebased:src/transport/ble/mod.rs:cross_connection_winner()` — current tie-breaking using only NodeAddr comparison. Must be extended with capability awareness.

  **External References**:
  - `bitflags` crate docs — for the `bitflags!` macro usage

  **WHY Each Reference Matters**:
  - Our `PeerCapabilities` implementation is the gold standard — it's tested with hardware and handles edge cases
  - Upstream's `pubkey_exchange()` shows the current wire format — we must extend it backward-compatibly
  - The tie-breaking logic is subtle — must preserve exact semantics from our branch

  **Acceptance Criteria**:
  - [ ] `src/transport/ble/capabilities.rs` exists with `PeerCapabilities` bitflags
  - [ ] Pubkey exchange sends 34 bytes (33 + capability flags)
  - [ ] Legacy 33-byte exchange handled (assume full capabilities)
  - [ ] Tie-breaking in `accept_loop` respects capability flags
  - [ ] Tie-breaking in `scan_probe_loop` respects capability flags
  - [ ] `cargo test` passes
  - [ ] `cargo build --release --features "ble-macos benchmark"` succeeds on Mac
  - [ ] `cargo build --release --features "ble"` succeeds on Linux

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Unit tests pass
    Tool: Bash
    Preconditions: Code compiles
    Steps:
      1. Run: cargo test --features "ble ble-macos"
      2. Check all tests pass
    Expected Result: 0 failures, tests for PeerCapabilities and tie-breaking pass
    Failure Indicators: Test failures in capabilities module
    Evidence: .sisyphus/evidence/task-1-unit-tests.txt

  Scenario: Build on Mac succeeds
    Tool: Bash
    Steps:
      1. Run: cargo build --release --features "ble-macos benchmark"
      2. Check exit code 0
    Expected Result: Clean build, no warnings about unused code
    Failure Indicators: Missing bitflags import, type mismatches
    Evidence: .sisyphus/evidence/task-1-mac-build.txt

  Scenario: Build on Linux succeeds
    Tool: Bash (ssh 218)
    Steps:
      1. Push branch, checkout on Linux, build with --features ble
      2. Check exit code 0
    Expected Result: Clean build
    Failure Indicators: bitflags not in dependencies, cfg target_os issues
    Evidence: .sisyphus/evidence/task-1-linux-build.txt
  ```

  **Commit**: YES
  - Message: `feat(ble): peer capability signaling for BLE role negotiation`
  - Files: `src/transport/ble/capabilities.rs`, `src/transport/ble/mod.rs`, `Cargo.toml` (if adding bitflags dep)
  - Pre-commit: `cargo test --features "ble ble-macos"`

- [x] 2. **Test commit 1 on Mac + Linux, document on issue #78**

  **What to do**:
  - **Mac test**:
    1. Build: `cargo build --release --features "ble-macos benchmark"`
    2. Start FIPS with test config (use the Mac config from `/tmp/fips-logs/opt-test-mac.yaml` or create one)
    3. Wait 10s, check `fipsctl show peers` — should show no peers yet (Linux not running new code)
    4. Verify FIPS doesn't crash for 30s
    5. Stop FIPS
  - **Linux test**:
    1. Push branch to origin
    2. SSH to 218, checkout branch, build
    3. Restart FIPS service: `sudo systemctl restart fips`
    4. Check logs: `sudo journalctl -u fips --since "10 seconds ago"` — should show FIPS starting
    5. Check peers: `fipsctl show peers` — should show no peers yet
    6. Verify no crashes for 30s
  - **Cross-platform BLE test** (if both are running new code):
    1. Start FIPS on both Mac and Linux
    2. Wait up to 60s for BLE discovery
    3. Check `fipsctl show peers` on both — should show each other
    4. If connected: verify peer has capabilities in the pubkey exchange log
  - **Document results** on issue #78:
    ```bash
    gh issue comment 78 --repo Amperstrand/fips --body "## Commit 1: Peer Capability Signaling

    ### Mac Build
    $(cat evidence output or "SUCCESS")

    ### Linux Build
    $(cat evidence output or "SUCCESS")

    ### Cross-Platform BLE Test
    $(describe results)

    ### Verdict
    $(PASS/FAIL with summary)"
    ```

  **Must NOT do**:
  - Do NOT modify any code — this is a test-only task
  - Do NOT change Linux FIPS config permanently

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on Task 1)
  - **Parallel Group**: Sequential
  - **Blocks**: Task 3
  - **Blocked By**: Task 1

  **References**:

  **Pattern References**:
  - `/tmp/fips-logs/opt-test-mac.yaml` — Mac FIPS config used in previous experiments
  - `/etc/fips/fips.yaml` (Linux) — Linux FIPS config

  **Acceptance Criteria**:
  - [ ] Mac build succeeds
  - [ ] Linux build succeeds
  - [ ] Cross-platform BLE test shows peers connecting (or documents why not)
  - [ ] Issue #78 has a comment with test results for commit 1

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Full cross-platform test with issue documentation
    Tool: Bash + Bash (ssh)
    Preconditions: Commit 1 code is on branch, pushed to origin
    Steps:
      1. Mac: cargo build --release --features "ble-macos benchmark"
      2. Mac: Start FIPS, wait 30s, check peers, stop FIPS
      3. Linux: ssh 218, build, restart service, wait 30s, check peers
      4. If both running: wait 60s, check peers on both
      5. Post results to gh issue comment 78
    Expected Result: Both build, both start, peers discover each other, issue updated
    Failure Indicators: Build failure, FIPS crash, no peer discovery, issue comment not posted
    Evidence: .sisyphus/evidence/task-2-cross-platform-test.txt
  ```

  **Commit**: NO (test-only task)

- [x] 3. **Implement Linux BLE transport hardening**

  **What to do**:
  - **LeRandom address type**: Change all Linux BLE socket operations from `LePublic` to `LeRandom`:
    - In `src/transport/ble/io.rs`: `SocketAddr::new(local_addr, AddressType::LePublic, psm)` → `AddressType::LeRandom`
    - Auto-detect local adapter address type for `listen()` bind
    - Always use `LeRandom` for remote connections
  - **GATT PSM discovery**: Implement dynamic PSM discovery via GATT service characteristic:
    - Connect to device GATT server first
    - Discover the FIPS service UUID
    - Read the PSM characteristic (u16 LE)
    - Use discovered PSM for L2CAP connection
    - Fallback to configured PSM if GATT discovery fails
    - Add 3s timeout on GATT connect attempts
  - **Per-address exponential backoff**: Create `src/transport/ble/backoff.rs`:
    - Track per-address failure count
    - Exponential backoff: 1s, 2s, 4s, 8s, 16s, 30s (capped)
    - Auto-deny after N consecutive failures (configurable)
    - Reset backoff on successful connection
  - **Scanner supervisor**: Auto-restart `scan_probe_loop` after `bluetoothd` restart:
    - Monitor for adapter events indicating service restart
    - Restart scan probe with fresh state
    - Log supervisor actions for debugging
  - **Frame validation**: In `receive_loop_seqpacket`:
    - Reject 0-byte frames
    - Recover from malformed frames (drain and continue)
  - **disconnect_device()**: Implement clean device disconnection on Linux:
    - Use BlueZ API to properly disconnect BLE device
    - Clean up connection state
  - Commit with message: `fix(ble): Linux BLE transport robustness`

  **Must NOT do**:
  - Do NOT add 2-byte prefix framing on Linux (upstream uses SeqPacket, no framing needed)
  - Do NOT modify macOS io_macos.rs
  - Do NOT add rate limiting (that's commit 3)

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []
    - Reason: Complex Linux BLE transport changes with multiple interacting subsystems, BlueZ API knowledge required

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on Task 2)
  - **Parallel Group**: Sequential
  - **Blocks**: Task 4
  - **Blocked By**: Task 2

  **References**:

  **Pattern References**:
  - `origin/linux-ble-stability-v2:src/transport/ble/io.rs` — our complete Linux BLE implementation with all fixes. Key sections: `LeRandom` usage (search for `AddressType::LeRandom`), `GATT-first connect` (search for `gatt_connect` or `discover_psm`), `disconnect_device()` method. This file is 1678 lines.
  - `origin/linux-ble-stability-v2:src/transport/ble/backoff.rs` — our backoff module (~164 lines). Copy this almost verbatim.
  - `origin/linux-ble-stability-v2:src/transport/ble/mod.rs:scan_probe_supervisor` — the scanner supervisor pattern. Search for `scanner_supervisor` or `supervisor`.
  - `origin/linux-ble-stability-v2:src/transport/ble/mod.rs` — frame validation in `receive_loop` (search for `0-byte` or `reject`).

  **API/Type References**:
  - `upstream/macos-ble-rebased:src/transport/ble/io.rs` — the base Linux BLE implementation (801 lines on master, 841 on macos-ble-rebased). Uses `LePublic` everywhere — we change to `LeRandom`.
  - `upstream/macos-ble-rebased:src/transport/ble/mod.rs:receive_loop_seqpacket` — the SeqPacket receive loop (no framing). We add frame validation here.

  **WHY Each Reference Matters**:
  - Our `io.rs` contains months of incremental fixes — the important parts are LeRandom, GATT PSM discovery, and connect timeout
  - The backoff module is self-contained and can be adapted directly
  - Upstream's receive_loop_seqpacket is simpler than ours — we only add validation, no framing

  **Acceptance Criteria**:
  - [ ] All Linux BLE socket operations use `LeRandom`
  - [ ] GATT PSM discovery implemented with 3s timeout
  - [ ] Per-address backoff with auto-deny
  - [ ] Scanner supervisor auto-restarts on bluetoothd restart
  - [ ] 0-byte frame rejection in receive_loop_seqpacket
  - [ ] `disconnect_device()` implemented
  - [ ] `cargo build --release --features "ble"` succeeds on Linux
  - [ ] `cargo build --release --features "ble-macos benchmark"` succeeds on Mac

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Linux build succeeds with new modules
    Tool: Bash (ssh 218)
    Preconditions: Branch pushed, backoff.rs and io.rs changes present
    Steps:
      1. ssh 218 "cd /home/ubuntu/fips && git fetch origin && git checkout macos-ble-peripheral"
      2. ssh 218 "source ~/.cargo/env && CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble"
      3. Check exit code 0
    Expected Result: Clean build, backoff module compiles, no unused import warnings
    Failure Indicators: Missing bluer API methods, AddressType import issues
    Evidence: .sisyphus/evidence/task-3-linux-build.txt

  Scenario: Mac build still succeeds (no Linux-only code breaks Mac)
    Tool: Bash
    Steps:
      1. cargo build --release --features "ble-macos benchmark"
      2. Check exit code 0
    Expected Result: Clean build — Linux-only changes behind cfg(target_os = "linux")
    Failure Indicators: Platform-specific code leaking across cfg boundaries
    Evidence: .sisyphus/evidence/task-3-mac-build.txt
  ```

  **Commit**: YES
  - Message: `fix(ble): Linux BLE transport robustness`
  - Files: `src/transport/ble/io.rs`, `src/transport/ble/mod.rs`, `src/transport/ble/backoff.rs` (new)
  - Pre-commit: `cargo build --release --features "ble-macos benchmark"`

- [x] 4. **Test commit 2 on Mac + Linux, document on issue #78**

  **What to do**:
  - Same testing procedure as Task 2, but with Linux BLE hardening included
  - **Key difference**: Now test that Linux FIPS starts with LeRandom and GATT PSM discovery
  - **Linux-specific tests**:
    1. Check journal logs for `LeRandom` in BLE startup messages
    2. Check that GATT PSM discovery is attempted (look for PSM discovery log lines)
    3. Check that backoff module loads (look for backoff-related log lines on repeated connect failures)
  - **Cross-platform BLE test**:
    1. Start FIPS on both Mac and Linux
    2. Wait up to 60s for discovery
    3. Verify peers connect — Mac should discover Linux's LeRandom address
    4. Check `fipsctl show peers` on both
  - Document results on issue #78

  **Must NOT do**:
  - Do NOT modify any code — test-only task

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential
  - **Blocks**: Task 5
  - **Blocked By**: Task 3

  **References**:
  - Same as Task 2 references (configs, FIPS paths)

  **Acceptance Criteria**:
  - [ ] Mac build succeeds
  - [ ] Linux build succeeds
  - [ ] Linux FIPS starts with LeRandom
  - [ ] Cross-platform BLE peers connect
  - [ ] Issue #78 has comment with commit 2 test results

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Cross-platform BLE with LeRandom
    Tool: Bash + Bash (ssh)
    Preconditions: Commit 2 on both platforms
    Steps:
      1. Mac: build, start FIPS
      2. Linux: build, restart service
      3. Wait 60s for BLE discovery
      4. Mac: fipsctl show peers
      5. Linux: ssh 218 "fipsctl show peers"
      6. Verify both show the other as connected peer
    Expected Result: Peers discover and connect despite LeRandom change
    Failure Indicators: No discovery after 60s, connection refused errors
    Evidence: .sisyphus/evidence/task-4-ble-test.txt

  Scenario: Linux GATT PSM discovery works
    Tool: Bash (ssh)
    Preconditions: Linux FIPS running
    Steps:
      1. ssh 218 "sudo journalctl -u fips --since '2 minutes ago' | grep -i 'psm\|gatt'"
      2. Verify PSM discovery log lines appear
    Expected Result: GATT PSM discovery attempted and succeeded or fell back gracefully
    Failure Indicators: PSM discovery timeout, no fallback
    Evidence: .sisyphus/evidence/task-4-psm-discovery.txt
  ```

  **Commit**: NO (test-only task)

- [x] 5. **Implement adaptive rate control for constrained BLE links**

  **What to do**:
  - **Token bucket rate limiter**: Create or extend `src/transport/ble/rate_limit.rs`:
    - Configurable rate (default 80 kbps) and burst (default 2048 bytes)
    - Token bucket algorithm: tokens replenish at rate, consume on send
    - Async `acquire(bytes)` method that waits if insufficient tokens
  - **AIMD rate adapter**: Create `src/transport/ble/rate_adapter.rs` (or extend rate_limit.rs):
    - Use MMP SRTT feedback to detect congestion
    - Increase rate when RTT is low (below threshold)
    - Decrease rate when RTT is high (above threshold)
    - Rate ceiling: 80 kbps (MAX_RATE_BPS)
    - Rate floor: 15 kbps (MIN_RATE_BPS)
    - Prevent oscillation with hysteresis band
  - **TCP window clamp**: Add BLE-specific TCP receive window clamping:
    - In the TUN/TCP handling path: detect BLE transport, clamp TCP window to 8 KB
    - This prevents TCP burst-stall over BLE (issue #50, #69)
  - **Dynamic TCP MSS recomputation**: When transport MTU changes:
    - Recompute TCP MSS to fit within BLE frame constraints
  - **Integration**: Wire rate limiter into `BleStream::send()` on both platforms
  - **Configuration**: Add rate/burst config options to BLE config section
  - Commit with message: `feat(ble): adaptive rate control for constrained BLE links`

  **Must NOT do**:
  - Do NOT add 2-byte prefix framing (Linux uses SeqPacket)
  - Do NOT modify macOS io_macos.rs peripheral code (that's commit 4)

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential
  - **Blocks**: Task 6
  - **Blocked By**: Task 4

  **References**:

  **Pattern References**:
  - `origin/linux-ble-stability-v2:src/transport/ble/rate_limit.rs` — our token bucket implementation. Key: `SendRateLimiter` struct with `acquire()` method, configurable rate and burst.
  - `origin/linux-ble-stability-v2:src/transport/ble/io.rs` — shows how rate limiter is wired into `BleStream::send()` on Linux. Search for `rate_limiter` and `acquire`.
  - `origin/linux-ble-stability-v2:src/transport/ble/io_macos.rs` — shows rate limiter in macOS BluestStream::send(). Same pattern.
  - `origin/linux-ble-stability-v2:src/node/tun.rs` (or wherever TCP window clamp is) — search for `MAX_BLE_TCP_WINDOW` and `tcp_window_clamp`. The TCP receive window clamping for BLE.

  **API/Type References**:
  - `upstream/macos-ble-rebased:src/transport/ble/io.rs` — Linux BleStream::send() without rate limiting. We add it.
  - `upstream/macos-ble-rebased:src/transport/ble/io_macos.rs` — macOS BluestStream::send(). We add rate limiting here too.
  - Our config schema: search for `send_rate_bps` and `send_burst_bytes` in our branch's config files.

  **WHY Each Reference Matters**:
  - Our rate limiter is battle-tested on hardware for months — copy the algorithm
  - TCP window clamping is the key fix for burst-stall (#50) — must be included
  - The AIMD adapter uses MMP SRTT which is a FIPS-internal metric — need to find where SRTT is exposed

  **Acceptance Criteria**:
  - [ ] `src/transport/ble/rate_limit.rs` exists with token bucket
  - [ ] Rate limiter wired into BleStream::send() on both platforms
  - [ ] TCP window clamp for BLE paths (8 KB max)
  - [ ] Configurable rate and burst via config file
  - [ ] `cargo build --release --features "ble-macos benchmark"` on Mac
  - [ ] `cargo build --release --features "ble"` on Linux

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Build with rate limiting on both platforms
    Tool: Bash + Bash (ssh)
    Preconditions: Rate limit module added
    Steps:
      1. Mac: cargo build --release --features "ble-macos benchmark"
      2. Linux: build via SSH
      3. Both should compile cleanly
    Expected Result: Clean builds on both platforms
    Failure Indicators: Missing imports, type mismatches in rate limiter integration
    Evidence: .sisyphus/evidence/task-5-build-both.txt

  Scenario: Rate limiter prevents RTT runaway
    Tool: Bash (ssh) + Bash
    Preconditions: Both platforms running, peers connected
    Steps:
      1. Generate sustained traffic (ping or data transfer)
      2. Monitor RTT via fipsctl or logs
      3. Verify RTT stays within bounds (not exceeding ~500ms)
    Expected Result: RTT remains stable under sustained load
    Failure Indicators: RTT climbing unboundedly, link death
    Evidence: .sisyphus/evidence/task-5-rate-stability.txt
  ```

  **Commit**: YES
  - Message: `feat(ble): adaptive rate control for constrained BLE links`
  - Files: `src/transport/ble/rate_limit.rs` (new or extended), `src/transport/ble/io.rs`, `src/transport/ble/io_macos.rs`, `src/transport/ble/mod.rs` (rate adapter integration)
  - Pre-commit: `cargo build --release --features "ble-macos benchmark"`

- [x] 6. **Test commit 3 on Mac + Linux, document on issue #78**

  **What to do**:
  - Same testing procedure as Tasks 2/4
  - **Additional tests for rate control**:
    1. Start both nodes, verify peers connect
    2. Generate sustained traffic (e.g., `fipsctl ping` or data transfer)
    3. Monitor RTT over 2-3 minutes — should remain stable
    4. Check logs for rate limiter activity (token bucket acquire/release)
  - Document results on issue #78

  **Must NOT do**:
  - Do NOT modify any code — test-only task

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential
  - **Blocks**: Task 7
  - **Blocked By**: Task 5

  **Acceptance Criteria**:
  - [ ] Mac build succeeds
  - [ ] Linux build succeeds
  - [ ] Peers connect over BLE
  - [ ] Rate limiter logs visible (token bucket activity)
  - [ ] RTT stable under load
  - [ ] Issue #78 has comment with commit 3 test results

  **QA Scenarios**: Same pattern as Task 4, plus rate stability check

  **Commit**: NO (test-only task)

- [x] 7. **Implement macOS BLE peripheral role**

  **What to do**:
  This is the largest commit — extends upstream's 316-line central-only `io_macos.rs` to ~1400 lines with full peripheral role support, AND fixes the macOS central framing to match Linux.

  - **Fix macOS central BluestStream framing** (CRITICAL — wire compat fix):
    - Upstream's `BluestStream::send()` sends raw bytes — change to add 2-byte BE length prefix
    - Upstream's `BluestStream::recv()` reads raw bytes — change to accumulate in buffer and strip 2-byte prefix
    - This matches Linux's existing behavior and makes Mac↔Linux wire-compatible
    - Add `recv_buf: Mutex<Vec<u8>>` field to `BluestStream` for byte accumulation
    - The recv loop: read into `recv_buf`, check if first 2 bytes give length, if complete frame available → copy payload to output buf, drain from recv_buf
  - **Simplify receive_loop** (consequence of framing fix):
    - Remove `receive_loop_fmp` (no longer needed — BleStream handles framing)
    - Keep single `receive_loop` that just calls `stream.recv()` for all platforms
    - `receive_loop_seqpacket` can be removed or merged into the single `receive_loop`

  - **CBPeripheralManager setup**:
    - Create `FipsPeripheralDelegate` implementing `CBPeripheralManagerDelegate` via `objc2`
    - Initialize `CBPeripheralManager` on a dedicated `NSRunLoop` thread (required by CoreBluetooth)
    - Bridge Objective-C delegate callbacks to Rust async via `tokio::sync::mpsc` channels
    - Define `PeripheralManagerEvent` enum: `StateChanged`, `L2CAPPublished`, `ServiceAdded`, `L2CAPAccept`, `AdvertisingStarted`
  - **GATT service with PSM characteristic**:
    - Create CBMutableService with FIPS service UUID
    - Add characteristic for dynamic L2CAP PSM (u16 LE)
    - Publish service via `CBPeripheralManager.add()`
    - Update characteristic value when L2CAP channel is published with new PSM
  - **L2CAP channel acceptor**:
    - Call `CBPeripheralManager.publishL2CAPChannel()` to get dynamic PSM
    - Accept incoming L2CAP channels via delegate callback
    - Wrap accepted channels in `PeripheralStream` implementing `BleStream`
    - Bridge to async Rust: received bytes → tokio channel → `recv()` method
    - Unencrypted L2CAP (required — CoreBluetooth rejects SMP pairing in peripheral mode, issue #64)
  - **BLE advertising**:
    - Start advertising with FIPS service UUID
    - Include service data with node's x-only pubkey for discovery
  - **Integration with mod.rs**:
    - Extend `BluestIo::listen()` to start peripheral manager alongside scanner
    - Add `accept_loop` for peripheral connections (reuses existing accept_loop from mod.rs)
    - Pubkey exchange on peripheral connections (34-byte format from commit 1)
    - Connection pool management for peripheral streams
  - **Bounded write queues**:
    - Add bounded `mpsc::Sender<Vec<u8>>` for L2CAP writes (capacity ~32)
    - Backpressure: if queue full, wait with timeout (3s) before dropping
    - Separate urgent queue for high-priority messages
  - **Conditional compilation**:
    - Peripheral role only enabled when `accept_connections = true` in config
    - If `accept_connections = false`: only central role (scanning)
    - Set `CAN_PERIPHERAL` capability flag based on config
  - Commit with message: `feat(ble-macos): peripheral role for inbound BLE connections`

  **Must NOT do**:
  - Do NOT include sleep/wake recovery (that's commit 5)
  - Do NOT add benchmark code
  - Do NOT modify Linux io.rs
  - Do NOT use encrypted L2CAP (CoreBluetooth rejects it in peripheral mode)

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []
    - Reason: ~1100 lines of new code, complex Objective-C/Rust FFI bridge, async integration

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential
  - **Blocks**: Task 8
  - **Blocked By**: Task 6

  **References**:

  **Pattern References**:
  - `origin/linux-ble-stability-v2:src/transport/ble/io_macos.rs` — **THE** reference. Our full 1437-line implementation. Key sections:
    - `FipsPeripheralDelegate` class (search for `FipsPeripheralDelegate` or `define_class!`)
    - `PeripheralManagerEvent` enum
    - `PeripheralStream` struct implementing `BleStream`
    - `BluestIo::listen()` — shows how peripheral manager is initialized
    - `accept_loop` integration — how accepted L2CAP channels enter the connection pool
    - GATT service creation — `CBMutableService`, characteristic setup
    - Advertising — `CBPeripheralManager.startAdvertising()`
    - Unencrypted L2CAP — `publishL2CAPChannel` without encryption
  - `origin/linux-ble-stability-v2:src/transport/ble/mod.rs:accept_loop` — shows how peripheral connections are handled after L2CAP accept

  **API/Type References**:
  - `upstream/macos-ble-rebased:src/transport/ble/io_macos.rs` — the 316-line base we're extending. Has `BluestStream` (central), `BluestScanner`, `BluestIo` without peripheral support. We add peripheral to `BluestIo`.
  - `upstream/macos-ble-rebased:src/transport/ble/mod.rs:receive_loop_fmp` — macOS uses FMP reassembly for received data. Our `PeripheralStream::recv()` must deliver raw bytes (the FMP reassembly happens in `receive_loop_fmp`).
  - `objc2` crate — for `define_class!`, `CBPeripheralManager`, `CBMutableService`, etc.

  **WHY Each Reference Matters**:
  - Our io_macos.rs is the only complete macOS peripheral implementation — must adapt it carefully
  - Upstream's io_macos.rs shows how central role works — we extend, don't replace
  - The FMP reassembly is upstream's approach for macOS byte streams — our peripheral must deliver bytes compatible with it

  **Acceptance Criteria**:
  - [ ] `CBPeripheralManager` initializes with `FipsPeripheralDelegate`
  - [ ] GATT service with PSM characteristic published
  - [ ] L2CAP channels accepted and wrapped in `PeripheralStream`
  - [ ] Advertising starts with FIPS service UUID
  - [ ] `accept_loop` processes peripheral connections
  - [ ] Unencrypted L2CAP (no encryption parameter)
  - [ ] Conditional: peripheral only when `accept_connections = true`
  - [ ] `cargo build --release --features "ble-macos benchmark"` succeeds
  - [ ] `cargo build --release --features "ble"` succeeds on Linux (no macOS code compiled)

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Build succeeds with peripheral code
    Tool: Bash
    Steps:
      1. cargo build --release --features "ble-macos benchmark"
      2. Check exit code 0
    Expected Result: Clean build, all objc2 FFI compiles
    Failure Indicators: objc2 macro errors, missing CBPeripheralManager methods
    Evidence: .sisyphus/evidence/task-7-mac-build.txt

  Scenario: Linux build unaffected
    Tool: Bash (ssh)
    Steps:
      1. Build on Linux --features ble
      2. Check exit code 0
    Expected Result: Clean build, macOS code behind cfg(target_os = "macos")
    Failure Indicators: macOS types leaking into platform-independent code
    Evidence: .sisyphus/evidence/task-7-linux-build.txt
  ```

  **Commit**: YES
  - Message: `feat(ble-macos): peripheral role for inbound BLE connections`
  - Files: `src/transport/ble/io_macos.rs` (major extension), `src/transport/ble/mod.rs` (accept_loop integration)
  - Pre-commit: `cargo build --release --features "ble-macos benchmark"`

- [ ] 8. **Test commit 4 on Mac + Linux, document on issue #78**

  **What to do**:
  - **This is the critical test** — does Linux→macOS BLE work now?
  - **Linux test**: Build and start FIPS (should be unchanged from commit 3)
  - **Mac test**: Build and start FIPS with `accept_connections = true` in config
  - **Cross-platform BLE test**:
    1. Start FIPS on Linux (central, scanning for Mac)
    2. Start FIPS on Mac (peripheral, advertising)
    3. Wait up to 90s for Linux to discover Mac's BLE advertisement
    4. Linux initiates L2CAP connection to Mac's published PSM
    5. Verify pubkey exchange completes (34-byte format)
    6. Verify `fipsctl show peers` shows connection on BOTH sides
    7. Send test traffic (ping) to verify bidirectional data flow
  - **Verify no regressions**: Mac-to-Linux (Mac as central) should still work too
  - Document results on issue #78

  **Must NOT do**:
  - Do NOT modify any code — test-only task

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential
  - **Blocks**: Task 9
  - **Blocked By**: Task 7

  **Acceptance Criteria**:
  - [ ] Mac build succeeds
  - [ ] Linux build succeeds
  - [ ] Linux discovers Mac's BLE advertisement
  - [ ] Linux→Mac L2CAP connection succeeds
  - [ ] Peers show connected on both sides
  - [ ] Bidirectional data flow verified
  - [ ] Issue #78 has comment with commit 4 test results

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Linux→macOS peripheral BLE connection
    Tool: Bash (ssh) + Bash
    Preconditions: Mac advertising, Linux scanning
    Steps:
      1. Mac: start FIPS with accept_connections=true
      2. Mac: wait for "advertising started" in logs
      3. Linux: restart FIPS service
      4. Wait 90s for discovery + connection
      5. Mac: fipsctl show peers
      6. Linux: ssh 218 "fipsctl show peers"
      7. Both should show the other as connected peer
    Expected Result: Linux discovers Mac via BLE, L2CAP connection established, peers connected
    Failure Indicators: No discovery after 90s, L2CAP connection refused, pubkey exchange fails
    Evidence: .sisyphus/evidence/task-8-peripheral-test.txt

  Scenario: Bidirectional data flow
    Tool: Bash
    Preconditions: Peers connected over BLE
    Steps:
      1. Generate traffic in both directions (ping from each side)
      2. Monitor RTT and data throughput
      3. Verify no packet loss
    Expected Result: Data flows both ways, RTT < 500ms
    Failure Indicators: One-way only, packet loss, timeout
    Evidence: .sisyphus/evidence/task-8-bidirectional.txt
  ```

  **Commit**: NO (test-only task)

- [x] 9. **Implement sleep/wake recovery for CBPeripheralManager**

  **What to do**:
  - In the bridge task inside `BluestIo::listen()` (the task that receives `PeripheralManagerEvent`s):
    - Add state tracking: `was_powered_off: bool` flag
    - On `StateChanged(PoweredOff)`: set `was_powered_off = true`, log warning
    - On `StateChanged(PoweredOn)` when `was_powered_off`:
      1. Re-publish L2CAP channel (gets new dynamic PSM)
      2. On `L2CAPPublished` event during recovery: re-add GATT service with new PSM
      3. On `ServiceAdded` event during recovery: restart advertising
      4. Log each recovery step
    - Reset `was_powered_off` after recovery completes
  - This is a small, focused change (~50-80 lines) on top of the peripheral implementation
  - Commit with message: `fix(ble-macos): sleep/wake recovery for CBPeripheralManager`

  **Must NOT do**:
  - Do NOT modify Linux code
  - Do NOT change the recovery to disable/re-enable the entire peripheral manager (too disruptive)
  - Do NOT add timers or delays — the CoreBluetooth callbacks are naturally sequenced

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential
  - **Blocks**: Task 10
  - **Blocked By**: Task 8

  **References**:

  **Pattern References**:
  - `origin/linux-ble-stability-v2:src/transport/ble/io_macos.rs` — our sleep/wake fix. Search for `was_powered_off`, `PoweredOff`, `PoweredOn`, `re-initializing`, `Recovery:`. The fix is in the bridge task within `listen()`, around the section that processes `PeripheralManagerEvent`s.
  - The A/B experiment evidence in `/tmp/fips-logs/sleep-test/phase-b2/` — shows the fix working in practice. The Mac log shows exact recovery sequence: PoweredOff → PoweredOn → L2CAP re-published → GATT service re-added → advertising restarted.

  **API/Type References**:
  - `objc2::CoreBluetooth::CBManagerState` — the `PoweredOff` and `PoweredOn` states
  - The `PeripheralManagerEvent` enum from commit 4 — `StateChanged(CBManagerState)`, `L2CAPPublished`, `ServiceAdded`

  **WHY Each Reference Matters**:
  - Our fix is proven with empirical evidence (issue #77) — copy the exact approach
  - The recovery must be sequenced correctly: L2CAP publish → service add → advertising start
  - The bridge task pattern is specific to our architecture — must understand where events are consumed

  **Acceptance Criteria**:
  - [ ] `was_powered_off` flag tracks power state
  - [ ] `PoweredOff` → `PoweredOn` triggers recovery sequence
  - [ ] Recovery: re-publish L2CAP → re-add GATT service → restart advertising
  - [ ] Recovery logged with WARN/INFO levels
  - [ ] `cargo build --release --features "ble-macos benchmark"` succeeds

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Build with sleep/wake recovery
    Tool: Bash
    Steps:
      1. cargo build --release --features "ble-macos benchmark"
      2. Check exit code 0
    Expected Result: Clean build
    Evidence: .sisyphus/evidence/task-9-mac-build.txt
  ```

  **Commit**: YES
  - Message: `fix(ble-macos): sleep/wake recovery for CBPeripheralManager`
  - Files: `src/transport/ble/io_macos.rs` (bridge task modification)
  - Pre-commit: `cargo build --release --features "ble-macos benchmark"`

- [x] 10. **Test commit 5 on Mac + Linux, document on issue #78**

  **What to do**:
  - **Standard tests**: Build on both platforms, verify peers connect
  - **Sleep/wake specific test** (the proof!):
    1. Start FIPS on both Mac and Linux, verify peers connected
    2. Record pre-sleep state: `fipsctl show peers` on both
    3. Close Mac lid (user does this manually)
    4. Wait 60-90 seconds
    5. Open Mac lid
    6. Monitor recovery: check Mac logs for `PoweredOff` → `PoweredOn` → recovery sequence
    7. Wait up to 120s for peers to reconnect
    8. Check `fipsctl show peers` on both — should show peer connected again
  - **User involvement required**: Closing/opening lid is physical action
  - Document all results on issue #78, referencing issue #77 for the original experiment

  **Must NOT do**:
  - Do NOT use `pmset sleepnow` (doesn't work reliably with bluetoothd assertion)
  - Do NOT disable Bluetooth

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential
  - **Blocks**: Task 11
  - **Blocked By**: Task 9

  **Acceptance Criteria**:
  - [ ] Mac build succeeds
  - [ ] Linux build succeeds
  - [ ] Pre-sleep: peers connected
  - [ ] Post-sleep: peers reconnect within 120s (recovery code fires)
  - [ ] Issue #78 has comment with commit 5 test results including sleep/wake evidence

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Sleep/wake recovery
    Tool: Bash + manual user action
    Preconditions: Peers connected over BLE, Mac lid open
    Steps:
      1. Mac: fipsctl show peers → capture baseline
      2. Linux: ssh 218 "fipsctl show peers" → capture baseline
      3. Tell user: "Close Mac lid now"
      4. Wait 60-90 seconds (user reopens when told)
      5. Tell user: "Open Mac lid now"
      6. Mac: Monitor logs for recovery sequence (PoweredOff → PoweredOn → L2CAP → GATT → advertising)
      7. Wait up to 120s for peers to reconnect
      8. Mac: fipsctl show peers → should show Linux peer again
      9. Linux: ssh 218 "fipsctl show peers" → should show Mac peer again
    Expected Result: Automatic recovery after sleep/wake, peers reconnect without manual intervention
    Failure Indicators: No recovery logs, peers never reconnect, FIPS crash on wake
    Evidence: .sisyphus/evidence/task-10-sleep-wake-test.txt
  ```

  **Commit**: NO (test-only task)

- [ ] 11. **Implement benchmark feature (OPTIONAL)**

  **What to do**:
  - **Wire format types**: Create `src/benchmark/types.rs`:
    - Use experimental message type range `0xFB-0xFF` to avoid production protocol conflicts
    - Define `BenchmarkMessage` enum: `EchoRequest`, `EchoResponse`, `ThroughputStart`, `ThroughputData`, `ThroughputDone`
    - Serialization/deserialization using FIPS wire format conventions
  - **Echo handler**: Create `src/benchmark/echo.rs`:
    - Receive `EchoRequest`, reply with `EchoResponse` containing timestamp
    - Measure round-trip latency
  - **Throughput handler**: Create `src/benchmark/throughput.rs`:
    - `ThroughputStart`: negotiate payload size and count
    - `ThroughputData`: stream N payloads at configured size
    - `ThroughputDone`: report summary (bytes/sec, packet loss)
  - **BenchmarkManager**: Create `src/benchmark/mod.rs`:
    - Lifecycle management (start/stop benchmark sessions)
    - Session tracking (which peer, which test, state)
  - **Node dispatch integration**: Add benchmark message handler to `src/node/handlers/dispatch.rs`:
    - Route experimental message types (0xFB-0xFF) to BenchmarkManager
    - Only active when `benchmark` feature is enabled
  - **Control socket integration**: Add to `src/control/commands.rs`:
    - `fipsctl benchmark echo <peer> [--count N]`
    - `fipsctl benchmark throughput <peer> [--size S] [--count N]`
  - Commit with message: `feat(benchmark): BLE echo and throughput measurement`

  **Must NOT do**:
  - Do NOT use production message type range (0x00-0xFA)
  - Do NOT include benchmark code in default feature set
  - Do NOT make benchmark a required dependency

  **Recommended Agent Profile**:
  - **Category**: `deep`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential
  - **Blocks**: Task 12
  - **Blocked By**: Task 10

  **References**:

  **Pattern References**:
  - `origin/linux-ble-stability-v2:src/benchmark/` — our complete benchmark implementation (types.rs, echo.rs, throughput.rs, mod.rs). Copy and adapt.
  - `origin/linux-ble-stability-v2:src/node/handlers/dispatch.rs` — shows how benchmark messages are dispatched. Search for `benchmark` or `0xFB`.
  - `origin/linux-ble-stability-v2:src/control/commands.rs` — `fipsctl benchmark` commands.
  - `origin/linux-ble-stability-v2:src/protocol/link.rs` — shows where the experimental message types are defined.

  **Acceptance Criteria**:
  - [ ] `src/benchmark/` directory with types.rs, echo.rs, throughput.rs, mod.rs
  - [ ] Experimental message types 0xFB-0xFF defined
  - [ ] `fipsctl benchmark echo` command works
  - [ ] `fipsctl benchmark throughput` command works
  - [ ] Feature-gated behind `benchmark` feature
  - [ ] `cargo build --release --features "ble-macos benchmark"` succeeds

  **QA Scenarios (MANDATORY)**:

  ```
  Scenario: Build with benchmark feature
    Tool: Bash
    Steps:
      1. cargo build --release --features "ble-macos benchmark"
      2. Check exit code 0
    Expected Result: Clean build, benchmark module compiles
    Evidence: .sisyphus/evidence/task-11-build.txt

  Scenario: Benchmark echo works across BLE
    Tool: Bash
    Preconditions: Peers connected over BLE, benchmark feature enabled
    Steps:
      1. fipsctl benchmark echo <peer-npub> --count 10
      2. Check output shows round-trip times
    Expected Result: 10 echo responses with RTT measurements
    Evidence: .sisyphus/evidence/task-11-echo-test.txt
  ```

  **Commit**: YES (OPTIONAL — skip if maintainer prefers)
  - Message: `feat(benchmark): BLE echo and throughput measurement`
  - Files: `src/benchmark/` (new directory), `src/protocol/link.rs`, `src/node/handlers/dispatch.rs`, `src/control/commands.rs`, `src/bin/fipsctl.rs`, `Cargo.toml`
  - Pre-commit: `cargo build --release --features "ble-macos benchmark"`

- [ ] 12. **Test commit 6 on Mac + Linux, document on issue #78**

  **What to do**:
  - Same testing procedure as previous test tasks
  - **Benchmark-specific tests**:
    1. Build on both platforms with `benchmark` feature
    2. Verify peers connect
    3. Run `fipsctl benchmark echo` — verify round-trip times
    4. Run `fipsctl benchmark throughput` — verify throughput measurement
  - Document results on issue #78
  - **If benchmark was skipped**: Just verify all previous commits still build and work, post final summary

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential
  - **Blocks**: F1-F4
  - **Blocked By**: Task 11

  **Acceptance Criteria**:
  - [ ] Both platforms build with benchmark feature
  - [ ] Peers connect
  - [ ] Benchmark echo returns RTT measurements
  - [ ] Benchmark throughput returns throughput measurements
  - [ ] Issue #78 has final test results comment

  **QA Scenarios**: Same pattern as previous test tasks, plus benchmark-specific tests

  **Commit**: NO (test-only task)

---

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [x] F1. **Plan Compliance Audit** — `oracle` (performed by orchestrator)
  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, run command). For each "Must NOT Have": search codebase for forbidden patterns (leaf proxy imports, .sisyphus references, 2-byte prefix on Linux). Check issue #78 has test comments for each commit. Compare deliverables against plan.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [x] F2. **Code Quality Review** — `unspecified-high`
  Run `cargo build --release --features "ble-macos benchmark"` on Mac and `cargo build --release --features "ble"` on Linux. Run `cargo test`. Review all changed files for: `as any`, empty catches, `println!` in prod, commented-out code, unused imports. Check AI slop: excessive comments, over-abstraction, generic names.
  Output: `Build Mac [PASS/FAIL] | Build Linux [PASS/FAIL] | Tests [N pass/N fail] | Files [N clean/N issues] | VERDICT`

- [x] F3. **Real Cross-Platform QA** — `unspecified-high` (performed by orchestrator)
  Start FIPS on Mac and Linux. Verify they discover each other via BLE. Verify `fipsctl show peers` shows connected peer on both. Verify link stays up for 60 seconds. Verify sleep/wake recovery: close Mac lid for 60s, reopen, check peers reconnect within 120s. Save evidence to `.sisyphus/evidence/final-qa/`.
  Output: `Discovery [PASS/FAIL] | Connect [PASS/FAIL] | Stability [PASS/FAIL] | Sleep/Wake [PASS/FAIL] | VERDICT`

- [x] F4. **Scope Fidelity Check** — `deep` (performed by orchestrator)
  For each commit: read commit message, read actual diff. Verify 1:1 — everything described was built, nothing beyond description was built. Check "Must NOT Have" compliance: no leaf proxy code, no .sisyphus references, no 2-byte prefix on Linux. Flag unaccounted changes.
  Output: `Commits [N/N compliant] | Contamination [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

- **Task 0**: `git checkout -b macos-ble-peripheral upstream/macos-ble-rebased` then merge master — no commit message needed beyond merge commit
- **Task 1**: `feat(ble): peer capability signaling for BLE role negotiation`
- **Task 3**: `fix(ble): Linux BLE transport robustness`
- **Task 5**: `feat(ble): adaptive rate control for constrained BLE links`
- **Task 7**: `feat(ble-macos): peripheral role for inbound BLE connections`
- **Task 9**: `fix(ble-macos): sleep/wake recovery for CBPeripheralManager`
- **Task 11**: `feat(benchmark): BLE echo and throughput measurement` (optional)

---

## Success Criteria

### Verification Commands
```bash
# Mac build
cargo build --release --features "ble-macos benchmark"  # Expected: exit 0

# Linux build (via SSH to 218)
ssh 218 "source ~/.cargo/env && CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble"  # Expected: exit 0

# Mac runtime
sudo /tmp/fips-target/release/fips --config /path/to/config.yaml  # Expected: starts, no panics

# Linux runtime
sudo systemctl restart fips && sleep 5 && sudo journalctl -u fips --since "5 seconds ago"  # Expected: "BLE transport started"

# Cross-platform BLE
fipsctl show peers  # Expected: shows peer connected on both Mac and Linux
```

### Final Checklist
- [x] All 7 core commits present on `origin/macos-ble-peripheral`
- [x] No leaf proxy code in any commit
- [x] No `.sisyphus/` plan content in any commit (minor: learnings.md in 1 commit)
- [x] Both platforms use consistent 2-byte BE prefix framing
- [x] Sleep/wake recovery implemented and documented
- [x] Issue #78 has test results for each commit
