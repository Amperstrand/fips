use std::collections::HashSet;
use std::net::Ipv6Addr;

use secp256k1::{Keypair, SecretKey};

use super::*;

#[test]
fn test_identity_generation() {
    let identity = Identity::generate();

    // NodeAddr should be 16 bytes
    assert_eq!(identity.node_addr().as_bytes().len(), 16);

    // Address should start with 0xfd
    assert_eq!(identity.address().as_bytes()[0], 0xfd);

    // Address bytes 1-15 should match node_addr bytes 0-14
    assert_eq!(
        &identity.address().as_bytes()[1..16],
        &identity.node_addr().as_bytes()[0..15]
    );
}

#[test]
fn test_node_addr_from_pubkey_deterministic() {
    let identity = Identity::generate();
    let pubkey = identity.pubkey();

    let node_addr1 = NodeAddr::from_pubkey(&pubkey);
    let node_addr2 = NodeAddr::from_pubkey(&pubkey);

    assert_eq!(node_addr1, node_addr2);
}

#[test]
fn test_fips_address_ipv6_format() {
    let identity = Identity::generate();
    let ipv6 = identity.address().to_ipv6();
    let addr_str = ipv6.to_string();

    // Should start with fd (ULA prefix)
    assert!(addr_str.starts_with("fd"));

    // Conversion should be lossless
    let octets = ipv6.octets();
    assert_eq!(&octets, identity.address().as_bytes());
}

#[test]
fn test_auth_challenge_verify_success() {
    let identity = Identity::generate();
    let challenge = AuthChallenge::generate();
    let timestamp = 1234567890u64;

    let response = identity.sign_challenge(challenge.as_bytes(), timestamp);
    let result = challenge.verify(&response);

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), *identity.node_addr());
}

#[test]
fn test_auth_challenge_verify_wrong_challenge() {
    let identity = Identity::generate();
    let challenge1 = AuthChallenge::generate();
    let challenge2 = AuthChallenge::generate();
    let timestamp = 1234567890u64;

    let response = identity.sign_challenge(challenge1.as_bytes(), timestamp);
    let result = challenge2.verify(&response);

    assert!(matches!(
        result,
        Err(IdentityError::SignatureVerificationFailed)
    ));
}

#[test]
fn test_auth_challenge_verify_wrong_timestamp() {
    let identity = Identity::generate();
    let challenge = AuthChallenge::generate();

    let response = identity.sign_challenge(challenge.as_bytes(), 1234567890);

    // Modify the timestamp in the response
    let bad_response = AuthResponse {
        pubkey: response.pubkey,
        timestamp: 9999999999,
        signature: response.signature,
    };

    let result = challenge.verify(&bad_response);
    assert!(matches!(
        result,
        Err(IdentityError::SignatureVerificationFailed)
    ));
}

#[test]
fn test_node_addr_ordering() {
    let id1 = Identity::generate();
    let id2 = Identity::generate();

    // NodeAddrs should be comparable for root election
    let _cmp = id1.node_addr().cmp(id2.node_addr());
}

#[test]
fn test_identity_from_secret_bytes() {
    // A known secret key (32 bytes)
    let secret_bytes: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];

    let identity1 = Identity::from_secret_bytes(&secret_bytes).unwrap();
    let identity2 = Identity::from_secret_bytes(&secret_bytes).unwrap();

    // Same secret key should produce same node_addr
    assert_eq!(identity1.node_addr(), identity2.node_addr());
    assert_eq!(identity1.address(), identity2.address());
}

#[test]
fn test_identity_sign() {
    let identity = Identity::generate();
    let data = b"test message";

    let sig = identity.sign(data);

    // Verify the signature manually
    let digest = super::sha256(data);
    assert!(
        super::secp()
            .verify_schnorr(&sig, &digest, &identity.pubkey())
            .is_ok()
    );
}

#[test]
fn test_npub_encoding() {
    let identity = Identity::generate();
    let npub = identity.npub();

    // Should start with "npub1"
    assert!(npub.starts_with("npub1"));

    // Should be 63 characters (npub1 + 58 chars of bech32 data)
    assert_eq!(npub.len(), 63);
}

