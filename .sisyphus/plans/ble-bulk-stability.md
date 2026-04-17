# BLE Bulk Stability Work Plan

## TL;DR

> **Quick Summary**: Stabilize BLE bulk transfer between Linux and macOS by hardening writer-side backpressure handling, using conservative/adaptive pacing, and validating with the full hardware campaign.
>
> **Deliverables**:
> - Stable Linux↔macOS BLE control plane and bulk transfer path
> - Hardened macOS writer/backpressure implementation
> - Full hardware evidence for TEST A-E
> - mac↔mac-ready writer/backpressure design direction
>
> **Estimated Effort**: Large
> **Parallel Execution**: YES - 4 waves
> **Critical Path**: writer diagnosis → writer hardening → focused bulk tests → full campaign

---

## Context

### Original Request
Get Linux and macOS BLE transport to a stable, working state where both platforms can play both central/peripheral roles, sustain iperf in both directions, and keep ping latency acceptable during iperf. Artificial rate limiting is acceptable if needed for stability, but established patterns are preferred.

### Interview Summary
**Key Discussions**:
- Linux and macOS must both support central and peripheral BLE roles.
- Stable bulk-transfer behavior matters more than peak throughput.
- Pairing should be avoided if possible; FIPS/Noise remains the true security layer.
- The work should prepare for eventual mac↔mac viability, not just Linux↔macOS.

**Research Findings**:
- TEST A/B/C currently pass: role symmetry, authentication, tie-break, and reconnect are working.
- TEST D/E still stall under sustained iperf load.
- Dynamic PSM discovery over GATT is already working on macOS.
- Existing evidence points to writer-side backpressure / credit blindness, especially on macOS/CoreBluetooth paths.
- FIPS already contains `SendRateLimiter` and `BleRateAdapter` AIMD, but the bulk path remains unstable.

### Metis Review
**Identified Gaps** (addressed):
- Acceptance needed a concrete target → resolved: stability-first, modest throughput acceptable, ping under load under ~1s.
- mac↔mac implications needed explicit inclusion → resolved: writer/backpressure work must be general enough for future mac↔mac validation.
- Plan needed guardrails against over-refactoring → resolved below.

---

## Work Objectives

### Core Objective
Make BLE bulk transfer stable enough that Linux and macOS can both act as central or peripheral, complete iperf in both directions without stalls/resets, and keep ping latency under load acceptable.

### Concrete Deliverables
- Hardened macOS BLE writer/backpressure implementation in `src/transport/ble/io_macos.rs`
- Any required companion BLE pacing/config updates in `src/transport/ble/mod.rs`, `src/transport/ble/io.rs`, and `src/config/transport.rs`
- Updated hardware evidence from `testing/ble/run-ble-test-campaign.sh` for TEST A-E
- Clear guidance on whether the stabilized design is suitable for future mac↔mac validation

### Definition of Done
- [ ] TEST A/B/C pass on current branch with updated binaries
- [ ] TEST D completes both directions without stall/reset
- [ ] TEST E completes both isolated directions without stall/reset
- [ ] iperf shows non-zero end summaries in both directions
- [ ] ping under iperf stays below ~1s in the stability-first profile

### Must Have
- Linux and macOS can both play both BLE roles
- Stable writer/backpressure behavior under sustained transfer
- Conservative or adaptive pacing if required for stability
- Full hardware evidence from the campaign artifacts

### Must NOT Have (Guardrails)
- Do not change the 2-byte BE framing format
- Do not move framing logic into `receive_loop`
- Do not require BLE pairing for the working path
- Do not fork or patch external `bluest` crate code directly unless absolutely unavoidable
- Do not perform unrelated BLE architecture rewrites

---

## Verification Strategy

### Test Decision
- **Infrastructure exists**: YES
- **Automated tests**: Tests-after
- **Framework**: cargo test + hardware campaign shell harness

### QA Policy
Every task must include agent-executed QA. Hardware BLE validation is mandatory for bulk-transfer tasks.

- **Frontend/UI**: N/A
- **TUI/CLI**: `bash` / `interactive_bash` where required
- **API/Backend**: `bash` / `curl` / iperf / ping
- **Library/Module**: cargo build/test and direct binary execution

