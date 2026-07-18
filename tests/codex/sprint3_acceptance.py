#!/usr/bin/env python3
"""Validate and merge local Sprint 3 cross-platform acceptance evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import subprocess
import tempfile
from pathlib import Path
from typing import Any, cast

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
STORAGE_SPIKE_MANIFEST = REPOSITORY_ROOT / "tests" / "codex" / "storage_spike" / "Cargo.toml"
SOURCE_SCOPE_PATH = REPOSITORY_ROOT / "tests" / "codex" / "sprint3_source_scopes.txt"
SOURCE_SCOPE_MANIFEST = SOURCE_SCOPE_PATH.relative_to(REPOSITORY_ROOT).as_posix()
SOURCE_SCOPES = tuple(
    line for line in SOURCE_SCOPE_PATH.read_text(encoding="utf-8").splitlines() if line and not line.startswith("#")
)
ACCEPTANCE_POLICY_SCOPES = frozenset({
    "docs/codex-integration/SPRINT-3-PLAN.md",
    "docs/codex-integration/SPRINT-3-STAGE-5-PLAN.md",
    "tests/codex/sprint3_acceptance.py",
    "tests/codex/test_sprint3_acceptance.py",
    "tests/codex/storage_spike/README.md",
    "tests/codex/storage_spike/src/evidence.rs",
    "tests/codex/storage_spike/src/main.rs",
    "tests/codex/storage_spike/src/runner.rs",
})
if (
    not SOURCE_SCOPES
    or tuple(sorted(set(SOURCE_SCOPES))) != SOURCE_SCOPES
    or SOURCE_SCOPE_MANIFEST not in SOURCE_SCOPES
):
    raise RuntimeError("Sprint 3 source scopes must be nonempty, unique, and sorted")
GOLDEN_ORACLE_PATH = (
    REPOSITORY_ROOT / "tests" / "codex" / "fixtures" / "resource_graph_oracle" / "golden-resource-graph.json"
)

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
LIVE_PLATFORM_TARGETS = {
    "macos-arm64": "aarch64-apple-darwin",
    "windows-x86_64": "x86_64-pc-windows-msvc",
}
LIVE_PLATFORMS = set(LIVE_PLATFORM_TARGETS)
STORAGE_PLATFORM_ARCHITECTURES = {
    "macos": "aarch64",
    "windows": "x86_64",
}
STORAGE_PLATFORMS = set(STORAGE_PLATFORM_ARCHITECTURES)
STORAGE_GATE_NAMES = {
    "cached_query_p95_lte_300ms",
    "canonical_oracle",
    "concurrent_read_activation",
    "corrupt_cache_isolation",
    "direct_reverse_parity",
    "graceful_cancellation",
    "process_crash_recovery",
    "process_locking_reopen",
    "rename_p95_lte_2s",
    "single_binary_packaging",
    "stress_large_reopen",
    "v1_v2_migration",
}
STORAGE_FAULT_NAMES = {
    "graceful_cancel.capture",
    "graceful_cancel.staging",
    "graceful_cancel.pre_commit",
    "graceful_cancel.post_commit",
    "hard_kill.capture",
    "hard_kill.staging",
    "hard_kill.pre_commit",
    "hard_kill.post_commit",
    "corruption.metadata",
    "corruption.resource",
    "corruption.reverse",
    "corruption.project_binding",
    "corruption.incompatible_schema",
}
TOOLCHAIN_FIELDS = {"python", "scons", "rustc", "cargo", "rust_target"}
COMPLETION_FIELDS = {
    "requested_phases",
    "completed_phases",
    "all_phases_completed",
    "format_import_matrix_verified",
    "resource_mcp_contract_verified",
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
MCP_CONTRACT_FIELDS = {
    "closed_input_schemas",
    "cross_project_cursor_rejected",
    "cross_tool_cursor_rejected",
    "exact_ordering",
    "exact_tool_registry",
    "limit_bounds_rejected",
    "schema_negatives_rejected",
    "stable_project_scope",
    "stale_generation_cursor_rejected",
    "tampered_cursor_rejected",
    "traversal_rejected",
}
PHASE_COMMON_FIELDS = {
    "phase",
    "generation_id",
    "index_revision",
    "resource_count",
    "dependency_count",
    "diagnostic_count",
    "diagnostic_codes",
    "diagnostics_oracle_match",
    "normalized_graph_sha256",
    "direct_reverse_oracle_match",
    "stable_project_scope",
    "startup_visibility_ms",
    "cached_resource_query_samples_ms",
    "bulk_status_ping_samples_ms",
    "passed",
    "bridge_main_thread_sessions",
    "cleanup_verified",
}
PHASE_EXTRA_FIELDS = {
    "base": {
        "reopen_visibility_ms",
        "editor_reopened",
        "sidecar_reopened",
        "same_generation_after_reopen",
        "immutable_segment_set_reused",
    },
    "rename_uid": {
        "startup_generation",
        "startup_index_revision",
        "change_visibility_ms",
        "generation_advanced",
        "entity_identity_preserved",
        "full_rebuild_count_delta",
        "incremental_commit_count",
    },
    "rename_uidless": {
        "startup_generation",
        "startup_index_revision",
        "change_visibility_ms",
        "generation_advanced",
    },
    "delete": {
        "startup_generation",
        "startup_index_revision",
        "change_visibility_ms",
        "generation_advanced",
    },
    "re_add": {
        "startup_generation",
        "startup_index_revision",
        "change_visibility_ms",
        "generation_advanced",
        "removal_visibility_ms",
    },
    "reimport": {
        "startup_generation",
        "startup_index_revision",
        "change_visibility_ms",
        "generation_advanced",
    },
    "content_edit": {
        "startup_generation",
        "startup_index_revision",
        "change_visibility_ms",
        "generation_advanced",
    },
    "journal_gap": {
        "startup_generation",
        "startup_index_revision",
        "change_visibility_ms",
        "generation_advanced",
        "full_rebuild_after_gap",
    },
}
EXPECTED_DIAGNOSTIC_COUNTS = {phase: 3 if phase == "delete" else 2 for phase in PHASES}
EXPECTED_DIAGNOSTIC_CODES = ["missing_dependency", "stale_resource_uid"]
STORAGE_TOP_FIELDS = {
    "schema_version",
    "decision",
    "platform_runs",
    "backend_summaries",
    "cross_platform_complete",
    "chosen_backend",
    "decision_reason",
}
STORAGE_RUN_FIELDS = {
    "schema_version",
    "decision",
    "profile",
    "seed",
    "git_commit",
    "git_dirty",
    "source_tree_sha256",
    "rustc",
    "os",
    "architecture",
    "host",
    "oracle_sha256",
    "dataset",
    "backends",
    "chosen_backend",
    "decision_reason",
}
STORAGE_DATASET_FIELDS = {
    "resources",
    "edges",
    "full_build_iterations",
    "rename_iterations",
    "query_warmup",
    "query_iterations",
    "stress_resources",
    "stress_edges",
    "fan_in_targets",
}
STORAGE_BACKEND_FIELDS = {
    "backend",
    "config",
    "qualified",
    "gates",
    "fault_matrix",
    "full_build_ns",
    "rename_ns",
    "reverse_query_ns",
    "reverse_query_p50_ns",
    "reverse_query_p95_ns",
    "artifact_bytes",
    "rename_changed_blocks",
    "rename_normalized_bytes",
    "rename_write_amplification",
    "binary_bytes",
    "dependency_count",
    "dependencies",
    "weighted_score",
    "weighted_score_ci95_low",
    "weighted_score_ci95_high",
    "errors",
}
STORAGE_SUMMARY_FIELDS = {
    "backend",
    "qualified_all_platforms",
    "median_weighted_score",
    "weighted_score_ci95_low",
    "weighted_score_ci95_high",
}
STORAGE_BACKEND_CONFIGS = {
    "sqlite": {
        "artifact": ".godot/codex/index.sqlite",
        "foreign_keys": "ON",
        "journal_mode": "WAL",
        "lock": ".godot/codex/index.lock",
        "physical_version": "2",
        "sqlite_linkage": "bundled",
        "synchronous": "FULL",
    },
    "segment": {
        "activation": "unique_commit_marker",
        "artifact": ".godot/codex/index/",
        "content_address": "sha256",
        "lock": ".godot/codex/index.lock",
        "physical_version": "2",
        "record_encoding": "length_prefixed_json",
        "shards": "256",
    },
}
ABSOLUTE_PATH = re.compile(
    r"(?:(?<![A-Za-z0-9])[A-Za-z]:[\\/]|\\\\[^\\/\s]+[\\/][^\\/\s]+|"
    r"(?<![A-Za-z0-9/])/(?!/)[^\s]+)",
    re.IGNORECASE,
)
ENCODED_ABSOLUTE_PATH = re.compile(
    r"(?:%2f(?:users|home|private|tmp|var|opt)%2f|"
    r"%5c(?:users|home|private|tmp|var|opt)%5c|"
    r"(?<![a-z0-9])(?:[a-z]|%(?:4[1-9a-f]|5[0-9a]|6[1-9a-f]|7[0-9a]))(?:%3a|:)(?:%2f|%5c)|"
    r"%5c%5c[^%\s/?#]+%5c[^%\s/?#]+)",
    re.IGNORECASE,
)
ENDPOINT_KEYS = {"bridge_endpoint", "endpoint", "pipe_name", "socket_path"}
SECRET_KEYS = {"auth_token", "hmac", "proof", "secret", "session_token", "token"}
SESSION_KEYS = {"editor_session_id", "session_id"}
SOURCE_KEYS = {"file_contents", "script_source", "source_bytes", "source_text"}
SHA256 = re.compile(r"sha256:[0-9a-f]{64}")
RAW_SHA256 = re.compile(r"[0-9a-f]{64}")
GIT_COMMIT = re.compile(r"[0-9a-f]{40}")
BRIDGE_TELEMETRY_SAMPLE_CAPACITY = 16_384
MAX_REDACTION_DECODE_LAYERS = 16
MAX_REDACTION_STRING_BYTES = 1_048_576
AGGREGATE_MAIN_THREAD_FIELDS = {
    "samples",
    "p50",
    "p95",
    "unit",
    "budget_usec",
    "busy_frame_count",
    "max_elapsed_usec",
    "over_budget_count",
    "overflow",
}


class AcceptanceError(RuntimeError):
    """Raised when evidence cannot qualify Sprint 3 acceptance."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AcceptanceError(message)


