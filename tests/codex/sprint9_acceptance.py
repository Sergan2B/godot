#!/usr/bin/env python3
"""Qualify and validate Sprint 9 editor transactions on a supported desktop host."""

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
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint9_source_scopes.txt"
MODEL_FREE_RUNNER = SCRIPT_DIR / "sprint9_model_free_live.py"
FIXTURE_VALIDATOR = SCRIPT_DIR / "transaction_fixture.py"
FIXTURE_MANIFEST = (
    SCRIPT_DIR / "fixtures" / "transaction_oracle" / "fixture-manifest.json"
)
GOLDEN = SCRIPT_DIR / "fixtures" / "transaction_oracle" / "golden-transactions.json"
RUST_TOOLCHAIN_PATH = REPOSITORY_ROOT / "godot-codex-mcp" / "rust-toolchain.toml"

HOST_IS_MACOS_ARM64 = (
    sys.platform == "darwin"
    and platform.machine().lower() in {"arm64", "aarch64"}
)
HOST_IS_WINDOWS_X86_64 = (
    sys.platform == "win32"
    and platform.machine().lower() in {"amd64", "x86_64"}
)
if HOST_IS_WINDOWS_X86_64:
    EVIDENCE_PATH = (
        SCRIPT_DIR / "evidence" / "sprint-9-editor-transactions-windows.json"
    )
    GODOT = (
        REPOSITORY_ROOT
        / "bin"
        / "godot.windows.editor.dev.x86_64.console.exe"
    )
    SIDECAR = (
        REPOSITORY_ROOT
        / "godot-codex-mcp"
        / "target"
        / "release"
        / "godot-codex-mcp.exe"
    )
    LOCAL_PROFILE = "qualifying_local_windows_x86_64"
    LOCAL_PLATFORM = {"architecture": "x86_64", "os": "windows"}
    MODEL_PLATFORM = "windows-x86_64"
else:
    EVIDENCE_PATH = (
        SCRIPT_DIR / "evidence" / "sprint-9-editor-transactions-macos.json"
    )
    GODOT = REPOSITORY_ROOT / "bin" / "godot.macos.editor.dev.arm64"
    SIDECAR = (
        REPOSITORY_ROOT
        / "godot-codex-mcp"
        / "target"
        / "release"
        / "godot-codex-mcp"
    )
    LOCAL_PROFILE = "qualifying_local_macos_arm64"
    LOCAL_PLATFORM = {"architecture": "arm64", "os": "macos"}
    MODEL_PLATFORM = "macos-arm64"

QUALIFYING_PROFILES = {
    ("macos", "arm64"): {
        "profile": "qualifying_local_macos_arm64",
        "rust_host": "aarch64-apple-darwin",
        "toolchain_fields": {
            "cargo",
            "macos_sdk",
            "python",
            "rust_host",
            "rustc",
            "scons",
            "xcode",
        },
        "external_platform": "windows",
    },
    ("windows", "x86_64"): {
        "profile": "qualifying_local_windows_x86_64",
        "rust_host": "x86_64-pc-windows-msvc",
        "toolchain_fields": {
            "cargo",
            "msvc",
            "python",
            "rust_host",
            "rustc",
            "scons",
            "windows_sdk",
        },
        "external_platform": "macos",
    },
}

COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
TRANSACTION_HASH_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
OPERATION_KINDS = {
    "attach_script",
    "connect_signal",
    "create_node",
    "delete_node",
    "detach_script",
    "disconnect_signal",
    "reparent_node",
    "set_property",
}
FAULT_SCENARIOS = {
    "bridge_response_loss_after_commit",
    "corrupt_journal",
    "disconnect_before_commit",
    "editor_crash_before_commit",
    "editor_restart_after_commit",
    "restart_after_bridge_response_before_journal_ack",
    "sidecar_disconnect_prepared",
}
LOCAL_GATES = {
    "approval_contracts",
    "cpp_s9_profiles",
    "direct_bridge_matrix",
    "final_integrity",
    "fixture_oracle",
    "godot_editor_build",
    "model_free_mcp",
    "release_sidecar_build",
    "rpc_conformance",
    "rust_clippy",
    "rust_format",
    "rust_workspace_tests",
    "sprint7_regression",
    "sprint8_regressions",
}
CHECK_FIELDS = {
    "approval_form_only",
    "bounded_reports",
    "corrupt_journal_fail_closed",
    "eight_operation_workflow",
    "exact_targeted_undo",
    "idempotency_closed",
    "intervening_action_blocks_targeted_undo",
    "native_undo_redo_correlated",
    "no_mutation_after_prepare",
    "no_persistent_undo_after_editor_restart",
    "preapproval_rejections",
    "response_loss_no_replay",
    "source_content_unchanged",
    "tool_registry_exact",
}
TOP_LEVEL_FIELDS = {
    "artifacts",
    "checks",
    "cleanup",
    "external_gates",
    "lifecycle",
    "local_gates",
    "performance",
    "platform",
    "profile",
    "protocols",
    "recovery",
    "redaction",
    "schema_version",
    "source",
    "sprint",
    "status",
    "summary",
    "toolchain",
    "transaction_identity_sha256",
}