Evidence saved under `.sisyphus/evidence/` or `/tmp/fips-ble-campaign/` as appropriate.

---

## Execution Strategy

### Parallel Execution Waves

Wave 1 (foundation diagnosis + bounded writer plan):
├── Task 1: Consolidate current BLE writer failure evidence [deep]
├── Task 2: Define stable writer/backpressure contract for macOS central + peripheral [deep]
├── Task 3: Audit existing pacing/config defaults against stability target [quick]
└── Task 4: Prepare focused bulk-transfer validation profile and evidence checklist [quick]

Wave 2 (implementation — core writer hardening):
├── Task 5: Harden macOS central writer path [deep]
├── Task 6: Harden macOS peripheral writer path [deep]
├── Task 7: Bound queue/backpressure behavior and timeout policy [deep]
└── Task 8: Tune pacing defaults for stability-first BLE profile [quick]

Wave 3 (focused validation + iterative fixes):
├── Task 9: Run focused directional bulk tests and inspect artifacts [unspecified-high]
├── Task 10: Fix any remaining directional stall discovered in focused tests [deep]
└── Task 11: Validate ping-under-load behavior with stability profile [unspecified-high]

Wave 4 (full campaign + mac↔mac readiness assessment):
├── Task 12: Run full TEST A-E campaign on updated baseline [unspecified-high]
├── Task 13: Summarize whether writer/backpressure model is mac↔mac-ready [deep]
└── Task 14: Update docs/issues/evidence summary with validated results [writing]

Wave FINAL:
├── Task F1: Plan compliance audit (oracle)
├── Task F2: Code quality review (unspecified-high)
├── Task F3: Real manual QA / evidence audit (unspecified-high)
└── Task F4: Scope fidelity check (deep)

Critical Path: 1 → 2 → 5/6/7 → 9 → 10 → 12 → F1-F4

### Dependency Matrix
- **1**: none → 2, 5, 6, 7
- **2**: 1 → 5, 6, 7
- **3**: none → 8, 11
- **4**: none → 9, 12
- **5**: 1, 2 → 9, 12, 13
- **6**: 1, 2 → 9, 12, 13
- **7**: 1, 2 → 9, 10, 12
- **8**: 3 → 9, 11, 12
- **9**: 4, 5, 6, 7, 8 → 10, 11
- **10**: 7, 9 → 12, 13
- **11**: 3, 8, 9 → 12, 13
- **12**: 4, 5, 6, 7, 8, 10, 11 → 14, F1-F4
- **13**: 5, 6, 10, 11 → 14, F4
- **14**: 12, 13 → F1-F4

### Agent Dispatch Summary
- **Wave 1**: T1-T2 → `deep`, T3-T4 → `quick`
- **Wave 2**: T5-T7 → `deep`, T8 → `quick`
- **Wave 3**: T9/T11 → `unspecified-high`, T10 → `deep`
- **Wave 4**: T12 → `unspecified-high`, T13 → `deep`, T14 → `writing`
- **Final**: F1 → `oracle`, F2/F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [x] 1. Consolidate BLE writer failure evidence

  **What to do**:
  - Re-read current TEST D/E artifacts and logs to isolate exactly where sustained transfer stops.
  - Map the failure signatures to central vs peripheral writer paths.
  - Produce a short implementation note naming the primary and secondary suspects.

  **Must NOT do**:
  - Do not change code yet.
  - Do not reopen already-closed hypotheses like pairing or dynamic PSM discovery.

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: root-cause synthesis from existing code and artifacts
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1
  - **Blocks**: 2, 5, 6, 7
  - **Blocked By**: None

  **References**:
  - `src/transport/ble/io_macos.rs` - current macOS central/peripheral send paths
  - `src/transport/ble/io.rs` - Linux send path and timeout behavior
  - `/tmp/fips-ble-campaign/testD-*` - full mixed-direction iperf evidence
  - `/tmp/fips-ble-campaign/testE-*` - isolated directional iperf evidence
  - `.sisyphus/drafts/ble-bulk-stability.md` - current planning context and findings

  **Acceptance Criteria**:
  - [ ] Primary suspect list exists with central/peripheral path attribution
  - [ ] No new hypotheses added without artifact support

  **QA Scenarios**:
  ```
  Scenario: Artifact-based diagnosis
    Tool: Bash
    Preconditions: /tmp/fips-ble-campaign contains latest testD/testE outputs
    Steps:
      1. Read testD/testE iperf outputs and logs
      2. Identify whether stalls correlate with macOS central, macOS peripheral, or Linux path
      3. Save summary to .sisyphus/evidence/task-1-writer-diagnosis.txt
    Expected Result: Clear attribution of bulk stall to specific writer/backpressure paths
    Evidence: .sisyphus/evidence/task-1-writer-diagnosis.txt

  Scenario: Negative - reject unsupported causes
    Tool: Bash
    Preconditions: Same artifacts available
    Steps:
      1. Search logs for pairing/auth/PSM negotiation failures during D/E
      2. Confirm those are not the dominant current failure signature
    Expected Result: Evidence shows role/pairing/PSM are not the main current blocker
    Evidence: .sisyphus/evidence/task-1-negative-cause-check.txt
  ```