def require_sha256(value: Any, name: str) -> None:
    require(isinstance(value, str) and SHA256.fullmatch(value) is not None, f"{name} is invalid")


def require_commit(value: Any, name: str) -> None:
    require(isinstance(value, str) and GIT_COMMIT.fullmatch(value) is not None, f"{name} is invalid")


def validate_true_record(record: Any, fields: set[str], name: str) -> None:
    require(isinstance(record, dict) and set(record) == fields, f"{name} fields differ")
    require(all(record[field] is True for field in fields), f"{name} is incomplete")


def redaction_status(value: Any) -> dict[str, bool]:
    """Derive redaction claims from every nested key and string value."""
    keys: set[str] = set()
    strings: list[str] = []

    def visit(item: Any) -> None:
        if isinstance(item, dict):
            for key, child in item.items():
                keys.add(str(key).lower())
                visit(child)
        elif isinstance(item, list):
            for child in item:
                visit(child)
        elif isinstance(item, str):
            strings.append(item)

    visit(value)
    normalized_keys = {re.sub(r"[^a-z0-9]", "", key) for key in keys}
    normalized_keys.difference_update(re.sub(r"[^a-z0-9]", "", key) for key in REDACTION_FIELDS)
    joined = "\n".join(strings)
    lowered = joined.lower()
    endpoint_value = (
        ".sock" in lowered
        or "\\\\.\\pipe\\" in lowered
        or re.search(r"\b(?:file|npipe|pipe|unix)://", lowered) is not None
        or re.search(r"(?:127\.0\.0\.1|localhost):[0-9]{1,5}\b", lowered) is not None
    )
    secret_value = (
        re.search(
            r"(?:\bbearer\s+[a-z0-9._~-]+|\bsk-[a-z0-9_-]{8,}|"
            r"\bgh[pousr]_[a-z0-9]{20,}\b|\bgithub_pat_[a-z0-9_]{20,}\b|"
            r"\b(?:akia|asia|aida|aroa|aipa|anpa|anva)[a-z0-9]{16}\b|"
            r"\beyj[a-z0-9_-]+\.[a-z0-9_-]+\.)",
            lowered,
        )
        is not None
    )
    source_value = (
        any(
            marker in lowered
            for marker in (
                "[gd_resource",
                "[gd_scene",
                "resource_name =",
                "extends node",
                "class_name ",
                "func ",
                "@tool",
            )
        )
        or re.search(
            r"(?:^|\n)\s*(?:var|const)\s+[a-z_][a-z0-9_]*(?:\s*:[^=\n]+)?\s*(?::=|=)",
            lowered,
        )
        is not None
    )
    session_value = (
        re.search(r"\b(?:editor|session):[0-9a-f]{16,}\b", lowered) is not None
        or re.search(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b", lowered) is not None
        or re.search(r"\b(?:sid|session)[_:-][a-z0-9][a-z0-9_-]{7,}\b", lowered) is not None
    )
    endpoint_key = any(
        marker in key
        for key in normalized_keys
        for marker in ("endpoint", "socket", "pipename", "transporturi", "artifacturi")
    )
    secret_key = any(
        marker in key
        for key in normalized_keys
        for marker in (
            "token",
            "secret",
            "password",
            "credential",
            "apikey",
            "hmac",
            "proof",
            "authorization",
            "privatekey",
        )
    )
    source_key = any(
        marker in key
        for key in normalized_keys
        for marker in ("filecontents", "contents", "scriptsource", "sourcebytes", "sourcetext")
    )
    session_key = any(
        marker in key for key in normalized_keys for marker in ("editorsessionid", "sessionidentifier", "sessionid")
    )
    encoded_absolute_path = False
    for item in strings:
        candidate = item
        for _ in range(MAX_REDACTION_DECODE_LAYERS + 1):
            if len(candidate.encode("utf-8", errors="surrogatepass")) > MAX_REDACTION_STRING_BYTES:
                encoded_absolute_path = True
                break
            if ENCODED_ABSOLUTE_PATH.search(candidate) is not None:
                encoded_absolute_path = True
                break
            decoded = re.sub(
                r"%([0-9a-f]{2})",
                lambda match: chr(int(match.group(1), 16)),
                candidate,
                flags=re.IGNORECASE,
            )
            if decoded == candidate:
                break
            candidate = decoded
        else:
            # Fail closed when a value is encoded more deeply than the bounded
            # fixed-point scan can safely inspect.
            encoded_absolute_path = True
        if encoded_absolute_path:
            break
    return {
        "absolute_paths_absent": not any(ABSOLUTE_PATH.search(item) for item in strings)
        and re.search(r"\b(?:file|unix):///(?:users|private|home|opt|var|tmp|[a-z]:/)", lowered) is None
        and not encoded_absolute_path,
        "bridge_endpoints_absent": keys.isdisjoint(ENDPOINT_KEYS) and not endpoint_key and not endpoint_value,
        "secret_material_absent": keys.isdisjoint(SECRET_KEYS) and not secret_key and not secret_value,
        "source_bytes_absent": keys.isdisjoint(SOURCE_KEYS) and not source_key and not source_value,
        "session_identifiers_absent": keys.isdisjoint(SESSION_KEYS) and not session_key and not session_value,
    }


def finite_number(value: Any, name: str, *, positive: bool = False) -> float:
    try:
        finite = math.isfinite(value) if isinstance(value, (int, float)) and not isinstance(value, bool) else False
    except OverflowError:
        finite = False
    require(
        finite,
        f"{name} is not finite",
    )
    require(value > 0 if positive else value >= 0, f"{name} is negative or zero")
    return float(value)


def positive_integer(value: Any, name: str) -> int:
    require(isinstance(value, int) and not isinstance(value, bool) and value > 0, f"{name} is invalid")
    return cast(int, value)


def strict_json_snapshot(path: Path) -> tuple[dict[str, Any], bytes, str]:
    def object_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            require(key not in result, f"duplicate JSON member {key} in {path.name}")
            result[key] = value
        return result

    def reject_constant(constant: str) -> None:
        raise AcceptanceError(f"non-finite JSON number {constant} in {path.name}")

    def finite_float(number: str) -> float:
        value = float(number)
        if not math.isfinite(value):
            raise AcceptanceError(f"non-finite JSON number {number} in {path.name}")
        return value

    try:
        raw_bytes = path.read_bytes()
        raw_text = raw_bytes.decode("utf-8")
        value = json.loads(
            raw_text,
            object_pairs_hook=object_pairs,
            parse_constant=reject_constant,
            parse_float=finite_float,
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AcceptanceError(f"cannot read {path.name}: {error}") from error
    require(isinstance(value, dict), f"{path.name} is not a JSON object")
    return value, raw_bytes, raw_text


def strict_json(path: Path) -> dict[str, Any]:
    return strict_json_snapshot(path)[0]


def validate_storage_canonical_receipt(evidence_bytes: bytes, chosen_backend: str) -> None:
    with tempfile.TemporaryDirectory(prefix="sprint3-storage-receipt-") as temporary:
        snapshot = Path(temporary) / "storage-evidence.json"
        snapshot.write_bytes(evidence_bytes)
        command = [
            "cargo",
            "+1.94.1",
            "run",
            "--quiet",
            "--locked",
            "--release",
            "--manifest-path",
            str(STORAGE_SPIKE_MANIFEST),
            "--",
            "validate",
            str(snapshot),
        ]
        try:
            completed = subprocess.run(
                command,
                cwd=REPOSITORY_ROOT,
                check=False,
                capture_output=True,
                text=True,
                timeout=600,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise AcceptanceError("canonical Rust D-05 validation could not run") from error
    require(
        completed.returncode == 0,
        "storage aggregate differs from canonical Rust raw-sample scoring",
    )
    try:
        receipt = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise AcceptanceError("canonical Rust D-05 validation returned an invalid receipt") from error
    require(
        receipt
        == {
            "canonical": True,
            "chosen_backend": chosen_backend,
            "decision": "D-05",
            "evidence_sha256": f"sha256:{hashlib.sha256(evidence_bytes).hexdigest()}",
            "platform_runs": 2,
            "schema_version": 3,
        },
        "canonical Rust D-05 validation receipt differs",
    )


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def sha256_file(path: Path) -> str:
    return f"sha256:{hashlib.sha256(path.read_bytes()).hexdigest()}"


def expected_query_sample_counts() -> dict[str, int]:
    """Derive the exact paged-query call population from the canonical oracle."""
    oracle = strict_json(GOLDEN_ORACLE_PATH)
    phases = oracle.get("phases")
    require(isinstance(phases, list), "canonical oracle phases are missing")
    counts: dict[str, int] = {}
    for phase in cast(list[Any], phases):
        require(isinstance(phase, dict), "canonical oracle phase is invalid")
        name = phase.get("name")
        resources = phase.get("resources")
        dependencies = phase.get("dependencies")
        direct = phase.get("expected_direct_queries")
        require(
            name in PHASES
            and isinstance(resources, list)
            and isinstance(dependencies, list)
            and isinstance(direct, dict),
            "canonical oracle query population is invalid",
        )
        incoming: dict[str, int] = {}
        for resource in resources:
            require(isinstance(resource, dict), "canonical oracle resource is invalid")
            entity_id = resource.get("entity_id")
            require(isinstance(entity_id, str), "canonical oracle resource identity is invalid")
            incoming[entity_id] = 0
        for dependency in dependencies:
            require(isinstance(dependency, dict), "canonical oracle dependency is invalid")
            target = dependency.get("resolved_target_entity_id")
            if target is not None:
                require(target in incoming, "canonical oracle dependency target is invalid")
                incoming[target] += 1
        direct_calls = 0
        for resource in sorted(resources, key=lambda item: str(item.get("oracle_id"))):
            oracle_id = resource.get("oracle_id")
            expected = direct.get(oracle_id)
            require(isinstance(expected, list), "canonical oracle direct query is invalid")
            direct_calls += max(1, len(expected))
        reverse_calls = sum(max(1, incoming[str(resource["entity_id"])]) for resource in resources)
        counts[str(name)] = direct_calls + reverse_calls
    require(set(counts) == set(PHASES), "canonical oracle query phases differ")
    counts["base"] *= 2
    # The re-add visibility measurement proves both the intermediate removal
    # state and the final restored state against their respective oracles.
    counts["re_add"] += counts["delete"]
    return counts


def checkout_source_coordinates(evidence_commit: str) -> dict[str, Any]:
    require_commit(evidence_commit, "evidence source commit")
    exists = subprocess.run(
        ["git", "cat-file", "-e", f"{evidence_commit}^{{commit}}"],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        timeout=30,
    )
    require(exists.returncode == 0, "evidence source commit does not exist in the current checkout")
    ancestor = subprocess.run(
        ["git", "merge-base", "--is-ancestor", evidence_commit, "HEAD"],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        timeout=30,
    )
    require(ancestor.returncode == 0, "evidence source commit is not an ancestor of current HEAD")
    status = subprocess.run(
        ["git", "--literal-pathspecs", "status", "--porcelain", "--", *SOURCE_SCOPES],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
    )
    require(status.returncode == 0, "git status failed while anchoring acceptance")
    require(not status.stdout, "current Sprint 3 source scope is dirty")
    scoped_diff = subprocess.run(
        [
            "git",
            "--literal-pathspecs",
            "diff",
            "--name-only",
            "-z",
            f"{evidence_commit}..HEAD",
            "--",
            *SOURCE_SCOPES,
        ],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        timeout=30,
    )
    require(scoped_diff.returncode == 0, "git diff failed while anchoring acceptance")
    changed_scopes = {path.decode("utf-8") for path in scoped_diff.stdout.split(b"\0") if path}
    require(
        changed_scopes <= ACCEPTANCE_POLICY_SCOPES,
        "current Sprint 3 producer source differs from the evidence source commit",
    )
    listed = subprocess.run(
        [
            "git",
            "--literal-pathspecs",
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            evidence_commit,
            "--",
            *SOURCE_SCOPES,
        ],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        timeout=30,
    )
    require(listed.returncode == 0, "git ls-tree failed while anchoring acceptance")
    digest = hashlib.sha256()
    for encoded_path in sorted(path for path in listed.stdout.split(b"\0") if path):
        relative = encoded_path.decode("utf-8")
        blob = subprocess.run(
            ["git", "cat-file", "blob", f"{evidence_commit}:{relative}"],
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            timeout=30,
        )
        require(blob.returncode == 0, f"cannot read frozen source blob: {relative}")
        content = blob.stdout
        digest.update(len(encoded_path).to_bytes(8, "big"))
        digest.update(encoded_path)
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    oracle_relative = GOLDEN_ORACLE_PATH.relative_to(REPOSITORY_ROOT).as_posix()
    oracle = subprocess.run(
        ["git", "cat-file", "blob", f"{evidence_commit}:{oracle_relative}"],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        timeout=30,
    )
    require(oracle.returncode == 0, "cannot read frozen canonical oracle")
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
    )
    require(commit.returncode == 0, "git rev-parse failed while anchoring acceptance")
    return {
        "evidence_commit": evidence_commit,
        "head_commit": commit.stdout.strip(),
        "git_dirty": False,
        "source_tree_sha256": f"sha256:{digest.hexdigest()}",
        "oracle_sha256": f"sha256:{hashlib.sha256(oracle.stdout).hexdigest()}",
    }


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
        and all(
            isinstance(sample, (int, float)) and not isinstance(sample, bool) and sample >= 0 for sample in samples
        ),
        f"{name} raw samples are invalid",
    )
    if required:
        require(bool(samples), f"{name} raw samples are empty")
    if samples:
        require(metric["p50"] == percentile(samples, 50), f"{name} p50 does not match raw samples")
        require(metric["p95"] == percentile(samples, 95), f"{name} p95 does not match raw samples")
    else:
        require(metric["p50"] is None and metric["p95"] is None, f"{name} empty summary differs")
    return cast("list[int | float]", samples)


def validate_trace_samples(
    samples: Any,
    name: str,
    *,
    expected_count: int | None = None,
    required: bool = False,
) -> list[int | float]:
    require(
        isinstance(samples, list)
        and all(
            isinstance(sample, (int, float)) and not isinstance(sample, bool) and math.isfinite(sample) and sample >= 0
            for sample in samples
        ),
        f"{name} trace samples are invalid",
    )
    if required:
        require(bool(samples), f"{name} trace samples are empty")
    if expected_count is not None:
        require(len(samples) == expected_count, f"{name} trace sample population differs")
    return cast("list[int | float]", samples)


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
        and isinstance(record["sample_capacity"], int)
        and not isinstance(record["sample_capacity"], bool)
        and record["sample_capacity"] == BRIDGE_TELEMETRY_SAMPLE_CAPACITY
        and isinstance(samples, list)
        and bool(samples)
        and all(isinstance(sample, int) and not isinstance(sample, bool) and sample >= 0 for sample in samples),
        f"{phase} telemetry values are invalid",
    )
    require(
        len(samples) <= record["sample_capacity"],
        f"{phase} telemetry exceeds its sample capacity",
    )
    require(record["overflow"] is False, f"{phase} telemetry overflowed")
    require(
        all(
            isinstance(record[field], int) and not isinstance(record[field], bool) and record[field] >= 0
            for field in ("busy_frame_count", "max_elapsed_usec", "over_budget_count")
        ),
        f"{phase} telemetry counters are invalid",
    )
    require(record["busy_frame_count"] == len(samples), f"{phase} telemetry omitted busy frames")
    require(record["max_elapsed_usec"] == max(samples), f"{phase} telemetry maximum differs")
    require(
        record["over_budget_count"] == sum(sample > 2000 for sample in samples),
        f"{phase} telemetry over-budget count differs",
    )
    return cast(list[int], samples)


