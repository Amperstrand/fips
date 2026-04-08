# Cherry-Pick BLE Fixes onto Upstream `jmcorgan/macos-ble`

## TL;DR

> **Quick Summary**: Create a clean branch from upstream's `jmcorgan/macos-ble` with cherry-picked BLE bug fixes from our work, and create GitHub issues on Amperstrand/fips for each fix arguing both FOR and AGAINST upstream inclusion.
> 
> **Deliverables**:
> - Branch `cherry-picks-for-upstream` based on `jmcorgan/macos-ble` with 7 cherry-picked commits
> - 6 GitHub issues on Amperstrand/fips (one per fix group) with FOR/AGAINST arguments
> - Branch pushed to `origin` (no PR)
> 
> **Estimated Effort**: Medium
> **Parallel Execution**: YES — 2 waves
> **Critical Path**: Task 1 (branch + cherry-picks) → Task 2 (build verify) → Tasks 3-8 (issues, parallel) → Task 9 (push)

---

## Context

### Original Request
Cherry-pick useful BLE bug fixes from our `macos-support` branch onto upstream's `jmcorgan/macos-ble` branch, creating clean commits with matching GitHub issues that argue both for AND against upstream adoption. Purpose: make it easy for upstream to reuse our work if useful.

### Interview Summary
**Key Discussions**:
- Only "actually useful" fixes — no overengineering, no debug logging, no HTTP proxy
- Each cherry-pick gets a matching GitHub issue arguing both sides
- Push branch but no PR yet
- Issues go on Amperstrand/fips fork

**Research Findings**:
- Upstream's `jmcorgan/macos-ble` (commit `3621e4b`) adds length-prefix framing to ALL BLE send/recv — this solves coalesced-frame problems differently from our FMP-boundary splitter
- Our frame splitting fix (cda7979 + bbbe6eb) would conflict semantically with upstream's framing — **excluded**
- Upstream still has `LePublic` in `addr.rs` — our `LeRandom` fix is needed
- 4 of 8 original commits cherry-pick cleanly; 3 will conflict in `ble/mod.rs` due to `cfg`-gating changes
- Existing issues #7, #8, #16, #19, #20 cover some of these bugs — new issues will cross-reference

### Metis Review
**Identified Gaps** (addressed):
- Frame splitting would double-frame with upstream's length-prefix → EXCLUDED
- LeRandom fix was missing from cherry-pick list → ADDED as group 7
- Existing issues overlap → new issues will cross-reference, not duplicate
- `git fetch jmcorgan` needed before branching → added as step 0
- Conflict resolution strategy needed → manual resolve with compilation check per commit

---

## Work Objectives

### Core Objective
Present our BLE bug fixes cleanly on top of upstream's latest branch, with documentation that makes it trivial for the upstream maintainer to evaluate and adopt each fix independently.

### Concrete Deliverables
- Branch `cherry-picks-for-upstream` from `jmcorgan/macos-ble` with 7 cherry-picked commits
- 6 GitHub issues on Amperstrand/fips with `upstream` label
- Branch pushed to `origin/cherry-picks-for-upstream`

### Definition of Done
- [ ] `cargo check --features ble` passes on the cherry-pick branch
- [ ] `git log --oneline jmcorgan/macos-ble..cherry-picks-for-upstream` shows exactly 7 commits
- [ ] All 7 commits have `(cherry picked from commit ...)` in their body
- [ ] 6 GitHub issues exist with `upstream` label on Amperstrand/fips
- [ ] Branch pushed to `origin/cherry-picks-for-upstream`

### Must Have
- Original commit authorship preserved on all cherry-picks
- Each cherry-pick uses `-x` flag for provenance tracking
- Each issue argues both FOR and AGAINST inclusion
- Each issue cross-references existing related issues
- Compilation verified after each conflict resolution

### Must NOT Have (Guardrails)
- Frame splitting commits (cda7979, bbbe6eb) — upstream's length-prefix framing supersedes
- Any debug logging commits (AAD logging, key fingerprints)
- HTTP proxy/visualizer code
- Config files, test keypairs, Swift prototypes
- Modified commit messages (beyond cherry-pick note appended by `-x`)
- Force-push to any existing branch
- `.sisyphus/` notepad files from cherry-picked commits (strip if present)
- PR creation — push only

