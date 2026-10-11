use super::*;
use alloc::vec;

#[test]
fn test_common_prefix_parse() {
    let data = [0x00, 0x04, 0x20, 0x00]; // ver=0, phase=0, flags=SP, payload_len=32
    let prefix = CommonPrefix::parse(&data).unwrap();
    assert_eq!(prefix.version, 0);
    assert_eq!(prefix.phase, 0);
    assert_eq!(prefix.flags, FLAG_SP);
    assert_eq!(prefix.payload_len, 32);
}

#[test]
fn test_common_prefix_too_short() {
    assert!(CommonPrefix::parse(&[0, 0, 0]).is_none());
}

#[test]
fn test_encrypted_header_parse() {
    let receiver_idx = SessionIndex::new(0x12345678);
    let counter = 42u64;
    let flags = 0u8;
    let payload_len = 32u16; // 16 plaintext + 16 tag
    let ciphertext = vec![0xaa; 48]; // payload_len + TAG_SIZE

    let header = build_established_header(receiver_idx, counter, flags, payload_len);
    let packet = build_encrypted(&header, &ciphertext);

    assert_eq!(packet.len(), ESTABLISHED_HEADER_SIZE + 48);
    assert_eq!(packet[0], 0x00); // ver=0, phase=0

    let parsed = EncryptedHeader::parse(&packet).expect("should parse");
    assert_eq!(parsed.receiver_idx, receiver_idx);
    assert_eq!(parsed.counter, 42);
    assert_eq!(parsed.flags, 0);
    assert_eq!(parsed.payload_len, 32);
    assert_eq!(parsed.header_bytes, header);
    assert_eq!(parsed.ciphertext(&packet), &ciphertext[..]);
}

#[test]
fn test_encrypted_header_too_short() {
    let packet = vec![0x00; ENCRYPTED_MIN_SIZE - 1];
    assert!(EncryptedHeader::parse(&packet).is_none());
}

#[test]
fn test_encrypted_header_wrong_phase() {
    let mut packet = vec![0x00; ENCRYPTED_MIN_SIZE];
    packet[0] = 0x01; // phase 1 (msg1), not established
    assert!(EncryptedHeader::parse(&packet).is_none());
}

#[test]
fn test_encrypted_header_wrong_version() {
    let mut packet = vec![0x00; ENCRYPTED_MIN_SIZE];
    packet[0] = 0x10; // version 1, phase 0
    assert!(EncryptedHeader::parse(&packet).is_none());
}

#[test]
fn test_msg1_header_parse() {
    let sender_idx = SessionIndex::new(0xABCDEF01);
    let noise_msg1 = vec![0xbb; HANDSHAKE_MSG1_SIZE];

    let packet = build_msg1(sender_idx, &noise_msg1);

    assert_eq!(packet.len(), MSG1_WIRE_SIZE);
    assert_eq!(packet[0], 0x01); // ver=0, phase=1

    let header = Msg1Header::parse(&packet).expect("should parse");
    assert_eq!(header.sender_idx, sender_idx);
    assert_eq!(header.noise_msg1_offset, 8);
    assert_eq!(header.noise_msg1(&packet), &noise_msg1[..]);
}

#[test]
fn test_msg1_header_wrong_size() {
    let packet = vec![0x01; MSG1_WIRE_SIZE - 1];
    assert!(Msg1Header::parse(&packet).is_none());

    let packet = vec![0x01; MSG1_WIRE_SIZE + 1];
    assert!(Msg1Header::parse(&packet).is_none());
}

#[test]
fn test_msg1_header_wrong_phase() {
    let mut packet = vec![0x00; MSG1_WIRE_SIZE];
    packet[0] = 0x02; // phase 2, not phase 1
    assert!(Msg1Header::parse(&packet).is_none());
}

#[test]
fn test_msg1_header_nonzero_flags() {
    let mut packet = build_msg1(SessionIndex::new(1), &[0u8; HANDSHAKE_MSG1_SIZE]);
    packet[1] = 0x01; // flags must be zero during handshake
    assert!(Msg1Header::parse(&packet).is_none());
}

#[test]
fn test_msg2_header_parse() {
    let sender_idx = SessionIndex::new(0x11223344);
    let receiver_idx = SessionIndex::new(0x55667788);
    let noise_msg2 = vec![0xcc; HANDSHAKE_MSG2_SIZE];

    let packet = build_msg2(sender_idx, receiver_idx, &noise_msg2);

    assert_eq!(packet.len(), MSG2_WIRE_SIZE);
    assert_eq!(packet[0], 0x02); // ver=0, phase=2

    let header = Msg2Header::parse(&packet).expect("should parse");
    assert_eq!(header.sender_idx, sender_idx);
    assert_eq!(header.receiver_idx, receiver_idx);
    assert_eq!(header.noise_msg2_offset, 12);
    assert_eq!(header.noise_msg2(&packet), &noise_msg2[..]);
}

#[test]
fn test_msg2_header_wrong_size() {
    let packet = vec![0x02; MSG2_WIRE_SIZE - 1];
    assert!(Msg2Header::parse(&packet).is_none());

    let packet = vec![0x02; MSG2_WIRE_SIZE + 1];
    assert!(Msg2Header::parse(&packet).is_none());
}

