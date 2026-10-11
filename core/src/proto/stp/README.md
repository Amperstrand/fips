# core/src/proto/stp

The spanning tree: how nodes elect a root, choose parents and derive
the tree coordinates that routing and lookups use. A node announces its
parent declaration and ancestry to its peers; this module ranks and
classifies those announcements and decides when to re-parent, when to
announce and when to hold down a flapping parent. The root crate's tree
handlers sign declarations, send announcements and drive this state.

## Files

- `mod.rs`: declares the files, re-exports their public items and
  `TreeCoordinate` and `CoordEntry` from `crate::proto::coord`, and
  defines `TreeError`.
- `core.rs`: `Stp`, the classification of received and periodic
  announcements.
- `state.rs`: `TreeState`, with parent evaluation, root election and
  next-hop lookup.
- `declaration.rs`: `ParentDeclaration`, a node's signed announcement of
  its parent, and the bytes its signature covers.
- `limits.rs`: the flap-dampening and hold-down state machine.
- `wire.rs`: `TreeAnnounce` and `validate_semantics`.
- `tests/`: the unit tests; its README describes it.

## Invariants

- **No signing here.** Declarations carry a signature this module
  treats as bytes; the root crate signs with the node's key and checks
  signatures (`node::tree`).
- **Ranking and the announcement format are protocol.** Every node must
  rank parents the same way and decode the same bytes, so neither
  changes on the deployed lines.