---

## Verification Strategy (MANDATORY)

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES (cargo test)
- **Automated tests**: Tests-after (compile check per commit, full test at end)
- **Framework**: cargo check / cargo test

### QA Policy
Every task includes agent-executed QA scenarios.
Evidence saved to `.sisyphus/evidence/task-{N}-{scenario-slug}.{ext}`.

- **Git operations**: Use Bash — verify commit count, authorship, cherry-pick notes
- **Build**: Use Bash — `cargo check --features ble`, `cargo test --lib`
- **GitHub issues**: Use Bash — `gh issue view` to verify content

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Sequential — cherry-picks must be ordered):
└── Task 1: Create branch + cherry-pick all 7 commits [deep]
└── Task 2: Build verification [quick]

Wave 2 (After Wave 1 — all 6 issues in parallel):
├── Task 3: Issue for cross-connection fix [quick]
├── Task 4: Issue for L2CAP disconnect notification [quick]
├── Task 5: Issue for stale peer detection [quick]
├── Task 6: Issue for MSG1 rate limit [quick]
├── Task 7: Issue for disable_tiebreaker [quick]
└── Task 8: Issue for LeRandom address type [quick]

Wave 3 (After Wave 2):
└── Task 9: Push branch + final verification [quick]

Wave FINAL (After ALL tasks — review):
├── Task F1: Plan compliance audit (oracle)
├── Task F2: Code quality review (unspecified-high)
├── Task F3: Real manual QA (unspecified-high)
└── Task F4: Scope fidelity check (deep)
-> Present results -> Get explicit user okay
```

### Dependency Matrix

| Task | Depends On | Blocks |
|------|-----------|--------|
| 1 | — | 2, 3-8 |
| 2 | 1 | 3-8, 9 |
| 3-8 | 2 | 9 |
| 9 | 3-8 | F1-F4 |
| F1-F4 | 9 | user okay |

### Agent Dispatch Summary

- **Wave 1**: 2 tasks — T1 → `deep`, T2 → `quick`
- **Wave 2**: 6 tasks — T3-T8 → `quick` (all parallel)
- **Wave 3**: 1 task — T9 → `quick`
- **FINAL**: 4 tasks — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [ ] 1. Create branch and cherry-pick all commits

  **What to do**:
  1. Run `git fetch jmcorgan` to ensure `jmcorgan/macos-ble` is current
  2. Create branch: `git checkout -b cherry-picks-for-upstream jmcorgan/macos-ble`
  3. Cherry-pick in this exact order (use `-x` flag on all):
     - `git cherry-pick -x ea375c7` — cross-connection: close stale outbound socket (CLEAN)
     - `git cherry-pick -x d032728` — cross-connection: block outbound pool insert (CONFLICT expected in `ble/mod.rs`)
     - `git cherry-pick -x 516fe87` — L2CAP disconnect notification (CONFLICT expected in `ble/mod.rs`, `transport/mod.rs`, `node/mod.rs`)
     - `git cherry-pick -x 72979b8` — stale peer detection (CLEAN)
     - `git cherry-pick -x 808214c` — per-peer MSG1 rate limit (CLEAN)
     - `git cherry-pick -x 929b734` — disable_tiebreaker config (CLEAN or minor conflict in `ble/mod.rs`)
     - `git cherry-pick -x b4800c2` — LeRandom address type (CLEAN — touches `addr.rs` only)
  4. For each conflict, resolve by:
     - Keep upstream's `cfg` gating structure
     - Apply our functional changes within the appropriate `cfg` blocks
     - Run `cargo check --features ble` after each conflict resolution
     - `git add` resolved files and `git cherry-pick --continue`
  5. If `bbbe6eb` (the notepad file commit) is referenced by `929b734`, strip the `.sisyphus/` file changes using `git checkout HEAD -- .sisyphus/` before continuing

  **Must NOT do**:
  - Cherry-pick cda7979 or bbbe6eb (frame splitting — superseded by upstream's length-prefix framing)
  - Cherry-pick any debug logging commits
  - Modify commit messages beyond what `-x` appends
  - Squash or reorder commits within a dependency group

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Conflict resolution requires understanding both upstream's cfg-gating and our functional changes, reading both versions carefully
  - **Skills**: [`git-master`]
    - `git-master`: Cherry-pick with conflict resolution is core git workflow

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 1 (sequential with Task 2)
  - **Blocks**: Tasks 2, 3-8, 9
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `src/transport/ble/mod.rs` (upstream version at `jmcorgan/macos-ble`) — the base file where most conflicts occur. Note `cfg(all(feature = "ble", target_os = "linux"))` gating patterns
  - `src/transport/ble/mod.rs` (our version at `macos-support`) — our changes with `blocked_outbound_addrs`, disconnect channel, etc.

  **API/Type References**:
  - `src/transport/mod.rs` (upstream) — `TransportHandle` enum with `cfg` gates for `ble-macos`
  - `src/node/mod.rs` (upstream) — Node struct with `tun_shutdown_fd` additions

  **Commit References** (the actual commits to cherry-pick):
  - `ea375c7` — adds `blocked_outbound_addrs: HashSet` to `BleTransportInner`, checks before `insert_outbound`
  - `d032728` — adds `is_blocked()` check in the outbound pool insert path in `scan_probe_loop`
  - `516fe87` — adds `TransportDisconnect` variant, `disconnect_tx/rx` channels through Node/lifecycle, and disconnect detection in `receive_loop`
  - `72979b8` — adds stale peer removal in `handle_handshake_msg1` before re-handshake
  - `808214c` — adds `competing_msg1_counts: HashMap` and `MAX_COMPETING_MSG1` const, drops connection after threshold
  - `929b734` — adds `disable_tiebreaker: bool` to `BleConfig` and checks in `scan_probe_loop`/`accept_loop`
  - `b4800c2` — changes `LePublic` to `LeRandom` in `addr.rs:to_socket_addr()`

  **Conflict Resolution Guide**:
  - `d032728` conflict in `ble/mod.rs`: upstream added `#[cfg(all(feature = "ble", target_os = "linux", not(test)))]` blocks. Our `blocked_outbound_addrs` field goes inside the `BleTransportInner` struct (which is behind the linux cfg gate). Apply the new field, keep upstream's cfg structure.
  - `516fe87` conflict in `ble/mod.rs`: upstream restructured with cfg gates for macos. Our disconnect channel plumbing should go inside the linux-gated sections. In `transport/mod.rs`: `TransportDisconnect` variant can be added unconditionally (it's a Node-level concept, not platform-specific). In `node/mod.rs`: upstream added `tun_shutdown_fd` — our `disconnect_rx` field is separate, add alongside.
  - `929b734` potential conflict in `ble/mod.rs`: `disable_tiebreaker` field in `BleConfig` (config/transport.rs, usually clean) and checks in `scan_probe_loop`/`accept_loop` (may conflict if cfg gating changed these functions)

  **Acceptance Criteria**:

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: All 7 cherry-picks applied successfully
    Tool: Bash
    Preconditions: On branch cherry-picks-for-upstream
    Steps:
      1. Run: git log --oneline jmcorgan/macos-ble..cherry-picks-for-upstream
      2. Count lines in output
      3. Verify each line contains expected commit summary text
    Expected Result: Exactly 7 commits listed, each with recognizable subject line
    Failure Indicators: Fewer than 7 commits, or missing expected subject text
    Evidence: .sisyphus/evidence/task-1-cherry-pick-log.txt

  Scenario: Cherry-pick provenance preserved
    Tool: Bash
    Preconditions: On branch cherry-picks-for-upstream
    Steps:
      1. For each of the 7 commits, run: git log --format="%b" -1 <sha>
      2. Check each body contains "(cherry picked from commit"
      3. Verify the original SHA is referenced
    Expected Result: All 7 commits have cherry-pick provenance notes
    Failure Indicators: Any commit missing the "(cherry picked from commit" line
    Evidence: .sisyphus/evidence/task-1-provenance-check.txt

  Scenario: Original authorship preserved
    Tool: Bash
    Preconditions: On branch cherry-picks-for-upstream
    Steps:
      1. Run: git log --format="%an <%ae>" jmcorgan/macos-ble..cherry-picks-for-upstream
      2. Check that author is NOT "Sisyphus" or any agent name as sole author
    Expected Result: Original commit authors preserved (e.g., "Amperstrand <amperstrand@localhost>")
    Failure Indicators: Agent name appears as author
    Evidence: .sisyphus/evidence/task-1-authorship-check.txt

  Scenario: No frame splitting commits included
    Tool: Bash
    Preconditions: On branch cherry-picks-for-upstream
    Steps:
      1. Run: git log --oneline jmcorgan/macos-ble..cherry-picks-for-upstream | grep -i "frame\|coalesce\|split"
      2. Verify empty output
    Expected Result: No matches — frame splitting commits not included
    Failure Indicators: Any match found
    Evidence: .sisyphus/evidence/task-1-no-frame-split.txt
  ```

  **Commit**: NO (cherry-picks ARE the commits)

---

- [ ] 2. Build verification on cherry-pick branch

  **What to do**:
  1. Ensure on branch `cherry-picks-for-upstream`
  2. Run `cargo check --features ble` — must exit 0
  3. Run `cargo test --lib` — capture pass/fail count
  4. Run `cargo clippy --features ble -- -D warnings` — note any warnings (don't need to fix, just report)

  **Must NOT do**:
  - Fix any pre-existing warnings from upstream code
  - Modify any source files

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Just running build commands and checking output
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on Task 1)
  - **Parallel Group**: Wave 1 (after Task 1)
  - **Blocks**: Tasks 3-8, 9
  - **Blocked By**: Task 1

  **References**:

  **Pattern References**:
  - `Cargo.toml` — features definition, `ble` feature flag
  - Upstream builds with zero warnings on `jmcorgan/macos-ble`

  **Acceptance Criteria**:

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: cargo check passes with BLE feature
    Tool: Bash
    Preconditions: On branch cherry-picks-for-upstream after Task 1
    Steps:
      1. Run: cargo check --features ble 2>&1
      2. Check exit code is 0
    Expected Result: Exit code 0, no errors
    Failure Indicators: Non-zero exit code, "error[E" in output
    Evidence: .sisyphus/evidence/task-2-cargo-check.txt

  Scenario: Library tests pass
    Tool: Bash
    Preconditions: On branch cherry-picks-for-upstream
    Steps:
      1. Run: cargo test --lib 2>&1
      2. Parse "test result:" line
    Expected Result: "test result: ok" with 0 failures
    Failure Indicators: Any test failures
    Evidence: .sisyphus/evidence/task-2-cargo-test.txt
  ```

  **Commit**: NO

---

- [ ] 3. GitHub issue: Cross-connection deadlock fix

  **What to do**:
  Create a GitHub issue on Amperstrand/fips with these sections:
  - **Title**: `upstream: fix BLE cross-connection deadlock (blocked_outbound_addrs)`
  - **Labels**: `upstream`
  - **Body**:
    - **Commits**: `ea375c7`, `d032728` (cherry-picked onto `cherry-picks-for-upstream`)
    - **Problem**: When two BLE peers discover each other simultaneously, both create outbound connections. The "cross-connection" detector closes the losing connection, but the outbound probe loop immediately re-inserts the stale socket into the pool, creating an infinite connect/disconnect cycle that blocks all other BLE traffic.
    - **Fix**: `ea375c7` closes the stale outbound transport socket when cross-connection is detected. `d032728` adds a `blocked_outbound_addrs` set to prevent re-insertion after resolution.
    - **Arguments FOR upstream adoption**:
      - Fixes a real deadlock observed with ESP32 BLE controllers
      - Without this, two-node BLE meshes can permanently deadlock after restart
      - Minimal code change: one HashSet field + two checks
      - Defense-in-depth: even if cross-connection logic changes, blocked addrs prevents the loop
    - **Arguments AGAINST / Why it could be ignored**:
      - Only observed with ESP32 controllers that don't properly close L2CAP channels
      - Upstream's length-prefix framing may change the reconnection dynamics enough to avoid this
      - The `blocked_outbound_addrs` set is never cleaned up (grows monotonically for the session lifetime) — could be considered a small memory leak for long-running nodes with many peers
    - **Cross-references**: Closes #16, supersedes #19

  **Must NOT do**:
  - Create issue on jmcorgan/fips (only on Amperstrand/fips)
  - Include implementation details beyond what's needed to understand the fix

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Single GitHub issue creation with predetermined content
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4, 5, 6, 7, 8)
  - **Blocks**: Task 9
  - **Blocked By**: Task 2

  **References**:

  **Pattern References**:
  - Existing issue #16 on Amperstrand/fips — original bug report for cross-connection deadlock
  - Existing issue #19 on Amperstrand/fips — stale outbound link report

  **Commit References**:
  - `ea375c7` — the core fix (close stale outbound socket)
  - `d032728` — defense-in-depth (blocked_outbound_addrs)

  **Acceptance Criteria**:

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Issue created with correct content
    Tool: Bash
    Preconditions: gh cli authenticated
    Steps:
      1. Run: gh issue list -R Amperstrand/fips --label upstream --state open --json number,title
      2. Find issue with title containing "cross-connection"
      3. Run: gh issue view <number> -R Amperstrand/fips --json body
      4. Verify body contains "ea375c7", "d032728", "Arguments FOR", "Arguments AGAINST"
    Expected Result: Issue exists with all required sections
    Failure Indicators: Missing issue, missing sections, wrong commits referenced
    Evidence: .sisyphus/evidence/task-3-issue-cross-connection.txt
  ```

  **Commit**: NO

---

- [ ] 4. GitHub issue: L2CAP disconnect notification

  **What to do**:
  Create a GitHub issue on Amperstrand/fips:
  - **Title**: `upstream: fix BLE session state not cleared on L2CAP disconnect`
  - **Labels**: `upstream`
  - **Body**:
    - **Commit**: `516fe87` (cherry-picked onto `cherry-picks-for-upstream`)
    - **Problem**: When a BLE L2CAP connection drops (peer walks away, controller reset, etc.), the Node layer has no notification. The stale Noise session remains in memory with old keys. When the peer reconnects, msg1 decryption fails because the responder still holds the old session state. Recovery requires waiting for a 30-second heartbeat timeout.
    - **Fix**: Adds a `TransportDisconnect` channel from BLE transport to Node. When `receive_loop` detects connection close (recv returns 0), it sends a disconnect notification. Node immediately resets the Noise session state for that peer, enabling instant re-handshake.
    - **Arguments FOR**:
      - Reduces reconnection time from 30s (heartbeat timeout) to <1s
      - Critical for BLE where connections are inherently unreliable
      - Clean channel-based design — no polling, no timers
      - Essential for mobile/MCU peers that frequently go in/out of range
    - **Arguments AGAINST**:
      - Adds a new channel (disconnect_tx/rx) threading through Node, lifecycle, and transport layers — moderate plumbing
      - The 30s heartbeat timeout IS the existing recovery mechanism — this just makes it faster
      - Could trigger premature session reset if L2CAP has transient errors that self-heal
    - **Cross-references**: Fixes #20

  **Must NOT do**:
  - Duplicate the detailed analysis already in issue #20

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 3, 5, 6, 7, 8)
  - **Blocks**: Task 9
  - **Blocked By**: Task 2

  **References**:
  - Existing issue #20 on Amperstrand/fips — original bug report
  - `516fe87` commit

  **Acceptance Criteria**:

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Issue created with correct content
    Tool: Bash
    Steps:
      1. Run: gh issue list -R Amperstrand/fips --label upstream --state open --json number,title
      2. Find issue with title containing "disconnect" or "session state"
      3. Verify body contains "516fe87", "Arguments FOR", "Arguments AGAINST"
    Expected Result: Issue exists with all required sections
    Evidence: .sisyphus/evidence/task-4-issue-disconnect.txt
  ```

  **Commit**: NO

