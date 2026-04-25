"""Control socket queries for native FIPS processes.

Unlike control.py which uses docker exec + Python one-liner,
this directly connects to the Unix domain socket on the host.
"""

from __future__ import annotations

import json
import logging
import socket

from .topology import SimTopology

log = logging.getLogger(__name__)

CONTROL_SOCKET = "/run/fips/control.sock"


def query_node(socket_path: str, command: str, timeout: float = 5.0) -> dict | None:
    """Query a FIPS node's control socket directly."""
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect(socket_path)
        s.sendall(json.dumps({"command": command}).encode() + b"\n")
        s.shutdown(socket.SHUT_WR)
        chunks = []
        for chunk in iter(lambda: s.recv(65536), b""):
            chunks.append(chunk)
        s.close()
        response = json.loads(b"".join(chunks).decode())
        if response.get("status") == "ok":
            return response.get("data", {})
        log.warning(
            "Control query %s on %s failed: %s",
            command,
            socket_path,
            response.get("message", "unknown error"),
        )
    except Exception as e:
        log.warning("Control query %s on %s failed: %s", command, socket_path, e)
    return None


def send_command(
    socket_path: str, command: str, params: dict, timeout: float = 5.0
) -> dict | None:
    """Send a mutating command with params to a node's control socket."""
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect(socket_path)
        payload = json.dumps({"command": command, "params": params})
        s.sendall(payload.encode() + b"\n")
        s.shutdown(socket.SHUT_WR)
        chunks = []
        for chunk in iter(lambda: s.recv(65536), b""):
            chunks.append(chunk)
        s.close()
        response = json.loads(b"".join(chunks).decode())
        if response.get("status") == "ok":
            return response.get("data", {})
        log.debug(
            "Command %s on %s: %s",
            command,
            socket_path,
            response.get("message", "unknown error"),
        )
    except Exception as e:
        log.warning("Command %s on %s failed: %s", command, socket_path, e)
    return None


def query_status(socket_path: str) -> dict | None:
    return query_node(socket_path, "show_status")


def query_tree(socket_path: str) -> dict | None:
    return query_node(socket_path, "show_tree")


def query_mmp(socket_path: str) -> dict | None:
    return query_node(socket_path, "show_mmp")


def query_peers(socket_path: str) -> dict | None:
    return query_node(socket_path, "show_peers")


def query_routing(socket_path: str) -> dict | None:
    return query_node(socket_path, "show_routing")


def query_transports(socket_path: str) -> dict | None:
    return query_node(socket_path, "show_transports")


def snapshot_all_trees(
    topology: SimTopology, socket_paths: dict[str, str]
) -> dict[str, dict]:
    """Query show_tree on all nodes, return {node_id: tree_data}."""
    result = {}
    for node_id in sorted(topology.nodes):
        socket_path = socket_paths.get(node_id, CONTROL_SOCKET)
        data = query_tree(socket_path)
        if data is not None:
            result[node_id] = data
        else:
            log.warning("No tree data from %s", node_id)
    return result


def snapshot_all_mmp(
    topology: SimTopology, socket_paths: dict[str, str]
) -> dict[str, dict]:
    """Query show_mmp on all nodes, return {node_id: mmp_data}."""
    result = {}
    for node_id in sorted(topology.nodes):
        socket_path = socket_paths.get(node_id, CONTROL_SOCKET)
        data = query_mmp(socket_path)
        if data is not None:
            result[node_id] = data
        else:
            log.warning("No MMP data from %s", node_id)
    return result


def snapshot_all_congestion(
    topology: SimTopology, socket_paths: dict[str, str]
) -> dict[str, dict]:
    """Query show_routing on all nodes to capture congestion counters."""
    result = {}
    for node_id in sorted(topology.nodes):
        socket_path = socket_paths.get(node_id, CONTROL_SOCKET)
        routing = query_routing(socket_path)
        transports = query_transports(socket_path)
        if routing is not None:
            entry = {"congestion": routing.get("congestion", {})}
            if transports is not None:
                drops = []
                for t in transports.get("transports", []):
                    stats = t.get("stats", {})
                    drops.append({
                        "transport_id": t.get("transport_id"),
                        "name": t.get("name"),
                        "kernel_drops": stats.get("kernel_drops"),
                    })
                entry["kernel_drops"] = drops
            result[node_id] = entry
        else:
            log.warning("No routing data from %s", node_id)
    return result
