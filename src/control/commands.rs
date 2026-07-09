//! Mutating control socket commands.
//!
//! Commands that modify node state (connect, disconnect) are handled here,
//! separate from read-only queries in `queries.rs`.

use super::protocol::Response;
use crate::node::Node;
use crate::peer::ForwardingPolicy;
use serde_json::Value;
use tracing::debug;

pub async fn dispatch(node: &mut Node, command: &str, params: Option<&Value>) -> Response {
    match command {
        "connect" => connect(node, params).await,
        "disconnect" => disconnect(node, params).await,
        "set_peer_policy" => set_peer_policy(node, params).await,
        #[cfg(feature = "benchmark")]
        "benchmark_echo" => benchmark_echo(node, params).await,
        #[cfg(feature = "benchmark")]
        "benchmark_throughput" => benchmark_throughput(node, params).await,
        #[cfg(feature = "benchmark")]
        "show_benchmark_echo_results" => benchmark_echo_results(node, params),
        #[cfg(feature = "benchmark")]
        "show_benchmark_throughput_results" => benchmark_throughput_results(node, params),
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
async fn disconnect(node: &mut Node, params: Option<&Value>) -> Response {
    let Some(params) = params else {
        return Response::error("missing params for disconnect");
    };

    let npub = match params.get("npub").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return Response::error("missing 'npub' parameter"),
    };

    debug!(npub = %npub, "API disconnect requested");

    match node.api_disconnect(npub).await {
        Ok(data) => Response::ok(data),
        Err(msg) => Response::error(msg),
    }
}

async fn set_peer_policy(node: &mut Node, params: Option<&Value>) -> Response {
    let Some(params) = params else {
        return Response::error("missing params for set_peer_policy");
    };
    let npub = match params.get("npub").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return Response::error("missing 'npub' parameter"),
    };
    let policy_str = match params.get("policy").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return Response::error("missing 'policy' parameter (expected 'full' or 'local_only')"),
    };
    let policy = match policy_str {
        "full" => ForwardingPolicy::Full,
        "local_only" => ForwardingPolicy::LocalOnly,
        _ => return Response::error(format!("invalid policy '{policy_str}': expected 'full' or 'local_only'")),
    };
    let peer_identity = match crate::identity::PeerIdentity::from_npub(npub) {
        Ok(id) => id,
        Err(e) => return Response::error(format!("invalid npub: {e}")),
    };
    let node_addr = *peer_identity.node_addr();
    match node.get_peer_mut(&node_addr) {
        Some(peer) => {
            let old: ForwardingPolicy = peer.forwarding_policy();
            peer.set_forwarding_policy(policy);
            Response::ok(serde_json::json!({
                "npub": npub,
                "old_policy": format!("{}", old),
                "new_policy": format!("{}", policy),
            }))
        }
        None => Response::error(format!("peer {npub} not found or not authenticated")),
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
    let payload_size = params
        .get("payload_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let peer_addr = match crate::identity::PeerIdentity::from_npub(npub) {
        Ok(id) => *id.node_addr(),
        Err(e) => return Response::error(format!("invalid peer npub: {e}")),
    };

    node.benchmark_mut()
        .start_echo_test(peer_addr, count, payload_size);

    Response::ok(serde_json::json!({
        "status": "echo_test_started",
        "peer": npub,
        "queued": count,
        "expected": count,
    }))
}

