# core/src/proto/lookup

Mesh lookups: how a node finds a destination's tree coordinates by
asking its tree peers, and how it forwards, answers and rate-limits the
requests it receives. The root crate's lookup handlers decode requests,
call the planners here through the `RoutingView` trait, and send what
they return.

## Files

- `mod.rs`: declares the files and re-exports their public items.
- `core.rs`: the `RoutingView` trait through which the root crate shows
  its peers and tree, the `plan_forward` and `plan_initiate` planners,
  their `LookupAction` and `ForwardOutcome` results, and the dedup cache
  capacity policy.
- `state.rs`: `Lookup`, which groups the recent-request dedup cache,
  the lookups in flight, the originator's backoff and the transit
  forward rate limiter.
- `limits.rs`: `LookupBackoff`, the forward rate limiter, and the caps
  on the dedup cache and the backoff table.
- `wire.rs`: `LookupRequest` and `LookupResponse` and their codecs.
- `tests/`: the unit tests; its README describes it.

## Invariants

- **Tables a remote party fills are bounded.** The dedup cache and the
  backoff table have fixed caps (`MAX_RECENT_LOOKUP_REQUESTS`,
  `MAX_BACKOFF_ENTRIES`) that requests cannot raise.
- **The request and response formats are wire format** and do not
  change on the deployed lines.
