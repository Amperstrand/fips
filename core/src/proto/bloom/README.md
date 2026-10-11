# core/src/proto/bloom

Reachability bloom filters. Each node keeps a filter per peer saying
which destinations are reachable through that peer, merges them into
the filter it announces, and routes with them before it knows a
destination's coordinates. The root crate's tree and bloom handlers
decode announcements, call `BloomState`, and send what it returns.

## Files

- `mod.rs`: declares the files and re-exports their public items.
- `core.rs`: `BloomFilter`, the bit array with its insert, merge and
  query operations and its fill ratio and false-positive estimate.
- `state.rs`: `BloomState`, the per-peer inbound filters, the outgoing
  filter computation and the decision of when to send an update.
- `limits.rs`: the filter size, the size classes and the hash count.
- `wire.rs`: `FilterAnnounce` and its codec.
- `tests/`: the unit tests; its README describes it.

## Invariants

- **Identical arithmetic everywhere.** The false-positive estimate uses
  `crate::proto::math::powi` and `libm::log` rather than `f64` methods,
  so every platform computes the same bits. The estimate feeds a reject
  comparison that nodes must agree on.
- **The filter size, hash count and hashing are wire format.** Nodes
  test each other's filters, so all three must match across the mesh.
- **Received filters are untrusted.** `FilterAnnounce::decode` rejects
  any size class but the v1 one, so a peer cannot choose the size of the
  filter this node stores for it.
