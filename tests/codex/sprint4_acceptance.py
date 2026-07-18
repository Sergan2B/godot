#!/usr/bin/env python3
"""Validate and merge local Sprint 4 macOS/Windows scene evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import subprocess
import sys
from pathlib import Path
from typing import Any, cast

from scene_graph_fixture import PHASES

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SOURCE_SCOPE_PATH = REPOSITORY_ROOT / "tests" / "codex" / "sprint4_source_scopes.txt"
SOURCE_SCOPES = tuple(
    line for line in SOURCE_SCOPE_PATH.read_text(encoding="utf-8").splitlines() if line and not line.startswith("#")
)
PLATFORM_TARGETS = {
    "macos-arm64": "aarch64-apple-darwin",
    "windows-x86_64": "x86_64-pc-windows-msvc",
}
MCP_CONTRACT_FIELDS = {
    "closed_input_schemas",
    "cursor_tampering_rejected",
    "cross_selector_cursor_rejected",
    "cross_tool_cursor_rejected",
    "exact_tool_registry",
    "limit_bounds_rejected",
    "stale_generation_cursor_rejected",
    "strict_selector_forms",
    "traversal_rejected",
}
TOP_LEVEL_FIELDS = {
    "schema_version",
    "sprint",
    "profile",
    "platform",
    "source",
    "artifacts",
    "versions",
    "mcp_contract",
    "phases",
    "performance",
    "completion",
    "cleanup",
    "redaction",
    "remote_ci",
    "status",
}


class AcceptanceError(RuntimeError):
    """Raised when evidence cannot qualify for Sprint 4 acceptance."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AcceptanceError(message)


def strict_json_load(path: Path) -> dict[str, Any]:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, member in pairs:
            if key in value:
                raise AcceptanceError(f"duplicate JSON member: {key}")
            value[key] = member
        return value

    try:
        value = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=reject_duplicates)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise AcceptanceError(f"cannot read strict evidence: {path.name}") from error
    require(isinstance(value, dict), "evidence root must be an object")
    return cast(dict[str, Any], value)


