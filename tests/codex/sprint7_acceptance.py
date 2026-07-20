#!/usr/bin/env python3
"""Build, run, validate, and record the local Sprint 7 macOS gate."""

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
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint7_source_scopes.txt"
LIVE_RUNNER = SCRIPT_DIR / "sprint7_live_editor.py"
EVIDENCE_PATH = SCRIPT_DIR / "evidence" / "sprint-7-live-editor-macos.json"
GODOT = REPOSITORY_ROOT / "bin" / "godot.macos.editor.dev.arm64"
SIDECAR = REPOSITORY_ROOT / "godot-codex-mcp" / "target" / "release" / "godot-codex-mcp"
MANIFEST = SCRIPT_DIR / "fixtures" / "live_editor_oracle" / "fixture-manifest.json"
GOLDEN = SCRIPT_DIR / "fixtures" / "live_editor_oracle" / "golden-live-editor.json"

PHASES = (
    "baseline",
    "commit",
    "undo",
    "redo",
    "opaque",
    "partial_capture",
    "close",
    "reimport",
    "event_gap",
    "reconnect",
    "session_reset",
)
LOCAL_GATES = {
    "fixture_oracle",
    "python_regressions",
    "rust_format",
    "rust_workspace_tests",
    "rust_clippy",
    "godot_editor_build",
    "cpp_bridge_tests",
    "release_sidecar_build",
    "model_free_live",
}
MCP_FIELDS = {
    "closed_input_schemas",
    "exact_sixteen_tool_registry",
    "read_only_annotations",
    "subscriptions_disabled",
}
CHECK_FIELDS = {
    "changed_snapshot_cursor_rejected",
    "closed_scene_retired",
    "diagnostics_redacted_and_bounded",
    "dirty_editor_value_precedes_disk",
    "dirty_state_explicit",
    "disk_value_retained_for_comparison",
    "editor_session_reset_invalidates_ids",
    "editor_summary_bounded",
    "multi_selection_identity_stable",
    "native_payload_remains_opaque",
    "old_session_guard_rejected",
    "script_source_absent",
    "sidecar_reconnect_full_snapshot",
    "stale_editor_revision_rejected",
    "stale_scene_revision_rejected",
    "unaffected_scene_revision_stable",
    "undo_redo_advance_history_and_revision",
    "viewport_metadata_only",
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
    "checks",
    "observations",
    "performance",
    "transitions",
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
    """Raised when local evidence cannot qualify Sprint 7."""


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
    require(scopes == tuple(sorted(set(scopes))), "Sprint 7 source scopes are not canonical")
    require("tests/codex/sprint7_source_scopes.txt" in scopes, "source manifest is not self-bound")
    return scopes


def tracked_source_paths() -> list[str]:
    result = subprocess.run(
        ["git", "--literal-pathspecs", "ls-files", "-z", "--", *source_scopes()],
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
    )
    paths = sorted(item.decode() for item in result.stdout.split(b"\0") if item)
    require(bool(paths), "Sprint 7 source scope has no tracked files")
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
    print(f"[Sprint 7] {name}...", flush=True)
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
    print(f"[Sprint 7] {name}: passed ({elapsed} ms)", flush=True)
    return {"status": "passed", "duration_ms": elapsed}


def percentile(samples: list[float] | list[int], value: int) -> float:
    require(bool(samples), "performance sample population is empty")
    ordered = sorted(float(sample) for sample in samples)
    index = max(0, min(len(ordered) - 1, math.ceil(value * len(ordered) / 100) - 1))
    return round(ordered[index], 3)


def validate_report(report: dict[str, Any], check_checkout: bool = True) -> dict[str, Any]:
    require(set(report) == TOP_LEVEL_FIELDS, "Sprint 7 evidence fields differ")
    require(report.get("schema_version") == 1 and report.get("sprint") == 7, "evidence version differs")
    require(report.get("profile") == "qualifying_local_macos", "evidence profile differs")
    require(report.get("platform") == "macos-arm64", "only local macOS arm64 qualifies")
    require(report.get("status") == "passed", "Sprint 7 evidence did not pass")

    source = report.get("source")
    require(
        isinstance(source, dict)
        and set(source)
        == {
            "git_commit",
            "relevant_source_sha256",
            "relevant_source_clean",
            "tracked_file_count",
            "scope_manifest",
            "fixture_manifest_sha256",
            "golden_live_editor_sha256",
        },
        "source coordinate fields differ",
    )
    require(isinstance(source.get("git_commit"), str) and COMMIT_RE.fullmatch(source["git_commit"]), "git commit is invalid")
    require(source.get("relevant_source_clean") is True, "relevant source was dirty")
    require(source.get("scope_manifest") == "tests/codex/sprint7_source_scopes.txt", "scope manifest differs")
    for key in ("relevant_source_sha256", "fixture_manifest_sha256", "golden_live_editor_sha256"):
        require(isinstance(source.get(key), str) and SHA256_RE.fullmatch(source[key]), f"{key} is invalid")
    require(isinstance(source.get("tracked_file_count"), int) and source["tracked_file_count"] > 0, "tracked source count differs")
    if check_checkout:
        digest, count = source_digest()
        require(source_is_clean(), "current relevant source is dirty")
        require(source["relevant_source_sha256"] == digest and source["tracked_file_count"] == count, "source digest differs")
        require(source["fixture_manifest_sha256"] == sha256_file(MANIFEST), "fixture manifest digest differs")
        require(source["golden_live_editor_sha256"] == sha256_file(GOLDEN), "golden digest differs")

    versions = report.get("versions")
    require(
        isinstance(versions, dict)
        and set(versions)
        == {"godot", "sidecar", "python", "rustc", "cargo", "rust_target", "bridge_rpc", "mcp_protocol"},
        "version coordinate fields differ",
    )
    require(versions.get("rust_target") == "aarch64-apple-darwin", "Rust target differs")
    require(versions.get("bridge_rpc") == "1.5" and versions.get("mcp_protocol") == "2025-11-25", "protocol versions differ")

    artifacts = report.get("artifacts")
    require(
        isinstance(artifacts, dict)
        and set(artifacts)
        == {"godot_sha256", "sidecar_sha256", "fixture_manifest_sha256", "golden_live_editor_sha256", "live_runner_sha256"}
        and all(isinstance(value, str) and SHA256_RE.fullmatch(value) for value in artifacts.values()),
        "artifact hashes differ",
    )
    if check_checkout:
        require(artifacts["godot_sha256"] == sha256_file(GODOT), "Godot artifact digest differs")
        require(artifacts["sidecar_sha256"] == sha256_file(SIDECAR), "sidecar artifact digest differs")
        require(artifacts["live_runner_sha256"] == sha256_file(LIVE_RUNNER), "live runner digest differs")

    coordinates = report.get("coordinates")
    require(isinstance(coordinates, dict), "revision coordinates are absent")
    require(
        set(coordinates)
        == {
            "editor_session_id",
            "initial_snapshot_id",
            "initial_event_seq",
            "final_event_seq",
            "initial_operation_seq",
            "final_operation_seq",
            "dirty_scene_revision_initial",
            "dirty_scene_revision_final",
        },
        "revision coordinate fields differ",
    )
    require(
        isinstance(coordinates.get("editor_session_id"), str)
        and coordinates["editor_session_id"].startswith("editor:")
        and isinstance(coordinates.get("initial_snapshot_id"), str)
        and coordinates["initial_snapshot_id"].startswith("snapshot:"),
        "live identities differ",
    )
    numeric_coordinates = [
        "initial_event_seq",
        "final_event_seq",
        "initial_operation_seq",
        "final_operation_seq",
        "dirty_scene_revision_initial",
        "dirty_scene_revision_final",
    ]
    require(
        all(isinstance(coordinates.get(key), int) and not isinstance(coordinates[key], bool) for key in numeric_coordinates)
        and coordinates["final_event_seq"] > coordinates["initial_event_seq"]
        and coordinates["final_operation_seq"] > coordinates["initial_operation_seq"]
        and coordinates["dirty_scene_revision_final"] > coordinates["dirty_scene_revision_initial"],
        "revision coordinates did not advance",
    )

    mcp = report.get("mcp_contract")
    require(isinstance(mcp, dict) and set(mcp) == MCP_FIELDS and all(mcp.values()), "MCP contract differs")
    checks = report.get("checks")
    require(isinstance(checks, dict) and set(checks) == CHECK_FIELDS and all(checks.values()), "a correctness check did not pass")
    observations = report.get("observations")
    require(
        isinstance(observations, dict)
        and set(observations)
        == {
            "open_scene_count",
            "selected_node_count",
            "open_script_count",
            "diagnostic_count",
            "redacted_diagnostic_count",
            "editor_value",
            "disk_value",
            "editor_summary_bytes",
            "viewport_kind",
        }
        and observations.get("open_scene_count") >= 2
        and observations.get("selected_node_count") == 3
        and observations.get("open_script_count") >= 2
        and observations.get("redacted_diagnostic_count", 0) >= 1
        and observations.get("editor_value") == 21.5
        and observations.get("disk_value") == 8.0
        and 0 < observations.get("editor_summary_bytes", 0) <= 4096
        and observations.get("viewport_kind") in {"2d", "3d"},
        "live editor observations differ",
    )

    transitions = report.get("transitions")
    require(isinstance(transitions, dict) and set(transitions) == {"commit", "undo", "redo", "opaque"}, "history transitions differ")
    require(
        all(
            set(item)
            == {
                "operation_seq",
                "dirty_scene_revision",
                "clean_scene_revision",
                "history_transition",
                "history_operation_kind",
            }
            for item in transitions.values()
        ),
        "history transition fields differ",
    )
    sequence = [transitions[key].get("operation_seq") for key in ("commit", "undo", "redo", "opaque")]
    require(all(isinstance(value, int) and not isinstance(value, bool) for value in sequence) and sequence == sorted(set(sequence)), "operation sequence is not strictly monotonic")
    require(transitions["undo"].get("history_transition") == "undo" and transitions["redo"].get("history_transition") == "redo", "Undo/Redo history summaries differ")
    require(all(item.get("history_operation_kind") == "opaque" for item in transitions.values()), "native payload was inferred")

    performance = report.get("performance")
    performance_fields = {
        "selection_samples_ms",
        "selection_p95_ms",
        "change_visibility_samples_ms",
        "change_visibility_p95_ms",
        "control_ping_samples_ms",
        "control_ping_p95_ms",
        "editor_summary_samples_ms",
        "editor_summary_p95_ms",
        "dispatcher_samples_usec",
        "dispatcher_p95_usec",
        "dispatcher_max_usec",
        "dispatcher_budget_usec",
        "dispatcher_over_budget_count",
    }
    require(isinstance(performance, dict) and set(performance) == performance_fields, "performance fields differ")
    for prefix, budget, minimum in (
        ("selection", 500, 20),
        ("change_visibility", 2000, 20),
        ("control_ping", 200, 20),
        ("editor_summary", 500, 20),
    ):
        samples = performance.get(f"{prefix}_samples_ms")
        reported = performance.get(f"{prefix}_p95_ms")
        require(
            isinstance(samples, list)
            and len(samples) >= minimum
            and all(isinstance(sample, (int, float)) and not isinstance(sample, bool) and sample >= 0 for sample in samples)
            and reported == percentile(samples, 95)
            and reported <= budget,
            f"{prefix} SLO differs",
        )
    dispatcher_samples = performance.get("dispatcher_samples_usec")
    require(
        isinstance(dispatcher_samples, list)
        and dispatcher_samples
        and all(isinstance(sample, int) and not isinstance(sample, bool) and sample >= 0 for sample in dispatcher_samples)
        and performance.get("dispatcher_p95_usec") == percentile(dispatcher_samples, 95)
        and performance.get("dispatcher_max_usec") == max(dispatcher_samples)
        and performance.get("dispatcher_budget_usec") == 2000
        and performance.get("dispatcher_over_budget_count") == 0
        and performance["dispatcher_max_usec"] <= 2000,
        "dispatcher budget differs",
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
    require(
        isinstance(gates, dict)
        and set(gates) == LOCAL_GATES
        and all(
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
        and cli.get("status") == "not_run"
        and cli.get("blocking") is False,
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
    require(isinstance(external, dict) and set(external) == {"windows", "linux", "remote_ci", "model_facing"}, "external gates differ")
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
    require(
        not any(marker in serialized for marker in ("/Users/", "/tmp/", "C:\\\\", "127.0.0.1", "session.token", "fixture-only-secret")),
        "evidence leaks private runtime material",
    )
    return report


def rust_host() -> str:
    output = command_output(["rustc", "-vV"])
    return next(line.split(":", 1)[1].strip() for line in output.splitlines() if line.startswith("host:"))


def require_new_output(output: Path) -> None:
    require(not output.exists(), f"refusing to overwrite existing evidence: {output.name}")


def build_report(output: Path, timeout: float) -> dict[str, Any]:
    require(sys.platform == "darwin" and platform.machine().lower() in {"arm64", "aarch64"}, "Sprint 7 qualifying gate requires macOS arm64")
    require_new_output(output)
    require(source_is_clean(), "commit Sprint 7 source before generating qualifying evidence")
    gates: dict[str, dict[str, Any]] = {}
    gates["fixture_oracle"] = run_gate("fixture/oracle", [sys.executable, str(SCRIPT_DIR / "live_editor_fixture.py")])
    gates["python_regressions"] = run_gate(
        "Python regressions",
        [sys.executable, "-m", "unittest", "tests/codex/test_live_editor_fixture.py", "tests/codex/test_sprint7_acceptance.py"],
    )
    gates["rust_format"] = run_gate(
        "Rust format",
        ["cargo", "fmt", "--manifest-path", "godot-codex-mcp/Cargo.toml", "--all", "--", "--check"],
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
        [str(REPOSITORY_ROOT / ".venv" / "bin" / "scons"), "platform=macos", "arch=arm64", "target=editor", "dev_build=yes", "tests=yes", "module_codex_bridge_enabled=yes", "accesskit=no", "angle=no", "vulkan=no", "-j8"],
    )
    gates["cpp_bridge_tests"] = run_gate(
        "C++ Bridge tests",
        [str(GODOT), "--headless", "--test", "--source-file=tests/codex/test_codex_bridge.cpp"],
    )
    gates["release_sidecar_build"] = run_gate(
        "release sidecar build",
        ["cargo", "build", "--manifest-path", "godot-codex-mcp/Cargo.toml", "--locked", "--offline", "--release", "-p", "godot-codex-mcp"],
    )
    with tempfile.TemporaryDirectory(prefix="s7-acceptance.", dir="/tmp") as directory:
        live_path = Path(directory) / "live.json"
        gates["model_free_live"] = run_gate(
            "model-free live editor",
            [sys.executable, str(LIVE_RUNNER), "--godot", str(GODOT), "--sidecar", str(SIDECAR), "--timeout", str(timeout), "--output", str(live_path)],
            timeout=max(300, timeout * 5),
        )
        live = strict_json_load(live_path)

    require(source_is_clean(), "local gates changed relevant source")
    require(live.get("schema_version") == 1 and live.get("sprint") == 7 and live.get("status") == "passed", "live report did not pass")
    digest, tracked_count = source_digest()
    cli_path = shutil.which("codex")
    cli_version = command_output([cli_path, "--version"]) if cli_path else None
    live_phases = {"baseline", "commit", "undo", "redo", "opaque", "close", "reconnect", "session_reset"}
    phase_coverage = {
        phase: {
            "status": "passed",
            "mode": "live_oracle" if phase in live_phases else "deterministic_regression",
        }
        for phase in PHASES
    }
    artifacts = dict(live["artifacts"])
    artifacts["live_runner_sha256"] = sha256_file(LIVE_RUNNER)
    report = {
        "schema_version": 1,
        "sprint": 7,
        "profile": "qualifying_local_macos",
        "platform": "macos-arm64",
        "source": {
            "git_commit": command_output(["git", "rev-parse", "HEAD"]),
            "relevant_source_sha256": digest,
            "relevant_source_clean": True,
            "tracked_file_count": tracked_count,
            "scope_manifest": "tests/codex/sprint7_source_scopes.txt",
            "fixture_manifest_sha256": sha256_file(MANIFEST),
            "golden_live_editor_sha256": sha256_file(GOLDEN),
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
        "artifacts": artifacts,
        "coordinates": live["coordinates"],
        "mcp_contract": live["mcp_contract"],
        "checks": live["checks"],
        "observations": live["observations"],
        "performance": live["performance"],
        "transitions": live["transitions"],
        "phase_coverage": phase_coverage,
        "local_gates": gates,
        "codex_cli_smoke": {
            "status": "not_run",
            "blocking": False,
            "cli_available": cli_path is not None,
            "version": cli_version,
            "reason": "no isolated model-facing profile was configured; deterministic local acceptance is authoritative",
        },
        "cleanup": live["cleanup"],
        "redaction": {
            "absolute_paths_absent": True,
            "bridge_endpoints_absent": True,
            "secret_material_absent": True,
            "source_bytes_absent": True,
        },
        "external_gates": {
            "windows": {"status": "not_run", "reason": "Sprint 7 qualifying coordinate is local macOS arm64"},
            "linux": {"status": "not_run", "reason": "no Linux system is available or required for this local milestone"},
            "remote_ci": {"status": "not_run", "reason": "the agreed acceptance path is local and no Git CI is configured"},
            "model_facing": {"status": "not_run", "reason": "advisory model smoke is outside the deterministic acceptance boundary"},
        },
        "status": "passed",
    }
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
            print(f"Sprint 7 evidence is valid: {arguments.validate}")
        else:
            report = build_report(arguments.output.resolve(), arguments.timeout)
            print(json.dumps({"status": report["status"], "output": str(arguments.output)}, sort_keys=True))
        return 0
    except (AcceptanceError, OSError, subprocess.SubprocessError, StopIteration) as error:
        print(f"Sprint 7 acceptance failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
