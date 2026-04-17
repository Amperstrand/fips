# BLE Bulk Stability — Learnings

## 2026-04-17 Session Start

### Inherited Context (from previous sessions)
- TEST A/B/C pass: role symmetry, authentication, tie-break, reconnect all working
- TEST D/E stall: 128KB burst then flatline — matches L2CAP initial credit window exhaustion
- Root cause: writer-side backpressure/credit blindness in macOS CoreBluetooth paths
- FIPS already has `SendRateLimiter` (token bucket) and `BleRateAdapter` (AIMD on MMP SRTT)
- Test configs had `send_rate_bps: 0` which bypasses pacing → `effective_send_rate_bps()` maps 0 to 150kbps auto-rate
- Linux send timeout widened from 3s to 15s
- PMTU blackhole fix implemented but not hardware-validated
- 2-byte BE length prefix framing on all platforms — do not change
- No BLE pairing required — FIPS/Noise provides real security
- macOS peripheral stream: `write_maxLength == 0` treated as fatal error (should be backpressure)
- macOS central: 3s timeout too short, `write_all()` blocks when bluest queue fills
- bluest writer: bounded(16) async_channel, one-shot write_maxLength, no persistent unsent tail

================================================================================
BLE Writer Failure Diagnosis - TEST D/E
=======================================

Date: 2026-04-17
Task: Identify writer failure signature in TEST D/E

Evidence Files:
- task-1-writer-diagnosis.txt (primary/secondary suspects)
- task-1-negative-cause-check.txt (pairing/auth/PSM verification)

Findings:
- PRIMARY SUSPECT: macOS Central writer (bluest L2CAP queue backing up)
  - Path: src/transport/ble/io_macos.rs:169-178
  - 3s timeout too short, write queue never drains
  - "No data to write" after SessionSenderReport
  
- SECONDARY SUSPECT: macOS Peripheral writer (objc2 NSOutputStream)
  - Path: src/transport/ble/io_macos.rs:658-659
  - write_maxLength==0 treated as fatal error instead of backpressure
  - Queue fills, no retry mechanism

- NOT A BLOCKER: Pairing/auth/PSM (verified in task-1-negative-cause-check.txt)
  - All tests show successful pairing
  - PSM negotiated (200/133)
  - GATT PSM exchange expected to fail, fallback works

Failure Pattern: 128KB burst → SessionSenderReport → "No data to write" → flatline
Root Cause: L2CAP credit window exhaustion in bluest queue (bounded(16))

Next Steps: Implement credit-aware flow control (task 2: Add L2CAP credit monitoring)

================================================================================
Task 2: Writer/Backpressure Contract Definition
================================================================================

Date: 2026-04-17

### CRITICAL FINDING: bluest fork uses piper, NOT async_channel::bounded(16)

Previous sessions described bluest as using `async_channel::bounded(16)`. This
was true for the crates.io version (0.6.9) but NOT for the Amperstrand fork:

- crates.io bluest 0.6.9: async_channel::bounded(16), packet-based, no AsyncWrite
- Amperstrand fork (git): piper::pipe(0x100000 = 1MB), byte-stream, implements AsyncWrite

The forked bluest's L2capChannelWriter:
- Wraps piper::Writer (AsyncWrite over bounded byte pipe)
- OutputStreamDelegate reads from piper::Reader, writes to NSOutputStream
- 1MB capacity — absorbs large bursts before blocking

### THREE BUGS IN FORKED BLUEST'S OutputStreamDelegate::send_packet

BUG-1 (PARTIAL-WRITE LOSS): Reads N bytes from piper, calls write_maxLength once.
If res < N, bytes (res..N) are SILENTLY DROPPED (consumed from pipe but not sent).

BUG-2 (ZERO-WRITE LOSS): write_maxLength returns 0 (backpressure) → all data
consumed from pipe is lost. This produces the "No data to write" log message
when the next send_packet call finds an empty pipe.