#[test]
fn test_msg2_header_wrong_phase() {
    let mut packet = vec![0x00; MSG2_WIRE_SIZE];
    packet[0] = 0x00; // phase 0, not phase 2
    assert!(Msg2Header::parse(&packet).is_none());
}

#[test]
fn test_wire_sizes() {
    assert_eq!(MSG1_WIRE_SIZE, 114); // 4 + 4 + 106
    assert_eq!(MSG2_WIRE_SIZE, 69); // 4 + 4 + 4 + 57
    assert_eq!(ENCRYPTED_MIN_SIZE, 32); // 16 + 16
    assert_eq!(COMMON_PREFIX_SIZE, 4);
    assert_eq!(ESTABLISHED_HEADER_SIZE, 16);
    assert_eq!(INNER_HEADER_SIZE, 5);
}

#[test]
fn test_roundtrip_indices() {
    let idx = SessionIndex::new(0xDEADBEEF);

    let msg1 = build_msg1(idx, &[0u8; HANDSHAKE_MSG1_SIZE]);
    let parsed = Msg1Header::parse(&msg1).unwrap();
    assert_eq!(parsed.sender_idx.as_u32(), 0xDEADBEEF);

    // Verify little-endian encoding (sender_idx starts at offset 4)
    assert_eq!(msg1[4], 0xEF);
    assert_eq!(msg1[5], 0xBE);
    assert_eq!(msg1[6], 0xAD);
    assert_eq!(msg1[7], 0xDE);
}

#[test]
fn test_inner_header_prepend_strip() {
    let timestamp: u32 = 12345;
    let original = vec![0x10, 0xAA, 0xBB]; // msg_type + payload

    let with_header = prepend_inner_header(timestamp, &original);
    assert_eq!(with_header.len(), 4 + 3); // timestamp + original

    let (ts, rest) = strip_inner_header(&with_header).unwrap();
    assert_eq!(ts, 12345);
    assert_eq!(rest, &original[..]);
}

#[test]
fn test_inner_header_too_short() {
    assert!(strip_inner_header(&[0, 0, 0, 0]).is_none()); // needs 5 bytes minimum
}

#[test]
fn test_flags_byte() {
    let header = build_established_header(SessionIndex::new(1), 0, FLAG_KEY_EPOCH | FLAG_SP, 100);
    assert_eq!(header[1], 0x05); // bits 0 and 2 set

    let parsed = EncryptedHeader::parse(&[
        header[0], header[1], header[2], header[3], header[4], header[5], header[6], header[7],
        header[8], header[9], header[10], header[11], header[12], header[13], header[14],
        header[15], // minimum: TAG_SIZE bytes of ciphertext
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ])
    .unwrap();
    assert_eq!(parsed.flags & FLAG_KEY_EPOCH, FLAG_KEY_EPOCH);
    assert_eq!(parsed.flags & FLAG_CE, 0);
    assert_eq!(parsed.flags & FLAG_SP, FLAG_SP);
}

#[test]
fn test_payload_len_in_msg1() {
    let packet = build_msg1(SessionIndex::new(1), &[0u8; HANDSHAKE_MSG1_SIZE]);
    let prefix = CommonPrefix::parse(&packet).unwrap();
    // payload_len = sender_idx(4) + noise_msg1(106) = 110
    assert_eq!(prefix.payload_len, 110);
}

#[test]
fn test_payload_len_in_msg2() {
    let packet = build_msg2(
        SessionIndex::new(1),
        SessionIndex::new(2),
        &[0u8; HANDSHAKE_MSG2_SIZE],
    );
    let prefix = CommonPrefix::parse(&packet).unwrap();
    // payload_len = sender_idx(4) + receiver_idx(4) + noise_msg2(57) = 65
    assert_eq!(prefix.payload_len, 65);
}

#[test]
fn expected_payload_len_for_an_established_frame_excludes_header_and_tag() {
    let header = build_established_header(SessionIndex::new(7), 0, 0, 40);
    let frame = build_encrypted(&header, &[0u8; 40 + TAG_SIZE]);

    // 16 header + 40 plaintext + 16 tag = 72 on the wire, declaring 40.
    assert_eq!(frame.len(), ESTABLISHED_HEADER_SIZE + 40 + TAG_SIZE);
    assert_eq!(
        expected_payload_len(PHASE_ESTABLISHED, frame.len()),
        Some(40)
    );
}

#[test]
fn expected_payload_len_matches_what_build_msg1_and_build_msg2_actually_emit() {
    // The Noise buffers are sized with literals rather than
    // HANDSHAKE_MSG1_SIZE / HANDSHAKE_MSG2_SIZE, unlike the two tests
    // above. With the constant on both sides, changing it would move the
    // encoder's payload_len and the validator's expectation together and
    // leave this test green. Pinning the input keeps the two sides
    // independent so a one-sided change is caught.
    let msg1 = build_msg1(SessionIndex::new(1), &[0u8; 106]);
    let prefix = CommonPrefix::parse(&msg1).unwrap();
    assert_eq!(
        expected_payload_len(PHASE_MSG1, msg1.len()),
        Some(prefix.payload_len)
    );

    let msg2 = build_msg2(SessionIndex::new(1), SessionIndex::new(2), &[0u8; 57]);
    let prefix = CommonPrefix::parse(&msg2).unwrap();
    assert_eq!(
        expected_payload_len(PHASE_MSG2, msg2.len()),
        Some(prefix.payload_len)
    );
}
