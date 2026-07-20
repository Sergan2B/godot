#!/usr/bin/env python3
"""Build, run, validate, and record the local Sprint 6 macOS gate."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, cast

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint6_source_scopes.txt"
LIVE_RUNNER = SCRIPT_DIR / "sprint6_semantic_context_live.py"
EVIDENCE_PATH = SCRIPT_DIR / "evidence" / "sprint-6-semantic-context-macos.json"
GODOT = REPOSITORY_ROOT / "bin" / "godot.macos.editor.dev.arm64"
SIDECAR = REPOSITORY_ROOT / "godot-codex-mcp" / "target" / "release" / "godot-codex-mcp"
MANIFEST = SCRIPT_DIR / "fixtures" / "semantic_context_oracle" / "fixture-manifest.json"
GOLDEN = SCRIPT_DIR / "fixtures" / "semantic_context_oracle" / "golden-usages.json"

PHASES = (
    "base",
    "resource_uid_rename",
    "symbol_rename",
    "duplicate_provenance",
    "partial_domains",
    "conflict",
    "budget_pressure",
    "journal_gap",
)
LOCAL_GATES = {
    "fixture_oracle",
    "python_regressions",
    "rust_format",
    "rust_workspace_tests",
    "rust_clippy",
    "godot_editor_build",
    "cpp_semantic_adapter",
    "release_sidecar_build",
    "model_free_live",
}
MCP_FIELDS = {
    "closed_input_schemas",
    "cross_filter_cursor_rejected",
    "cross_selector_cursor_rejected",
    "exact_ten_tool_registry",
    "fixed_project_resource",
    "limit_bounds_rejected",
    "read_only_annotations",
    "scene_resource_template",
    "signed_cursor_tampering_rejected",
    "subscriptions_disabled",
}
TOP_LEVEL_FIELDS = {
    "schema_version",
    "sprint",
    "profile",
    "platform",
    "source",
    "versions",
    "artifacts",
    "coordinates",
    "mcp_contract",
    "accuracy",
    "summaries",
    "performance",
    "live_phases",
    "phase_coverage",
    "local_gates",
    "codex_cli_smoke",
    "cleanup",
    "redaction",
    "external_gates",
    "status",
}
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")


class AcceptanceError(RuntimeError):
    """Raised when local evidence cannot qualify Sprint 6."""


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


def sha256_file(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def source_scopes() -> tuple[str, ...]:
    scopes = tuple(
        line
        for line in SOURCE_SCOPE_PATH.read_text(encoding="utf-8").splitlines()
        if line and not line.startswith("#")
    )
    require(scopes == tuple(sorted(set(scopes))), "Sprint 6 source scopes are not canonical")
    require("tests/codex/sprint6_source_scopes.txt" in scopes, "source manifest is not self-bound")
    return scopes


def tracked_source_paths() -> list[str]:
    scopes = source_scopes()
    result = subprocess.run(
        ["git", "--literal-pathspecs", "ls-files", "-z", "--", *scopes],
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
    )
    paths = sorted(item.decode() for item in result.stdout.split(b"\0") if item)
    require(bool(paths), "Sprint 6 source scope has no tracked files")
    return paths


def source_digest() -> tuple[str, int]:
    paths = tracked_source_paths()
    digest = hashlib.sha256()
    for relative in paths:
        digest.update(relative.encode())
        digest.update(b"\0")
        digest.update((REPOSITORY_ROOT / relative).read_bytes())
        digest.update(b"\0")
    return "sha256:" + digest.hexdigest(), len(paths)


def source_is_clean() -> bool:
    result = subprocess.run(
        [
            "git",
            "--literal-pathspecs",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--",
            *source_scopes(),
        ],
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
    )
    return not result.stdout


def command_output(command: list[str]) -> str:
    result = subprocess.run(
        command,
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    return (result.stdout or result.stderr).strip()


def run_gate(name: str, command: list[str], timeout: float = 900) -> dict[str, Any]:
    print(f"[Sprint 6] {name}...", flush=True)
    started = time.monotonic()
    result = subprocess.run(
        command,
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    elapsed = round((time.monotonic() - started) * 1000, 3)
    if result.returncode != 0:
        output = (result.stdout + result.stderr)[-12_000:]
        raise AcceptanceError(f"local gate failed: {name}\n{output}")
    print(f"[Sprint 6] {name}: passed ({elapsed} ms)", flush=True)
    return {"status": "passed", "duration_ms": elapsed}


def percentile(samples: list[float], value: int) -> float:
    require(bool(samples), "performance sample population is empty")
    index = max(0, min(len(samples) - 1, math.ceil(value * len(samples) / 100) - 1))
    return round(sorted(samples)[index], 3)


def validate_report(report: dict[str, Any], check_checkout: bool = True) -> dict[str, Any]:
    require(set(report) == TOP_LEVEL_FIELDS, "Sprint 6 evidence fields differ")
    require(report.get("schema_version") == 1 and report.get("sprint") == 6, "evidence version differs")
    require(report.get("profile") == "qualifying_local_macos", "evidence profile differs")
    require(report.get("platform") == "macos-arm64", "only local macOS arm64 qualifies")
    require(report.get("status") == "passed", "Sprint 6 evidence did not pass")

    source = report.get("source")
    require(isinstance(source, dict), "source coordinate is absent")
    require(
        set(source)
        == {
            "git_commit",
            "relevant_source_sha256",
            "relevant_source_clean",
            "tracked_file_count",
            "scope_manifest",
            "fixture_manifest_sha256",
            "golden_usages_sha256",
        },
        "source coordinate fields differ",
    )
    require(isinstance(source.get("git_commit"), str) and COMMIT_RE.fullmatch(source["git_commit"]), "git commit is invalid")
    require(source.get("relevant_source_clean") is True, "relevant source was dirty")
    require(source.get("scope_manifest") == "tests/codex/sprint6_source_scopes.txt", "scope manifest differs")
    for key in ("relevant_source_sha256", "fixture_manifest_sha256", "golden_usages_sha256"):
        require(isinstance(source.get(key), str) and SHA256_RE.fullmatch(source[key]), f"{key} is invalid")
    require(isinstance(source.get("tracked_file_count"), int) and source["tracked_file_count"] > 0, "tracked source count differs")
    if check_checkout:
        digest, count = source_digest()
        require(source_is_clean(), "current relevant source is dirty")
        require(source["relevant_source_sha256"] == digest and source["tracked_file_count"] == count, "source digest differs")
        require(source["fixture_manifest_sha256"] == sha256_file(MANIFEST), "fixture manifest digest differs")
        require(source["golden_usages_sha256"] == sha256_file(GOLDEN), "golden usages digest differs")

    versions = report.get("versions")
    require(
        isinstance(versions, dict)
        and set(versions)
        == {
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
        },
        "version coordinate fields differ",
    )
    require(versions["rust_target"] == "aarch64-apple-darwin", "Rust target differs")
    require(
        versions["bridge_rpc"] == "1.4"
        and versions["logical_schema"] == "1.3"
        and versions["physical_store"] == "segment-v3",
        "semantic protocol versions differ",
    )
    artifacts = report.get("artifacts")
    require(
        isinstance(artifacts, dict)
        and set(artifacts)
        == {"godot_sha256", "sidecar_sha256", "fixture_manifest_sha256", "golden_usages_sha256"},
        "artifact fields differ",
    )
    require(all(isinstance(value, str) and SHA256_RE.fullmatch(value) for value in artifacts.values()), "artifact hash differs")
    coordinates = report.get("coordinates")
    require(
        isinstance(coordinates, dict)
        and set(coordinates)
        == {
            "generation_id",
            "index_revision",
            "resource_revision",
            "scene_graph_revision",
            "script_graph_revision",
        }
        and isinstance(coordinates.get("generation_id"), str)
        and all(
            isinstance(coordinates.get(key), int) and not isinstance(coordinates[key], bool)
            for key in ("index_revision", "resource_revision", "scene_graph_revision", "script_graph_revision")
        ),
        "generation coordinates differ",
    )

    mcp = report.get("mcp_contract")
    require(isinstance(mcp, dict) and set(mcp) == MCP_FIELDS and all(mcp.values()), "MCP contract differs")
    accuracy = report.get("accuracy")
    require(isinstance(accuracy, dict), "accuracy evidence is absent")
    require(
        set(accuracy)
        == {
            "deduplication",
            "dynamic_false_exact",
            "queries",
            "resolvable_recall",
            "resolvable_truth_matched",
            "resolvable_truth_total",
            "zero_false_exact",
        },
        "accuracy fields differ",
    )
    require(
        accuracy.get("zero_false_exact") is True
        and accuracy.get("dynamic_false_exact") == 0
        and accuracy.get("resolvable_truth_total") == 7
        and accuracy.get("resolvable_truth_matched") == 7
        and accuracy.get("resolvable_recall") == 1.0,
        "frozen semantic truth differs",
    )
    queries = accuracy.get("queries")
    require(
        isinstance(queries, dict)
        and set(queries) == {"resource_profile", "scene_child", "signal_healed", "symbol_take_damage"},
        "oracle query coverage differs",
    )
    for query in queries.values():
        require(
            query.get("expected_pairs") == query.get("actual_pairs")
            and query.get("missing_pairs") == []
            and query.get("unexpected_pairs") == [],
            "oracle query truth differs",
        )
    dedup = accuracy.get("deduplication")
    require(
        isinstance(dedup, dict)
        and dedup.get("fact_count") == 1
        and dedup.get("evidence_count", 0) >= 2
        and len(set(dedup.get("evidence_sources", []))) >= 2,
        "evidence deduplication differs",
    )

    summaries = report.get("summaries")
    require(
        isinstance(summaries, dict)
        and set(summaries)
        == {"project_budget", "project_bytes", "scene_budget", "scene_bytes"}
        and summaries.get("project_budget") == 4096
        and summaries.get("scene_budget") == 2048
        and 0 < summaries.get("project_bytes", 0) <= 4096
        and 0 < summaries.get("scene_bytes", 0) <= 2048,
        "summary budgets differ",
    )
    performance = report.get("performance")
    require(
        isinstance(performance, dict)
        and set(performance)
        == {
            "find_usages_samples_ms",
            "find_usages_p95_ms",
            "summary_samples_ms",
            "summary_p95_ms",
            "control_ping_samples_ms",
            "control_ping_p95_ms",
        },
        "performance fields differ",
    )
    for prefix, budget in (("find_usages", 300), ("summary", 300), ("control_ping", 200)):
        samples = performance.get(f"{prefix}_samples_ms")
        reported = performance.get(f"{prefix}_p95_ms")
        require(
            isinstance(samples, list)
            and len(samples) >= 5
            and all(isinstance(sample, (int, float)) and not isinstance(sample, bool) and sample >= 0 for sample in samples),
            f"{prefix} samples differ",
        )
        require(reported == percentile([float(sample) for sample in samples], 95), f"{prefix} p95 differs")
        require(reported <= budget, f"{prefix} p95 exceeded its SLO")

    live_phases = report.get("live_phases")
    require(
        isinstance(live_phases, dict)
        and set(live_phases) == {"base", "resource_uid_rename", "symbol_rename"}
        and all(item.get("status") == "passed" for item in live_phases.values()),
        "live mutation phases differ",
    )
    require(
        set(live_phases["base"])
        == {"status", "index_revision", "resource_target_entity_id"}
        and set(live_phases["resource_uid_rename"])
        == {"status", "index_revision", "resource_target_entity_id", "identity_preserved"}
        and set(live_phases["symbol_rename"])
        == {"status", "index_revision", "symbol_target_entity_id", "old_selector_not_found"},
        "live phase fields differ",
    )
    require(
        live_phases["base"].get("resource_target_entity_id")
        == live_phases["resource_uid_rename"].get("resource_target_entity_id")
        and live_phases["resource_uid_rename"].get("identity_preserved") is True,
        "resource UID identity was not preserved",
    )
    require(
        isinstance(live_phases["base"].get("index_revision"), int)
        and live_phases["base"]["index_revision"]
        < live_phases["resource_uid_rename"]["index_revision"]
        < live_phases["symbol_rename"]["index_revision"]
        and live_phases["symbol_rename"].get("old_selector_not_found") is True,
        "renamed symbol remained current",
    )
    coverage = report.get("phase_coverage")
    require(isinstance(coverage, dict) and set(coverage) == set(PHASES), "phase coverage differs")
    require(
        all(
            set(item) == {"status", "mode"}
            and item.get("status") == "passed"
            and item.get("mode") in {"live_oracle", "deterministic_regression", "live_and_unit"}
            for item in coverage.values()
        ),
        "a phase did not pass",
    )
    gates = report.get("local_gates")
    require(isinstance(gates, dict) and set(gates) == LOCAL_GATES, "local gate set differs")
    require(
        all(
            set(item) == {"status", "duration_ms"}
            and item.get("status") == "passed"
            and isinstance(item.get("duration_ms"), (int, float))
            and item["duration_ms"] >= 0
            for item in gates.values()
        ),
        "a local gate did not pass",
    )
    cli = report.get("codex_cli_smoke")
    require(
        isinstance(cli, dict)
        and set(cli) == {"status", "blocking", "cli_available", "version", "reason"}
        and cli.get("blocking") is False
        and cli.get("status") in {"passed", "not_run"},
        "Codex CLI advisory coordinate differs",
    )
    cleanup = report.get("cleanup")
    require(
        isinstance(cleanup, dict)
        and set(cleanup)
        == {
            "editor_processes_stopped",
            "sidecar_processes_stopped",
            "temporary_workspace_removed",
            "bridge_runtime_files_absent",
        }
        and all(cleanup.values()),
        "cleanup evidence differs",
    )
    redaction = report.get("redaction")
    require(
        isinstance(redaction, dict)
        and set(redaction)
        == {
            "absolute_paths_absent",
            "bridge_endpoints_absent",
            "secret_material_absent",
            "source_bytes_absent",
        }
        and all(redaction.values()),
        "redaction evidence differs",
    )
    external = report.get("external_gates")
    require(isinstance(external, dict) and set(external) == {"windows", "linux", "remote_ci"}, "external gates differ")
    require(
        all(
            isinstance(item, dict)
            and set(item) == {"status", "reason"}
            and item.get("status") == "not_run"
            and isinstance(item.get("reason"), str)
            and item["reason"]
            for item in external.values()
        ),
        "an external gate was claimed",
    )
    serialized = json.dumps(report, ensure_ascii=False)
    require(not any(marker in serialized for marker in ("/Users/", "C:\\", "127.0.0.1", "session.token")), "evidence leaks private runtime material")
    return report


def rust_host() -> str:
    output = command_output(["rustc", "-vV"])
    return next(line.split(":", 1)[1].strip() for line in output.splitlines() if line.startswith("host:"))


def build_report(output: Path, timeout: float) -> dict[str, Any]:
    require(sys.platform == "darwin" and platform.machine().lower() in {"arm64", "aarch64"}, "Sprint 6 qualifying gate requires macOS arm64")
    require(source_is_clean(), "commit Sprint 6 source before generating qualifying evidence")
    gates: dict[str, dict[str, Any]] = {}
    gates["fixture_oracle"] = run_gate("fixture/oracle", [sys.executable, str(SCRIPT_DIR / "semantic_context_fixture.py")])
    gates["python_regressions"] = run_gate(
        "Python regressions",
        [sys.executable, "-m", "unittest", "tests/codex/test_semantic_context_fixture.py", "tests/codex/test_sprint6_acceptance.py"],
    )
    gates["rust_format"] = run_gate(
        "Rust format", ["cargo", "fmt", "--manifest-path", "godot-codex-mcp/Cargo.toml", "--all", "--", "--check"]
    )
    gates["rust_workspace_tests"] = run_gate(
        "Rust workspace tests",
        ["cargo", "test", "--manifest-path", "godot-codex-mcp/Cargo.toml", "--locked", "--offline", "--workspace", "--all-targets"],
    )
    gates["rust_clippy"] = run_gate(
        "Rust clippy",
        ["cargo", "clippy", "--manifest-path", "godot-codex-mcp/Cargo.toml", "--locked", "--offline", "--workspace", "--all-targets", "--", "-D", "warnings"],
    )
    gates["godot_editor_build"] = run_gate(
        "Godot editor build",
        [str(REPOSITORY_ROOT / ".venv" / "bin" / "scons"), "platform=macos", "arch=arm64", "target=editor", "dev_build=yes", "tests=yes", "module_codex_bridge_enabled=yes", "vulkan=no", "-j8"],
    )
    gates["cpp_semantic_adapter"] = run_gate(
        "C++ semantic adapter",
        [str(GODOT), "--headless", "--test", "--source-file=tests/codex/test_script_semantic_adapter.cpp", "--test-case=[CodexS6SemanticContext]*"],
    )
    gates["release_sidecar_build"] = run_gate(
        "release sidecar build",
        ["cargo", "build", "--manifest-path", "godot-codex-mcp/Cargo.toml", "--locked", "--offline", "--release", "-p", "godot-codex-mcp"],
    )
    with tempfile.TemporaryDirectory(prefix="s6-acceptance.", dir="/tmp") as directory:
        live_path = Path(directory) / "live.json"
        gates["model_free_live"] = run_gate(
            "model-free live smoke",
            [sys.executable, str(LIVE_RUNNER), "--godot", str(GODOT), "--sidecar", str(SIDECAR), "--timeout", str(timeout), "--output", str(live_path)],
            timeout=max(180, timeout * 3),
        )
        live = strict_json_load(live_path)

    require(source_is_clean(), "local gates changed relevant source")
    digest, tracked_count = source_digest()
    commit = command_output(["git", "rev-parse", "HEAD"])
    cli_path = shutil.which("codex")
    cli_version = command_output([cli_path, "--version"]) if cli_path else None
    phase_coverage = {
        "base": {"status": "passed", "mode": "live_oracle"},
        "resource_uid_rename": {"status": "passed", "mode": "live_oracle"},
        "symbol_rename": {"status": "passed", "mode": "live_oracle"},
        "duplicate_provenance": {"status": "passed", "mode": "live_oracle"},
        "partial_domains": {"status": "passed", "mode": "deterministic_regression"},
        "conflict": {"status": "passed", "mode": "deterministic_regression"},
        "budget_pressure": {"status": "passed", "mode": "live_and_unit"},
        "journal_gap": {"status": "passed", "mode": "deterministic_regression"},
    }
    report = {
        key: value
        for key, value in live.items()
        if key
        not in {"profile", "remote_ci", "windows", "linux", "status", "schema_version", "sprint", "platform", "versions"}
    }
    report.update(
        {
            "schema_version": 1,
            "sprint": 6,
            "profile": "qualifying_local_macos",
            "platform": "macos-arm64",
            "source": {
                "git_commit": commit,
                "relevant_source_sha256": digest,
                "relevant_source_clean": True,
                "tracked_file_count": tracked_count,
                "scope_manifest": "tests/codex/sprint6_source_scopes.txt",
                "fixture_manifest_sha256": sha256_file(MANIFEST),
                "golden_usages_sha256": sha256_file(GOLDEN),
            },
            "versions": {
                "godot": command_output([str(GODOT), "--version"]),
                "sidecar": command_output([str(SIDECAR), "--version"]),
                "python": platform.python_version(),
                "rustc": command_output(["rustc", "--version"]),
                "cargo": command_output(["cargo", "--version"]),
                "rust_target": rust_host(),
                **live["versions"],
            },
            "phase_coverage": phase_coverage,
            "local_gates": gates,
            "codex_cli_smoke": {
                "status": "not_run",
                "blocking": False,
                "cli_available": cli_path is not None,
                "version": cli_version,
                "reason": "isolated local MCP profile is not configured; advisory does not affect the deterministic gate",
            },
            "external_gates": {
                "windows": {"status": "not_run", "reason": "deferred Sprint 5 Windows gate does not block Sprint 6"},
                "linux": {"status": "not_run", "reason": "no local Linux system is in scope"},
                "remote_ci": {"status": "not_run", "reason": "the agreed acceptance path is local macOS"},
            },
            "status": "passed",
        }
    )
    validate_report(report, check_checkout=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=EVIDENCE_PATH)
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--validate", type=Path)
    arguments = parser.parse_args()
    try:
        if arguments.validate is not None:
            validate_report(strict_json_load(arguments.validate), check_checkout=True)
            print(f"Sprint 6 evidence is valid: {arguments.validate}")
        else:
            report = build_report(arguments.output.resolve(), arguments.timeout)
            print(json.dumps({"status": report["status"], "output": str(arguments.output)}, sort_keys=True))
        return 0
    except (AcceptanceError, OSError, subprocess.SubprocessError, StopIteration) as error:
        print(f"Sprint 6 acceptance failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
