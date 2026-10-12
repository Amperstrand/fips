//! A hash map for keys a remote party can choose.
//!
//! `alloc` has no hash map. This is `hashbrown`'s, the table `std`'s
//! `HashMap` is built on, with the hasher `std` uses: SipHash-1-3 under a
//! random 128-bit key. The key comes from the host's [`EntropySource`], so
//! a peer that chooses the keys cannot predict where they land, and iteration
//! order differs from process to process, as it does with `std`'s map.

use core::hash::BuildHasher;

use siphasher::sip::SipHasher13;

use crate::entropy::EntropySource;

/// A hash map keyed with [`KeyedHasher`].
pub type HashMap<K, V> = hashbrown::HashMap<K, V, KeyedHasher>;

/// Builds SipHash-1-3 hashers under one random key.
#[derive(Clone)]
pub struct KeyedHasher {
    k0: u64,
    k1: u64,
}

impl KeyedHasher {
    /// A new random key, drawn from `E`.
    pub fn new<E: EntropySource>() -> Self {
        let mut key = [0u8; 16];
        E::fill(&mut key);
        let (k0, k1) = key.split_at(8);
        Self {
            k0: u64::from_le_bytes(k0.try_into().expect("8 bytes")),
            k1: u64::from_le_bytes(k1.try_into().expect("8 bytes")),
        }
    }
}

impl BuildHasher for KeyedHasher {
    type Hasher = SipHasher13;

    fn build_hasher(&self) -> SipHasher13 {
        SipHasher13::new_with_keys(self.k0, self.k1)
    }
}

impl core::fmt::Debug for KeyedHasher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("KeyedHasher { .. }")
    }
}

#[cfg(test)]
#[path = "tests/hash.rs"]
mod tests;
