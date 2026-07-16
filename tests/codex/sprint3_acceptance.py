#!/usr/bin/env python3
"""Validate and merge local Sprint 3 cross-platform acceptance evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
from pathlib import Path
from typing import Any

PHASES = (
    "base",
    "rename_uid",
    "rename_uidless",
    "delete",
    "re_add",
    "reimport",
    "content_edit",
    "journal_gap",
)
TOOLS = {
    "godot_get_current_scene",
    "godot_get_editor_state",
    "godot_get_selected_nodes",
    "godot_get_resource_dependencies",
    "godot_find_resource_owners",
}
LIVE_PLATFORMS = {"macos-arm64", "windows-x86_64"}
STORAGE_PLATFORMS = {"linux", "macos", "windows"}
ABSOLUTE_PATH = re.compile(r"(?:[A-Za-z]:\\|/(?:Users|home|private|tmp|var/folders)/)")


class AcceptanceError(RuntimeError):
    """Raised when evidence cannot qualify Sprint 3 acceptance."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AcceptanceError(message)


def strict_json(path: Path) -> dict[str, Any]:
    def object_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            require(key not in result, f"duplicate JSON member {key} in {path.name}")
            result[key] = value
        return result

    try:
        value = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=object_pairs)
    except (OSError, json.JSONDecodeError) as error:
        raise AcceptanceError(f"cannot read {path.name}: {error}") from error
    require(isinstance(value, dict), f"{path.name} is not a JSON object")
    return value


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def sha256_file(path: Path) -> str:
    return f"sha256:{hashlib.sha256(path.read_bytes()).hexdigest()}"


def percentile(samples: list[int | float], value: int) -> int | float:
    ordered = sorted(samples)
    require(bool(ordered), "cannot calculate a percentile without samples")
    index = max(0, min(len(ordered) - 1, math.ceil(value * len(ordered) / 100) - 1))
    return round(ordered[index], 3)


def validate_metric(metric: Any, name: str, required: bool = True) -> list[int | float]:
    require(isinstance(metric, dict), f"{name} metric is missing")
    require(set(metric) == {"samples", "p50", "p95"}, f"{name} metric fields differ")
    samples = metric["samples"]
    require(
        isinstance(samples, list)
        and all(isinstance(sample, (int, float)) and not isinstance(sample, bool) and sample >= 0 for sample in samples),
        f"{name} raw samples are invalid",
    )
    if required:
        require(bool(samples), f"{name} raw samples are empty")
    if samples:
        require(metric["p50"] == percentile(samples, 50), f"{name} p50 does not match raw samples")
        require(metric["p95"] == percentile(samples, 95), f"{name} p95 does not match raw samples")
    else:
        require(metric["p50"] is None and metric["p95"] is None, f"{name} empty summary differs")
    return samples


def validate_phase_telemetry(record: Any, phase: str) -> list[int]:
    required = {
        "schema_version",
        "budget_usec",
        "sample_capacity",
        "busy_frame_count",
        "samples_usec",
        "max_elapsed_usec",
        "over_budget_count",
        "overflow",
    }
    require(isinstance(record, dict) and set(record) == required, f"{phase} telemetry fields differ")
    samples = record["samples_usec"]
    require(
        record["schema_version"] == 1
        and record["budget_usec"] == 2000
        and isinstance(samples, list)
        and bool(samples)
        and all(isinstance(sample, int) and not isinstance(sample, bool) and sample >= 0 for sample in samples),
        f"{phase} telemetry values are invalid",
    )
    require(record["overflow"] is False, f"{phase} telemetry overflowed")
    require(record["busy_frame_count"] == len(samples), f"{phase} telemetry omitted busy frames")
    require(record["max_elapsed_usec"] == max(samples), f"{phase} telemetry maximum differs")
    require(
        record["over_budget_count"] == sum(sample > 2000 for sample in samples),
        f"{phase} telemetry over-budget count differs",
    )
    return samples


