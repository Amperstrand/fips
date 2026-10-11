# core/src/proto/fmp/tests

Unit tests for the FMP link layer, declared from `fmp/mod.rs` as
`#[cfg(test)] mod tests;`, with one exception noted below.

- `mod.rs`: declares the test files.
- `util.rs`: shared fixtures, among them a handshake outcome with a
  fresh key.
- `core.rs`: the `poll_*` decisions and the inbound msg1 classification.
- `limits.rs`: the retry backoff.
- `offlink.rs`: classifying a same-epoch msg1 by whether it arrived on
  the peer's established link.
- `silent.rs`: `SilentSessions` and the refusals it starts.
- `state.rs`: `ConnectionState`.
- `stored_msg2.rs`: resending a stored msg2 for a duplicate msg1.
- `wire.rs`: the framing codec, through its public interface.
- `wire_private.rs`: tests of `wire.rs` that need its private items.
  It is not declared in `mod.rs`: `wire.rs` attaches it with
  `#[cfg(test)] #[path = "tests/wire_private.rs"] mod tests;`, so its
  tests are named `proto::fmp::wire::tests::...`.
