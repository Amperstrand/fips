# core/src

The source of the `fips-core` crate. The crate's purpose and the rules
its code follows are in `core/README.md`, one level up; read that before
changing anything here.

## What is here

- `lib.rs`: the crate root. It declares `#![no_std]`, brings in `alloc`,
  brings in `std` for tests only, and lists the crate's modules.
- `cache/`: the coordinate cache, which maps node addresses to tree
  coordinates for routing. Its own README describes it.
- `entropy.rs`: the `EntropySource` trait, through which the host supplies
  random bytes. A type that needs them takes the source as a type
  parameter; the core never reads a random source of its own.
- `identity/`: node and peer identity types and their encodings. Its own
  README describes it.
- `noise/`: the fixed sizes of Noise handshake messages and their parts.
  Its own README describes it.
- `proto/`: the protocol: wire codecs and sans-IO state machines for
  every layer. The root crate reaches it as `crate::proto`. Its own
  README describes it.
- `transport/`: the transport value types (identifiers, address,
  statistics) that transports and the protocol cores both name. Its own
  README describes it.
- `utils/`: small shared value types: the session index, and the hash
  map for keys a peer can choose, keyed from the host's entropy source.
  Its own README describes it.
- `testutil.rs`: fixtures shared by the crate's unit tests
  (`make_node_addr`, `make_keypair`; `TestEntropy`, the random source
  tests build randomized types with; and `FixedEntropy`, a source that
  fills every byte with one value). Compiled for tests only.

## Rules for this level

- `lib.rs` stays `#![no_std]`. The `extern crate std;` line stays behind
  `#[cfg(test)]`, so only the host-run tests see `std`.
- Each module moved in from the root crate gets a `pub mod` line here,
  and its directory gets a README of its own.
- Keep this README's list in step with the directory: a module added,
  removed or renamed here updates it in the same change.
