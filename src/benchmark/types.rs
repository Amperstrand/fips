//! Benchmark protocol wire format types: echo RTT and throughput testing.

use crate::protocol::ProtocolError;

// Minimum body sizes for variable-length messages (after msg_type byte stripped).
pub const ECHO_REQUEST_MIN_BODY_SIZE: usize = 12; // 8 + 4
pub const ECHO_RESPONSE_MIN_BODY_SIZE: usize = 20; // 8 + 8 + 4
pub const THROUGHPUT_STREAM_MIN_BODY_SIZE: usize = 8; // 4 + 4

// Fixed body sizes.
pub const THROUGHPUT_REQUEST_BODY_SIZE: usize = 12; // 4 + 1 + 1 + 2 + 4
pub const THROUGHPUT_REPORT_BODY_SIZE: usize = 36; // 4 + 4 + 4 + 8 + 8 + 8

// ============================================================================
// Echo Request
// ============================================================================

/// Benchmark echo request for RTT measurement.
///
/// ## Wire Format (body after msg_type byte stripped)
///
/// ```text
/// [0..7]   timestamp_us: u64 LE  — sender's microsecond timestamp
/// [8..11]  sequence: u32 LE      — sequential counter for loss detection
/// [12..]   payload: variable     — optional padding bytes
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EchoRequest {
    pub timestamp_us: u64,
    pub sequence: u32,
    pub payload: Vec<u8>,
}

impl EchoRequest {
    pub fn new(timestamp_us: u64, sequence: u32) -> Self {
        Self {
            timestamp_us,
            sequence,
            payload: Vec::new(),
        }
    }

    pub fn with_payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = payload;
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(ECHO_REQUEST_MIN_BODY_SIZE + self.payload.len());
        buf.extend_from_slice(&self.timestamp_us.to_le_bytes());
        buf.extend_from_slice(&self.sequence.to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    pub fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        if body.len() < ECHO_REQUEST_MIN_BODY_SIZE {
            return Err(ProtocolError::MessageTooShort {
                expected: ECHO_REQUEST_MIN_BODY_SIZE,
                got: body.len(),
            });
        }
        let timestamp_us = u64::from_le_bytes(body[0..8].try_into().unwrap());
        let sequence = u32::from_le_bytes(body[8..12].try_into().unwrap());
        let payload = body[12..].to_vec();
        Ok(Self {
            timestamp_us,
            sequence,
            payload,
        })
    }
}

// ============================================================================
// Echo Response
// ============================================================================

/// Benchmark echo response for RTT measurement.
///
/// ## Wire Format (body after msg_type byte stripped)
///
/// ```text
/// [0..7]   send_timestamp_us: u64 LE  — original request timestamp
/// [8..15]  recv_timestamp_us: u64 LE  — when responder processed request
/// [16..19] sequence: u32 LE          — echoed back from request
/// [20..]   payload: variable         — echoed back from request
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EchoResponse {
    pub send_timestamp_us: u64,
    pub recv_timestamp_us: u64,
    pub sequence: u32,
    pub payload: Vec<u8>,
}

impl EchoResponse {
    pub fn new(send_timestamp_us: u64, recv_timestamp_us: u64, sequence: u32) -> Self {
        Self {
            send_timestamp_us,
            recv_timestamp_us,
            sequence,
            payload: Vec::new(),
        }
    }

    pub fn with_payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = payload;
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(ECHO_RESPONSE_MIN_BODY_SIZE + self.payload.len());
        buf.extend_from_slice(&self.send_timestamp_us.to_le_bytes());
        buf.extend_from_slice(&self.recv_timestamp_us.to_le_bytes());
        buf.extend_from_slice(&self.sequence.to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    pub fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        if body.len() < ECHO_RESPONSE_MIN_BODY_SIZE {
            return Err(ProtocolError::MessageTooShort {
                expected: ECHO_RESPONSE_MIN_BODY_SIZE,
                got: body.len(),
            });
        }
        let send_timestamp_us = u64::from_le_bytes(body[0..8].try_into().unwrap());
        let recv_timestamp_us = u64::from_le_bytes(body[8..16].try_into().unwrap());
        let sequence = u32::from_le_bytes(body[16..20].try_into().unwrap());
        let payload = body[20..].to_vec();
        Ok(Self {
            send_timestamp_us,
            recv_timestamp_us,
            sequence,
            payload,
        })
    }
}

// ============================================================================
// Throughput Request
// ============================================================================

