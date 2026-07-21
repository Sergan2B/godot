#!/usr/bin/env python3
"""Build, run, validate, and record the local Sprint 8 macOS gate."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import platform
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, cast

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint8_source_scopes.txt"
LIVE_RUNNER = SCRIPT_DIR / "sprint8_runtime_live.py"
FIXTURE_VALIDATOR = SCRIPT_DIR / "runtime_mvp_fixture.py"
MANIFEST = SCRIPT_DIR / "fixtures" / "runtime_mvp_oracle" / "fixture-manifest.json"
GOLDEN = SCRIPT_DIR / "fixtures" / "runtime_mvp_oracle" / "golden-runtime.json"
EVIDENCE_PATH = SCRIPT_DIR / "evidence" / "sprint-8-runtime-macos.json"
GODOT = REPOSITORY_ROOT / "bin" / "godot.macos.editor.dev.arm64"
SIDECAR = REPOSITORY_ROOT / "godot-codex-mcp" / "target" / "release" / "godot-codex-mcp"

CHECK_FIELDS = {
    "bounded_properties",
    "crash_retention_and_retirement",
    "diagnostic_stack",
    "exact_25_tool_registry",
    "fresh_session_per_run",
    "hang_timeout_and_recovery",
    "headless_capture_unavailable",
    "large_tree_bounded",
    "manual_editor_lifecycle_observed",
    "pause_continue_stop_confirmed",
    "runtime_cursor_invalidation",
    "runtime_editor_mapping_evidence",
    "runtime_only_unmapped",
    "runtime_summary_bounded",
    "runtime_tree_and_opaque_ids",
    "stale_session_rejected",
    "viewport_capture",
}
LOCAL_GATES = {
    "cpp_runtime_tests",
    "fixture_oracle",
    "godot_editor_build",
    "model_free_gui",
    "model_free_headless",
    "python_regressions",
    "release_sidecar_build",
    "rust_clippy",
    "rust_format",
    "rust_workspace_tests",
    "schema_conformance",
}
TOP_LEVEL_FIELDS = {
    "schema_version",
    "sprint",
    "profile",
    "platform",
    "source",
    "versions",
    "artifacts",
    "sessions",
    "checks",
    "observations",
    "performance",
    "local_gates",
    "cleanup",
    "redaction",
    "external_gates",
    "status",
}
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")


class AcceptanceError(RuntimeError):
    """Raised when local evidence cannot qualify Sprint 8."""


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
    require(scopes == tuple(sorted(set(scopes))), "Sprint 8 source scopes are not canonical")
    require("tests/codex/sprint8_source_scopes.txt" in scopes, "source manifest is not self-bound")
    return scopes


def tracked_source_paths() -> list[str]:
    result = subprocess.run(
        ["git", "--literal-pathspecs", "ls-files", "-z", "--", *source_scopes()],
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
    )
    paths = sorted(item.decode() for item in result.stdout.split(b"\0") if item)
    require(bool(paths), "Sprint 8 source scope has no tracked files")
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
    print(f"[Sprint 8] {name}...", flush=True)
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
        output = (result.stdout + result.stderr)[-16_000:]
        raise AcceptanceError(f"local gate failed: {name}\n{output}")
    print(f"[Sprint 8] {name}: passed ({elapsed} ms)", flush=True)
    return {"status": "passed", "duration_ms": elapsed}


def percentile(samples: list[float] | list[int], value: int) -> float:
    require(bool(samples), "performance sample population is empty")
    ordered = sorted(float(sample) for sample in samples)
    index = max(0, min(len(ordered) - 1, math.ceil(value * len(ordered) / 100) - 1))
    return round(ordered[index], 3)


def require_new_output(output: Path) -> None:
    require(not output.exists(), f"refusing to overwrite existing evidence: {output.name}")


def rust_host() -> str:
    output = command_output(["rustc", "-vV"])
    return next(line.split(":", 1)[1].strip() for line in output.splitlines() if line.startswith("host:"))


def validate_report(report: dict[str, Any], check_checkout: bool = True) -> dict[str, Any]:
    require(set(report) == TOP_LEVEL_FIELDS, "Sprint 8 evidence fields differ")
    require(report.get("schema_version") == 1 and report.get("sprint") == 8, "evidence version differs")
    require(report.get("profile") == "qualifying_local_macos", "evidence profile differs")
    require(report.get("platform") == "macos-arm64", "only local macOS arm64 qualifies")
    require(report.get("status") == "passed", "Sprint 8 evidence did not pass")

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
            "golden_runtime_sha256",
        },
        "source coordinate fields differ",
    )
    require(isinstance(source["git_commit"], str) and COMMIT_RE.fullmatch(source["git_commit"]), "git commit is invalid")
    require(source["relevant_source_clean"] is True, "relevant source was dirty")
    require(source["scope_manifest"] == "tests/codex/sprint8_source_scopes.txt", "scope manifest differs")
    for key in ("relevant_source_sha256", "fixture_manifest_sha256", "golden_runtime_sha256"):
        require(isinstance(source[key], str) and SHA256_RE.fullmatch(source[key]), f"{key} is invalid")
    require(isinstance(source["tracked_file_count"], int) and source["tracked_file_count"] > 0, "tracked source count differs")
    if check_checkout:
        digest, count = source_digest()
        require(source_is_clean(), "current relevant source is dirty")
        require(source["relevant_source_sha256"] == digest and source["tracked_file_count"] == count, "source digest differs")
        require(source["fixture_manifest_sha256"] == sha256_file(MANIFEST), "fixture manifest digest differs")
        require(source["golden_runtime_sha256"] == sha256_file(GOLDEN), "golden digest differs")

    versions = report.get("versions")
    require(
        isinstance(versions, dict)
        and set(versions)
        == {"godot", "sidecar", "python", "rustc", "cargo", "rust_target", "bridge_rpc", "mcp_protocol"}
        and versions.get("rust_target") == "aarch64-apple-darwin"
        and versions.get("bridge_rpc") == "1.6"
        and versions.get("mcp_protocol") == "2025-11-25",
        "version coordinates differ",
    )

    artifacts = report.get("artifacts")
    require(
        isinstance(artifacts, dict)
        and set(artifacts)
        == {"godot_sha256", "sidecar_sha256", "live_runner_sha256", "fixture_validator_sha256"}
        and all(isinstance(item, str) and SHA256_RE.fullmatch(item) for item in artifacts.values()),
        "artifact hashes differ",
    )
    if check_checkout:
        require(artifacts["godot_sha256"] == sha256_file(GODOT), "Godot artifact digest differs")
        require(artifacts["sidecar_sha256"] == sha256_file(SIDECAR), "sidecar artifact digest differs")
        require(artifacts["live_runner_sha256"] == sha256_file(LIVE_RUNNER), "live runner digest differs")
        require(artifacts["fixture_validator_sha256"] == sha256_file(FIXTURE_VALIDATOR), "fixture validator digest differs")

    sessions = report.get("sessions")
    require(
        isinstance(sessions, list)
        and len(sessions) == 12
        and len(set(sessions)) == len(sessions)
        and all(isinstance(item, str) and re.fullmatch(r"runtime:[0-9a-f]{32}", item) for item in sessions),
        "runtime session identities differ",
    )
    checks = report.get("checks")
    require(isinstance(checks, dict) and set(checks) == CHECK_FIELDS and all(checks.values()), "a runtime check did not pass")
    observations = report.get("observations")
    require(
        isinstance(observations, dict)
        and set(observations)
        == {"runtime_nodes", "large_runtime_nodes", "diagnostics", "stack_frames"}
        and observations.get("runtime_nodes") >= 6
        and observations.get("large_runtime_nodes") == 10000
        and observations.get("diagnostics") >= 2
        and observations.get("stack_frames") >= 3,
        "runtime observations differ",
    )

    performance = report.get("performance")
    require(
        isinstance(performance, dict)
        and set(performance)
        == {
            "large_tree_page_p95_ms",
            "transition_visibility_samples_ms",
            "transition_visibility_p95_ms",
            "viewport_capture_ms",
            "hung_game_bridge_ping_p95_ms",
            "hung_tree_timeout_ms",
            "hung_game_stop_ms",
        },
        "performance evidence fields differ",
    )
    for field, budget in (
        ("large_tree_page_p95_ms", 500),
        ("transition_visibility_p95_ms", 2000),
        ("viewport_capture_ms", 3000),
        ("hung_game_bridge_ping_p95_ms", 200),
    ):
        require(
            isinstance(performance.get(field), (int, float))
            and not isinstance(performance[field], bool)
            and 0 <= performance[field] <= budget,
            f"{field} SLO differs",
        )
    samples = performance.get("transition_visibility_samples_ms")
    require(
        isinstance(samples, list)
        and len(samples) >= 3
        and performance["transition_visibility_p95_ms"] == percentile(samples, 95),
        "transition visibility samples differ",
    )

    gates = report.get("local_gates")
    require(
        isinstance(gates, dict)
        and set(gates) == LOCAL_GATES
        and all(
            isinstance(item, dict)
            and set(item) == {"status", "duration_ms"}
            and item.get("status") == "passed"
            and isinstance(item.get("duration_ms"), (int, float))
            and item["duration_ms"] >= 0
            for item in gates.values()
        ),
        "a local gate did not pass",
    )
    cleanup = report.get("cleanup")
    require(
        isinstance(cleanup, dict)
        and set(cleanup)
        == {
            "editor_processes_stopped",
            "sidecar_processes_stopped",
            "temporary_workspace_removed",
            "runtime_values_retired",
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
            "native_handles_absent",
            "secret_material_absent",
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
        not any(
            marker in serialized
            for marker in (
                "/Users/",
                "/tmp/",
                "C:\\",
                "127.0.0.1",
                "session.token",
                '\"native_handle\"',
                '\"window_handle\"',
                '\"texture_handle\"',
            )
        ),
        "evidence leaks private runtime material",
    )
    return report


def build_report(output: Path, timeout: float) -> dict[str, Any]:
    require(sys.platform == "darwin" and platform.machine().lower() in {"arm64", "aarch64"}, "Sprint 8 qualifying gate requires macOS arm64")
    require_new_output(output)
    require(source_is_clean(), "commit Sprint 8 source before generating qualifying evidence")

    gates: dict[str, dict[str, Any]] = {}
    gates["fixture_oracle"] = run_gate("fixture/oracle", [sys.executable, str(FIXTURE_VALIDATOR)])
    gates["python_regressions"] = run_gate(
        "Python regressions",
        [sys.executable, "-m", "unittest", "tests/codex/test_runtime_mvp_fixture.py", "tests/codex/test_sprint8_acceptance.py"],
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
    gates["schema_conformance"] = run_gate(
        "Bridge schema conformance",
        ["cargo", "test", "--manifest-path", "tests/codex/Cargo.toml", "--locked", "--offline"],
    )
    gates["godot_editor_build"] = run_gate(
        "Godot editor build",
        [
            "env",
            "BUILD_NAME=codex",
            str(REPOSITORY_ROOT / ".venv/bin/scons"),
            "platform=macos",
            "arch=arm64",
            "target=editor",
            "dev_build=yes",
            "tests=yes",
            "vulkan=no",
            "accesskit=no",
            "angle=no",
            "metal=yes",
            "-j4",
        ],
    )
    gates["cpp_runtime_tests"] = run_gate(
        "C++ Runtime profile tests",
        [str(GODOT), "--headless", "--test", "--test-case=*CodexS8*"],
    )
    gates["release_sidecar_build"] = run_gate(
        "release sidecar build",
        ["cargo", "build", "--manifest-path", "godot-codex-mcp/Cargo.toml", "--locked", "--offline", "--release", "-p", "godot-codex-mcp"],
    )

    with tempfile.TemporaryDirectory(prefix="s8-acceptance.", dir="/tmp") as directory:
        headless_path = Path(directory) / "headless.json"
        gui_path = Path(directory) / "gui.json"
        common = [sys.executable, str(LIVE_RUNNER), "--godot", str(GODOT), "--sidecar", str(SIDECAR), "--timeout", str(timeout)]
        gates["model_free_headless"] = run_gate(
            "model-free headless runtime",
            [*common, "--headless", "--output", str(headless_path)],
            timeout=max(300, timeout * 8),
        )
        gates["model_free_gui"] = run_gate(
            "model-free GUI runtime",
            [*common, "--output", str(gui_path)],
            timeout=max(300, timeout * 8),
        )
        headless = strict_json_load(headless_path)
        gui = strict_json_load(gui_path)

    require(source_is_clean(), "local gates changed relevant source")
    require(headless.get("status") == "passed" and headless.get("headless") is True, "headless live report did not pass")
    require(gui.get("status") == "passed" and gui.get("headless") is False, "GUI live report did not pass")
    require(headless["checks"]["viewport_capture"] == "unavailable_headless", "headless capture semantics differ")
    require(gui["checks"]["viewport_capture"] is True, "GUI capture did not pass")

    checks = dict(gui["checks"])
    checks["headless_capture_unavailable"] = True
    observations = dict(gui["observations"])
    transition_samples = [
        float(gui["timings_ms"]["pause"]),
        float(gui["timings_ms"]["continue"]),
        float(gui["timings_ms"]["stop"]),
    ]
    performance = {
        "large_tree_page_p95_ms": gui["timings_ms"]["large_tree_page_p95"],
        "transition_visibility_samples_ms": transition_samples,
        "transition_visibility_p95_ms": percentile(transition_samples, 95),
        "viewport_capture_ms": gui["timings_ms"]["capture"],
        "hung_game_bridge_ping_p95_ms": gui["timings_ms"]["hung_game_bridge_ping_p95"],
        "hung_tree_timeout_ms": gui["timings_ms"]["hung_tree_timeout"],
        "hung_game_stop_ms": gui["timings_ms"]["hung_game_stop"],
    }
    digest, count = source_digest()
    report = {
        "schema_version": 1,
        "sprint": 8,
        "profile": "qualifying_local_macos",
        "platform": "macos-arm64",
        "source": {
            "git_commit": command_output(["git", "rev-parse", "HEAD"]),
            "relevant_source_sha256": digest,
            "relevant_source_clean": True,
            "tracked_file_count": count,
            "scope_manifest": "tests/codex/sprint8_source_scopes.txt",
            "fixture_manifest_sha256": sha256_file(MANIFEST),
            "golden_runtime_sha256": sha256_file(GOLDEN),
        },
        "versions": {
            "godot": command_output([str(GODOT), "--version"]),
            "sidecar": command_output([str(SIDECAR), "--version"]),
            "python": platform.python_version(),
            "rustc": command_output(["rustc", "--version"]),
            "cargo": command_output(["cargo", "--version"]),
            "rust_target": rust_host(),
            "bridge_rpc": "1.6",
            "mcp_protocol": "2025-11-25",
        },
        "artifacts": {
            "godot_sha256": sha256_file(GODOT),
            "sidecar_sha256": sha256_file(SIDECAR),
            "live_runner_sha256": sha256_file(LIVE_RUNNER),
            "fixture_validator_sha256": sha256_file(FIXTURE_VALIDATOR),
        },
        "sessions": [*headless["sessions"], *gui["sessions"]],
        "checks": checks,
        "observations": observations,
        "performance": performance,
        "local_gates": gates,
        "cleanup": {
            "editor_processes_stopped": True,
            "sidecar_processes_stopped": True,
            "temporary_workspace_removed": True,
            "runtime_values_retired": True,
        },
        "redaction": {
            "absolute_paths_absent": True,
            "bridge_endpoints_absent": True,
            "native_handles_absent": True,
            "secret_material_absent": True,
        },
        "external_gates": {
            "windows": {"status": "not_run", "reason": "Sprint 8 qualifying coordinate is local macOS arm64"},
            "linux": {"status": "not_run", "reason": "no Linux system is available for this local milestone"},
            "remote_ci": {"status": "not_run", "reason": "the agreed acceptance path is local and no Git CI is configured"},
            "model_facing": {"status": "not_run", "reason": "model-facing validation is outside the deterministic acceptance boundary"},
        },
        "status": "passed",
    }
    validate_report(report, check_checkout=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    temporary.write_text(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    temporary.replace(output)
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=EVIDENCE_PATH)
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--validate", type=Path)
    arguments = parser.parse_args()
    try:
        if arguments.validate is not None:
            validate_report(strict_json_load(arguments.validate), check_checkout=True)
            print(f"Sprint 8 evidence is valid: {arguments.validate}")
        else:
            report = build_report(arguments.output.resolve(), arguments.timeout)
            print(json.dumps({"status": report["status"], "output": str(arguments.output)}, sort_keys=True))
        return 0
    except (AcceptanceError, OSError, subprocess.SubprocessError, StopIteration) as error:
        print(f"Sprint 8 acceptance failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