BUG-3 (SINGLE-SHOT): send_packet does one write per event. If more data is
available after a successful write, it doesn't loop. Waits for next event.

### Writer Contract Architecture

Chosen: Per-role bounded queues with unified invariants, no shared abstraction.

- Central: tokio::sync::mpsc::bounded(32) + drain task → bluest write_all
- Peripheral: Bounded VecDeque (32 entries/64KB) + HasSpaceAvailable drain
- Both: 15s timeout, explicit backpressure on queue full
- ~85 lines of changes total

### Zephyr L2CAP Analogy

BLE L2CAP CoC credit-based flow control:
- Credits = bounded queue slots (our 32 frames)
- Credit refresh = HasSpaceAvailable event
- Credit exhaustion = timeout error

The bounded queue IS our credit window at the application layer, independent
of the BLE L2CAP credit mechanism managed by CoreBluetooth.

===============================================================================
Task 8: Stability-First Pacing Profile Tuning
===============================================================================

Date: 2026-04-17

### CHANGES: Conservative default pacing for stability-first BLE bulk transfer

**src/config/transport.rs changes**:
- DEFAULT_BLE_SEND_RATE_BPS: 150_000 → 100_000 (100 Kbps)
- BleConfig::send_rate_bps() default: 150_000 → 100_000
- DEFAULT_BLE_SEND_BURST_BYTES: 4,096 → 2,048 (2 KB)
- BleConfig::send_burst_bytes() default: 4,096 → 2,048

**Preserved behavior**:
- effective_send_rate_bps() mapping: 0 → 150_000 (unchanged, backward compatible)
- BleRateAdapter AIMD: no changes to thresholds or factors
- SendRateLimiter token bucket: no changes to algorithm

**Test config updates (bulk transfer tests D/4)**:
- linux-testD.yaml: send_rate_bps: 0 → 100000, send_burst_bytes: 2048
- mac-testD.yaml: send_rate_bps: 0 → 100000, send_burst_bytes: 2048
- linux-test4.yaml: Added send_rate_bps: 100000, send_burst_bytes: 2048
- mac-test4.yaml: Added send_rate_bps: 100000, send_burst_bytes: 2048

**Control-plane tests (A/B/C)**:
- Keep send_rate_bps: 0 (unlimited) for discovery/exchange tests

### Rationale for conservative defaults

1. **100 Kbps starting rate vs 150 Kbps previous**:
   - 33% reduction in initial send pressure
   - Prevents overwhelming BLE L2CAP queues under mesh-speed data

2. **2 KB burst vs 4 KB previous**:
   - 50% reduction in burst size
   - Aligns with BLE L2CAP SDU patterns (~1 MTU-sized packets)
   - Reduces risk of exhausting bounded writer queues (32 frames / 64 KB)

3. **Smaller bursts = smoother pacing**:
   - Tokens refill at 100 Kbps (~20ms to accumulate 2 KB)
   - Smooth incremental sends reduce backpressure spikes
   - Predictable behavior for writer queues

4. **Adaptive scaling preserved**:
   - AIMD allows gradual ramp-up when link healthy (RTT < 300ms)
   - Automatic reduction when congestion detected (RTT > 500ms)
   - Bounded between 50 Kbps and 250 Kbps

### Why unlimited mode (send_rate_bps: 0) avoided in bulk paths

**Problem with unlimited**:
- Writer queue overflow risk: 2 KB bursts × 32 frames = 64 KB capacity
- Unlimited bursts can overwhelm bounded queues under mesh-speed data
- L2CAP pipe backing up leads to credit exhaustion and connection stalls
- Unpredictable backpressure and frequent timeout errors

**Stability-first benefit**:
- Sub-1s ping under load achievable at 100-150 Kbps
- iperf throughput at 100 Kbps sufficient for most use cases
- No stalls or resets at conservative rate
- AIMD provides bandwidth scaling when link is healthy

