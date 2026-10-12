//! FIPS Identity System
//!
//! Node identity based on Nostr keypairs (secp256k1). The node_addr is derived
//! from the public key via SHA-256, and the node's IPv6 address is the 0xfd
//! prefix followed by the first 15 bytes of the node_addr.

mod auth;
mod local;

pub use auth::AuthResponse;
pub use fips_core::identity::{
    IPV6_ADDRESS_PREFIX, IdentityError, Ipv6Address, NodeAddr, PeerIdentity, decode_npub,
    decode_nsec, decode_secret, encode_npub, encode_nsec, init_secp,
};
pub(crate) use fips_core::identity::{secp, sha256};
pub(crate) use local::ErasingKeypair;

/// The local node's identity, drawing its random bytes from [`OsEntropy`].
pub type Identity = local::Identity<OsEntropy>;

/// A random authentication challenge, drawn from [`OsEntropy`].
pub type AuthChallenge = auth::AuthChallenge<OsEntropy>;

/// The host's random source for the core's randomized types: `rand`'s
/// thread-local generator, seeded from the operating system.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsEntropy;

impl fips_core::entropy::EntropySource for OsEntropy {
    fn fill(dest: &mut [u8]) {
        rand::Rng::fill_bytes(&mut rand::rng(), dest);
    }
}

/// Former name of [`Ipv6Address`], kept so code written against it still builds.
#[deprecated(note = "renamed to Ipv6Address")]
pub type FipsAddress = Ipv6Address;

/// Former name of [`IPV6_ADDRESS_PREFIX`].
#[deprecated(note = "renamed to IPV6_ADDRESS_PREFIX")]
pub const FIPS_ADDRESS_PREFIX: u8 = IPV6_ADDRESS_PREFIX;

#[cfg(test)]
mod tests;