- [x] 2. Define stable writer/backpressure contract for macOS central + peripheral

  **What to do**:
  - Specify the exact writer semantics both macOS roles must follow:
    - bounded queue
    - partial-write retention
    - writable-event driven draining or equivalent
    - explicit backpressure when queue is full
  - Choose the smallest viable implementation shape that preserves existing architecture.

  **Must NOT do**:
  - Do not invent a new BLE protocol.
  - Do not fork `bluest` unless absolutely unavoidable.

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: architecture-level stabilization choice with minimal surface area
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1
  - **Blocks**: 5, 6, 7
  - **Blocked By**: 1

  **References**:
  - `src/transport/ble/io_macos.rs: BluestStream, PeripheralStream` - local writer wrappers
  - `~/.cargo/registry/.../bluest-0.6.9/src/corebluetooth/l2cap_channel.rs` - upstream writer behavior
  - `src/transport/ble/rate_limit.rs` - pacing primitives already available
  - External research in draft context - established credit/backpressure patterns

  **Acceptance Criteria**:
  - [ ] One chosen writer contract documented and aligned to established patterns
  - [ ] Contract covers both central and peripheral send paths

  **QA Scenarios**:
  ```
  Scenario: Design contract completeness
    Tool: Bash
    Preconditions: Task 1 diagnosis complete
    Steps:
      1. Enumerate required writer invariants
      2. Check both central and peripheral paths are covered
      3. Save contract to .sisyphus/evidence/task-2-writer-contract.txt
    Expected Result: A concrete writer/backpressure contract exists for implementation
    Evidence: .sisyphus/evidence/task-2-writer-contract.txt

  Scenario: Negative - avoid scope creep
    Tool: Bash
    Preconditions: Same contract available
    Steps:
      1. Verify contract does not require framing, pairing, or protocol redesign
      2. Record excluded scope explicitly
    Expected Result: Contract remains bounded to writer/backpressure stabilization
    Evidence: .sisyphus/evidence/task-2-scope-check.txt
  ```

- [x] 3. Audit pacing/config defaults against stability target

  **What to do**:
  - Confirm how `send_rate_bps`, `effective_send_rate_bps()`, burst size, and receive timeout interact with bulk transfer.
  - Choose a stability-first starting profile for BLE bulk transfer.

  **Must NOT do**:
  - Do not tune for maximum throughput first.

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: targeted configuration audit with bounded scope
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1
  - **Blocks**: 8, 11
  - **Blocked By**: None

  **References**:
  - `src/config/transport.rs` - config accessors and effective rate mapping
  - `src/transport/ble/rate_limit.rs` - token bucket + AIMD defaults
  - `testing/ble/configs/*.yaml` - current test campaign values

  **Acceptance Criteria**:
  - [ ] Stability-first BLE pacing profile documented
  - [ ] Candidate rate/burst/timeout values identified for validation

  **QA Scenarios**:
  ```
  Scenario: Config audit
    Tool: Bash
    Preconditions: Source tree available
    Steps:
      1. Read transport config defaults and BLE test configs
      2. Record effective defaults and campaign overrides
      3. Save summary to .sisyphus/evidence/task-3-config-audit.txt
    Expected Result: Clear stability-first pacing profile proposal exists
    Evidence: .sisyphus/evidence/task-3-config-audit.txt

  Scenario: Negative - reject unlimited blast mode
    Tool: Bash
    Preconditions: Same config summary available
    Steps:
      1. Confirm no stability-first profile depends on unlimited send bursts
      2. Record why unlimited mode is not acceptable for the first stable target
    Expected Result: Stability-first profile explicitly avoids uncontrolled burst mode
    Evidence: .sisyphus/evidence/task-3-negative-blast-mode.txt
  ```

