//! Benchmark module — echo latency and throughput measurement.
//!
//! Feature-gated behind `#[cfg(feature = "benchmark")]`.

pub mod echo;
pub mod throughput;
pub mod types;

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crate::NodeAddr;
use echo::{EchoResult, compute_echo_stats};
use throughput::{
    Direction, ThroughputResult, ThroughputTestConfig, ThroughputTestState,
    build_throughput_request_frame, handle_throughput_stream, parse_throughput_report,
    parse_throughput_request,
};
use types::{MSG_ECHO_REQUEST, MSG_ECHO_RESPONSE, MSG_THROUGHPUT_REPORT, MSG_THROUGHPUT_STREAM};

use crate::peer_policy::PeerPolicy;
use tracing::{debug, info, warn};

const DEFAULT_ECHO_INTER_SEND_DELAY_MS: u64 = 100;

pub struct BenchmarkManager {
    active_tests: HashMap<u32, ThroughputTestState>,
    echo_results: HashMap<NodeAddr, Vec<EchoResult>>,
    echo_expected: HashMap<NodeAddr, u32>,
    next_test_id: u32,
    pending_echo_response: Option<(NodeAddr, Vec<u8>)>,
    pending_throughput_config: Option<(NodeAddr, ThroughputTestConfig)>,
    pending_throughput_report: Option<(NodeAddr, Vec<u8>)>,
    last_throughput_result: Option<(NodeAddr, ThroughputResult)>,
    pending_echo_sends: VecDeque<(NodeAddr, u32, Vec<u8>)>,
    echo_inter_send_delay_ms: u64,
    last_echo_send_time: Option<Instant>,
    echo_sent_at: Option<Instant>,
    pending_throughput_sends: VecDeque<(NodeAddr, Vec<u8>)>,
    throughput_send_interval_ms: u64,
    last_throughput_send_time: Option<Instant>,
    initiator_frames_sent: HashMap<u32, u32>,
    peer_policy: PeerPolicy,
}

impl BenchmarkManager {
    pub fn new() -> Self {
        Self {
            active_tests: HashMap::new(),
            echo_results: HashMap::new(),
            echo_expected: HashMap::new(),
            next_test_id: 1,
            pending_echo_response: None,
            pending_throughput_config: None,
            pending_throughput_report: None,
            last_throughput_result: None,
            pending_echo_sends: VecDeque::new(),
            echo_inter_send_delay_ms: DEFAULT_ECHO_INTER_SEND_DELAY_MS,
            last_echo_send_time: None,
            echo_sent_at: None,
            pending_throughput_sends: VecDeque::new(),
            throughput_send_interval_ms: 100,
            last_throughput_send_time: None,
            initiator_frames_sent: HashMap::new(),
            peer_policy: PeerPolicy::new(),
        }
    }

    pub fn allocate_test_id(&mut self) -> u32 {
        let id = self.next_test_id;
        self.next_test_id += 1;
        id
    }

    /// Dispatch an incoming link-layer benchmark message.
    ///
    /// `msg_type` is the first byte of the decrypted link payload.
    /// `payload` is everything after that first byte.
    pub fn handle_link_message(&mut self, from: &NodeAddr, msg_type: u8, payload: &[u8]) {
        if !self.peer_policy.check_frame_rate() {
            return;
        }

        match msg_type {
            MSG_ECHO_REQUEST => {
                debug!(peer = ?from, "Benchmark: echo request received");
                // Build echo response and store for dispatch layer to send.
                if let Some(frame) = echo::build_echo_response_frame(payload) {
                    self.pending_echo_response = Some((*from, frame));
                }
            }
            MSG_ECHO_RESPONSE => {
                debug!(peer = ?from, "Benchmark: echo response received");
                self.echo_sent_at = None;
                if let Some(result) = echo::handle_echo_response(payload) {
                    self.echo_results.entry(*from).or_default().push(result);
                }
            }
            types::MSG_THROUGHPUT_REQUEST => {
                if let Some(config) = parse_throughput_request(payload) {
                    info!(
                        peer = ?from,
                        test_id = config.test_id,
                        direction = ?config.direction,
                        duration = config.duration_secs,
                        "Benchmark: throughput request received"
                    );
                    let state = ThroughputTestState::new(config.test_id, config.duration_secs);
                    self.active_tests.insert(config.test_id, state);
                    self.pending_throughput_config = Some((*from, config));
                }
            }
            MSG_THROUGHPUT_STREAM => {
                if let Some((test_id, _, _)) = types::ThroughputStream::decode(payload) {
                    if let Some(state) = self.active_tests.get_mut(&test_id) {
                        handle_throughput_stream(state, payload);
                        if state.is_expired() {
                            let report_frame =
                                throughput::build_throughput_report_frame(state);
                            let peer = *from;
                            self.active_tests.remove(&test_id);
                            self.pending_throughput_report =
                                Some((peer, report_frame));
                        }
                    }
                }
            }
            MSG_THROUGHPUT_REPORT => {
                if let Some(mut result) = parse_throughput_report(payload) {
                    if let Some(initiator_sent) = self.initiator_frames_sent.remove(&result.test_id) {
                        result.frame_loss_rate = if initiator_sent > 0 {
                            1.0 - (result.frames_recv as f64 / initiator_sent as f64)
                        } else {
                            0.0
                        };
                        result.frames_sent = initiator_sent;
                    }
                    info!(
                        peer = ?from,
                        achieved_bps = result.achieved_bps,
                        loss_rate = format!("{:.1}%", result.frame_loss_rate * 100.0),
                        "Benchmark: throughput report received"
                    );
                    self.last_throughput_result = Some((*from, result));
                }
            }
            _ => {
                warn!(msg_type, "Benchmark: unknown benchmark message type");
            }
        }
    }

