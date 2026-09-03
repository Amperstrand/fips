//! Wire types for the FIPS benchmark protocol.
//!
//! Encodes and decodes benchmark messages (0xFB-0xFF) matching the
//! microfips-core wire format exactly. All multi-byte integers are
//! little-endian.

use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Message type constants — must match microfips-core/src/wire.rs
// ---------------------------------------------------------------------------

/// Echo request — round-trip latency probe.
pub const MSG_ECHO_REQUEST: u8 = 0xFF;
/// Echo response — reply to an echo request.
pub const MSG_ECHO_RESPONSE: u8 = 0xFE;
/// Throughput request — start a throughput measurement.
pub const MSG_THROUGHPUT_REQUEST: u8 = 0xFD;
/// Throughput stream — data frame during a throughput test.
pub const MSG_THROUGHPUT_STREAM: u8 = 0xFC;
/// Throughput report — final summary of a throughput test.
pub const MSG_THROUGHPUT_REPORT: u8 = 0xFB;

// ---------------------------------------------------------------------------
// Minimum / exact sizes — must match microfips-core/src/wire.rs
// ---------------------------------------------------------------------------

/// Minimum echo request body: 8 (ts_us) + 4 (seq).
pub const ECHO_REQUEST_MIN_SIZE: usize = 12;
/// Minimum echo response body: 8 (send_ts) + 8 (recv_ts) + 4 (seq).
pub const ECHO_RESPONSE_MIN_SIZE: usize = 20;
/// Maximum echo payload bytes.
pub const ECHO_MAX_PAYLOAD: usize = 256;
/// Exact throughput request body: 4 (test_id) + 1 (direction) + 1 (duration)
/// + 2 (frame_size) + 4 (rate_bps).
pub const THROUGHPUT_REQUEST_SIZE: usize = 12;
/// Minimum throughput stream body: 4 (test_id) + 4 (seq).
pub const THROUGHPUT_STREAM_MIN_SIZE: usize = 8;
/// Exact throughput report body: 4 (test_id) + 4 (frames_sent)
/// + 4 (frames_recv) + 8 (bytes_recv) + 8 (duration_us) + 8 (achieved_bps).
pub const THROUGHPUT_REPORT_SIZE: usize = 36;

// ---------------------------------------------------------------------------
// Throughput direction constant
// ---------------------------------------------------------------------------

/// Upload direction (client sends stream to peer).
pub const DIRECTION_UPLOAD: u8 = 0;
/// Download direction (peer sends stream to client).
pub const DIRECTION_DOWNLOAD: u8 = 1;

// ---------------------------------------------------------------------------
// EchoRequest
// ---------------------------------------------------------------------------

/// Echo request: timestamp (micros since epoch), sequence number, payload.
#[derive(Clone, Debug)]
pub struct EchoRequest {
    /// Microseconds since Unix epoch when the request was sent.
    pub ts_us: u64,
    /// Monotonically increasing sequence number.
    pub seq: u32,
    /// Optional payload bytes.
    pub payload: Vec<u8>,
}

impl EchoRequest {
    /// Build a new echo request with the current timestamp.
    pub fn new(seq: u32, payload: Vec<u8>) -> Self {
        Self {
            ts_us: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_micros() as u64)
                .unwrap_or(0),
            seq,
            payload,
        }
    }

    /// Encode to wire format: `[ts_us:u64 LE][seq:u32 LE][payload:variable]`.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(ECHO_REQUEST_MIN_SIZE + self.payload.len());
        buf.extend_from_slice(&self.ts_us.to_le_bytes());
        buf.extend_from_slice(&self.seq.to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Decode from wire format. Returns `None` if the buffer is too short.
    pub fn decode(body: &[u8]) -> Option<Self> {
        if body.len() < ECHO_REQUEST_MIN_SIZE {
            return None;
        }
        let ts_us = u64::from_le_bytes(body[0..8].try_into().ok()?);
        let seq = u32::from_le_bytes(body[8..12].try_into().ok()?);
        let payload = body[12..].to_vec();
        Some(Self {
            ts_us,
            seq,
            payload,
        })
    }
}

// ---------------------------------------------------------------------------
// EchoResponse
// ---------------------------------------------------------------------------

