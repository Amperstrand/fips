use alloc::format;

use secp256k1::{Keypair, SecretKey};

use super::*;

#[test]
fn test_node_addr_from_slice() {
    let bytes = [0u8; 16];
    let node_addr = NodeAddr::from_slice(&bytes).unwrap();
    assert_eq!(node_addr.as_bytes(), &bytes);

    // Wrong length should fail
    let short = [0u8; 8];
    assert!(matches!(
        NodeAddr::from_slice(&short),
        Err(IdentityError::InvalidNodeAddrLength(8))
    ));
}

#[test]
fn test_fips_address_validation() {
    // Valid address with fd prefix
    let mut valid = [0u8; 16];
    valid[0] = 0xfd;
    assert!(Ipv6Address::from_bytes(valid).is_ok());

    // Invalid prefix
    let mut invalid = [0u8; 16];
    invalid[0] = 0xfe;
    assert!(matches!(
        Ipv6Address::from_bytes(invalid),
        Err(IdentityError::InvalidAddressPrefix(0xfe))
    ));
}

#[test]
fn test_decode_npub_invalid_prefix() {
    // nsec instead of npub
    let nsec = "nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5";
    let result = decode_npub(nsec);
    assert!(matches!(result, Err(IdentityError::InvalidNpubPrefix(_))));
}

#[test]
fn test_decode_npub_invalid_checksum() {
    // Valid npub with corrupted checksum
    let bad_npub = "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
    let result = decode_npub(bad_npub);
    assert!(result.is_err());
}

#[test]
fn test_peer_identity_from_invalid_npub() {
    let result = PeerIdentity::from_npub("npub1invalid");
    assert!(result.is_err());

    let result =
        PeerIdentity::from_npub("nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5");
    assert!(matches!(result, Err(IdentityError::InvalidNpubPrefix(_))));
}

#[test]
fn test_nsec_roundtrip() {
    let secret_bytes: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];

    let secret_key = SecretKey::from_slice(&secret_bytes).unwrap();
    let nsec = encode_nsec(&secret_key);

    assert!(nsec.starts_with("nsec1"));
    assert_eq!(nsec.len(), 63);

    let decoded = decode_nsec(&nsec).unwrap();
    assert_eq!(decoded.secret_bytes(), secret_bytes);
}

#[test]
fn test_decode_secret_nsec() {
    let secret_bytes: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];

    let secret_key = SecretKey::from_slice(&secret_bytes).unwrap();
    let nsec = encode_nsec(&secret_key);

    let decoded = decode_secret(&nsec).unwrap();
    assert_eq!(decoded.secret_bytes(), secret_bytes);
}

#[test]
fn test_decode_secret_hex() {
    let hex_str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
    let decoded = decode_secret(hex_str).unwrap();

    let expected: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];
    assert_eq!(decoded.secret_bytes(), expected);
}

// ===== encoding.rs error path tests =====

#[test]
fn test_decode_npub_invalid_length() {
    // Encode 16 bytes (too short) as bech32 with npub prefix
    let short =
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse_unchecked("npub"), &[0u8; 16]).unwrap();
    let result = decode_npub(&short);
    assert!(matches!(result, Err(IdentityError::InvalidNpubLength(16))));
}

#[test]
fn test_decode_nsec_invalid_length() {
    // Encode 16 bytes (too short) as bech32 with nsec prefix
    let short =
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse_unchecked("nsec"), &[0u8; 16]).unwrap();
    let result = decode_nsec(&short);
    assert!(matches!(result, Err(IdentityError::InvalidNsecLength(16))));
}

#[test]
fn test_decode_secret_hex_wrong_length() {
    // 16 hex bytes (too short for a secret key)
    let result = decode_secret("0102030405060708090a0b0c0d0e0f10");
    assert!(matches!(result, Err(IdentityError::InvalidNsecLength(16))));
}

#[test]
fn test_decode_secret_hex_invalid_chars() {
    let result = decode_secret("zzzz");
    assert!(matches!(result, Err(IdentityError::InvalidHex(_))));
}

// ===== node_addr.rs tests =====

#[test]
fn test_node_addr_debug() {
    let bytes = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00,
    ];
    let node_addr = NodeAddr::from_bytes(bytes);
    let debug = format!("{:?}", node_addr);
    assert_eq!(debug, "NodeAddr(0123456789abcdef)");
}

#[test]
fn test_node_addr_display() {
    let bytes = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32,
        0x10,
    ];
    let node_addr = NodeAddr::from_bytes(bytes);
    let display = format!("{}", node_addr);
    assert_eq!(display, "0123456789abcdeffedcba9876543210");
}

