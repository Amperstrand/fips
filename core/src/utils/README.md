# core/src/utils

Small shared value types that the protocol cores name and that belong to
no one protocol. It mirrors the root crate's `utils` module, so an item
moved here keeps its path: `crate::utils::index::SessionIndex` resolves
the same way in both crates.

## Files

- `mod.rs`: declares the submodules.
- `index.rs`: `SessionIndex`, the 32-bit number a receiver chooses for a
  Noise session and its peer puts in every encrypted frame, so the
  receiver finds the session without a search. It converts to and from
  little-endian bytes and displays as eight hex digits.
- `tests/`: the unit tests for the files here; its README describes it.

## Invariants

- **Only the index type is here.** `IndexAllocator`, which draws random
  indices from `rand`'s thread-local generator, and `IndexError` stay
  in the root crate's `utils::index`, which re-exports `SessionIndex`
  from here. The allocator can join it once randomness is passed in.
- **The byte order is wire format.** `to_le_bytes` and `from_le_bytes`
  are how the index appears in every encrypted frame header; they do not
  change on the deployed lines.
- **The field is private.** Code builds and reads an index through `new`
  and `as_u32`, including the root crate's allocator.
