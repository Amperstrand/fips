# BLE Bulk Stability — Unresolved Problems

## 2026-04-17 Session Start

### Primary Problem
BLE bulk transfer stalls after ~128KB in both directions. All control-plane operations work.

### Suspects (from Oracle + Metis analysis)
1. `PeripheralStream::send()` treats `write_maxLength == 0` as fatal (should be backpressure)
2. `PeripheralStream::send()` has no timeout (blocks indefinitely in spawn_blocking)
3. `BluestStream::send()` has only 3s timeout (too short under congestion)
4. No partial-write resilience in central path
5. bluest's bounded(16) queue fills → `write_all()` stalls

### Constraints
- Cannot patch bluest crate directly
- Must preserve 2-byte BE framing
- Must preserve role symmetry (both platforms play both roles)
