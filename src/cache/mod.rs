//! Caching Entities
//!
//! Coordinate caching for FIPS routing. The CoordCache stores
//! address-to-coordinate mappings populated by session setup and discovery.

mod coord_cache;
mod entry;

use thiserror::Error;

pub use coord_cache::{DEFAULT_COORD_CACHE_SIZE, DEFAULT_COORD_CACHE_TTL_MS, HintOutcome};

/// The coordinate cache, its hasher keyed from [`OsEntropy`](crate::identity::OsEntropy).
pub type CoordCache = coord_cache::CoordCache<crate::identity::OsEntropy>;
pub use entry::{CacheEntry, CoordSource, VERIFIED_TTL_MS};

/// Errors related to cache operations.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cache full: max {max} entries")]
    CacheFull { max: usize },

    #[error("entry not found")]
    NotFound,

    #[error("entry expired")]
    Expired,
}

/// Cache statistics.
#[derive(Clone, Debug)]
pub struct CacheStats {
    /// Current number of entries.
    pub entries: usize,
    /// Maximum capacity.
    pub max_entries: usize,
    /// Number of expired entries.
    pub expired: usize,
    /// Average entry age in milliseconds.
    pub avg_age_ms: u64,
}

impl CacheStats {
    /// Fill ratio (entries / max_entries).
    pub fn fill_ratio(&self) -> f64 {
        if self.max_entries == 0 {
            0.0
        } else {
            self.entries as f64 / self.max_entries as f64
        }
    }
}

#[cfg(test)]
mod tests;