- [x] 4. Prepare focused bulk-transfer validation profile and evidence checklist

  **What to do**:
  - Define which focused tests should run before the full campaign (at minimum isolated directional iperf and ping-under-load).
  - Define the exact artifacts to inspect after each run.

  **Must NOT do**:
  - Do not skip artifact inspection between implementation rounds.

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: operational test checklist preparation
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1
  - **Blocks**: 9, 12
  - **Blocked By**: None

  **References**:
  - `testing/ble/run-ble-test-campaign.sh` - hardware campaign harness
  - `/tmp/fips-ble-campaign/` - expected artifact locations

  **Acceptance Criteria**:
  - [ ] Focused test order and evidence checklist documented
  - [ ] Full campaign artifact set identified

  **QA Scenarios**:
  ```
  Scenario: Focused validation checklist
    Tool: Bash
    Preconditions: Harness script exists
    Steps:
      1. Enumerate focused test sequence before full campaign
      2. Enumerate required artifacts per step
      3. Save checklist to .sisyphus/evidence/task-4-validation-checklist.txt
    Expected Result: Repeatable focused validation checklist exists
    Evidence: .sisyphus/evidence/task-4-validation-checklist.txt

  Scenario: Negative - artifact completeness
    Tool: Bash
    Preconditions: Same checklist available
    Steps:
      1. Verify checklist includes both iperf outputs and peer/link logs
      2. Verify checklist includes ping-under-load artifact requirement
    Expected Result: No focused run can be marked complete without sufficient evidence
    Evidence: .sisyphus/evidence/task-4-artifact-completeness.txt
  ```

- [x] 5. Harden macOS central writer path

  **What to do**:
  - Replace the fragile one-shot `bluest` send semantics at the integration layer with bounded, backpressure-aware behavior.
  - Ensure partial progress is preserved and queue saturation is handled deliberately.
  - Keep the fix compatible with future mac↔mac use.

  **Must NOT do**:
  - Do not change BLE framing.
  - Do not patch external crate code unless local integration-layer hardening is proven insufficient.

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: non-trivial transport hardening with behavior-sensitive code
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: 9, 12, 13
  - **Blocked By**: 1, 2

  **References**:
  - `src/transport/ble/io_macos.rs: BluestStream` - current central send wrapper
  - `~/.cargo/registry/.../bluest-0.6.9/src/corebluetooth/l2cap_channel.rs` - upstream central writer behavior
  - `src/transport/ble/rate_limit.rs` - pacing helper

  **Acceptance Criteria**:
  - [ ] Central send path preserves forward progress under backpressure
  - [ ] Queue saturation/backpressure does not immediately tear down connection
  - [ ] `cargo build --release --features ble-macos` succeeds

  **QA Scenarios**:
  ```
  Scenario: Central writer bounded-progress behavior
    Tool: Bash
    Preconditions: Updated macOS binary built
    Steps:
      1. Run focused macOS-central bulk transfer test
      2. Confirm connection stays up under sustained send pressure
      3. Save key log lines to .sisyphus/evidence/task-5-central-writer.txt
    Expected Result: Central path no longer fails immediately under bulk pressure
    Evidence: .sisyphus/evidence/task-5-central-writer.txt

  Scenario: Negative - no spurious immediate timeout
    Tool: Bash
    Preconditions: Same test run
    Steps:
      1. Search logs for early writer timeout/reset caused by central send path
      2. Confirm absence or significant reduction
    Expected Result: No early central-path timeout/reset on the focused run
    Evidence: .sisyphus/evidence/task-5-central-negative.txt
  ```

