#!/usr/bin/env python3
"""Acquire a source-bound Sprint 6-10 live receipt from the detached package.

This runner is intentionally separate from the short Sprint 11 contract
preflight.  It executes the real Godot prerequisite and the exact sidecar
listed as ``bin/godot-codex-mcp`` in the detached package manifest.  Sprint 9
is sharded so every child has the same fail-closed 180 second maximum.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import struct
import subprocess
import sys
import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Final, cast

try:
    from tests.codex import sprint11_acquisition_paths as acquisition_paths
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_acquisition_paths as acquisition_paths

SCRIPT_DIR: Final = Path(__file__).resolve().parent
REPOSITORY_ROOT: Final = SCRIPT_DIR.parent.parent
RECEIPT_SCHEMA: Final = "s11-packaged-regression-receipt/1.0"
CAPTURE_KIND: Final = "real_package_live"
PACKAGE_MANIFEST_SCHEMA: Final = "s11-package-manifest/1.0"
PACKAGE_SIDECAR_PATH: Final = "bin/godot-codex-mcp"
MAX_REPORT_BYTES: Final = 2 * 1024 * 1024
MAX_RECEIPT_BYTES: Final = 128 * 1024
MAX_OUTPUT_BYTES: Final = 1024 * 1024
MAX_ARTIFACT_BYTES: Final = 512 * 1024 * 1024
MAX_COMMANDS: Final = 16
MACHO_CPU_TYPE_ARM64: Final = 0x0100000C
COMMIT_RE: Final = re.compile(r"[0-9a-f]{40}\Z")
DIGEST_RE: Final = re.compile(r"sha256:[0-9a-f]{64}\Z")
BUILD_PROVENANCE_FIELDS: Final = frozenset(
    {
        "cargo_lock_sha256",
        "cargo_version",
        "fresh_target",
        "rust_toolchain_sha256",
        "rustc_commit",
        "rustc_release",
        "target_triple",
    }
)

SPRINT9_OPERATIONS: Final = frozenset(
    {
        "attach_script",
        "connect_signal",
        "create_node",
        "delete_node",
        "detach_script",
        "disconnect_signal",
        "reparent_node",
        "set_property",
    }
)
SPRINT9_NEGATIVES: Final = frozenset(
    {
        "cancel",
        "committed_replay",
        "confirm_false",
        "decline",
        "expired_preview",
        "idempotency",
        "timeout",
        "unsupported",
    }
)
SPRINT9_FAULTS: Final = frozenset(
    {
        "sidecar_disconnect_prepared",
        "bridge_response_loss_after_commit",
        "disconnect_before_commit",
        "editor_crash_before_commit",
        "editor_restart_after_commit",
        "corrupt_journal",
        "restart_after_bridge_response_before_journal_ack",
    }
)
FIXTURE_PATHS: Final = (
    "tests/codex/fixtures/live_editor_oracle/fixture-manifest.json",
    "tests/codex/fixtures/live_editor_oracle/golden-live-editor.json",
    "tests/codex/fixtures/runtime_mvp_oracle/fixture-manifest.json",
    "tests/codex/fixtures/runtime_mvp_oracle/golden-runtime.json",
    "tests/codex/fixtures/semantic_context_oracle/fixture-manifest.json",
    "tests/codex/fixtures/semantic_context_oracle/golden-usages.json",
    "tests/codex/fixtures/transaction_oracle/fixture-manifest.json",
    "tests/codex/fixtures/transaction_oracle/golden-transactions.json",
)


class PackagedRegressionError(RuntimeError):
    """Raised when package-live acquisition cannot qualify."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise PackagedRegressionError(message)


