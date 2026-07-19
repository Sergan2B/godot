#!/usr/bin/env python3
"""Validate and merge local Sprint 5 macOS/Windows script evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import subprocess
import sys
from pathlib import Path
from typing import Any, cast

from script_semantics_fixture import PHASES

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SOURCE_SCOPE_PATH = REPOSITORY_ROOT / "tests" / "codex" / "sprint5_source_scopes.txt"
SOURCE_SCOPES = tuple(
    line
    for line in SOURCE_SCOPE_PATH.read_text(encoding="utf-8").splitlines()
    if line and not line.startswith("#")
)
PLATFORM_TARGETS = {
    "macos-arm64": "aarch64-apple-darwin",
    "windows-x86_64": "x86_64-pc-windows-msvc",
}
MCP_CONTRACT_FIELDS = {
    "closed_input_schemas",
    "cursor_tampering_rejected",
    "cross_filter_cursor_rejected",
    "cross_selector_cursor_rejected",
    "cross_tool_cursor_rejected",
    "exact_tool_registry",
    "limit_bounds_rejected",
    "locals_excluded",
    "stale_generation_cursor_rejected",
    "strict_selector_forms",
    "traversal_rejected",
}
SOURCE_FIELDS = {
    "git_commit",
    "relevant_source_sha256",
    "relevant_source_clean",
    "tracked_file_count",
    "scope_manifest",
    "fixture_sha256",
    "fixture_manifest_sha256",
    "golden_script_graph_sha256",
    "canonical_symbol_ids_sha256",
}
ARTIFACT_FIELDS = {"godot_sha256", "sidecar_sha256", "script_probe_sha256"}
VERSION_FIELDS = {
    "godot",
    "sidecar",
    "python",
    "rustc",
    "cargo",
    "rust_target",
    "bridge_rpc",
    "mcp_protocol",
    "logical_schema",
    "physical_store",
}
PHASE_FIELDS = {
    "phase",
    "initial_script_graph_revision",
    "script_graph_revision",
    "index_revision",
    "document_count",
    "symbol_count",
    "relation_count",
    "diagnostic_count",
    "normalized_script_sha256",
    "assertions",
    "cached_symbol_query_samples_ms",
    "bulk_status_ping_samples_ms",
    "change_visibility_ms",
    "bridge_main_thread_sessions",
    "passed",
}
TELEMETRY_FIELDS = {
    "schema_version",
    "budget_usec",
    "sample_capacity",
    "busy_frame_count",
    "samples_usec",
    "max_elapsed_usec",
    "over_budget_count",
    "overflow",
}
COMPLETION_FIELDS = {
    "requested_phases",
    "completed_phases",
    "all_phases_completed",
    "script_mcp_contract_verified",
    "fixture_integrity_preserved",
    "oracle_accuracy_verified",
    "rust_workspace_tests_passed",
    "rust_clippy_passed",
    "script_fixture_tests_passed",
}
CLEANUP_FIELDS = {
    "editor_processes_stopped",
    "sidecar_processes_stopped",
    "bridge_runtime_files_absent",
    "temporary_workspace_removed",
}
REDACTION_FIELDS = {
    "absolute_paths_absent",
    "bridge_endpoints_absent",
    "secret_material_absent",
    "source_bytes_absent",
    "session_identifiers_absent",
}
ACCURACY_FIELDS = {
    "dynamic_false_exact",
    "dynamic_truth_total",
    "resolvable_recall",
    "resolvable_truth_matched",
    "resolvable_truth_total",
    "symbol_truth_matched",
    "symbol_truth_total",
    "zero_false_exact",
}
TOP_LEVEL_FIELDS = {
    "schema_version",
    "sprint",
    "profile",
    "platform",
    "source",
    "artifacts",
    "versions",
    "adapter_profile",
    "mcp_contract",
    "accuracy",
    "phases",
    "performance",
    "completion",
    "cleanup",
    "redaction",
    "remote_ci",
    "status",
}


class AcceptanceError(RuntimeError):
    """Raised when evidence cannot qualify for Sprint 5 acceptance."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AcceptanceError(message)