    pub fn take_echo_response(&mut self) -> Option<(NodeAddr, Vec<u8>)> {
        self.pending_echo_response.take()
    }

    pub fn take_throughput_config(&mut self) -> Option<(NodeAddr, ThroughputTestConfig)> {
        self.pending_throughput_config.take()
    }

    pub fn take_throughput_report(&mut self) -> Option<(NodeAddr, Vec<u8>)> {
        self.pending_throughput_report.take()
    }

    pub fn take_throughput_result(&mut self) -> Option<(NodeAddr, ThroughputResult)> {
        self.last_throughput_result.take()
    }

    pub fn last_throughput_result(&self) -> Option<&(NodeAddr, ThroughputResult)> {
        self.last_throughput_result.as_ref()
    }

    pub fn expect_echo_probes(&mut self, peer: NodeAddr, count: u32) {
        self.echo_expected.insert(peer, count);
    }

    pub fn take_echo_stats(&mut self, peer: &NodeAddr) -> Option<echo::EchoStats> {
        let results = self.echo_results.remove(peer)?;
        let expected = self.echo_expected.remove(peer).unwrap_or(results.len() as u32);
        Some(compute_echo_stats(results, expected as usize))
    }

    pub fn echo_results_ready(&self, peer: &NodeAddr) -> Option<bool> {
        let expected = self.echo_expected.get(peer)?;
        let current = self.echo_results.get(peer).map(|r| r.len()).unwrap_or(0);
        Some(current >= *expected as usize)
    }

    pub fn get_echo_stats(&self, peer: &NodeAddr) -> Option<&[EchoResult]> {
        self.echo_results.get(peer).map(|v| v.as_slice())
    }

    pub fn echo_expected_count(&self, peer: &NodeAddr) -> Option<u32> {
        self.echo_expected.get(peer).copied()
    }

    pub fn prepare_echo_test(
        &mut self,
        peer: NodeAddr,
        count: u32,
        payload_size: usize,
    ) -> Vec<Vec<u8>> {
        let payload = if payload_size > 0 {
            vec![0xAB; payload_size]
        } else {
            Vec::new()
        };
        self.expect_echo_probes(peer, count);
        self.echo_results.remove(&peer);
        (0..count)
            .map(|seq| echo::build_echo_request_frame(seq, &payload))
            .collect()
    }

    pub fn start_echo_test(&mut self, peer: NodeAddr, count: u32, payload_size: usize) {
        let payload = if payload_size > 0 {
            vec![0xAB; payload_size]
        } else {
            Vec::new()
        };
        self.expect_echo_probes(peer, count);
        self.echo_results.remove(&peer);
        self.last_echo_send_time = None;
        self.echo_sent_at = None;
        for seq in 0..count {
            self.pending_echo_sends.push_back((peer, seq, payload.clone()));
        }
    }

    const ECHO_RESPONSE_TIMEOUT_MS: u64 = 2000;