def validate_live(path: Path, expected_platform: str | None = None) -> dict[str, Any]:
    raw_text = path.read_text(encoding="utf-8")
    require(ABSOLUTE_PATH.search(raw_text) is None, f"{path.name} contains an absolute local path")
    evidence = strict_json(path)
    required_top = {
        "schema_version",
        "stage",
        "status",
        "execution",
        "profile",
        "platform",
        "host",
        "git_commit",
        "git_dirty",
        "source_tree_sha256",
        "artifacts",
        "mcp_protocol",
        "tools",
        "oracle_sha256",
        "fixture_digest_before",
        "fixture_digest_after",
        "canonical_fixture_unchanged",
        "phases",
        "metrics_ms",
        "bridge_main_thread",
        "slo",
        "platform_evidence",
    }
    require(set(evidence) == required_top, f"{path.name} top-level contract differs")
    platform_tag = evidence["platform"]
    require(platform_tag in LIVE_PLATFORMS, f"unsupported live platform {platform_tag}")
    if expected_platform is not None:
        require(platform_tag == expected_platform, f"expected {expected_platform}, got {platform_tag}")
    require(
        evidence["schema_version"] == 2
        and evidence["stage"] == "Sprint 3 Stage 5 / S3-09-S3-10"
        and evidence["status"] == "passed"
        and evidence["execution"] == "local_model_free"
        and evidence["profile"] == "acceptance",
        f"{path.name} is not qualifying live evidence",
    )
    require(evidence["git_dirty"] is False, f"{path.name} was produced from dirty relevant source")
    for key in ("source_tree_sha256", "oracle_sha256"):
        require(isinstance(evidence[key], str) and evidence[key].startswith("sha256:"), f"{key} is invalid")
    require(
        evidence["fixture_digest_before"] == evidence["fixture_digest_after"]
        and evidence["canonical_fixture_unchanged"] is True,
        f"{path.name} changed the canonical fixture",
    )
    require(set(evidence["tools"]) == TOOLS and len(evidence["tools"]) == len(TOOLS), "MCP tool set differs")
    require(evidence["platform_evidence"] == {"remote_ci": "not_run"}, "remote CI state differs")
    artifacts = evidence["artifacts"]
    require(
        isinstance(artifacts, dict)
        and set(artifacts) == {"godot_version", "godot_sha256", "sidecar_version", "sidecar_sha256"}
        and artifacts["godot_sha256"].startswith("sha256:")
        and artifacts["sidecar_sha256"].startswith("sha256:"),
        "artifact identity is incomplete",
    )

    phases = evidence["phases"]
    require(isinstance(phases, list) and [phase.get("phase") for phase in phases] == list(PHASES), "live phases differ")
    telemetry_samples: list[int] = []
    graph_digests: dict[str, str] = {}
    for phase in phases:
        name = phase["phase"]
        require(phase.get("passed") is True and phase.get("direct_reverse_oracle_match") is True, f"{name} did not pass")
        digest = phase.get("normalized_graph_sha256")
        require(isinstance(digest, str) and digest.startswith("sha256:"), f"{name} graph digest is invalid")
        graph_digests[name] = digest
        telemetry_samples.extend(validate_phase_telemetry(phase.get("bridge_main_thread"), name))
    base = phases[0]
    require(
        base.get("same_generation_after_reopen") is True
        and base.get("immutable_segment_set_reused") is True,
        "compatible reopen was not proven",
    )
    require(phases[-1].get("full_rebuild_after_gap") is True, "journal gap did not prove full rebuild")

    metrics = evidence["metrics_ms"]
    expected_metrics = {
        "cached_resource_query",
        "ordinary_incremental_visibility",
        "bulk_status_ping",
        "startup_visibility",
        "compatible_reopen",
        "journal_gap_full_rebuild",
    }
    require(isinstance(metrics, dict) and set(metrics) == expected_metrics, "live metric populations differ")
    query = validate_metric(metrics["cached_resource_query"], "cached resource query")
    visibility = validate_metric(metrics["ordinary_incremental_visibility"], "ordinary visibility")
    status = validate_metric(metrics["bulk_status_ping"], "bulk status/ping")
    validate_metric(metrics["startup_visibility"], "startup visibility")
    validate_metric(metrics["compatible_reopen"], "compatible reopen")
    validate_metric(metrics["journal_gap_full_rebuild"], "journal gap rebuild")

    main_thread = evidence["bridge_main_thread"]
    require(isinstance(main_thread, dict), "aggregate main-thread telemetry is missing")
    aggregate_samples = validate_metric(
        {key: main_thread[key] for key in ("samples", "p50", "p95")},
        "Bridge main thread",
    )
    require(aggregate_samples == telemetry_samples, "aggregate main-thread samples differ from phase telemetry")
    require(
        main_thread.get("unit") == "microseconds"
        and main_thread.get("budget_usec") == 2000
        and main_thread.get("busy_frame_count") == len(telemetry_samples)
        and main_thread.get("max_elapsed_usec") == max(telemetry_samples)
        and main_thread.get("over_budget_count") == sum(sample > 2000 for sample in telemetry_samples)
        and main_thread.get("overflow") is False,
        "aggregate main-thread summary differs",
    )
    expected_slo = {
        "cached_resource_query_p95_lte_300ms": percentile(query, 95) <= 300,
        "ordinary_incremental_visibility_p95_lte_2000ms": percentile(visibility, 95) <= 2000,
        "bulk_status_ping_p95_lte_200ms": percentile(status, 95) <= 200,
        "bridge_main_thread_over_2000us_zero": all(sample <= 2000 for sample in telemetry_samples),
    }
    expected_slo["all_passed"] = all(expected_slo.values())
    require(evidence["slo"] == expected_slo and expected_slo["all_passed"], "live SLO result differs or failed")
    return {
        "evidence": evidence,
        "graph_digests": graph_digests,
        "sha256": sha256_file(path),
    }