/// Echo response: original send timestamp, receive timestamp, sequence, payload.
#[derive(Clone, Debug)]
pub struct EchoResponse {
    /// Original send timestamp from the request (micros since epoch).
    pub send_ts: u64,
    /// Timestamp when the peer received the request (micros since epoch).
    pub recv_ts: u64,
    /// Sequence number echoed from the request.
    pub seq: u32,
    /// Payload echoed from the request.
    pub payload: Vec<u8>,
}

impl EchoResponse {
    /// Build an echo response from a received request.
    pub fn from_request(req: &EchoRequest, recv_ts: u64) -> Self {
        Self {
            send_ts: req.ts_us,
            recv_ts,
            seq: req.seq,
            payload: req.payload.clone(),
        }
    }

    /// Encode to wire format:
    /// `[send_ts:u64 LE][recv_ts:u64 LE][seq:u32 LE][payload:variable]`.
    pub fn encode(&self) -> Vec<u8> {
        let needed = ECHO_RESPONSE_MIN_SIZE + self.payload.len();
        let mut buf = Vec::with_capacity(needed);
        buf.extend_from_slice(&self.send_ts.to_le_bytes());
        buf.extend_from_slice(&self.recv_ts.to_le_bytes());
        buf.extend_from_slice(&self.seq.to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Encode into a pre-allocated buffer. Returns the number of bytes written.
    pub fn encode_into(&self, out: &mut [u8]) -> Option<usize> {
        let needed = ECHO_RESPONSE_MIN_SIZE + self.payload.len();
        if out.len() < needed || self.payload.len() > ECHO_MAX_PAYLOAD {
            return None;
        }
        out[0..8].copy_from_slice(&self.send_ts.to_le_bytes());
        out[8..16].copy_from_slice(&self.recv_ts.to_le_bytes());
        out[16..20].copy_from_slice(&self.seq.to_le_bytes());
        out[20..needed].copy_from_slice(&self.payload);
        Some(needed)
    }

    /// Decode from wire format. Returns `None` if the buffer is too short.
    pub fn decode(body: &[u8]) -> Option<Self> {
        if body.len() < ECHO_RESPONSE_MIN_SIZE {
            return None;
        }
        let send_ts = u64::from_le_bytes(body[0..8].try_into().ok()?);
        let recv_ts = u64::from_le_bytes(body[8..16].try_into().ok()?);
        let seq = u32::from_le_bytes(body[16..20].try_into().ok()?);
        let payload = body[20..].to_vec();
        Some(Self {
            send_ts,
            recv_ts,
            seq,
            payload,
        })
    }
}

// ---------------------------------------------------------------------------
// ThroughputRequest
// ---------------------------------------------------------------------------

/// Throughput request: start a throughput measurement.
#[derive(Clone, Debug)]
pub struct ThroughputRequest {
    /// Unique test identifier.
    pub test_id: u32,
    /// Direction: 0 = upload, 1 = download.
    pub direction: u8,
    /// Test duration in seconds.
    pub duration_secs: u8,
    /// Frame size in bytes.
    pub frame_size: u16,
    /// Target rate in bits per second.
    pub rate_bps: u32,
}

impl ThroughputRequest {
    /// Encode to wire format:
    /// `[test_id:u32 LE][direction:u8][duration_secs:u8][frame_size:u16 LE][rate_bps:u32 LE]`.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(THROUGHPUT_REQUEST_SIZE);
        buf.extend_from_slice(&self.test_id.to_le_bytes());
        buf.push(self.direction);
        buf.push(self.duration_secs);
        buf.extend_from_slice(&self.frame_size.to_le_bytes());
        buf.extend_from_slice(&self.rate_bps.to_le_bytes());
        buf
    }

    /// Decode from wire format. Returns `None` if the buffer is too short.
    pub fn decode(body: &[u8]) -> Option<Self> {
        if body.len() < THROUGHPUT_REQUEST_SIZE {
            return None;
        }
        let test_id = u32::from_le_bytes(body[0..4].try_into().ok()?);
        let direction = body[4];
        let duration_secs = body[5];
        let frame_size = u16::from_le_bytes(body[6..8].try_into().ok()?);
        let rate_bps = u32::from_le_bytes(body[8..12].try_into().ok()?);
        Some(Self {
            test_id,
            direction,
            duration_secs,
            frame_size,
            rate_bps,
        })
    }
}

// ---------------------------------------------------------------------------
// ThroughputStream
// ---------------------------------------------------------------------------

/// Throughput stream: a single data frame during a throughput test.
#[derive(Clone, Debug)]
pub struct ThroughputStream {
    /// Test identifier.
    pub test_id: u32,
    /// Frame sequence number.
    pub seq: u32,
    /// Data payload.
    pub data: Vec<u8>,
}

impl ThroughputStream {
    /// Encode to wire format: `[test_id:u32 LE][seq:u32 LE][data:variable]`.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(THROUGHPUT_STREAM_MIN_SIZE + self.data.len());
        buf.extend_from_slice(&self.test_id.to_le_bytes());
        buf.extend_from_slice(&self.seq.to_le_bytes());
        buf.extend_from_slice(&self.data);
        buf
    }

