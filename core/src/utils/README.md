# core/src/utils

Small shared value types that the protocol cores name and that belong to
no one protocol. It mirrors the root crate's `utils` module, so an item
moved here keeps its path: `crate::utils::index::SessionIndex` resolves
the same way in both crates.

## Files

- `mod.rs`: declares the submodules.
- `hash.rs`: `HashMap`, `hashbrown`'s map with `KeyedHasher`, which
  builds SipHash-1-3 hashers under a 128-bit key drawn from the host's
  `EntropySource`. That is how `std` builds its own `HashMap`, which
  `alloc` lacks: the same table and the same keyed hash, so lookups cost
  the same, a peer that chooses the keys cannot predict the table's
  layout, and iteration order differs from process to process.
  `hashbrown`'s default hasher, `foldhash`, is not used because it is
  not keyed on targets without address-space randomization. `BTreeMap`
  needs no key, but it is several times slower per lookup and its order
  is the key order, which a peer choosing keys can steer; use it unless
  a peer chooses the keys and the map is looked up on a hot path.
- `index.rs`: `SessionIndex`, the 32-bit number a receiver chooses for a
  Noise session and its peer puts in every encrypted frame, so the
  receiver finds the session without a search. It converts to and from
  little-endian bytes and displays as eight hex digits.
- `tests/`: the unit tests for the files here; its README describes it.

## Invariants

- **A `KeyedHasher` is keyed from its entropy source.** `new::<E>()`
  draws all 16 key bytes from `E`; a fixed or zero key would give a peer
  a table layout it can predict and grind. Its `Debug` prints no key.
- **Of the session index code, only the type is here.**
  `IndexAllocator`, which draws random indices from `rand`'s
  thread-local generator, and `IndexError` stay in the root crate's
  `utils::index`, which re-exports `SessionIndex` from here. The
  allocator can join it once randomness is passed in.
- **The byte order is wire format.** `to_le_bytes` and `from_le_bytes`
  are how the index appears in every encrypted frame header; they do not
  change on the deployed lines.
- **The field is private.** Code builds and reads an index through `new`
  and `as_u32`, including the root crate's allocator.
