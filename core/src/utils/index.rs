//! Session indices.
//!
//! A session index is the 32-bit number a receiver chooses for each Noise
//! session and its peer puts in every encrypted frame, so the receiver can
//! find the session without a search. This module holds the index type
//! only; the allocator that draws random indices stays in the root crate
//! until randomness is passed in.

/// A 32-bit session index.
///
/// Wrapper type for type safety and clarity in APIs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionIndex(u32);

impl SessionIndex {
    /// Create from raw u32.
    pub fn new(value: u32) -> Self {
        Self(value)
    }

    /// Get the raw u32 value.
    pub fn as_u32(&self) -> u32 {
        self.0
    }

    /// Convert to little-endian bytes.
    pub fn to_le_bytes(&self) -> [u8; 4] {
        self.0.to_le_bytes()
    }

    /// Create from little-endian bytes.
    pub fn from_le_bytes(bytes: [u8; 4]) -> Self {
        Self(u32::from_le_bytes(bytes))
    }
}

impl core::fmt::Display for SessionIndex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:08x}", self.0)
    }
}

#[cfg(test)]
#[path = "tests/index.rs"]
mod tests;
