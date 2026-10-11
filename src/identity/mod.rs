//! FIPS Identity System
//!
//! Node identity based on Nostr keypairs (secp256k1). The node_addr is derived
//! from the public key via SHA-256, and the node's IPv6 address is the 0xfd
//! prefix followed by the first 15 bytes of the node_addr.

mod auth;
mod local;

pub use auth::{AuthChallenge, AuthResponse};
pub use fips_core::identity::{
    IPV6_ADDRESS_PREFIX, IdentityError, Ipv6Address, NodeAddr, PeerIdentity, decode_npub,
    decode_nsec, decode_secret, encode_npub, encode_nsec,
};
pub(crate) use fips_core::identity::{secp, sha256};
pub(crate) use local::ErasingKeypair;
pub use local::Identity;

/// Former name of [`Ipv6Address`], kept so code written against it still builds.
#[deprecated(note = "renamed to Ipv6Address")]
pub type FipsAddress = Ipv6Address;

/// Former name of [`IPV6_ADDRESS_PREFIX`].
#[deprecated(note = "renamed to IPV6_ADDRESS_PREFIX")]
pub const FIPS_ADDRESS_PREFIX: u8 = IPV6_ADDRESS_PREFIX;

#[cfg(test)]
mod tests;