- [x] 6. Harden macOS peripheral writer path

  **What to do**:
  - Make the peripheral writer treat `write_maxLength == 0` as backpressure, not fatal error.
  - Ensure the path uses bounded queueing or resumable partial-write handling instead of brute-force blocking.
  - Add a sane timeout for the whole write operation.

  **Must NOT do**:
  - Do not leave `res == 0` as a fatal transport error.
  - Do not create an unbounded queue.

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: most likely bulk-stall fix with concurrency/backpressure implications
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: 9, 12, 13
  - **Blocked By**: 1, 2

  **References**:
  - `src/transport/ble/io_macos.rs: PeripheralStream::send(), PeripheralOutputDelegate` - current peripheral send logic
  - Existing `drain_to_stream()` implementation - partial-write/backpressure handling pattern already present locally

  **Acceptance Criteria**:
  - [ ] `write_maxLength == 0` no longer causes immediate transport failure
  - [ ] Peripheral writer has bounded backpressure-aware behavior
  - [ ] `cargo build --release --features ble-macos` succeeds

  **QA Scenarios**:
  ```
  Scenario: Peripheral backpressure handling
    Tool: Bash
    Preconditions: Updated macOS binary built
    Steps:
      1. Run focused Linux->macOS bulk transfer test
      2. Inspect logs for `write_maxLength` backpressure behavior
      3. Save summary to .sisyphus/evidence/task-6-peripheral-backpressure.txt
    Expected Result: Peripheral path waits/resumes under backpressure instead of failing immediately
    Evidence: .sisyphus/evidence/task-6-peripheral-backpressure.txt

  Scenario: Negative - no `res == 0` fatal path remains
    Tool: Bash
    Preconditions: Source tree available after implementation
    Steps:
      1. Search `src/transport/ble/io_macos.rs` for `res == 0`
      2. Verify no remaining path returns fatal error directly on that condition
    Expected Result: All zero-write cases are handled as backpressure or queued retry
    Evidence: .sisyphus/evidence/task-6-no-zero-write-fatal.txt
  ```

- [x] 7. Bound queue/backpressure behavior and timeout policy

  **What to do**:
  - Standardize BLE write timeout policy across Linux/macOS paths.
  - Enforce bounded queue depths and make saturation behavior explicit.
  - Ensure timeout/backpressure policy aligns with stability-first target.

  **Must NOT do**:
  - Do not silently allow indefinite buffering.
  - Do not make timeout values so aggressive that normal congestion kills the link.

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: cross-path policy tuning with failure-mode implications
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: 9, 10, 12
  - **Blocked By**: 1, 2

  **References**:
  - `src/transport/ble/io.rs` - Linux timeout/send behavior
  - `src/transport/ble/io_macos.rs` - macOS timeouts and queueing
  - `src/config/transport.rs` - transport config accessors

  **Acceptance Criteria**:
  - [ ] Timeout policy consistent across paths
  - [ ] Queue/backpressure bounds documented in code behavior
  - [ ] No path uses effectively unlimited buffering for bulk traffic

  **QA Scenarios**:
  ```
  Scenario: Timeout/backpressure policy audit
    Tool: Bash
    Preconditions: Implementation complete
    Steps:
      1. Read Linux/macOS BLE send paths
      2. Confirm write timeout values and queue bounds are consistent with the chosen policy
      3. Save audit to .sisyphus/evidence/task-7-timeout-policy.txt
    Expected Result: Timeouts and queue bounds are explicit and stability-first
    Evidence: .sisyphus/evidence/task-7-timeout-policy.txt

  Scenario: Negative - no indefinite writer wedge
    Tool: Bash
    Preconditions: Focused bulk test available
    Steps:
      1. Verify no send path blocks forever without timeout/backpressure outcome
      2. Record evidence from logs/build inspection
    Expected Result: Writer paths either progress, backpressure, or fail cleanly within policy
    Evidence: .sisyphus/evidence/task-7-no-indefinite-wedge.txt
  ```

