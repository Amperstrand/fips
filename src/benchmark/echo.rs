//! Echo benchmark: round-trip latency measurement.

use crate::benchmark::types::{EchoRequest, EchoResponse, now_micros, MSG_ECHO_RESPONSE};

/// Result of a single echo measurement.
#[derive(Clone, Debug)]
pub struct EchoResult {
    /// Round-trip time in microseconds.
    pub rtt_us: u64,
    /// Sequence number of this probe.
    pub seq: u32,
    /// Payload length echoed back.
    pub payload_len: usize,
}

/// Aggregate statistics over a set of echo results.
#[derive(Clone, Debug)]
pub struct EchoStats {
    /// Individual results, sorted by sequence number.
    pub results: Vec<EchoResult>,
    /// Minimum RTT in microseconds.
    pub min_us: u64,
    /// Maximum RTT in microseconds.
    pub max_us: u64,
    /// Mean RTT in microseconds.
    pub mean_us: u64,
    /// Median RTT in microseconds.
    pub median_us: u64,
    /// 95th percentile RTT in microseconds.
    pub p95_us: u64,
    /// Number of missing sequences (lost probes).
    pub loss_count: usize,
    /// Jitter (mean absolute deviation of RTTs) in microseconds.
    pub jitter_us: u64,
}

/// Build an echo request wire frame (msg_type byte + encoded body).
pub fn build_echo_request_frame(seq: u32, payload: &[u8]) -> Vec<u8> {
    let req = EchoRequest::new(seq, payload.to_vec());
    let mut frame = vec![super::types::MSG_ECHO_REQUEST];
    frame.extend(req.encode());
    frame
}

/// Build an echo response wire frame for a received echo request body.
///
/// `request_body` is the payload *after* the msg_type byte.
/// Returns `None` if the request body is malformed.
pub fn build_echo_response_frame(request_body: &[u8]) -> Option<Vec<u8>> {
    let req = EchoRequest::decode(request_body)?;
    let recv_ts = now_micros();
    let resp = EchoResponse::from_request(&req, recv_ts);
    let mut frame = vec![MSG_ECHO_RESPONSE];
    frame.extend(resp.encode());
    Some(frame)
}

/// Process a received echo response and compute the RTT.
///
/// `response_body` is the payload *after* the msg_type byte.
/// Returns `None` if the response is malformed.
pub fn handle_echo_response(response_body: &[u8]) -> Option<EchoResult> {
    let resp = EchoResponse::decode(response_body)?;
    let now = now_micros();
    let rtt_us = now.saturating_sub(resp.send_ts);
    Some(EchoResult {
        rtt_us,
        seq: resp.seq,
        payload_len: resp.payload.len(),
    })
}

/// Compute aggregate statistics from a sorted list of echo results.
///
/// `total_expected` is the number of requests sent (to compute loss).
pub fn compute_echo_stats(results: Vec<EchoResult>, total_expected: usize) -> EchoStats {
    if results.is_empty() {
        return EchoStats {
            results: Vec::new(),
            min_us: 0,
            max_us: 0,
            mean_us: 0,
            median_us: 0,
            p95_us: 0,
            loss_count: total_expected,
            jitter_us: 0,
        };
    }

    let mut rtts: Vec<u64> = results.iter().map(|r| r.rtt_us).collect();
    rtts.sort();

    let min_us = rtts[0];
    let max_us = rtts[rtts.len() - 1];
    let sum: u64 = rtts.iter().sum();
    let mean_us = sum / rtts.len() as u64;

    let median_us = if rtts.len() % 2 == 1 {
        rtts[rtts.len() / 2]
    } else {
        (rtts[rtts.len() / 2 - 1] + rtts[rtts.len() / 2]) / 2
    };

    let p95_idx = ((rtts.len() as f64) * 0.95).ceil() as usize;
    let p95_us = rtts[p95_idx.min(rtts.len()) - 1];

    let loss_count = total_expected.saturating_sub(results.len());

    let jitter_us = if rtts.len() > 1 {
        let mean = mean_us as i64;
        let deviation_sum: u64 = rtts
            .iter()
            .map(|&r| (r as i64 - mean).unsigned_abs())
            .sum();
        deviation_sum / rtts.len() as u64
    } else {
        0
    };

    EchoStats {
        results,
        min_us,
        max_us,
        mean_us,
        median_us,
        p95_us,
        loss_count,
        jitter_us,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_echo_request() {
        let frame = build_echo_request_frame(1, &[0xAA, 0xBB]);
        assert_eq!(frame[0], super::super::types::MSG_ECHO_REQUEST);
        let body = &frame[1..];
        let decoded = EchoRequest::decode(body).unwrap();
        assert_eq!(decoded.seq, 1);
        assert_eq!(decoded.payload, vec![0xAA, 0xBB]);
    }

    #[test]
    fn build_echo_response_from_request() {
        let req_frame = build_echo_request_frame(42, &[]);
        let req_body = &req_frame[1..];
        let resp_frame = build_echo_response_frame(req_body).unwrap();
        assert_eq!(resp_frame[0], MSG_ECHO_RESPONSE);
        let resp_body = &resp_frame[1..];
        let resp = EchoResponse::decode(resp_body).unwrap();
        assert_eq!(resp.seq, 42);
    }

    #[test]
    fn compute_stats_basic() {
        let results = vec![
            EchoResult { rtt_us: 100, seq: 0, payload_len: 0 },
            EchoResult { rtt_us: 200, seq: 1, payload_len: 0 },
            EchoResult { rtt_us: 300, seq: 2, payload_len: 0 },
        ];
        let stats = compute_echo_stats(results, 5);
        assert_eq!(stats.min_us, 100);
        assert_eq!(stats.max_us, 300);
        assert_eq!(stats.mean_us, 200);
        assert_eq!(stats.median_us, 200);
        assert_eq!(stats.loss_count, 2);
        assert!(stats.jitter_us > 0);
    }

    #[test]
    fn compute_stats_empty() {
        let stats = compute_echo_stats(vec![], 10);
        assert_eq!(stats.loss_count, 10);
        assert_eq!(stats.min_us, 0);
    }
}
