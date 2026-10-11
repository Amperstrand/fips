# core/src/proto/fsp/tests

Unit tests for the session layer, declared from `fsp/mod.rs` as
`#[cfg(test)] mod tests;`.

- `mod.rs`: declares the test files.
- `core.rs`: the rekey and resend decisions and the data transforms.
- `quorum.rs`: `LinkQuorum`.
- `wire.rs`: the session message and header codecs.