- [x] 8. Tune pacing defaults for stability-first BLE profile

  **What to do**:
  - Set a conservative starting rate/burst profile suitable for stable iperf and sub-1s ping under load.
  - Preserve adaptive adjustment via MMP feedback where available.

  **Must NOT do**:
  - Do not optimize for peak throughput at the expense of stalls.

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: bounded tuning task building on existing pacing infrastructure
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: 9, 11, 12
  - **Blocked By**: 3

  **References**:
  - `src/transport/ble/rate_limit.rs` - current limiter and AIMD adapter
  - `testing/ble/configs/*.yaml` - current test profiles

  **Acceptance Criteria**:
  - [ ] Stability-first pacing profile chosen and applied
  - [ ] Profile favors no-stall bulk transfer over peak bandwidth

  **QA Scenarios**:
  ```
  Scenario: Pacing profile verification
    Tool: Bash
    Preconditions: Config/code changes applied
    Steps:
      1. Inspect effective BLE send rate and burst settings in code/config
      2. Save chosen profile to .sisyphus/evidence/task-8-pacing-profile.txt
    Expected Result: Conservative/adaptive pacing profile is clearly defined
    Evidence: .sisyphus/evidence/task-8-pacing-profile.txt

  Scenario: Negative - no unlimited stress profile used as default
    Tool: Bash
    Preconditions: Same profile available
    Steps:
      1. Verify stability profile does not default to unlimited send bursts
      2. Record rationale
    Expected Result: Stability-first profile avoids uncontrolled send behavior
    Evidence: .sisyphus/evidence/task-8-negative-unlimited.txt
  ```

- [ ] 9. Run focused directional bulk tests and inspect artifacts

  **What to do**:
  - Run isolated directional bulk-transfer tests first.
  - Inspect iperf, peer, link, and relevant log artifacts between each iteration.

  **Must NOT do**:
  - Do not jump straight to full campaign without focused results.

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: hands-on hardware validation and artifact-driven debugging
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential after writer changes
  - **Blocks**: 10, 11
  - **Blocked By**: 4, 5, 6, 7, 8

  **References**:
  - `testing/ble/run-ble-test-campaign.sh --test E`
  - `/tmp/fips-ble-campaign/testE-*`

  **Acceptance Criteria**:
  - [ ] Both isolated directions complete without stall/reset
  - [ ] iperf outputs contain valid end summaries

  **QA Scenarios**:
  ```
  Scenario: Isolated Linux->macOS bulk transfer
    Tool: Bash
    Preconditions: Updated binaries deployed
    Steps:
      1. Run `bash testing/ble/run-ble-test-campaign.sh --test E`
      2. Read `/tmp/fips-ble-campaign/testE-linux-to-mac-iperf`
      3. Verify end summary exists and transfer does not flatline after initial burst
    Expected Result: Linux->macOS isolated iperf completes with non-zero end summary
    Evidence: /tmp/fips-ble-campaign/testE-linux-to-mac-iperf

  Scenario: Isolated macOS->Linux bulk transfer
    Tool: Bash
    Preconditions: Same run available
    Steps:
      1. Read `/tmp/fips-ble-campaign/testE-mac-to-linux-iperf`
      2. Verify end summary exists and transfer does not stall after first burst
    Expected Result: macOS->Linux isolated iperf completes with non-zero end summary
    Evidence: /tmp/fips-ble-campaign/testE-mac-to-linux-iperf
  ```

- [ ] 10. Fix any remaining directional stall discovered in focused tests

  **What to do**:
  - Use focused artifacts to make one additional bounded fix if one direction still degrades.
  - Keep the fix within writer/backpressure/pacing scope.

  **Must NOT do**:
  - Do not broaden into unrelated transport layers unless artifacts force it.

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: iterative root-cause fix based on fresh hardware evidence
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential
  - **Blocks**: 12, 13
  - **Blocked By**: 7, 9

  **References**:
  - Fresh `/tmp/fips-ble-campaign/testE-*` artifacts
  - Updated writer/backpressure code from Tasks 5-8

  **Acceptance Criteria**:
  - [ ] Remaining directional stall is reduced or eliminated on rerun
  - [ ] No unrelated regressions introduced

  **QA Scenarios**:
  ```
  Scenario: Focused regression fix verification
    Tool: Bash
    Preconditions: Additional directional fix implemented
    Steps:
      1. Re-run focused directional test
      2. Compare pre/post artifacts for the previously failing direction
      3. Save comparison summary to .sisyphus/evidence/task-10-directional-fix.txt
    Expected Result: Previously failing direction shows measurable stability improvement
    Evidence: .sisyphus/evidence/task-10-directional-fix.txt

  Scenario: Negative - no new role regression
    Tool: Bash
    Preconditions: Same updated build available
    Steps:
      1. Re-check that connection/session establishment still works after the fix
      2. Record peer/link evidence
    Expected Result: Control-plane behavior remains intact
    Evidence: .sisyphus/evidence/task-10-no-regression.txt
  ```