def validate_live(path: Path, expected_platform: str | None = None) -> dict[str, Any]:
    evidence, raw_bytes, raw_text = strict_json_snapshot(path)
    require(ABSOLUTE_PATH.search(raw_text) is None, f"{path.name} contains an absolute local path")
    derived_redaction = redaction_status(evidence)
    require(all(derived_redaction.values()), f"{path.name} contains redacted local or secret material")
    required_top = {
        "schema_version",
        "stage",
        "status",
        "execution",
        "profile",
        "platform",
        "host",
        "toolchain",
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
        "mcp_contract",
        "metrics_ms",
        "bridge_main_thread",
        "slo",
        "completion",
        "cleanup",
        "redaction",
        "platform_evidence",
    }
    require(set(evidence) == required_top, f"{path.name} top-level contract differs")
    platform_tag = evidence["platform"]
    require(platform_tag in LIVE_PLATFORMS, f"unsupported live platform {platform_tag}")
    if expected_platform is not None:
        require(platform_tag == expected_platform, f"expected {expected_platform}, got {platform_tag}")
    require(
        evidence["schema_version"] == 3
        and evidence["stage"] == "Sprint 3 Stage 5 / S3-09-S3-10"
        and evidence["status"] == "passed"
        and evidence["execution"] == "local_model_free"
        and evidence["profile"] == "acceptance",
        f"{path.name} is not qualifying live evidence",
    )
    require(isinstance(evidence["host"], str) and bool(evidence["host"]), f"{path.name} host is missing")
    require_commit(evidence["git_commit"], f"{path.name} git commit")
    require(evidence["git_dirty"] is False, f"{path.name} was produced from dirty relevant source")
    for key in ("source_tree_sha256", "oracle_sha256"):
        require_sha256(evidence[key], key)

    toolchain = evidence["toolchain"]
    require(isinstance(toolchain, dict) and set(toolchain) == TOOLCHAIN_FIELDS, "toolchain fields differ")
    require(
        all(isinstance(toolchain[field], str) and bool(toolchain[field]) for field in TOOLCHAIN_FIELDS),
        "toolchain is incomplete",
    )
    require(toolchain["rustc"].startswith("rustc 1.94.1 "), "Rust toolchain is not pinned to 1.94.1")
    require(toolchain["cargo"].startswith("cargo 1.94.1 "), "Cargo toolchain is not pinned to 1.94.1")
    require(toolchain["rust_target"] == LIVE_PLATFORM_TARGETS[platform_tag], "Rust target differs from live platform")

    completion = evidence["completion"]
    require(isinstance(completion, dict) and set(completion) == COMPLETION_FIELDS, "completion fields differ")
    require(
        completion["requested_phases"] == list(PHASES)
        and completion["completed_phases"] == list(PHASES)
        and completion["all_phases_completed"] is True
        and completion["format_import_matrix_verified"] is True
        and completion["resource_mcp_contract_verified"] is True,
        "live completion is incomplete",
    )
    validate_true_record(evidence["cleanup"], CLEANUP_FIELDS, "cleanup")
    require(
        isinstance(evidence["redaction"], dict)
        and set(evidence["redaction"]) == REDACTION_FIELDS
        and evidence["redaction"] == derived_redaction,
        "redaction claims do not match the serialized evidence",
    )
    require(
        isinstance(evidence["fixture_digest_before"], str)
        and RAW_SHA256.fullmatch(evidence["fixture_digest_before"]) is not None
        and evidence["fixture_digest_before"] == evidence["fixture_digest_after"]
        and evidence["canonical_fixture_unchanged"] is True,
        f"{path.name} changed the canonical fixture",
    )
    require(evidence["mcp_protocol"] == "2025-11-25", "MCP protocol differs")
    require(evidence["tools"] == sorted(TOOLS), "MCP tool set differs")
    validate_true_record(evidence["mcp_contract"], MCP_CONTRACT_FIELDS, "live MCP contract")
    require(evidence["platform_evidence"] == {"remote_ci": "not_run"}, "remote CI state differs")
    artifacts = evidence["artifacts"]
    require(
        isinstance(artifacts, dict)
        and set(artifacts) == {"godot_version", "godot_sha256", "sidecar_version", "sidecar_sha256"}
        and isinstance(artifacts["godot_version"], str)
        and bool(artifacts["godot_version"])
        and isinstance(artifacts["sidecar_version"], str)
        and bool(artifacts["sidecar_version"]),
        "artifact identity is incomplete",
    )
    require_sha256(artifacts["godot_sha256"], "Godot artifact SHA-256")
    require_sha256(artifacts["sidecar_sha256"], "sidecar artifact SHA-256")
    require(
        evidence["git_commit"][:9] in artifacts["godot_version"],
        "Godot artifact version is not bound to the evidence commit",
    )

    phases = evidence["phases"]
    require(
        isinstance(phases, list)
        and all(isinstance(phase, dict) for phase in phases)
        and [phase.get("phase") for phase in phases] == list(PHASES),
        "live phases differ",
    )
    telemetry_samples: list[int] = []
    query_trace_samples: list[int | float] = []
    status_trace_samples: list[int | float] = []
    query_sample_counts = expected_query_sample_counts()
    graph_digests: dict[str, str] = {}
    for phase in phases:
        name = phase["phase"]
        require(set(phase) == PHASE_COMMON_FIELDS | PHASE_EXTRA_FIELDS[name], f"{name} phase fields differ")
        require(
            phase["passed"] is True
            and phase["direct_reverse_oracle_match"] is True
            and phase["stable_project_scope"] is True
            and phase["cleanup_verified"] is True,
            f"{name} did not pass",
        )
        require(
            isinstance(phase["generation_id"], str)
            and re.fullmatch(r"generation:sha256:[0-9a-f]{64}", phase["generation_id"]) is not None
            and isinstance(phase["index_revision"], int)
            and not isinstance(phase["index_revision"], bool)
            and phase["index_revision"] > 0
            and isinstance(phase["resource_count"], int)
            and not isinstance(phase["resource_count"], bool)
            and phase["resource_count"] > 0
            and isinstance(phase["dependency_count"], int)
            and not isinstance(phase["dependency_count"], bool)
            and phase["dependency_count"] >= 0,
            f"{name} generation or graph counts are invalid",
        )
        diagnostic_codes = phase.get("diagnostic_codes")
        require(
            phase.get("diagnostics_oracle_match") is True
            and phase.get("diagnostic_count") == EXPECTED_DIAGNOSTIC_COUNTS[name]
            and diagnostic_codes == EXPECTED_DIAGNOSTIC_CODES,
            f"{name} diagnostics differ from the canonical oracle",
        )
        digest = phase.get("normalized_graph_sha256")
        require_sha256(digest, f"{name} graph digest")
        graph_digests[name] = digest
        query_trace_samples.extend(
            validate_trace_samples(
                phase["cached_resource_query_samples_ms"],
                f"{name} cached resource query",
                expected_count=query_sample_counts[name],
                required=True,
            )
        )
        phase_status_samples = validate_trace_samples(
            phase["bulk_status_ping_samples_ms"],
            f"{name} bulk status/ping",
            required=name == "journal_gap",
        )
        require(
            name == "journal_gap" or not phase_status_samples,
            f"{name} contains bulk status samples outside the journal-gap phase",
        )
        status_trace_samples.extend(phase_status_samples)
        session_records = phase.get("bridge_main_thread_sessions")
        expected_session_count = 2 if name == "base" else 1
        require(
            isinstance(session_records, list) and len(session_records) == expected_session_count,
            f"{name} telemetry session population differs",
        )
        for session_index, session_record in enumerate(session_records, start=1):
            telemetry_samples.extend(validate_phase_telemetry(session_record, f"{name} session {session_index}"))
        finite_number(phase["startup_visibility_ms"], f"{name} startup visibility")
        if name == "base":
            finite_number(phase["reopen_visibility_ms"], "base reopen visibility")
        else:
            require(
                isinstance(phase["startup_generation"], str)
                and re.fullmatch(r"generation:sha256:[0-9a-f]{64}", phase["startup_generation"]) is not None
                and isinstance(phase["startup_index_revision"], int)
                and not isinstance(phase["startup_index_revision"], bool)
                and phase["startup_index_revision"] > 0
                and phase["generation_id"] != phase["startup_generation"]
                and phase["index_revision"] > phase["startup_index_revision"]
                and phase["generation_advanced"] is True,
                f"{name} startup or generation transition is invalid",
            )
            finite_number(phase["change_visibility_ms"], f"{name} change visibility")
        if name == "re_add":
            finite_number(phase["removal_visibility_ms"], "re-add removal visibility")
    base = phases[0]
    require(
        base.get("editor_reopened") is True
        and base.get("sidecar_reopened") is True
        and base.get("same_generation_after_reopen") is True
        and base.get("immutable_segment_set_reused") is True,
        "compatible reopen was not proven",
    )
    rename_uid = phases[PHASES.index("rename_uid")]
    require(
        rename_uid.get("entity_identity_preserved") is True
        and rename_uid.get("full_rebuild_count_delta") == 0
        and rename_uid.get("incremental_commit_count") == 1,
        "UID-preserving rename was not proven incremental",
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
    startup = validate_metric(metrics["startup_visibility"], "startup visibility")
    reopen = validate_metric(metrics["compatible_reopen"], "compatible reopen")
    rebuild = validate_metric(metrics["journal_gap_full_rebuild"], "journal gap rebuild")
    expected_visibility = [
        sample
        for phase in phases
        if phase["phase"] not in {"base", "journal_gap"}
        for sample in (
            [phase["removal_visibility_ms"], phase["change_visibility_ms"]]
            if phase["phase"] == "re_add"
            else [phase["change_visibility_ms"]]
        )
    ]
    require(query == query_trace_samples, "cached resource query population differs from phase traces")
    require(visibility == expected_visibility, "ordinary visibility population differs from phase timings")
    require(status == status_trace_samples, "bulk status/ping population differs from phase traces")
    require(
        startup == [phase["startup_visibility_ms"] for phase in phases],
        "startup visibility population differs from phase timings",
    )
    require(reopen == [base["reopen_visibility_ms"]], "compatible reopen population differs from phase timing")
    require(
        rebuild == [phases[-1]["change_visibility_ms"]],
        "journal-gap rebuild population differs from phase timing",
    )

    main_thread = evidence["bridge_main_thread"]
    require(
        isinstance(main_thread, dict) and set(main_thread) == AGGREGATE_MAIN_THREAD_FIELDS,
        "aggregate main-thread telemetry fields differ",
    )
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
        "sha256": f"sha256:{hashlib.sha256(raw_bytes).hexdigest()}",
    }