def canonical_json(value: Any) -> bytes:
    try:
        return json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise PackagedRegressionError("value is not canonical JSON") from error


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    try:
        return acquisition_paths.sha256_regular_file(
            path,
            maximum=MAX_ARTIFACT_BYTES,
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise PackagedRegressionError("artifact cannot be hashed") from error


def strict_json_bytes(value: bytes, *, label: str, maximum: int) -> dict[str, Any]:
    require(len(value) <= maximum, f"{label} exceeds byte bound")

    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, child in items:
            require(key not in result, f"{label} contains a duplicate member")
            result[key] = child
        return result

    def constant(name: str) -> None:
        raise PackagedRegressionError(f"{label} contains non-finite JSON: {name}")

    try:
        result = json.loads(
            value,
            object_pairs_hook=pairs,
            parse_constant=constant,
        )
    except (UnicodeError, json.JSONDecodeError) as error:
        raise PackagedRegressionError(f"{label} is invalid JSON") from error
    require(isinstance(result, dict), f"{label} root is not an object")
    return cast(dict[str, Any], result)


def strict_json_load(
    path: Path,
    *,
    label: str,
    maximum: int = MAX_REPORT_BYTES,
) -> dict[str, Any]:
    try:
        value = acquisition_paths.read_regular_file(
            path,
            maximum=maximum,
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise PackagedRegressionError(f"{label} is unreadable") from error
    return strict_json_bytes(value, label=label, maximum=maximum)


def safe_relative_path(value: Any, *, label: str) -> str:
    require(isinstance(value, str), f"{label} is not a path")
    path = PurePosixPath(value)
    require(
        0 < len(value.encode("utf-8")) <= 512
        and value == path.as_posix()
        and not path.is_absolute()
        and "." not in path.parts
        and ".." not in path.parts,
        f"{label} is not a canonical relative path",
    )
    return value


def exact_fields(
    value: Any,
    fields: set[str] | frozenset[str],
    *,
    label: str,
) -> Mapping[str, Any]:
    require(isinstance(value, dict) and set(value) == set(fields), f"{label} fields differ")
    return cast(Mapping[str, Any], value)


def digest(value: Any, *, label: str) -> str:
    require(
        isinstance(value, str) and DIGEST_RE.fullmatch(value) is not None,
        f"{label} digest differs",
    )
    return value


def require_regular(path: Path, *, executable: bool = False) -> None:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise PackagedRegressionError("required artifact is missing") from error
    require(
        stat.S_ISREG(metadata.st_mode) and not path.is_symlink(),
        "required artifact is not a regular file",
    )
    if executable:
        require(metadata.st_mode & 0o111 != 0, "required artifact is not executable")


def require_arm64_macho(path: Path) -> None:
    require_regular(path, executable=True)
    try:
        header = path.read_bytes()[:8]
    except OSError as error:
        raise PackagedRegressionError("Mach-O header is unreadable") from error
    require(len(header) == 8, "artifact is not a Mach-O executable")
    if header[:4] == b"\xcf\xfa\xed\xfe":
        cpu_type = struct.unpack("<I", header[4:8])[0]
    elif header[:4] == b"\xfe\xed\xfa\xcf":
        cpu_type = struct.unpack(">I", header[4:8])[0]
    else:
        raise PackagedRegressionError("artifact is not a thin Mach-O executable")
    require(cpu_type == MACHO_CPU_TYPE_ARM64, "artifact architecture is not arm64")


@dataclass(frozen=True)
class CommandSpec:
    command_id: str
    runner_path: str
    report_name: str
    arguments: tuple[str, ...]

    def argv_template(self) -> tuple[str, ...]:
        return (
            "{python}",
            self.runner_path,
            "--godot",
            "{godot}",
            "--sidecar",
            "{package_sidecar}",
            *self.arguments,
            "--report" if self.runner_path.endswith(("sprint9_model_free_live.py", "sprint10_model_free_live.py")) else "--output",
            f"{{report_root}}/{self.report_name}",
        )


def canonical_command_specs() -> tuple[CommandSpec, ...]:
    common_s9 = (
        "--additive-sprint10-registry",
        "--additive-sprint11-registry",
        "--timeout",
        "{timeout}",
    )
    return (
        CommandSpec(
            "s6_semantic",
            "tests/codex/sprint6_semantic_context_live.py",
            "sprint6.json",
            (
                "--additive-sprint11-registry",
                "--timeout",
                "{timeout}",
            ),
        ),
        CommandSpec(
            "s7_editor",
            "tests/codex/sprint7_live_editor.py",
            "sprint7.json",
            (
                "--additive-sprint11-registry",
                "--timeout",
                "{timeout}",
            ),
        ),
        CommandSpec(
            "s8_runtime",
            "tests/codex/sprint8_runtime_live.py",
            "sprint8.json",
            (
                "--additive-sprint11-registry",
                "--headless",
                "--timeout",
                "{timeout}",
            ),
        ),
        CommandSpec(
            "s9_operations_a",
            "tests/codex/sprint9_model_free_live.py",
            "sprint9-operations-a.json",
            (
                *common_s9,
                "--operations",
                "attach_script",
                "connect_signal",
                "create_node",
                "delete_node",
                "--skip-negatives",
                "--skip-faults",
            ),
        ),
        CommandSpec(
            "s9_operations_b",
            "tests/codex/sprint9_model_free_live.py",
            "sprint9-operations-b.json",
            (
                *common_s9,
                "--operations",
                "detach_script",
                "disconnect_signal",
                "reparent_node",
                "set_property",
                "--skip-negatives",
                "--skip-faults",
            ),
        ),
        CommandSpec(
            "s9_negatives",
            "tests/codex/sprint9_model_free_live.py",
            "sprint9-negatives.json",
            (
                *common_s9,
                "--operations",
                "--skip-faults",
            ),
        ),
        CommandSpec(
            "s9_faults_a",
            "tests/codex/sprint9_model_free_live.py",
            "sprint9-faults-a.json",
            (
                *common_s9,
                "--operations",
                "--skip-negatives",
                "--fault-scenarios",
                "sidecar_disconnect_prepared",
                "bridge_response_loss_after_commit",
                "disconnect_before_commit",
            ),
        ),
        CommandSpec(
            "s9_faults_b",
            "tests/codex/sprint9_model_free_live.py",
            "sprint9-faults-b.json",
            (
                *common_s9,
                "--operations",
                "--skip-negatives",
                "--fault-scenarios",
                "editor_crash_before_commit",
                "editor_restart_after_commit",
                "corrupt_journal",
                "restart_after_bridge_response_before_journal_ack",
            ),
        ),
        CommandSpec(
            "s10_compound",
            "tests/codex/sprint10_model_free_live.py",
            "sprint10.json",
            (
                "--additive-sprint11-registry",
                "--timeout",
                "{timeout}",
            ),
        ),
    )


def expand_argv(
    spec: CommandSpec,
    *,
    godot: Path,
    sidecar: Path,
    report_root: Path,
    timeout: float,
) -> tuple[str, ...]:
    substitutions = {
        "{python}": sys.executable,
        "{godot}": str(godot),
        "{package_sidecar}": str(sidecar),
        "{timeout}": str(timeout),
    }
    prefix = "{report_root}/"
    expanded: list[str] = []
    for item in spec.argv_template():
        if item.startswith(prefix):
            expanded.append(str(report_root / item[len(prefix) :]))
        else:
            expanded.append(substitutions.get(item, item))
    return tuple(expanded)


@dataclass(frozen=True)
class Execution:
    exit_code: int
    stdout: bytes
    stderr: bytes
    duration_ms: int


Executor = Callable[[CommandSpec, tuple[str, ...], Path, float], Execution]


def execute_command(
    _spec: CommandSpec,
    argv: tuple[str, ...],
    cwd: Path,
    timeout: float,
) -> Execution:
    started = time.monotonic()
    try:
        result = subprocess.run(
            argv,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
            env={
                **os.environ,
                "NO_COLOR": "1",
                "PYTHONHASHSEED": "0",
            },
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise PackagedRegressionError("package-live command failed or timed out") from error
    require(
        len(result.stdout) <= MAX_OUTPUT_BYTES
        and len(result.stderr) <= MAX_OUTPUT_BYTES,
        "package-live command output exceeds byte bound",
    )
    return Execution(
        exit_code=result.returncode,
        stdout=result.stdout,
        stderr=result.stderr,
        duration_ms=int((time.monotonic() - started) * 1_000),
    )


def _artifacts(report: Mapping[str, Any]) -> Mapping[str, Any]:
    artifacts = report.get("artifacts")
    require(
        isinstance(artifacts, dict)
        and {"godot_sha256", "sidecar_sha256"}.issubset(artifacts),
        "live report artifacts differ",
    )
    return cast(Mapping[str, Any], artifacts)


def _require_artifacts(
    report: Mapping[str, Any],
    *,
    godot_sha256: str,
    sidecar_sha256: str,
) -> None:
    artifacts = _artifacts(report)
    require(
        artifacts["godot_sha256"] == godot_sha256
        and artifacts["sidecar_sha256"] == sidecar_sha256,
        "live report artifact binding differs",
    )


def _all_true(value: Any) -> bool:
    return isinstance(value, dict) and bool(value) and all(item is True for item in value.values())


def validate_live_report(
    command_id: str,
    report: Mapping[str, Any],
    *,
    godot_sha256: str,
    sidecar_sha256: str,
) -> None:
    _require_artifacts(
        report,
        godot_sha256=godot_sha256,
        sidecar_sha256=sidecar_sha256,
    )
    if command_id == "s6_semantic":
        require(
            report.get("schema_version") == 1
            and report.get("sprint") == 6
            and report.get("profile") == "model_free_live_smoke"
            and report.get("status") == "passed"
            and _all_true(report.get("cleanup"))
            and _all_true(report.get("redaction")),
            "Sprint 6 package-live report differs",
        )
        contract = report.get("mcp_contract")
        require(
            isinstance(contract, dict)
            and contract.get("exact_ten_tool_registry") is True
            and contract.get("closed_input_schemas") is True
            and contract.get("read_only_annotations") is True,
            "Sprint 6 MCP contract proof differs",
        )
        return
    if command_id == "s7_editor":
        require(
            report.get("schema_version") == 1
            and report.get("sprint") == 7
            and report.get("profile") == "model_free_live_editor"
            and report.get("status") == "passed"
            and _all_true(report.get("cleanup")),
            "Sprint 7 package-live report differs",
        )
        checks = report.get("checks")
        require(
            isinstance(checks, dict)
            and checks
            and all(item is True for item in checks.values()),
            "Sprint 7 live checks differ",
        )
        return
    if command_id == "s8_runtime":
        require(
            report.get("schema_version") == 1
            and report.get("sprint") == 8
            and report.get("status") == "passed"
            and report.get("headless") is True
            and _all_true(report.get("cleanup"))
            and _all_true(report.get("redaction")),
            "Sprint 8 package-live report differs",
        )
        checks = report.get("checks")
        require(
            isinstance(checks, dict)
            and checks.get("runtime_tree_and_opaque_ids") is True
            and checks.get("bounded_properties") is True
            and checks.get("diagnostic_stack") is True
            and checks.get("fresh_session_per_run") is True
            and checks.get("hang_timeout_and_recovery") is True,
            "Sprint 8 runtime proof differs",
        )
        return
    if command_id.startswith("s9_"):
        require(
            report.get("schema_version") == "s9-model-free-workflow/1.0"
            and report.get("status") == "passed"
            and report.get("platform") == "macos-arm64"
            and report.get("protocol") == "2025-11-25"
            and report.get("tool_registry") == 41
            and report.get("source_unchanged") is True
            and report.get("cleanup") is True
            and report.get("redaction") is True,
            "Sprint 9 package-live report differs",
        )
        operations = report.get("operations")
        negatives = report.get("negatives")
        faults = report.get("faults")
        require(
            isinstance(operations, list)
            and isinstance(negatives, dict)
            and isinstance(faults, dict),
            "Sprint 9 report collections differ",
        )
        operation_names = {
            item.get("operation")
            for item in operations
            if isinstance(item, dict)
        }
        require(
            all(
                isinstance(item, dict)
                and item.get("oracle_cycle") is True
                and item.get("source_unchanged") is True
                and item.get("transaction_native_actions") == 1
                for item in operations
            ),
            "Sprint 9 operation lifecycle proof differs",
        )
        if command_id == "s9_operations_a":
            require(
                operation_names
                == {"attach_script", "connect_signal", "create_node", "delete_node"}
                and not negatives
                and not faults,
                "Sprint 9 operation shard A differs",
            )
        elif command_id == "s9_operations_b":
            require(
                operation_names
                == {"detach_script", "disconnect_signal", "reparent_node", "set_property"}
                and not negatives
                and not faults,
                "Sprint 9 operation shard B differs",
            )
        elif command_id == "s9_negatives":
            approval = negatives.get("approval")
            decisions = {
                item.get("decision")
                for item in approval
                if isinstance(item, dict)
            } if isinstance(approval, list) else set()
            require(
                not operations
                and not faults
                and set(negatives)
                == {"approval", "committed_replay", "expired_preview", "idempotency"}
                and decisions
                == {"confirm_false", "decline", "cancel", "unsupported", "timeout"},
                "Sprint 9 negative matrix differs",
            )
        elif command_id == "s9_faults_a":
            require(
                not operations
                and not negatives
                and set(faults)
                == {
                    "sidecar_disconnect_prepared",
                    "bridge_response_loss_after_commit",
                    "disconnect_before_commit",
                },
                "Sprint 9 fault shard A differs",
            )
        elif command_id == "s9_faults_b":
            require(
                not operations
                and not negatives
                and set(faults)
                == {
                    "editor_crash_before_commit",
                    "editor_restart_after_commit",
                    "corrupt_journal",
                    "restart_after_bridge_response_before_journal_ack",
                },
                "Sprint 9 fault shard B differs",
            )
        else:
            raise PackagedRegressionError("unknown Sprint 9 command")
        return
    if command_id == "s10_compound":
        require(
            report.get("schema_version") == "s10-model-free-live/1.0"
            and report.get("status") == "passed"
            and report.get("platform") == "macos-arm64"
            and report.get("protocol") == "2025-11-25"
            and report.get("bridge_rpc") == "1.8"
            and report.get("tool_registry") == 41
            and report.get("operation_count") == 2
            and report.get("one_native_action") is True
            and report.get("prepare_read_only") is True
            and report.get("exact_undo") is True
            and report.get("cleanup") == {"processes_stopped": True},
            "Sprint 10 package-live report differs",
        )
        validation = report.get("validation")
        require(
            isinstance(validation, dict)
            and validation.get("outcome") == "passed"
            and isinstance(validation.get("checks"), dict)
            and validation["checks"],
            "Sprint 10 validation proof differs",
        )
        return
    raise PackagedRegressionError("unknown package-live command")


def validate_aggregate_reports(reports: Mapping[str, Mapping[str, Any]]) -> None:
    require(
        set(reports) == {spec.command_id for spec in canonical_command_specs()},
        "package-live report set differs",
    )
    operation_names: list[str] = []
    fault_names: list[str] = []
    for command_id in ("s9_operations_a", "s9_operations_b"):
        operation_names.extend(
            cast(str, item["operation"])
            for item in cast(list[dict[str, Any]], reports[command_id]["operations"])
        )
    for command_id in ("s9_faults_a", "s9_faults_b"):
        fault_names.extend(cast(dict[str, Any], reports[command_id]["faults"]))
    negatives = cast(dict[str, Any], reports["s9_negatives"]["negatives"])
    approval = cast(list[dict[str, Any]], negatives["approval"])
    negative_names = {
        "idempotency",
        "committed_replay",
        "expired_preview",
        *(cast(str, item["decision"]) for item in approval),
    }
    require(
        len(operation_names) == len(set(operation_names))
        and set(operation_names) == SPRINT9_OPERATIONS
        and len(fault_names) == len(set(fault_names))
        and set(fault_names) == SPRINT9_FAULTS
        and negative_names == SPRINT9_NEGATIVES,
        "Sprint 9 aggregate shard coverage differs",
    )


def _manifest_bindings(
    manifest_path: Path,
    artifact_root: Path,
    godot: Path,
    *,
    version_probe: Callable[[Path], str],
) -> tuple[dict[str, Any], Path]:
    manifest = strict_json_load(
        manifest_path,
        label="detached package manifest",
        maximum=256 * 1024,
    )
    manifest = exact_fields(
        manifest,
        {
            "schema_version",
            "package_version",
            "source_commit",
            "build_provenance",
            "archive",
            "compatibility_matrix_sha256",
            "registry_sha256",
            "third_party_licenses_sha256",
            "godot_prerequisite",
            "contents",
        },
        label="detached package manifest",
    )
    require(
        manifest["schema_version"] == PACKAGE_MANIFEST_SCHEMA,
        "detached package manifest schema differs",
    )
    source_commit = manifest.get("source_commit")
    require(
        isinstance(source_commit, str) and COMMIT_RE.fullmatch(source_commit) is not None,
        "package source commit differs",
    )
    provenance = exact_fields(
        manifest["build_provenance"],
        BUILD_PROVENANCE_FIELDS,
        label="package build provenance",
    )
    for field in ("cargo_lock_sha256", "rust_toolchain_sha256"):
        digest(provenance[field], label=f"package {field}")
    require(
        isinstance(provenance["cargo_version"], str)
        and 0 < len(provenance["cargo_version"].encode("utf-8")) <= 256
        and isinstance(provenance["rustc_release"], str)
        and 0 < len(provenance["rustc_release"].encode("utf-8")) <= 128
        and isinstance(provenance["rustc_commit"], str)
        and COMMIT_RE.fullmatch(provenance["rustc_commit"]) is not None
        and provenance["fresh_target"] is True
        and provenance["target_triple"] == "aarch64-apple-darwin",
        "package build provenance differs",
    )
    archive = exact_fields(
        manifest.get("archive"),
        {"path", "sha256", "bytes"},
        label="package archive",
    )
    archive_relative = safe_relative_path(archive["path"], label="package archive")
    archive_path = artifact_root.joinpath(*PurePosixPath(archive_relative).parts)
    require_regular(archive_path)
    require(
        sha256_file(archive_path) == digest(archive["sha256"], label="package archive"),
        "package archive digest differs",
    )
    contents = manifest.get("contents")
    require(isinstance(contents, list), "package contents are missing")
    content_records = {
        safe_relative_path(item.get("path"), label="package content"): item
        for item in contents
        if isinstance(item, dict)
    }
    require(
        len(content_records) == len(contents)
        and PACKAGE_SIDECAR_PATH in content_records,
        "package sidecar content record is missing or duplicated",
    )
    sidecar = artifact_root / PACKAGE_SIDECAR_PATH
    require_arm64_macho(sidecar)
    sidecar_record = cast(dict[str, Any], content_records[PACKAGE_SIDECAR_PATH])
    sidecar_sha256 = digest(
        sidecar_record.get("sha256"),
        label="package sidecar",
    )
    require(
        sha256_file(sidecar) == sidecar_sha256,
        "package sidecar digest differs",
    )
    prerequisite = exact_fields(
        manifest.get("godot_prerequisite"),
        {
            "version",
            "commit",
            "sha256",
            "architecture",
            "expected_install_path",
            "verification",
        },
        label="Godot prerequisite",
    )
    require_arm64_macho(godot)
    godot_sha256 = digest(prerequisite["sha256"], label="Godot prerequisite")
    require(
        prerequisite["architecture"] == "arm64"
        and sha256_file(godot) == godot_sha256
        and version_probe(godot) == prerequisite["version"],
        "Godot prerequisite identity differs",
    )
    return (
        {
            "package_source_commit": source_commit,
            "package_manifest_sha256": sha256_file(manifest_path),
            "package_build_provenance_sha256": sha256_bytes(
                canonical_json(provenance)
            ),
            "package_archive_sha256": archive["sha256"],
            "package_sidecar_path": PACKAGE_SIDECAR_PATH,
            "package_sidecar_sha256": sidecar_sha256,
            "godot_version": prerequisite["version"],
            "godot_commit": prerequisite["commit"],
            "godot_artifact_sha256": godot_sha256,
            "registry_sha256": manifest["registry_sha256"],
        },
        sidecar,
    )


def probe_godot_version(godot: Path) -> str:
    try:
        result = subprocess.run(
            [godot, "--version"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
            check=False,
            text=True,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise PackagedRegressionError("Godot version probe failed") from error
    require(
        result.returncode == 0
        and 0 < len(result.stdout.encode("utf-8")) <= 256,
        "Godot version probe differs",
    )
    return result.stdout.strip()


def repository_head() -> str:
    try:
        result = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=REPOSITORY_ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise PackagedRegressionError("repository source coordinate is unavailable") from error
    value = result.stdout.strip()
    require(COMMIT_RE.fullmatch(value) is not None, "repository HEAD differs")
    return value


def require_source_bound_inputs_clean(specs: Sequence[CommandSpec]) -> None:
    paths = sorted(
        {
            Path(__file__).resolve().relative_to(REPOSITORY_ROOT).as_posix(),
            *FIXTURE_PATHS,
            *(spec.runner_path for spec in specs),
        }
    )
    try:
        result = subprocess.run(
            [
                "git",
                "--literal-pathspecs",
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--",
                *paths,
            ],
            cwd=REPOSITORY_ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as error:
        raise PackagedRegressionError(
            "repository source cleanliness is unavailable"
        ) from error
    require(
        result.returncode == 0 and result.stdout == b"",
        "package-live runner or fixture differs from package source",
    )


def acquire(
    *,
    artifact_root: Path,
    package_manifest: Path,
    godot: Path,
    output_root: Path,
    timeout: float,
    executor: Executor = execute_command,
    version_probe: Callable[[Path], str] = probe_godot_version,
    check_repository: bool = True,
) -> dict[str, Any]:
    require(1 <= timeout <= 180, "timeout must be between 1 and 180 seconds")
    artifact_root = artifact_root.resolve(strict=True)
    package_manifest = package_manifest.resolve(strict=True)
    godot = godot.resolve(strict=True)
    repository = REPOSITORY_ROOT.resolve()
    bindings, sidecar = _manifest_bindings(
        package_manifest,
        artifact_root,
        godot,
        version_probe=version_probe,
    )
    specs = canonical_command_specs()
    if check_repository:
        require(
            repository_head() == bindings["package_source_commit"],
            "acquisition HEAD differs from package source",
        )
        require_source_bound_inputs_clean(specs)
    original_sidecar_sha256 = sha256_file(sidecar)
    original_godot_sha256 = sha256_file(godot)
    original_manifest_sha256 = sha256_file(package_manifest)
    require(
        1 <= len(specs) <= MAX_COMMANDS
        and len({item.command_id for item in specs}) == len(specs),
        "canonical command matrix differs",
    )
    try:
        destination = acquisition_paths.prepare_repository_directory(
            output_root,
            repository=repository,
            prefix=".s11-package-live.",
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise PackagedRegressionError(
            "output root must be a safe new repository-local directory"
        ) from error
    output_root = destination.target
    staging = destination.staging
    reports: dict[str, Mapping[str, Any]] = {}
    command_records: list[dict[str, Any]] = []
    try:
        for spec in specs:
            runner = REPOSITORY_ROOT.joinpath(
                *PurePosixPath(spec.runner_path).parts
            )
            require_regular(runner)
            argv = expand_argv(
                spec,
                godot=godot,
                sidecar=sidecar,
                report_root=staging,
                timeout=timeout,
            )
            execution = executor(spec, argv, REPOSITORY_ROOT, timeout)
            require(
                execution.exit_code == 0
                and 0 <= execution.duration_ms <= int(timeout * 1_000),
                f"{spec.command_id} failed or exceeded its bound",
            )
            report_path = staging / spec.report_name
            report = strict_json_load(
                report_path,
                label=f"{spec.command_id} report",
            )
            validate_live_report(
                spec.command_id,
                report,
                godot_sha256=cast(str, bindings["godot_artifact_sha256"]),
                sidecar_sha256=cast(str, bindings["package_sidecar_sha256"]),
            )
            final_report_path = output_root / spec.report_name
            report_relative = final_report_path.relative_to(repository).as_posix()
            template = list(spec.argv_template())
            command_records.append(
                {
                    "id": spec.command_id,
                    "cwd": ".",
                    "argv_template": template,
                    "command_sha256": sha256_bytes(
                        canonical_json({"cwd": ".", "argv": template})
                    ),
                    "runner_path": spec.runner_path,
                    "runner_sha256": sha256_file(runner),
                    "report_path": report_relative,
                    "report_sha256": sha256_file(report_path),
                    "stdout_sha256": sha256_bytes(execution.stdout),
                    "stderr_sha256": sha256_bytes(execution.stderr),
                    "exit_code": 0,
                    "duration_ms": execution.duration_ms,
                }
            )
            reports[spec.command_id] = report
        validate_aggregate_reports(reports)
        require(
            sha256_file(sidecar) == original_sidecar_sha256
            and sha256_file(godot) == original_godot_sha256
            and sha256_file(package_manifest) == original_manifest_sha256,
            "package or Godot artifact changed during acquisition",
        )
        fixture_records = [
            {
                "id": PurePosixPath(relative).parent.name
                + ":"
                + PurePosixPath(relative).name,
                "path": relative,
                "sha256": sha256_file(
                    REPOSITORY_ROOT.joinpath(*PurePosixPath(relative).parts)
                ),
            }
            for relative in FIXTURE_PATHS
        ]
        receipt = {
            "schema_version": RECEIPT_SCHEMA,
            "capture_kind": CAPTURE_KIND,
            "status": "passed",
            "bindings": bindings,
            "fixtures": fixture_records,
            "commands": command_records,
            "coverage": {
                "sprints": [6, 7, 8, 9, 10],
                "sprint9_operations": sorted(SPRINT9_OPERATIONS),
                "sprint9_negative_scenarios": sorted(SPRINT9_NEGATIVES),
                "sprint9_fault_scenarios": sorted(SPRINT9_FAULTS),
                "sprint10_validation": True,
            },
            "cleanup": {
                "editor_processes_stopped": True,
                "game_processes_stopped": True,
                "sidecar_processes_stopped": True,
                "temporary_workspaces_removed": True,
                "package_unchanged": True,
            },
            "redaction": {
                "absolute_paths_absent": True,
                "native_ids_absent": True,
                "secrets_absent": True,
                "source_content_absent": True,
            },
        }
        encoded = canonical_json(receipt) + b"\n"
        require(len(encoded) <= MAX_RECEIPT_BYTES, "receipt exceeds byte bound")
        (staging / "receipt.json").write_bytes(encoded)
        destination.publish()
        return receipt
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact-root", type=Path, required=True)
    parser.add_argument("--package-manifest", type=Path, required=True)
    parser.add_argument("--godot", type=Path, required=True)
    parser.add_argument("--output-root", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=180.0)
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    receipt = acquire(
        artifact_root=arguments.artifact_root,
        package_manifest=arguments.package_manifest,
        godot=arguments.godot,
        output_root=arguments.output_root,
        timeout=arguments.timeout,
    )
    print(
        json.dumps(
            {
                "schema_version": RECEIPT_SCHEMA,
                "status": receipt["status"],
                "receipt": (
                    arguments.output_root.resolve() / "receipt.json"
                ).relative_to(REPOSITORY_ROOT.resolve()).as_posix(),
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except PackagedRegressionError as error:
        print(f"Sprint 11 packaged regressions failed: {error}", file=sys.stderr)
        raise SystemExit(1)
