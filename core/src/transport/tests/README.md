# core/src/transport/tests

Unit tests for the transport value types one level up.

- `types.rs`: tests for `types.rs`, attached from there with
  `#[cfg(test)] #[path = "tests/types.rs"] mod tests;`, so they are a
  child module of `types` and keep their names
  (`transport::types::tests::...`). They check that clones share their
  bytes and that equality and hashing go by value.

The test of `TransportAddr::from_socket_addr` against `SocketAddr`'s own
`to_string` stays in the root crate's `transport` tests, which run
against this code.