def validate_storage(path: Path) -> dict[str, Any]:
    raw_text = path.read_text(encoding="utf-8")
    require(ABSOLUTE_PATH.search(raw_text) is None, f"{path.name} contains an absolute local path")
    evidence = strict_json(path)
    require(
        evidence.get("schema_version") == 2
        and evidence.get("decision") == "D-05"
        and evidence.get("cross_platform_complete") is True
        and evidence.get("chosen_backend") == "segment",
        "cross-platform D-05 decision is incomplete or differs",
    )
    runs = evidence.get("platform_runs")
    require(isinstance(runs, list) and {run.get("os") for run in runs} == STORAGE_PLATFORMS, "storage platforms differ")
    commits = {run.get("git_commit") for run in runs}
    source_digests = {run.get("source_tree_sha256") for run in runs}
    oracle_digests = {run.get("oracle_sha256") for run in runs}
    require(len(commits) == len(source_digests) == len(oracle_digests) == 1, "storage source coordinates differ")
    for run in runs:
        require(run.get("profile") == "decision" and run.get("git_dirty") is False, "storage run is quick or dirty")
        segment = next((backend for backend in run.get("backends", []) if backend.get("backend") == "segment"), None)
        require(isinstance(segment, dict) and segment.get("qualified") is True, "segment store did not qualify")
        require(all(segment.get("gates", {}).values()), "segment store gate failed")
        require(all(segment.get("fault_matrix", {}).values()), "segment store fault/recovery case failed")
    return {
        "evidence": evidence,
        "git_commit": next(iter(commits)),
        "source_tree_sha256": next(iter(source_digests)),
        "oracle_sha256": next(iter(oracle_digests)),
        "sha256": sha256_file(path),
    }


def merge_acceptance(macos_live: Path, windows_live: Path, storage_path: Path, output: Path) -> dict[str, Any]:
    macos = validate_live(macos_live, "macos-arm64")
    windows = validate_live(windows_live, "windows-x86_64")
    storage = validate_storage(storage_path)
    mac = macos["evidence"]
    win = windows["evidence"]
    require(mac["git_commit"] == win["git_commit"] == storage["git_commit"], "live/storage commits differ")
    require(mac["source_tree_sha256"] == win["source_tree_sha256"], "live source digests differ")
    require(mac["oracle_sha256"] == win["oracle_sha256"], "live oracle digests differ")
    require(macos["graph_digests"] == windows["graph_digests"], "Windows/macOS normalized graphs differ")
    criteria = {
        "S3-AC-01": ["live.normalized_graph", "golden.direct_reverse"],
        "S3-AC-02": ["live.rename_uid", "index.incremental_commit"],
        "S3-AC-03": ["live.stale_uid", "contracts.diagnostic"],
        "S3-AC-04": ["live.compatible_reopen", "storage.reopen"],
        "S3-AC-05": ["storage.cancellation_matrix"],
        "S3-AC-06": ["storage.corruption_migration_recovery"],
        "S3-AC-07": ["golden.format_import_matrix", "live.all_phases"],
        "S3-AC-08": ["live.delete_readd_reimport_gap"],
        "S3-AC-09": ["mcp.five_tool_contract", "mcp.pagination_tests"],
        "S3-AC-10": ["live.query_visibility_slo"],
        "S3-AC-11": ["live.bulk_status_main_thread_slo"],
        "S3-AC-12": ["live.cross_platform_graph_digest"],
    }
    result = {
        "schema_version": 1,
        "sprint": 3,
        "status": "passed",
        "source_commit": mac["git_commit"],
        "live_source_tree_sha256": mac["source_tree_sha256"],
        "storage_source_tree_sha256": storage["source_tree_sha256"],
        "oracle_sha256": mac["oracle_sha256"],
        "live": {
            "macos-arm64": {"file": macos_live.name, "sha256": macos["sha256"], "slo": mac["slo"]},
            "windows-x86_64": {"file": windows_live.name, "sha256": windows["sha256"], "slo": win["slo"]},
            "normalized_graphs_equal": True,
        },
        "storage": {
            "file": storage_path.name,
            "sha256": storage["sha256"],
            "platforms": sorted(STORAGE_PLATFORMS),
            "chosen_backend": "segment",
        },
        "acceptance_criteria": {key: {"passed": True, "proof": proof} for key, proof in criteria.items()},
        "remote_ci": "not_run",
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    temporary.write_text(canonical_json(result), encoding="utf-8")
    os.replace(temporary, output)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    live = subparsers.add_parser("validate-live")
    live.add_argument("evidence", type=Path)
    storage = subparsers.add_parser("validate-storage")
    storage.add_argument("evidence", type=Path)
    merge = subparsers.add_parser("merge")
    merge.add_argument("--macos-live", required=True, type=Path)
    merge.add_argument("--windows-live", required=True, type=Path)
    merge.add_argument("--storage", required=True, type=Path)
    merge.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()
    if arguments.command == "validate-live":
        result = validate_live(arguments.evidence)
    elif arguments.command == "validate-storage":
        result = validate_storage(arguments.evidence)
    else:
        result = merge_acceptance(arguments.macos_live, arguments.windows_live, arguments.storage, arguments.output)
    print(canonical_json(result))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except AcceptanceError as error:
        raise SystemExit(f"Sprint 3 acceptance failed: {error}") from error