/// Benchmark throughput test negotiation.
///
/// ## Wire Format (body after msg_type byte stripped, 12 bytes fixed)
///
/// ```text
/// [0..3]   test_id: u32 LE       — unique test identifier
/// [4]      direction: u8         — 0 = download, 1 = upload
/// [5]      duration_secs: u8     — test duration in seconds
/// [6..7]   frame_size: u16 LE    — requested frame payload size
/// [8..11]  rate_bps: u32 LE      — target bitrate in bps
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThroughputRequest {
    pub test_id: u32,
    pub direction: u8,
    pub duration_secs: u8,
    pub frame_size: u16,
    pub rate_bps: u32,
}

impl ThroughputRequest {
    pub fn new(
        test_id: u32,
        direction: u8,
        duration_secs: u8,
        frame_size: u16,
        rate_bps: u32,
    ) -> Self {
        Self {
            test_id,
            direction,
            duration_secs,
            frame_size,
            rate_bps,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(THROUGHPUT_REQUEST_BODY_SIZE);
        buf.extend_from_slice(&self.test_id.to_le_bytes());
        buf.push(self.direction);
        buf.push(self.duration_secs);
        buf.extend_from_slice(&self.frame_size.to_le_bytes());
        buf.extend_from_slice(&self.rate_bps.to_le_bytes());
        buf
    }

    pub fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        if body.len() < THROUGHPUT_REQUEST_BODY_SIZE {
            return Err(ProtocolError::MessageTooShort {
                expected: THROUGHPUT_REQUEST_BODY_SIZE,
                got: body.len(),
            });
        }
        let test_id = u32::from_le_bytes(body[0..4].try_into().unwrap());
        let direction = body[4];
        let duration_secs = body[5];
        let frame_size = u16::from_le_bytes(body[6..8].try_into().unwrap());
        let rate_bps = u32::from_le_bytes(body[8..12].try_into().unwrap());
        Ok(Self {
            test_id,
            direction,
            duration_secs,
            frame_size,
            rate_bps,
        })
    }
}

// ============================================================================
// Throughput Stream
// ============================================================================

/// Benchmark throughput bulk data frame.
///
/// ## Wire Format (body after msg_type byte stripped)
///
/// ```text
/// [0..3]   test_id: u32 LE  — matches the request
/// [4..7]   sequence: u32 LE — frame counter
/// [8..]    data: variable   — bulk payload bytes
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThroughputStream {
    pub test_id: u32,
    pub sequence: u32,
    pub data: Vec<u8>,
}

impl ThroughputStream {
    pub fn new(test_id: u32, sequence: u32) -> Self {
        Self {
            test_id,
            sequence,
            data: Vec::new(),
        }
    }

    pub fn with_data(mut self, data: Vec<u8>) -> Self {
        self.data = data;
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(THROUGHPUT_STREAM_MIN_BODY_SIZE + self.data.len());
        buf.extend_from_slice(&self.test_id.to_le_bytes());
        buf.extend_from_slice(&self.sequence.to_le_bytes());
        buf.extend_from_slice(&self.data);
        buf
    }

    pub fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        if body.len() < THROUGHPUT_STREAM_MIN_BODY_SIZE {
            return Err(ProtocolError::MessageTooShort {
                expected: THROUGHPUT_STREAM_MIN_BODY_SIZE,
                got: body.len(),
            });
        }
        let test_id = u32::from_le_bytes(body[0..4].try_into().unwrap());
        let sequence = u32::from_le_bytes(body[4..8].try_into().unwrap());
        let data = body[8..].to_vec();
        Ok(Self {
            test_id,
            sequence,
            data,
        })
    }
}

// ============================================================================
// Throughput Report
// ============================================================================

/// Benchmark throughput test final report.
///
/// ## Wire Format (body after msg_type byte stripped, 36 bytes fixed)
///
/// ```text
/// [0..3]    test_id: u32 LE       — matches the request
/// [4..7]    frames_sent: u32 LE   — total frames sent
/// [8..11]   frames_recv: u32 LE   — total frames received
/// [12..19]  bytes_recv: u64 LE    — total bytes received
/// [20..27]  duration_us: u64 LE   — actual test duration in microseconds
/// [28..35]  achieved_bps: u64 LE  — achieved bitrate in bps
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThroughputReport {
    pub test_id: u32,
    pub frames_sent: u32,
    pub frames_recv: u32,
    pub bytes_recv: u64,
    pub duration_us: u64,
    pub achieved_bps: u64,
}