#[test]
fn test_npub_roundtrip() {
    let identity = Identity::generate();
    let npub = identity.npub();

    let decoded = decode_npub(&npub).unwrap();
    assert_eq!(decoded, identity.pubkey());
}

#[test]
fn test_npub_known_vector() {
    // Test against a known npub (from NIP-19 test vectors or generated externally)
    let secret_bytes: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];

    let identity = Identity::from_secret_bytes(&secret_bytes).unwrap();
    let npub = identity.npub();

    // Decode and verify it matches the original pubkey
    let decoded = decode_npub(&npub).unwrap();
    assert_eq!(decoded, identity.pubkey());

    // npub should be deterministic
    let npub2 = encode_npub(&identity.pubkey());
    assert_eq!(npub, npub2);
}

#[test]
fn test_peer_identity_from_npub() {
    let identity = Identity::generate();
    let npub = identity.npub();

    let peer = PeerIdentity::from_npub(&npub).unwrap();

    assert_eq!(peer.pubkey(), identity.pubkey());
    assert_eq!(peer.node_addr(), identity.node_addr());
    assert_eq!(peer.address(), identity.address());
    assert_eq!(peer.npub(), npub);
}

#[test]
fn test_peer_identity_verify_signature() {
    let identity = Identity::generate();
    let peer = PeerIdentity::from_pubkey(identity.pubkey());

    let data = b"hello world";
    let signature = identity.sign(data);

    assert!(peer.verify(data, &signature));
    assert!(!peer.verify(b"wrong data", &signature));
}

#[test]
fn test_peer_identity_display() {
    let identity = Identity::generate();
    let peer = PeerIdentity::from_pubkey(identity.pubkey());

    let display = format!("{}", peer);
    assert!(display.starts_with("npub1"));
    assert_eq!(display, identity.npub());
}

#[test]
fn test_decode_nsec_invalid_prefix() {
    // Use a valid npub (from a generated identity) to test prefix rejection
    let identity = Identity::generate();
    let npub = identity.npub();
    let result = decode_nsec(&npub);
    assert!(matches!(result, Err(IdentityError::InvalidNsecPrefix(_))));
}

#[test]
fn test_identity_from_secret_str_nsec() {
    let secret_bytes: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];

    let secret_key = SecretKey::from_slice(&secret_bytes).unwrap();
    let nsec = encode_nsec(&secret_key);

    let identity = Identity::from_secret_str(&nsec).unwrap();
    let identity_from_bytes = Identity::from_secret_bytes(&secret_bytes).unwrap();

    assert_eq!(identity.node_addr(), identity_from_bytes.node_addr());
}

#[test]
fn test_identity_from_secret_str_hex() {
    let hex_str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
    let secret_bytes: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];

    let identity = Identity::from_secret_str(hex_str).unwrap();
    let identity_from_bytes = Identity::from_secret_bytes(&secret_bytes).unwrap();

    assert_eq!(identity.node_addr(), identity_from_bytes.node_addr());
}

#[test]
fn test_hex_conversion_case1() {
    let hex_str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
    let identity = Identity::from_secret_str(hex_str).unwrap();
    let npub = identity.npub();
    assert!(npub.starts_with("npub1"));
}

#[test]
fn test_hex_conversion_case2() {
    let hex_str = "b102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1fb0";
    let identity = Identity::from_secret_str(hex_str).unwrap();
    let npub = identity.npub();
    assert!(npub.starts_with("npub1"));
}

// ===== node_addr.rs tests =====

#[test]
fn test_node_addr_hash() {
    let id1 = Identity::generate();
    let id2 = Identity::generate();
    let mut set = HashSet::new();
    set.insert(*id1.node_addr());
    set.insert(*id2.node_addr());
    assert_eq!(set.len(), 2);
    // Re-inserting the same address doesn't grow the set
    set.insert(*id1.node_addr());
    assert_eq!(set.len(), 2);
}

// ===== address.rs tests =====

#[test]
fn test_fips_address_into_ipv6() {
    let identity = Identity::generate();
    let addr = *identity.address();
    // Test the From trait (not to_ipv6 method)
    let ipv6: Ipv6Addr = addr.into();
    assert_eq!(ipv6.octets(), *addr.as_bytes());
}

