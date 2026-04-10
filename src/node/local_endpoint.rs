//! Local endpoint representation for leaf identity proxying.
//!
//! A `LocalEndpoint` represents a cryptographic identity that this node can
//! act on behalf of. The primary endpoint is the node's own identity; additional
//! endpoints are proxied leaf devices (e.g., ESP32 nodes) whose keypairs are
//! held by this gateway node.

use secp256k1::Keypair;

use crate::identity::{FipsAddress, NodeAddr};

/// A local endpoint identity held by this node.
///
/// The primary endpoint (the node's own identity) is always present and has
/// `is_primary == true`. Additional endpoints represent proxied leaf devices
/// whose keypairs are managed by this gateway.
#[derive(Debug, Clone)]
pub struct LocalEndpoint {
    /// The node address derived from this endpoint's public key.
    pub node_addr: NodeAddr,
    /// The secp256k1 keypair for this endpoint.
    pub keypair: Keypair,
    /// The FIPS IPv6 address for this endpoint.
    pub fips_address: FipsAddress,
    /// Epoch value for peer restart detection (8-byte startup epoch).
    pub startup_epoch: u64,
    /// Whether this is the node's own identity (true) or a proxied leaf (false).
    pub is_primary: bool,
}

impl LocalEndpoint {
    /// Create the primary endpoint from the node's own identity.
    pub fn primary(keypair: Keypair, startup_epoch: [u8; 8]) -> Self {
        let (x_only, _) = keypair.x_only_public_key();
        let node_addr = NodeAddr::from_pubkey(&x_only);
        let fips_address = FipsAddress::from_node_addr(&node_addr);
        let startup_epoch = u64::from_be_bytes(startup_epoch);

        Self {
            node_addr,
            keypair,
            fips_address,
            startup_epoch,
            is_primary: true,
        }
    }

    /// Create a proxied leaf endpoint.
    pub fn leaf(keypair: Keypair, startup_epoch: u64) -> Self {
        let (x_only, _) = keypair.x_only_public_key();
        let node_addr = NodeAddr::from_pubkey(&x_only);
        let fips_address = FipsAddress::from_node_addr(&node_addr);

        Self {
            node_addr,
            keypair,
            fips_address,
            startup_epoch,
            is_primary: false,
        }
    }

    /// Get the x-only public key for this endpoint.
    pub fn pubkey(&self) -> secp256k1::XOnlyPublicKey {
        self.keypair.x_only_public_key().0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;

    #[test]
    fn test_primary_endpoint_from_identity() {
        let identity = Identity::generate();
        let keypair = identity.keypair();
        let epoch = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];

        let endpoint = LocalEndpoint::primary(keypair, epoch);

        assert!(endpoint.is_primary);
        assert_eq!(&endpoint.node_addr, identity.node_addr());
        assert_eq!(endpoint.fips_address, *identity.address());
        assert_eq!(endpoint.startup_epoch, u64::from_be_bytes(epoch));
    }

    #[test]
    fn test_leaf_endpoint() {
        let identity = Identity::generate();
        let keypair = identity.keypair();

        let endpoint = LocalEndpoint::leaf(keypair, 42);

        assert!(!endpoint.is_primary);
        assert_eq!(endpoint.startup_epoch, 42);
        // Verify derived addresses are consistent
        let (x_only, _) = endpoint.keypair.x_only_public_key();
        assert_eq!(&endpoint.node_addr, &NodeAddr::from_pubkey(&x_only));
        assert_eq!(
            endpoint.fips_address,
            FipsAddress::from_node_addr(&endpoint.node_addr)
        );
    }

    #[test]
    fn test_primary_endpoint_known_seed() {
        // Verify with known seed "fips-esp32s3" from config tests
        use sha2::{Digest, Sha256};

        let seed = "fips-esp32s3";
        let prefix = format!("esphome:fips_ble:{}", seed);
        let mut hasher = Sha256::new();
        hasher.update(prefix.as_bytes());
        let secret_bytes = hasher.finalize();

        let secret_key = secp256k1::SecretKey::from_slice(&secret_bytes).unwrap();
        let secp = secp256k1::Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, &secret_key);

        let endpoint = LocalEndpoint::primary(keypair, [0u8; 8]);

        assert_eq!(
            endpoint.node_addr.to_string(),
            "91132891ed6ef5c0ff983fd4c1e9c970"
        );
        assert_eq!(
            endpoint.fips_address.to_string(),
            "fd91:1328:91ed:6ef5:c0ff:983f:d4c1:e9c9"
        );
    }

    #[test]
    fn test_pubkey_accessor() {
        let identity = Identity::generate();
        let keypair = identity.keypair();
        let endpoint = LocalEndpoint::primary(keypair, [0u8; 8]);

        assert_eq!(endpoint.pubkey(), identity.pubkey());
    }
}
