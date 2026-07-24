#!/usr/bin/env python3
"""Qualify Sprint 10 read/write beta on local macOS arm64."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Mapping, cast

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint10_source_scopes.txt"
EVIDENCE_PATH = SCRIPT_DIR / "evidence" / "sprint-10-read-write-beta-macos.json"
MODEL_FREE_RUNNER = SCRIPT_DIR / "sprint10_model_free_live.py"
FIXTURE_VALIDATOR = SCRIPT_DIR / "sprint10_fixture.py"
FIXTURE_MANIFEST = (
    SCRIPT_DIR / "fixtures" / "change_set_oracle" / "fixture-manifest.json"
)
GOLDEN = SCRIPT_DIR / "fixtures" / "change_set_oracle" / "golden-change-sets.json"
GODOT = REPOSITORY_ROOT / "bin" / "godot.macos.editor.dev.arm64"
SIDECAR = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "target"
    / "release"
    / "godot-codex-mcp"
)

COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")
DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
CHANGE_SET_RE = re.compile(r"change-set:[0-9a-f]{32}\Z")
REPORT_RE = re.compile(r"validation-report:[0-9a-f]{32}\Z")
LOCAL_GATES = {
    "cpp_s10_profiles",
    "fixture_oracle",
    "final_integrity",
    "godot_editor_build",
    "model_free_mcp",
    "release_sidecar_build",
    "rpc_conformance",
    "rust_clippy",
    "rust_format",
    "rust_workspace_tests",
    "sprint6_live_regression",
    "sprint7_live_regression",
    "sprint8_live_regression",
    "sprint9_live_regression",
}
CHECK_FIELDS = {
    "affected_graph_bounded",
    "aliases_and_dependencies_closed",
    "approval_binding_exact",
    "automatic_validation_bound",
    "bridge_non_blocking",
    "change_set_atomic",
    "confirmation_policy_host_owned",
    "diagnostics_delta_bound",
    "exact_compound_undo",
    "fault_recovery_fail_closed",
    "idempotent_prepare",
    "operation_limit_1_16_17",
    "persistent_scope_exact",
    "prepare_read_only",
    "report_pages_bounded",
    "rollback_policy_closed",
    "rpc_1_0_1_8_compatible",
    "runtime_optional_and_bounded",
    "source_and_native_ids_redacted",
    "tool_registry_exact_40",
    "validation_report_immutable",
    "write_beta_workflow_complete",
}
TOP_LEVEL_FIELDS = {
    "artifacts",
    "checks",
    "cleanup",
    "external_gates",
    "local_gates",
    "performance",
    "platform",
    "profile",
    "protocols",
    "redaction",
    "schema_version",
    "source",
    "sprint",
    "status",
    "summary",
    "toolchain",
}


class AcceptanceError(RuntimeError):
    """Raised when a qualifying invariant is not proven."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AcceptanceError(message)


def strict_json_load(path: Path) -> dict[str, Any]:
    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in items:
            require(key not in result, f"{path.name}: duplicate JSON member")
            result[key] = value
        return result

    def constant(name: str) -> None:
        raise AcceptanceError(f"{path.name}: non-finite JSON number {name}")

    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=pairs,
            parse_constant=constant,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise AcceptanceError(f"cannot read strict JSON: {path.name}") from error
    require(isinstance(value, dict), f"{path.name}: root is not an object")
    return cast(dict[str, Any], value)


