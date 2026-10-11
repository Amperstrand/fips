use super::TransportAddr;

#[test]
fn transport_addr_clone_shares_immutable_bytes() {
    let addr = TransportAddr::from_string("192.168.1.1:2121");
    let cloned = addr.clone();

    assert_eq!(addr, cloned);
    assert_eq!(addr.as_bytes().as_ptr(), cloned.as_bytes().as_ptr());
}

#[test]
fn transport_addr_equality_and_hash_use_byte_values() {
    // Only tests use std; the transport primitives remain alloc-only.
    use std::collections::HashSet;

    let original = TransportAddr::from_string("192.168.1.1:2121");
    let same_value = TransportAddr::from_bytes(b"192.168.1.1:2121");
    let mut addrs = HashSet::new();

    assert_ne!(original.as_bytes().as_ptr(), same_value.as_bytes().as_ptr());
    assert_eq!(original, same_value);
    addrs.insert(original);
    assert!(addrs.contains(&same_value));
}