- [ ] 11. Validate ping-under-load behavior with stability profile

  **What to do**:
  - Run ping concurrently with bulk transfer using the stability-first profile.
  - Confirm latency stays under the accepted threshold (~1s).

  **Must NOT do**:
  - Do not claim success on throughput alone.

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: concurrent hardware validation of stability/latency target
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential after focused transfer stability
  - **Blocks**: 12, 13
  - **Blocked By**: 3, 8, 9

  **References**:
  - `testing/ble/run-ble-test-campaign.sh`
  - System ping tools and campaign artifacts

  **Acceptance Criteria**:
  - [ ] Ping under iperf remains below ~1s
  - [ ] Latency evidence captured alongside transfer evidence

  **QA Scenarios**:
  ```
  Scenario: Ping under load
    Tool: Bash
    Preconditions: Stable focused iperf profile available
    Steps:
      1. Start a bulk transfer
      2. Run ping concurrently across the BLE path
      3. Capture latency output to .sisyphus/evidence/task-11-ping-under-load.txt
    Expected Result: Ping stays under ~1s for the stability-first profile
    Evidence: .sisyphus/evidence/task-11-ping-under-load.txt

  Scenario: Negative - no latency collapse during iperf
    Tool: Bash
    Preconditions: Same concurrent run available
    Steps:
      1. Inspect ping output for spikes above threshold or long no-response gaps
      2. Record findings
    Expected Result: No sustained latency collapse during the bulk run
    Evidence: .sisyphus/evidence/task-11-latency-negative.txt
  ```

- [ ] 12. Run full TEST A-E campaign on updated baseline

  **What to do**:
  - Rebuild/deploy updated binaries on both platforms.
  - Run full hardware campaign A-E.
  - Collect and inspect all expected artifacts.

  **Must NOT do**:
  - Do not skip A/B/C regression checks after bulk-path changes.

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: full hardware validation and evidence collection
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential after focused validation
  - **Blocks**: 14, F1-F4
  - **Blocked By**: 4, 5, 6, 7, 8, 10, 11

  **References**:
  - `testing/ble/run-ble-test-campaign.sh`
  - `/tmp/fips-ble-campaign/`

  **Acceptance Criteria**:
  - [ ] TEST A/B/C pass
  - [ ] TEST D completes both directions without stall/reset
  - [ ] TEST E completes both isolated directions without stall/reset

  **QA Scenarios**:
  ```
  Scenario: Full hardware campaign
    Tool: Bash
    Preconditions: Updated binaries built and deployable
    Steps:
      1. Run `bash testing/ble/run-ble-test-campaign.sh`
      2. Inspect A/B/C peer artifacts and D/E iperf artifacts
      3. Save campaign summary to .sisyphus/evidence/task-12-full-campaign-summary.txt
    Expected Result: Full campaign demonstrates stable roles and stable bulk transfer
    Evidence: .sisyphus/evidence/task-12-full-campaign-summary.txt

  Scenario: Negative - no regression in role symmetry
    Tool: Bash
    Preconditions: Same campaign available
    Steps:
      1. Verify TEST A and TEST B both establish authenticated peers
      2. Verify TEST C dual-role+tiebreak+reconnect still passes
    Expected Result: Role symmetry remains intact after bulk-path hardening
    Evidence: .sisyphus/evidence/task-12-role-regression-check.txt
  ```