def choose_backend(records: list[dict[str, Any]]) -> str:
    qualified = [record for record in records if record.get("qualified") is True]
    if not qualified:
        return "blocked"
    if len(qualified) == 1:
        return str(qualified[0]["backend"])
    by_name = {str(record["backend"]): record for record in qualified}
    sqlite = by_name["sqlite"]
    segment = by_name["segment"]
    intervals_overlap = (
        sqlite["weighted_score_ci95_low"] <= segment["weighted_score_ci95_high"]
        and segment["weighted_score_ci95_low"] <= sqlite["weighted_score_ci95_high"]
    )
    if abs(sqlite["weighted_score"] - segment["weighted_score"]) < 0.05 or intervals_overlap:
        return "sqlite"
    return cast(str, max(qualified, key=lambda record: record["weighted_score"])["backend"])


def validate_storage_backend(backend: Any, os_name: str, dataset: dict[str, Any]) -> None:
    require(
        isinstance(backend, dict) and set(backend) == STORAGE_BACKEND_FIELDS,
        f"{os_name} storage backend fields differ",
    )
    backend_name = backend["backend"]
    require(backend_name in {"sqlite", "segment"}, f"{os_name} storage backend is unsupported")
    config = backend["config"]
    require(config == STORAGE_BACKEND_CONFIGS[backend_name], f"{os_name}/{backend_name} config differs")
    require(int(config["physical_version"]) > 0, f"{os_name}/{backend_name} physical version is invalid")
    if backend_name == "segment":
        require(int(config["shards"]) > 0, f"{os_name}/{backend_name} shard count is invalid")

    sample_fields = {
        "full_build_ns": dataset["full_build_iterations"],
        "rename_ns": dataset["rename_iterations"],
        "reverse_query_ns": dataset["query_iterations"],
    }
    for field, expected_count in sample_fields.items():
        samples = backend[field]
        require(
            isinstance(samples, list)
            and len(samples) == expected_count
            and all(isinstance(sample, int) and not isinstance(sample, bool) and sample > 0 for sample in samples),
            f"{os_name}/{backend_name} {field} sample population differs",
        )
    require(
        backend["reverse_query_p50_ns"] == percentile(backend["reverse_query_ns"], 50)
        and backend["reverse_query_p95_ns"] == percentile(backend["reverse_query_ns"], 95),
        f"{os_name}/{backend_name} query percentiles differ from raw samples",
    )

    artifact_bytes = positive_integer(backend["artifact_bytes"], f"{os_name}/{backend_name} artifact bytes")
    changed_blocks = positive_integer(
        backend["rename_changed_blocks"], f"{os_name}/{backend_name} rename changed blocks"
    )
    normalized_bytes = positive_integer(
        backend["rename_normalized_bytes"], f"{os_name}/{backend_name} normalized rename bytes"
    )
    positive_integer(backend["binary_bytes"], f"{os_name}/{backend_name} binary bytes")
    dependency_count = positive_integer(backend["dependency_count"], f"{os_name}/{backend_name} dependency count")
    amplification = finite_number(
        backend["rename_write_amplification"],
        f"{os_name}/{backend_name} rename write amplification",
        positive=True,
    )
    require(
        math.isclose(amplification, changed_blocks * 4096.0 / normalized_bytes, rel_tol=0.0, abs_tol=1.0e-12),
        f"{os_name}/{backend_name} rename write amplification differs",
    )
    require(artifact_bytes > 0, f"{os_name}/{backend_name} artifact size is invalid")
    dependencies = backend["dependencies"]
    require(
        isinstance(dependencies, list) and len(dependencies) == dependency_count,
        f"{os_name}/{backend_name} dependency count differs from its manifest",
    )
    dependency_keys: set[tuple[str, str, str]] = set()
    for dependency in dependencies:
        require(
            isinstance(dependency, dict)
            and set(dependency) == {"name", "version", "source", "license"}
            and all(
                isinstance(dependency[field], str) and bool(dependency[field])
                for field in ("name", "version", "source")
            )
            and (dependency["license"] is None or isinstance(dependency["license"], str)),
            f"{os_name}/{backend_name} dependency manifest entry differs",
        )
        dependency_keys.add((dependency["name"], dependency["version"], dependency["source"]))
    require(
        len(dependency_keys) == len(dependencies),
        f"{os_name}/{backend_name} dependency manifest contains duplicates",
    )

    score = finite_number(backend["weighted_score"], f"{os_name}/{backend_name} weighted score")
    score_low = finite_number(
        backend["weighted_score_ci95_low"], f"{os_name}/{backend_name} weighted score lower bound"
    )
    score_high = finite_number(
        backend["weighted_score_ci95_high"], f"{os_name}/{backend_name} weighted score upper bound"
    )
    require(score_low <= score_high, f"{os_name}/{backend_name} weighted score interval is inverted")
    require(score >= 0, f"{os_name}/{backend_name} weighted score is invalid")

    gates = backend["gates"]
    faults = backend["fault_matrix"]
    require(
        isinstance(gates, dict)
        and set(gates) == STORAGE_GATE_NAMES
        and all(isinstance(value, bool) for value in gates.values()),
        f"{os_name}/{backend_name} storage gate map differs",
    )
    require(
        isinstance(faults, dict)
        and set(faults) == STORAGE_FAULT_NAMES
        and all(isinstance(value, bool) for value in faults.values()),
        f"{os_name}/{backend_name} storage fault map differs",
    )
    require(
        isinstance(backend["qualified"], bool)
        and backend["qualified"] is all(gates.values())
        and gates["graceful_cancellation"]
        is all(value for name, value in faults.items() if name.startswith("graceful_cancel."))
        and gates["process_crash_recovery"]
        is all(value for name, value in faults.items() if name.startswith("hard_kill."))
        and gates["corrupt_cache_isolation"]
        is all(value for name, value in faults.items() if name.startswith("corruption.")),
        f"{os_name}/{backend_name} storage qualification contradicts its outcomes",
    )
    require(
        gates["cached_query_p95_lte_300ms"] is (backend["reverse_query_p95_ns"] <= 300_000_000)
        and gates["rename_p95_lte_2s"] is (percentile(backend["rename_ns"], 95) <= 2_000_000_000),
        f"{os_name}/{backend_name} performance gate differs from raw samples",
    )
    require(
        isinstance(backend["errors"], list) and all(isinstance(error, str) for error in backend["errors"]),
        f"{os_name}/{backend_name} storage errors differ",
    )
    if backend["qualified"]:
        require(not backend["errors"], f"{os_name}/{backend_name} qualified with reported errors")