    /// Decode from wire format. Returns `None` if the buffer is too short.
    /// Returns the (test_id, seq, data) tuple.
    pub fn decode(body: &[u8]) -> Option<(u32, u32, &[u8])> {
        if body.len() < THROUGHPUT_STREAM_MIN_SIZE {
            return None;
        }
        let test_id = u32::from_le_bytes(body[0..4].try_into().ok()?);
        let seq = u32::from_le_bytes(body[4..8].try_into().ok()?);
        Some((test_id, seq, &body[8..]))
    }
}

// ---------------------------------------------------------------------------
// ThroughputReport
// ---------------------------------------------------------------------------

/// Throughput report: final summary of a completed throughput test.
#[derive(Clone, Debug)]
pub struct ThroughputReport {
    /// Test identifier.
    pub test_id: u32,
    /// Number of frames sent by the sender.
    pub frames_sent: u32,
    /// Number of frames received by the receiver.
    pub frames_recv: u32,
    /// Total bytes received.
    pub bytes_recv: u64,
    /// Test duration in microseconds.
    pub duration_us: u64,
    /// Achieved throughput in bits per second.
    pub achieved_bps: u64,
}

impl ThroughputReport {
    /// Encode to wire format:
    /// `[test_id:u32 LE][frames_sent:u32 LE][frames_recv:u32 LE]`
    /// `[bytes_recv:u64 LE][duration_us:u64 LE][achieved_bps:u64 LE]`.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = vec![0u8; THROUGHPUT_REPORT_SIZE];
        buf[0..4].copy_from_slice(&self.test_id.to_le_bytes());
        buf[4..8].copy_from_slice(&self.frames_sent.to_le_bytes());
        buf[8..12].copy_from_slice(&self.frames_recv.to_le_bytes());
        buf[12..20].copy_from_slice(&self.bytes_recv.to_le_bytes());
        buf[20..28].copy_from_slice(&self.duration_us.to_le_bytes());
        buf[28..36].copy_from_slice(&self.achieved_bps.to_le_bytes());
        buf
    }

    /// Encode into a pre-allocated buffer. Returns the number of bytes written.
    pub fn encode_into(&self, out: &mut [u8]) -> Option<usize> {
        if out.len() < THROUGHPUT_REPORT_SIZE {
            return None;
        }
        out[0..4].copy_from_slice(&self.test_id.to_le_bytes());
        out[4..8].copy_from_slice(&self.frames_sent.to_le_bytes());
        out[8..12].copy_from_slice(&self.frames_recv.to_le_bytes());
        out[12..20].copy_from_slice(&self.bytes_recv.to_le_bytes());
        out[20..28].copy_from_slice(&self.duration_us.to_le_bytes());
        out[28..36].copy_from_slice(&self.achieved_bps.to_le_bytes());
        Some(THROUGHPUT_REPORT_SIZE)
    }

    /// Decode from wire format. Returns `None` if the buffer is too short.
    pub fn decode(body: &[u8]) -> Option<Self> {
        if body.len() < THROUGHPUT_REPORT_SIZE {
            return None;
        }
        let test_id = u32::from_le_bytes(body[0..4].try_into().ok()?);
        let frames_sent = u32::from_le_bytes(body[4..8].try_into().ok()?);
        let frames_recv = u32::from_le_bytes(body[8..12].try_into().ok()?);
        let bytes_recv = u64::from_le_bytes(body[12..20].try_into().ok()?);
        let duration_us = u64::from_le_bytes(body[20..28].try_into().ok()?);
        let achieved_bps = u64::from_le_bytes(body[28..36].try_into().ok()?);
        Some(Self {
            test_id,
            frames_sent,
            frames_recv,
            bytes_recv,
            duration_us,
            achieved_bps,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the current time as microseconds since the Unix epoch.
pub fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_request_roundtrip() {
        let req = EchoRequest {
            ts_us: 1_700_000_000_000_000,
            seq: 42,
            payload: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };
        let encoded = req.encode();
        let decoded = EchoRequest::decode(&encoded).unwrap();
        assert_eq!(decoded.ts_us, req.ts_us);
        assert_eq!(decoded.seq, req.seq);
        assert_eq!(decoded.payload, req.payload);
    }

    #[test]
    fn echo_request_min_payload() {
        let req = EchoRequest {
            ts_us: 0,
            seq: 0,
            payload: vec![],
        };
        let encoded = req.encode();
        assert_eq!(encoded.len(), ECHO_REQUEST_MIN_SIZE);
        let decoded = EchoRequest::decode(&encoded).unwrap();
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn echo_request_too_short() {
        assert!(EchoRequest::decode(&[0u8; 11]).is_none());
    }

    #[test]
    fn echo_response_roundtrip() {
        let resp = EchoResponse {
            send_ts: 100,
            recv_ts: 200,
            seq: 7,
            payload: vec![1, 2, 3],
        };
        let encoded = resp.encode();
        let decoded = EchoResponse::decode(&encoded).unwrap();
        assert_eq!(decoded.send_ts, 100);
        assert_eq!(decoded.recv_ts, 200);
        assert_eq!(decoded.seq, 7);
        assert_eq!(decoded.payload, vec![1, 2, 3]);
    }

    #[test]
    fn throughput_request_roundtrip() {
        let req = ThroughputRequest {
            test_id: 1234,
            direction: DIRECTION_UPLOAD,
            duration_secs: 5,
            frame_size: 256,
            rate_bps: 40000,
        };
        let encoded = req.encode();
        assert_eq!(encoded.len(), THROUGHPUT_REQUEST_SIZE);
        let decoded = ThroughputRequest::decode(&encoded).unwrap();
        assert_eq!(decoded.test_id, 1234);
        assert_eq!(decoded.direction, DIRECTION_UPLOAD);
        assert_eq!(decoded.duration_secs, 5);
        assert_eq!(decoded.frame_size, 256);
        assert_eq!(decoded.rate_bps, 40000);
    }

    #[test]
    fn throughput_stream_roundtrip() {
        let stream = ThroughputStream {
            test_id: 99,
            seq: 1000,
            data: vec![0xAA; 64],
        };
        let encoded = stream.encode();
        let (test_id, seq, data) = ThroughputStream::decode(&encoded).unwrap();
        assert_eq!(test_id, 99);
        assert_eq!(seq, 1000);
        assert_eq!(data.len(), 64);
    }

    #[test]
    fn throughput_report_roundtrip() {
        let report = ThroughputReport {
            test_id: 7,
            frames_sent: 100,
            frames_recv: 95,
            bytes_recv: 24320,
            duration_us: 5_000_000,
            achieved_bps: 2_000_000,
        };
        let encoded = report.encode();
        assert_eq!(encoded.len(), THROUGHPUT_REPORT_SIZE);
        let decoded = ThroughputReport::decode(&encoded).unwrap();
        assert_eq!(decoded.test_id, 7);
        assert_eq!(decoded.frames_sent, 100);
        assert_eq!(decoded.frames_recv, 95);
        assert_eq!(decoded.bytes_recv, 24320);
        assert_eq!(decoded.duration_us, 5_000_000);
        assert_eq!(decoded.achieved_bps, 2_000_000);
    }

    #[test]
    fn throughput_report_encode_into() {
        let report = ThroughputReport {
            test_id: 1,
            frames_sent: 10,
            frames_recv: 8,
            bytes_recv: 2048,
            duration_us: 1000000,
            achieved_bps: 16384,
        };
        let mut buf = vec![0u8; THROUGHPUT_REPORT_SIZE];
        let len = report.encode_into(&mut buf).unwrap();
        assert_eq!(len, THROUGHPUT_REPORT_SIZE);
        let decoded = ThroughputReport::decode(&buf).unwrap();
        assert_eq!(decoded.test_id, 1);
    }

    #[test]
    fn now_micros_is_reasonable() {
        let us = now_micros();
        // Should be after 2020 and before 2100
        assert!(us > 1577836800000000);
        assert!(us < 4102444800000000);
    }
}