def strict_json_load(path: Path) -> dict[str, Any]:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise AcceptanceError(f"duplicate JSON member: {key}")
            result[key] = value
        return result

    def reject_constant(value: str) -> None:
        raise AcceptanceError(f"non-finite JSON number: {value}")

    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=reject_duplicates,
            parse_constant=reject_constant,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise AcceptanceError(f"cannot read strict evidence: {path.name}") from error
    require(isinstance(value, dict), "evidence root must be an object")
    return cast(dict[str, Any], value)


def canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha256_file(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def current_source_digest() -> str:
    require(bool(SOURCE_SCOPES), "Sprint 5 source scope is empty")
    require(tuple(sorted(set(SOURCE_SCOPES))) == SOURCE_SCOPES, "Sprint 5 source scopes are not canonical")
    require(
        "tests/codex/sprint5_source_scopes.txt" in SOURCE_SCOPES,
        "Sprint 5 source scopes do not bind their manifest",
    )
    listed = subprocess.run(
        ["git", "--literal-pathspecs", "ls-files", "-z", "--", *SOURCE_SCOPES],
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
    ).stdout.split(b"\0")
    paths = sorted(path.decode() for path in listed if path)
    require(bool(paths), "Sprint 5 source scope has no tracked files")
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
    cached = [sample for phase in phases for sample in phase["cached_symbol_query_samples_ms"]]
    visibility = [
        phase["change_visibility_ms"]
        for phase in phases
        if phase["phase"] not in {"base", "journal_gap"}
    ]
    control = [sample for phase in phases for sample in phase["bulk_status_ping_samples_ms"]]
    bridge = [
        sample
        for phase in phases
        for session in phase["bridge_main_thread_sessions"]
        for sample in session["samples_usec"]
    ]
    require(
        bool(cached) and len(visibility) >= 8 and bool(control) and bool(bridge),
        "required SLO populations are incomplete",
    )
    cached_p95 = percentile(cached, 95)
    visibility_p95 = percentile(visibility, 95)
    control_p95 = percentile(control, 95)
    if cached_p95 is None or visibility_p95 is None or control_p95 is None:
        raise AcceptanceError("required SLO percentiles are unavailable")
    return {
        "cached_symbol_query_ms": metric(cached),
        "saved_change_visibility_ms": metric(visibility),
        "bulk_status_ping_ms": metric(control),
        "bridge_main_thread_usec": metric(bridge),
        "gates": {
            "cached_symbol_query_p95_lte_300ms": cached_p95 <= 300,
            "saved_change_visibility_p95_lte_2000ms": visibility_p95 <= 2000,
            "bulk_status_ping_p95_lte_200ms": control_p95 <= 200,
            "bridge_main_thread_zero_over_2000us": all(sample <= 2000 for sample in bridge),
        },
    }


def validate_telemetry(record: dict[str, Any]) -> None:
    require(set(record) == TELEMETRY_FIELDS, "Bridge telemetry fields differ")
    samples = record.get("samples_usec")
    require(record.get("schema_version") == 1, "Bridge telemetry schema differs")
    require(record.get("budget_usec") == 2000, "Bridge telemetry budget differs")
    require(record.get("overflow") is False, "Bridge telemetry overflowed")
    if (
        not isinstance(samples, list)
        or not samples
        or not all(isinstance(sample, int) and not isinstance(sample, bool) and sample >= 0 for sample in samples)
    ):
        raise AcceptanceError("Bridge telemetry samples are empty")
    require(
        isinstance(record.get("sample_capacity"), int)
        and not isinstance(record["sample_capacity"], bool)
        and record["sample_capacity"] >= len(samples),
        "Bridge telemetry sample capacity differs",
    )
    require(record.get("busy_frame_count") == len(samples), "Bridge telemetry omitted samples")
    require(record.get("max_elapsed_usec") == max(samples), "Bridge telemetry maximum differs")
    require(
        record.get("over_budget_count") == sum(sample > 2000 for sample in samples),
        "Bridge telemetry over-budget count differs",
    )


def validate_accuracy(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise AcceptanceError("accuracy evidence is missing")
    accuracy = cast(dict[str, Any], value)
    require(set(accuracy) == ACCURACY_FIELDS, "accuracy evidence fields differ")
    require(accuracy.get("zero_false_exact") is True, "false exact semantic facts were reported")
    require(accuracy.get("resolvable_truth_total") == 9, "resolvable truth denominator differs")
    require(accuracy.get("resolvable_truth_matched") == 9, "resolvable truth recall differs")
    require(accuracy.get("resolvable_recall") == 1.0, "resolvable truth recall is not 100%")
    require(accuracy.get("dynamic_truth_total") == 5, "dynamic truth denominator differs")
    require(accuracy.get("dynamic_false_exact") == 0, "dynamic truth received false exact targets")
    require(accuracy.get("symbol_truth_total") == 48, "symbol truth denominator differs")
    require(accuracy.get("symbol_truth_matched") == 48, "symbol/range truth differs")
    return accuracy


def validate_nonnegative_samples(value: Any, context: str, *, nonempty: bool) -> list[float | int]:
    if not isinstance(value, list) or (nonempty and not value):
        raise AcceptanceError(f"{context} samples are incomplete")
    if not all(
        isinstance(sample, (int, float))
        and not isinstance(sample, bool)
        and math.isfinite(sample)
        and sample >= 0
        for sample in value
    ):
        raise AcceptanceError(f"{context} samples are invalid")
    return cast(list[float | int], value)


def validate_redaction(report: dict[str, Any]) -> None:
    serialized = json.dumps(report, ensure_ascii=False, sort_keys=True)
    forbidden = (
        "/Users/",
        "/home/",
        "/tmp/",
        "C:\\",
        "\\\\.\\pipe\\",
        ".godot/imported",
        "session.token",
        "authorization",
        "source_excerpt",
    )
    require(not any(member in serialized for member in forbidden), "evidence contains private host/runtime data")


def validate_platform_report(
    report: dict[str, Any],
    expected_platform: str | None = None,
    *,
    check_checkout: bool = True,
) -> dict[str, Any]:
    require(set(report) == TOP_LEVEL_FIELDS, "platform evidence fields differ")
    validate_redaction(report)
    platform_tag = report.get("platform")
    if not isinstance(platform_tag, str) or platform_tag not in PLATFORM_TARGETS:
        raise AcceptanceError("unsupported Sprint 5 platform")
    if expected_platform is not None:
        require(platform_tag == expected_platform, "platform evidence coordinate differs")
    require(report.get("schema_version") == 1 and report.get("sprint") == 5, "evidence version differs")
    require(report.get("profile") == "qualifying" and report.get("status") == "passed", "report is not qualifying")
    require(report.get("remote_ci") == "not_run", "remote CI coordinate must remain not_run")

    source_value = report.get("source")
    if not isinstance(source_value, dict):
        raise AcceptanceError("source coordinates are missing")
    source = cast(dict[str, Any], source_value)
    require(set(source) == SOURCE_FIELDS, "source coordinate fields differ")
    require(source.get("relevant_source_clean") is True, "producer source was dirty")
    require(isinstance(source.get("git_commit"), str) and len(source["git_commit"]) == 40, "source commit is invalid")
    require(
        isinstance(source.get("tracked_file_count"), int)
        and not isinstance(source["tracked_file_count"], bool)
        and source["tracked_file_count"] > 0,
        "tracked source count is invalid",
    )
    require(
        source.get("scope_manifest") == "tests/codex/sprint5_source_scopes.txt",
        "source scope manifest differs",
    )
    for field in (
        "relevant_source_sha256",
        "fixture_sha256",
        "fixture_manifest_sha256",
        "golden_script_graph_sha256",
        "canonical_symbol_ids_sha256",
    ):
        require(
            isinstance(source.get(field), str) and source[field].startswith("sha256:") and len(source[field]) == 71,
            f"source digest is invalid: {field}",
        )
    if check_checkout:
        require(source["relevant_source_sha256"] == current_source_digest(), "current Sprint 5 source differs")
        commit = subprocess.run(
            ["git", "cat-file", "-e", source["git_commit"] + "^{commit}"],
            cwd=REPOSITORY_ROOT,
            check=False,
        )
        require(commit.returncode == 0, "source-freeze commit is unavailable")

    artifacts_value = report.get("artifacts")
    if not isinstance(artifacts_value, dict):
        raise AcceptanceError("artifact coordinates are missing")
    artifacts = cast(dict[str, Any], artifacts_value)
    require(set(artifacts) == ARTIFACT_FIELDS, "artifact coordinate fields differ")
    require(
        all(isinstance(value, str) and value.startswith("sha256:") and len(value) == 71 for value in artifacts.values()),
        "artifact digest is invalid",
    )

    versions_value = report.get("versions")
    if not isinstance(versions_value, dict):
        raise AcceptanceError("version coordinates are missing")
    versions = cast(dict[str, Any], versions_value)
    require(set(versions) == VERSION_FIELDS, "version coordinate fields differ")
    require(
        all(isinstance(versions[field], str) and versions[field] for field in VERSION_FIELDS),
        "version coordinate is empty",
    )
    require(versions.get("rust_target") == PLATFORM_TARGETS[platform_tag], "Rust target differs from platform")
    require(
        versions.get("bridge_rpc") == "1.4"
        and versions.get("mcp_protocol") == "2025-11-25"
        and versions.get("logical_schema") == "1.3"
        and versions.get("physical_store") == "segment-v3",
        "Sprint 5 protocol/store versions differ",
    )
    adapter = report.get("adapter_profile")
    require(
        adapter
        == {
            "gdscript": "gdscript_parser_analyzer_v1",
            "csharp": "csharp_discovery_only_v1",
            "csharp_runtime_required": False,
        },
        "language adapter profile differs",
    )
    contract_value = report.get("mcp_contract")
    if not isinstance(contract_value, dict):
        raise AcceptanceError("MCP contract fields differ")
    contract = cast(dict[str, Any], contract_value)
    require(set(contract) == MCP_CONTRACT_FIELDS, "MCP contract fields differ")
    require(all(value is True for value in contract.values()), "MCP contract is incomplete")
    validate_accuracy(report.get("accuracy"))

    phase_values = report.get("phases")
    if not isinstance(phase_values, list) or not all(isinstance(phase, dict) for phase in phase_values):
        raise AcceptanceError("canonical phase sequence differs")
    phases = cast(list[dict[str, Any]], phase_values)
    require([phase.get("phase") for phase in phases] == list(PHASES), "canonical phase sequence differs")
    for phase in phases:
        require(set(phase) == PHASE_FIELDS, f"phase fields differ: {phase.get('phase')}")
        require(phase.get("passed") is True, f"phase did not pass: {phase.get('phase')}")
        for field in (
            "initial_script_graph_revision",
            "script_graph_revision",
            "index_revision",
            "document_count",
            "symbol_count",
            "relation_count",
            "diagnostic_count",
        ):
            minimum = 1 if field.endswith("revision") else 0
            require(
                isinstance(phase.get(field), int)
                and not isinstance(phase[field], bool)
                and phase[field] >= minimum,
                f"phase numeric coordinate is invalid: {field}",
            )
        require(
            isinstance(phase.get("normalized_script_sha256"), str)
            and phase["normalized_script_sha256"].startswith("sha256:")
            and len(phase["normalized_script_sha256"]) == 71,
            "phase semantic digest is invalid",
        )
        assertions = phase.get("assertions")
        if not isinstance(assertions, dict) or not assertions:
            raise AcceptanceError("phase assertions are missing")
        require(
            all(isinstance(key, str) and key and value is True for key, value in assertions.items()),
            "phase semantic assertion failed",
        )
        validate_nonnegative_samples(
            phase.get("cached_symbol_query_samples_ms"),
            "cached symbol query",
            nonempty=True,
        )
        validate_nonnegative_samples(
            phase.get("bulk_status_ping_samples_ms"),
            "bulk status ping",
            nonempty=phase["phase"] == "journal_gap",
        )
        visibility = phase.get("change_visibility_ms")
        require(
            visibility is None
            or (
                isinstance(visibility, (int, float))
                and not isinstance(visibility, bool)
                and math.isfinite(visibility)
                and visibility >= 0
            ),
            "change visibility sample is invalid",
        )
        sessions = phase.get("bridge_main_thread_sessions")
        if not isinstance(sessions, list) or len(sessions) != 1 or not isinstance(sessions[0], dict):
            raise AcceptanceError("phase Bridge telemetry is missing")
        validate_telemetry(cast(dict[str, Any], sessions[0]))
    performance = expected_performance(phases)
    require(report.get("performance") == performance, "reported SLO metrics do not match raw samples")
    require(all(performance["gates"].values()), "one or more Sprint 5 SLOs failed")

    completion = report.get("completion")
    if not isinstance(completion, dict):
        raise AcceptanceError("completion evidence is incomplete")
    require(set(completion) == COMPLETION_FIELDS, "completion evidence fields differ")
    require(
        completion.get("requested_phases") == list(PHASES)
        and completion.get("completed_phases") == list(PHASES)
        and all(
            value is True
            for key, value in completion.items()
            if key not in {"requested_phases", "completed_phases"}
        ),
        "completion evidence is incomplete",
    )
    cleanup = report.get("cleanup")
    require(
        isinstance(cleanup, dict)
        and set(cleanup) == CLEANUP_FIELDS
        and all(member is True for member in cleanup.values()),
        "cleanup failed",
    )
    redaction = report.get("redaction")
    require(
        isinstance(redaction, dict)
        and set(redaction) == REDACTION_FIELDS
        and all(member is True for member in redaction.values()),
        "redaction failed",
    )
    return report


def merge(
    macos_path: Path,
    windows_path: Path,
    output: Path,
    *,
    check_checkout: bool = True,
) -> dict[str, Any]:
    macos = validate_platform_report(strict_json_load(macos_path), "macos-arm64", check_checkout=check_checkout)
    windows = validate_platform_report(
        strict_json_load(windows_path), "windows-x86_64", check_checkout=check_checkout
    )
    source_fields = (
        "git_commit",
        "relevant_source_sha256",
        "fixture_sha256",
        "fixture_manifest_sha256",
        "golden_script_graph_sha256",
        "canonical_symbol_ids_sha256",
    )
    require(
        all(macos["source"][field] == windows["source"][field] for field in source_fields),
        "platform reports do not share one source freeze",
    )
    phase_digests: dict[str, str] = {}
    for index, phase_name in enumerate(PHASES):
        left = macos["phases"][index]["normalized_script_sha256"]
        right = windows["phases"][index]["normalized_script_sha256"]
        require(left == right, f"cross-platform script semantics differ: {phase_name}")
        phase_digests[phase_name] = left
    performance = expected_performance(macos["phases"] + windows["phases"])
    require(all(performance["gates"].values()), "combined Sprint 5 SLOs failed")
    aggregate = {
        "schema_version": 1,
        "sprint": 5,
        "profile": "qualifying",
        "platforms": ["macos-arm64", "windows-x86_64"],
        "source": {field: macos["source"][field] for field in source_fields},
        "adapter_profile": macos["adapter_profile"],
        "phase_semantic_digests": phase_digests,
        "accuracy": macos["accuracy"],
        "performance": performance,
        "platform_report_sha256": {
            "macos-arm64": sha256_file(macos_path),
            "windows-x86_64": sha256_file(windows_path),
        },
        "remote_ci": "not_run",
        "status": "passed",
    }
    encoded = canonical_bytes(aggregate)
    if output.exists():
        require(output.read_bytes() == encoded, "refusing to replace different Sprint 5 aggregate evidence")
    else:
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(encoded)
    return aggregate


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    validate = subparsers.add_parser("validate-platform")
    validate.add_argument("report", type=Path)
    validate.add_argument("--platform", choices=tuple(PLATFORM_TARGETS))
    validate.add_argument("--no-checkout", action="store_true")
    aggregate = subparsers.add_parser("merge")
    aggregate.add_argument("macos", type=Path)
    aggregate.add_argument("windows", type=Path)
    aggregate.add_argument("output", type=Path)
    aggregate.add_argument("--no-checkout", action="store_true")
    arguments = parser.parse_args()
    try:
        if arguments.command == "validate-platform":
            result = validate_platform_report(
                strict_json_load(arguments.report),
                arguments.platform,
                check_checkout=not arguments.no_checkout,
            )
        else:
            result = merge(
                arguments.macos,
                arguments.windows,
                arguments.output,
                check_checkout=not arguments.no_checkout,
            )
    except (AcceptanceError, OSError, subprocess.SubprocessError, json.JSONDecodeError) as error:
        print(f"Sprint 5 acceptance failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
