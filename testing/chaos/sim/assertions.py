"""Post-run scenario assertions evaluated via control-socket data.

Assertions are declared in the scenario YAML under ``assertions:`` and
are evaluated near the end of the simulation, before teardown begins.
Each failing assertion is recorded with a clear pass/fail message; the
runner exits non-zero when any assertion fails.

Currently supported assertions:

- ``bloom_send_rate``: per-node trailing-window ceiling on
  ``stats.bloom.sent`` delta. Calibrated for the bloom-storm
  regression scenario but generally usable.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass

from .control import snapshot_all_bloom
from .scenario import (
    BloomSendRateAssertion,
    CongestionSignalsAssertion,
    MaxErrorsAssertion,
    MaxParentSwitchesAssertion,
    MinParentSwitchesAssertion,
)
from .topology import SimTopology

log = logging.getLogger(__name__)


@dataclass
class AssertionOutcome:
    name: str
    passed: bool
    detail: str


def _bloom_sent_total(node_data: dict) -> int | None:
    """Extract stats.bloom.sent from a show_bloom response."""
    stats = node_data.get("stats") or {}
    sent = stats.get("sent")
    if sent is None:
        return None
    try:
        return int(sent)
    except (TypeError, ValueError):
        return None


class BloomSendRateMonitor:
    """Samples per-node ``stats.bloom.sent`` to evaluate a trailing-window
    ceiling assertion at end-of-run.

    Usage:
        m = BloomSendRateMonitor(topology, cfg)
        m.sample_window_start()   # called window_secs before scenario end
        ...
        m.sample_end()            # called at scenario end
        outcome = m.evaluate()
    """

    def __init__(self, topology: SimTopology, cfg: BloomSendRateAssertion):
        self.topology = topology
        self.cfg = cfg
        self.window_start: dict[str, int] = {}
        self.window_end: dict[str, int] = {}

    def sample_window_start(self) -> None:
        snap = snapshot_all_bloom(self.topology)
        for nid, data in snap.items():
            v = _bloom_sent_total(data)
            if v is not None:
                self.window_start[nid] = v

    def sample_end(self) -> None:
        snap = snapshot_all_bloom(self.topology)
        for nid, data in snap.items():
            v = _bloom_sent_total(data)
            if v is not None:
                self.window_end[nid] = v

    def evaluate(self) -> AssertionOutcome:
        max_per_node = self.cfg.max_per_node
        window_secs = self.cfg.window_secs

        if not self.window_start or not self.window_end:
            return AssertionOutcome(
                name="bloom_send_rate",
                passed=False,
                detail=(
                    f"FAIL bloom_send_rate: failed to sample window endpoints "
                    f"(start={len(self.window_start)} nodes, "
                    f"end={len(self.window_end)} nodes)"
                ),
            )

        per_node_deltas: dict[str, int] = {}
        for nid, end_v in self.window_end.items():
            start_v = self.window_start.get(nid)
            if start_v is None:
                continue
            per_node_deltas[nid] = end_v - start_v

        offenders = {
            nid: d for nid, d in per_node_deltas.items() if d > max_per_node
        }
        max_obs = max(per_node_deltas.values()) if per_node_deltas else 0

        if offenders:
            sorted_off = sorted(offenders.items(), key=lambda kv: -kv[1])
            details = ", ".join(f"{nid}={d}" for nid, d in sorted_off)
            detail = (
                f"FAIL bloom_send_rate: {len(offenders)} node(s) exceeded "
                f"ceiling of {max_per_node} bloom_sent over trailing "
                f"{window_secs}s — offenders: {details} "
                f"(all per-node deltas: "
                f"{', '.join(f'{n}={v}' for n, v in sorted(per_node_deltas.items()))})"
            )
            return AssertionOutcome(
                name="bloom_send_rate",
                passed=False,
                detail=detail,
            )

        detail = (
            f"PASS bloom_send_rate: max per-node delta {max_obs} <= "
            f"ceiling {max_per_node} over trailing {window_secs}s "
            f"(per-node: "
            f"{', '.join(f'{n}={v}' for n, v in sorted(per_node_deltas.items()))})"
        )
        return AssertionOutcome(
            name="bloom_send_rate",
            passed=True,
            detail=detail,
        )


def evaluate_max_parent_switches(
    cfg: MaxParentSwitchesAssertion,
    parent_switch_count: int,
    scope: str,
) -> AssertionOutcome:
    """Stability ceiling on parent switches over the run.

    ``scope`` describes what was counted and appears in the message, so a
    reader can tell a per-node result from a mesh-wide one. Resolving a
    per-node scope to a real node is the caller's job, and so is failing
    loudly when it cannot: an unresolvable node would count zero switches
    and sail under any ceiling without having observed anything.
    """
    if parent_switch_count <= cfg.max_total:
        return AssertionOutcome(
            name="max_parent_switches",
            passed=True,
            detail=(
                f"PASS max_parent_switches: {parent_switch_count} switches "
                f"({scope}) <= ceiling {cfg.max_total}"
            ),
        )
    return AssertionOutcome(
        name="max_parent_switches",
        passed=False,
        detail=(
            f"FAIL max_parent_switches: {parent_switch_count} switches "
            f"({scope}) > ceiling {cfg.max_total} — the tree is reparenting "
            f"more than the hysteresis band should allow. Check whether a "
            f"cost change smaller than the hysteresis margin is still "
            f"triggering a switch."
        ),
    )


_CONGESTION_FLOORS = (
    ("min_nodes_detected", "congestion_detected"),
    ("min_nodes_ce_forwarded", "ce_forwarded"),
    ("min_nodes_ce_received", "ce_received"),
)


def evaluate_congestion_signals(
    cfg: CongestionSignalsAssertion,
    snapshot: dict | None,
) -> AssertionOutcome:
    """Floors on how many nodes observed each congestion counter.

    ``snapshot`` is the final congestion snapshot keyed by node id. None
    means the snapshot never ran, which fails: a missing snapshot and a
    mesh that observed no congestion produce the same zero counts, and
    treating them alike is how an assertion comes to pass on an absence
    of evidence.
    """
    if not snapshot:
        return AssertionOutcome(
            name="congestion_signals",
            passed=False,
            detail=(
                "FAIL congestion_signals: no final congestion snapshot was "
                "taken, so no node was observed at all. This is a harness "
                "failure, not a statement about congestion."
            ),
        )

    parts, failures = [], []
    for attr, counter in _CONGESTION_FLOORS:
        floor = getattr(cfg, attr)
        if floor is None:
            continue
        hits = sorted(
            nid for nid, data in snapshot.items()
            if (data.get("congestion") or {}).get(counter, 0) > 0
        )
        parts.append(f"{counter}: {len(hits)} node(s) >0 (floor {floor})")
        if len(hits) < floor:
            failures.append(
                f"{counter} non-zero on {len(hits)} node(s), need {floor}"
            )
        else:
            parts[-1] += f" [{', '.join(hits)}]"

    summary = "; ".join(parts)
    if failures:
        return AssertionOutcome(
            name="congestion_signals",
            passed=False,
            detail=(
                f"FAIL congestion_signals: {'; '.join(failures)} — across "
                f"{len(snapshot)} node(s) sampled. Full counts: {summary}"
            ),
        )
    return AssertionOutcome(
        name="congestion_signals",
        passed=True,
        detail=f"PASS congestion_signals: {summary}",
    )


def evaluate_max_errors(
    cfg: MaxErrorsAssertion,
    errors: list[tuple[str, str]],
) -> AssertionOutcome:
    """Ceiling on ERROR-level log lines across the whole mesh.

    ``errors`` is the ``AnalysisResult.errors`` list of ``(source, line)``
    pairs rather than a bare count, so a failure can name the nodes and
    quote the lines. A ceiling breach that only reports a number sends the
    reader back to the logs it was supposed to save them reading.
    """
    count = len(errors)
    if count <= cfg.max_total:
        return AssertionOutcome(
            name="max_errors",
            passed=True,
            detail=(
                f"PASS max_errors: {count} ERROR line(s) mesh-wide <= "
                f"ceiling {cfg.max_total}"
            ),
        )

    per_node: dict[str, int] = {}
    for source, _line in errors:
        per_node[source] = per_node.get(source, 0) + 1
    worst = sorted(per_node.items(), key=lambda kv: -kv[1])
    breakdown = ", ".join(f"{src}={n}" for src, n in worst)
    samples = "\n".join(
        f"    [{src}] {line.strip()}" for src, line in errors[:5]
    )
    return AssertionOutcome(
        name="max_errors",
        passed=False,
        detail=(
            f"FAIL max_errors: {count} ERROR line(s) mesh-wide > ceiling "
            f"{cfg.max_total} — per node: {breakdown}. First "
            f"{min(5, count)}:\n{samples}"
        ),
    )


def evaluate_min_parent_switches(
    cfg: MinParentSwitchesAssertion,
    parent_switch_count: int,
) -> AssertionOutcome:
    """Sanity guard: fail the scenario if the harness-induced flap did
    not produce at least ``cfg.min_total`` parent switches across the
    run. Detects misconfiguration (e.g., wrong root election) where
    the bloom-rate assertion would otherwise trivially pass on any
    binary including the regressed one.
    """
    if parent_switch_count >= cfg.min_total:
        return AssertionOutcome(
            name="min_parent_switches",
            passed=True,
            detail=(
                f"PASS min_parent_switches: {parent_switch_count} switches "
                f"(mesh-wide) >= floor {cfg.min_total}"
            ),
        )
    return AssertionOutcome(
        name="min_parent_switches",
        passed=False,
        detail=(
            f"FAIL min_parent_switches: {parent_switch_count} switches "
            f"(mesh-wide) < floor {cfg.min_total} — harness did not induce "
            f"sufficient "
            f"parent flapping; bloom-rate assertion would be trivially "
            f"true. Check tree-snapshot-warmup.json: did the expected "
            f"node win the root election?"
        ),
    )