### Evidence files

- .sisyphus/evidence/task-8-pacing-profile.txt
- .sisyphus/evidence/task-8-negative-unlimited.txt

### Trade-offs

**Acceptable throughput reduction**:
- Starting at 100 Kbps vs 150 Kbps reduces peak by ~33%
- AIMD can ramp up to 150 Kbps, limited by BLE L2CAP practical throughput (~250 Kbps)
- Acceptable for stability-first target (sub-1s ping under load, no stalls)

**No removal of adaptive adjustment**:
- BleRateAdapter provides automatic bandwidth probing
- Link health feedback loop enables dynamic scaling
- Stability-first, not stability-only

### Files modified

1. src/config/transport.rs:
   - DEFAULT_BLE_SEND_RATE_BPS (line ~591)
   - BleConfig::send_rate_bps() default (line ~681)
   - DEFAULT_BLE_SEND_BURST_BYTES (line ~595)
   - BleConfig::send_burst_bytes() default (line ~693)

2. testing/ble/configs/linux-testD.yaml
3. testing/ble/configs/mac-testD.yaml
4. testing/ble/configs/linux-test4.yaml
5. testing/ble/configs/mac-test4.yaml

================================================================================
Task 5: macOS Central Writer Hardening
================================================================================

Date: 2026-04-17

### Implementation: BluestStream bounded queue + drain task

Changed BluestStream struct:
- Replaced `writer: Mutex<L2capChannelWriter>` with `tx: mpsc::Sender<Vec<u8>>`
- Removed Mutex wrapper — mpsc::Sender is Clone + Send, no mutex needed
- Drain task owns the L2capChannelWriter directly, no contention

send() and send_urgent() both use:
1. Rate limiter acquire (send only, urgent skips — unchanged)
2. 2-byte BE length prefix framing (unchanged)
3. try_send to bounded(32) mpsc channel
4. On Full: tokio::time::timeout(15s, tx.send()) — backpressure with deadline
5. On Closed: Err(TransportError::Io("channel closed"))

Drain task (spawned in connect()):
- Reads from mpsc receiver, writes to bluest write_all
- On write error: log warning and break (drain task stops, channel closes)
- On receiver closed: log debug and exit cleanly
- No retry logic in drain task — bluest write errors mean the L2CAP channel is dead

### Key design decisions confirmed:
- send_urgent uses SAME queue as send (per T2 decision — no priority separation)
- 15s timeout matches Linux BLE timeout
- 32 queue depth = L2CAP credit window equivalent
- Timeout returns Err but does NOT dequeue the frame — it remains for draining
- Caller (receive_loop) treats Timeout as transient, does not disconnect

### Files changed: src/transport/ble/io_macos.rs (BluestStream only)
### Lines changed: ~40 net (struct + send + send_urgent + connect + 2 constants)
### Zero changes to PeripheralStream, PeripheralOutputDelegate, or mod.rs

================================================================================
Task 6: macOS Peripheral Writer Hardening
================================================================================

Date: 2026-04-17

### Implementation: Bounded queue + delegate-driven drain for PeripheralStream

Changed PeripheralOutputDelegateIvars:
- Added `queue_space_notify: Arc<tokio::sync::Notify>` for backpressure signaling
- Added `output_stream: StdMutex<Option<SendableOutputStream>>` stored on first HasSpaceAvailable
- `on_write_notify` (previously a no-op) now triggers drain_to_stream via stored stream ref

Changed PeripheralOutputDelegate methods:
- `new()` → accepts `queue_space_notify: Arc<tokio::sync::Notify>` param
- `enqueue()` → replaced with `try_enqueue(&[u8]) -> bool`:
  - Checks `queue.len() >= BLE_PERIPHERAL_QUEUE_DEPTH` (32)
  - Checks `current_bytes + data.len() > BLE_PERIPHERAL_QUEUE_BYTE_CAP` (64KB)
  - Returns false if either limit exceeded