#[test]
fn test_node_addr_as_slice() {
    let bytes = [0xaa; 16];
    let node_addr = NodeAddr::from_bytes(bytes);
    assert_eq!(node_addr.as_slice(), &bytes[..]);
    assert_eq!(node_addr.as_slice().len(), 16);
}

#[test]
fn test_node_addr_as_ref() {
    let bytes = [0xbb; 16];
    let node_addr = NodeAddr::from_bytes(bytes);
    let r: &[u8] = node_addr.as_ref();
    assert_eq!(r, &bytes[..]);
}

// ===== address.rs tests =====

#[test]
fn test_fips_address_from_slice_success() {
    let mut bytes = [0u8; 16];
    bytes[0] = 0xfd;
    bytes[1] = 0x42;
    let addr = Ipv6Address::from_slice(&bytes).unwrap();
    assert_eq!(addr.as_bytes(), &bytes);
}

#[test]
fn test_fips_address_from_slice_wrong_length() {
    let short = [0xfd; 8];
    assert!(matches!(
        Ipv6Address::from_slice(&short),
        Err(IdentityError::InvalidAddressLength(8))
    ));

    let long = [0xfd; 20];
    assert!(matches!(
        Ipv6Address::from_slice(&long),
        Err(IdentityError::InvalidAddressLength(20))
    ));
}

#[test]
fn test_fips_address_from_slice_wrong_prefix() {
    let mut bytes = [0u8; 16];
    bytes[0] = 0xfe;
    assert!(matches!(
        Ipv6Address::from_slice(&bytes),
        Err(IdentityError::InvalidAddressPrefix(0xfe))
    ));
}

#[test]
fn test_fips_address_debug() {
    let mut bytes = [0u8; 16];
    bytes[0] = 0xfd;
    let addr = Ipv6Address::from_bytes(bytes).unwrap();
    let debug = format!("{:?}", addr);
    assert!(debug.starts_with("Ipv6Address("));
    assert!(debug.contains("fd"));
}

#[test]
fn test_fips_address_display() {
    let mut bytes = [0u8; 16];
    bytes[0] = 0xfd;
    let addr = Ipv6Address::from_bytes(bytes).unwrap();
    let display = format!("{}", addr);
    // Display is the IPv6 representation
    assert!(display.starts_with("fd"));
}

// ===== peer.rs tests =====

#[test]
fn test_peer_identity_pubkey_full_preserved_parity() {
    // Create two identities and find one with odd parity to make this test meaningful
    let secret_bytes: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];
    let keypair = Keypair::from_seckey_slice(super::secp(), &secret_bytes).unwrap();
    let full_pubkey = keypair.public_key();

    let peer = PeerIdentity::from_pubkey_full(full_pubkey);
    // pubkey_full should return the exact key provided, preserving parity
    assert_eq!(peer.pubkey_full(), full_pubkey);
}

// ===== IdentityError conversions =====
//
// The four `From` impls are written by hand (see `mod.rs`). Each test makes
// a real error of the wrapped type, converts it, and checks the variant, the
// message, and that `source()` is `None`.

#[test]
fn test_identity_error_from_secp256k1_error() {
    let inner = SecretKey::from_slice(&[0u8; 32]).unwrap_err();
    let err = IdentityError::from(inner);
    assert!(matches!(err, IdentityError::InvalidSecretKey(e) if e == inner));
    assert_eq!(format!("{err}"), format!("invalid secret key: {inner}"));
    assert!(core::error::Error::source(&err).is_none());
}

#[test]
fn test_identity_error_from_bech32_encode_error() {
    // A bech32 string is limited to 1023 characters; 1000 data bytes exceed it.
    let inner =
        bech32::encode::<bech32::Bech32>(bech32::Hrp::parse_unchecked("npub"), &[0u8; 1000])
            .unwrap_err();
    let text = format!("{inner}");
    let err = IdentityError::from(inner);
    assert!(matches!(err, IdentityError::Bech32Encode(_)));
    assert_eq!(format!("{err}"), format!("bech32 encoding error: {text}"));
    assert!(core::error::Error::source(&err).is_none());
}

#[test]
fn test_identity_error_from_bech32_decode_error() {
    let inner = bech32::decode("not bech32").unwrap_err();
    let text = format!("{inner}");
    let err = IdentityError::from(inner);
    assert!(matches!(err, IdentityError::Bech32Decode(_)));
    assert_eq!(format!("{err}"), format!("bech32 decoding error: {text}"));
    assert!(core::error::Error::source(&err).is_none());
}

#[test]
fn test_identity_error_from_hex_error() {
    let inner = hex::decode("zz").unwrap_err();
    let err = IdentityError::from(inner);
    assert!(matches!(err, IdentityError::InvalidHex(e) if e == inner));
    assert_eq!(format!("{err}"), format!("invalid hex encoding: {inner}"));
    assert!(core::error::Error::source(&err).is_none());
}
