//! Mutating control socket commands.
//!
//! Commands that modify node state (connect, disconnect) are handled here,
//! separate from read-only queries in `queries.rs`.

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
    let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
    let payload_size = params.get("payload_size").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    match node.api_benchmark_echo(npub, count, payload_size).await {
        Ok(data) => Response::ok(data),
        Err(msg) => Response::error(msg),
    }
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
    let direction = match params.get("direction").and_then(|v| v.as_str()) {
        Some(v) => v.to_string(),
        None => return Response::error("missing 'direction' parameter (upload/download)"),
    };
    let duration_secs = params.get("duration_secs").and_then(|v| v.as_u64()).unwrap_or(5) as u8;
    let frame_size = params.get("frame_size").and_then(|v| v.as_u64()).unwrap_or(256) as u16;
    let rate_bps = params.get("rate_bps").and_then(|v| v.as_u64()).unwrap_or(40000) as u32;

    match node.api_benchmark_throughput(npub, &direction, duration_secs, frame_size, rate_bps).await {
        Ok(data) => Response::ok(data),
        Err(msg) => Response::error(msg),
    }
}
