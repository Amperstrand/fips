# macOS BLE L2CAP Support for FIPS

## TL;DR

> **Quick Summary**: Add macOS BLE support to FIPS using the `bluest` crate as a cross-platform CoreBluetooth backend. Implement `BluestIo` wrapping `bluest::Adapter`/`Device`/`L2capChannel` behind the existing `BleIo` trait, central role only (scan + connect to Linux peripheral on PSM 0x0085). Peripheral methods stubbed with `NotSupported`.
> 
> **Deliverables**:
> - `BluestIo` struct implementing `BleIo` trait (central-only, L2CAP CoC)
> - `BluestStream`, `BluestAcceptor`, `BluestScanner` associated types
> - Platform gates widened from Linux-only to Linux+macOS
> - `bluest` dependency added to Cargo.toml (macOS-gated)
> - BleAddr extended with UUID-based addressing for CoreBluetooth
> - macOS integration tests for BLE transport
> - All 46 existing MockBleIo tests still passing
> 
> **Estimated Effort**: Medium (2-4 days)
> **Parallel Execution**: YES - 3 waves
> **Critical Path**: Task 1 (deps) → Task 3 (BleAddr) → Task 5 (BluestIo skeleton) → Task 7 (scanning) → Task 8 (connect+L2CAP) → Task 9 (integration) → F1-F4

---

## Context

### Original Request
Add native BLE L2CAP support to FIPS on macOS by implementing a CoreBluetooth backend, enabling the macOS FIPS node to discover and connect to a nearby Linux FIPS node over Bluetooth.

### Interview Summary
**Key Discussions**:
- `bluer` crate (Linux BLE backend) is Linux-only — macOS needs different crate
- `bluest` crate (v0.6.10) discovered: cross-platform, L2CAP CoC support on macOS, 140+ stars, ~47k downloads/month
- `bluest` is central-only — no CBPeripheralManager, no advertising, no L2CAP listening
- macOS dynamically assigns PSMs for peripheral — can't fix to 0x0085. But central CAN connect to Linux's fixed 0x0085
- User chose: central-first approach, peripheral stubbed with NotSupported
- User chose: mock tests + integration tests for macOS-specific backend
- BleAddr uses MAC addresses (`[u8; 6]`) but bluest uses UUID-based `DeviceId` — needs adaptation

**Research Findings**:
- bluest L2CAP API: `Device::open_l2cap_channel(psm, secure)` → `L2capChannel` with `read()`/`write()`/`split()`
- bluest requires features: `l2cap` + `unstable`
- CoreBluetooth L2CAP available since macOS 10.14
- Platform gates in 4 files: `transport/mod.rs`, `node/mod.rs`, `node/lifecycle.rs`, `Cargo.toml`
- BluerIo wraps bluer Session+Adapter, uses async-first patterns, maps errors to TransportError
- Existing Swift prototype at `prototype/main.swift` (446 lines, working L2CAP peripheral)

### Metis Review
**Identified Gaps** (addressed):
- BleAddr MAC vs UUID mismatch — addressed via enum variant or adapter-prefixed UUID string
- MTU negotiation fallback — addressed by logging warning and using negotiated MTU
- Pre-handshake timing — already handled in BleTransport layer, not in BleIo
- Peripheral method stubs — explicit NotSupported with descriptive messages
- Integration test CI gap — macOS integration tests gated behind `#[cfg(feature = "ble-integration")]`
- bluest stability risk — pin to specific version, have objc2 fallback path documented

---

## Work Objectives

### Core Objective
Enable macOS FIPS nodes to discover and connect to Linux FIPS nodes over BLE L2CAP, using the `bluest` crate as the CoreBluetooth backend behind the existing `BleIo` trait abstraction.

### Concrete Deliverables
- `src/transport/ble/bluest.rs` — BluestIo + BluestStream + BluestScanner + BluestAcceptor implementations
- Updated `Cargo.toml` — bluest dependency gated to macOS
- Updated `src/transport/ble/mod.rs` — DefaultBleTransport selects BluestIo on macOS
- Updated `src/transport/ble/io.rs` — BleIo module re-export for BluestIo
- Updated `src/transport/ble/addr.rs` — BleAddr UUID support
- Updated `src/transport/mod.rs` — cfg gates widened
- Updated `src/node/mod.rs` — cfg gates widened
- Updated `src/node/lifecycle.rs` — cfg gates widened
- `src/node/tests/ble_macos.rs` or similar — macOS integration tests

### Definition of Done
- [ ] `cargo build --release` on macOS with BLE feature compiles cleanly
- [ ] `cargo build --release --features ble` on Linux still compiles cleanly
- [ ] `cargo test` — all 46 MockBleIo tests pass
- [ ] `cargo test` — new BluestIo unit tests pass
- [ ] `cargo clippy` — no new warnings
- [ ] macOS node can scan and discover Linux FIPS node advertising FIPS UUID
- [ ] macOS node can connect to Linux FIPS node via L2CAP on PSM 0x0085
- [ ] Pre-handshake [0x00][pubkey:32] exchange succeeds over L2CAP channel

### Must Have
- BluestIo implementing all 7 BleIo trait methods
- Central role: start_scanning, connect produce working BLE connections
- Peripheral role: listen, start_advertising, stop_advertising return TransportError with clear message
- BleAddr support for UUID-based device identification (bluest DeviceId)
- All cfg gates widened to include macOS
- DefaultBleTransport resolves to BluestIo on macOS (non-test builds)
- bluest pinned to specific version in Cargo.toml
- Error mapping from bluest errors to TransportError

### Must NOT Have (Guardrails)
- NO peripheral role implementation (advertising, L2CAP listening) — stubbed only
- NO changes to BluerIo (Linux backend must be untouched)
- NO new TransportError variants — use existing Io, NotSupported, ConnectionRefused, etc.
- NO GATT-based transport — L2CAP CoC only
- NO exploration of bluest APIs beyond Adapter, Device, L2capChannel, scan, connect
- NO abstraction layers or trait changes to BleIo — implement it as-is
- NO protocol changes — same UUID, same PSM, same pre-handshake, same wire format
- NO excessive error mapping (generic map to TransportError::Io with context, not per-error-code mapping)
- NO over-engineering of MTU negotiation — request 2048, use whatever is negotiated, log if less

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed. No exceptions.
> Acceptance criteria requiring "user manually tests/confirms" are FORBIDDEN.

### Test Decision
- **Infrastructure exists**: YES — 46 MockBleIo tests in `src/node/tests/ble.rs`
- **Automated tests**: YES (tests-after) — new unit tests for BluestIo + macOS integration tests
- **Framework**: `cargo test` (standard Rust test framework)

### QA Policy
Every task MUST include agent-executed QA scenarios.
Evidence saved to `.sisyphus/evidence/task-{N}-{scenario-slug}.{ext}`.

- **Compilation**: Use Bash — `cargo build`, `cargo check`
- **Unit tests**: Use Bash — `cargo test --lib`
- **Integration tests**: Use Bash — `cargo test --test` (gated behind feature flag for hardware-dependent)
- **Lint**: Use Bash — `cargo clippy`
- **Manual BLE tests**: Use interactive_bash (tmux) — run fips daemon, fipsctl queries

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Start Immediately — foundation, no dependencies):
├── Task 1: Add bluest dependency + new feature flag [quick]
├── Task 2: Widen platform cfg gates (4 files) [quick]
└── Task 3: Extend BleAddr for UUID-based device IDs [quick]

Wave 2 (After Wave 1 — core implementation):
├── Task 4: Update DefaultBleTransport type alias for macOS [quick]
├── Task 5: BluestIo skeleton + peripheral stubs [deep]
├── Task 6: BluestStream + BluestAcceptor types [deep]
└── Task 7: BluestScanner + scanning implementation [deep]