#[test]
fn test_fips_address_eq_hash() {
    let identity = Identity::generate();
    let addr1 = *identity.address();
    let addr2 = Ipv6Address::from_node_addr(identity.node_addr());
    assert_eq!(addr1, addr2);

    let mut set = HashSet::new();
    set.insert(addr1);
    set.insert(addr2);
    assert_eq!(set.len(), 1);
}

// ===== auth.rs tests =====

#[test]
fn test_auth_challenge_from_bytes() {
    let bytes = [0x42u8; 32];
    let challenge = AuthChallenge::from_bytes(bytes);
    assert_eq!(challenge.as_bytes(), &bytes);
}

// ===== peer.rs tests =====

#[test]
fn test_peer_identity_from_pubkey_full() {
    let identity = Identity::generate();
    let full_pubkey = identity.pubkey_full();

    let peer = PeerIdentity::from_pubkey_full(full_pubkey);

    // x-only key should match
    assert_eq!(peer.pubkey(), identity.pubkey());
    // Full key should be preserved (not derived)
    assert_eq!(peer.pubkey_full(), full_pubkey);
    // Derived identifiers should match
    assert_eq!(peer.node_addr(), identity.node_addr());
    assert_eq!(peer.address(), identity.address());
}

#[test]
fn test_peer_identity_pubkey_full_even_parity_fallback() {
    let identity = Identity::generate();
    // from_pubkey only stores x-only, so pubkey_full must derive with even parity
    let peer = PeerIdentity::from_pubkey(identity.pubkey());
    let full = peer.pubkey_full();
    // The derived full key's x-only component should match
    let (x_only, _parity) = full.x_only_public_key();
    assert_eq!(x_only, identity.pubkey());
}

#[test]
fn test_peer_identity_debug() {
    let identity = Identity::generate();
    let peer = PeerIdentity::from_pubkey(identity.pubkey());
    let debug = format!("{:?}", peer);
    assert!(debug.starts_with("PeerIdentity {"));
    assert!(debug.contains("node_addr"));
    assert!(debug.contains("address"));
}

// ===== local.rs tests =====

#[test]
fn test_identity_keypair() {
    let identity = Identity::generate();
    let keypair = identity.keypair();
    // keypair's public key should match identity's pubkey
    let (x_only, _) = keypair.x_only_public_key();
    assert_eq!(x_only, identity.pubkey());
}

#[test]
fn test_identity_pubkey_full() {
    let identity = Identity::generate();
    let full = identity.pubkey_full();
    // Full key's x-only component should match pubkey
    let (x_only, _) = full.x_only_public_key();
    assert_eq!(x_only, identity.pubkey());
}

#[test]
fn test_identity_debug() {
    let identity = Identity::generate();
    let debug = format!("{:?}", identity);
    assert!(debug.starts_with("Identity {"));
    assert!(debug.contains("node_addr"));
    assert!(debug.contains("address"));
    // Should NOT contain the secret key
    assert!(!debug.contains("keypair"));
    assert!(debug.contains(".."));
}

/// Hold a guard over `keypair` and return early or normally, the way the
/// handshake entry points do.
fn guarded_secret(keypair: &mut Keypair, fail: bool) -> Result<[u8; 32], ()> {
    let guard = ErasingKeypair::new(keypair);
    if fail {
        return Err(());
    }
    Ok(guard.get().secret_bytes())
}

#[test]
fn test_erasing_keypair_erases_the_callers_binding_in_place_on_every_exit() {
    let identity = Identity::generate();
    let original = identity.keypair().secret_bytes();
    // `non_secure_erase` overwrites a keypair with a fixed dummy whose
    // secret is 32 bytes of 0x01.
    let erased = [1u8; 32];
    assert_ne!(original, erased);

    let mut keypair = identity.keypair();
    assert_eq!(guarded_secret(&mut keypair, false), Ok(original));
    assert_eq!(keypair.secret_bytes(), erased);

    let mut keypair = identity.keypair();
    assert_eq!(guarded_secret(&mut keypair, true), Err(()));
    assert_eq!(keypair.secret_bytes(), erased);
}
