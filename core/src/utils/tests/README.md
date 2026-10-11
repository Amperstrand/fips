# core/src/utils/tests

Unit tests for the files one level up.

- `index.rs`: tests for `index.rs`, attached from there with
  `#[cfg(test)] #[path = "tests/index.rs"] mod tests;`, so they keep
  their names (`utils::index::tests::...`). They check the byte
  round trip and the hex display. The allocator's tests stay with the
  allocator in the root crate.
