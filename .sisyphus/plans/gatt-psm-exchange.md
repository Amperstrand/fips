# GATT PSM Exchange — Enable macOS↔macOS and Linux→macOS BLE

## TL;DR

> **Quick Summary**: Implement GATT-based PSM discovery so macOS can act as a BLE peripheral with a dynamically-assigned L2CAP PSM, enabling macOS↔macOS and Linux→macOS connectivity. All data stays on L2CAP at full speed — GATT is only used to exchange the PSM number.
> 
> **Deliverables**:
> - macOS GATT peripheral: publishes L2CAP channel, advertises dynamic PSM via GATT characteristic
> - macOS GATT central: reads peer's GATT PSM characteristic before L2CAP connect
> - Linux GATT central: reads macOS peer's GATT PSM before L2CAP connect
> - PeerCapabilities GATT_SUPPORTED flag activated in negotiation
> - Connection decision logic updated to use GATT PSM discovery when needed
> - BLE capture + Python decrypter proof that it works
> - GitHub issue documenting design and evidence
> 
> **Estimated Effort**: Large
> **Parallel Execution**: YES — 3 waves
> **Critical Path**: Task 1 → Task 2 → Task 5 → Task 7 → Task 8 → Task 10 → Task 11

---

## Context

### Original Request
User asked to "enable GATT as an alternative to L2CAP" for macOS↔macOS BLE. After research, clarified this means GATT PSM exchange (not GATT data streaming) — all data stays on L2CAP at full speed (~1 Mbps). GATT is only used to discover macOS's dynamically-assigned L2CAP PSM.

### Interview Summary
**Key Discussions**:
- macOS cannot bind a fixed L2CAP PSM — CoreBluetooth dynamically assigns one via `publishL2CAPChannel`. The PSM must be communicated to peers via a GATT characteristic.
- GATT data streaming was rejected (10x slower than L2CAP, ~100 Kbps vs ~1 Mbps)
- Linux→macOS direction included for mesh resilience (bidirectional connection initiation) and testability (only one Mac available)
- Linux does NOT need GATT peripheral — it can bind a fixed PSM (0x0085)

**Research Findings**:
- `docs/macos-ble-design.md` already has the full GATT PSM exchange architecture designed
- **CRITICAL**: `bluest` has NO GATT server/peripheral support — macOS peripheral must use `objc2-core-bluetooth` directly (already available as transitive dependency v0.3.2)
- `bluest` DOES support GATT client (reading characteristics) — use for macOS central
- `bluer` supports GATT client — use for Linux central
- `PeerCapabilities` already has `GATT_SUPPORTED (0x40)` flag but it's inert
- Current macOS `listen()`, `start_advertising()`, and `BluestAcceptor` are all stubs

### Metis Review
**Identified Gaps** (addressed):
- bluest cannot do GATT server → use objc2-core-bluetooth directly for macOS peripheral
- Scanner cannot differentiate macOS (GATT PSM) vs Linux (fixed PSM) peers → use GATT_SUPPORTED capability or distinct GATT service UUID
- Capability exchange happens AFTER L2CAP connect (chicken-and-egg for PSM discovery) → GATT PSM read happens during scan/connect, before L2CAP
- PSM staleness risk → always re-read PSM before connecting, never cache
- Cross-probe tie-breaker race window widens with GATT latency → existing tie-breaker still applies, just takes longer

---

## Work Objectives

### Core Objective
Enable macOS to act as a BLE peripheral so that other nodes (macOS or Linux) can discover its dynamically-assigned L2CAP PSM via GATT and establish full-speed L2CAP connections.

### Concrete Deliverables
- `src/transport/ble/io_macos.rs` — Real `listen()`, `start_advertising()`, `BluestAcceptor` using CBPeripheralManager
- `src/transport/ble/io_macos.rs` — GATT PSM characteristic read in `connect()` for macOS↔macOS
- `src/transport/ble/io.rs` — GATT PSM characteristic read in `connect()` for Linux→macOS
- `src/transport/ble/mod.rs` — Updated PeerCapabilities defaults, connection decision logic
- Unit tests for capability flags, PSM encoding, and mock GATT scenarios
- BLE capture evidence proving the flow works
- GitHub issue with design documentation

### Definition of Done
- [ ] `cargo build --release --features ble-macos` succeeds on macOS
- [ ] `cargo build --release --features ble` succeeds on Linux
- [ ] `cargo test` passes with all existing + new tests, zero regressions
- [ ] Linux can connect to macOS via GATT PSM discovery → L2CAP (verified by ping + BLE capture)
- [ ] macOS→Linux still works unchanged (regression check)
- [ ] Python decrypter decrypts traffic from GATT-PSM-discovered L2CAP connections

### Must Have
- GATT PSM discovery before L2CAP connect when connecting to a macOS peripheral
- Backwards compatibility with non-GATT peers (fall back to fixed PSM 0x0085)
- Backwards compatibility with 33-byte pubkey exchange (old peers without capability flags)
- macOS peripheral L2CAP publish + GATT service advertising
- macOS acceptor that receives inbound L2CAP connections on the dynamic PSM

### Must NOT Have (Guardrails)
- No GATT data transport — all FMP/Noise data stays on L2CAP
- No GATT server abstraction layer — go direct to `objc2-core-bluetooth` on macOS
- No Linux GATT peripheral (Linux binds fixed PSM, doesn't need GATT advertising)
- No ESP32 GATT support
- No GATT PSM caching across sessions — always re-read before connecting
- No `async-trait` migration of BleIo
- No new config file options for GATT (use capability negotiation + existing config)
- No changes to Noise/FMP wire format
- No over-abstraction of the peripheral manager (keep it in io_macos.rs)
- No feature-flagged "GATT-only fallback mode"

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES (cargo test, MockBleIo)
- **Automated tests**: YES (Tests-after — capability logic is testable, GATT integration requires hardware)
- **Framework**: `cargo test` (built-in Rust test framework)

### QA Policy
Every task MUST include agent-executed QA scenarios.
Evidence saved to `.sisyphus/evidence/task-{N}-{scenario-slug}.{ext}`.

- **Unit tests**: `cargo test` for capability negotiation, PSM encoding, mock GATT scenarios
- **Integration QA**: BLE capture + Python decrypter proof, ping verification, log analysis
- **Compilation gate**: Both `--features ble-macos` and `--features ble` must build

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Foundation — types, constants, capabilities):
├── Task 1: GATT PSM UUIDs + types [quick]
├── Task 2: PeerCapabilities GATT_SUPPORTED activation + tests [quick]
├── Task 3: GitHub issue creation [quick]
└── Task 4: Study objc2-core-bluetooth APIs [quick]

Wave 2 (Core implementation — all independent given Wave 1):
├── Task 5: macOS peripheral — CBPeripheralManager L2CAP publish + GATT service [deep]
├── Task 6: Linux central — bluer GATT PSM read [unspecified-high]
├── Task 7: macOS central — bluest GATT PSM read [unspecified-high]
└── Task 8: Connection decision logic — scan_probe_loop + connect_async [deep]

Wave 3 (Integration, testing, evidence):
├── Task 9: MockBleIo GATT extensions + unit tests [unspecified-high]
├── Task 10: Build + deploy + integration test [deep]
└── Task 11: BLE capture, decrypt, evidence, issue update [deep]

Wave FINAL (After ALL tasks — parallel reviews, then user okay):
├── Task F1: Plan compliance audit (oracle)
├── Task F2: Code quality review (unspecified-high)
├── Task F3: Real manual QA (unspecified-high)
└── Task F4: Scope fidelity check (deep)
-> Present results -> Get explicit user okay

