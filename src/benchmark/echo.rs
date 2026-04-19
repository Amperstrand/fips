//! Echo benchmark handler — measures RTT, loss, and jitter.

use super::types::{EchoRequest, EchoResponse};
use crate::protocol::LinkMessageType;
use crate::NodeAddr;
use tracing::debug;

/// Events produced by echo benchmark handlers.
#[derive(Clone, Debug)]
pub enum BenchmarkEvent {
    EchoResponseReceived {
        from: NodeAddr,
        rtt_us: u64,
        send_timestamp_us: u64,
        recv_timestamp_us: u64,
        sequence: u32,
        payload_len: usize,
    },
}

/// Handle an incoming EchoRequest: decode, stamp recv time, build response.
///
/// Returns encoded bytes ready to send via `send_encrypted_link_message`
/// (includes the `EchoResponse` msg_type prefix byte).
pub fn handle_echo_request(from: &NodeAddr, body: &[u8]) -> Option<Vec<u8>> {
    let request = match EchoRequest::decode(body) {
        Ok(req) => req,
        Err(e) => {
            debug!(from = %from, error = %e, "Malformed EchoRequest");
            return None;
        }
    };

    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64;

    let response = EchoResponse {
        send_timestamp_us: request.timestamp_us,
        recv_timestamp_us: now_us,
        sequence: request.sequence,
        payload: request.payload.clone(),
    };

    debug!(
        from = %from,
        sequence = request.sequence,
        payload_len = request.payload.len(),
        "EchoRequest received, sending response"
    );

    let encoded = response.encode();
    let mut out = Vec::with_capacity(1 + encoded.len());
    out.push(LinkMessageType::EchoResponse.to_byte());
    out.extend_from_slice(&encoded);
    Some(out)
}

pub fn handle_echo_response(from: &NodeAddr, body: &[u8]) -> Option<BenchmarkEvent> {
    let response = match EchoResponse::decode(body) {
        Ok(resp) => resp,
        Err(e) => {
            debug!(from = %from, error = %e, "Malformed EchoResponse");
            return None;
        }
    };

    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64;

    let rtt_us = now_us.saturating_sub(response.send_timestamp_us);

    debug!(
        from = %from,
        rtt_us = rtt_us,
        sequence = response.sequence,
        payload_len = response.payload.len(),
        "EchoResponse received"
    );

    Some(BenchmarkEvent::EchoResponseReceived {
        from: *from,
        rtt_us,
        send_timestamp_us: response.send_timestamp_us,
        recv_timestamp_us: response.recv_timestamp_us,
        sequence: response.sequence,
        payload_len: response.payload.len(),
    })
}
