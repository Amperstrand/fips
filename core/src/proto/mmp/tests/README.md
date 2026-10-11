# core/src/proto/mmp/tests

Unit tests for the metrics protocol, declared from `mmp/mod.rs` as
`#[cfg(test)] mod tests;`.

- `mod.rs`: declares the test files.
- `algorithms.rs`: the estimators and the spin bit.
- `core.rs`: the `plan_*` decisions.
- `path_mtu.rs`: the path-MTU state machine.
- `state.rs`: the aggregates, and `MmpMode`'s configuration spelling
  through `serde_yaml`.
- `wire.rs`: the report codecs.
