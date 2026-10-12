//! Random bytes, supplied by the host.
//!
//! The core never reads a random source of its own. A type that needs random
//! bytes takes the host's source as a type parameter bounded by
//! [`EntropySource`], and the host fixes that parameter once, in a type alias
//! at the path its callers already use.

/// A source of cryptographically secure random bytes, supplied by the host.
///
/// `fill` must fill all of `dest` with output from a cryptographically secure
/// generator seeded from the host's entropy, and must not fail. The core
/// derives secret keys from these bytes: a source that returns predictable
/// bytes makes every key it produces predictable.
pub trait EntropySource {
    /// Fill `dest` with random bytes.
    fn fill(dest: &mut [u8]);
}
