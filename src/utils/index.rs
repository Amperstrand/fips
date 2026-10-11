//! Session Index Allocator
//!
//! Manages allocation of 32-bit session indices for O(1) packet dispatch.
//! Each Noise session receives a unique index chosen by the receiver;
//! incoming encrypted packets include the receiver's index for fast lookup.
//!
//! ## Design
//!
//! - Indices are random (cryptographically secure) to prevent guessing
//! - Unique per transport to avoid collision between transports
//! - 32-bit space supports ~65K concurrent sessions before birthday collision
//! - Indices are rotated on rekey for anti-correlation
//!
//! ## Wire Format
//!
//! Encrypted frames include receiver_idx for session lookup:
//!
//! ```text
//! [0x00][receiver_idx:4 LE][counter:8 LE][ciphertext+tag]
//! ```

use rand::RngExt;
use std::collections::HashSet;
use thiserror::Error;

pub use fips_core::utils::index::SessionIndex;

/// Errors related to index allocation.
#[derive(Debug, Error)]
pub enum IndexError {
    #[error("no available indices (too many active sessions)")]
    Exhausted,

    #[error("index {0} not found")]
    NotFound(u32),

    #[error("index {0} already in use")]
    AlreadyInUse(u32),
}

/// Allocator for session indices within a single transport.
///
/// Manages a pool of random 32-bit indices, tracking which are in use
/// to prevent collision. Thread-safe for single-threaded async use.
#[derive(Debug)]
pub struct IndexAllocator {
    /// Set of currently allocated indices.
    in_use: HashSet<u32>,
    /// Maximum allocation attempts before giving up.
    max_attempts: usize,
}

impl IndexAllocator {
    /// Create a new index allocator.
    pub fn new() -> Self {
        Self {
            in_use: HashSet::new(),
            max_attempts: 100,
        }
    }

    /// Create with a specific max attempts limit.
    pub fn with_max_attempts(max_attempts: usize) -> Self {
        Self {
            in_use: HashSet::new(),
            max_attempts,
        }
    }

    /// Allocate a new random index.
    ///
    /// Returns a cryptographically random 32-bit index that is not
    /// currently in use. Returns error if allocation fails after
    /// max_attempts tries (indicates too many active sessions).
    pub fn allocate(&mut self) -> Result<SessionIndex, IndexError> {
        let mut rng = rand::rng();

        for _ in 0..self.max_attempts {
            let candidate = rng.random::<u32>();
            if !self.in_use.contains(&candidate) {
                self.in_use.insert(candidate);
                return Ok(SessionIndex::new(candidate));
            }
        }

        Err(IndexError::Exhausted)
    }

    /// Free an index, returning it to the available pool.
    ///
    /// Returns error if the index was not allocated.
    pub fn free(&mut self, index: SessionIndex) -> Result<(), IndexError> {
        if self.in_use.remove(&index.as_u32()) {
            Ok(())
        } else {
            Err(IndexError::NotFound(index.as_u32()))
        }
    }

    /// Check if an index is currently allocated.
    pub fn is_allocated(&self, index: SessionIndex) -> bool {
        self.in_use.contains(&index.as_u32())
    }

    /// Number of currently allocated indices.
    pub fn count(&self) -> usize {
        self.in_use.len()
    }

    /// Check if the allocator is empty (no indices allocated).
    pub fn is_empty(&self) -> bool {
        self.in_use.is_empty()
    }

    /// Reserve a specific index (for testing or migration).
    ///
    /// Returns error if the index is already in use.
    pub fn reserve(&mut self, index: SessionIndex) -> Result<(), IndexError> {
        if self.in_use.contains(&index.as_u32()) {
            Err(IndexError::AlreadyInUse(index.as_u32()))
        } else {
            self.in_use.insert(index.as_u32());
            Ok(())
        }
    }

    /// Clear all allocations (use with caution).
    pub fn clear(&mut self) {
        self.in_use.clear();
    }
}

impl Default for IndexAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "tests/index.rs"]
mod tests;