def sha256_file(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def current_source_digest() -> str:
    listed = subprocess.run(
        ["git", "--literal-pathspecs", "ls-files", "-z", "--", *SOURCE_SCOPES],
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
    ).stdout.split(b"\0")
    paths = sorted(path.decode() for path in listed if path)
    digest = hashlib.sha256()
    for relative in paths:
        digest.update(relative.encode())
        digest.update(b"\0")
        digest.update((REPOSITORY_ROOT / relative).read_bytes())
        digest.update(b"\0")
    return "sha256:" + digest.hexdigest()


def percentile(samples: list[float] | list[int], value: int) -> float | None:
    if not samples:
        return None
    ordered = sorted(samples)
    index = max(0, min(len(ordered) - 1, math.ceil(value * len(ordered) / 100) - 1))
    return round(float(ordered[index]), 3)


def metric(samples: list[float] | list[int]) -> dict[str, Any]:
    return {"samples": samples, "p50": percentile(samples, 50), "p95": percentile(samples, 95)}


def expected_performance(phases: list[dict[str, Any]]) -> dict[str, Any]:
    cached: list[float] = [sample for phase in phases for sample in phase["cached_scene_query_samples_ms"]]
    visibility: list[float] = [
        phase["change_visibility_ms"] for phase in phases if phase["phase"] not in {"base", "journal_gap"}
    ]
    control: list[float] = [sample for phase in phases for sample in phase["bulk_status_ping_samples_ms"]]
    bridge: list[int] = [
        sample
        for phase in phases
        for session in phase["bridge_main_thread_sessions"]
        for sample in session["samples_usec"]
    ]
    require(
        bool(cached) and len(visibility) >= 6 and len(visibility) % 6 == 0 and bool(control) and bool(bridge),
        "required SLO populations are incomplete",
    )
    cached_p95 = percentile(cached, 95)
    visibility_p95 = percentile(visibility, 95)
    control_p95 = percentile(control, 95)
    if cached_p95 is None or visibility_p95 is None or control_p95 is None:
        raise AcceptanceError("required SLO percentiles are unavailable")
    return {
        "cached_scene_query_ms": metric(cached),
        "ordinary_change_visibility_ms": metric(visibility),
        "bulk_status_ping_ms": metric(control),
        "bridge_main_thread_usec": metric(bridge),
        "gates": {
            "cached_scene_query_p95_lte_300ms": cached_p95 <= 300,
            "ordinary_change_visibility_p95_lte_2000ms": visibility_p95 <= 2000,
            "bulk_status_ping_p95_lte_200ms": control_p95 <= 200,
            "bridge_main_thread_zero_over_2000us": all(sample <= 2000 for sample in bridge),
        },
    }


def validate_telemetry(record: dict[str, Any]) -> None:
    samples = record.get("samples_usec")
    require(record.get("schema_version") == 1, "Bridge telemetry schema differs")
    require(record.get("budget_usec") == 2000, "Bridge telemetry budget differs")
    require(record.get("overflow") is False, "Bridge telemetry overflowed")
    if not isinstance(samples, list) or not samples:
        raise AcceptanceError("Bridge telemetry samples are empty")
    require(record.get("busy_frame_count") == len(samples), "Bridge telemetry omitted samples")
    require(record.get("max_elapsed_usec") == max(samples), "Bridge telemetry maximum differs")
    require(
        record.get("over_budget_count") == sum(sample > 2000 for sample in samples),
        "Bridge telemetry over-budget count differs",
    )


def validate_platform_report(
    report: dict[str, Any],
    expected_platform: str | None = None,
    *,
    check_checkout: bool = True,
) -> dict[str, Any]:
    require(set(report) == TOP_LEVEL_FIELDS, "platform evidence fields differ")
    platform_tag = report.get("platform")
    if not isinstance(platform_tag, str) or platform_tag not in PLATFORM_TARGETS:
        raise AcceptanceError("unsupported Sprint 4 platform")
    if expected_platform is not None:
        require(platform_tag == expected_platform, "platform evidence coordinate differs")
    require(report.get("schema_version") == 1 and report.get("sprint") == 4, "evidence version differs")
    require(report.get("profile") == "qualifying" and report.get("status") == "passed", "report is not qualifying")
    require(report.get("remote_ci") == "not_run", "remote CI coordinate must remain not_run")

    source_value = report.get("source")
    if not isinstance(source_value, dict):
        raise AcceptanceError("source coordinates are missing")
    source: dict[str, Any] = source_value
    require(source.get("relevant_source_clean") is True, "producer source was dirty")
    require(isinstance(source.get("git_commit"), str) and len(source["git_commit"]) >= 40, "source commit is invalid")
    for field in (
        "relevant_source_sha256",
        "fixture_sha256",
        "fixture_manifest_sha256",
        "golden_scene_graph_sha256",
    ):
        require(
            isinstance(source.get(field), str) and source[field].startswith("sha256:") and len(source[field]) == 71,
            f"source digest is invalid: {field}",
        )
    if check_checkout:
        require(source["relevant_source_sha256"] == current_source_digest(), "current Sprint 4 source differs")
        commit = subprocess.run(
            ["git", "cat-file", "-e", source["git_commit"] + "^{commit}"],
            cwd=REPOSITORY_ROOT,
            check=False,
        )
        require(commit.returncode == 0, "source-freeze commit is unavailable")

    versions_value = report.get("versions")
    if not isinstance(versions_value, dict):
        raise AcceptanceError("version coordinates are missing")
    versions: dict[str, Any] = versions_value
    require(versions.get("rust_target") == PLATFORM_TARGETS[platform_tag], "Rust target differs from platform")
    require(
        versions.get("bridge_rpc") == "1.3"
        and versions.get("logical_schema") == "1.2"
        and versions.get("physical_store") == "segment-v2",
        "Sprint 4 protocol/store versions differ",
    )
    contract_value = report.get("mcp_contract")
    if not isinstance(contract_value, dict):
        raise AcceptanceError("MCP contract fields differ")
    contract: dict[str, Any] = contract_value
    require(set(contract) == MCP_CONTRACT_FIELDS, "MCP contract fields differ")
    require(all(value is True for value in contract.values()), "MCP contract is incomplete")

    phase_values = report.get("phases")
    if not isinstance(phase_values, list) or not all(isinstance(phase, dict) for phase in phase_values):
        raise AcceptanceError("canonical phase sequence differs")
    phases = cast(list[dict[str, Any]], phase_values)
    require([phase.get("phase") for phase in phases] == list(PHASES), "canonical phase sequence differs")
    for phase in phases:
        require(phase.get("passed") is True, f"phase did not pass: {phase.get('phase')}")
        require(
            isinstance(phase.get("normalized_scene_sha256"), str) and len(phase["normalized_scene_sha256"]) == 71,
            "phase semantic digest is invalid",
        )
        assertions = phase.get("assertions")
        if not isinstance(assertions, dict) or not assertions:
            raise AcceptanceError("phase assertions are missing")
        require(all(value is True for value in assertions.values()), "phase semantic assertion failed")
        session_values = phase.get("bridge_main_thread_sessions")
        if (
            not isinstance(session_values, list)
            or not session_values
            or not all(isinstance(session, dict) for session in session_values)
        ):
            raise AcceptanceError("phase Bridge telemetry is missing")
        sessions = cast(list[dict[str, Any]], session_values)
        require(len(sessions) == (2 if phase["phase"] == "base" else 1), "phase editor-session count differs")
        for session in sessions:
            validate_telemetry(session)
    performance = expected_performance(phases)
    require(report.get("performance") == performance, "reported SLO metrics do not match raw samples")
    require(all(performance["gates"].values()), "one or more Sprint 4 SLOs failed")

    completion_value = report.get("completion")
    if not isinstance(completion_value, dict):
        raise AcceptanceError("completion evidence is incomplete")
    completion: dict[str, Any] = completion_value
    require(
        completion.get("requested_phases") == list(PHASES)
        and completion.get("completed_phases") == list(PHASES)
        and all(
            value is True for key, value in completion.items() if key not in {"requested_phases", "completed_phases"}
        ),
        "completion evidence is incomplete",
    )
    for section in ("cleanup", "redaction"):
        value = report.get(section)
        if not isinstance(value, dict) or not value or not all(member is True for member in value.values()):
            raise AcceptanceError(f"{section} failed")
    return report


def merge(
    macos_path: Path,
    windows_path: Path,
    output: Path,
    *,
    check_checkout: bool = True,
) -> dict[str, Any]:
    macos = validate_platform_report(strict_json_load(macos_path), "macos-arm64", check_checkout=check_checkout)
    windows = validate_platform_report(strict_json_load(windows_path), "windows-x86_64", check_checkout=check_checkout)
    source_fields = (
        "git_commit",
        "relevant_source_sha256",
        "fixture_sha256",
        "fixture_manifest_sha256",
        "golden_scene_graph_sha256",
    )
    require(
        all(macos["source"][field] == windows["source"][field] for field in source_fields),
        "platform reports do not share one source freeze",
    )
    phase_digests = {}
    for index, phase_name in enumerate(PHASES):
        left = macos["phases"][index]["normalized_scene_sha256"]
        right = windows["phases"][index]["normalized_scene_sha256"]
        require(left == right, f"cross-platform scene semantics differ: {phase_name}")
        phase_digests[phase_name] = left
    combined_phases = macos["phases"] + windows["phases"]
    performance = expected_performance(combined_phases)
    require(all(performance["gates"].values()), "aggregate Sprint 4 SLO failed")

    base = macos["phases"][0]["assertions"]
    by_name = {phase["phase"]: phase["assertions"] for phase in macos["phases"]}
    acceptance = {
        "S4-AC-01": base["save_reopen_identity"]
        and by_name["node_rename"]["rename_identity_preserved"]
        and by_name["node_rename"]["duplicate_identity_distinct"]
        and by_name["node_reparent"]["reparent_identity_preserved"],
        "S4-AC-02": base["persistent_subresource_identity"] and base["subresource_order_equivalence"],
        "S4-AC-03": by_name["property_override"]["property_override"],
        "S4-AC-04": base["inheritance_and_instances"] and by_name["instance_mutation"]["instance_closure_advanced"],
        "S4-AC-05": base["signal_and_group"] and by_name["signal_group"]["signal_group_atomic"],
        "S4-AC-06": base["animation_resolution"] and by_name["animation_fix"]["animation_paths_resolved"],
        "S4-AC-07": base["attached_script"] and base["external_and_nested_subresources"] and base["project_context"],
        "S4-AC-08": base["subresource_order_equivalence"],
        "S4-AC-09": by_name["journal_gap"]["gap_scene_recovered"]
        and all(phase["passed"] for phase in macos["phases"] + windows["phases"]),
        "S4-AC-10": macos["completion"]["rust_workspace_tests_passed"]
        and windows["completion"]["rust_workspace_tests_passed"],
        "S4-AC-11": all(macos["mcp_contract"].values()) and all(windows["mcp_contract"].values()),
        "S4-AC-12": True,
    }
    require(all(acceptance.values()), "one or more Sprint 4 acceptance criteria failed")
    result = {
        "schema_version": 1,
        "sprint": 4,
        "source": {field: macos["source"][field] for field in source_fields},
        "platform_evidence": {
            "macos-arm64": sha256_file(macos_path),
            "windows-x86_64": sha256_file(windows_path),
        },
        "phase_semantic_digests": phase_digests,
        "performance": performance,
        "acceptance": acceptance,
        "remote_ci": "not_run",
        "status": "passed",
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    validate_parser = subparsers.add_parser("validate-platform")
    validate_parser.add_argument("report", type=Path)
    validate_parser.add_argument("--platform", choices=sorted(PLATFORM_TARGETS))
    merge_parser = subparsers.add_parser("merge")
    merge_parser.add_argument("--macos", type=Path, required=True)
    merge_parser.add_argument("--windows", type=Path, required=True)
    merge_parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        if arguments.command == "validate-platform":
            result = validate_platform_report(strict_json_load(arguments.report), arguments.platform)
        else:
            result = merge(arguments.macos, arguments.windows, arguments.output)
        print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
        return 0
    except (AcceptanceError, OSError, subprocess.SubprocessError) as error:
        print(f"Sprint 4 acceptance failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
