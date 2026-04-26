"""Native process simulation orchestration.

Orchestrates FIPS nodes as local processes for BLE testing on real hardware.
No Docker, no veth pairs, no netem — BLE uses physical radio links.
"""

from __future__ import annotations

import json
import logging
import os
import random
import signal
import sys
import time
from datetime import datetime

from .ble_capture import BleCaptureManager
from .config_gen import write_configs
from .logs import AnalysisResult, analyze_logs, write_sim_metadata
from .native_control import (
    CONTROL_SOCKET,
    snapshot_all_congestion,
    snapshot_all_mmp,
    snapshot_all_trees,
)
from .native_nodes import NativeNodeManager
from .scenario import Scenario
from .topology import SimTopology, generate_topology

log = logging.getLogger(__name__)


class NativeSimRunner:
    def __init__(self, scenario: Scenario, fips_binary: str = "fips"):
        self.scenario = scenario
        self.rng = random.Random(scenario.seed)
        self.topology: SimTopology | None = None
        self.output_dir: str = self._resolve_output_dir(scenario)
        self.fips_binary = fips_binary
        self._interrupted = False

        self._down_nodes: set[str] = set()

        self.node_mgr: NativeNodeManager | None = None
        self.ble_capture: BleCaptureManager | None = None
        self._socket_paths: dict[str, str] = {}
        self._startup_mgr: NativeNodeManager | None = None

    @staticmethod
    def _resolve_output_dir(scenario: Scenario) -> str:
        base = os.environ.get("FIPS_SIM_OUTPUT", scenario.logging.output_dir)
        timestamp = datetime.now().strftime("%Y%m%d-%H%M%S")
        return os.path.join(base, f"{timestamp}-{scenario.name}")

    def run(self) -> AnalysisResult | None:
        """Run the full native simulation lifecycle."""
        signal.signal(signal.SIGINT, self._handle_sigint)
        signal.signal(signal.SIGTERM, self._handle_sigint)

        result = None
        try:
            self._setup()
            self._warmup()
            self._simulation_loop()
        except Exception:
            log.exception("Native simulation failed")
        finally:
            result = self._teardown()

        return result

    def _handle_sigint(self, signum, frame):
        if self._interrupted:
            log.warning("Force exit")
            sys.exit(1)
        log.info("Interrupt received, shutting down gracefully...")
        self._interrupted = True

    def _setup(self):
        """Generate topology, configs, start FIPS processes."""
        s = self.scenario
        mesh_name = f"sim-{s.name}-{s.seed}"

        os.makedirs(self.output_dir, exist_ok=True)
        runner_log_path = os.path.join(self.output_dir, "runner.log")
        fh = logging.FileHandler(runner_log_path, mode="w")
        fh.setLevel(logging.DEBUG)
        fh.setFormatter(logging.Formatter(
            "%(asctime)s %(levelname)-5s %(name)s: %(message)s",
            datefmt="%H:%M:%S",
        ))
        logging.getLogger().addHandler(fh)
        log.info("Runner log: %s", runner_log_path)

        # 1. Generate topology
        log.info(
            "Generating %d-node %s topology (seed=%d)...",
            s.topology.num_nodes,
            s.topology.algorithm,
            s.seed,
        )
        self.topology = generate_topology(s.topology, self.rng, mesh_name)
        log.info(
            "Topology: %d nodes, %d edges",
            len(self.topology.nodes),
            len(self.topology.edges),
        )

        for nid in sorted(self.topology.nodes):
            peers = sorted(self.topology.nodes[nid].peers)
            log.info("  %s: peers=%s", nid, ",".join(peers))

        # 2. Generate configs
        config_dir = os.path.join(self.output_dir, "configs")
        write_configs(
            self.topology,
            config_dir,
            s.fips_overrides,
            ble_interface=s.topology.ble_interface,
        )
        log.info("Wrote node configs to %s", config_dir)

        # 3. Build socket paths per node (each gets a unique control socket)
        for nid in sorted(self.topology.nodes):
            self._socket_paths[nid] = os.path.join(
                self.output_dir, f"fips-{nid}", "control.sock"
            )

        # 4. Start BLE capture (if BLE edges present)
        if self.topology.has_ble():
            self.ble_capture = BleCaptureManager(
                interface=s.topology.ble_interface,
                output_dir=self.output_dir,
            )
            pcap_path = self.ble_capture.start()
            log.info("BLE capture started: %s", pcap_path)

        # 5. Initialize node manager
        if s.node_churn.enabled:
            self.node_mgr = NativeNodeManager(
                self.topology,
                s.node_churn,
                self.rng,
                fips_binary=self.fips_binary,
                config_dir=config_dir,
                output_dir=self.output_dir,
                down_nodes=self._down_nodes,
            )

        # 6. Start FIPS processes
        log.info("Starting %d FIPS processes...", len(self.topology.nodes))

        # For nodes not managed by churn, we still need to start them.
        # Create a temporary manager for startup if node_mgr doesn't handle it.
        startup_mgr = NativeNodeManager(
            self.topology,
            s.node_churn,
            self.rng,
            fips_binary=self.fips_binary,
            config_dir=config_dir,
            output_dir=self.output_dir,
            down_nodes=self._down_nodes,
        )
        startup_mgr.start_all(config_dir)

        # Transfer running processes to the churn manager if it exists
        if self.node_mgr:
            for nid in self.topology.nodes:
                self.node_mgr.node_states[nid] = startup_mgr.node_states[nid]
        else:
            self._startup_mgr = startup_mgr

    def _warmup(self):
        """Wait for mesh convergence."""
        assert self.topology is not None
        n = len(self.topology.nodes)
        wait = max(10, n)
        log.info("Waiting %ds for mesh convergence...", wait)
        self._sleep(wait)
        self._take_snapshot("warmup")

    def _simulation_loop(self):
        """Main event loop — node churn only (no netem/link flaps for BLE)."""
        assert self.topology is not None
        start = time.time()
        s = self.scenario
        duration = s.duration_secs
        log.info("Simulation running for %ds...", duration)

        next_churn = (
            self._schedule_next(start, s.node_churn.interval_secs)
            if self.node_mgr
            else float("inf")
        )

        while not self._interrupted:
            now = time.time()
            elapsed = now - start
            if elapsed >= duration:
                break

            # Node churn
            if self.node_mgr:
                if now >= next_churn:
                    self.node_mgr.maybe_kill()
                    next_churn = self._schedule_next(
                        now, s.node_churn.interval_secs
                    )
                self.node_mgr.restore_expired()

            down_nodes = self.node_mgr.down_count if self.node_mgr else 0
            print(
                f"\r  [{elapsed:.0f}s/{duration}s] "
                f"nodes={len(self.topology.nodes)} "
                f"edges={len(self.topology.edges)} "
                f"nodes_down={down_nodes}   ",
                end="",
                flush=True,
            )

            self._sleep(1)

        print()

    def _teardown(self) -> AnalysisResult | None:
        """Stop processes, collect logs, analyze."""
        result = None

        if not self.topology:
            return result

        assert self.topology is not None

        # Restore stopped nodes
        if self.node_mgr:
            log.info("Restoring stopped nodes...")
            self.node_mgr.restore_all()

        # Take final snapshot
        self._take_snapshot("final")

        # Collect logs from per-node log files
        log.info("Collecting logs from %d processes...", len(self.topology.nodes))
        logs = self._collect_native_logs()

        # Analyze
        result = analyze_logs(logs)
        analysis_path = os.path.join(self.output_dir, "analysis.txt")
        with open(analysis_path, "w") as f:
            f.write(result.summary())
        print(result.summary())

        # Write metadata
        write_sim_metadata(
            self.output_dir,
            scenario_name=self.scenario.name,
            seed=self.scenario.seed,
            num_nodes=len(self.topology.nodes),
            num_edges=len(self.topology.edges),
            duration_secs=self.scenario.duration_secs,
            topology=self.topology,
        )

        # Stop BLE capture
        if self.ble_capture:
            log.info("Stopping BLE capture...")
            self.ble_capture.stop()

            self._decrypt_capture()

        # Stop all processes
        mgr = getattr(self, "_startup_mgr", None) or self.node_mgr
        if mgr:
            log.info("Stopping all processes...")
            mgr.stop_all()

        return result

    def _decrypt_capture(self):
        """Attempt post-run decryption of BLE capture using keylog files."""
        from pathlib import Path

        pcap_path = self.ble_capture.pcap_path if self.ble_capture else None
        if not pcap_path or not os.path.exists(pcap_path):
            log.info("No BLE capture to decrypt")
            return

        keylog_paths = sorted(
            str(p) for p in Path(self.output_dir).glob("keys-*.log")
        )
        if not keylog_paths:
            log.info("No keylog files found — skipping decryption")
            return

        log.info(
            "Decrypting capture with %d keylog files...", len(keylog_paths)
        )
        try:
            from .decrypt_capture import decrypt_capture
            dec_result = decrypt_capture(pcap_path, keylog_paths)
            dec_path = os.path.join(self.output_dir, "decryption-analysis.txt")
            with open(dec_path, "w") as f:
                f.write(dec_result.summary())
            log.info(
                "Decryption: %d/%d frames decrypted",
                dec_result.decrypted_frames,
                dec_result.total_frames,
            )
        except Exception:
            log.warning("Capture decryption failed", exc_info=True)

    def _collect_native_logs(self) -> dict[str, str]:
        """Read per-node log files from the output directory."""
        from .logs import strip_ansi

        logs = {}
        for node_id in sorted(self.topology.nodes):
            log_path = os.path.join(self.output_dir, f"fips-{node_id}.log")
            if os.path.exists(log_path):
                with open(log_path) as f:
                    logs[f"fips-{node_id}"] = strip_ansi(f.read())
            else:
                logs[f"fips-{node_id}"] = ""
                log.warning("No log file for %s", node_id)
        return logs

    def _take_snapshot(self, label: str):
        """Query all nodes via control socket and save snapshots."""
        if not self.topology:
            return
        log.info("Taking %s snapshot...", label)
        tree_snap = snapshot_all_trees(self.topology, self._socket_paths)
        mmp_snap = snapshot_all_mmp(self.topology, self._socket_paths)
        congestion_snap = snapshot_all_congestion(
            self.topology, self._socket_paths
        )

        tree_path = os.path.join(self.output_dir, f"tree-snapshot-{label}.json")
        mmp_path = os.path.join(self.output_dir, f"mmp-snapshot-{label}.json")
        congestion_path = os.path.join(
            self.output_dir, f"congestion-snapshot-{label}.json"
        )
        os.makedirs(self.output_dir, exist_ok=True)
        with open(tree_path, "w") as f:
            json.dump(tree_snap, f, indent=2)
        with open(mmp_path, "w") as f:
            json.dump(mmp_snap, f, indent=2)
        with open(congestion_path, "w") as f:
            json.dump(congestion_snap, f, indent=2)
        log.info(
            "Snapshot %s: %d/%d tree, %d/%d mmp, %d/%d congestion responses",
            label,
            len(tree_snap),
            len(self.topology.nodes),
            len(mmp_snap),
            len(self.topology.nodes),
            len(congestion_snap),
            len(self.topology.nodes),
        )

    def _schedule_next(self, now: float, interval) -> float:
        return now + self.rng.uniform(interval.min, interval.max)

    def _sleep(self, seconds: float):
        end = time.time() + seconds
        while time.time() < end and not self._interrupted:
            time.sleep(min(0.5, end - time.time()))
