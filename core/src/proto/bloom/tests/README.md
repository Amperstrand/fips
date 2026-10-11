# core/src/proto/bloom/tests

Unit tests for the bloom filter subsystem, declared from `bloom/mod.rs`
as `#[cfg(test)] mod tests;`.

- `mod.rs`: declares the test files.
- `core.rs`: `BloomFilter` operations and the false-positive estimate.
- `state.rs`: `BloomState`, the outgoing filter and the send debounce.
- `wire.rs`: the `FilterAnnounce` codec and its bounds.