- [ ] 13. Summarize whether writer/backpressure model is mac↔mac-ready

  **What to do**:
  - Evaluate whether the chosen writer/backpressure design generalizes to both mac roles sufficiently to support future mac↔mac testing.
  - Identify any remaining blockers specific to mac↔mac.

  **Must NOT do**:
  - Do not claim mac↔mac is proven unless it was actually tested.

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: architecture readiness assessment based on validated behavior
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: 14, F4
  - **Blocked By**: 5, 6, 10, 11

  **References**:
  - Updated writer/backpressure code
  - TEST A/B/C results
  - Focused and full campaign evidence

  **Acceptance Criteria**:
  - [ ] Explicit mac↔mac readiness assessment exists
  - [ ] Remaining unknowns are clearly named

  **QA Scenarios**:
  ```
  Scenario: mac↔mac readiness assessment
    Tool: Bash
    Preconditions: Updated evidence from focused/full campaign available
    Steps:
      1. Compare central and peripheral writer behavior after hardening
      2. Assess whether both are robust enough for future mac↔mac
      3. Save assessment to .sisyphus/evidence/task-13-mac-mac-readiness.txt
    Expected Result: Clear statement of mac↔mac readiness and remaining blockers
    Evidence: .sisyphus/evidence/task-13-mac-mac-readiness.txt

  Scenario: Negative - avoid overclaiming
    Tool: Bash
    Preconditions: Same assessment available
    Steps:
      1. Verify assessment distinguishes between “prepared for” and “proven by test”
      2. Record any untested assumptions
    Expected Result: mac↔mac claims remain evidence-bound
    Evidence: .sisyphus/evidence/task-13-no-overclaim.txt
  ```

- [ ] 14. Update docs/issues/evidence summary with validated results

  **What to do**:
  - Update the relevant markdown docs and Amperstrand issue comments with the validated state.
  - Record what is now proven and what remains open.

  **Must NOT do**:
  - Do not comment on upstream `jmcorgan/fips`.
  - Do not publish claims not backed by the latest campaign.

  **Recommended Agent Profile**:
  - **Category**: `writing`
    - Reason: evidence-based documentation and issue-summary updates
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: F1-F4
  - **Blocked By**: 12, 13

  **References**:
  - `docs/macos-ble-design.md`
  - Relevant Amperstrand/fips issues (#52, #63, others as appropriate)
  - Campaign artifacts and evidence files

  **Acceptance Criteria**:
  - [ ] Docs/issues reflect the latest validated state
  - [ ] Evidence-backed summary exists for the new baseline

  **QA Scenarios**:
  ```
  Scenario: Documentation correctness
    Tool: Bash
    Preconditions: Full campaign summary available
    Steps:
      1. Compare updated docs/issues text to latest campaign evidence
      2. Save verification summary to .sisyphus/evidence/task-14-docs-verification.txt
    Expected Result: Updated docs/issues match validated reality
    Evidence: .sisyphus/evidence/task-14-docs-verification.txt

  Scenario: Negative - no upstream issue updates
    Tool: Bash
    Preconditions: Planned issue updates identified
    Steps:
      1. Verify all GitHub references target `Amperstrand/fips`
      2. Record check result
    Expected Result: No upstream issue/comment scope violation
    Evidence: .sisyphus/evidence/task-14-upstream-scope-check.txt
  ```

---

## Final Verification Wave

- [ ] F1. **Plan Compliance Audit** — `oracle`
  Verify all planned writer/backpressure/pacing changes and campaign outcomes match the plan.

- [ ] F2. **Code Quality Review** — `unspecified-high`
  Run builds/tests and inspect changed BLE files for fragile or over-complex logic.

- [ ] F3. **Real Manual QA** — `unspecified-high`
  Re-run the exact QA scenarios and verify artifacts exist and support the claimed stability.

- [ ] F4. **Scope Fidelity Check** — `deep`
  Confirm the work stayed bounded to BLE writer/backpressure/pacing stabilization and did not creep into unrelated transport redesign.

---

## Commit Strategy

- **1**: `fix(ble): harden macOS writer backpressure handling`
- **2**: `fix(ble): unify timeout and queue policy for stable bulk transfer`
- **3**: `test(ble): validate stable bulk-transfer profile on hardware`

---

## Success Criteria

### Verification Commands
```bash
bash testing/ble/run-ble-test-campaign.sh --test A
bash testing/ble/run-ble-test-campaign.sh --test B
bash testing/ble/run-ble-test-campaign.sh --test C
bash testing/ble/run-ble-test-campaign.sh --test D
bash testing/ble/run-ble-test-campaign.sh --test E
```

### Final Checklist
- [ ] All Must Have items are present
- [ ] All Must NOT Have guardrails remain intact
- [ ] macOS and Linux both support both BLE roles
- [ ] iperf completes both ways without stalls/resets in the stability-first profile
- [ ] ping under load remains acceptable (<~1s)
