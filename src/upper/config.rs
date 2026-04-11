//! Upper layer configuration types.
//!
//! Configuration for the IPv6 adaptation layer components: TUN interface
//! and DNS responder.

use secp256k1::{Keypair, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::identity::{FipsAddress, NodeAddr};
use crate::IdentityError;

/// Default TUN device name.
const DEFAULT_TUN_NAME: &str = "fips0";

/// Default TUN MTU (IPv6 minimum).
const DEFAULT_TUN_MTU: u16 = 1280;

/// Default DNS responder bind address.
const DEFAULT_DNS_BIND_ADDR: &str = "127.0.0.1";

/// Default DNS responder port.
const DEFAULT_DNS_PORT: u16 = 5354;

/// Default DNS record TTL in seconds (5 minutes).
const DEFAULT_DNS_TTL: u32 = 300;

const DEFAULT_TCP_PROXY_LISTEN_ADDR: &str = "127.0.0.1:6053";

fn default_true() -> bool {
    true
}

/// DNS responder configuration (`dns.*`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsConfig {
    /// Enable DNS responder (`dns.enabled`, default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Bind address (`dns.bind_addr`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_addr: Option<String>,

    /// Port (`dns.port`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,

    /// Record TTL in seconds (`dns.ttl`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u32>,
}

impl Default for DnsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind_addr: None,
            port: None,
            ttl: None,
        }
    }
}

impl DnsConfig {
    /// Get the bind address (default: 127.0.0.1).
    pub fn bind_addr(&self) -> &str {
        self.bind_addr.as_deref().unwrap_or(DEFAULT_DNS_BIND_ADDR)
    }

    /// Get the port (default: 5354).
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(DEFAULT_DNS_PORT)
    }

    /// Get the TTL in seconds (default: 300).
    pub fn ttl(&self) -> u32 {
        self.ttl.unwrap_or(DEFAULT_DNS_TTL)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct TcpProxyConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_addr: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_npub: Option<String>,
}


impl TcpProxyConfig {
    pub fn listen_addr(&self) -> &str {
        self.listen_addr
            .as_deref()
            .unwrap_or(DEFAULT_TCP_PROXY_LISTEN_ADDR)
    }
}

/// TUN interface configuration (`tun.*`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TunConfig {
    /// Enable TUN interface (`tun.enabled`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enabled: bool,

    /// Device name (`tun.name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// MTU (`tun.mtu`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u16>,
}

impl TunConfig {
    /// Get the device name (default: "fips0").
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or(DEFAULT_TUN_NAME)
    }

    /// Get the MTU (default: 1280).
    pub fn mtu(&self) -> u16 {
        self.mtu.unwrap_or(DEFAULT_TUN_MTU)
    }
}

/// Leaf service configuration (TCP service on leaf node).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeafServiceConfig {
    /// Port number (e.g., 6053 for ESPHome API).
    pub port: u16,

    /// Protocol type (currently only "tcp" is supported).
    pub protocol: String,
}

impl Default for LeafServiceConfig {
    fn default() -> Self {
        Self {
            port: 6053,
            protocol: "tcp".to_string(),
        }
    }
}

/// Leaf proxy configuration (ESPHome device identity and services).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeafProxyConfig {
    /// Identity seed for key derivation.
    /// The actual secret key is computed as SHA256("esphome:fips_ble:" + identity_seed).
    pub identity_seed: String,

    /// Services to proxy from this leaf node.
    #[serde(default)]
    pub services: Vec<LeafServiceConfig>,
}

impl Default for LeafProxyConfig {
    fn default() -> Self {
        Self {
            identity_seed: String::new(),
            services: vec![LeafServiceConfig::default()],
        }
    }
}

impl LeafProxyConfig {
    pub fn derived_identity(&self) -> Result<LeafIdentity, IdentityError> {
        let prefix = format!("esphome:fips_ble:{}", self.identity_seed);
        let mut hasher = Sha256::new();
        hasher.update(prefix.as_bytes());
        let secret_bytes = hasher.finalize();

        let secret_key = SecretKey::from_slice(&secret_bytes)?;
        let secp = secp256k1::Secp256k1::new();
        let keypair = Keypair::from_secret_key(&secp, &secret_key);
        let (x_only, _parity) = keypair.x_only_public_key();
        let public_key = keypair.public_key();

        let node_addr = NodeAddr::from_pubkey(&x_only);
        let fips_address = FipsAddress::from_node_addr(&node_addr);

        Ok(LeafIdentity {
            node_addr,
            fips_address,
            secret_key: keypair.secret_key(),
            public_key,
            keypair,
        })
    }
}

#[derive(Debug, Clone)]
pub struct LeafIdentity {
    pub node_addr: NodeAddr,
    pub fips_address: FipsAddress,
    pub secret_key: SecretKey,
    pub public_key: secp256k1::PublicKey,
    pub keypair: Keypair,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_leaf_service_config() {
        let config = LeafServiceConfig::default();
        assert_eq!(config.port, 6053);
        assert_eq!(config.protocol, "tcp");
    }

    #[test]
    fn test_leaf_proxy_config_derive_identity_esp32s3() {
        let config = LeafProxyConfig {
            identity_seed: "fips-esp32s3".to_string(),
            services: vec![LeafServiceConfig::default()],
        };

        let identity = config.derived_identity().unwrap();

        assert_eq!(
            identity.node_addr.to_string(),
            "91132891ed6ef5c0ff983fd4c1e9c970"
        );
        assert_eq!(
            identity.fips_address.to_string(),
            "fd91:1328:91ed:6ef5:c0ff:983f:d4c1:e9c9"
        );
    }
}