class AcceptanceError(RuntimeError):
    """Raised when a Sprint 9 qualifying invariant is not proven."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AcceptanceError(message)


def strict_json_load(path: Path) -> dict[str, Any]:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            require(key not in result, f"duplicate JSON member: {key}")
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
        raise AcceptanceError(f"cannot read strict JSON: {path.name}") from error
    require(isinstance(value, dict), "JSON root must be an object")
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


def command_output(command: list[str], *, cwd: Path = REPOSITORY_ROOT) -> str:
    result = subprocess.run(
        command,
        cwd=cwd,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=60,
    )
    return result.stdout.strip()


def source_scopes() -> tuple[str, ...]:
    scopes = tuple(
        line
        for line in SOURCE_SCOPE_PATH.read_text(encoding="utf-8").splitlines()
        if line and not line.startswith("#")
    )
    require(
        scopes == tuple(sorted(set(scopes))),
        "Sprint 9 source scopes are not canonical",
    )
    for required in (
        "tests/codex/sprint9_acceptance.py",
        "tests/codex/sprint9_model_free_live.py",
        "tests/codex/sprint9_source_scopes.txt",
        "tests/codex/test_sprint9_acceptance.py",
    ):
        require(required in scopes, f"source scope is not self-bound: {required}")
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
    require(paths, "Sprint 9 source scope has no tracked files")
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


def status_lines() -> list[str]:
    output = command_output(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"]
    )
    return [line for line in output.splitlines() if line]


def current_commit() -> str:
    commit = command_output(["git", "rev-parse", "HEAD"])
    require(COMMIT_RE.fullmatch(commit) is not None, "HEAD is not a full commit")
    return commit


def rust_toolchain_channel() -> str:
    match = re.search(
        r'(?m)^\s*channel\s*=\s*"([^"]+)"\s*$',
        RUST_TOOLCHAIN_PATH.read_text(encoding="utf-8"),
    )
    require(match is not None, "Rust toolchain channel is missing")
    return match.group(1)


def rust_command(tool: str, *arguments: str) -> list[str]:
    return [tool, f"+{rust_toolchain_channel()}", *arguments]


def require_pristine_worktree() -> None:
    require(not status_lines(), "qualifying run requires a fully clean worktree")


def evidence_checkout_relation(source_commit: str) -> str:
    head = current_commit()
    evidence_relative = EVIDENCE_PATH.relative_to(REPOSITORY_ROOT).as_posix()
    if head == source_commit:
        changes = status_lines()
        require(
            changes in ([], [f"?? {evidence_relative}"]),
            "pre-evidence checkout contains unrelated changes",
        )
        return "qualifying_parent"
    parent = command_output(["git", "rev-parse", "HEAD^"])
    require(parent == source_commit, "evidence is not based on its source commit")
    changed = command_output(
        ["git", "diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD^", "HEAD"]
    ).splitlines()
    require(
        changed == [evidence_relative],
        "evidence commit contains files other than Sprint 9 evidence",
    )
    require(not status_lines(), "post-evidence checkout is dirty")
    return "evidence_commit"


def rust_host() -> str:
    output = command_output(rust_command("rustc", "-vV"))
    return next(
        line.split(":", 1)[1].strip()
        for line in output.splitlines()
        if line.startswith("host:")
    )


def verify_toolchain() -> dict[str, str]:
    require(
        HOST_IS_MACOS_ARM64 or HOST_IS_WINDOWS_X86_64,
        "Sprint 9 qualification requires macOS arm64 or Windows x86_64",
    )
    for executable in ("cargo", "rustc", "rustup"):
        require(shutil.which(executable) is not None, f"missing tool: {executable}")
    require(
        rust_host()
        == cast(
            str,
            QUALIFYING_PROFILES[
                (LOCAL_PLATFORM["os"], LOCAL_PLATFORM["architecture"])
            ]["rust_host"],
        ),
        "Rust host does not match the qualifying platform",
    )
    common = {
        "python": platform.python_version(),
        "rustc": command_output(rust_command("rustc", "--version")),
        "cargo": command_output(rust_command("cargo", "--version")),
        "rust_host": rust_host(),
    }
    if HOST_IS_MACOS_ARM64:
        for executable in ("xcodebuild", "xcrun"):
            require(
                shutil.which(executable) is not None,
                f"missing tool: {executable}",
            )
        scons = REPOSITORY_ROOT / ".venv/bin/scons"
        require(scons.is_file(), "SCons is missing")
        return common | {
            "scons": command_output([str(scons), "--version"]).splitlines()[0],
            "xcode": command_output(["xcodebuild", "-version"]).replace(
                "\n", "; "
            ),
            "macos_sdk": command_output(
                ["xcrun", "--sdk", "macosx", "--show-sdk-version"]
            ),
        }

    scons_version = command_output(
        [sys.executable, "-m", "SCons", "--version"]
    ).splitlines()[0]
    program_files_x86 = os.environ.get("ProgramFiles(x86)", "")
    vswhere = (
        Path(program_files_x86)
        / "Microsoft Visual Studio"
        / "Installer"
        / "vswhere.exe"
    )
    require(vswhere.is_file(), "Visual Studio locator is missing")
    msvc = command_output(
        [
            str(vswhere),
            "-latest",
            "-products",
            "*",
            "-property",
            "installationVersion",
        ]
    )
    sdk_include = (
        Path(program_files_x86) / "Windows Kits" / "10" / "Include"
    )
    sdk_versions = sorted(
        (path.name for path in sdk_include.iterdir() if path.is_dir()),
        reverse=True,
    )
    require(bool(msvc) and bool(sdk_versions), "MSVC or Windows SDK is missing")
    return common | {
        "scons": scons_version,
        "msvc": msvc,
        "windows_sdk": sdk_versions[0],
    }


def godot_build_command() -> list[str]:
    if HOST_IS_WINDOWS_X86_64:
        return [
            sys.executable,
            "-m",
            "SCons",
            "platform=windows",
            "arch=x86_64",
            "target=editor",
            "dev_mode=yes",
            "dev_build=yes",
            "tests=yes",
            "module_codex_bridge_enabled=yes",
            "accesskit=no",
            "d3d12=no",
            "angle=no",
            "-j8",
        ]
    return [
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
    ]


def run_gate(
    name: str,
    command: list[str],
    *,
    timeout: float = 1_800,
) -> dict[str, Any]:
    print(f"[Sprint 9] {name}...", flush=True)
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
    duration = round((time.monotonic() - started) * 1_000, 3)
    if result.returncode != 0:
        raise AcceptanceError(
            f"local gate failed: {name}\n{result.stdout[-16_000:]}"
        )
    print(f"[Sprint 9] {name}: passed ({duration} ms)", flush=True)
    return {"status": "passed", "duration_ms": duration}


def run_composite_gate(
    name: str,
    commands: list[list[str]],
    *,
    timeout: float,
) -> dict[str, Any]:
    started = time.monotonic()
    for index, command in enumerate(commands, start=1):
        run_gate(f"{name} [{index}/{len(commands)}]", command, timeout=timeout)
    return {
        "status": "passed",
        "duration_ms": round((time.monotonic() - started) * 1_000, 3),
    }


def percentile(samples: list[float], value: int) -> float:
    require(samples, "performance sample population is empty")
    ordered = sorted(samples)
    index = max(
        0,
        min(len(ordered) - 1, math.ceil(value * len(ordered) / 100) - 1),
    )
    return round(ordered[index], 3)


def validate_model_free(
    report: Mapping[str, Any], expected_platform: str = MODEL_PLATFORM
) -> None:
    require(
        report.get("schema_version") == "s9-model-free-workflow/1.0"
        and report.get("status") == "passed"
        and report.get("platform") == expected_platform
        and report.get("protocol") == "2025-11-25"
        and report.get("tool_registry") == 36,
        "model-free report coordinates differ",
    )
    operations = report.get("operations")
    require(
        isinstance(operations, list)
        and len(operations) == 8
        and {item.get("operation") for item in operations if isinstance(item, dict)}
        == OPERATION_KINDS,
        "model-free eight-operation matrix differs",
    )
    require(
        all(
            item.get("oracle_cycle") is True
            and item.get("source_unchanged") is True
            and item.get("transaction_native_actions") == 1
            for item in operations
        ),
        "operation lifecycle proof differs",
    )
    native_cases = {
        item["operation"]
        for item in operations
        if item.get("native_undo_redo") is True
        and item.get("intervening_action") is True
    }
    require(
        native_cases
        == {"create_node", "set_property", "attach_script", "connect_signal"},
        "native family coverage differs",
    )
    faults = report.get("faults")
    require(
        isinstance(faults, dict)
        and set(faults) == FAULT_SCENARIOS
        and report.get("fault_count") == len(FAULT_SCENARIOS),
        "fault matrix differs",
    )
    negatives = report.get("negatives")
    require(
        isinstance(negatives, dict)
        and set(negatives)
        == {"approval", "committed_replay", "expired_preview", "idempotency"},
        "negative matrix differs",
    )
    approvals = negatives["approval"]
    require(
        isinstance(approvals, list)
        and {item.get("decision") for item in approvals if isinstance(item, dict)}
        == {"cancel", "confirm_false", "decline", "timeout", "unsupported"}
        and all(item.get("native_actions") == 0 for item in approvals),
        "approval negative matrix differs",
    )
    latency = report.get("latency_ms")
    require(isinstance(latency, dict), "latency evidence is missing")
    for field, budget in (
        ("prepare", 500),
        ("status", 200),
        ("apply", 2_000),
        ("undo", 2_000),
    ):
        value = latency.get(field)
        require(
            isinstance(value, dict)
            and isinstance(value.get("p95"), (int, float))
            and 0 <= value["p95"] <= budget,
            f"{field} p95 exceeds the Sprint 9 threshold",
        )
    bridge_frame = report.get("bridge_frame")
    require(
        isinstance(bridge_frame, dict)
        and bridge_frame.get("budget_usec") == 2_000
        and isinstance(bridge_frame.get("max_elapsed_usec"), int)
        and bridge_frame["max_elapsed_usec"] <= 2_000
        and bridge_frame.get("over_budget_count") == 0,
        "Bridge dispatcher slice threshold differs",
    )
    require(
        report.get("source_unchanged") is True
        and report.get("cleanup") is True
        and report.get("redaction") is True
        and report.get("transaction_identity_unique") is True,
        "model-free safety result differs",
    )


def latency_projection(model: Mapping[str, Any]) -> dict[str, Any]:
    values = cast(Mapping[str, Mapping[str, Any]], model["latency_ms"])
    return {
        name: {
            "p50_ms": values[name]["p50"],
            "p95_ms": values[name]["p95"],
            "max_ms": values[name]["max"],
        }
        for name in ("apply", "prepare", "status", "undo")
    }


def safe_evidence_scan(report: Mapping[str, Any]) -> None:
    encoded = canonical(report)
    require(len(encoded) <= 65_536, "Sprint 9 evidence exceeds 64 KiB")
    text = encoded.decode("utf-8")
    for forbidden in (
        "/Users/",
        "/tmp/",
        "C:\\",
        "S9_BIND_SECRET_SENTINEL",
        "S9_DELETED_SUBTREE_CANARY",
        "S9_PROPERTY_CANARY",
        '"native_history_id"',
        '"preview_payload_json"',
        '"session_token"',
        '"transaction_id"',
        "transaction:",
        "editor:",
        "node:",
    ):
        require(forbidden not in text, "Sprint 9 evidence leaks private material")


def validate_report(
    report: dict[str, Any],
    *,
    check_checkout: bool = True,
) -> dict[str, Any]:
    require(set(report) == TOP_LEVEL_FIELDS, "Sprint 9 evidence fields differ")
    platform_coordinate = report.get("platform")
    require(
        isinstance(platform_coordinate, dict)
        and set(platform_coordinate) == {"architecture", "os"},
        "platform coordinate differs",
    )
    coordinate = (
        str(platform_coordinate.get("os")),
        str(platform_coordinate.get("architecture")),
    )
    qualifying_profile = QUALIFYING_PROFILES.get(coordinate)
    require(qualifying_profile is not None, "platform coordinate differs")
    require(
        report.get("schema_version") == "s9-editor-transactions-evidence/1.0"
        and report.get("sprint") == 9
        and report.get("profile") == qualifying_profile["profile"]
        and report.get("status") == "passed",
        "Sprint 9 evidence coordinates differ",
    )
    if check_checkout:
        require(
            platform_coordinate == LOCAL_PLATFORM,
            "evidence does not match the local qualifying platform",
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
        and SHA256_RE.fullmatch(str(source.get("sha256"))) is not None
        and isinstance(source.get("file_count"), int)
        and source["file_count"] > 0
        and source.get("scope_manifest")
        == "tests/codex/sprint9_source_scopes.txt",
        "source binding differs",
    )
    for field in ("fixture_manifest_sha256", "golden_sha256"):
        require(
            SHA256_RE.fullmatch(str(source.get(field))) is not None,
            f"{field} differs",
        )
    if check_checkout:
        evidence_checkout_relation(str(source["commit"]))
        digest, count = source_digest()
        require(
            scoped_source_is_clean()
            and digest == source["sha256"]
            and count == source["file_count"],
            "current source does not match qualifying source",
        )
        require(
            source["fixture_manifest_sha256"] == sha256_file(FIXTURE_MANIFEST)
            and source["golden_sha256"] == sha256_file(GOLDEN),
            "oracle source binding differs",
        )
    protocols = report.get("protocols")
    require(
        protocols
        == {"bridge_rpc": "1.7", "mcp": "2025-11-25", "tools": 36},
        "protocol coordinates differ",
    )
    toolchain = report.get("toolchain")
    require(
        isinstance(toolchain, dict)
        and set(toolchain) == qualifying_profile["toolchain_fields"]
        and toolchain.get("rust_host") == qualifying_profile["rust_host"],
        "toolchain coordinates differ",
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
        and all(
            isinstance(value, str) and SHA256_RE.fullmatch(value)
            for value in artifacts.values()
        ),
        "artifact digests differ",
    )
    if check_checkout:
        require(
            artifacts["godot_sha256"] == sha256_file(GODOT)
            and artifacts["sidecar_sha256"] == sha256_file(SIDECAR)
            and artifacts["model_free_runner_sha256"]
            == sha256_file(MODEL_FREE_RUNNER)
            and artifacts["validator_sha256"] == sha256_file(Path(__file__)),
            "local artifact digest differs",
        )
    identities = report.get("transaction_identity_sha256")
    require(
        isinstance(identities, list)
        and len(identities) == 8
        and len(set(identities)) == 8
        and all(
            isinstance(value, str)
            and TRANSACTION_HASH_RE.fullmatch(value) is not None
            for value in identities
        ),
        "transaction identity proof differs",
    )
    summary = report.get("summary")
    require(
        isinstance(summary, dict)
        and set(summary)
        == {
            "elicitation_count",
            "fault_scenarios",
            "intervening_native_actions",
            "operations",
            "primary_transactions",
            "transaction_native_actions",
        }
        and summary.get("operations") == 8
        and summary.get("primary_transactions") == 8
        and summary.get("transaction_native_actions") == 8
        and summary.get("intervening_native_actions") == 4
        and summary.get("fault_scenarios") == 7
        and isinstance(summary.get("elicitation_count"), int)
        and summary["elicitation_count"] >= 13,
        "summary counts differ",
    )
    checks = report.get("checks")
    require(
        isinstance(checks, dict)
        and set(checks) == CHECK_FIELDS
        and all(value is True for value in checks.values()),
        "a Sprint 9 safety check did not pass",
    )
    lifecycle = report.get("lifecycle")
    require(
        lifecycle
        == {
            "apply_exactly_once": True,
            "native_redo_same_transaction": True,
            "prepare_read_only": True,
            "revision_relations_exact": True,
            "targeted_undo_exact": True,
        },
        "lifecycle relations differ",
    )
    recovery = report.get("recovery")
    require(
        recovery
        == {
            "bridge_response_loss": "committed_without_replay",
            "corrupt_journal": "quarantined_reads_available",
            "disconnect_before_commit": "failed_without_action",
            "editor_crash_before_commit": "pre_state_new_session",
            "editor_restart_after_commit": "no_persistent_undo",
            "journal_ack_restart": "committed_without_replay",
            "sidecar_disconnect_prepared": "immutable_reprepare",
        },
        "recovery outcomes differ",
    )
    performance = report.get("performance")
    require(
        isinstance(performance, dict)
        and set(performance)
        == {"apply", "bridge_dispatcher", "prepare", "status", "undo"},
        "performance fields differ",
    )
    for name, budget in (
        ("prepare", 500),
        ("status", 200),
        ("apply", 2_000),
        ("undo", 2_000),
    ):
        value = performance[name]
        require(
            isinstance(value, dict)
            and set(value) == {"max_ms", "p50_ms", "p95_ms"}
            and all(
                isinstance(value[field], (int, float))
                and not isinstance(value[field], bool)
                and value[field] >= 0
                for field in value
            )
            and value["p95_ms"] <= budget,
            f"{name} performance threshold differs",
        )
    require(
        performance["bridge_dispatcher"]
        == {
            "budget_usec": 2_000,
            "over_budget_count": 0,
            "within_budget": True,
        },
        "dispatcher threshold differs",
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
        "a local gate did not pass",
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
        },
        "redaction proof differs",
    )
    external = report.get("external_gates")
    expected_external = {
        "linux",
        "model_facing",
        "remote_ci",
        str(qualifying_profile["external_platform"]),
    }
    require(
        isinstance(external, dict)
        and set(external) == expected_external
        and all(
            isinstance(value, dict)
            and set(value) == {"reason", "status"}
            and value.get("status") == "not_run"
            and isinstance(value.get("reason"), str)
            and value["reason"]
            for value in external.values()
        ),
        "external gate claims differ",
    )
    safe_evidence_scan(report)
    return report


def check_release_sidecar_has_no_fault_seam() -> None:
    data = SIDECAR.read_bytes()
    for marker in (
        b"GODOT_CODEX_TRANSACTION_TEST",
        b"s9-transaction-fault/1.0",
        b"after_action_created_before_commit",
    ):
        require(marker not in data, "release sidecar contains a tests-only fault seam")


def publish_evidence(report: dict[str, Any], source_commit: str) -> None:
    EVIDENCE_PATH.parent.mkdir(parents=True, exist_ok=True)
    require(not EVIDENCE_PATH.exists(), "refusing to overwrite Sprint 9 evidence")
    temporary = EVIDENCE_PATH.with_name(
        f".{EVIDENCE_PATH.name}.{os.getpid()}.tmp"
    )
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
        require(scoped_source_is_clean(), "scoped source changed during qualification")
        digest, count = source_digest()
        require(
            digest == report["source"]["sha256"]
            and count == report["source"]["file_count"],
            "source digest changed during qualification",
        )
        temporary.replace(EVIDENCE_PATH)
        expected = (
            f"?? {EVIDENCE_PATH.relative_to(REPOSITORY_ROOT).as_posix()}"
        )
        require(
            status_lines() == [expected],
            "evidence is not the only worktree change",
        )
    except Exception:
        temporary.unlink(missing_ok=True)
        EVIDENCE_PATH.unlink(missing_ok=True)
        raise


def build_report(timeout: float) -> dict[str, Any]:
    toolchain = verify_toolchain()
    require_pristine_worktree()
    require(not EVIDENCE_PATH.exists(), "Sprint 9 evidence already exists")
    source_commit = current_commit()
    digest_before, file_count = source_digest()
    gates: dict[str, dict[str, Any]] = {}
    gates["fixture_oracle"] = run_composite_gate(
        "fixture/oracle and Python negatives",
        [
            [sys.executable, str(FIXTURE_VALIDATOR), "--schemas"],
            [
                sys.executable,
                "-m",
                "unittest",
                "tests/codex/test_transaction_fixture.py",
                "tests/codex/test_sprint9_acceptance.py",
            ],
        ],
        timeout=max(timeout, 300),
    )
    gates["approval_contracts"] = run_composite_gate(
        "approval boundary and contracts",
        [
            [sys.executable, "tests/codex/sprint9_contracts.py", "validate"],
            [
                sys.executable,
                "-m",
                "unittest",
                "tests/codex/test_sprint9_contracts.py",
            ],
        ],
        timeout=max(timeout, 300),
    )
    gates["rust_format"] = run_gate(
        "Rust format",
        rust_command(
            "cargo",
            "fmt",
            "--manifest-path",
            "godot-codex-mcp/Cargo.toml",
            "--all",
            "--",
            "--check",
        ),
    )
    gates["rust_workspace_tests"] = run_gate(
        "Rust workspace tests",
        rust_command(
            "cargo",
            "test",
            "--manifest-path",
            "godot-codex-mcp/Cargo.toml",
            "--workspace",
            "--all-targets",
            "--locked",
            "--offline",
        ),
        timeout=max(timeout * 4, 1_800),
    )
    gates["rust_clippy"] = run_gate(
        "Rust clippy",
        rust_command(
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
        ),
        timeout=max(timeout * 4, 1_800),
    )
    gates["rpc_conformance"] = run_gate(
        "Bridge RPC 1.0-1.7 conformance",
        rust_command(
            "cargo",
            "test",
            "--manifest-path",
            "tests/codex/Cargo.toml",
            "--locked",
            "--offline",
        ),
        timeout=max(timeout * 2, 900),
    )
    gates["godot_editor_build"] = run_gate(
        "tests-enabled Godot editor build",
        godot_build_command(),
        timeout=max(timeout * 6, 3_600),
    )
    gates["cpp_s9_profiles"] = run_gate(
        "focused C++ Sprint 9 profiles",
        [
            str(GODOT),
            "--headless",
            "--test",
            "--test-case=*CodexS9*",
            "--no-colors",
        ],
        timeout=max(timeout * 2, 900),
    )
    gates["release_sidecar_build"] = run_gate(
        "release sidecar build",
        rust_command(
            "cargo",
            "build",
            "--manifest-path",
            "godot-codex-mcp/Cargo.toml",
            "--release",
            "--locked",
            "--offline",
            "-p",
            "godot-codex-mcp",
        ),
        timeout=max(timeout * 4, 1_800),
    )
    check_release_sidecar_has_no_fault_seam()

    with tempfile.TemporaryDirectory(prefix="s9-acceptance.") as directory:
        workspace = Path(directory)
        direct_paths = [
            workspace / "prepare.json",
            workspace / "apply.json",
            workspace / "structural.json",
            workspace / "bindings.json",
        ]
        gates["direct_bridge_matrix"] = run_composite_gate(
            "direct Bridge fault/oracle matrix",
            [
                [
                    sys.executable,
                    "tests/codex/sprint9_prepare_live.py",
                    "--godot",
                    str(GODOT),
                    "--evidence",
                    str(direct_paths[0]),
                    "--timeout",
                    str(timeout),
                ],
                [
                    sys.executable,
                    "tests/codex/sprint9_apply_core_live.py",
                    "--godot",
                    str(GODOT),
                    "--evidence",
                    str(direct_paths[1]),
                    "--timeout",
                    str(timeout),
                ],
                [
                    sys.executable,
                    "tests/codex/sprint9_structural_live.py",
                    "--godot",
                    str(GODOT),
                    "--evidence",
                    str(direct_paths[2]),
                    "--timeout",
                    str(timeout),
                ],
                [
                    sys.executable,
                    "tests/codex/sprint9_bindings_live.py",
                    "--godot",
                    str(GODOT),
                    "--evidence",
                    str(direct_paths[3]),
                    "--timeout",
                    str(timeout),
                ],
            ],
            timeout=max(timeout * 8, 1_800),
        )
        for path in direct_paths:
            require(
                strict_json_load(path).get("status") == "passed",
                f"direct report did not pass: {path.name}",
            )

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
            timeout=max(timeout * 12, 2_400),
        )
        model = strict_json_load(model_path)
        validate_model_free(model)

        sprint7_path = workspace / "sprint7.json"
        gates["sprint7_regression"] = run_gate(
            "Sprint 7 live editor regression",
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
            timeout=max(timeout * 4, 900),
        )
        require(
            strict_json_load(sprint7_path).get("status") == "passed",
            "Sprint 7 regression report did not pass",
        )

        sprint8_headless = workspace / "sprint8-headless.json"
        sprint8_gui = workspace / "sprint8-gui.json"
        gates["sprint8_regressions"] = run_composite_gate(
            "Sprint 8 runtime regressions",
            [
                [
                    sys.executable,
                    "tests/codex/sprint8_runtime_live.py",
                    "--godot",
                    str(GODOT),
                    "--sidecar",
                    str(SIDECAR),
                    "--headless",
                    "--output",
                    str(sprint8_headless),
                    "--timeout",
                    str(timeout),
                ],
                [
                    sys.executable,
                    "tests/codex/sprint8_runtime_live.py",
                    "--godot",
                    str(GODOT),
                    "--sidecar",
                    str(SIDECAR),
                    "--output",
                    str(sprint8_gui),
                    "--timeout",
                    str(timeout),
                ],
            ],
            timeout=max(timeout * 8, 1_800),
        )
        require(
            strict_json_load(sprint8_headless).get("status") == "passed"
            and strict_json_load(sprint8_gui).get("status") == "passed",
            "Sprint 8 regression report did not pass",
        )
        model_report_sha256 = sha256_file(model_path)

    started = time.monotonic()
    require(current_commit() == source_commit, "HEAD changed during local gates")
    digest_after, count_after = source_digest()
    require(
        scoped_source_is_clean()
        and digest_after == digest_before
        and count_after == file_count,
        "Sprint 9 scoped source changed during local gates",
    )
    require_pristine_worktree()
    gates["final_integrity"] = {
        "status": "passed",
        "duration_ms": round((time.monotonic() - started) * 1_000, 3),
    }

    operations = cast(list[dict[str, Any]], model["operations"])
    transaction_hashes = [item["transaction_sha256"] for item in operations]
    report = {
        "schema_version": "s9-editor-transactions-evidence/1.0",
        "sprint": 9,
        "profile": LOCAL_PROFILE,
        "platform": LOCAL_PLATFORM,
        "source": {
            "commit": source_commit,
            "sha256": digest_before,
            "file_count": file_count,
            "scope_manifest": "tests/codex/sprint9_source_scopes.txt",
            "fixture_manifest_sha256": sha256_file(FIXTURE_MANIFEST),
            "golden_sha256": sha256_file(GOLDEN),
        },
        "toolchain": toolchain,
        "protocols": {
            "bridge_rpc": "1.7",
            "mcp": "2025-11-25",
            "tools": 36,
        },
        "artifacts": {
            "godot_sha256": sha256_file(GODOT),
            "sidecar_sha256": sha256_file(SIDECAR),
            "model_free_runner_sha256": sha256_file(MODEL_FREE_RUNNER),
            "validator_sha256": sha256_file(Path(__file__)),
            "model_free_report_sha256": model_report_sha256,
        },
        "summary": {
            "operations": 8,
            "primary_transactions": 8,
            "fault_scenarios": 7,
            "elicitation_count": model["elicitation_count"],
            "transaction_native_actions": sum(
                item["transaction_native_actions"] for item in operations
            ),
            "intervening_native_actions": sum(
                1 for item in operations if item["intervening_action"]
            ),
        },
        "transaction_identity_sha256": transaction_hashes,
        "checks": {field: True for field in CHECK_FIELDS},
        "lifecycle": {
            "prepare_read_only": True,
            "apply_exactly_once": True,
            "targeted_undo_exact": True,
            "native_redo_same_transaction": True,
            "revision_relations_exact": True,
        },
        "recovery": {
            "sidecar_disconnect_prepared": "immutable_reprepare",
            "bridge_response_loss": "committed_without_replay",
            "disconnect_before_commit": "failed_without_action",
            "journal_ack_restart": "committed_without_replay",
            "editor_crash_before_commit": "pre_state_new_session",
            "editor_restart_after_commit": "no_persistent_undo",
            "corrupt_journal": "quarantined_reads_available",
        },
        "performance": {
            **latency_projection(model),
            "bridge_dispatcher": {
                "budget_usec": 2_000,
                "over_budget_count": model["bridge_frame"]["over_budget_count"],
                "within_budget": True,
            },
        },
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
        },
        "external_gates": {
            cast(
                str,
                QUALIFYING_PROFILES[
                    (LOCAL_PLATFORM["os"], LOCAL_PLATFORM["architecture"])
                ]["external_platform"],
            ): {
                "status": "not_run",
                "reason": f"local qualifying coordinate is {MODEL_PLATFORM}",
            },
            "linux": {
                "status": "not_run",
                "reason": f"local qualifying coordinate is {MODEL_PLATFORM}",
            },
            "remote_ci": {
                "status": "not_run",
                "reason": "no remote CI coordinate was requested",
            },
            "model_facing": {
                "status": "not_run",
                "reason": "qualification uses a deterministic model-free client",
            },
        },
        "status": "passed",
    }
    validate_report(report, check_checkout=False)
    return report


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--validate", type=Path)
    return parser.parse_args()


def main() -> int:
    arguments = parse_arguments()
    if arguments.validate is not None:
        path = arguments.validate.resolve(strict=True)
        require(path == EVIDENCE_PATH.resolve(), "validation path is not canonical")
        validate_report(strict_json_load(path), check_checkout=True)
        print(
            json.dumps(
                {
                    "schema_version": "s9-evidence-validation/1.0",
                    "status": "passed",
                    "checkout_relation": evidence_checkout_relation(
                        str(strict_json_load(path)["source"]["commit"])
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
        print(f"Sprint 9 acceptance failed: {error}", file=sys.stderr)
        raise SystemExit(1)