    pub fn poll_echo_sends(&mut self) -> Option<(NodeAddr, Vec<u8>)> {
        if self.pending_echo_sends.is_empty() {
            return None;
        }
        if let Some(sent_at) = self.echo_sent_at {
            let elapsed = sent_at.elapsed().as_millis() as u64;
            if elapsed < Self::ECHO_RESPONSE_TIMEOUT_MS {
                return None;
            }
            debug!(elapsed_ms = elapsed, "Benchmark: echo response timed out, sending next");
            self.echo_sent_at = None;
        }
        if let Some(last) = self.last_echo_send_time {
            let elapsed = last.elapsed().as_millis() as u64;
            if elapsed < self.echo_inter_send_delay_ms {
                return None;
            }
        }
        self.last_echo_send_time = Some(Instant::now());
        self.echo_sent_at = Some(Instant::now());
        let (peer, seq, payload) = self.pending_echo_sends.pop_front()?;
        // Build frame at send time so ts_us reflects actual transmission moment
        let frame = echo::build_echo_request_frame(seq, &payload);
        Some((peer, frame))
    }

    pub fn echo_sends_pending(&self) -> bool {
        !self.pending_echo_sends.is_empty()
    }

    pub fn start_throughput_sends(
        &mut self,
        peer: NodeAddr,
        test_id: u32,
        frame_size: u16,
        rate_bps: u32,
        duration_secs: u8,
    ) {
        let data_len = frame_size as usize;
        let interval_us = if rate_bps > 0 {
            ((data_len as u64) * 8 * 1_000_000) / rate_bps as u64
        } else {
            1000
        };
        let total_frames = (duration_secs as u64 * 1_000_000) / interval_us.max(1);

        self.throughput_send_interval_ms = (interval_us / 1000).max(1) as u64;
        self.last_throughput_send_time = None;
        self.pending_throughput_sends.clear();

        if let Some(state) = self.active_tests.get_mut(&test_id) {
            state.frames_sent = total_frames as u32;
        }
        self.initiator_frames_sent.insert(test_id, total_frames as u32);

        for seq in 0..total_frames {
            let frame = throughput::build_throughput_stream_frame(
                test_id,
                seq as u32,
                data_len,
            );
            self.pending_throughput_sends.push_back((peer, frame));
        }
    }

    /// Drain throughput frames that are due based on elapsed time.
    ///
    /// Returns a batch of (peer, frame) pairs to send immediately.
    /// Capped at 2 frames per drain to avoid BLE transport congestion.
    pub fn poll_throughput_sends(&mut self) -> Vec<(NodeAddr, Vec<u8>)> {
        if self.pending_throughput_sends.is_empty() {
            return Vec::new();
        }
        let elapsed_ms = self
            .last_throughput_send_time
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(u64::MAX);
        if elapsed_ms < self.throughput_send_interval_ms {
            return Vec::new();
        }
        let frames_due = if self.throughput_send_interval_ms > 0 {
            (elapsed_ms / self.throughput_send_interval_ms).max(1) as usize
        } else {
            1
        };
        let count = frames_due.min(2).min(self.pending_throughput_sends.len());
        let mut batch = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(item) = self.pending_throughput_sends.pop_front() {
                batch.push(item);
            }
        }
        if !batch.is_empty() {
            self.last_throughput_send_time = Some(Instant::now());
        }
        batch
    }

    pub fn throughput_sends_pending(&self) -> bool {
        !self.pending_throughput_sends.is_empty()
    }

    pub fn prepare_throughput_test(
        &mut self,
        direction: Direction,
        duration_secs: u8,
        frame_size: u16,
        rate_bps: u32,
    ) -> (u32, ThroughputTestConfig, Vec<u8>) {
        let test_id = self.allocate_test_id();
        let config = ThroughputTestConfig {
            test_id,
            direction,
            duration_secs,
            frame_size,
            rate_bps,
        };
        let frame = build_throughput_request_frame(&config);
        let state = ThroughputTestState::new(test_id, duration_secs);
        self.active_tests.insert(test_id, state);
        (test_id, config, frame)
    }

    pub fn active_test_mut(&mut self, test_id: u32) -> Option<&mut ThroughputTestState> {
        self.active_tests.get_mut(&test_id)
    }

    pub fn remove_test(&mut self, test_id: u32) -> Option<ThroughputTestState> {
        self.active_tests.remove(&test_id)
    }
}

impl Default for BenchmarkManager {
    fn default() -> Self {
        Self::new()
    }
}