impl ThroughputReport {
    pub fn new(
        test_id: u32,
        frames_sent: u32,
        frames_recv: u32,
        bytes_recv: u64,
        duration_us: u64,
        achieved_bps: u64,
    ) -> Self {
        Self {
            test_id,
            frames_sent,
            frames_recv,
            bytes_recv,
            duration_us,
            achieved_bps,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(THROUGHPUT_REPORT_BODY_SIZE);
        buf.extend_from_slice(&self.test_id.to_le_bytes());
        buf.extend_from_slice(&self.frames_sent.to_le_bytes());
        buf.extend_from_slice(&self.frames_recv.to_le_bytes());
        buf.extend_from_slice(&self.bytes_recv.to_le_bytes());
        buf.extend_from_slice(&self.duration_us.to_le_bytes());
        buf.extend_from_slice(&self.achieved_bps.to_le_bytes());
        buf
    }

    pub fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        if body.len() < THROUGHPUT_REPORT_BODY_SIZE {
            return Err(ProtocolError::MessageTooShort {
                expected: THROUGHPUT_REPORT_BODY_SIZE,
                got: body.len(),
            });
        }
        let test_id = u32::from_le_bytes(body[0..4].try_into().unwrap());
        let frames_sent = u32::from_le_bytes(body[4..8].try_into().unwrap());
        let frames_recv = u32::from_le_bytes(body[8..12].try_into().unwrap());
        let bytes_recv = u64::from_le_bytes(body[12..20].try_into().unwrap());
        let duration_us = u64::from_le_bytes(body[20..28].try_into().unwrap());
        let achieved_bps = u64::from_le_bytes(body[28..36].try_into().unwrap());
        Ok(Self {
            test_id,
            frames_sent,
            frames_recv,
            bytes_recv,
            duration_us,
            achieved_bps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_echo_request_roundtrip_no_payload() {
        let req = EchoRequest::new(1_700_000_000_000_000, 42);
        let encoded = req.encode();
        let decoded = EchoRequest::decode(&encoded).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn test_echo_request_roundtrip_with_payload() {
        let req =
            EchoRequest::new(1_700_000_000_000_000, 7).with_payload(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        let encoded = req.encode();
        assert_eq!(encoded.len(), ECHO_REQUEST_MIN_BODY_SIZE + 4);
        let decoded = EchoRequest::decode(&encoded).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn test_echo_request_too_short() {
        assert!(EchoRequest::decode(&[]).is_err());
        assert!(EchoRequest::decode(&[0u8; 4]).is_err());
        assert!(EchoRequest::decode(&[0u8; 11]).is_err());
    }

    #[test]
    fn test_echo_request_min_body_valid() {
        let body = vec![0u8; ECHO_REQUEST_MIN_BODY_SIZE];
        let decoded = EchoRequest::decode(&body).unwrap();
        assert_eq!(decoded.timestamp_us, 0);
        assert_eq!(decoded.sequence, 0);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn test_echo_response_roundtrip_no_payload() {
        let resp = EchoResponse::new(1_000_000, 1_000_100, 42);
        let encoded = resp.encode();
        let decoded = EchoResponse::decode(&encoded).unwrap();
        assert_eq!(decoded, resp);
    }

    #[test]
    fn test_echo_response_roundtrip_with_payload() {
        let resp = EchoResponse::new(500, 600, 1).with_payload(vec![1, 2, 3, 4, 5]);
        let encoded = resp.encode();
        assert_eq!(encoded.len(), ECHO_RESPONSE_MIN_BODY_SIZE + 5);
        let decoded = EchoResponse::decode(&encoded).unwrap();
        assert_eq!(decoded, resp);
    }

    #[test]
    fn test_echo_response_too_short() {
        assert!(EchoResponse::decode(&[]).is_err());
        assert!(EchoResponse::decode(&[0u8; 8]).is_err());
        assert!(EchoResponse::decode(&[0u8; 19]).is_err());
    }

    #[test]
    fn test_echo_response_min_body_valid() {
        let body = vec![0u8; ECHO_RESPONSE_MIN_BODY_SIZE];
        let decoded = EchoResponse::decode(&body).unwrap();
        assert_eq!(decoded.send_timestamp_us, 0);
        assert_eq!(decoded.recv_timestamp_us, 0);
        assert_eq!(decoded.sequence, 0);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn test_throughput_request_roundtrip() {
        let req = ThroughputRequest::new(0xABCD_1234, 0, 10, 1024, 80_000);
        let encoded = req.encode();
        assert_eq!(encoded.len(), THROUGHPUT_REQUEST_BODY_SIZE);
        let decoded = ThroughputRequest::decode(&encoded).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn test_throughput_request_upload_direction() {
        let req = ThroughputRequest::new(1, 1, 30, 2048, 1_000_000);
        let encoded = req.encode();
        let decoded = ThroughputRequest::decode(&encoded).unwrap();
        assert_eq!(decoded.direction, 1);
        assert_eq!(decoded, req);
    }

    #[test]
    fn test_throughput_request_too_short() {
        assert!(ThroughputRequest::decode(&[]).is_err());
        assert!(ThroughputRequest::decode(&[0u8; 4]).is_err());
        assert!(ThroughputRequest::decode(&[0u8; 11]).is_err());
    }

    #[test]
    fn test_throughput_request_exact_size() {
        let body = vec![0u8; THROUGHPUT_REQUEST_BODY_SIZE];
        let decoded = ThroughputRequest::decode(&body).unwrap();
        assert_eq!(decoded.test_id, 0);
        assert_eq!(decoded.direction, 0);
        assert_eq!(decoded.duration_secs, 0);
        assert_eq!(decoded.frame_size, 0);
        assert_eq!(decoded.rate_bps, 0);
    }

    #[test]
    fn test_throughput_stream_roundtrip_no_data() {
        let stream = ThroughputStream::new(42, 0);
        let encoded = stream.encode();
        assert_eq!(encoded.len(), THROUGHPUT_STREAM_MIN_BODY_SIZE);
        let decoded = ThroughputStream::decode(&encoded).unwrap();
        assert_eq!(decoded, stream);
    }

    #[test]
    fn test_throughput_stream_roundtrip_with_data() {
        let stream = ThroughputStream::new(99, 500).with_data(vec![0xFF; 1024]);
        let encoded = stream.encode();
        assert_eq!(encoded.len(), THROUGHPUT_STREAM_MIN_BODY_SIZE + 1024);
        let decoded = ThroughputStream::decode(&encoded).unwrap();
        assert_eq!(decoded, stream);
    }

    #[test]
    fn test_throughput_stream_too_short() {
        assert!(ThroughputStream::decode(&[]).is_err());
        assert!(ThroughputStream::decode(&[0u8; 4]).is_err());
        assert!(ThroughputStream::decode(&[0u8; 7]).is_err());
    }

    #[test]
    fn test_throughput_stream_min_body_valid() {
        let body = vec![0u8; THROUGHPUT_STREAM_MIN_BODY_SIZE];
        let decoded = ThroughputStream::decode(&body).unwrap();
        assert_eq!(decoded.test_id, 0);
        assert_eq!(decoded.sequence, 0);
        assert!(decoded.data.is_empty());
    }

    #[test]
    fn test_throughput_report_roundtrip() {
        let report = ThroughputReport::new(0x1337, 10000, 9998, 10_239_744, 10_000_000, 8_191_795);
        let encoded = report.encode();
        assert_eq!(encoded.len(), THROUGHPUT_REPORT_BODY_SIZE);
        let decoded = ThroughputReport::decode(&encoded).unwrap();
        assert_eq!(decoded, report);
    }

    #[test]
    fn test_throughput_report_zero_values() {
        let report = ThroughputReport::new(0, 0, 0, 0, 0, 0);
        let encoded = report.encode();
        let decoded = ThroughputReport::decode(&encoded).unwrap();
        assert_eq!(decoded, report);
    }

    #[test]
    fn test_throughput_report_max_values() {
        let report =
            ThroughputReport::new(u32::MAX, u32::MAX, u32::MAX, u64::MAX, u64::MAX, u64::MAX);
        let encoded = report.encode();
        let decoded = ThroughputReport::decode(&encoded).unwrap();
        assert_eq!(decoded, report);
    }

    #[test]
    fn test_throughput_report_too_short() {
        assert!(ThroughputReport::decode(&[]).is_err());
        assert!(ThroughputReport::decode(&[0u8; 16]).is_err());
        assert!(ThroughputReport::decode(&[0u8; 35]).is_err());
    }

    #[test]
    fn test_echo_request_wire_layout() {
        let req =
            EchoRequest::new(0x0102_0304_0506_0708, 0x0A0B_0C0D).with_payload(vec![0xAA, 0xBB]);
        let wire = req.encode();
        assert_eq!(wire[0..8], [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
        assert_eq!(wire[8..12], [0x0D, 0x0C, 0x0B, 0x0A]);
        assert_eq!(wire[12..14], [0xAA, 0xBB]);
    }

    #[test]
    fn test_throughput_request_wire_layout() {
        let req = ThroughputRequest::new(0x1122_3344, 1, 30, 0x0200, 0x00FF_FFFF);
        let wire = req.encode();
        assert_eq!(wire.len(), 12);
        assert_eq!(wire[0..4], [0x44, 0x33, 0x22, 0x11]);
        assert_eq!(wire[4], 1);
        assert_eq!(wire[5], 30);
        assert_eq!(wire[6..8], [0x00, 0x02]);
        assert_eq!(wire[8..12], [0xFF, 0xFF, 0xFF, 0x00]);
    }
}
