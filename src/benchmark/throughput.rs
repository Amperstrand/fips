//! Throughput benchmark handler — measures link capacity with bulk frame transfer.

use super::types::{ThroughputReport, ThroughputRequest, ThroughputStream};
use crate::NodeAddr;
use tracing::{debug, info, warn};

/// State tracked for an active throughput test.
#[derive(Clone, Debug)]
pub struct ThroughputTestState {
    pub test_id: u32,
    pub peer: NodeAddr,
    pub direction: u8,
    pub duration_secs: u8,
    pub frame_size: u16,
    pub rate_bps: u32,
    pub start_time: Option<std::time::Instant>,
    pub frames_sent: u32,
    pub frames_recv: u32,
    pub bytes_recv: u64,
    pub last_sequence: u32,
}

impl ThroughputTestState {
    pub fn new(peer: NodeAddr, request: &ThroughputRequest) -> Self {
        Self {
            test_id: request.test_id,
            peer,
            direction: request.direction,
            duration_secs: request.duration_secs,
            frame_size: request.frame_size,
            rate_bps: request.rate_bps,
            start_time: None,
            frames_sent: 0,
            frames_recv: 0,
            bytes_recv: 0,
            last_sequence: 0,
        }
    }
}

/// Results from a completed throughput test.
#[derive(Clone, Debug)]
pub struct ThroughputTestResult {
    pub test_id: u32,
    pub peer: NodeAddr,
    pub frames_sent: u32,
    pub frames_recv: u32,
    pub bytes_recv: u64,
    pub duration_us: u64,
    pub achieved_bps: u64,
    pub loss_rate: f64,
}

pub fn handle_throughput_request(
    from: &NodeAddr,
    body: &[u8],
) -> Option<ThroughputTestState> {
    let request = match ThroughputRequest::decode(body) {
        Ok(req) => req,
        Err(e) => {
            debug!(from = %from, error = %e, "Malformed ThroughputRequest");
            return None;
        }
    };

    if request.duration_secs == 0 {
        warn!(from = %from, test_id = request.test_id, "ThroughputRequest rejected: duration_secs is 0");
        return None;
    }
    if request.frame_size < 16 {
        warn!(from = %from, test_id = request.test_id, frame_size = request.frame_size, "ThroughputRequest rejected: frame_size < 16");
        return None;
    }
    if request.rate_bps == 0 {
        warn!(from = %from, test_id = request.test_id, "ThroughputRequest rejected: rate_bps is 0");
        return None;
    }
    if request.direction > 1 {
        warn!(from = %from, test_id = request.test_id, direction = request.direction, "ThroughputRequest rejected: invalid direction");
        return None;
    }

    let state = ThroughputTestState::new(*from, &request);

    debug!(
        from = %from,
        test_id = request.test_id,
        direction = request.direction,
        duration_secs = request.duration_secs,
        frame_size = request.frame_size,
        rate_bps = request.rate_bps,
        "ThroughputRequest accepted"
    );

    Some(state)
}

pub fn handle_throughput_stream(
    from: &NodeAddr,
    body: &[u8],
    state: &mut ThroughputTestState,
) -> bool {
    let stream = match ThroughputStream::decode(body) {
        Ok(s) => s,
        Err(e) => {
            debug!(from = %from, error = %e, "Malformed ThroughputStream");
            return false;
        }
    };

    if stream.test_id != state.test_id || *from != state.peer {
        debug!(
            from = %from,
            test_id = stream.test_id,
            expected_test_id = state.test_id,
            "ThroughputStream for unknown test"
        );
        return false;
    }

    if state.frames_recv == 0 {
        state.start_time = Some(std::time::Instant::now());
    }

    let data_len = stream.data.len() as u64;
    state.frames_recv += 1;
    state.bytes_recv += data_len;

    if stream.sequence > state.last_sequence + 1 && state.last_sequence > 0 {
        let gap = stream.sequence - state.last_sequence - 1;
        debug!(
            test_id = state.test_id,
            gap = gap,
            after_seq = state.last_sequence,
            "Sequence gap detected in ThroughputStream"
        );
    }
    state.last_sequence = stream.sequence;

    true
}

pub fn handle_throughput_report(from: &NodeAddr, body: &[u8]) -> Option<ThroughputTestResult> {
    let report = match ThroughputReport::decode(body) {
        Ok(r) => r,
        Err(e) => {
            debug!(from = %from, error = %e, "Malformed ThroughputReport");
            return None;
        }
    };

    let frames_sent = report.frames_sent;
    let frames_recv = report.frames_recv;
    let loss_rate = if frames_sent > 0 {
        1.0 - (frames_recv as f64 / frames_sent as f64)
    } else {
        0.0
    };

    info!(
        from = %from,
        test_id = report.test_id,
        frames_sent = frames_sent,
        frames_recv = frames_recv,
        bytes_recv = report.bytes_recv,
        duration_us = report.duration_us,
        achieved_bps = report.achieved_bps,
        loss_rate = format!("{:.1}%", loss_rate * 100.0),
        "ThroughputReport received"
    );

    Some(ThroughputTestResult {
        test_id: report.test_id,
        peer: *from,
        frames_sent,
        frames_recv,
        bytes_recv: report.bytes_recv,
        duration_us: report.duration_us,
        achieved_bps: report.achieved_bps,
        loss_rate,
    })
}
