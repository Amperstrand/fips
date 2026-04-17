# BLE Bulk Stability — Decisions

## 2026-04-17 Session Start

### Key Decisions from Previous Sessions
- Stability-first approach: modest throughput acceptable, no stalls
- 2-byte BE prefix framing kept on all platforms (ESP32 firmware requires it)
- No BLE pairing — FIPS/Noise is the security layer
- Wave-based execution: diagnosis → hardening → focused tests → full campaign
- Do not fork/patch bluest crate directly — harden at integration layer

## 2026-04-17 Task 2: Writer Contract

### Decision: Per-role bounded queues (no shared abstraction)
- Central: tokio::sync::mpsc::bounded(32) wrapping bluest's AsyncWrite
- Peripheral: Bounded VecDeque (32 entries / 64KB) replacing unbounded queue
- Rationale: Each role has different transport API (AsyncWrite vs raw NSOutputStream).
  Shared trait adds complexity without benefit. Same invariants, different shapes.

### Decision: 32-frame / 64KB queue depth
- 32 frames ≈ 64KB at average FMP frame size
- Drains in ~2s at 250kbps BLE throughput
- Matches L2CAP CoC credit window convention (16-32 SDUs)
- Small enough to avoid memory pressure, large enough for routing bursts

### Decision: 15-second send timeout (both roles)
- Matches Linux BLE timeout (proven stable)
- 4× safety margin over expected drain time (~3.4s at 150kbps)
- Task 3 stability profile recommends 15s for all paths

### Decision: write_maxLength==0 is backpressure, not fatal error
- Peripheral path currently treats write_maxLength==0 as fatal (line 658)
- Must change to: return and wait for next HasSpaceAvailable event
- Retain unsent data in queue (existing drain_to_stream logic is correct)

### Decision: Harden at integration layer, accept bluest bugs as documented
- bluest OutputStreamDelegate has 3 data-loss bugs (partial-write, zero-write, single-shot)
- FIPS queue acts as buffer between protocol engine and bluest transport
- Small frame sizes (< MTU) mitigate partial-write bug in practice
- Future task may patch bluest directly

### Decision: No priority queue for send_urgent
- send_urgent uses same bounded queue as send
- Urgent frames are rare and small (peer wire messages)
- Same timeout applies — if queue is full, urgent waits too
- Simplicity over optimization; can add priority later if needed