Wave 3 (After Wave 2 — connect + integration):
├── Task 8: BluestIo connect + L2CAP channel establishment [deep]
├── Task 9: Integration tests + wiring in node/mod.rs [unspecified-high]
└── Task 10: Build verification + cross-platform check [quick]

Wave FINAL (After ALL tasks — 4 parallel reviews, then user okay):
├── Task F1: Plan compliance audit (oracle)
├── Task F2: Code quality review (unspecified-high)
├── Task F3: Real manual QA (unspecified-high)
└── Task F4: Scope fidelity check (deep)
-> Present results -> Get explicit user okay
```

### Dependency Matrix

| Task | Depends On | Blocks | Wave |
|------|-----------|--------|------|
| 1    | —         | 4, 5, 6, 7, 8, 9 | 1 |
| 2    | —         | 4, 9, 10 | 1 |
| 3    | —         | 5, 6, 7, 8 | 1 |
| 4    | 1, 2      | 9, 10 | 2 |
| 5    | 1, 3      | 8, 9 | 2 |
| 6    | 1, 3      | 8 | 2 |
| 7    | 1, 3      | 9 | 2 |
| 8    | 5, 6      | 9 | 3 |
| 9    | 4, 5, 7, 8 | 10 | 3 |
| 10   | 2, 4, 9   | F1-F4 | 3 |

### Agent Dispatch Summary

- **Wave 1**: **3 tasks** — T1 → `quick`, T2 → `quick`, T3 → `quick`
- **Wave 2**: **4 tasks** — T4 → `quick`, T5 → `deep`, T6 → `deep`, T7 → `deep`
- **Wave 3**: **3 tasks** — T8 → `deep`, T9 → `unspecified-high`, T10 → `quick`
- **FINAL**: **4 tasks** — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [x] 1. Add bluest dependency and BLE macOS feature flag

  **What to do**:
  - Add `bluest` crate (v0.6.10) to `Cargo.toml` under `[target.'cfg(target_os = "macos")'.dependencies]`
  - Enable features: `l2cap`, `unstable`
  - Create a new cargo feature `ble-macos = ["dep:bluest"]` (mirrors the Linux `ble = ["dep:bluer"]` pattern)
  - Ensure `cargo check --features ble-macos` compiles on macOS (even before implementation code exists)
  - Verify `cargo check --features ble` still works on Linux (no changes to Linux deps)

  **Must NOT do**:
  - Do NOT change or remove the existing `ble` feature flag
  - Do NOT add bluest to non-macOS targets
  - Do NOT add any other new dependencies

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Single-file modification (Cargo.toml), straightforward dependency addition
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - `git-master`: Not needed — simple file edit, not git operations

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 2, 3)
  - **Blocks**: Tasks 4, 5, 6, 7, 8, 9
  - **Blocked By**: None (can start immediately)

  **References**:

  **Pattern References**:
  - `Cargo.toml` (root) — Look at `[target.'cfg(target_os = "linux")'.dependencies]` section around line with `bluer` — replicate this pattern for macOS with bluest
  - `Cargo.toml` — Look at `[features]` section where `ble = ["dep:bluer"]` is defined — add `ble-macos` feature mirroring this pattern

  **External References**:
  - bluest crate: `https://crates.io/crates/bluest` — verify version 0.6.10 exists, check feature flag names
  - bluest GitHub: `https://github.com/alexmoon/bluest` — check Cargo.toml for available features (`l2cap`, `unstable`)

  **WHY Each Reference Matters**:
  - Cargo.toml bluer section: Shows exact pattern for platform-gated BLE dependency — replicate for macOS
  - Features section: Shows naming convention for BLE feature flag — follow for consistency

  **Acceptance Criteria**:
  - [ ] `cargo check --features ble-macos` succeeds on macOS (may have unused import warnings, that's OK)
  - [ ] `cargo check --features ble` succeeds on Linux (unchanged behavior)
  - [ ] `Cargo.toml` contains `bluest` under macOS target dependencies
  - [ ] `Cargo.toml` contains `ble-macos` feature flag

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: macOS feature flag compiles
    Tool: Bash
    Preconditions: On macOS host with Rust toolchain
    Steps:
      1. Run `cargo check --features ble-macos` in project root
      2. Verify exit code is 0
    Expected Result: Clean compilation (exit code 0), no errors (warnings OK)
    Failure Indicators: Compilation error mentioning `bluest`, unresolved import, feature not found
    Evidence: .sisyphus/evidence/task-1-macos-feature-check.txt

  Scenario: Linux feature flag unchanged
    Tool: Bash
    Preconditions: Cargo.toml modified with new dependency
    Steps:
      1. Run `cargo check` (default, no BLE features) in project root
      2. Verify exit code is 0
    Expected Result: Clean compilation, no regressions
    Failure Indicators: Any new compilation error
    Evidence: .sisyphus/evidence/task-1-linux-unchanged.txt
  ```

  **Commit**: YES (group 1)
  - Message: `feat(ble): add bluest dependency and ble-macos feature flag`
  - Files: `Cargo.toml`
  - Pre-commit: `cargo check --features ble-macos`

- [x] 2. Widen platform cfg gates from Linux-only to Linux+macOS

  **What to do**:
  - In `src/transport/mod.rs`: Change every `#[cfg(target_os = "linux")]` to `#[cfg(any(target_os = "linux", target_os = "macos"))]` for BLE-related imports, TransportHandle::Ble variant, and associated methods
  - In `src/node/mod.rs`: Change `#[cfg(target_os = "linux")]` to `#[cfg(any(target_os = "linux", target_os = "macos"))]` for BLE transport creation block (~lines 740-785) and `resolve_ble_addr` method
  - In `src/node/lifecycle.rs`: Change `#[cfg(target_os = "linux")]` to `#[cfg(any(target_os = "linux", target_os = "macos"))]` for BLE address resolution in `initiate_peer_connection`
  - Also update any `#[cfg(all(feature = "ble", ...))]` patterns to include `ble-macos` where appropriate — e.g., `#[cfg(all(any(feature = "ble", feature = "ble-macos"), not(test)))]`
  - Search the entire codebase with `grep -r 'target_os.*linux' src/` to ensure NO BLE-related cfg gates are missed

  **Must NOT do**:
  - Do NOT change non-BLE cfg gates (there may be other Linux-specific code unrelated to BLE)
  - Do NOT modify any BLE implementation logic — only cfg attributes
  - Do NOT change test cfg gates

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Mechanical find-and-replace across known files, no logic changes
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 3)
  - **Blocks**: Tasks 4, 9, 10
  - **Blocked By**: None (can start immediately)

  **References**:

  **Pattern References**:
  - `src/transport/mod.rs` — All `#[cfg(target_os = "linux")]` gates (search for them) — these are the BLE import guards and TransportHandle variant gates
  - `src/node/mod.rs:740-785` — BLE transport creation block with `#[cfg(target_os = "linux")]` — this wires BleTransport into the node
  - `src/node/lifecycle.rs` — `initiate_peer_connection` function, look for `#[cfg(target_os = "linux")]` around BLE address resolution

  **WHY Each Reference Matters**:
  - transport/mod.rs: Controls whether BLE types are available at all — must include macOS for BluestIo to be usable
  - node/mod.rs: Controls whether BLE transport is created on startup — must include macOS
  - node/lifecycle.rs: Controls whether BLE addresses are resolved for peer connections — must include macOS

  **Acceptance Criteria**:
  - [ ] `grep -r 'cfg.*target_os.*"linux"' src/transport/ src/node/` returns NO BLE-related Linux-only gates
  - [ ] `cargo check` succeeds (no BLE features — should compile as before)
  - [ ] No non-BLE cfg gates were changed

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: No remaining Linux-only BLE gates
    Tool: Bash
    Preconditions: All cfg gates widened
    Steps:
      1. Run `grep -rn 'cfg.*target_os.*"linux"' src/transport/ src/node/`
      2. Inspect each result — verify none are BLE-related
    Expected Result: Zero BLE-related Linux-only cfg gates remain
    Failure Indicators: Any `cfg(target_os = "linux")` near `ble`, `Ble`, `BLE`, `BluerIo`
    Evidence: .sisyphus/evidence/task-2-cfg-gate-audit.txt

  Scenario: Default build still compiles
    Tool: Bash
    Preconditions: cfg gates modified
    Steps:
      1. Run `cargo check` (no features)
      2. Verify exit code 0
    Expected Result: Clean compilation
    Failure Indicators: Any compilation error in transport or node modules
    Evidence: .sisyphus/evidence/task-2-default-build.txt
  ```

  **Commit**: YES (group 2)
  - Message: `refactor(ble): widen platform gates from linux-only to linux+macos`
  - Files: `src/transport/mod.rs`, `src/node/mod.rs`, `src/node/lifecycle.rs`
  - Pre-commit: `cargo check`

- [x] 3. Extend BleAddr with UUID-based device identification

  **What to do**:
  - bluest identifies devices via `DeviceId` which wraps a UUID (128-bit), not a MAC address (48-bit)
  - Extend `BleAddr` to support UUID-based device identification alongside MAC addresses
  - **Recommended approach**: Change `BleAddr.device` from `[u8; 6]` to an enum:
    ```rust
    pub enum BleDeviceAddr {
        Mac([u8; 6]),          // Linux (BlueZ) — "AA:BB:CC:DD:EE:FF"
        Uuid([u8; 16]),        // macOS (CoreBluetooth) — UUID string
    }
    ```
  - Update `BleAddr` struct: `pub device: BleDeviceAddr` (was `pub device: [u8; 6]`)
  - Update `BleAddr::parse()` to handle both formats:
    - "adapter/AA:BB:CC:DD:EE:FF" → `BleDeviceAddr::Mac`
    - "adapter/XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX" → `BleDeviceAddr::Uuid`
  - Update `to_string_repr()` to format both variants
  - Update `to_transport_addr()` to work with both
  - Keep all existing `#[cfg(feature = "ble")]` bluer-specific methods working (they use `[u8; 6]`)
  - Add new `#[cfg(feature = "ble-macos")]` methods:
    - `from_bluest(device_id: bluest::DeviceId, adapter: &str) -> BleAddr`
    - `to_bluest_device_id() -> Option<bluest::DeviceId>`
  - Add comprehensive unit tests for UUID parsing, formatting, round-trip

  **Must NOT do**:
  - Do NOT remove MAC address support — Linux still uses it
  - Do NOT change how existing bluer-specific methods work
  - Do NOT change peer serialization format without careful consideration
  - Do NOT add UUID support to parts of the codebase that don't need it

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Single file modification with clear pattern, unit tests
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with Tasks 1, 2)
  - **Blocks**: Tasks 5, 6, 7, 8
  - **Blocked By**: None (can start immediately)

  **References**:

  **Pattern References**:
  - `src/transport/ble/addr.rs` — COMPLETE FILE — Current BleAddr implementation with MAC-based device field, parse(), to_string_repr(), from_bluer(), to_bluer_address(), to_socket_addr()
  - `src/transport/ble/addr.rs:from_bluer()` — Pattern for platform-specific constructor — replicate for bluest

  **API/Type References**:
  - bluest `DeviceId`: Check bluest docs at `https://docs.rs/bluest/latest/bluest/struct.DeviceId.html` — understand UUID format

  **WHY Each Reference Matters**:
  - addr.rs complete: Must understand ALL existing methods to avoid breaking them when changing device field type
  - from_bluer pattern: Shows how to add platform-specific constructor gated behind feature flag

  **Acceptance Criteria**:
  - [ ] `cargo test --lib transport::ble::addr` passes — all existing tests + new UUID tests
  - [ ] BleAddr::parse("hci0/AA:BB:CC:DD:EE:FF") still works (MAC format)
  - [ ] BleAddr::parse("default/XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX") works (UUID format)
  - [ ] Round-trip: parse → to_string_repr → parse produces identical BleAddr

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: MAC address backward compatibility
    Tool: Bash
    Preconditions: BleAddr modified with enum device type
    Steps:
      1. Run `cargo test --lib transport::ble::addr`
      2. Verify all existing addr tests pass
    Expected Result: All pre-existing tests pass (0 failures)
    Failure Indicators: Any test failure mentioning `parse`, `to_string`, `from_bluer`
    Evidence: .sisyphus/evidence/task-3-addr-compat.txt

  Scenario: UUID format parsing works
    Tool: Bash
    Preconditions: New UUID parsing code added
    Steps:
      1. Run `cargo test --lib transport::ble::addr::tests::uuid`
      2. Verify new UUID-specific tests pass
    Expected Result: UUID parse, format, round-trip tests pass
    Failure Indicators: Parse failure, format mismatch
    Evidence: .sisyphus/evidence/task-3-uuid-parsing.txt
  ```

  **Commit**: YES (group 3)
  - Message: `feat(ble): extend BleAddr with UUID-based device identification`
  - Files: `src/transport/ble/addr.rs`
  - Pre-commit: `cargo test --lib transport::ble::addr`

- [x] 4. Update DefaultBleTransport type alias for macOS

  **What to do**:
  - Add a new cfg-gated type alias in `src/transport/ble/mod.rs` for macOS:
    ```rust
    #[cfg(all(feature = "ble-macos", not(test)))]
    pub type DefaultBleTransport = BleTransport<io::BluestIo>;
    ```
  - Place it between the existing `BluerIo` alias (`#[cfg(all(feature = "ble", not(test)))]`) and the mock alias (`#[cfg(any(not(feature = "ble"), test))]`)
  - Update the mock alias to also exclude `ble-macos`:
    ```rust
    #[cfg(any(all(not(feature = "ble"), not(feature = "ble-macos")), test))]
    pub type DefaultBleTransport = BleTransport<io::MockBleIo>;
    ```
  - Also add `pub use bluest_impl::BluestIo;` in `src/transport/ble/io.rs` (gated behind `#[cfg(feature = "ble-macos")]`) mirroring the `pub use bluer_impl::*;` pattern
  - Ensure `cargo check --features ble-macos` resolves the BluestIo type (it won't compile fully yet — Task 5 creates the struct)

  **Must NOT do**:
  - Do NOT change the `ble` feature's BluerIo alias
  - Do NOT change any BleTransport generic code
  - Do NOT add BluestIo struct here — that's Task 5

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Small, targeted changes to type aliases and re-exports in 2 files
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 5, 6, 7)
  - **Blocks**: Tasks 9, 10
  - **Blocked By**: Tasks 1, 2

  **References**:

  **Pattern References**:
  - `src/transport/ble/mod.rs:55-59` — Existing `DefaultBleTransport` cfg-gated aliases (BluerIo for ble, MockBleIo for non-ble/test) — add macOS alias following this exact pattern
  - `src/transport/ble/io.rs:114-115` — `#[cfg(feature = "ble")] mod bluer_impl` + its `pub use bluer_impl::*` — replicate for `ble-macos` with `mod bluest_impl` pointing to the new file/module

  **WHY Each Reference Matters**:
  - mod.rs:55-59: Shows the 3-way cfg-gate pattern — must add BluestIo variant without breaking existing logic
  - io.rs:114-115: Shows how platform modules are declared and re-exported — exact pattern to copy for bluest

  **Acceptance Criteria**:
  - [ ] `src/transport/ble/mod.rs` has 3 cfg-gated `DefaultBleTransport` aliases (ble/BluerIo, ble-macos/BluestIo, mock)
  - [ ] `src/transport/ble/io.rs` has `#[cfg(feature = "ble-macos")] mod bluest_impl;` with appropriate re-export
  - [ ] `cargo check` (no features) still compiles — MockBleIo is selected

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Mock alias still selected without features
    Tool: Bash
    Preconditions: Type aliases updated in mod.rs
    Steps:
      1. Run `cargo check` (no features)
      2. Verify exit code 0
    Expected Result: Compiles cleanly — MockBleIo selected as DefaultBleTransport
    Failure Indicators: Type resolution error, "conflicting definitions"
    Evidence: .sisyphus/evidence/task-4-mock-alias.txt

  Scenario: BluestIo re-export declared correctly
    Tool: Bash
    Preconditions: io.rs updated with bluest_impl module declaration
    Steps:
      1. Run `grep -n 'bluest_impl\|BluestIo' src/transport/ble/io.rs`
      2. Verify module declaration and re-export lines exist
    Expected Result: Module declaration with cfg(feature = "ble-macos") gate present
    Failure Indicators: Missing module declaration, wrong cfg gate
    Evidence: .sisyphus/evidence/task-4-reexport-check.txt
  ```

  **Commit**: YES (group 4)
  - Message: `feat(ble): add BluestIo type alias for macOS DefaultBleTransport`
  - Files: `src/transport/ble/mod.rs`, `src/transport/ble/io.rs`
  - Pre-commit: `cargo check`

- [x] 5. Create BluestIo skeleton with peripheral stubs

  **What to do**:
  - Create new file `src/transport/ble/bluest.rs` (will be included as `mod bluest_impl` from io.rs)
  - Define `BluestIo` struct:
    ```rust
    pub struct BluestIo {
        adapter: bluest::Adapter,
        adapter_name: String,
        mtu: u16,
    }
    ```
  - Implement constructor `BluestIo::new(adapter_name: &str, mtu: u16) -> Result<Self, TransportError>`:
    - Call `Adapter::default().await` to get CoreBluetooth adapter
    - Wait for adapter to be available: `adapter.wait_available().await`
    - Store adapter, adapter name ("default" for macOS since CoreBluetooth has one adapter), and MTU
    - Map errors: `bluest::error::Error` → `TransportError::Io(std::io::Error::other(format!(...)))` following BluerIo pattern
  - Define FIPS_SERVICE_UUID as a `uuid::Uuid` constant: `9C90B790-2CC5-42C0-9F87-C9CC40648F4C`
  - Implement `BleIo` for `BluestIo` with STUBS for peripheral methods:
    - `listen(&self, psm: u16)` → `Err(TransportError::NotSupported("BLE peripheral role not supported on macOS (bluest is central-only)".into()))`
    - `start_advertising(&self)` → `Err(TransportError::NotSupported("BLE advertising not supported on macOS (bluest is central-only)".into()))`
    - `stop_advertising(&self)` → `Err(TransportError::NotSupported("BLE advertising not supported on macOS (bluest is central-only)".into()))`
    - `local_addr(&self)` → Return a BleAddr with UUID-based device ID (from `adapter.device_id()` or synthetic)
    - `adapter_name(&self)` → Return `&self.adapter_name`
  - Leave `connect()` and `start_scanning()` as `todo!()` placeholders (Tasks 7, 8 implement them)
  - Set associated types: `type Stream = BluestStream;` `type Acceptor = BluestAcceptor;` `type Scanner = BluestScanner;` (defined in Task 6)
  - File structure: all types in one `bluest.rs` file (mirroring how `bluer_impl` module has everything in `io.rs`)

  **Must NOT do**:
  - Do NOT implement scanning or connect logic — those are Tasks 7, 8
  - Do NOT add any GATT operations
  - Do NOT over-engineer error mapping — use generic `TransportError::Io` with context string
  - Do NOT create separate files for each type — keep everything in `bluest.rs`
  - Do NOT add `CBPeripheralManager` or any peripheral infrastructure

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Core struct + trait implementation, needs understanding of async patterns, associated types, error mapping
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - `playwright`: Not relevant — Rust backend code
    - `git-master`: Not needed — file creation, not git operations

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4, 6, 7)
  - **Blocks**: Tasks 8, 9
  - **Blocked By**: Tasks 1, 3

  **References**:

  **Pattern References**:
  - `src/transport/ble/io.rs:278-323` — `BluerIo` struct + constructor — exact pattern to replicate: struct fields, async `new()`, error mapping, adapter initialization, debug log
  - `src/transport/ble/io.rs:326-329` — `BleIo for BluerIo` associated type declarations — match exactly for BluestIo
  - `src/transport/ble/io.rs:134-145` — `map_err` and `map_io_err` helper functions — replicate for bluest errors

  **API/Type References**:
  - `src/transport/ble/io.rs:67-108` — Full `BleIo` trait definition — the contract BluestIo must satisfy
  - `src/transport/ble/io.rs:16-39` — `BleStream` trait — BluestStream must implement this
  - `src/transport/ble/io.rs:42-50` — `BleAcceptor` trait — BluestAcceptor must implement this
  - `src/transport/ble/io.rs:53-60` — `BleScanner` trait — BluestScanner must implement this

  **External References**:
  - bluest Adapter API: `https://docs.rs/bluest/latest/bluest/struct.Adapter.html` — `default()`, `wait_available()`
  - bluest L2capChannel: `https://docs.rs/bluest/latest/bluest/l2cap/struct.L2capChannel.html` — read/write/split API
  - bluest error types: `https://docs.rs/bluest/latest/bluest/error/enum.Error.html` — for error mapping

  **WHY Each Reference Matters**:
  - BluerIo struct/constructor: The template — follow field layout, async init sequence, error handling exactly
  - BleIo trait: The contract — every method signature must match
  - Sub-traits: Define what associated types must implement — guides BluestStream/Acceptor/Scanner shape
  - bluest docs: API specifics for Adapter initialization — `default()` vs `new()`, `wait_available()`

  **Acceptance Criteria**:
  - [ ] `src/transport/ble/bluest.rs` exists with BluestIo struct and BleIo impl
  - [ ] Peripheral stubs return `TransportError::NotSupported` with descriptive messages
  - [ ] `cargo check --features ble-macos` succeeds on macOS (with todo!() in connect/scanning)
  - [ ] `cargo test` (no features) still passes — MockBleIo unaffected

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: BluestIo compiles with feature flag
    Tool: Bash
    Preconditions: bluest.rs created, io.rs updated with module declaration
    Steps:
      1. Run `cargo check --features ble-macos`
      2. Verify exit code 0
    Expected Result: Compiles on macOS (todo!() in connect/scanning is OK for check)
    Failure Indicators: Type mismatch, missing trait impl, import errors
    Evidence: .sisyphus/evidence/task-5-bluest-check.txt

  Scenario: Peripheral stubs have correct error type
    Tool: Bash
    Preconditions: bluest.rs with stub implementations
    Steps:
      1. Run `grep -n 'NotSupported' src/transport/ble/bluest.rs`
      2. Verify listen, start_advertising, stop_advertising all return NotSupported
    Expected Result: 3 lines containing NotSupported with descriptive messages
    Failure Indicators: Missing stubs, wrong error type, empty error messages
    Evidence: .sisyphus/evidence/task-5-stubs-check.txt

  Scenario: Existing tests unaffected
    Tool: Bash
    Preconditions: All changes in place
    Steps:
      1. Run `cargo test --lib`
      2. Check that all 46 BLE tests pass
    Expected Result: 46 BLE tests pass, 0 failures
    Failure Indicators: Any test failure in transport::ble module
    Evidence: .sisyphus/evidence/task-5-existing-tests.txt
  ```

  **Commit**: YES (group 4)
  - Message: `feat(ble): add BluestIo skeleton with central-only support and peripheral stubs`
  - Files: `src/transport/ble/bluest.rs`
  - Pre-commit: `cargo check --features ble-macos`

- [x] 6. Implement BluestStream and BluestAcceptor types

  **What to do**:
  - In `src/transport/ble/bluest.rs`, implement `BluestStream` wrapping `bluest::l2cap::L2capChannel`:
    ```rust
    pub struct BluestStream {
        channel: L2capChannel,
        remote: BleAddr,
        mtu: u16,
    }
    ```
  - Implement `BleStream` for `BluestStream`:
    - `send(&self, data: &[u8])` — Use `channel.write(data).await`, map errors to `TransportError::SendFailed`
    - `recv(&self, buf: &mut [u8])` — Use `channel.read(buf).await`, map errors to `TransportError::RecvFailed`
    - `send_mtu(&self)` → return stored MTU value
    - `recv_mtu(&self)` → return stored MTU value
    - `remote_addr(&self)` → return `&self.remote`
  - **Thread safety**: `L2capChannel` may NOT be `Sync`. If it isn't, wrap with `tokio::sync::Mutex`:
    ```rust
    pub struct BluestStream {
        channel: tokio::sync::Mutex<L2capChannel>,
        remote: BleAddr,
        mtu: u16,
    }
    ```
    Check `L2capChannel`'s `Send`/`Sync` bounds in bluest docs before deciding.
  - Implement `BluestAcceptor` as a non-functional stub (central-only, no accepting):
    ```rust
    pub struct BluestAcceptor;

    impl BleAcceptor for BluestAcceptor {
        type Stream = BluestStream;
        async fn accept(&mut self) -> Result<BluestStream, TransportError> {
            // Peripheral not supported — this should never be called
            // (listen() returns NotSupported before an acceptor is created)
            Err(TransportError::NotSupported(
                "BLE accept not supported on macOS (central-only)".into()
            ))
        }
    }
    ```
  - Note on MTU: bluest's L2capChannel doesn't expose separate send/recv MTU. Use the configured MTU from BluestIo (passed during construction). If `channel.mtu()` exists in bluest, prefer it; otherwise use the configured value and log a debug message.

  **Must NOT do**:
  - Do NOT implement split() on L2capChannel — use whole-channel read/write
  - Do NOT add custom buffering — L2CAP provides packet boundaries
  - Do NOT add retry logic in send/recv — upper layers handle retries
  - Do NOT make BluestAcceptor do anything — it's an unreachable stub

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Needs async correctness, thread-safety decisions (Mutex vs direct), trait bound satisfaction
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4, 5, 7)
  - **Blocks**: Task 8
  - **Blocked By**: Tasks 1, 3

  **References**:

  **Pattern References**:
  - `src/transport/ble/io.rs:152-206` — `BluerStream` struct + `BleStream` impl — exact pattern to follow: store connection + remote addr + MTU, delegate send/recv to underlying transport
  - `src/transport/ble/io.rs:213-231` — `BluerAcceptor` struct + `BleAcceptor` impl — shows the acceptor pattern (BluestAcceptor is a stub but must satisfy the same trait)

  **API/Type References**:
  - `src/transport/ble/io.rs:16-39` — `BleStream` trait — the exact contract BluestStream must implement
  - `src/transport/ble/io.rs:42-50` — `BleAcceptor` trait — BluestAcceptor must have matching associated type

  **External References**:
  - bluest L2capChannel: `https://docs.rs/bluest/latest/bluest/l2cap/struct.L2capChannel.html` — check `read()`, `write()`, `split()`, `mtu()` methods and Send/Sync bounds
  - bluest L2capChannel source: `https://github.com/alexmoon/bluest/blob/main/src/corebluetooth/l2cap_channel.rs` — check if Send+Sync are implemented

  **WHY Each Reference Matters**:
  - BluerStream: The template for wrapping a transport connection — copy structure, adapt for bluest L2capChannel
  - BleStream trait: Must satisfy ALL method signatures exactly
  - bluest docs: Need to verify Send/Sync bounds on L2capChannel to decide Mutex wrapping

  **Acceptance Criteria**:
  - [ ] `BluestStream` implements `BleStream` with `send()`, `recv()`, MTU methods, `remote_addr()`
  - [ ] `BluestAcceptor` implements `BleAcceptor` with stub `accept()` returning NotSupported
  - [ ] `cargo check --features ble-macos` succeeds on macOS
  - [ ] `BleStream` bounds satisfied: `BluestStream: Send + Sync`

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: BluestStream satisfies BleStream trait bounds
    Tool: Bash
    Preconditions: BluestStream implemented in bluest.rs
    Steps:
      1. Run `cargo check --features ble-macos`
      2. Verify no "trait bound not satisfied" errors
    Expected Result: Clean compilation, BluestStream accepted as BleIo::Stream
    Failure Indicators: "the trait `Send` is not implemented", "the trait `Sync` is not implemented"
    Evidence: .sisyphus/evidence/task-6-stream-bounds.txt

  Scenario: BluestAcceptor stub compiles correctly
    Tool: Bash
    Preconditions: BluestAcceptor stub in bluest.rs
    Steps:
      1. Run `grep -n 'BluestAcceptor' src/transport/ble/bluest.rs`
      2. Verify struct definition and BleAcceptor impl exist
    Expected Result: BluestAcceptor defined with BleAcceptor impl returning NotSupported
    Failure Indicators: Missing impl, wrong associated type
    Evidence: .sisyphus/evidence/task-6-acceptor-stub.txt
  ```

  **Commit**: YES (group 4)
  - Message: `feat(ble): implement BluestStream and BluestAcceptor types`
  - Files: `src/transport/ble/bluest.rs`
  - Pre-commit: `cargo check --features ble-macos`

- [x] 7. Implement BluestScanner and scanning via bluest

  **What to do**:
  - In `src/transport/ble/bluest.rs`, implement `BluestScanner`:
    ```rust
    pub struct BluestScanner {
        scan_stream: bluest::DeviceStream, // or whatever type adapter.scan() returns
        adapter: bluest::Adapter,
    }
    ```
  - Implement `BleScanner` for `BluestScanner`:
    - `next(&mut self) -> Option<BleAddr>`:
      - Loop over `scan_stream.next().await`
      - For each discovered device, check if it advertises the FIPS service UUID
      - bluest scan API: `adapter.scan(&[FIPS_UUID]).await?` returns a stream filtered by UUID
      - Since bluest already filters by UUID during scan, every device from the stream IS a FIPS peer
      - Convert `bluest::Device` to `BleAddr` using UUID-based device ID from Task 3
      - Return `Some(ble_addr)` on discovery, `None` when scan ends
      - Add `tracing::debug!` log on each FIPS peer discovery (matching BluerScanner pattern)
  - Implement `BluestIo::start_scanning()`:
    - Call `self.adapter.scan(&[FIPS_SERVICE_UUID]).await` to get scan stream
    - Map errors to `TransportError::Io`
    - Return `BluestScanner { scan_stream, adapter: self.adapter.clone() }`
    - Replace the `todo!()` placeholder from Task 5
  - **Important**: Check whether `bluest::Adapter::scan()` takes a slice of `uuid::Uuid` or `bluest::Uuid` — match the constant type accordingly

  **Must NOT do**:
  - Do NOT implement active scanning — use passive scan (let adapter.scan() handle it)
  - Do NOT add manual UUID filtering if bluest already filters during scan
  - Do NOT add timeouts to scanning — upper layer (BleTransport) handles scan lifecycle
  - Do NOT cache discovered devices — return each as discovered
  - Do NOT implement connect logic here — that's Task 8

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Async stream handling, UUID type matching, understanding bluest scan API return types
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4, 5, 6)
  - **Blocks**: Task 9
  - **Blocked By**: Tasks 1, 3

  **References**:

  **Pattern References**:
  - `src/transport/ble/io.rs:237-272` — `BluerScanner` struct + `BleScanner` impl — exact pattern: store event stream + adapter, loop in next() filtering for FIPS UUID, convert to BleAddr
  - `src/transport/ble/io.rs:413-453` — `BluerIo::start_scanning()` — shows how scanning is initiated: set discovery filter, start discovery, get events stream, return scanner

  **API/Type References**:
  - `src/transport/ble/io.rs:53-60` — `BleScanner` trait — the contract to satisfy

  **External References**:
  - bluest Adapter::scan: `https://docs.rs/bluest/latest/bluest/struct.Adapter.html#method.scan` — scan API, what it returns, UUID filter parameter
  - bluest Device: `https://docs.rs/bluest/latest/bluest/struct.Device.html` — device identification methods for converting to BleAddr
  - bluest source adapter.rs: `https://github.com/alexmoon/bluest/blob/main/src/corebluetooth/adapter.rs` — check actual scan() return type on macOS

  **WHY Each Reference Matters**:
  - BluerScanner: Shows the loop-and-filter pattern — adapt for bluest (likely simpler since bluest pre-filters by UUID)
  - BluerIo::start_scanning: Shows the init sequence — create filter, start, wrap in scanner
  - bluest scan docs: Need exact return type to declare BluestScanner fields correctly

  **Acceptance Criteria**:
  - [ ] `BluestScanner` implements `BleScanner` with `next()` returning discovered FIPS peers
  - [ ] `BluestIo::start_scanning()` replaces `todo!()` with working implementation
  - [ ] `cargo check --features ble-macos` succeeds on macOS
  - [ ] Scan filtering uses FIPS service UUID `9C90B790-2CC5-42C0-9F87-C9CC40648F4C`

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Scanner compiles and satisfies BleScanner trait
    Tool: Bash
    Preconditions: BluestScanner implemented
    Steps:
      1. Run `cargo check --features ble-macos`
      2. Verify no type errors for Scanner associated type
    Expected Result: Clean compilation
    Failure Indicators: "expected type BluestScanner, found ...", trait bound errors
    Evidence: .sisyphus/evidence/task-7-scanner-check.txt

  Scenario: FIPS UUID constant is correct
    Tool: Bash
    Preconditions: UUID constant defined in bluest.rs
    Steps:
      1. Run `grep -n 'FIPS_SERVICE_UUID\|9c90b790\|9C90B790' src/transport/ble/bluest.rs`
      2. Verify UUID matches 9C90B790-2CC5-42C0-9F87-C9CC40648F4C
    Expected Result: UUID constant found matching FIPS service UUID
    Failure Indicators: UUID mismatch, missing constant
    Evidence: .sisyphus/evidence/task-7-uuid-check.txt

  Scenario: start_scanning no longer uses todo!()
    Tool: Bash
    Preconditions: start_scanning implemented
    Steps:
      1. Run `grep -n 'todo!' src/transport/ble/bluest.rs`
      2. Check that start_scanning is NOT in results (only connect should remain)
    Expected Result: Only connect() has todo!(), start_scanning is implemented
    Failure Indicators: start_scanning still has todo!()
    Evidence: .sisyphus/evidence/task-7-no-todo.txt
  ```

  **Commit**: YES (group 5)
  - Message: `feat(ble): implement BluestScanner with FIPS UUID filtered scanning`
  - Files: `src/transport/ble/bluest.rs`
  - Pre-commit: `cargo check --features ble-macos`

- [ ] 8. Implement BluestIo connect and L2CAP channel establishment

  **What to do**:
  - In `src/transport/ble/bluest.rs`, implement `BluestIo::connect()`:
    - Extract device UUID from `BleAddr` (must be UUID variant from Task 3)
    - Scan for the target device:
      - Start scan with `adapter.scan(&[FIPS_SERVICE_UUID]).await`
      - Iterate scan results to find device matching the target BleAddr UUID
      - OR: Use `adapter.discover_devices(&[FIPS_SERVICE_UUID]).await` if available
      - Add a reasonable timeout (e.g., 30 seconds) using `tokio::time::timeout`
      - If device not found within timeout, return `TransportError::ConnectionRefused("BLE device not found during scan")`
    - Connect to the device: `adapter.connect_device(&device).await`
      - Map connection errors to `TransportError::ConnectionRefused`
    - Open L2CAP channel: `device.open_l2cap_channel(psm, true).await`
      - `true` = encrypted connection
      - Map errors to `TransportError::Io`
    - MTU handling:
      - Request `self.mtu` (2048)
      - If `channel.mtu()` is available, use negotiated value; otherwise use configured value
      - Log debug message with negotiated MTU
    - Construct `BluestStream` from `L2capChannel` + remote `BleAddr` + MTU
    - Replace the `todo!()` placeholder from Task 5
  - Add a helper for device lookup during connect:
    ```rust
    async fn find_device_by_addr(
        adapter: &Adapter,
        target: &BleAddr,
        timeout_secs: u64,
    ) -> Result<Device, TransportError>
    ```

  **Must NOT do**:
  - Do NOT implement bonding/pairing logic — bluest handles pairing transparently
  - Do NOT implement connection pooling — `BleTransport` handles the pool
  - Do NOT add retry logic for connection — upper layer retries
  - Do NOT add keep-alive or heartbeat — upper layer handles that
  - Do NOT change PSM from the passed parameter — use what `BleTransport` provides

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Core L2CAP connection logic, async timeout handling, device discovery flow, error mapping
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (with Tasks 9, 10 — but 9, 10 depend on 8)
  - **Blocks**: Task 9
  - **Blocked By**: Tasks 5, 6

  **References**:

  **Pattern References**:
  - `src/transport/ble/io.rs:362-390` — `BluerIo::connect()` — the pattern: create socket, bind, set MTU, connect, wrap in stream. Bluest is different (no raw sockets) but the flow concept matches: find device → connect → open channel → wrap
  - `src/transport/ble/io.rs:159-176` — `BluerStream::new()` — shows post-connection setup: query MTU, log, store. Replicate for BluestStream

  **API/Type References**:
  - `src/transport/ble/io.rs:81-86` — `BleIo::connect()` signature — must match exactly
  - `src/transport/ble/addr.rs` — `BleAddr` with UUID device variant (from Task 3) — how to extract device identity for matching

  **External References**:
  - bluest Device::open_l2cap_channel: `https://docs.rs/bluest/latest/bluest/struct.Device.html#method.open_l2cap_channel` — signature: `open_l2cap_channel(psm: u16, secure: bool) -> Result<L2capChannel>`
  - bluest Adapter::connect_device: `https://docs.rs/bluest/latest/bluest/struct.Adapter.html#method.connect_device` — how to connect before opening channel
  - bluest Adapter::scan: `https://docs.rs/bluest/latest/bluest/struct.Adapter.html#method.scan` — for device discovery during connect

  **WHY Each Reference Matters**:
  - BluerIo::connect: The flow template — adapt from BlueZ socket model to bluest scan-connect-open model
  - BluerStream::new: How to finalize a connection — MTU query, logging, wrapping
  - bluest docs: Critical for correct API call sequence (scan → connect → open_l2cap_channel)

  **Acceptance Criteria**:
  - [ ] `BluestIo::connect()` replaces `todo!()` with working L2CAP connection logic
  - [ ] Connection flow: scan for device → connect → open L2CAP channel → return BluestStream
  - [ ] Timeout on device discovery (30s default)
  - [ ] `cargo check --features ble-macos` succeeds on macOS
  - [ ] No `todo!()` remains in `bluest.rs`

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Connect implementation compiles
    Tool: Bash
    Preconditions: connect() implemented
    Steps:
      1. Run `cargo check --features ble-macos`
      2. Verify exit code 0
    Expected Result: Clean compilation, no type errors
    Failure Indicators: Type mismatch on L2capChannel, missing imports
    Evidence: .sisyphus/evidence/task-8-connect-check.txt

  Scenario: No todo!() macros remain
    Tool: Bash
    Preconditions: All methods implemented
    Steps:
      1. Run `grep -n 'todo!' src/transport/ble/bluest.rs`
      2. Verify empty output (no todo! macros)
    Expected Result: Zero todo!() macros in bluest.rs
    Failure Indicators: Any remaining todo!()
    Evidence: .sisyphus/evidence/task-8-no-todo.txt

  Scenario: Error types correct for connection failures
    Tool: Bash
    Preconditions: connect() with error mapping
    Steps:
      1. Run `grep -n 'ConnectionRefused\|TransportError::Io\|SendFailed\|RecvFailed' src/transport/ble/bluest.rs`
      2. Verify connect uses ConnectionRefused for connection failures, Io for channel errors
    Expected Result: Appropriate error types used at each failure point
    Failure Indicators: Generic error types, unwrap() calls, panic paths
    Evidence: .sisyphus/evidence/task-8-error-types.txt
  ```

  **Commit**: YES (group 5)
  - Message: `feat(ble): implement L2CAP connection via bluest central role`
  - Files: `src/transport/ble/bluest.rs`
  - Pre-commit: `cargo check --features ble-macos`

- [ ] 9. Wire BluestIo into node transport creation and add integration tests

  **What to do**:
  - **Part A: Node wiring** — In `src/node/mod.rs`, inside the BLE transport creation block (~lines 746-785):
    - Add a new cfg block for macOS BLE:
      ```rust
      #[cfg(all(feature = "ble-macos", not(test)))]
      for (name, ble_config) in ble_instances {
          let transport_id = self.allocate_transport_id();
          let adapter = ble_config.adapter().to_string();
          let mtu = ble_config.mtu();
          match crate::transport::ble::io::BluestIo::new(&adapter, mtu).await {
              Ok(io) => {
                  let mut ble = crate::transport::ble::BleTransport::new(
                      transport_id,
                      name,
                      ble_config,
                      io,
                      packet_tx.clone(),
                  );
                  ble.set_local_pubkey(self.identity.pubkey().serialize());
                  transports.push(TransportHandle::Ble(ble));
              }
              Err(e) => {
                  tracing::warn!(adapter = %adapter, error = %e, "failed to initialize BLE adapter (macOS/bluest)");
              }
          }
      }
      ```
    - This mirrors the existing `#[cfg(all(feature = "ble", not(test)))]` block but uses BluestIo
    - Also update the "feature not enabled" warning to mention both features
  - **Part B: Integration tests** — Create `tests/ble_macos.rs` (or `src/node/tests/ble_macos.rs`):
    - Gate behind `#[cfg(all(target_os = "macos", feature = "ble-macos"))]`
    - Test 1: `BluestIo::new("default", 2048)` succeeds (adapter initialization)
    - Test 2: `start_scanning()` returns Ok (can start scan, even if no peers found)
    - Test 3: `listen(0x0085)` returns `Err(NotSupported)`
    - Test 4: `start_advertising()` returns `Err(NotSupported)`
    - Test 5: `stop_advertising()` returns `Err(NotSupported)`
    - Test 6: `local_addr()` returns a valid BleAddr with UUID device type
    - Test 7: `adapter_name()` returns "default"
    - These tests require macOS with Bluetooth hardware — they won't run in CI but can be run locally
  - **Part C: Verify MockBleIo tests** — Run `cargo test` and confirm all 46 existing tests pass

  **Must NOT do**:
  - Do NOT modify the existing BluerIo wiring block
  - Do NOT change BleConfig or add macOS-specific config options
  - Do NOT add tests that require a Linux FIPS node to be running
  - Do NOT create test utilities that duplicate MockBleIo functionality

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Multi-part task touching node wiring + test creation, needs understanding of both runtime flow and test patterns
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (sequential after Tasks 4, 5, 7, 8)
  - **Blocks**: Task 10
  - **Blocked By**: Tasks 4, 5, 7, 8

  **References**:

  **Pattern References**:
  - `src/node/mod.rs:746-785` — BLE transport creation block — the exact section to add macOS variant, copy structure of BluerIo block
  - `src/node/tests/ble.rs` — Existing 46 MockBleIo tests — test structure, assertion patterns, how BLE tests are organized

  **API/Type References**:
  - `src/transport/ble/io.rs:67-108` — BleIo trait — the API surface being tested
  - `src/transport/ble/bluest.rs` (from Tasks 5-8) — BluestIo public API to test

  **WHY Each Reference Matters**:
  - node/mod.rs:746-785: The wiring point — must add macOS variant without breaking Linux path
  - tests/ble.rs: Test pattern template — follow same structure for macOS tests
  - BleIo trait: Defines what methods to test

  **Acceptance Criteria**:
  - [ ] `src/node/mod.rs` has `#[cfg(all(feature = "ble-macos", not(test)))]` block creating BluestIo
  - [ ] Integration test file exists with 7 tests for BluestIo
  - [ ] `cargo test` (no features) passes — all 46 MockBleIo tests green
  - [ ] `cargo test --features ble-macos` on macOS runs new integration tests
  - [ ] No changes to existing BluerIo wiring block

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Existing 46 MockBleIo tests still pass
    Tool: Bash
    Preconditions: All node wiring changes in place
    Steps:
      1. Run `cargo test --lib` (no BLE features)
      2. Count BLE test results
    Expected Result: 46 BLE tests pass, 0 failures, 0 regressions
    Failure Indicators: Any test failure in ble:: or node::tests::ble modules
    Evidence: .sisyphus/evidence/task-9-mock-tests.txt

  Scenario: macOS integration tests compile
    Tool: Bash
    Preconditions: Integration test file created
    Steps:
      1. Run `cargo test --features ble-macos -- --list 2>&1 | grep ble_macos`
      2. Verify 7 tests are listed
    Expected Result: 7 integration tests listed for ble_macos module
    Failure Indicators: 0 tests found, compilation error
    Evidence: .sisyphus/evidence/task-9-integration-list.txt

  Scenario: Node wiring block is cfg-gated correctly
    Tool: Bash
    Preconditions: node/mod.rs updated
    Steps:
      1. Run `grep -A2 'ble-macos' src/node/mod.rs`
      2. Verify cfg gate references "ble-macos" feature
    Expected Result: cfg(all(feature = "ble-macos", not(test))) block exists
    Failure Indicators: Missing cfg gate, wrong feature name
    Evidence: .sisyphus/evidence/task-9-node-wiring.txt
  ```

  **Commit**: YES (group 6)
  - Message: `feat(ble): wire BluestIo into node transport and add macOS integration tests`
  - Files: `src/node/mod.rs`, `tests/ble_macos.rs`
  - Pre-commit: `cargo test --lib`

- [ ] 10. Full build verification and cross-platform check

  **What to do**:
  - Run complete build verification on macOS:
    - `cargo build --release --features ble-macos` — full release build
    - `cargo clippy --features ble-macos -- -D warnings` — lint check
    - `cargo test --lib` — all unit tests (MockBleIo)
    - `cargo test --features ble-macos` — unit + integration tests
    - `cargo doc --features ble-macos --no-deps` — verify docs generate
  - Cross-platform verification:
    - `cargo check` (no features) — base build still works
    - Verify no macOS-specific code leaks into non-macOS builds by checking cfg gates
  - Fix any issues discovered:
    - Clippy warnings (unused variables, redundant clones, etc.)
    - Missing documentation on public items (if `cargo doc` warns)
    - Unused imports in cfg-gated blocks
  - Final audit:
    - `grep -rn 'todo!\|unimplemented!\|fixme\|FIXME\|HACK' src/transport/ble/bluest.rs` — no leftover TODOs
    - `grep -rn 'unwrap()\|expect(' src/transport/ble/bluest.rs` — no panic paths in production code (expect() OK in tests only)
    - Verify all public types in bluest.rs have doc comments

  **Must NOT do**:
  - Do NOT add new features or behavior — this is verification only
  - Do NOT refactor code that works — fix only actual issues
  - Do NOT add unnecessary doc comments for private items
  - Do NOT change test expectations to make them pass — fix the code

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Verification + minor fixups, no significant code changes
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 3 (final, after Task 9)
  - **Blocks**: F1-F4
  - **Blocked By**: Tasks 2, 4, 9

  **References**:

  **Pattern References**:
  - All modified files from Tasks 1-9 — comprehensive review

  **WHY Each Reference Matters**:
  - This task reviews ALL work — no specific pattern needed, just verification

  **Acceptance Criteria**:
  - [ ] `cargo build --release --features ble-macos` succeeds
  - [ ] `cargo clippy --features ble-macos -- -D warnings` passes
  - [ ] `cargo test --lib` — 0 failures
  - [ ] `cargo test --features ble-macos` — 0 failures
  - [ ] `cargo doc --features ble-macos --no-deps` — no errors
  - [ ] Zero `todo!()`, `unimplemented!()`, `FIXME`, `HACK` in bluest.rs
  - [ ] Zero `unwrap()` in production code (tests OK)

  **QA Scenarios (MANDATORY):**

  ```
  Scenario: Full release build succeeds
    Tool: Bash
    Preconditions: All implementation tasks complete
    Steps:
      1. Run `cargo build --release --features ble-macos`
      2. Verify exit code 0
      3. Verify binary exists at target/release/fips
    Expected Result: Release build completes without errors
    Failure Indicators: Any compilation error, linker error
    Evidence: .sisyphus/evidence/task-10-release-build.txt

  Scenario: Clippy clean
    Tool: Bash
    Preconditions: All code finalized
    Steps:
      1. Run `cargo clippy --features ble-macos -- -D warnings`
      2. Verify exit code 0
    Expected Result: Zero warnings, zero errors
    Failure Indicators: Any clippy warning (treated as error with -D)
    Evidence: .sisyphus/evidence/task-10-clippy.txt

  Scenario: No leftover TODOs or panics
    Tool: Bash
    Preconditions: bluest.rs finalized
    Steps:
      1. Run `grep -n 'todo!\|unimplemented!\|FIXME\|HACK' src/transport/ble/bluest.rs`
      2. Run `grep -n 'unwrap()' src/transport/ble/bluest.rs`
      3. Verify both return empty
    Expected Result: Zero leftover markers, zero unwrap() calls
    Failure Indicators: Any match found
    Evidence: .sisyphus/evidence/task-10-audit.txt

  Scenario: Default build (no features) unaffected
    Tool: Bash
    Preconditions: All changes complete
    Steps:
      1. Run `cargo check`
      2. Verify exit code 0
    Expected Result: Base build compiles as before
    Failure Indicators: Any new compilation error
    Evidence: .sisyphus/evidence/task-10-default-build.txt
  ```

  **Commit**: YES (group 7)
  - Message: `chore(ble): fix clippy warnings and verify macOS BLE build`
  - Files: any files with fixups
  - Pre-commit: `cargo clippy --features ble-macos -- -D warnings && cargo test --lib`

---

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.
>
> **Do NOT auto-proceed after verification. Wait for user's explicit approval before marking work complete.**

- [ ] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, check compilation). For each "Must NOT Have": search codebase for forbidden patterns — reject with file:line if found. Check evidence files exist in .sisyphus/evidence/. Compare deliverables against plan.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [ ] F2. **Code Quality Review** — `unspecified-high`
  Run `cargo clippy`, `cargo test`. Review all changed files for: `as any` equivalent patterns, empty error handling, commented-out code, unused imports. Check AI slop: excessive comments, over-abstraction, generic names (data/result/item/temp). Verify error mapping is concise (not per-error-code).
  Output: `Build [PASS/FAIL] | Clippy [PASS/FAIL] | Tests [N pass/N fail] | Files [N clean/N issues] | VERDICT`

- [ ] F3. **Real Manual QA** — `unspecified-high`
  Start from clean state. Build FIPS with BLE feature on macOS. Verify BluestIo initializes without crash. Test scanning (even if no peers found, verify no panic). Test that peripheral stubs return NotSupported. Run all 46 MockBleIo tests. Save evidence.
  Output: `Build [PASS/FAIL] | Init [PASS/FAIL] | Scan [PASS/FAIL] | Stubs [PASS/FAIL] | Mock Tests [N/N] | VERDICT`

- [ ] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual diff (git log/diff). Verify 1:1 — everything in spec was built (no missing), nothing beyond spec was built (no creep). Check "Must NOT do" compliance. Detect cross-task contamination. Flag unaccounted changes.
  Output: `Tasks [N/N compliant] | Contamination [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

| Commit | Message | Files | Pre-commit |
|--------|---------|-------|------------|
| 1 | `feat(ble): add bluest dependency and ble-macos feature flag` | Cargo.toml | `cargo check` |
| 2 | `refactor(ble): widen platform gates from linux-only to linux+macos` | transport/mod.rs, node/mod.rs, node/lifecycle.rs | `cargo check` |
| 3 | `feat(ble): extend BleAddr with UUID-based device identification` | transport/ble/addr.rs | `cargo test --lib transport::ble::addr` |
| 4 | `feat(ble): add BluestIo as macOS BLE backend with central role` | transport/ble/bluest.rs, transport/ble/mod.rs, transport/ble/io.rs | `cargo test --lib transport::ble` |
| 5 | `feat(ble): implement L2CAP scanning and connection via bluest` | transport/ble/bluest.rs | `cargo test --lib transport::ble` |
| 6 | `feat(ble): wire BluestIo into node transport creation` | node/mod.rs, node/tests/ | `cargo test` |
| 7 | `test(ble): add macOS BLE integration tests` | tests/ or node/tests/ | `cargo test` |

---

## Success Criteria

### Verification Commands
```bash
cargo check --features ble-macos                    # Expected: compiles on macOS
cargo check --features ble                          # Expected: compiles on Linux (unchanged)
cargo test --lib                                    # Expected: all tests pass including 46 MockBleIo
cargo test --lib transport::ble::bluest             # Expected: BluestIo unit tests pass
cargo clippy --features ble-macos                   # Expected: no warnings
```

### Final Checklist
- [ ] All "Must Have" items present
- [ ] All "Must NOT Have" items absent
- [ ] All 46 MockBleIo tests pass
- [ ] New BluestIo unit tests pass
- [ ] `cargo clippy` clean
- [ ] macOS build with BLE compiles
- [ ] Linux build with BLE still compiles (unchanged)
