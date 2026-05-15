//! Throughput benchmark: bandwidth measurement.

use crate::benchmark::types::{
    DIRECTION_UPLOAD, ThroughputReport, ThroughputRequest, ThroughputStream,
    MSG_THROUGHPUT_REPORT, MSG_THROUGHPUT_REQUEST, MSG_THROUGHPUT_STREAM,
    now_micros,
};

/// Throughput test direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

/// Configuration for a throughput test.
#[derive(Clone, Debug)]
pub struct ThroughputTestConfig {
    pub test_id: u32,
    pub direction: Direction,
    pub duration_secs: u8,
    pub frame_size: u16,
    pub rate_bps: u32,
}

/// State tracker for an active throughput test.
#[derive(Clone, Debug)]
pub struct ThroughputTestState {
    pub test_id: u32,
    pub duration_secs: u8,
    pub frames_sent: u32,
    pub frames_recv: u32,
    pub bytes_recv: u64,
    pub start_us: u64,
    pub expected_seq: u32,
    pub gap_count: u32,
}

impl ThroughputTestState {
    pub fn new(test_id: u32, duration_secs: u8) -> Self {
        Self {
            test_id,
            duration_secs,
            frames_sent: 0,
            frames_recv: 0,
            bytes_recv: 0,
            start_us: now_micros(),
            expected_seq: 0,
            gap_count: 0,
        }
    }

    pub fn is_expired(&self) -> bool {
        let elapsed_us = now_micros().saturating_sub(self.start_us);
        let target_us = (self.duration_secs as u64) * 1_000_000;
        elapsed_us >= target_us
    }
}

/// Final result of a throughput test.
#[derive(Clone, Debug)]
pub struct ThroughputResult {
    pub test_id: u32,
    pub achieved_bps: u64,
    pub frame_loss_rate: f64,
    pub total_bytes: u64,
    pub duration_us: u64,
    pub frames_sent: u32,
    pub frames_recv: u32,
}

/// Build a throughput request wire frame.
pub fn build_throughput_request_frame(config: &ThroughputTestConfig) -> Vec<u8> {
    let req = ThroughputRequest {
        test_id: config.test_id,
        direction: match config.direction {
            Direction::Upload => DIRECTION_UPLOAD,
            Direction::Download => super::types::DIRECTION_DOWNLOAD,
        },
        duration_secs: config.duration_secs,
        frame_size: config.frame_size,
        rate_bps: config.rate_bps,
    };
    let mut frame = vec![MSG_THROUGHPUT_REQUEST];
    frame.extend(req.encode());
    frame
}

/// Parse a throughput request from a wire body (after msg_type byte).
pub fn parse_throughput_request(body: &[u8]) -> Option<ThroughputTestConfig> {
    let req = ThroughputRequest::decode(body)?;
    Some(ThroughputTestConfig {
        test_id: req.test_id,
        direction: if req.direction == DIRECTION_UPLOAD {
            Direction::Upload
        } else {
            Direction::Download
        },
        duration_secs: req.duration_secs,
        frame_size: req.frame_size,
        rate_bps: req.rate_bps,
    })
}

/// Build a throughput stream frame with pseudorandom fill data.
pub fn build_throughput_stream_frame(test_id: u32, seq: u32, data_len: usize) -> Vec<u8> {
    let data = generate_stream_data(test_id, seq, data_len);
    let stream = ThroughputStream {
        test_id,
        seq,
        data,
    };
    let mut frame = vec![MSG_THROUGHPUT_STREAM];
    frame.extend(stream.encode());
    frame
}

/// Process a received throughput stream frame.
///
/// Updates the test state with frame/byte counts and detects sequence gaps.
/// Returns `false` if the frame doesn't match an active test.
pub fn handle_throughput_stream(
    state: &mut ThroughputTestState,
    body: &[u8],
) -> bool {
    let (test_id, seq, data) = match ThroughputStream::decode(body) {
        Some(t) => t,
        None => return false,
    };
    if test_id != state.test_id {
        return false;
    }

    state.frames_recv += 1;
    state.bytes_recv += data.len() as u64;

    if seq > state.expected_seq {
        state.gap_count += seq - state.expected_seq;
    }
    state.expected_seq = seq + 1;

    true
}