---

- [ ] 5. GitHub issue: Stale peer detection

  **What to do**:
  Create a GitHub issue on Amperstrand/fips:
  - **Title**: `upstream: detect and remove stale peers before re-handshake`
  - **Labels**: `upstream`
  - **Body**:
    - **Commit**: `72979b8` (cherry-picked onto `cherry-picks-for-upstream`)
    - **Problem**: When a peer reconnects after a crash/restart, its old peer entry may still exist in the node's peer table with stale routing state (old coordinates, old links). The new handshake succeeds but the node layer has two conflicting entries — one stale, one fresh — causing routing confusion and duplicate packet delivery.
    - **Fix**: Before processing a new msg1 from a peer whose npub is already in the peer table, remove the stale peer entry entirely. This ensures the handshake creates a clean peer with fresh state.
    - **Arguments FOR**:
      - Fixes routing corruption after peer restart
      - Simple and surgical — one check before handshake processing
      - No behavioral change for normal operation (check is no-op if peer doesn't exist)
      - Essential for BLE where peers restart frequently (battery, firmware update, etc.)
    - **Arguments AGAINST**:
      - Could discard valid peer state if msg1 is received from an attacker spoofing the npub (mitigated by Noise authentication — msg1 is authenticated via static key)
      - In multi-transport scenarios, removing the peer removes ALL transport links, not just the one that triggered the re-handshake
    - **Cross-references**: Related to #13 (robust handling for broken peers)

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 9
  - **Blocked By**: Task 2

  **References**:
  - `72979b8` commit
  - `src/node/handlers/handshake.rs` — where the check is added

  **Acceptance Criteria**:

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Issue created with correct content
    Tool: Bash
    Steps:
      1. Find issue with title containing "stale peer"
      2. Verify body contains "72979b8", "Arguments FOR", "Arguments AGAINST"
    Expected Result: Issue exists with all required sections
    Evidence: .sisyphus/evidence/task-5-issue-stale-peer.txt
  ```

  **Commit**: NO

---

- [ ] 6. GitHub issue: Per-peer MSG1 rate limit

  **What to do**:
  Create a GitHub issue on Amperstrand/fips:
  - **Title**: `upstream: add per-peer competing MSG1 rate limit for DoS protection`
  - **Labels**: `upstream`
  - **Body**:
    - **Commit**: `808214c` (cherry-picked onto `cherry-picks-for-upstream`)
    - **Problem**: A malfunctioning or malicious BLE peer can send msg1 handshake initiation packets repeatedly. Each msg1 triggers expensive Noise IK processing (Diffie-Hellman, AEAD). Without rate limiting, a single peer can consume all CPU on the node, effectively DoS-ing the entire mesh.
    - **Fix**: Adds a `competing_msg1_counts: HashMap<NodeAddr, u32>` that tracks consecutive msg1 failures per peer. After `MAX_COMPETING_MSG1` (3) failures, the connection is dropped. Counter resets on successful handshake completion.
    - **Arguments FOR**:
      - Protects against both malicious and malfunctioning peers
      - Observed in practice: ESP32 with broken firmware sends msg1 in a tight loop
      - Threshold of 3 is generous — legitimate handshake rarely needs >1 retry
      - Per-peer isolation — one bad peer doesn't affect others
    - **Arguments AGAINST**:
      - The rate limit is per-connection, not per-peer-identity — a peer could reconnect and get a fresh counter
      - Doesn't implement exponential backoff (just hard cutoff)
      - The HashMap grows monotonically during a session (entries never removed for peers that disconnect cleanly without hitting the limit)
    - **Cross-references**: Partially addresses #13 (robust handling for broken peers), related to #17 (consecutive msg1 failures)

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 9
  - **Blocked By**: Task 2

  **References**:
  - `808214c` commit
  - `src/node/handlers/handshake.rs` — rate limit logic
  - `src/node/handlers/dispatch.rs` — counter integration

  **Acceptance Criteria**:

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Issue created with correct content
    Tool: Bash
    Steps:
      1. Find issue with title containing "MSG1" or "rate limit"
      2. Verify body contains "808214c", "Arguments FOR", "Arguments AGAINST"
    Expected Result: Issue exists with all required sections
    Evidence: .sisyphus/evidence/task-6-issue-msg1-limit.txt
  ```

  **Commit**: NO

---

- [ ] 7. GitHub issue: disable_tiebreaker config

  **What to do**:
  Create a GitHub issue on Amperstrand/fips:
  - **Title**: `upstream: add disable_tiebreaker config for leaf/MCU BLE peers`
  - **Labels**: `upstream`
  - **Body**:
    - **Commit**: `929b734` (cherry-picked onto `cherry-picks-for-upstream`)
    - **Problem**: The BLE cross-probe tie-breaker uses address comparison to deterministically resolve simultaneous connections. This works for symmetric peers, but MCU/leaf peers (ESP32, mobile) can only initiate connections — they cannot accept. The tie-breaker may incorrectly tell the full node to "yield" its outbound probe to the MCU, but the MCU can never create the expected inbound connection, resulting in permanent disconnection.
    - **Fix**: Adds `disable_tiebreaker: bool` to `BleConfig`. When true, the node always accepts inbound connections and always keeps its outbound probes, bypassing the address-based tie-breaker. Intended for nodes that peer exclusively with asymmetric (central-only) devices.
    - **Arguments FOR**:
      - Essential for MCU/leaf peer interop (ESP32, mobile devices)
      - Minimal change: one bool field + two conditional checks
      - Opt-in via config — no behavioral change for existing deployments
      - Already proven in production with ESP32 BLE peers
    - **Arguments AGAINST**:
      - Could cause duplicate connections if enabled between two full nodes (both keep outbound + accept inbound)
      - A per-peer config would be more precise than a global transport-level flag
      - The tie-breaker logic itself could be improved to detect asymmetric peers instead of requiring manual config
    - **Cross-references**: Related to #8 (previously closed as applied), #5 (upstream tracking)

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 9
  - **Blocked By**: Task 2

  **References**:
  - `929b734` commit
  - `src/config/transport.rs` — BleConfig struct
  - `src/transport/ble/mod.rs` — tie-breaker checks in scan_probe_loop/accept_loop

  **Acceptance Criteria**:

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Issue created with correct content
    Tool: Bash
    Steps:
      1. Find issue with title containing "tiebreaker" or "disable_tiebreaker"
      2. Verify body contains "929b734", "Arguments FOR", "Arguments AGAINST"
    Expected Result: Issue exists with all required sections
    Evidence: .sisyphus/evidence/task-7-issue-tiebreaker.txt
  ```

  **Commit**: NO

---

- [ ] 8. GitHub issue: LeRandom address type fix

  **What to do**:
  Create a GitHub issue on Amperstrand/fips:
  - **Title**: `upstream: use LeRandom address type for BLE L2CAP connections`
  - **Labels**: `upstream`
  - **Body**:
    - **Commit**: `b4800c2` (cherry-picked onto `cherry-picks-for-upstream`)
    - **Problem**: `BleAddr::to_socket_addr()` uses `AddressType::LePublic`, but most BLE devices (especially mobile and MCU) use random addresses (either static random or resolvable private). Using `LePublic` when the remote device has a random address causes `connect()` to fail silently or connect to the wrong device.
    - **Fix**: Changes `LePublic` to `LeRandom` in `addr.rs:to_socket_addr()`. One-line change.
    - **Arguments FOR**:
      - One-line correctness fix
      - Most real-world BLE devices use random addresses
      - Without this, L2CAP connections fail for any peer using a random address
      - Already tested with ESP32 BLE peers (which use random addresses)
    - **Arguments AGAINST**:
      - Hardcoding `LeRandom` is also wrong for devices that genuinely use public addresses
      - The correct fix might be to detect address type from the scan result and pass it through
      - BlueZ may auto-detect the correct address type in some kernel versions, making this unnecessary
    - **Cross-references**: Related to #7 (previously closed), #5 (upstream tracking)

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 9
  - **Blocked By**: Task 2

  **References**:
  - `b4800c2` commit
  - `src/transport/ble/addr.rs:to_socket_addr()` — the one-line change

  **Acceptance Criteria**:

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Issue created with correct content
    Tool: Bash
    Steps:
      1. Find issue with title containing "LeRandom" or "address type"
      2. Verify body contains "b4800c2", "Arguments FOR", "Arguments AGAINST"
    Expected Result: Issue exists with all required sections
    Evidence: .sisyphus/evidence/task-8-issue-lerandom.txt
  ```

  **Commit**: NO

---

- [ ] 9. Push branch and final verification

  **What to do**:
  1. Push the branch: `git push -u origin cherry-picks-for-upstream`
  2. Verify remote: `git log --oneline origin/cherry-picks-for-upstream`
  3. Verify all 6 issues exist: `gh issue list -R Amperstrand/fips --label upstream --state open`
  4. Capture final state

  **Must NOT do**:
  - Create a PR
  - Force-push
  - Push to any branch other than `cherry-picks-for-upstream`

  **Recommended Agent Profile**:
  - **Category**: `quick`
  - **Skills**: [`git-master`]
    - `git-master`: Clean push with verification

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (after all issues created)
  - **Blocks**: F1-F4
  - **Blocked By**: Tasks 3-8

  **References**:
  - Branch `cherry-picks-for-upstream` (created in Task 1)

  **Acceptance Criteria**:

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Branch pushed to origin
    Tool: Bash
    Steps:
      1. Run: git log --oneline origin/cherry-picks-for-upstream | wc -l
      2. Run: git log --oneline origin/cherry-picks-for-upstream | head -10
    Expected Result: Branch exists on remote with correct commit count
    Evidence: .sisyphus/evidence/task-9-push-verify.txt

  Scenario: All upstream issues exist
    Tool: Bash
    Steps:
      1. Run: gh issue list -R Amperstrand/fips --label upstream --state open --json number,title
      2. Count issues with "upstream:" prefix in title
    Expected Result: At least 6 new upstream issues (may have existing ones too)
    Evidence: .sisyphus/evidence/task-9-issues-verify.txt
  ```

  **Commit**: NO

---

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [ ] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists. For each "Must NOT Have": verify absence. Check: 7 cherry-pick commits present, 6 issues created, branch pushed, no frame splitting commits, no PR created.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [ ] F2. **Code Quality Review** — `unspecified-high`
  Run `cargo check --features ble` and `cargo test --lib`. Review cherry-picked commits for: correct conflict resolution, no unintended changes, cfg gates match upstream's pattern. Verify no `.sisyphus/` files leaked into commits.
  Output: `Build [PASS/FAIL] | Tests [N pass/N fail] | Cherry-picks [N clean/N issues] | VERDICT`

- [ ] F3. **Real Manual QA** — `unspecified-high`
  Verify each GitHub issue: has title, labels, all 5 required sections (commit, problem, fix, FOR, AGAINST), cross-references existing issues where applicable. Verify branch on GitHub matches local. Check no duplicate issues were created.
  Output: `Issues [N/N complete] | Duplicates [CLEAN/N found] | Branch [matches/diverged] | VERDICT`

- [ ] F4. **Scope Fidelity Check** — `deep`
  For each cherry-picked commit: compare the diff on our branch vs the original commit diff. Verify only cfg-gate adaptations were made during conflict resolution, no functional changes were altered. Check that exactly the 7 intended commits are on the branch — no more, no less.
  Output: `Commits [N/N faithful] | Additions [CLEAN/N extra] | Modifications [CLEAN/N altered] | VERDICT`

---

## Commit Strategy

No new commits are created by this plan — cherry-picks ARE the commits. The push is the final deliverable.

---

## Success Criteria

### Verification Commands
```bash
# Branch has exactly 7 commits on top of upstream
git log --oneline jmcorgan/macos-ble..cherry-picks-for-upstream | wc -l
# Expected: 7

# All commits have cherry-pick provenance
git log --format="%b" jmcorgan/macos-ble..cherry-picks-for-upstream | grep -c "cherry picked from"
# Expected: 7

# Build passes
cargo check --features ble
# Expected: exit 0

# Issues exist
gh issue list -R Amperstrand/fips --label upstream --state open --json title | jq length
# Expected: >= 6

# Branch pushed
git log --oneline origin/cherry-picks-for-upstream | wc -l
# Expected: non-zero
```

### Final Checklist
- [ ] 7 cherry-pick commits on branch (ea375c7, d032728, 516fe87, 72979b8, 808214c, 929b734, b4800c2)
- [ ] All commits have `-x` provenance notes
- [ ] Original authorship preserved
- [ ] `cargo check --features ble` passes
- [ ] 6 GitHub issues with `upstream` label
- [ ] Each issue has: commit SHA, problem, fix, FOR, AGAINST, cross-references
- [ ] Branch pushed to origin
- [ ] No PR created
- [ ] No frame splitting commits included
- [ ] No debug/logging commits included