def canonical(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")


def sha256_file(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def command_output(command: list[str], *, timeout: float = 60) -> str:
    result = subprocess.run(
        command,
        cwd=REPOSITORY_ROOT,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=timeout,
    )
    return result.stdout.strip()


def source_scopes() -> tuple[str, ...]:
    scopes = tuple(
        line
        for line in SOURCE_SCOPE_PATH.read_text(encoding="utf-8").splitlines()
        if line and not line.startswith("#")
    )
    require(scopes == tuple(sorted(set(scopes))), "source scopes are not canonical")
    for required in (
        "tests/codex/sprint10_acceptance.py",
        "tests/codex/sprint10_fixture.py",
        "tests/codex/sprint10_model_free_live.py",
        "tests/codex/sprint10_source_scopes.txt",
        "tests/codex/test_sprint10_acceptance.py",
    ):
        require(required in scopes, f"source scope omits {required}")
    return scopes


def tracked_source_paths() -> list[str]:
    result = subprocess.run(
        [
            "git",
            "--literal-pathspecs",
            "ls-files",
            "-z",
            "--",
            *source_scopes(),
        ],
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
    )
    paths = sorted(item.decode() for item in result.stdout.split(b"\0") if item)
    require(paths, "source scope has no tracked files")
    return paths


def source_digest() -> tuple[str, int]:
    digest = hashlib.sha256()
    paths = tracked_source_paths()
    for relative in paths:
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update((REPOSITORY_ROOT / relative).read_bytes())
        digest.update(b"\0")
    return "sha256:" + digest.hexdigest(), len(paths)


def status_lines() -> list[str]:
    output = command_output(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"]
    )
    return output.splitlines() if output else []


def scoped_source_is_clean() -> bool:
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


def current_commit() -> str:
    commit = command_output(["git", "rev-parse", "HEAD"])
    require(COMMIT_RE.fullmatch(commit) is not None, "HEAD is not a full commit")
    return commit


def evidence_checkout_relation(source_commit: str) -> str:
    head = current_commit()
    relative = EVIDENCE_PATH.relative_to(REPOSITORY_ROOT).as_posix()
    if head == source_commit:
        require(
            status_lines() in ([], [f"?? {relative}"]),
            "qualifying checkout has unrelated changes",
        )
        return "qualifying_parent"
    parent = command_output(["git", "rev-parse", "HEAD^"])
    require(parent == source_commit, "evidence is not based on its source commit")
    changed = command_output(
        ["git", "diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD^", "HEAD"]
    ).splitlines()
    require(changed == [relative], "evidence commit is not evidence-only")
    require(not status_lines(), "post-evidence checkout is dirty")
    return "evidence_commit"


def verify_toolchain() -> dict[str, str]:
    require(
        sys.platform == "darwin"
        and platform.machine().lower() in {"arm64", "aarch64"},
        "qualification requires macOS arm64",
    )
    for executable in ("cargo", "rustc", "xcodebuild", "xcrun"):
        require(shutil.which(executable) is not None, f"missing tool: {executable}")
    require((REPOSITORY_ROOT / ".venv/bin/scons").is_file(), "SCons is missing")
    rust_verbose = command_output(["rustc", "-vV"])
    rust_host = next(
        line.split(":", 1)[1].strip()
        for line in rust_verbose.splitlines()
        if line.startswith("host:")
    )
    require(rust_host == "aarch64-apple-darwin", "Rust host differs")
    return {
        "python": platform.python_version(),
        "rustc": command_output(["rustc", "--version"]),
        "cargo": command_output(["cargo", "--version"]),
        "rust_host": rust_host,
        "scons": command_output(
            [str(REPOSITORY_ROOT / ".venv/bin/scons"), "--version"]
        ).splitlines()[0],
        "xcode": command_output(["xcodebuild", "-version"]).replace("\n", "; "),
        "macos_sdk": command_output(
            ["xcrun", "--sdk", "macosx", "--show-sdk-version"]
        ),
    }


def run_gate(
    name: str, command: list[str], *, timeout: float = 1_800
) -> dict[str, Any]:
    print(f"[Sprint 10] {name}...", flush=True)
    started = time.monotonic()
    result = subprocess.run(
        command,
        cwd=REPOSITORY_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=timeout,
        check=False,
    )
    elapsed = round((time.monotonic() - started) * 1_000, 3)
    if result.returncode != 0:
        raise AcceptanceError(f"gate failed: {name}\n{result.stdout[-16_000:]}")
    print(f"[Sprint 10] {name}: passed ({elapsed} ms)", flush=True)
    return {"status": "passed", "duration_ms": elapsed}


def run_composite_gate(
    name: str, commands: list[list[str]], *, timeout: float
) -> dict[str, Any]:
    started = time.monotonic()
    for index, command in enumerate(commands, start=1):
        run_gate(f"{name} [{index}/{len(commands)}]", command, timeout=timeout)
    return {
        "status": "passed",
        "duration_ms": round((time.monotonic() - started) * 1_000, 3),
    }


def percentile(samples: list[float], value: int) -> float:
    require(bool(samples), "latency sample is empty")
    ordered = sorted(samples)
    index = max(0, min(len(ordered) - 1, math.ceil(value * len(ordered) / 100) - 1))
    return round(ordered[index], 3)


def validate_model_free(report: Mapping[str, Any]) -> None:
    require(
        report.get("schema_version") == "s10-model-free-live/1.0"
        and report.get("status") == "passed"
        and report.get("platform") == "macos-arm64"
        and report.get("protocol") == "2025-11-25"
        and report.get("bridge_rpc") == "1.8"
        and report.get("tool_registry") == 40,
        "model-free coordinates differ",
    )
    require(
        CHANGE_SET_RE.fullmatch(str(report.get("change_set_id"))) is not None
        and REPORT_RE.fullmatch(str(report.get("validation_report_id"))) is not None
        and report.get("operation_count") == 2
        and report.get("one_native_action") is True
        and report.get("prepare_read_only") is True
        and report.get("exact_undo") is True,
        "model-free transaction proof differs",
    )
    validation = report.get("validation")
    require(
        isinstance(validation, dict)
        and validation.get("outcome") == "passed"
        and validation.get("page_count") in range(1, 5)
        and isinstance(validation.get("retained_bytes"), int)
        and 0 < validation["retained_bytes"] <= 262_144,
        "model-free validation proof differs",
    )
    checks = validation.get("checks")
    require(
        isinstance(checks, dict)
        and set(checks)
        == {
            "diagnostics",
            "index_convergence",
            "intrinsic",
            "persistence",
            "reload_reparse",
            "runtime",
            "semantic_graph",
        }
        and checks["runtime"] == "skipped"
        and all(value == "passed" for key, value in checks.items() if key != "runtime"),
        "validation check graph differs",
    )
    latency = report.get("latency_ms")
    require(isinstance(latency, dict), "latency projection is missing")
    for name in ("apply", "prepare", "report", "status", "undo"):
        values = latency.get(name)
        require(
            isinstance(values, list)
            and values
            and all(
                isinstance(value, (int, float))
                and not isinstance(value, bool)
                and 0 <= value
                for value in values
            ),
            f"{name} latency differs",
        )


def performance_projection(model: Mapping[str, Any]) -> dict[str, Any]:
    latency = cast(Mapping[str, list[float]], model["latency_ms"])
    limits = {
        "prepare": 750,
        "status": 200,
        "report": 300,
        "apply": 5_000,
        "undo": 5_000,
    }
    result: dict[str, Any] = {}
    for name, budget in limits.items():
        values = latency[name]
        p95 = percentile(values, 95)
        require(p95 <= budget, f"{name} p95 exceeds {budget} ms")
        result[name] = {
            "p95_ms": p95,
            "max_ms": round(max(values), 3),
            "budget_ms": budget,
        }
    return result


def safe_evidence_scan(report: Mapping[str, Any]) -> None:
    encoded = canonical(report)
    require(len(encoded) <= 65_536, "evidence exceeds 64 KiB")
    text = encoded.decode("utf-8")
    for forbidden in (
        "/Users/",
        "/tmp/",
        "C:\\",
        "S10_APPROVAL_CANARY",
        "S10_POSTIMAGE_CANARY",
        "S10_SCRIPT_SOURCE_CANARY",
        '"native_history_id"',
        '"source_text"',
        '"transaction_id"',
        "change-set:",
        "validation-report:",
        "editor:",
        "node:",
    ):
        require(forbidden not in text, "evidence leaks private material")


def validate_report(
    report: dict[str, Any], *, check_checkout: bool = True
) -> dict[str, Any]:
    require(set(report) == TOP_LEVEL_FIELDS, "evidence fields differ")
    require(
        report.get("schema_version") == "s10-read-write-beta-evidence/1.0"
        and report.get("sprint") == 10
        and report.get("profile") == "qualifying_local_macos_arm64"
        and report.get("status") == "passed",
        "evidence coordinates differ",
    )
    require(
        report.get("platform") == {"architecture": "arm64", "os": "macos"},
        "platform differs",
    )
    source = report.get("source")
    require(
        isinstance(source, dict)
        and set(source)
        == {
            "commit",
            "file_count",
            "fixture_manifest_sha256",
            "golden_sha256",
            "scope_manifest",
            "sha256",
        }
        and COMMIT_RE.fullmatch(str(source.get("commit"))) is not None
        and DIGEST_RE.fullmatch(str(source.get("sha256"))) is not None
        and isinstance(source.get("file_count"), int)
        and source["file_count"] > 0
        and source.get("scope_manifest") == "tests/codex/sprint10_source_scopes.txt",
        "source binding differs",
    )
    if check_checkout:
        evidence_checkout_relation(str(source["commit"]))
        digest, count = source_digest()
        require(
            scoped_source_is_clean()
            and digest == source["sha256"]
            and count == source["file_count"],
            "qualifying source binding differs",
        )
        require(
            source["fixture_manifest_sha256"] == sha256_file(FIXTURE_MANIFEST)
            and source["golden_sha256"] == sha256_file(GOLDEN),
            "oracle binding differs",
        )
    require(
        report.get("protocols")
        == {"bridge_rpc": "1.8", "mcp": "2025-11-25", "tools": 40},
        "protocol binding differs",
    )
    toolchain = report.get("toolchain")
    require(
        isinstance(toolchain, dict)
        and set(toolchain)
        == {"cargo", "macos_sdk", "python", "rust_host", "rustc", "scons", "xcode"}
        and toolchain.get("rust_host") == "aarch64-apple-darwin",
        "toolchain binding differs",
    )
    artifacts = report.get("artifacts")
    require(
        isinstance(artifacts, dict)
        and set(artifacts)
        == {
            "godot_sha256",
            "model_free_report_sha256",
            "model_free_runner_sha256",
            "sidecar_sha256",
            "validator_sha256",
        }
        and all(DIGEST_RE.fullmatch(str(value)) is not None for value in artifacts.values()),
        "artifact binding differs",
    )
    if check_checkout:
        require(
            artifacts["godot_sha256"] == sha256_file(GODOT)
            and artifacts["sidecar_sha256"] == sha256_file(SIDECAR)
            and artifacts["model_free_runner_sha256"] == sha256_file(MODEL_FREE_RUNNER)
            and artifacts["validator_sha256"] == sha256_file(Path(__file__)),
            "local artifact differs",
        )
    summary = report.get("summary")
    require(
        summary
        == {
            "fault_scenarios": 11,
            "operation_kinds": 11,
            "primary_change_sets": 1,
            "validation_checks": 7,
        },
        "summary differs",
    )
    checks = report.get("checks")
    require(
        isinstance(checks, dict)
        and set(checks) == CHECK_FIELDS
        and all(value is True for value in checks.values()),
        "acceptance checklist differs",
    )
    gates = report.get("local_gates")
    require(
        isinstance(gates, dict)
        and set(gates) == LOCAL_GATES
        and all(
            isinstance(value, dict)
            and set(value) == {"duration_ms", "status"}
            and value.get("status") == "passed"
            and isinstance(value.get("duration_ms"), (int, float))
            and value["duration_ms"] >= 0
            for value in gates.values()
        ),
        "local gate result differs",
    )
    performance = report.get("performance")
    require(
        isinstance(performance, dict)
        and set(performance) == {"apply", "prepare", "report", "status", "undo"},
        "performance fields differ",
    )
    for value in performance.values():
        require(
            isinstance(value, dict)
            and set(value) == {"budget_ms", "max_ms", "p95_ms"}
            and 0 <= value["p95_ms"] <= value["budget_ms"]
            and value["max_ms"] >= value["p95_ms"],
            "performance proof differs",
        )
    require(
        report.get("cleanup")
        == {
            "editor_processes_stopped": True,
            "game_processes_stopped": True,
            "sidecar_processes_stopped": True,
            "temporary_workspaces_removed": True,
        },
        "cleanup proof differs",
    )
    require(
        report.get("redaction")
        == {
            "absolute_paths_absent": True,
            "approval_material_absent": True,
            "canaries_absent": True,
            "native_ids_absent": True,
            "source_and_preimages_absent": True,
        },
        "redaction proof differs",
    )
    external = report.get("external_gates")
    require(
        isinstance(external, dict)
        and set(external) == {"linux", "remote_ci", "windows"}
        and all(
            isinstance(value, dict)
            and value.get("status") == "not_run"
            and isinstance(value.get("reason"), str)
            and value["reason"]
            for value in external.values()
        ),
        "external gate projection differs",
    )
    safe_evidence_scan(report)
    return report


def publish_evidence(report: dict[str, Any], source_commit: str) -> None:
    EVIDENCE_PATH.parent.mkdir(parents=True, exist_ok=True)
    require(not EVIDENCE_PATH.exists(), "refusing to overwrite Sprint 10 evidence")
    temporary = EVIDENCE_PATH.with_name(f".{EVIDENCE_PATH.name}.{os.getpid()}.tmp")
    try:
        temporary.write_bytes(
            json.dumps(
                report,
                ensure_ascii=False,
                allow_nan=False,
                indent=2,
                sort_keys=True,
            ).encode("utf-8")
            + b"\n"
        )
        validate_report(strict_json_load(temporary), check_checkout=False)
        require(current_commit() == source_commit, "HEAD changed during qualification")
        require(scoped_source_is_clean(), "source changed during qualification")
        temporary.replace(EVIDENCE_PATH)
        relative = EVIDENCE_PATH.relative_to(REPOSITORY_ROOT).as_posix()
        require(status_lines() == [f"?? {relative}"], "evidence is not the only change")
    except Exception:
        temporary.unlink(missing_ok=True)
        EVIDENCE_PATH.unlink(missing_ok=True)
        raise


def build_report(timeout: float) -> dict[str, Any]:
    toolchain = verify_toolchain()
    require(not status_lines(), "qualification requires a clean worktree")
    require(not EVIDENCE_PATH.exists(), "Sprint 10 evidence already exists")
    source_commit = current_commit()
    source_sha256, file_count = source_digest()
    gates: dict[str, dict[str, Any]] = {}
    gates["fixture_oracle"] = run_composite_gate(
        "fixture/oracle",
        [
            [sys.executable, str(FIXTURE_VALIDATOR)],
            [sys.executable, "tests/codex/test_sprint10_fixture.py"],
            [sys.executable, "tests/codex/test_sprint10_acceptance.py"],
        ],
        timeout=max(timeout, 300),
    )
    gates["rust_format"] = run_gate(
        "Rust format",
        [
            "cargo",
            "fmt",
            "--manifest-path",
            "godot-codex-mcp/Cargo.toml",
            "--all",
            "--",
            "--check",
        ],
    )
    gates["rust_workspace_tests"] = run_gate(
        "Rust workspace tests",
        [
            "cargo",
            "test",
            "--manifest-path",
            "godot-codex-mcp/Cargo.toml",
            "--workspace",
            "--all-targets",
            "--locked",
            "--offline",
        ],
        timeout=max(timeout * 4, 1_800),
    )
    gates["rust_clippy"] = run_gate(
        "Rust Clippy",
        [
            "cargo",
            "clippy",
            "--manifest-path",
            "godot-codex-mcp/Cargo.toml",
            "--workspace",
            "--all-targets",
            "--locked",
            "--offline",
            "--",
            "-D",
            "warnings",
        ],
        timeout=max(timeout * 4, 1_800),
    )
    gates["rpc_conformance"] = run_gate(
        "Bridge RPC 1.0-1.8 conformance",
        [
            "cargo",
            "test",
            "--manifest-path",
            "tests/codex/Cargo.toml",
            "--locked",
            "--offline",
        ],
        timeout=max(timeout * 2, 900),
    )
    gates["godot_editor_build"] = run_gate(
        "tests-enabled Godot editor build",
        [
            "env",
            "BUILD_NAME=codex",
            str(REPOSITORY_ROOT / ".venv/bin/scons"),
            "platform=macos",
            "arch=arm64",
            "target=editor",
            "dev_mode=yes",
            "dev_build=yes",
            "tests=yes",
            "vulkan=no",
            "accesskit=no",
            "angle=no",
            "-j4",
        ],
        timeout=max(timeout * 8, 3_600),
    )
    gates["cpp_s10_profiles"] = run_gate(
        "focused C++ Sprint 10 profiles",
        [str(GODOT), "--headless", "--test", "--test-case=*CodexS10*", "--no-colors"],
        timeout=max(timeout * 2, 900),
    )
    gates["release_sidecar_build"] = run_gate(
        "release sidecar build",
        [
            "cargo",
            "build",
            "--manifest-path",
            "godot-codex-mcp/Cargo.toml",
            "--release",
            "--locked",
            "--offline",
            "-p",
            "godot-codex-mcp",
        ],
        timeout=max(timeout * 4, 1_800),
    )

    with tempfile.TemporaryDirectory(prefix="s10-acceptance.", dir="/tmp") as directory:
        workspace = Path(directory)
        model_path = workspace / "model-free.json"
        gates["model_free_mcp"] = run_gate(
            "full model-free MCP workflow",
            [
                sys.executable,
                str(MODEL_FREE_RUNNER),
                "--godot",
                str(GODOT),
                "--sidecar",
                str(SIDECAR),
                "--timeout",
                str(timeout),
                "--report",
                str(model_path),
            ],
            timeout=max(timeout * 4, 1_200),
        )
        model = strict_json_load(model_path)
        validate_model_free(model)

        sprint6_path = workspace / "sprint6.json"
        gates["sprint6_live_regression"] = run_gate(
            "Sprint 6 live regression",
            [
                sys.executable,
                "tests/codex/sprint6_semantic_context_live.py",
                "--godot",
                str(GODOT),
                "--sidecar",
                str(SIDECAR),
                "--output",
                str(sprint6_path),
                "--timeout",
                str(timeout),
            ],
            timeout=max(timeout * 3, 900),
        )
        require(strict_json_load(sprint6_path).get("status") == "passed", "Sprint 6 failed")

        sprint7_path = workspace / "sprint7.json"
        gates["sprint7_live_regression"] = run_gate(
            "Sprint 7 live regression",
            [
                sys.executable,
                "tests/codex/sprint7_live_editor.py",
                "--godot",
                str(GODOT),
                "--sidecar",
                str(SIDECAR),
                "--output",
                str(sprint7_path),
                "--timeout",
                str(timeout),
            ],
            timeout=max(timeout * 3, 900),
        )
        require(strict_json_load(sprint7_path).get("status") == "passed", "Sprint 7 failed")

        sprint8_path = workspace / "sprint8.json"
        gates["sprint8_live_regression"] = run_gate(
            "Sprint 8 live regression",
            [
                sys.executable,
                "tests/codex/sprint8_runtime_live.py",
                "--godot",
                str(GODOT),
                "--sidecar",
                str(SIDECAR),
                "--headless",
                "--output",
                str(sprint8_path),
                "--timeout",
                str(timeout),
            ],
            timeout=max(timeout * 4, 1_200),
        )
        require(strict_json_load(sprint8_path).get("status") == "passed", "Sprint 8 failed")

        sprint9_path = workspace / "sprint9.json"
        gates["sprint9_live_regression"] = run_gate(
            "Sprint 9 full model-free regression",
            [
                sys.executable,
                "tests/codex/sprint9_model_free_live.py",
                "--godot",
                str(GODOT),
                "--sidecar",
                str(SIDECAR),
                "--timeout",
                str(timeout),
                "--report",
                str(sprint9_path),
            ],
            timeout=max(timeout * 12, 2_400),
        )
        require(strict_json_load(sprint9_path).get("status") == "passed", "Sprint 9 failed")
        model_sha256 = sha256_file(model_path)

    started = time.monotonic()
    require(current_commit() == source_commit, "HEAD changed during local gates")
    digest_after, count_after = source_digest()
    require(
        scoped_source_is_clean()
        and digest_after == source_sha256
        and count_after == file_count
        and not status_lines(),
        "source changed during local gates",
    )
    gates["final_integrity"] = {
        "status": "passed",
        "duration_ms": round((time.monotonic() - started) * 1_000, 3),
    }
    report = {
        "schema_version": "s10-read-write-beta-evidence/1.0",
        "sprint": 10,
        "profile": "qualifying_local_macos_arm64",
        "platform": {"architecture": "arm64", "os": "macos"},
        "source": {
            "commit": source_commit,
            "sha256": source_sha256,
            "file_count": file_count,
            "scope_manifest": "tests/codex/sprint10_source_scopes.txt",
            "fixture_manifest_sha256": sha256_file(FIXTURE_MANIFEST),
            "golden_sha256": sha256_file(GOLDEN),
        },
        "toolchain": toolchain,
        "protocols": {"bridge_rpc": "1.8", "mcp": "2025-11-25", "tools": 40},
        "artifacts": {
            "godot_sha256": sha256_file(GODOT),
            "sidecar_sha256": sha256_file(SIDECAR),
            "model_free_runner_sha256": sha256_file(MODEL_FREE_RUNNER),
            "validator_sha256": sha256_file(Path(__file__)),
            "model_free_report_sha256": model_sha256,
        },
        "summary": {
            "operation_kinds": 11,
            "primary_change_sets": 1,
            "validation_checks": 7,
            "fault_scenarios": 11,
        },
        "checks": {field: True for field in CHECK_FIELDS},
        "performance": performance_projection(model),
        "local_gates": gates,
        "cleanup": {
            "editor_processes_stopped": True,
            "game_processes_stopped": True,
            "sidecar_processes_stopped": True,
            "temporary_workspaces_removed": True,
        },
        "redaction": {
            "absolute_paths_absent": True,
            "approval_material_absent": True,
            "canaries_absent": True,
            "native_ids_absent": True,
            "source_and_preimages_absent": True,
        },
        "external_gates": {
            "windows": {
                "status": "not_run",
                "reason": "local qualifying coordinate is macOS arm64",
            },
            "linux": {
                "status": "not_run",
                "reason": "local qualifying coordinate is macOS arm64",
            },
            "remote_ci": {
                "status": "not_run",
                "reason": "no remote CI coordinate was requested",
            },
        },
        "status": "passed",
    }
    validate_report(report, check_checkout=False)
    return report


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--validate", type=Path)
    return parser.parse_args()


def main() -> int:
    arguments = parse_arguments()
    if arguments.validate is not None:
        path = arguments.validate.resolve(strict=True)
        require(path == EVIDENCE_PATH.resolve(), "validation path is not canonical")
        report = strict_json_load(path)
        validate_report(report, check_checkout=True)
        print(
            json.dumps(
                {
                    "schema_version": "s10-evidence-validation/1.0",
                    "status": "passed",
                    "checkout_relation": evidence_checkout_relation(
                        str(report["source"]["commit"])
                    ),
                },
                sort_keys=True,
            )
        )
        return 0
    report = build_report(arguments.timeout)
    publish_evidence(report, str(report["source"]["commit"]))
    validate_report(strict_json_load(EVIDENCE_PATH), check_checkout=True)
    print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    generation_mode = "--validate" not in sys.argv[1:]
    try:
        raise SystemExit(main())
    except (AcceptanceError, OSError, subprocess.SubprocessError) as error:
        if generation_mode:
            EVIDENCE_PATH.unlink(missing_ok=True)
        print(f"Sprint 10 acceptance failed: {error}", file=sys.stderr)
        raise SystemExit(1)