Critical Path: Task 1 → Task 5 → Task 7 → Task 8 → Task 10 → Task 11 → F1-F4
Parallel Speedup: ~60% faster than sequential
Max Concurrent: 4 (Waves 1 & 2)
```

### Dependency Matrix

| Task | Depends On | Blocks | Wave |
|------|-----------|--------|------|
| 1 | — | 2, 5, 6, 7, 8, 9 | 1 |
| 2 | 1 | 8, 9 | 1 |
| 3 | — | 11 | 1 |
| 4 | — | 5 | 1 |
| 5 | 1, 4 | 8, 10 | 2 |
| 6 | 1 | 10 | 2 |
| 7 | 1 | 8, 10 | 2 |
| 8 | 1, 2, 5, 7 | 9, 10 | 2 |
| 9 | 2, 8 | — | 3 |
| 10 | 5, 6, 7, 8 | 11 | 3 |
| 11 | 3, 10 | — | 3 |

### Agent Dispatch Summary

- **Wave 1**: **4** — T1 → `quick`, T2 → `quick`, T3 → `quick`, T4 → `quick`
- **Wave 2**: **4** — T5 → `deep`, T6 → `unspecified-high`, T7 → `unspecified-high`, T8 → `deep`
- **Wave 3**: **3** — T9 → `unspecified-high`, T10 → `deep`, T11 → `deep`
- **FINAL**: **4** — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [x] 1. GATT PSM UUIDs and Type Definitions

  **What to do**:
  - Define `FIPS_GATT_PSM_SERVICE_UUID` — a new UUID distinct from `FIPS_SERVICE_UUID` (which is `9c90b790-2cc5-42c0-9f87-c9cc40648f4c`). Derive it deterministically (e.g., SHA-256 of "FIPS GATT PSM Exchange Service" with UUID v4 bits).
  - Define `FIPS_GATT_PSM_CHAR_UUID` — characteristic UUID for the PSM value. Same derivation approach.
  - Both UUIDs must be defined in BOTH `io.rs` (for Linux/mock, using `bluer::Uuid`) and `io_macos.rs` (for macOS, using `uuid::Uuid`). Keep them as module-level constants next to the existing `FIPS_SERVICE_UUID` constants.
  - Document the UUID values in a comment explaining their derivation.

  **Must NOT do**:
  - Do not use random UUIDs — derive deterministically so they're reproducible
  - Do not put UUIDs in a config file — they're protocol constants

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Small, self-contained task — two constants in two files
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - None needed — this is a constants-only change

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 2, 3, 4)
  - **Blocks**: Tasks 2, 5, 6, 7, 8, 9
  - **Blocked By**: None (can start immediately)

  **References**:

  **Pattern References**:
  - `src/transport/ble/io.rs:160-161` — `FIPS_SERVICE_UUID` definition for Linux (bluer::Uuid format)
  - `src/transport/ble/io_macos.rs:26-27` — `FIPS_SERVICE_UUID` definition for macOS (uuid::Uuid format)

  **API/Type References**:
  - `bluer::Uuid` — Linux UUID type (re-export of `uuid::Uuid`)
  - `uuid::Uuid::from_u128()` — how existing UUID is constructed from a 128-bit constant

  **External References**:
  - UUID v4 format: bits 48-51 = 0100 (version), bits 64-65 = 10 (variant)

  **WHY Each Reference Matters**:
  - The existing `FIPS_SERVICE_UUID` shows the exact pattern for defining BLE UUIDs on both platforms. The new GATT PSM UUIDs must follow the same pattern (const, from_u128, same crate types).

  **Acceptance Criteria**:
  - [ ] `FIPS_GATT_PSM_SERVICE_UUID` defined in `io.rs` and `io_macos.rs`
  - [ ] `FIPS_GATT_PSM_CHAR_UUID` defined in `io.rs` and `io_macos.rs`
  - [ ] Both UUIDs are distinct from `FIPS_SERVICE_UUID` and from each other
  - [ ] UUIDs have valid v4 format bits
  - [ ] `cargo build --features ble-macos` and `cargo build --features ble` succeed

  **QA Scenarios**:

  ```
  Scenario: UUID constants compile and are distinct
    Tool: Bash (cargo)
    Preconditions: Clean checkout on macOS
    Steps:
      1. cargo build --release --features ble-macos
      2. grep -n "FIPS_GATT_PSM" src/transport/ble/io_macos.rs — verify two UUIDs present
      3. grep -n "FIPS_GATT_PSM" src/transport/ble/io.rs — verify two UUIDs present (in bluer_impl and/or mock sections)
    Expected Result: Build succeeds, 2 UUID constants in each file, all distinct values
    Failure Indicators: Compilation error, missing constants, duplicate UUID values
    Evidence: .sisyphus/evidence/task-1-uuid-constants.txt
  ```

  **Commit**: YES (groups with Task 2)
  - Message: `feat(ble): add GATT PSM exchange UUIDs and activate GATT_SUPPORTED capability`
  - Files: `src/transport/ble/io.rs`, `src/transport/ble/io_macos.rs`, `src/transport/ble/mod.rs`
  - Pre-commit: `cargo test`

- [x] 2. PeerCapabilities GATT_SUPPORTED Activation + Tests

  **What to do**:
  - Update `PeerCapabilities::macos_default()` (line 845-847 in mod.rs) to include `GATT_SUPPORTED` flag. The new value should be: `L2CAP_SUPPORTED | CAN_CENTRAL | CAN_PERIPHERAL | GATT_SUPPORTED | PREFER_OUTBOUND`. Note: macOS now gets `CAN_PERIPHERAL` because it can accept inbound via GATT PSM exchange.
  - Consider whether `linux_default()` should also set `GATT_SUPPORTED` — Linux can READ GATT PSM (as central) but cannot SERVE it. The flag means "I support GATT PSM exchange" which could mean either role. Decision: YES, set it on Linux too — it signals that Linux knows how to do GATT PSM reads, so macOS peers know they can be discovered.
  - Update existing tests at line 1443-1452 that assert `!linux.supports_gatt()` and `!mac.supports_gatt()` — these must now assert `true`.
  - Add new test: `test_macos_default_can_accept_inbound` — verify `CAN_PERIPHERAL` is set.
  - Add new test: `test_gatt_supported_flag_encoding` — verify the byte encoding round-trips correctly with GATT flag.

  **Must NOT do**:
  - Do not change the `GATT_SUPPORTED` constant value (0x40) — it's already defined
  - Do not change backwards compatibility with 33-byte peers or `LEGACY_CENTRAL_ONLY`

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Small flag changes + test updates, all in one file
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 3, 4)
  - **Blocks**: Tasks 8, 9
  - **Blocked By**: Task 1 (needs UUIDs defined first for consistency, though technically independent)

  **References**:

  **Pattern References**:
  - `src/transport/ble/mod.rs:820-894` — Full PeerCapabilities struct with all flags, defaults, and query methods
  - `src/transport/ble/mod.rs:1430-1460` — Existing capability tests (`test_peer_capabilities_defaults_and_queries`)

  **WHY Each Reference Matters**:
  - Lines 820-894 show the complete flag layout and how `macos_default()` / `linux_default()` are constructed. The change is adding `GATT_SUPPORTED` to both and `CAN_PERIPHERAL` to macOS.
  - Lines 1430-1460 have the tests that currently assert `!supports_gatt()` — these must flip to `true`.

  **Acceptance Criteria**:
  - [ ] `PeerCapabilities::macos_default().supports_gatt()` returns `true`
  - [ ] `PeerCapabilities::macos_default().can_accept_inbound()` returns `true`
  - [ ] `PeerCapabilities::linux_default().supports_gatt()` returns `true`
  - [ ] `cargo test test_peer_capabilities` passes
  - [ ] Backwards compatibility: `PeerCapabilities::from_byte(0x01)` still maps to `central_only()`

  **QA Scenarios**:

  ```
  Scenario: Updated capability defaults pass all tests
    Tool: Bash (cargo test)
    Preconditions: Task 1 complete (UUIDs defined)
    Steps:
      1. cargo test test_peer_capabilities -- --nocapture
      2. Verify output shows all tests passing
    Expected Result: All capability tests pass including new GATT assertions
    Failure Indicators: Test failure on supports_gatt() or can_accept_inbound()
    Evidence: .sisyphus/evidence/task-2-capability-tests.txt

  Scenario: Backwards compatibility with legacy peers
    Tool: Bash (cargo test)
    Preconditions: None
    Steps:
      1. cargo test test_peer_capabilities -- --nocapture
      2. Verify test covers: from_byte(0x01) maps to central_only, from_byte(0x00) maps to unrestricted
    Expected Result: Legacy peer handling unchanged
    Failure Indicators: from_byte(0x01) behavior changed
    Evidence: .sisyphus/evidence/task-2-legacy-compat.txt
  ```

  **Commit**: YES (groups with Task 1)
  - Message: `feat(ble): add GATT PSM exchange UUIDs and activate GATT_SUPPORTED capability`
  - Files: `src/transport/ble/mod.rs`
  - Pre-commit: `cargo test`

- [x] 3. GitHub Issue Creation — GATT PSM Exchange Design

  **What to do**:
  - Create a new GitHub issue on `Amperstrand/fips` titled "BLE: GATT PSM exchange for macOS↔macOS and Linux→macOS connectivity"
  - Document the design: why GATT PSM exchange (not GATT data), the flow (peripheral publishes L2CAP → GATT advertises PSM → central reads PSM → L2CAP connects), architecture diagram, UUID constants
  - Reference existing issues: #46 (capability negotiation), #50 (TCP death fix), #51 (ephemeral key dump)
  - Reference `docs/macos-ble-design.md` as the architectural basis
  - Include the interoperability matrix from the design doc (macOS→Linux ✅, macOS→macOS ✅, Linux→macOS ✅)
  - Note that `bluest` lacks GATT server support → using `objc2-core-bluetooth` directly
  - Include the "Must NOT Have" guardrails
  - Leave space for evidence (will be updated in Task 11)

  **Must NOT do**:
  - Do not include implementation code in the issue — it's a design document
  - Do not duplicate content already in `docs/macos-ble-design.md` — reference it

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Single `gh issue create` command with a well-defined body
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2, 4)
  - **Blocks**: Task 11
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `docs/macos-ble-design.md` — Full GATT PSM exchange architecture (reference, don't duplicate)
  - GitHub issue #46 — Existing capability negotiation issue (reference)
  - GitHub issue #50 — TCP death fix with evidence format (pattern for evidence section)

  **WHY Each Reference Matters**:
  - The design doc is the authoritative architecture source. The issue should explain "why now" and "what we're building" while referencing the doc for detailed design.
  - Issue #50 shows the pattern for documenting fixes with evidence (captures, log output, before/after).

  **Acceptance Criteria**:
  - [ ] GitHub issue created with `gh issue create`
  - [ ] Issue references #46 and `docs/macos-ble-design.md`
  - [ ] Issue includes interoperability matrix
  - [ ] Issue includes "Evidence" section (placeholder for Task 11)

  **QA Scenarios**:

  ```
  Scenario: Issue exists and is properly formatted
    Tool: Bash (gh)
    Preconditions: None
    Steps:
      1. gh issue list --repo Amperstrand/fips --search "GATT PSM exchange"
      2. Verify issue appears in list
      3. gh issue view <number> --repo Amperstrand/fips
      4. Verify body contains: "GATT PSM", interoperability matrix, references to #46
    Expected Result: Issue exists with complete design documentation
    Failure Indicators: Issue missing, body incomplete, missing references
    Evidence: .sisyphus/evidence/task-3-issue-created.txt
  ```

  **Commit**: NO (GitHub issue, not code)

- [x] 4. Study objc2-core-bluetooth APIs for Peripheral Role

  **What to do**:
  - This is a research task, not an implementation task. The executing agent must study the `objc2-core-bluetooth` crate APIs to understand exactly how to implement the macOS peripheral.
  - Find `CBPeripheralManager` bindings — how to instantiate, set delegate, handle state changes
  - Find `publishL2CAPChannel(withEncryption:)` — how to call it, how the PSM callback works
  - Find `CBMutableService`, `CBMutableCharacteristic` — how to create a GATT service with a read-only PSM characteristic
  - Find `add(_:)` (add service to peripheral manager) and `startAdvertising(_:)` APIs
  - Find `CBPeripheralManagerDelegate` — what callbacks are needed (didPublishL2CAPChannel, didReceiveRead, didOpen)
  - Check how bluest's existing `L2capChannel` works internally (it wraps CBL2CAPChannel) — the peripheral acceptor will receive the same type
  - Output: a research summary in `.sisyphus/evidence/task-4-objc2-api-study.md` documenting the API surface, code snippets, and gotchas

  **Must NOT do**:
  - Do not write implementation code — this is research only
  - Do not modify any source files

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Research task — read crate docs and source, write summary
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2, 3)
  - **Blocks**: Task 5
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `bluest` source code (Amperstrand/bluest.git) — how it wraps CoreBluetooth for L2CAP central role
  - `objc2-core-bluetooth` v0.3.2 — available as transitive dependency of bluest

  **External References**:
  - Apple CoreBluetooth docs: `CBPeripheralManager`, `CBL2CAPChannel`, `publishL2CAPChannel(withEncryption:)`
  - objc2-core-bluetooth crate docs on docs.rs

  **WHY Each Reference Matters**:
  - bluest shows the objc2 calling convention used in this project (how to create Objective-C objects, call methods, handle delegates). The peripheral implementation must follow the same patterns.
  - The Apple docs define the exact sequence: instantiate CBPeripheralManager → wait for poweredOn → publishL2CAPChannel → handle delegate callback with PSM → create GATT service → add to manager → startAdvertising.

  **Acceptance Criteria**:
  - [ ] Research summary saved to `.sisyphus/evidence/task-4-objc2-api-study.md`
  - [ ] Summary covers: CBPeripheralManager instantiation, delegate setup, L2CAP publish, GATT service creation, advertising
  - [ ] Summary includes code snippets showing objc2 calling patterns
  - [ ] Summary identifies potential gotchas (run loop requirements, thread safety, retain cycles)

  **QA Scenarios**:

  ```
  Scenario: Research summary is complete and actionable
    Tool: Bash (cat)
    Preconditions: None
    Steps:
      1. Verify .sisyphus/evidence/task-4-objc2-api-study.md exists
      2. Verify it contains sections: CBPeripheralManager, L2CAP Publish, GATT Service, Delegate, Gotchas
      3. Verify code snippets are present (not just prose)
    Expected Result: Complete research document with API surface mapped
    Failure Indicators: Missing sections, no code snippets, vague descriptions
    Evidence: .sisyphus/evidence/task-4-objc2-api-study.md (the task output IS the evidence)
  ```

  **Commit**: NO (research output, not code)

- [x] 5. macOS Peripheral — CBPeripheralManager L2CAP Publish + GATT PSM Service

  **What to do**:
  - This is the largest and most complex task. Implement the macOS BLE peripheral using `objc2-core-bluetooth` directly (NOT bluest — it has no GATT server support).
  - **Replace the stub `listen()`** in `BluestIo` (line 260-264 of `io_macos.rs`) with a real implementation that:
    1. Creates a `CBPeripheralManager` with a delegate
    2. Waits for `peripheralManagerDidUpdateState:` → `.poweredOn`
    3. Calls `publishL2CAPChannel(withEncryption: false)` (FIPS handles its own encryption via Noise)
    4. Receives the dynamic PSM via `peripheralManager:didPublishL2CAPChannel:error:` delegate callback
    5. Creates a `CBMutableService` with `FIPS_GATT_PSM_SERVICE_UUID`
    6. Creates a `CBMutableCharacteristic` with `FIPS_GATT_PSM_CHAR_UUID`, properties: `.read`, value: 2-byte LE-encoded PSM
    7. Adds the service to the peripheral manager
    8. Starts advertising with the FIPS service UUID
  - **Replace the stub `BluestAcceptor`** (line 163-172) with a real acceptor that:
    1. Waits for `peripheralManager:didOpenL2CAPChannel:error:` delegate callbacks
    2. Wraps received `CBL2CAPChannel` into `BluestStream` (same stream type used by outbound connections)
    3. Returns the stream from `accept()`
  - **Replace the stub `start_advertising()`** (line 315-318) — it should be wired to the peripheral manager's advertising state
  - **Replace the stub `stop_advertising()`** (line 321-323) — call `stopAdvertising()` on the peripheral manager
  - The peripheral manager and its delegate must be stored in `BluestIo` (or a new sub-struct). Use `Arc<Mutex<...>>` or `Arc<RwLock<...>>` for shared state between delegate callbacks and the async methods.
  - CoreBluetooth delegate callbacks happen on the main thread (CFRunLoop). The existing `io_macos.rs` header (line 207-210) notes that the main thread must run CFRunLoopRun() — this is already handled by the fips binary.
  - Use `tokio::sync::mpsc` or `tokio::sync::watch` channels to bridge CoreBluetooth delegate callbacks (sync, main thread) to async Rust (tokio runtime).

  **Must NOT do**:
  - Do not use bluest for any peripheral/server functionality — it doesn't support it
  - Do not create a separate module — keep all macOS BLE I/O in `io_macos.rs`
  - Do not cache the PSM — if peripheral restarts, a new PSM is assigned
  - Do not enable encryption on the L2CAP channel (FIPS uses Noise IK, not BLE encryption)
  - Do not create an abstraction layer over CBPeripheralManager — direct objc2 calls

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Complex Objective-C interop with CoreBluetooth, delegate pattern, async bridging, unsafe code
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - No BLE-specific skills available

  **Parallelization**:
  - **Can Run In Parallel**: YES (with Tasks 6, 7)
  - **Parallel Group**: Wave 2
  - **Blocks**: Tasks 8, 10
  - **Blocked By**: Tasks 1, 4

  **References**:

  **Pattern References**:
  - `src/transport/ble/io_macos.rs:42-153` — `BluestStream` implementation (the peripheral acceptor must produce the same stream type)
  - `src/transport/ble/io_macos.rs:155-172` — Current stub `BluestAcceptor` (replace this)
  - `src/transport/ble/io_macos.rs:255-323` — Current stub `listen()`, `start_advertising()`, `stop_advertising()` (replace these)
  - `src/transport/ble/io_macos.rs:267-312` — `connect()` method showing how `BluestStream` is constructed from L2CAP channel

  **API/Type References**:
  - `BluestStream` struct (line 42) — the acceptor must produce instances of this type
  - `bluest::L2capChannelReader` / `L2capChannelWriter` — BluestStream wraps these
  - `bluest::L2capChannel::split()` — how outbound connections create reader/writer (peripheral must do the same from CBL2CAPChannel)

  **External References**:
  - `.sisyphus/evidence/task-4-objc2-api-study.md` — Research output from Task 4 with exact API surface
  - Apple docs: CBPeripheralManager, CBPeripheralManagerDelegate, publishL2CAPChannel(withEncryption:)
  - `docs/macos-ble-design.md:88-105` — Architectural design for peripheral side GATT PSM exchange

  **WHY Each Reference Matters**:
  - The existing `BluestStream` is the return type of `accept()` — the peripheral must produce compatible streams. Study how `connect()` creates a BluestStream from an L2capChannel to replicate the pattern.
  - The design doc (lines 88-105) describes the exact sequence: publish L2CAP → create GATT service → add PSM characteristic → advertise.
  - Task 4's research output will have the objc2 calling patterns and gotchas.

  **Acceptance Criteria**:
  - [ ] `listen()` returns a real `BluestAcceptor` that can accept inbound L2CAP connections
  - [ ] `start_advertising()` starts GATT service advertising with PSM characteristic
  - [ ] `stop_advertising()` stops advertising
  - [ ] The GATT PSM characteristic is readable and contains the correct 2-byte LE PSM
  - [ ] `cargo build --release --features ble-macos` succeeds
  - [ ] No `unsafe` blocks without clear safety comments

  **QA Scenarios**:

  ```
  Scenario: macOS FIPS starts in peripheral mode and advertises GATT PSM service
    Tool: Bash (build + log check)
    Preconditions: macOS, Task 1 complete (UUIDs defined)
    Steps:
      1. cargo build --release --features ble-macos
      2. Update macOS config: set advertise: true, accept_connections: true
      3. Start FIPS in tmux: sudo ./target/release/fips --config /tmp/fips-test-macos/config.yaml 2>&1 | tee /tmp/fips-ble-test.log
      4. Wait 10 seconds
      5. grep "L2CAP.*PSM\|GATT.*service\|advertising" /tmp/fips-ble-test.log
    Expected Result: Log shows L2CAP channel published with dynamic PSM, GATT service added, advertising started
    Failure Indicators: "not supported" messages, crash, no PSM in logs
    Evidence: .sisyphus/evidence/task-5-peripheral-startup.txt

  Scenario: Peripheral handles missing Bluetooth gracefully
    Tool: Bash (log check)
    Preconditions: None
    Steps:
      1. If Bluetooth is disabled, start FIPS and check logs
      2. Verify error message mentions Bluetooth state, not a crash
    Expected Result: Graceful error: "Bluetooth not available" or similar
    Failure Indicators: Panic, segfault, no error message
    Evidence: .sisyphus/evidence/task-5-no-bluetooth-error.txt
  ```

  **Commit**: YES (groups with Tasks 6, 7, 8)
  - Message: `feat(ble): implement GATT PSM discovery for macOS peripheral and cross-platform central`
  - Files: `src/transport/ble/io_macos.rs`
  - Pre-commit: `cargo build --features ble-macos`

- [x] 6. Linux Central — bluer GATT PSM Read

  **What to do**:
  - Add GATT PSM read capability to `BluerIo::connect()` (in `io.rs`, `bluer_impl` module).
  - When connecting to a peer that requires GATT PSM discovery (determined by caller or by attempting GATT read first), the flow is:
    1. Connect to the peripheral at GATT level: `device.connect().await`
    2. Discover GATT services: find `FIPS_GATT_PSM_SERVICE_UUID`
    3. Read the PSM characteristic (`FIPS_GATT_PSM_CHAR_UUID`): parse 2-byte LE value
    4. Disconnect GATT (the L2CAP channel is independent)
    5. Open L2CAP channel with the discovered PSM
  - Add a new method to `BluerIo`: `async fn discover_gatt_psm(&self, addr: &BleAddr) -> Result<u16, TransportError>`
  - The existing `connect()` method takes a `psm: u16` parameter. The GATT PSM discovery should be called by the connection decision logic (Task 8) BEFORE calling `connect()`, passing the discovered PSM. This keeps `connect()` simple and unchanged.
  - Alternatively, add an `connect_with_gatt_psm()` method that does discovery + connect in one step. Choose the cleaner approach.
  - Use `bluer`'s GATT client APIs: `Device::services()`, `Service::characteristics()`, `Characteristic::read().await`
  - Handle errors: service not found, characteristic not found, invalid PSM value, GATT timeout

  **Must NOT do**:
  - Do not add GATT server/peripheral to Linux — Linux uses fixed PSM
  - Do not cache discovered PSM — always re-read
  - Do not change the `BleIo` trait — add the method directly to `BluerIo`

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: GATT client implementation with bluer, needs careful error handling, but simpler than peripheral
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with Tasks 5, 7, 8)
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 10
  - **Blocked By**: Task 1

  **References**:

  **Pattern References**:
  - `src/transport/ble/io.rs:404-700` — `BluerIo` struct and full `BleIo` trait implementation for Linux
  - `src/transport/ble/io.rs:470-540` — `BluerIo::connect()` — current L2CAP-only connect flow
  - `src/transport/ble/io.rs:160-161` — `FIPS_SERVICE_UUID` constant (bluer::Uuid format)

  **API/Type References**:
  - `bluer::Device` — has `services()`, `connect()`, `disconnect()` methods
  - `bluer::gatt::remote::Service` — GATT service with `characteristics()`
  - `bluer::gatt::remote::Characteristic` — has `read().await` for reading values

  **External References**:
  - bluer GATT example: https://github.com/bluez/bluer/tree/master/bluer/examples — look for `gatt_client.rs`

  **WHY Each Reference Matters**:
  - The existing `BluerIo::connect()` shows the error handling pattern, device lookup, and L2CAP channel opening. The GATT PSM read must integrate cleanly alongside it.
  - bluer's GATT client examples show the exact API for service discovery and characteristic reads.

  **Acceptance Criteria**:
  - [ ] `BluerIo::discover_gatt_psm()` (or equivalent) implemented
  - [ ] Handles: service not found → error, characteristic not found → error, invalid PSM → error
  - [ ] Returns valid u16 PSM on success
  - [ ] `cargo build --features ble` succeeds on Linux
  - [ ] No panics on GATT discovery failure — graceful error propagation

  **QA Scenarios**:

  ```
  Scenario: Linux discovers macOS GATT PSM and connects L2CAP
    Tool: Bash (SSH to Linux + logs)
    Preconditions: macOS running as peripheral (Task 5 complete), Linux has updated binary
    Steps:
      1. SSH to 192.168.13.218
      2. sudo systemctl restart fips
      3. Wait 60 seconds for BLE scan + GATT discovery
      4. sudo journalctl -u fips --since "2 minutes ago" | grep -i "gatt\|psm\|discovered"
    Expected Result: Log shows "GATT PSM discovered: <psm>" followed by L2CAP connect with that PSM
    Failure Indicators: "service not found", "characteristic not found", connection timeout
    Evidence: .sisyphus/evidence/task-6-linux-gatt-discovery.txt

  Scenario: Linux falls back to fixed PSM for non-GATT peers
    Tool: Bash (logs)
    Preconditions: macOS running as central (NOT peripheral — current mode)
    Steps:
      1. Verify macOS→Linux still connects with fixed PSM 0x0085
      2. Check Linux logs don't show GATT discovery attempts for Linux→macOS when macOS is central-only
    Expected Result: No regression — existing macOS→Linux flow unchanged
    Failure Indicators: GATT discovery attempted when peer doesn't support it
    Evidence: .sisyphus/evidence/task-6-fallback-fixed-psm.txt
  ```

  **Commit**: YES (groups with Tasks 5, 7, 8)
  - Message: `feat(ble): implement GATT PSM discovery for macOS peripheral and cross-platform central`
  - Files: `src/transport/ble/io.rs`
  - Pre-commit: `cargo build --features ble`

- [x] 7. macOS Central — bluest GATT PSM Read

  **What to do**:
  - Add GATT PSM read capability to `BluestIo` (in `io_macos.rs`) for macOS↔macOS connections.
  - Similar to Task 6 but using `bluest`'s GATT client APIs instead of `bluer`.
  - Add a new method: `async fn discover_gatt_psm(&self, addr: &BleAddr) -> Result<u16, TransportError>`
  - The flow:
    1. Look up `Device` from the devices cache (same as `connect()` does at line 268-277)
    2. Connect to device at GATT level: `adapter.connect_device(&device).await` (already done in `connect()`)
    3. Discover services: `device.discover_services().await` → find `FIPS_GATT_PSM_SERVICE_UUID`
    4. Discover characteristics of the service → find `FIPS_GATT_PSM_CHAR_UUID`
    5. Read the characteristic value → parse 2-byte LE PSM
    6. Return the PSM
  - Note: `connect()` already calls `adapter.connect_device()` (line 281-286) — GATT connection and L2CAP opening are separate. The GATT read happens between GATT connect and L2CAP open.
  - Consider refactoring `connect()` to optionally do GATT PSM read first, or keep `discover_gatt_psm()` separate and let the caller (Task 8) orchestrate.

  **Must NOT do**:
  - Do not change the `BleIo` trait
  - Do not cache PSM
  - Do not use objc2-core-bluetooth for this — bluest's GATT client works fine

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: GATT client with bluest, moderate complexity
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with Tasks 5, 6, 8)
  - **Parallel Group**: Wave 2
  - **Blocks**: Tasks 8, 10
  - **Blocked By**: Task 1

  **References**:

  **Pattern References**:
  - `src/transport/ble/io_macos.rs:267-312` — `BluestIo::connect()` — current flow showing device lookup, GATT connect, L2CAP open
  - `src/transport/ble/io_macos.rs:194-231` — `BluestIo` struct and constructor

  **API/Type References**:
  - `bluest::Device` — has `discover_services()`, GATT client methods
  - `bluest::Service` — GATT service with characteristics
  - `bluest::Characteristic` — has `read()` for reading values
  - `uuid::Uuid` — macOS UUID type used for service/characteristic matching

  **WHY Each Reference Matters**:
  - The existing `connect()` method shows exactly how device lookup and GATT-level connection work on macOS. The PSM discovery inserts between the GATT connect (line 281) and L2CAP open (line 290).

  **Acceptance Criteria**:
  - [ ] `BluestIo::discover_gatt_psm()` (or equivalent) implemented
  - [ ] Handles service/characteristic not found gracefully
  - [ ] Returns valid u16 PSM on success
  - [ ] `cargo build --release --features ble-macos` succeeds

  **QA Scenarios**:

  ```
  Scenario: macOS central reads GATT PSM from macOS peripheral
    Tool: Bash (build + log check)
    Preconditions: Another macOS node running as peripheral (or Linux emulating — tested via Task 10)
    Steps:
      1. cargo build --release --features ble-macos
      2. Start FIPS with scan enabled
      3. Wait for discovery + GATT PSM read
      4. grep "GATT PSM\|discover.*psm" /tmp/fips-ble-test.log
    Expected Result: Log shows successful GATT PSM read with PSM value
    Failure Indicators: "service not found", timeout, crash
    Evidence: .sisyphus/evidence/task-7-macos-gatt-read.txt
  ```

  **Commit**: YES (groups with Tasks 5, 6, 8)
  - Message: `feat(ble): implement GATT PSM discovery for macOS peripheral and cross-platform central`
  - Files: `src/transport/ble/io_macos.rs`
  - Pre-commit: `cargo build --features ble-macos`

- [x] 8. Connection Decision Logic — When to Use GATT PSM Discovery

  **What to do**:
  - Update `scan_probe_loop` (in `mod.rs`, around line 1277-1294) to decide whether to do GATT PSM discovery or use the fixed PSM.
  - The decision logic:
    1. If the peer's `GATT_SUPPORTED` flag is known (from a previous capability exchange) AND the peer is a macOS peripheral → do GATT PSM read first, then L2CAP connect with discovered PSM
    2. If the peer has no `GATT_SUPPORTED` flag OR is known to use fixed PSM → L2CAP connect directly with `config.psm()` (0x0085)
    3. Fallback: if GATT PSM read fails → try fixed PSM 0x0085 as fallback
  - **Problem**: PeerCapabilities are exchanged AFTER L2CAP connect (during pubkey exchange). So on first contact, we don't know if the peer supports GATT. Solutions:
    - **Option A**: Always try GATT PSM read first for all unknown peers, fall back to fixed PSM on failure. Simple but adds latency to all first connections.
    - **Option B**: Try fixed PSM first. If it works, capabilities are exchanged and stored. On reconnect, use stored capabilities to decide GATT vs fixed. This means the FIRST connection to a macOS peripheral will fail (wrong PSM), but the second attempt will use GATT.
    - **Option C**: During BLE scan, check if the peer advertises the `FIPS_GATT_PSM_SERVICE_UUID` in addition to `FIPS_SERVICE_UUID`. If yes, do GATT read. This requires scanner changes but avoids the chicken-and-egg.
    - **Recommendation**: Option C — it's clean, has zero false attempts, and the scanner change is minimal. macOS peripheral advertises BOTH UUIDs; scanner checks for GATT PSM UUID to decide.
  - Update `BluestScanner` and `BluerScanner` to detect `FIPS_GATT_PSM_SERVICE_UUID` in advertisements and return a flag/enum indicating GATT PSM support alongside the `BleAddr`.
  - This may require changing `BleScanner::next()` return type from `Option<BleAddr>` to `Option<DiscoveredPeer>` where `DiscoveredPeer` includes `addr: BleAddr` and `has_gatt_psm: bool`. Alternatively, keep `BleAddr` and add a separate lookup.
  - Update `connect_async` / `scan_probe_loop` to call `io.discover_gatt_psm()` when the peer has `has_gatt_psm == true`, then pass the discovered PSM to `io.connect()`.

  **Must NOT do**:
  - Do not attempt GATT PSM read for peers that don't advertise it — waste of time
  - Do not change the BleIo trait's `connect()` signature — keep `psm: u16`
  - Do not store/cache PSM across connections
  - Do not break the existing macOS→Linux flow (fixed PSM 0x0085)

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Changes the connection decision pipeline — core logic with scanner interface changes, needs careful backwards compatibility
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on Tasks 5, 7 for the discover_gatt_psm methods)
  - **Parallel Group**: Wave 2 (late — after Tasks 5, 7 provide the GATT read methods)
  - **Blocks**: Tasks 9, 10
  - **Blocked By**: Tasks 1, 2, 5, 7

  **References**:

  **Pattern References**:
  - `src/transport/ble/mod.rs:1277-1294` — `scan_probe_loop` L2CAP connect section (this is what changes)
  - `src/transport/ble/mod.rs:1296-1340` — pubkey exchange and cross-probe tie-breaker (unchanged, but runs after connect)
  - `src/transport/ble/io.rs:70-77` — `BleScanner` trait (may need return type change)
  - `src/transport/ble/io_macos.rs:178-186` — `BluestScanner` (needs GATT PSM detection in scan results)
  - `src/transport/ble/io_macos.rs:338-349` — macOS scan stream (where advertisement data is available)

  **API/Type References**:
  - `BleScanner::next()` — currently returns `Option<BleAddr>`, may need `Option<DiscoveredPeer>`
  - `BleIo::connect()` — takes `psm: u16`, unchanged

  **WHY Each Reference Matters**:
  - The scan_probe_loop (1277-1294) is the exact insertion point — currently it does `io.connect(&addr, psm)` with a fixed PSM. The change is: if peer advertises GATT PSM service → `io.discover_gatt_psm(&addr).await` → use discovered PSM.
  - The scanner trait change propagates to all implementations (BluerScanner, BluestScanner, MockBleScanner).

  **Acceptance Criteria**:
  - [ ] Scanner can detect GATT PSM service UUID in advertisements
  - [ ] scan_probe_loop uses GATT PSM discovery for peers that advertise it
  - [ ] scan_probe_loop uses fixed PSM 0x0085 for peers that don't advertise GATT PSM
  - [ ] Fallback: GATT PSM read failure → try fixed PSM
  - [ ] Existing macOS→Linux flow unchanged (Linux doesn't advertise GATT PSM)
  - [ ] `cargo build --features ble-macos` and `cargo build --features ble` succeed
  - [ ] `cargo test` passes

  **QA Scenarios**:

  ```
  Scenario: Connection decision selects GATT PSM for macOS peripheral
    Tool: Bash (logs)
    Preconditions: macOS peripheral running (Task 5), Linux scanning
    Steps:
      1. Restart Linux FIPS
      2. Wait 90 seconds for scan + GATT discovery + L2CAP connect
      3. grep "GATT PSM\|discovered.*psm\|L2CAP.*connect" in Linux logs
    Expected Result: Log sequence shows: peer advertises GATT PSM UUID → GATT PSM read → L2CAP connect with discovered PSM → pubkey exchange → Noise handshake
    Failure Indicators: Fixed PSM used despite GATT advertisement, GATT read skipped
    Evidence: .sisyphus/evidence/task-8-connection-decision.txt

  Scenario: Connection decision uses fixed PSM for Linux peer
    Tool: Bash (logs)
    Preconditions: macOS central scanning, Linux advertising (current setup)
    Steps:
      1. Check macOS logs for connection to Linux
      2. Verify no GATT PSM discovery attempted for Linux peer
      3. Verify L2CAP connects directly with PSM 0x0085
    Expected Result: Direct L2CAP connect, no GATT read
    Failure Indicators: GATT PSM read attempted for Linux peer
    Evidence: .sisyphus/evidence/task-8-fixed-psm-fallback.txt
  ```

  **Commit**: YES (groups with Tasks 5, 6, 7)
  - Message: `feat(ble): implement GATT PSM discovery for macOS peripheral and cross-platform central`
  - Files: `src/transport/ble/mod.rs`, `src/transport/ble/io.rs`, `src/transport/ble/io_macos.rs`
  - Pre-commit: `cargo test && cargo build --features ble-macos`

- [x] 9. MockBleIo GATT Extensions + Unit Tests

  **What to do**:
  - Extend `MockBleIo` (in `io.rs`, mock section starting ~line 700) to support GATT PSM exchange simulation.
  - If Task 8 changed `BleScanner::next()` return type to include GATT PSM flag, update `MockBleScanner` accordingly.
  - Add mock method: `set_gatt_psm(addr: BleAddr, psm: u16)` — configures the mock to return a specific PSM for GATT discovery on that address.
  - Add mock method: `set_no_gatt(addr: BleAddr)` — configures the mock to fail GATT discovery for that address (non-GATT peer).
  - If `discover_gatt_psm()` was added to `BluerIo`/`BluestIo`, add equivalent to `MockBleIo`.
  - Write unit tests:
    1. `test_gatt_psm_discovery_success` — mock peer with GATT PSM → verify connect uses discovered PSM
    2. `test_gatt_psm_discovery_fallback_fixed` — mock peer without GATT → verify connect uses fixed PSM 0x0085
    3. `test_gatt_psm_discovery_failure_fallback` — mock peer with GATT that fails → verify fallback to fixed PSM
    4. `test_scanner_gatt_flag_detection` — mock scanner returns peers with/without GATT flag
    5. `test_psm_encoding_roundtrip` — verify 2-byte LE encoding: u16 → bytes → u16

  **Must NOT do**:
  - Do not add GATT server simulation to MockBleIo (unnecessary complexity)
  - Do not change existing mock tests — only add new ones

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Mock infrastructure + test writing, needs understanding of the mock patterns
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with Tasks 10, 11)
  - **Parallel Group**: Wave 3
  - **Blocks**: None
  - **Blocked By**: Tasks 2, 8

  **References**:

  **Pattern References**:
  - `src/transport/ble/io.rs:700-1025` — MockBleIo, MockBleStream, MockBleAcceptor, MockBleScanner implementations
  - `src/transport/ble/io.rs:719-770` — `MockBleStream::pair()` — how mock streams are created
  - `src/transport/ble/io.rs:830-860` — `MockBleIo::new()`, `inject_inbound()`, `inject_scan_result()`, `set_connect_handler()`

  **WHY Each Reference Matters**:
  - The existing mock infrastructure shows the pattern for adding new mock methods. `set_connect_handler()` is a good pattern for `set_gatt_psm()` — both configure per-address behavior.

  **Acceptance Criteria**:
  - [ ] MockBleIo has GATT PSM simulation methods
  - [ ] MockBleScanner supports GATT flag if scanner interface changed
  - [ ] 5 new unit tests written and passing
  - [ ] `cargo test` passes (all existing + new tests)

  **QA Scenarios**:

  ```
  Scenario: All mock GATT tests pass
    Tool: Bash (cargo test)
    Preconditions: Tasks 2, 8 complete
    Steps:
      1. cargo test test_gatt -- --nocapture
      2. Verify all 5 new tests pass
      3. cargo test -- --nocapture (full test suite)
      4. Verify zero regressions
    Expected Result: All tests pass, including new GATT mock tests
    Failure Indicators: Any test failure
    Evidence: .sisyphus/evidence/task-9-mock-tests.txt

  Scenario: PSM encoding roundtrip is correct
    Tool: Bash (cargo test)
    Preconditions: None
    Steps:
      1. cargo test test_psm_encoding -- --nocapture
      2. Verify PSM 0x0085 encodes to [0x85, 0x00] (LE) and decodes back
      3. Verify PSM 0x0000 and PSM 0xFFFF edge cases
    Expected Result: All PSM values roundtrip correctly
    Failure Indicators: Encoding/decoding mismatch
    Evidence: .sisyphus/evidence/task-9-psm-encoding.txt
  ```

  **Commit**: YES
  - Message: `test(ble): add MockBleIo GATT PSM exchange scenarios`
  - Files: `src/transport/ble/io.rs`
  - Pre-commit: `cargo test`

- [x] 10. Build, Deploy, and Integration Test

  **What to do**:
  - Build the updated FIPS binary for macOS: `cargo build --release --features ble-macos`
  - Build the updated FIPS binary for Linux (via SSH): `cargo build --release --features ble`
  - Update macOS config (`/tmp/fips-test-macos/config.yaml`) to enable peripheral mode:
    ```yaml
    transports:
      ble:
        advertise: true
        accept_connections: true
        scan: true
        auto_connect: true
    ```
  - Deploy Linux binary: copy to Linux node, restart systemd service
  - Test sequence:
    1. Start macOS FIPS in peripheral + central mode
    2. Restart Linux FIPS
    3. Wait 90 seconds for BLE discovery + GATT PSM exchange + L2CAP connect
    4. Verify connection established: `fipsctl show peers` or log analysis
    5. Test ping: `ping6 -c 5 <peer-fips-addr>`
    6. Verify macOS→Linux still works (regression check): macOS should also connect outbound to Linux as before
    7. Verify two connections: macOS→Linux (outbound, fixed PSM) and Linux→macOS (inbound, GATT PSM)
    OR: the cross-probe tie-breaker should settle on one direction

  **Must NOT do**:
  - Do not change Linux config `[SHARED]` fields without commenting on issue #44
  - Do not change the macOS `send_rate_bps` setting (keep at 150000)
  - Do not restart macOS FIPS via script (requires sudo password — user must do it manually via tmux)

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Cross-platform deployment + integration testing with real BLE hardware
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO (sequential — needs all code complete)
  - **Parallel Group**: Wave 3 (sequential after Tasks 5-8)
  - **Blocks**: Task 11
  - **Blocked By**: Tasks 5, 6, 7, 8

  **References**:

  **Pattern References**:
  - macOS config: `/tmp/fips-test-macos/config.yaml` — current config (add `advertise: true`, `accept_connections: true`)
  - Linux config: `/etc/fips/fips.yaml` — IMMUTABLE, may not need changes (Linux already accepts connections)
  - AGENTS.md "Instructions" section — build commands, SSH details, systemd service management

  **WHY Each Reference Matters**:
  - The config files define the test setup. macOS needs peripheral mode enabled. Linux config should not change (it already scans + auto_connects).
  - AGENTS.md has SSH credentials, binary deployment steps, and the immutability note for Linux config.

  **Acceptance Criteria**:
  - [ ] macOS binary builds with `--features ble-macos`
  - [ ] Linux binary builds with `--features ble`
  - [ ] macOS FIPS starts in peripheral + central mode without errors
  - [ ] Linux FIPS discovers macOS via GATT PSM exchange
  - [ ] L2CAP connection established with discovered PSM
  - [ ] Ping works: 5/5 success, RTT ~130ms
  - [ ] macOS→Linux still works (regression)

  **QA Scenarios**:

  ```
  Scenario: End-to-end Linux→macOS connection via GATT PSM
    Tool: Bash (SSH + logs)
    Preconditions: Both binaries built and deployed, macOS in peripheral mode
    Steps:
      1. SSH to 192.168.13.218
      2. sudo systemctl restart fips
      3. Wait 90 seconds
      4. sudo journalctl -u fips --since "3 minutes ago" | grep -i "gatt\|psm\|connect\|handshake"
      5. Verify sequence: GATT PSM read → L2CAP connect → pubkey exchange → Noise handshake → link UP
      6. ping6 -c 5 <macOS-fips-ipv6>
    Expected Result: Full connection sequence in logs, ping 5/5 success
    Failure Indicators: GATT timeout, wrong PSM, handshake failure, ping loss
    Evidence: .sisyphus/evidence/task-10-e2e-connection.txt

  Scenario: Regression — macOS→Linux still works
    Tool: Bash (logs)
    Preconditions: Both nodes running
    Steps:
      1. Check macOS logs for outbound connection to Linux
      2. Verify L2CAP connect uses fixed PSM 0x0085 for Linux peer
      3. Verify ping from macOS to Linux works
    Expected Result: macOS→Linux unaffected by GATT changes
    Failure Indicators: macOS attempts GATT read for Linux peer, connection failure
    Evidence: .sisyphus/evidence/task-10-regression-mac-to-linux.txt
  ```

  **Commit**: NO (deployment, not code)

- [x] 11. BLE Capture, Decrypt, Evidence, and Issue Update

  **What to do**:
  - Capture BLE traffic during a Linux→macOS GATT PSM exchange + L2CAP connection:
    1. On Linux: `sudo btmon -w /tmp/gatt-psm-capture.btsnoop &`
    2. Restart Linux FIPS to trigger new connection
    3. Wait 120+ seconds (macOS BLE reconnect timing)
    4. Stop btmon
  - Copy JSONL ephemeral key dump from Linux: `/var/log/fips/fips-ik-ephemeral.jsonl`
  - Run Python decrypter on the capture:
    ```bash
    python3 /tmp/fips_decrypt_ble.py /tmp/gatt-psm-capture.btsnoop \
      --linux-key bbc890646a5e084a1c75474816f64906c21d391c802804350e57dda30fe6b051 \
      --macos-pubkey 026b124a11aa018552f777e509c613120ed74e9edbeed4efb719d9cf436de34e43 \
      --ik-log /tmp/fips-ik-ephemeral.jsonl
    ```
  - Verify the decrypter can handle the new connection flow (GATT read followed by L2CAP)
  - Compare performance: ping RTT for GATT-PSM-discovered L2CAP vs direct-PSM L2CAP. They should be identical since the data path is the same — the only difference is connection setup time.
  - Update the GitHub issue (created in Task 3) with:
    - Evidence: capture screenshot, decrypter output, ping results
    - Connection setup time comparison (GATT PSM flow vs direct PSM)
    - Interoperability matrix with checkmarks for what's tested
  - Save evidence files to `.sisyphus/evidence/`

  **Must NOT do**:
  - Do not modify the Python decrypter unless it breaks (it should handle the new flow since the Noise handshake is the same)
  - Do not push captures to git (binary files)
  - Do not include private keys in the GitHub issue

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Complex multi-step hardware testing with capture + decrypt + analysis
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO (sequential — needs working connection from Task 10)
  - **Parallel Group**: Wave 3 (after Task 10)
  - **Blocks**: None
  - **Blocked By**: Tasks 3, 10

  **References**:

  **Pattern References**:
  - `/tmp/fips_decrypt_ble.py` — Python BLE decrypter (use as-is)
  - AGENTS.md "Discoveries" → "macOS BLE Scanner Reconnection Timing" — 120+ second capture window needed
  - GitHub issue #48 — Previous capture/decrypt evidence format (follow same pattern)
  - GitHub issue #50 — Evidence format with before/after comparison

  **WHY Each Reference Matters**:
  - The Python decrypter handles Noise IK over L2CAP captures. Since GATT PSM exchange happens at a different BLE layer (ATT/GATT), it should appear as separate ATT protocol frames in the capture, not affecting the L2CAP/FMP decryption.
  - The 120+ second capture window is critical — macOS takes 30-60+ seconds to discover and connect.

  **Acceptance Criteria**:
  - [ ] BLE capture saved to `/tmp/gatt-psm-capture.btsnoop`
  - [ ] JSONL ephemeral keys captured
  - [ ] Python decrypter successfully decrypts FMP frames from GATT-PSM-initiated connection
  - [ ] Performance comparison documented (connection setup time, steady-state RTT)
  - [ ] GitHub issue updated with evidence
  - [ ] Evidence files saved to `.sisyphus/evidence/`

  **QA Scenarios**:

  ```
  Scenario: BLE capture shows GATT PSM exchange followed by L2CAP connection
    Tool: Bash (btmon + Python decrypter)
    Preconditions: Working Linux→macOS connection (Task 10 verified)
    Steps:
      1. SSH to Linux, start btmon: sudo btmon -w /tmp/gatt-psm-capture.btsnoop &
      2. sudo systemctl restart fips
      3. Wait 120 seconds
      4. Kill btmon
      5. SCP capture to macOS
      6. Run Python decrypter with --ik-log
      7. Verify output shows: ATT/GATT read of PSM characteristic, then L2CAP connect, then Noise MSG1/MSG2, then FMP frames
    Expected Result: ≥50 FMP frames decrypted, GATT PSM read visible in capture
    Failure Indicators: 0 frames decrypted, GATT not visible in capture, key mismatch
    Evidence: .sisyphus/evidence/task-11-decrypt-output.txt

  Scenario: Performance comparison GATT-PSM vs direct-PSM
    Tool: Bash (ping)
    Preconditions: Connection established via GATT PSM
    Steps:
      1. ping6 -c 20 <macOS-fips-addr> from Linux → record RTT stats
      2. Compare with previous ping results (RTT ~130ms from AGENTS.md)
    Expected Result: RTT within 10% of previous measurements (GATT PSM adds ~0ms to steady-state since data path is the same L2CAP)
    Failure Indicators: RTT significantly higher (>200ms), packet loss >10%
    Evidence: .sisyphus/evidence/task-11-performance-comparison.txt
  ```

  **Commit**: YES
  - Message: `docs(ble): update GATT PSM exchange issue with evidence and performance data`
  - Files: AGENTS.md (update Accomplished section)
  - Pre-commit: None

---

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [x] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, grep for pattern, run command). For each "Must NOT Have": search codebase for forbidden patterns — reject with file:line if found. Check evidence files exist in .sisyphus/evidence/. Compare deliverables against plan.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [x] F2. **Code Quality Review** — `unspecified-high`
  Run `cargo build --release --features ble-macos` and `cargo build --release --features ble` and `cargo test`. Review all changed files for: `as any` equivalents, empty error handlers, debug prints in prod, commented-out code, unused imports. Check AI slop: excessive comments, over-abstraction, generic variable names.
  Output: `Build macOS [PASS/FAIL] | Build Linux [PASS/FAIL] | Tests [N pass/N fail] | Files [N clean/N issues] | VERDICT`

- [x] F3. **Real Manual QA** — `unspecified-high`
  Start from clean state. Build macOS binary. Start macOS FIPS with `advertise: true`, `accept_connections: true`. Restart Linux FIPS. Wait for Linux to discover macOS via GATT PSM exchange and establish L2CAP connection. Verify ping works. Capture BLE traffic. Run Python decrypter on capture.
  Output: `Connection [PASS/FAIL] | Ping [PASS/FAIL] | Capture [N frames] | Decrypt [N/N] | VERDICT`

- [x] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual diff (git log/diff). Verify 1:1 — everything in spec was built (no missing), nothing beyond spec was built (no creep). Check "Must NOT do" compliance. Detect cross-task contamination. Flag unaccounted changes.
  Output: `Tasks [N/N compliant] | Contamination [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

| Commit | Scope | Message | Files | Pre-commit |
|--------|-------|---------|-------|------------|
| 1 | Tasks 1-2 | `feat(ble): add GATT PSM exchange UUIDs and activate GATT_SUPPORTED capability` | `mod.rs`, `io.rs`, `io_macos.rs` | `cargo test` |
| 2 | Tasks 5-8 | `feat(ble): implement GATT PSM discovery for macOS peripheral and cross-platform central` | `io_macos.rs`, `io.rs`, `mod.rs` | `cargo build --features ble-macos` |
| 3 | Task 9 | `test(ble): add MockBleIo GATT PSM exchange scenarios` | `io.rs` (mock section) | `cargo test` |
| 4 | Task 11 | `docs(ble): update GATT PSM exchange issue with evidence` | docs, AGENTS.md | — |

---

## Success Criteria

### Verification Commands
```bash
cargo build --release --features ble-macos  # Expected: success
cargo build --release --features ble        # Expected: success (cross-compile or on Linux)
cargo test                                  # Expected: all pass, 0 failures
# On Linux, after deployment:
ping6 -c 5 <macOS-fips-addr>               # Expected: 5/5 replies, RTT ~130ms
```

### Final Checklist
- [ ] macOS can advertise and accept inbound BLE connections
- [ ] Linux can discover macOS's dynamic L2CAP PSM via GATT
- [ ] L2CAP connection established using discovered PSM
- [ ] Noise handshake completes over GATT-discovered L2CAP
- [ ] Ping works bidirectionally
- [ ] macOS→Linux still works (no regression)
- [ ] BLE capture + Python decrypter proof saved
- [ ] GitHub issue updated with evidence
- [ ] All "Must Have" present
- [ ] All "Must NOT Have" absent
- [ ] All tests pass
