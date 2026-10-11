use super::*;
use alloc::format;

#[test]
fn test_session_index_roundtrip() {
    let idx = SessionIndex::new(0x12345678);
    assert_eq!(idx.as_u32(), 0x12345678);

    let bytes = idx.to_le_bytes();
    assert_eq!(bytes, [0x78, 0x56, 0x34, 0x12]);

    let restored = SessionIndex::from_le_bytes(bytes);
    assert_eq!(restored, idx);
}

#[test]
fn test_session_index_display() {
    let idx = SessionIndex::new(0x000000ff);
    assert_eq!(format!("{}", idx), "000000ff");

    let idx = SessionIndex::new(0xdeadbeef);
    assert_eq!(format!("{}", idx), "deadbeef");
}
