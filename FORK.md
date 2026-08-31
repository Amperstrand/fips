# Fork policy (decided 2026-08-30)

- `master` is a **protected, pure mirror of jmcorgan/fips:master**. Nothing
  of ours ever lands there — no merges, no direct commits. It is updated
  only by fast-forwarding to upstream (`git fetch upstream && git push
  origin upstream/master:master`), which branch protection permits; force
  pushes and deletions are blocked, linear history enforced.
- All Amperstrand work lives on `fork/main` (this branch) and topic branches
  off it. When upstream releases, we study it, check whether any of our
  changes now have better upstream solutions, and **rebase what we still
  want** onto the fresh mirror — rebases only, never merges into the mirror.
- Current fork line (rebased 2026-08-30 onto v0.5.0 / 66f0de8a): the BLE
  learned-address-type dial fix (PR-ready — Amperstrand/fips#151, held by
  decision), fork-local CI (tests+clippy, cargo-deny, libdbus), and
  secret-detection hooks. The merge-based sync workflow was dropped in this
  rebase — the mirror policy replaces it.
- Upstream state at mirror time: v0.5.0 released, master opened at
  0.6.0-dev, plus a native-datagram EOF fix (bc809c47). BLE untouched — our
  fix rebases cleanly (verified; only the old sync workflow conflicts).

## 2026-08-31 Fork Cleanup — What Happened, What We Learned

### Branch namespace: 74 → 12 refs

Deleted 24 dead branches (the full pre-upstream-merge BLE reliability
era: ble-transport-reliability v1/v2/v3, linux-ble-stability-v2 +99,
fix/ble-yield-disconnect +121, the macOS rescue/rebuild/integrate stack,
golden-vectors, openwrt-apk, maint, old main, ai-experiments, copilot/*
— all superseded by upstream's own evolution or merged into fork/main).

Kept: master (pure mirror, protected), fork/main (work line), 3
bench-testable branches (fix/ble-rssi-priority, fix/ble-rekey-recovery-103,
feat/tollgate-peer-policy), 2 build-infra branches (feature/mipsel ×2),
1 niche (feature/rfcomm-transport), 4 archive/backup refs.

### Issues: 30 open → 10 open

Closed 20 issues that were either:
- Pre-upstream-merge BLE analysis (RTT degradation, rekey cascades,
  throughput loss — all against code that no longer exists)
- Superseded by upstream v0.5.0's BLE pool architecture
- Label-workflow artifacts
- Salvage issues whose branches are preserved and ready for rebasing

### What upstream taught us (v0.5.0 vs our pre-merge work)

1. **The BLE pool with priority eviction** (upstream) replaced our
   ad-hoc connection management. Our old branches were trying to solve
   connection-limit and eviction problems that upstream solved
   architecturally.

2. **Bounded handshake concurrency** — upstream runs each inbound BLE
   handshake off the accept loop, bounded. Our branches were patching
   accept-loop blocking that no longer exists.

3. **Rekey recovery is built in** — upstream's `a3ea2245` handles the
   abandoned-rekey-index + ACL-rejected-dial case our branches targeted.

4. **The native-datagram EOF fix** (`bc809c47`) — a queued datagram
   being discarded as end-of-file — was a bug we never saw because we
   don't use the native API path, but it shows upstream's IPC layer
   is actively maintained.

### How things have improved since the last extensive tests (~2026-05-04)

| Metric | Then (0.4.x-era, pre-merge) | Now (v0.5.0 + our fix) |
|---|---|---|
| BLE L2CAP on D0WD | Peripheral-only fallback, no scan discovery | Full central+peripheral dual-role, IK handshake, sustained heartbeats (ETX 1.0, 0% loss) |
| BLE L2CAP on S3 | Broken (role deadlock misdiagnosed as daemon churn) | Same as D0WD — full chain verified 2026-08-31 |
| WiFi + mDNS | Working but fragile (pin mismatches, no retry) | mDNS-pinned discovery with self-healing on both reconnect and daemon-restart paths |
| ESP-NOW | Single transport, no hybrid | Full hybrid with session-windowed probing (fixed #158), retained-channel fast-start (#167) |
| HTTP over FIPS | Never E2E-tested | Full chain verified on STM32 (#12 closed) |
| Identity management | Hand-copied hex keys, silent failures | Registry-programmatic + build-time validation (garbage=error, mismatch=warning) |
| Toolchain | 8 versions, 22GB | 6 versions, 12GB, policy-documented |
| Upstream sync | 16-ahead/7-behind fork drift | Pure mirror (protected), work on fork/main, rebases only |
| Test coverage | Manual, ad-hoc | 2399 tests green, fips-lab regression scenarios, bench scripts for E2E |
| Config format | Old (identity, transports at root) | v0.5.0 (node.identity.nsec bech32, node.control.socket_path) |

### What we learned about debugging from this cleanup

The biggest lesson: **the S3 L2CAP "daemon churn" theory was wrong**.
We spent a session blaming the concurrent agent's daemon restarts for
the S3's failure to complete handshakes. The real bug: our own S3 bin
still had `peer_sent_first=true` (the D0WD fix in b3518b3 never
propagated to the S3 bin). The evidence chain that cracked it:
stable daemon → clean probe → exchange OK → "peer sent MSG1 first,
entering responder path" → mutual deadlock → 45s timeout → reset.
One word (`false`) fixed it.

**The meta-lesson: when two code paths share a fix, apply it to ALL
of them.** The D0WD bin and S3 bin are near-identical files, but the
fix only went to one. A grep for `peer_sent_first.*true` would have
found the second instance in seconds.
