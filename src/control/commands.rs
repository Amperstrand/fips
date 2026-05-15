//! Mutating control socket commands.
//!
//! Commands that modify node state (connect, disconnect, benchmark) are
//! handled here, separate from read-only queries in `queries.rs`.

use super::protocol::Response;
use crate::node::Node;
use serde_json::Value;
use tracing::debug;

/// Dispatch a mutating command to the appropriate handler.
pub async fn dispatch(node: &mut Node, command: &str, params: Option<&Value>) -> Response {
    match command {
        "connect" => connect(node, params).await,
        "disconnect" => disconnect(node, params),
        #[cfg(feature = "benchmark")]
        "benchmark_echo" => benchmark_echo(node, params).await,
        #[cfg(feature = "benchmark")]
        "benchmark_throughput" => benchmark_throughput(node, params).await,
        _ => Response::error(format!("unknown command: {command}")),
    }
}

/// Connect to a peer.
///
/// Params: `{"npub": "npub1...", "address": "host:port", "transport": "udp"}`
async fn connect(node: &mut Node, params: Option<&Value>) -> Response {
    let Some(params) = params else {
        return Response::error("missing params for connect");
    };

    let npub = match params.get("npub").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return Response::error("missing 'npub' parameter"),
    };
    let address = match params.get("address").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return Response::error("missing 'address' parameter"),
    };
    let transport = match params.get("transport").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return Response::error("missing 'transport' parameter"),
    };

    debug!(npub = %npub, address = %address, transport = %transport, "API connect requested");

    match node.api_connect(npub, address, transport).await {
        Ok(data) => Response::ok(data),
        Err(msg) => Response::error(msg),
    }
}

/// Disconnect a peer.
///
/// Params: `{"npub": "npub1..."}`
fn disconnect(node: &mut Node, params: Option<&Value>) -> Response {
    let Some(params) = params else {
        return Response::error("missing params for disconnect");
    };

    let npub = match params.get("npub").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return Response::error("missing 'npub' parameter"),
    };

    debug!(npub = %npub, "API disconnect requested");

    match node.api_disconnect(npub) {
        Ok(data) => Response::ok(data),
        Err(msg) => Response::error(msg),
    }
}

#[cfg(feature = "benchmark")]
async fn benchmark_echo(node: &mut Node, params: Option<&Value>) -> Response {
    let Some(params) = params else {
        return Response::error("missing params for benchmark_echo");
    };

    let npub = match params.get("npub").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return Response::error("missing 'npub' parameter"),
    };
    let count = params
        .get("count")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as u32;
    let payload_size = params
        .get("payload_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let peer_addr = match crate::identity::PeerIdentity::from_npub(npub) {
        Ok(id) => *id.node_addr(),
        Err(e) => return Response::error(format!("invalid peer npub: {e}")),
    };

    let frames = node.benchmark_mut().prepare_echo_test(peer_addr, count, payload_size);

    let mut sent = 0u32;
    for payload in &frames {
        // payload already includes msg_type byte (0xFF) + body
        match node
            .api_send_benchmark_message(&peer_addr, payload[0], &payload[1..])
            .await
        {
            Ok(_) => sent += 1,
            Err(e) => {
                debug!(seq = sent, error = %e, "benchmark_echo: send failed");
            }
        }
    }

    Response::ok(serde_json::json!({
        "status": "echo_test_started",
        "peer": npub,
        "sent": sent,
        "expected": count,
    }))
}

#[cfg(feature = "benchmark")]
async fn benchmark_throughput(node: &mut Node, params: Option<&Value>) -> Response {
    let Some(params) = params else {
        return Response::error("missing params for benchmark_throughput");
    };

    let npub = match params.get("npub").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return Response::error("missing 'npub' parameter"),
    };
    let direction_str = params
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("upload");
    let duration = params
        .get("duration")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as u8;
    let frame_size = params
        .get("frame_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(256) as u16;
    let rate = params
        .get("rate")
        .and_then(|v| v.as_u64())
        .unwrap_or(40000) as u32;

    let peer_addr = match crate::identity::PeerIdentity::from_npub(npub) {
        Ok(id) => *id.node_addr(),
        Err(e) => return Response::error(format!("invalid peer npub: {e}")),
    };

    let direction = match direction_str {
        "upload" => crate::benchmark::throughput::Direction::Upload,
        "download" => crate::benchmark::throughput::Direction::Download,
        _ => return Response::error("direction must be 'upload' or 'download'"),
    };

    let (test_id, request_frame) = node
        .benchmark_mut()
        .prepare_throughput_test(direction, duration, frame_size, rate);

    // Send the ThroughputRequest frame
    if let Err(e) = node
        .api_send_benchmark_message(
            &peer_addr,
            request_frame[0],
            &request_frame[1..],
        )
        .await
    {
        return Response::error(format!("failed to send throughput request: {e}"));
    }

    // For upload tests, also send the stream frames immediately.
    // The BLE socket buffers them; the ESP32 counts and sends a report.
    let mut stream_frames_sent = 0u32;
    if direction == crate::benchmark::throughput::Direction::Upload {
        let data_len = frame_size as usize;
        let interval_us = if rate > 0 {
            ((data_len as u64) * 8 * 1_000_000) / rate as u64
        } else {
            1000
        };
        let total_frames = (duration as u64 * 1_000_000) / interval_us.max(1);

        for seq in 0..total_frames {
            let stream_payload = crate::benchmark::throughput::build_throughput_stream_frame(
                test_id,
                seq as u32,
                data_len,
            );
            match node
                .api_send_benchmark_message(
                    &peer_addr,
                    stream_payload[0],
                    &stream_payload[1..],
                )
                .await
            {
                Ok(_) => stream_frames_sent += 1,
                Err(e) => {
                    debug!(seq, error = %e, "benchmark_throughput: stream send failed");
                }
            }
        }
    }

    Response::ok(serde_json::json!({
        "status": "throughput_test_started",
        "peer": npub,
        "test_id": test_id,
        "direction": direction_str,
        "duration_secs": duration,
        "frame_size": frame_size,
        "rate_bps": rate,
        "stream_frames_sent": stream_frames_sent,
    }))
}
