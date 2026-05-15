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
        "benchmark_echo" => benchmark_echo(node, params),
        #[cfg(feature = "benchmark")]
        "benchmark_throughput" => benchmark_throughput(node, params),
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
fn benchmark_echo(node: &mut Node, params: Option<&Value>) -> Response {
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

    Response::ok(serde_json::json!({
        "status": "echo_test_prepared",
        "peer": npub,
        "frame_count": frames.len(),
        "count": count,
        "payload_size": payload_size,
    }))
}

#[cfg(feature = "benchmark")]
fn benchmark_throughput(node: &mut Node, params: Option<&Value>) -> Response {
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

    let _peer_addr = match crate::identity::PeerIdentity::from_npub(npub) {
        Ok(id) => *id.node_addr(),
        Err(e) => return Response::error(format!("invalid peer npub: {e}")),
    };

    let direction = match direction_str {
        "upload" => crate::benchmark::throughput::Direction::Upload,
        "download" => crate::benchmark::throughput::Direction::Download,
        _ => return Response::error("direction must be 'upload' or 'download'"),
    };

    let (test_id, _frame) = node
        .benchmark_mut()
        .prepare_throughput_test(direction, duration, frame_size, rate);

    Response::ok(serde_json::json!({
        "status": "throughput_test_prepared",
        "peer": npub,
        "test_id": test_id,
        "direction": direction_str,
        "duration_secs": duration,
        "frame_size": frame_size,
        "rate_bps": rate,
    }))
}