def validate_storage(path: Path) -> dict[str, Any]:
    evidence, raw_bytes, raw_text = strict_json_snapshot(path)
    require(ABSOLUTE_PATH.search(raw_text) is None, f"{path.name} contains an absolute local path")
    require(
        all(redaction_status(evidence).values()),
        f"{path.name} contains redacted local or secret material",
    )
    require(
        set(evidence) == STORAGE_TOP_FIELDS
        and evidence.get("schema_version") == 3
        and evidence.get("decision") == "D-05"
        and evidence.get("cross_platform_complete") is True
        and evidence.get("chosen_backend") == "segment"
        and isinstance(evidence.get("decision_reason"), str)
        and bool(evidence["decision_reason"]),
        "cross-platform D-05 decision is incomplete or differs",
    )
    runs = evidence["platform_runs"]
    expected_coordinates = list(STORAGE_PLATFORM_ARCHITECTURES.items())
    require(
        isinstance(runs, list)
        and len(runs) == len(expected_coordinates)
        and all(isinstance(run, dict) for run in runs)
        and [(run.get("os"), run.get("architecture")) for run in runs] == expected_coordinates,
        "storage platform coordinates differ or contain duplicates",
    )
    commits: set[str] = set()
    source_digests: set[str] = set()
    oracle_digests: set[str] = set()
    segment_claims: dict[str, dict[str, bool]] = {}
    for run in runs:
        os_name = run["os"]
        require(set(run) == STORAGE_RUN_FIELDS, f"{os_name} storage run fields differ")
        require(
            run["schema_version"] == 2
            and run["decision"] == "D-05"
            and run["profile"] == "decision"
            and run["seed"] == "D05-1"
            and run["git_dirty"] is False
            and isinstance(run["chosen_backend"], str)
            and isinstance(run["decision_reason"], str)
            and bool(run["decision_reason"]),
            f"{os_name} storage run is not a clean D-05 decision profile",
        )
        require_commit(run["git_commit"], f"{os_name} storage git commit")
        require_sha256(run["source_tree_sha256"], f"{os_name} storage source digest")
        require_sha256(run["oracle_sha256"], f"{os_name} storage oracle digest")
        require(
            isinstance(run["rustc"], str) and run["rustc"].startswith("rustc 1.94.1 "),
            f"{os_name} storage Rust toolchain differs",
        )
        require(
            isinstance(run["host"], dict)
            and set(run["host"]) == {"runner", "logical_cpus"}
            and run["host"]["runner"] == "local"
            and isinstance(run["host"]["logical_cpus"], int)
            and not isinstance(run["host"]["logical_cpus"], bool)
            and run["host"]["logical_cpus"] > 0,
            f"{os_name} storage host coordinates differ",
        )
        dataset = run["dataset"]
        require(
            isinstance(dataset, dict)
            and set(dataset) == STORAGE_DATASET_FIELDS
            and dataset
            == {
                "resources": 10_000,
                "edges": 50_000,
                "full_build_iterations": 5,
                "rename_iterations": 50,
                "query_warmup": 1_000,
                "query_iterations": 10_000,
                "stress_resources": 100_000,
                "stress_edges": 500_000,
                "fan_in_targets": [0, 1, 10, 100, 1_000],
            },
            f"{os_name} storage decision dataset differs",
        )
        backends = run["backends"]
        require(
            isinstance(backends, list)
            and len(backends) == 2
            and all(isinstance(backend, dict) for backend in backends)
            and {backend.get("backend") for backend in backends} == {"sqlite", "segment"},
            f"{os_name} storage candidates differ or contain duplicates",
        )
        for backend in backends:
            validate_storage_backend(backend, os_name, dataset)
        require(
            run["chosen_backend"] == choose_backend(backends),
            f"{os_name} storage choice differs from its raw backend evidence",
        )
        segment = next(backend for backend in backends if backend["backend"] == "segment")
        require(
            segment["qualified"] is True and all(segment["gates"].values()) and all(segment["fault_matrix"].values()),
            f"{os_name} segment store did not pass every D-05 outcome",
        )
        segment_claims[os_name] = segment["gates"] | segment["fault_matrix"]
        commits.add(run["git_commit"])
        source_digests.add(run["source_tree_sha256"])
        oracle_digests.add(run["oracle_sha256"])

    require(
        len(commits) == len(source_digests) == len(oracle_digests) == 1,
        "storage source-freeze coordinates differ",
    )
    summaries = evidence["backend_summaries"]
    require(
        isinstance(summaries, list)
        and len(summaries) == 2
        and all(isinstance(summary, dict) for summary in summaries)
        and {summary.get("backend") for summary in summaries} == {"sqlite", "segment"},
        "cross-platform storage summaries differ or contain duplicates",
    )
    for summary in summaries:
        require(
            isinstance(summary, dict)
            and set(summary) == STORAGE_SUMMARY_FIELDS
            and isinstance(summary["qualified_all_platforms"], bool),
            "cross-platform storage summary fields differ",
        )
        backend_name = summary["backend"]
        expected_qualified = all(
            next(backend for backend in run["backends"] if backend["backend"] == backend_name)["qualified"]
            for run in runs
        )
        scores = sorted(
            next(backend for backend in run["backends"] if backend["backend"] == backend_name)["weighted_score"]
            for run in runs
        )
        median_score = (scores[(len(scores) - 1) // 2] + scores[len(scores) // 2]) / 2.0
        require(
            summary["qualified_all_platforms"] is expected_qualified
            and summary["median_weighted_score"] == median_score,
            f"cross-platform {backend_name} summary differs from platform runs",
        )
        summary_score = finite_number(summary["median_weighted_score"], f"cross-platform {backend_name} median score")
        summary_low = finite_number(
            summary["weighted_score_ci95_low"], f"cross-platform {backend_name} score lower bound"
        )
        summary_high = finite_number(
            summary["weighted_score_ci95_high"], f"cross-platform {backend_name} score upper bound"
        )
        require(summary_low <= summary_high, f"cross-platform {backend_name} score interval is inverted")
        require(summary_score >= 0, f"cross-platform {backend_name} score is invalid")
    segment_summary = next(summary for summary in summaries if summary["backend"] == "segment")
    require(segment_summary["qualified_all_platforms"] is True, "segment is not qualified on all storage platforms")
    aggregate_choice = choose_backend([
        {
            "backend": summary["backend"],
            "qualified": summary["qualified_all_platforms"],
            "weighted_score": summary["median_weighted_score"],
            "weighted_score_ci95_low": summary["weighted_score_ci95_low"],
            "weighted_score_ci95_high": summary["weighted_score_ci95_high"],
        }
        for summary in summaries
    ])
    require(
        evidence["chosen_backend"] == aggregate_choice == "segment",
        "cross-platform D-05 choice differs from its backend summaries",
    )
    validate_storage_canonical_receipt(raw_bytes, evidence["chosen_backend"])
    return {
        "evidence": evidence,
        "git_commit": next(iter(commits)),
        "source_tree_sha256": next(iter(source_digests)),
        "oracle_sha256": next(iter(oracle_digests)),
        "segment_claims": segment_claims,
        "sha256": f"sha256:{hashlib.sha256(raw_bytes).hexdigest()}",
    }


def merge_acceptance(
    macos_live: Path,
    windows_live: Path,
    storage_path: Path,
    output: Path,
    *,
    anchor_to_checkout: bool = True,
) -> dict[str, Any]:
    macos = validate_live(macos_live, "macos-arm64")
    windows = validate_live(windows_live, "windows-x86_64")
    storage = validate_storage(storage_path)
    mac = macos["evidence"]
    win = windows["evidence"]
    require(
        {mac["platform"], win["platform"]} == LIVE_PLATFORMS,
        "live platform coordinates differ or contain duplicates",
    )
    require(mac["git_commit"] == win["git_commit"] == storage["git_commit"], "live/storage commits differ")
    require(
        mac["source_tree_sha256"] == win["source_tree_sha256"] == storage["source_tree_sha256"],
        "live/storage source digests differ",
    )
    require(
        mac["oracle_sha256"] == win["oracle_sha256"] == storage["oracle_sha256"],
        "live/storage oracle digests differ",
    )
    if anchor_to_checkout:
        checkout = checkout_source_coordinates(mac["git_commit"])
        require(checkout["git_dirty"] is False, "current Sprint 3 source scope is dirty")
        require(
            checkout["source_tree_sha256"] == mac["source_tree_sha256"],
            "evidence source digest differs from current checkout",
        )
        require(
            checkout["oracle_sha256"] == mac["oracle_sha256"],
            "evidence oracle digest differs from current checkout",
        )
    require(macos["graph_digests"] == windows["graph_digests"], "Windows/macOS normalized graphs differ")
    live_reports = (mac, win)
    live_phases = (
        {phase["phase"]: phase for phase in mac["phases"]},
        {phase["phase"]: phase for phase in win["phases"]},
    )

    def storage_claim(name: str) -> bool:
        return all(claims[name] is True for claims in storage["segment_claims"].values())

    claims = {
        "live.direct_reverse_oracle": all(
            phase["direct_reverse_oracle_match"] is True for phases in live_phases for phase in phases.values()
        ),
        "live.uid_rename_incremental": all(
            phases["rename_uid"]["entity_identity_preserved"] is True
            and phases["rename_uid"]["full_rebuild_count_delta"] == 0
            and phases["rename_uid"]["incremental_commit_count"] == 1
            and phases["rename_uid"]["direct_reverse_oracle_match"] is True
            for phases in live_phases
        ),
        "live.stale_uid_diagnostic": all(
            phase["diagnostics_oracle_match"] is True and "stale_resource_uid" in phase["diagnostic_codes"]
            for phases in live_phases
            for phase in phases.values()
        ),
        "live.compatible_reopen": all(
            phases["base"]["editor_reopened"] is True
            and phases["base"]["sidecar_reopened"] is True
            and phases["base"]["same_generation_after_reopen"] is True
            and phases["base"]["immutable_segment_set_reused"] is True
            for phases in live_phases
        ),
        "storage.reopen": storage_claim("process_locking_reopen") and storage_claim("stress_large_reopen"),
        "storage.cancellation_matrix": storage_claim("graceful_cancellation")
        and all(storage_claim(name) for name in STORAGE_FAULT_NAMES if name.startswith("graceful_cancel.")),
        "storage.corruption_migration_recovery": storage_claim("corrupt_cache_isolation")
        and storage_claim("process_crash_recovery")
        and storage_claim("v1_v2_migration")
        and all(storage_claim(name) for name in STORAGE_FAULT_NAMES if name.startswith("hard_kill."))
        and all(storage_claim(name) for name in STORAGE_FAULT_NAMES if name.startswith("corruption.")),
        "live.format_import_matrix": all(
            report["completion"]["format_import_matrix_verified"] is True for report in live_reports
        ),
        "live.all_phases": all(
            report["completion"]["all_phases_completed"] is True
            and all(phase["passed"] is True for phase in report["phases"])
            for report in live_reports
        ),
        "live.delete_readd_reimport_gap": all(
            all(phases[name]["passed"] is True for name in ("delete", "re_add", "reimport", "journal_gap"))
            and phases["journal_gap"]["full_rebuild_after_gap"] is True
            for phases in live_phases
        ),
        "mcp.resource_contract": all(
            set(report["tools"]) == TOOLS
            and report["completion"]["resource_mcp_contract_verified"] is True
            and all(report["mcp_contract"][field] is True for field in MCP_CONTRACT_FIELDS)
            for report in live_reports
        ),
        "live.query_visibility_slo": all(
            report["slo"]["cached_resource_query_p95_lte_300ms"] is True
            and report["slo"]["ordinary_incremental_visibility_p95_lte_2000ms"] is True
            for report in live_reports
        ),
        "live.bulk_status_main_thread_slo": all(
            report["slo"]["bulk_status_ping_p95_lte_200ms"] is True
            and report["slo"]["bridge_main_thread_over_2000us_zero"] is True
            for report in live_reports
        ),
        "live.cross_platform_graph_digest": macos["graph_digests"] == windows["graph_digests"],
    }
    criterion_claims = {
        "S3-AC-01": ["live.direct_reverse_oracle"],
        "S3-AC-02": ["live.uid_rename_incremental"],
        "S3-AC-03": ["live.stale_uid_diagnostic"],
        "S3-AC-04": ["live.compatible_reopen", "storage.reopen"],
        "S3-AC-05": ["storage.cancellation_matrix"],
        "S3-AC-06": ["storage.corruption_migration_recovery"],
        "S3-AC-07": ["live.format_import_matrix", "live.all_phases"],
        "S3-AC-08": ["live.delete_readd_reimport_gap"],
        "S3-AC-09": ["mcp.resource_contract"],
        "S3-AC-10": ["live.query_visibility_slo"],
        "S3-AC-11": ["live.bulk_status_main_thread_slo"],
        "S3-AC-12": ["live.cross_platform_graph_digest"],
    }
    acceptance_criteria = {
        criterion: {"passed": all(claims[claim] for claim in proof), "proof": proof}
        for criterion, proof in criterion_claims.items()
    }
    require(all(criterion["passed"] for criterion in acceptance_criteria.values()), "acceptance claim failed")
    result = {
        "schema_version": 3,
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
        "claims": claims,
        "acceptance_criteria": acceptance_criteria,
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
