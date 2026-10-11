# core/src

The source of the `fips-core` crate. The crate's purpose and the rules
its code follows are in `core/README.md`, one level up; read that before
changing anything here.

## What is here

- `lib.rs`: the crate root. It declares `#![no_std]`, brings in `alloc`,
  brings in `std` for tests only, and lists the crate's modules.
- `identity/`: node and peer identity types and their encodings. Its own
  README describes it.

## Rules for this level

- `lib.rs` stays `#![no_std]`. The `extern crate std;` line stays behind
  `#[cfg(test)]`, so only the host-run tests see `std`.
- Each module moved in from the root crate gets a `pub mod` line here,
  and its directory gets a README of its own.
- Keep this README's list in step with the directory: a module added,
  removed or renamed here updates it in the same change.
