//! The `no_std` core of FIPS.
//!
//! Code in this crate builds for any target with an allocator. The rule that
//! keeps it so: no `std`, no async runtime, no OS calls, and no target `cfg`.
//! Anything that needs one of those belongs in the root `fips` crate, which
//! supplies it to the core rather than the core reaching for it.

#![no_std]

extern crate alloc;

// Tests run on the host, where the standard library is available.
#[cfg(test)]
extern crate std;

pub mod identity;
