"""Native process lifecycle management for BLE testing.

Manages FIPS nodes as local processes instead of Docker containers.
Each node runs as `fips -c <config>`, with stdout/stderr redirected to log files.
"""

from __future__ import annotations

import logging
import os
import random
import subprocess
import time
from collections import deque
from dataclasses import dataclass, field
from typing import IO

from .scenario import NodeChurnConfig
from .topology import SimTopology

log = logging.getLogger(__name__)


@dataclass
class NativeNodeState:
    node_id: str
    process: subprocess.Popen | None = None
    log_file: IO | None = None
    is_down: bool = False
    down_since: float | None = None
    restore_at: float | None = None


class NativeNodeManager:
    """Manages node stop/start lifecycle via local subprocesses."""

    def __init__(
        self,
        topology: SimTopology,
        config: NodeChurnConfig,
        rng: random.Random,
        fips_binary: str = "fips",
        config_dir: str = ".",
        output_dir: str = ".",
        down_nodes: set[str] | None = None,
        on_node_restart=None,
    ):
        self.topology = topology
        self.config = config
        self.rng = rng
        self.fips_binary = fips_binary
        self.config_dir = config_dir
        self.output_dir = output_dir
        self.down_nodes = down_nodes or set()
        self.on_node_restart = on_node_restart
        self.node_states: dict[str, NativeNodeState] = {
            nid: NativeNodeState(node_id=nid) for nid in topology.nodes
        }

    @property
    def down_count(self) -> int:
        return sum(1 for ns in self.node_states.values() if ns.is_down)

    def start_all(self, config_dir: str | None = None):
        """Start all nodes as subprocess.Popen with config files."""
        cfg_dir = config_dir or self.config_dir
        for node_id in sorted(self.topology.nodes):
            self._start_node(node_id, cfg_dir)

    def _start_node(self, node_id: str, config_dir: str):
        """Start a single FIPS process."""
        state = self.node_states[node_id]
        config_path = os.path.join(config_dir, f"{node_id}.yaml")
        log_path = os.path.join(self.output_dir, f"fips-{node_id}.log")

        env = os.environ.copy()
        keylog_path = os.path.join(self.output_dir, f"keys-{node_id}.log")
        env["FIPS_NOISE_KEYLOG"] = keylog_path

        log_fh = open(log_path, "w")
        process = subprocess.Popen(
            [self.fips_binary, "-c", config_path],
            stdout=log_fh,
            stderr=subprocess.STDOUT,
            env=env,
        )

        state.process = process
        state.log_file = log_fh
        state.is_down = False
        state.down_since = None
        state.restore_at = None

        log.info(
            "Node %s started (pid=%d, log=%s)", node_id, process.pid, log_path
        )

    def _stop_node(self, node_id: str, duration: float):
        """Stop a node process with SIGTERM."""
        state = self.node_states[node_id]
        if state.process is None:
            return

        state.process.terminate()
        try:
            state.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            state.process.kill()
            state.process.wait(timeout=3)

        now = time.time()
        state.is_down = True
        state.down_since = now
        state.restore_at = now + duration

        self.down_nodes.add(node_id)

        log.info("Node STOPPED: %s (pid=%d, restore in %.0fs)", node_id, state.process.pid, duration)

    def _restart_node(self, node_id: str):
        """Restart a stopped node."""
        state = self.node_states[node_id]
        down_for = time.time() - state.down_since if state.down_since else 0

        # Close old log file handle
        if state.log_file:
            state.log_file.close()

        self._start_node(node_id, self.config_dir)

        self.down_nodes.discard(node_id)

        log.info("Node STARTED: %s (was down %.0fs)", node_id, down_for)

        if self.on_node_restart:
            self.on_node_restart(node_id)

    def stop_all(self):
        """Stop all running nodes."""
        for node_id, state in self.node_states.items():
            if state.process and state.process.poll() is None:
                state.process.terminate()
                try:
                    state.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    state.process.kill()
                    state.process.wait(timeout=3)
            if state.log_file:
                state.log_file.close()
                state.log_file = None
            state.process = None

    def maybe_kill(self):
        """Attempt to stop a random node."""
        if self.down_count >= self.config.max_down_nodes:
            log.debug(
                "At max_down_nodes (%d), skipping churn",
                self.config.max_down_nodes,
            )
            return

        up_nodes = [nid for nid, ns in self.node_states.items() if not ns.is_down]
        if not up_nodes:
            return

        self.rng.shuffle(up_nodes)

        for node_id in up_nodes:
            if self.config.protect_connectivity and self._would_disconnect(node_id):
                log.debug("Skipping %s (would disconnect graph)", node_id)
                continue

            down_duration = self.rng.uniform(
                self.config.down_duration_secs.min,
                self.config.down_duration_secs.max,
            )
            self._stop_node(node_id, down_duration)
            return

        log.debug("No safe node to kill (all would disconnect)")

    def restore_expired(self):
        """Restart nodes whose down duration has expired."""
        now = time.time()
        for nid, state in self.node_states.items():
            if state.is_down and state.restore_at and now >= state.restore_at:
                self._restart_node(nid)

    def restore_all(self):
        """Restart all stopped nodes (for teardown)."""
        for nid, state in list(self.node_states.items()):
            if state.is_down:
                self._restart_node(nid)

    def _would_disconnect(self, node_id: str) -> bool:
        """Check if removing this node (plus currently-down nodes) disconnects the graph."""
        active_nodes = set()
        for nid, state in self.node_states.items():
            if not state.is_down and nid != node_id:
                active_nodes.add(nid)

        if len(active_nodes) <= 1:
            return True

        adj: dict[str, list[str]] = {nid: [] for nid in active_nodes}
        for a, b in self.topology.edges:
            if a in active_nodes and b in active_nodes:
                adj[a].append(b)
                adj[b].append(a)

        start = next(iter(active_nodes))
        visited = set()
        queue = deque([start])
        while queue:
            node = queue.popleft()
            if node in visited:
                continue
            visited.add(node)
            for neighbor in adj[node]:
                if neighbor not in visited:
                    queue.append(neighbor)

        return len(visited) < len(active_nodes)
