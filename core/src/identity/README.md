# core/src/identity

<!-- markdownlint-disable MD013 -->

Identity types for FIPS nodes: the 16-byte node address derived from a
Nostr public key, the node's IPv6 address, a remote peer's
identity, and the NIP-19 npub and nsec encodings. Nothing here signs.

The root crate re-exports all of it under `fips::identity`, next to the
items that stay there: `Identity` (the local node's keypair, which
signs), the authentication challenge and response types, and the
deprecated `FipsAddress` alias. `Identity` stays in the root crate until
randomness can be passed in.

## Three identifiers from one key

A node's identity is a Nostr secp256k1 keypair. Three identifiers derive
from its x-only public key:

| Identifier | Type | Derivation | Used for |
| ---------- | ---- | ---------- | -------- |
| npub | `String` | NIP-19 bech32 of the public key | what people configure and see |
| node address | `NodeAddr` | first 16 bytes of SHA-256 of the public key | routing and every protocol table |
| IPv6 address | `Ipv6Address` | `0xfd` followed by the first 15 bytes of the node address | the TUN interface, so IP applications can reach a node |

These derivations are part of the protocol: every node computes them the
same way, and a change to any of them stops nodes agreeing on who is
who. Treat them as wire format; they do not change on the deployed
lines. The node address hashes the key rather than using its bytes
directly, which denies grinding attacks that exploit secp256k1's
algebraic structure.

## Files

- `mod.rs`: declares the submodules and re-exports their public items.
  It also holds the shared secp256k1 context and its `secp()` accessor,
  `IPV6_ADDRESS_PREFIX` (`0xfd`), `IdentityError`, the `sha256` helper
  and the private `hex_encode` helper.
- `address.rs`: `Ipv6Address`, the node's 128-bit IPv6 address with the
  `0xfd` prefix, and its conversion to `core::net::Ipv6Addr`.
- `node_addr.rs`: `NodeAddr`, the 16-byte node identifier taken from the
  SHA-256 of the public key.
- `peer.rs`: `PeerIdentity`, a remote peer known by its public key only.
- `encoding.rs`: npub and nsec encoding and decoding, and
  `decode_secret`, which accepts an nsec or a hex secret key.
- `tests.rs`: the unit tests for the items above. Tests that need
  `Identity` or the authentication types live in the root crate's
  `src/identity/tests.rs`.

## Invariants

- **The shared context.** `secp()` returns one `Secp256k1<All>` built on
  first use and reused by every caller in a build, the root crate's
  included. `Secp256k1::new()` blinds it from `rand::thread_rng()`
  because `core/Cargo.toml` enables `rand` and `std` on `secp256k1`.
  This is the one exception to the crate's rule that randomness is
  passed in. Removing `rand` must come with an explicit `randomize` call,
  or a build of the core alone gets an unblinded context.
- **Public only for the root crate.** `secp` and `sha256` are `pub` so
  the root crate can reach them, and `#[doc(hidden)]` because they are
  not part of the crate's interface. The root crate re-exports them as
  `pub(crate)`.
- **`hex_encode` stays private.** It formats `NodeAddr` for display and
  is not re-exported.
- **`IdentityError`.** Its `#[from]` fields need `std::error::Error` on
  the `secp256k1`, `bech32` and `hex` error types, which is why
  `core/Cargo.toml` enables `std` on those three. Changing a variant or a
  message changes what the root crate's callers see.
- **Secret material.** Only `encoding.rs` touches secret keys, to encode
  or decode an nsec, and it clears the raw byte copies it makes
  (`Zeroizing`). The nsec string `encode_nsec` returns is the secret in
  another form, and clearing it is the caller's job.
