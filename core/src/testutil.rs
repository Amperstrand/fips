//! Test fixtures shared by the crate's unit tests.

use crate::identity::NodeAddr;

/// Build a `NodeAddr` from a single discriminating byte in position 0.
pub(crate) fn make_node_addr(val: u8) -> NodeAddr {
    let mut bytes = [0u8; 16];
    bytes[0] = val;
    NodeAddr::from_bytes(bytes)
}

/// A random secp256k1 keypair, built the way `Identity::generate` builds one.
pub(crate) fn make_keypair() -> secp256k1::Keypair {
    let mut secret = [0u8; 32];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut secret);
    let key =
        secp256k1::SecretKey::from_slice(&secret).expect("32 random bytes is a valid secret key");
    secp256k1::Keypair::from_secret_key(crate::identity::secp(), &key)
}

/// An entropy source that fills every byte with `B`, so a test can tell
/// which source a value was drawn from.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FixedEntropy<const B: u8>;

impl<const B: u8> crate::entropy::EntropySource for FixedEntropy<B> {
    fn fill(dest: &mut [u8]) {
        dest.fill(B);
    }
}
