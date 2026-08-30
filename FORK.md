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
