# core/src/proto/probe

The diagnostic path probe: a four-stage machine (resolve, path,
session, round-trip time) that an operator starts against a destination
and that advances one step per receive-loop tick. The root crate's
probe handler feeds it observations and its control socket shows the
report.

## Files

- `mod.rs`: declares the files and re-exports their public items.
- `core.rs`: `Probe`, the stage machine, its `Observation` input and
  `ProbeAction` output, and `describe_path`, the common-ancestor
  arithmetic over two tree coordinates.
- `state.rs`: the stage, verdict and reason vocabulary and the plain
  preflight and report snapshots.
- `limits.rs`: per-stage time floors and tick counts, and the cap on
  concurrent probes (`MAX_CONCURRENT_PROBES`).
- `tests/`: the unit tests; its README describes it.

## Invariants

- **Bounded.** The number of probes in flight is capped.
- **Diagnostic only.** Nothing here changes routing or session state; a
  probe reads what the node already knows and reports it.
