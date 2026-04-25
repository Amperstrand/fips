"""BLE traffic capture via btmon on Linux.

Starts btmon -i <interface> -w <output> as a subprocess
before FIPS nodes start, stops after teardown.
"""

from __future__ import annotations

import logging
import os
import subprocess

log = logging.getLogger(__name__)


class BleCaptureManager:
    def __init__(self, interface: str = "hci0", output_dir: str = "."):
        self.interface = interface
        self.output_dir = output_dir
        self._process: subprocess.Popen | None = None
        self._pcap_path: str = ""

    def start(self) -> str:
        """Start btmon capture. Returns capture file path."""
        self._pcap_path = os.path.join(
            self.output_dir, f"ble-capture-{self.interface}.log"
        )
        log.info(
            "Starting btmon capture on %s -> %s", self.interface, self._pcap_path
        )
        self._process = subprocess.Popen(
            ["sudo", "btmon", "-i", self.interface, "-w", self._pcap_path],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        return self._pcap_path

    def stop(self):
        """Stop btmon capture."""
        if self._process and self._process.poll() is None:
            log.info("Stopping btmon capture (pid=%d)", self._process.pid)
            self._process.terminate()
            try:
                self._process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._process.kill()
                self._process.wait(timeout=3)
            self._process = None

    @property
    def pcap_path(self) -> str:
        return self._pcap_path

    @property
    def is_running(self) -> bool:
        return self._process is not None and self._process.poll() is None
