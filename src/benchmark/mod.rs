//! Experimental link benchmark protocol.
//!
//! Measures RTT, loss, jitter, and throughput between directly connected
//! FIPS peers. Feature-gated behind `#[cfg(feature = "benchmark")]`.

pub mod echo;
pub mod throughput;
pub mod types;

use crate::NodeAddr;
use std::collections::HashMap;

pub struct DownloadStreamPlan {
    pub peer: NodeAddr,
    pub test_id: u32,
    pub duration_secs: u8,
    pub frame_size: u16,
    pub rate_bps: u32,
}

pub struct BenchmarkManager {
    throughput_tests: HashMap<(NodeAddr, u32), throughput::ThroughputTestState>,
    pending_echo_results: Vec<echo::BenchmarkEvent>,
    throughput_results: Vec<throughput::ThroughputTestResult>,
}

impl BenchmarkManager {
    pub fn new() -> Self {
        Self {
            throughput_tests: HashMap::new(),
            pending_echo_results: Vec::new(),
            throughput_results: Vec::new(),
        }
    }

    pub fn handle_echo_request(&mut self, from: &NodeAddr, body: &[u8]) -> Option<Vec<u8>> {
        echo::handle_echo_request(from, body)
    }

    pub fn handle_echo_response(&mut self, from: &NodeAddr, body: &[u8]) {
        if let Some(event) = echo::handle_echo_response(from, body) {
            self.pending_echo_results.push(event);
        }
    }

    pub fn handle_throughput_request(&mut self, from: &NodeAddr, body: &[u8]) -> Option<Option<DownloadStreamPlan>> {
        let state = throughput::handle_throughput_request(from, body)?;
        let download_plan = if state.direction == 0 {
            Some(DownloadStreamPlan {
                peer: state.peer,
                test_id: state.test_id,
                duration_secs: state.duration_secs,
                frame_size: state.frame_size,
                rate_bps: state.rate_bps,
            })
        } else {
            None
        };
        let key = (state.peer, state.test_id);
        self.throughput_tests.insert(key, state);
        Some(download_plan)
    }

    pub fn handle_throughput_stream(&mut self, from: &NodeAddr, body: &[u8]) {
        let stream = match types::ThroughputStream::decode(body) {
            Ok(s) => s,
            Err(_) => return,
        };
        let key = (*from, stream.test_id);
        if let Some(state) = self.throughput_tests.get_mut(&key) {
            throughput::handle_throughput_stream(from, body, state);
        }
    }

    pub fn handle_throughput_report(&mut self, from: &NodeAddr, body: &[u8]) {
        if let Some(mut result) = throughput::handle_throughput_report(from, body) {
            let key = (result.peer, result.test_id);
            if let Some(state) = self.throughput_tests.remove(&key) {
                result.frames_recv = state.frames_recv;
                result.bytes_recv = state.bytes_recv;
                if result.frames_sent > 0 {
                    result.loss_rate = 1.0 - (result.frames_recv as f64 / result.frames_sent as f64);
                }
                if let Some(start) = state.start_time {
                    let recv_elapsed = start.elapsed();
                    result.duration_us = recv_elapsed.as_micros() as u64;
                }
                if result.duration_us > 0 {
                    result.achieved_bps = result.bytes_recv * 8 * 1_000_000 / result.duration_us;
                }
            }
            self.throughput_results.push(result);
        }
    }

    pub fn drain_echo_results(&mut self) -> Vec<echo::BenchmarkEvent> {
        std::mem::take(&mut self.pending_echo_results)
    }

    pub fn drain_throughput_results(&mut self) -> Vec<throughput::ThroughputTestResult> {
        std::mem::take(&mut self.throughput_results)
    }

    pub fn active_tests(&self) -> usize {
        self.throughput_tests.len()
    }

    pub fn handle_link_message(
        &mut self,
        from: &NodeAddr,
        msg_type: u8,
        payload: &[u8],
    ) -> (Option<Vec<u8>>, Option<DownloadStreamPlan>) {
        match msg_type {
            0xFF => (self.handle_echo_request(from, payload), None),
            0xFE => {
                self.handle_echo_response(from, payload);
                (None, None)
            }
            0xFD => {
                match self.handle_throughput_request(from, payload) {
                    Some(plan) => (None, plan),
                    None => (None, None),
                }
            }
            0xFC => {
                self.handle_throughput_stream(from, payload);
                (None, None)
            }
            0xFB => {
                self.handle_throughput_report(from, payload);
                (None, None)
            }
            _ => {
                tracing::debug!(msg_type, "Unknown benchmark message type");
                (None, None)
            }
        }
    }
}

impl Default for BenchmarkManager {
    fn default() -> Self {
        Self::new()
    }
}