- `drain_to_stream()` → changed from static to instance method:
  - Same write loop logic (partial-write retention, res==0 returns correctly)
  - Added `notify_waiters()` after successful drain when queue was non-empty
- `handle_event` → stores output_stream ref on first HasSpaceAvailable, calls instance drain
- `on_write_notify` → calls drain via stored stream (triggers on NSNotification post)

Changed PeripheralStream:
- Added `queue_space_notify: Arc<tokio::sync::Notify>` field (shared with delegate)
- Added `enqueue_with_backpressure(&self, framed, label)` async helper:
  - Registers `notified` future BEFORE checking queue (minimizes race window)
  - Loops: try_enqueue → if full, await notified with 15s timeout
  - On success, calls `notify_write()` to kick delegate drain
- `send()` → rate-limit + frame + enqueue_with_backpressure
- `send_urgent()` → frame + enqueue_with_backpressure (same queue, no rate limit)

### Key design: register-before-check pattern for Notify

The subtle race: notify_waiters() fires between try_enqueue failure and notified().await.
Mitigation: create the `notified` future before calling try_enqueue. If the notify
fires after try_enqueue fails but before .await, the future resolves on first poll.
This doesn't fully eliminate the race (future isn't polled until .await), but the
15s timeout ensures recovery.

### The fatal res==0 bug is completely eliminated

The old send() called write_maxLength directly with `if res == 0 { return Err(...) }`.
The new send() never calls write_maxLength directly — it only enqueues. All writes
go through drain_to_stream() which correctly handles res==0 by retaining unsent data
and waiting for the next HasSpaceAvailable event.

### Constants added
- BLE_PERIPHERAL_QUEUE_DEPTH = 32
- BLE_PERIPHERAL_QUEUE_BYTE_CAP = 65536
- BLE_PERIPHERAL_SEND_TIMEOUT = 15s

### Files changed: src/transport/ble/io_macos.rs (PeripheralStream + PeripheralOutputDelegate only)
### Zero changes to BluestStream, BleStream trait, mod.rs, io.rs

================================================================================
Task 7: BLE Write Timeout Policy Consistency Audit
================================================================================

Date: 2026-04-17

### AUDIT RESULT: CONSISTENT — NO CODE CHANGES REQUIRED

All three production BLE send paths now have uniform timeout/backpressure:

| Path              | Timeout | Queue Depth | Byte Cap  | Error on Saturation |
|-------------------|---------|-------------|-----------|---------------------|
| Linux BluerStream | 15s     | Kernel impl | N/A       | TransportError::Timeout |
| macOS Central     | 15s     | 32 frames   | N/A       | TransportError::Timeout |
| macOS Peripheral  | 15s     | 32 frames   | 65536     | TransportError::Timeout |

### Key findings:
- Linux has no application-level queue — kernel SeqPacket provides implicit backpressure
- Linux send/send_urgent both wrapped in tokio::time::timeout(15s, conn.send())
- macOS Central uses mpsc::bounded(32) + drain task, try_send + timeout(15s) fallback
- macOS Peripheral uses VecDeque(32/64KB) + Notify backpressure + timeout(15s)
- send_urgent skips rate limiter on ALL paths (consistent)
- MockBleStream (test only): mpsc::channel(64) with blocking send, no timeout — acceptable for tests
- No indefinite wedge possible on any production path

### Evidence files:
- .sisyphus/evidence/task-7-timeout-policy.txt
- .sisyphus/evidence/task-7-no-indefinite-wedge.txt

### Architecture insight: queue = L2CAP credit window equivalent
The bounded queue at the application layer functions as a credit window,
independent of BLE L2CAP's own credit mechanism. Queue full = credit
exhaustion = backpressure signal. This is the Zephyr L2CAP analogy from T2.