/// Build a throughput report wire frame from test state.
pub fn build_throughput_report_frame(state: &ThroughputTestState) -> Vec<u8> {
    let now = now_micros();
    let duration_us = now.saturating_sub(state.start_us);
    let achieved_bps = if duration_us > 0 {
        (state.bytes_recv * 8 * 1_000_000) / duration_us
    } else {
        0
    };

    let report = ThroughputReport {
        test_id: state.test_id,
        frames_sent: state.frames_sent,
        frames_recv: state.frames_recv,
        bytes_recv: state.bytes_recv,
        duration_us,
        achieved_bps,
    };
    let mut frame = vec![MSG_THROUGHPUT_REPORT];
    frame.extend(report.encode());
    frame
}

/// Parse a throughput report from a wire body and compute final result.
pub fn parse_throughput_report(body: &[u8]) -> Option<ThroughputResult> {
    let report = ThroughputReport::decode(body)?;
    let frame_loss_rate = if report.frames_sent > 0 {
        1.0 - (report.frames_recv as f64 / report.frames_sent as f64)
    } else {
        0.0
    };
    Some(ThroughputResult {
        test_id: report.test_id,
        achieved_bps: report.achieved_bps,
        frame_loss_rate,
        total_bytes: report.bytes_recv,
        duration_us: report.duration_us,
        frames_sent: report.frames_sent,
        frames_recv: report.frames_recv,
    })
}

/// Generate pseudorandom stream data using a simple LCG seeded from test_id and seq.
pub fn generate_stream_data(test_id: u32, seq: u32, len: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(len);
    let mut state: u32 = test_id.wrapping_mul(2654435761) ^ seq;
    for _ in 0..len {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        data.push((state >> 24) as u8);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_request() {
        let config = ThroughputTestConfig {
            test_id: 42,
            direction: Direction::Upload,
            duration_secs: 5,
            frame_size: 256,
            rate_bps: 40000,
        };
        let frame = build_throughput_request_frame(&config);
        assert_eq!(frame[0], MSG_THROUGHPUT_REQUEST);
        let parsed = parse_throughput_request(&frame[1..]).unwrap();
        assert_eq!(parsed.test_id, 42);
        assert_eq!(parsed.direction, Direction::Upload);
        assert_eq!(parsed.duration_secs, 5);
    }

    #[test]
    fn stream_frame_roundtrip() {
        let frame = build_throughput_stream_frame(1, 10, 64);
        assert_eq!(frame[0], MSG_THROUGHPUT_STREAM);
        let (test_id, seq, data) = ThroughputStream::decode(&frame[1..]).unwrap();
        assert_eq!(test_id, 1);
        assert_eq!(seq, 10);
        assert_eq!(data.len(), 64);
    }

    #[test]
    fn handle_stream_tracks_state() {
        let mut state = ThroughputTestState::new(1, 5);
        let body = {
            let frame = build_throughput_stream_frame(1, 0, 32);
            frame[1..].to_vec()
        };
        assert!(handle_throughput_stream(&mut state, &body));
        assert_eq!(state.frames_recv, 1);
        assert_eq!(state.bytes_recv, 32);
        assert_eq!(state.expected_seq, 1);
    }

    #[test]
    fn handle_stream_detects_gaps() {
        let mut state = ThroughputTestState::new(1, 5);
        state.expected_seq = 0;

        let body = {
            let frame = build_throughput_stream_frame(1, 5, 16);
            frame[1..].to_vec()
        };
        assert!(handle_throughput_stream(&mut state, &body));
        assert_eq!(state.gap_count, 5);
    }

    #[test]
    fn handle_stream_rejects_wrong_test() {
        let mut state = ThroughputTestState::new(1, 5);
        let body = {
            let frame = build_throughput_stream_frame(99, 0, 16);
            frame[1..].to_vec()
        };
        assert!(!handle_throughput_stream(&mut state, &body));
    }

    #[test]
    fn generate_data_deterministic() {
        let a = generate_stream_data(1, 1, 32);
        let b = generate_stream_data(1, 1, 32);
        assert_eq!(a, b);
        let c = generate_stream_data(2, 1, 32);
        assert_ne!(a, c);
    }

    #[test]
    fn report_roundtrip() {
        let state = ThroughputTestState {
            test_id: 7,
            duration_secs: 5,
            frames_sent: 100,
            frames_recv: 95,
            bytes_recv: 24320,
            start_us: now_micros() - 5_000_000,
            expected_seq: 100,
            gap_count: 5,
        };
        let frame = build_throughput_report_frame(&state);
        assert_eq!(frame[0], MSG_THROUGHPUT_REPORT);
        let result = parse_throughput_report(&frame[1..]).unwrap();
        assert_eq!(result.test_id, 7);
        assert_eq!(result.frames_sent, 100);
        assert_eq!(result.frames_recv, 95);
        assert!((result.frame_loss_rate - 0.05).abs() < 0.001);
    }
}
