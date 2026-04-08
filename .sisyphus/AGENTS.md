# FIPS Project - Agent Coordination

## Last updated: 2026-04-08 (session ses_296a590)

## Active session

**ses_296a590** — working on `macos-support` branch. This is the primary active session.

## What was done (Apr 7-8)

### Completed fixes (committed and pushed to `origin/macos-support`)

1. **Cherry-pick: LeRandom address type** (`6c103e1`) — already applied
2. **Cherry-pick: disable_tiebreaker config** (`ac9727c`) — already applied
3. **Fix #20: BLE disconnect notification → session state reset** (`516fe87`) — TransportDisconnect channel from BLE transport to Node layer. Immediately resets Noise session on L2CAP disconnect instead of waiting 30s for heartbeat timeout.
4. **Fix: stale peer detection before re-handshake** (`72979b8`) — upstream commit, merged via rebase
5. **Fix: stale outbound socket on cross-connection** (`ea375c7`) — close stale outbound transport socket when cross-connection is detected
6. **Fix: block outbound pool insert after cross-connection** (`d032728`) — prevent stale outbound from re-inserting into BLE connection pool
7. **Fix #17: per-peer competing MSG1 rate limit** (`808214c`) — MAX_COMPETING_MSG1 (3) per-peer counter, drops connection after threshold
8. **Fix: peer static pubkey verification** (`808214c`) — reject MSG1 from unconfigured peers when peers list is configured

### Issues updated/closed

| Issue | Status | Notes |
|-------|--------|-------|
| #7 | Closed | LeRandom cherry-pick already applied |
| #8 | Closed | disable_tiebreaker cherry-pick already applied |
| #19 | Closed | Superseded by ea375c7 + d032728 |
| #20 | Closed | Fixed by 516fe87 |
| #17 | Left open | Mitigated by disconnect notification + MSG1 limit; exponential backoff not yet implemented |
| #13 | Left open | Partially addressed by MSG1 limit + pubkey verification |

### Binary deployed

- `/usr/local/bin/fips` — rev `808214cf80` (release, `--features ble`)
- Service: `sudo systemctl restart fips`

## Previous session (ses_298825e, Apr 7)

That session worked on the same issues (#19 cross-connection fix, stale session state) and implemented `blocked_outbound_addrs` in `src/transport/ble/mod.rs`. Their approach is complementary to ours:

- **Their approach**: `blocked_outbound_addrs` set prevents the outbound background task from re-inserting into the BLE pool after cross-connection detection
- **Our approach**: Disconnect notification channel immediately resets Node session state on L2CAP drop

Both solve different aspects. Our session's changes were built on a clean merge with upstream and supersede the earlier work. There is no conflict — their `blocked_outbound_addrs` code was not cherry-picked (it was in a dirty working state that got cleaned up during the Apr 7→Apr 8 transition).

If that session resumes, it should `git pull` to get the latest state before making any changes. Do NOT re-implement the `blocked_outbound_addrs` threading — our approach solves the problem differently and the codebase has moved on.