#[cfg(feature = "benchmark")]
async fn benchmark_throughput(node: &mut Node, params: Option<&Value>) -> Response {
    use crate::benchmark::throughput::Direction;

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
    let duration = params.get("duration").and_then(|v| v.as_u64()).unwrap_or(5) as u8;
    let frame_size = params
        .get("frame_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(256) as u16;
    let rate = params.get("rate").and_then(|v| v.as_u64()).unwrap_or(40000) as u32;

    let peer_addr = match crate::identity::PeerIdentity::from_npub(npub) {
        Ok(id) => *id.node_addr(),
        Err(e) => return Response::error(format!("invalid peer npub: {e}")),
    };

    let direction = match direction_str {
        "upload" => Direction::Upload,
        "download" => Direction::Download,
        _ => return Response::error("direction must be 'upload' or 'download'"),
    };

    let (test_id, _config, request_frame) = node
        .benchmark_mut()
        .prepare_throughput_test(direction, duration, frame_size, rate);

    if let Err(e) = node
        .api_send_benchmark_message(&peer_addr, request_frame[0], &request_frame[1..])
        .await
    {
        return Response::error(format!("failed to send throughput request: {e}"));
    }

    let queued_frames: u32 = if direction == Direction::Upload {
        let data_len = frame_size as usize;
        let interval_us = if rate > 0 {
            ((data_len as u64) * 8 * 1_000_000) / rate as u64
        } else {
            1000
        };
        let total_frames = (duration as u64 * 1_000_000) / interval_us.max(1);
        node.benchmark_mut()
            .start_throughput_sends(peer_addr, test_id, frame_size, rate, duration);
        total_frames as u32
    } else {
        0
    };

    Response::ok(serde_json::json!({
        "status": "throughput_test_started",
        "peer": npub,
        "test_id": test_id,
        "direction": direction_str,
        "duration_secs": duration,
        "frame_size": frame_size,
        "rate_bps": rate,
        "queued_stream_frames": queued_frames,
    }))
}

#[cfg(feature = "benchmark")]
fn benchmark_echo_results(node: &Node, params: Option<&Value>) -> Response {
    use serde_json::json;

    let Some(params) = params else {
        return Response::error("missing params for show_benchmark_echo_results");
    };
    let npub = match params.get("npub").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return Response::error("missing 'npub' parameter"),
    };
    let peer_addr = match crate::identity::PeerIdentity::from_npub(npub) {
        Ok(id) => *id.node_addr(),
        Err(e) => return Response::error(format!("invalid peer npub: {e}")),
    };

    let bm = node.benchmark();
    let stats = bm.get_echo_stats(&peer_addr);
    let expected = bm.echo_expected_count(&peer_addr);

    match (stats, expected) {
        (Some(results), Some(exp)) => {
            let results_vec: Vec<Value> = results
                .iter()
                .map(|r| {
                    json!({
                        "rtt_us": r.rtt_us,
                        "seq": r.seq,
                        "payload_len": r.payload_len,
                    })
                })
                .collect();
            let computed =
                crate::benchmark::echo::compute_echo_stats(results.to_vec(), exp as usize);
            Response::ok(json!({
                "results": results_vec,
                "min_us": computed.min_us,
                "max_us": computed.max_us,
                "mean_us": computed.mean_us,
                "median_us": computed.median_us,
                "p95_us": computed.p95_us,
                "loss_count": computed.loss_count,
                "jitter_us": computed.jitter_us,
            }))
        }
        (None, None) => Response::ok(json!({
            "status": "no_test",
            "peer": npub,
        })),
        _ => Response::ok(json!({
            "status": "pending",
            "peer": npub,
        })),
    }
}

#[cfg(feature = "benchmark")]
fn benchmark_throughput_results(node: &Node, params: Option<&Value>) -> Response {
    use serde_json::json;

    let Some(params) = params else {
        return Response::error("missing params for show_benchmark_throughput_results");
    };
    let npub = match params.get("npub").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return Response::error("missing 'npub' parameter"),
    };

    match node.benchmark().last_throughput_result() {
        Some((peer, result)) => Response::ok(json!({
            "status": "complete",
            "peer": hex::encode(peer.as_bytes()),
            "achieved_bps": result.achieved_bps,
            "frame_loss_rate": result.frame_loss_rate,
            "total_bytes": result.total_bytes,
            "duration_us": result.duration_us,
            "frames_sent": result.frames_sent,
            "frames_recv": result.frames_recv,
        })),
        None => Response::ok(json!({
            "status": "pending",
            "peer": npub,
        })),
    }
}
