#!/usr/bin/env python3
"""Build Sprint 11 surface artifacts without manufacturing host authority.

The command has three deliberately separate stages:

``prepare``
    Measures immutable package, host-provenance, Git, and fixture inputs and
    writes ``s11-surface-metadata/1.1``.  It accepts no caller-provided digest
    or host-observation flag.

``derive``
    Validates one private Rust ``sprint11-surface-capture-journal/1.1`` and
    converts it into a timestamp-free canonical ``s11-recorder-journal/1.1``
    plus an explicitly non-qualifying pending trace.  All 23 semantic records
    are independently derived from hash-bound, closed ``tool_observation``
    request/result projections.  An optional Rust semantic projection is
    accepted only as an exact redundant cross-check.

``attest``
    Uses the controlling terminal for an external operator's identity and one
    explicit yes/no response for every observation.  Only this stage joins the
    three host-only assertions and atomically publishes the canonical journal,
    final trace, and authority directory.

No stage accepts a ``--yes`` switch, prebuilt observation JSON, environment
attestation, or stdin batch.  A negative/incomplete attestation publishes
nothing.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, TextIO, cast

try:
    from tests.codex import sprint11_acceptance as acceptance
    from tests.codex import sprint11_acquisition_paths as acquisition_paths
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_acceptance as acceptance
    import sprint11_acquisition_paths as acquisition_paths

METADATA_SCHEMA_VERSION = "s11-surface-metadata/1.1"
RAW_CAPTURE_SCHEMA_VERSION = "sprint11-surface-capture-artifact/1.1"
RAW_JOURNAL_SCHEMA_VERSION = "sprint11-surface-capture-journal/1.1"
JOURNAL_SCHEMA_VERSION = "s11-recorder-journal/1.1"
PENDING_TRACE_SCHEMA_VERSION = "s11-surface-trace-pending/1.0"
FINAL_TRACE_SCHEMA_VERSION = "s11-surface-trace/1.0"
AUTHORITY_SCHEMA_VERSION = "s11-surface-acquisition-authority/1.0"
CAPTURE_KIND = "surface_transport_capture"
JOURNAL_STATUS = "complete"
PENDING_STATUS = "awaiting_external_attestation"
FINAL_STATUS = "passed"

EVENT_CHAIN_DOMAIN = b"godot-codex/s11-recorder-event-chain/v1.1\0"
RAW_EVENT_HASH_DOMAIN = b"sprint11-surface-capture-event-v1.1\0"
FIXTURE_BINDING_DOMAIN = b"godot-codex/s11-runtime-fixture-binding/v1\0"
PROJECT_IDENTITY_DOMAIN = b"sprint11-canonical-project-root-v1\0"
OPERATOR_ID_DOMAIN = b"godot-codex/s11-surface-operator/v1\0"
AUTHORITY_DOMAIN = b"s11-surface-acquisition-authority-v1\0"
AUTHORITY_TRUST_BOUNDARY = (
    "git_binds_exact_surface_artifacts_external_operator_attests_actual_"
    "host_session_without_cryptographic_process_origin_proof"
)

MAX_JSON_BYTES = 16 * 1024 * 1024
MAX_FILE_BYTES = 512 * 1024 * 1024
MAX_GIT_BLOB_BYTES = 16 * 1024 * 1024
MAX_FIXTURE_FILE_BYTES = 64 * 1024 * 1024
MAX_EVENTS = 4_096
MAX_EVENT_BYTES = 8_388_608
MAX_OPERATOR_ID_BYTES = 256
GIT_TIMEOUT_SECONDS = 30.0

COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")
DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
VERSION_RE = re.compile(r"[0-9A-Za-z][0-9A-Za-z.+-]{0,127}\Z")
SEMANTIC_TOKEN_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:/{}-]{0,255}\Z")

REPOSITORY_ROOT = Path(__file__).resolve().parent.parent.parent
REGISTRY_PATH = "godot-codex-mcp/product/registry-profile.v1.json"
PROMPT_PATH = "tests/codex/prompts/sprint11-external-beta-v1.json"
MATRIX_PATH = "godot-codex-mcp/product/compatibility-matrix.v1.json"
PROFILE_PATH = "godot-codex-mcp/product/host-coordinate-profile.v1.json"
INSTRUCTIONS_PATH = "godot-codex-mcp/product/server-instructions.v1.txt"
CAPTURE_SCHEMA_PACKAGE_PATHS = {
    (
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-recorder-journal.schema.json"
    ): (
        "share/godot-codex/schemas/godot_codex/"
        "sprint11-recorder-journal.schema.json"
    ),
    (
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-surface-capture-artifact.schema.json"
    ): (
        "share/godot-codex/schemas/godot_codex/"
        "sprint11-surface-capture-artifact.schema.json"
    ),
    (
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-surface-metadata.schema.json"
    ): (
        "share/godot-codex/schemas/godot_codex/"
        "sprint11-surface-metadata.schema.json"
    ),
}
CAPTURE_REQUIRED_PACKAGE_MODES = {
    "bin/godot-codex": "0755",
    "bin/godot-codex-mcp": "0755",
    **{
        package_path: "0644"
        for package_path in CAPTURE_SCHEMA_PACKAGE_PATHS.values()
    },
}
FIXTURE_MANIFEST_PATH = (
    "tests/codex/fixtures/runtime_mvp_oracle/fixture-manifest.json"
)
FIXTURE_SOURCE_ROOT = "tests/codex/fixtures/runtime_mvp_project"

# Exactly fourteen facts are derived from MCP transport.  The three
# HOST_ATTESTED_ASSERTIONS are intentionally absent until the external
# operator completes the authority checklist.  host.unsupported_form is a
# package-global gate and is intentionally absent from both sets.
TRANSPORT_ASSERTION_IDS = frozenset(
    {
        "approval.accept",
        "approval.cancel",
        "approval.decline",
        "approval.timeout",
        "connection.status",
        "host.offline_status",
        "multi_project.reject",
        "offline.saved_query",
        "runtime.error_stack",
        "saved.current_scene",
        "transaction.apply",
        "transaction.preview",
        "transaction.undo",
        "validation.result",
    }
)
HOST_ATTESTED_ASSERTIONS = frozenset(
    {
        "host.approval_layers",
        "host.config_reload",
        "host.launcher",
    }
)
FINAL_ASSERTION_IDS = TRANSPORT_ASSERTION_IDS | HOST_ATTESTED_ASSERTIONS
FORM_SCENARIOS = frozenset({"accept", "cancel", "decline", "timeout"})
REVISION_STEPS = frozenset(
    {
        "initial",
        "prepared",
        "applied",
        "restarted",
        "stale_guard_rejected",
    }
)

# Derivation emits one record for every identifier in this exact allowlist:
#   {"projection_id": <id>, "source_event_seq": <zero-based event>, "value": ...}
# No prefix wildcard, partial derived coverage, duplicate identifier, unknown
# projection, or projection not reproducible from its tool observation is
# accepted.
SEMANTIC_PROJECTION_IDS = frozenset(
    {
        *(f"assertion.{item}" for item in TRANSPORT_ASSERTION_IDS),
        *(f"form_outcome.{item}" for item in FORM_SCENARIOS),
        *(f"revision.{item}" for item in REVISION_STEPS),
    }
)

AUTHORITY_OBSERVATIONS = (
    "actual_surface_session_observed",
    "official_host_process_observed",
    "recorder_stdio_bound_to_host_session",
    "exact_project_root_observed",
    "project_trust_reviewed_and_accepted",
    "nested_cwd_resolution_observed",
    "project_config_reload_observed",
    "package_launcher_observed",
    "sandbox_approval_observed",
    "form_accept_observed",
    "form_decline_observed",
    "form_cancel_observed",
    "form_timeout_observed",
    "foreign_project_config_rejected",
    "foreign_project_root_rejected",
    "foreign_project_cwd_rejected",
    "package_digest_mismatch_rejected_before_launch",
    "package_version_mismatch_rejected_before_launch",
    "cross_project_fallback_absent",
    "developer_bypass_absent",
)

OBSERVATION_PROMPTS = {
    "actual_surface_session_observed": (
        "Did you personally observe the actual selected Codex surface session?"
    ),
    "official_host_process_observed": (
        "Did you verify that the session used the official host process?"
    ),
    "recorder_stdio_bound_to_host_session": (
        "Did you observe the recorder stdio bound to that host session?"
    ),
    "exact_project_root_observed": (
        "Did you verify the exact intended project root?"
    ),
    "project_trust_reviewed_and_accepted": (
        "Did you personally review that exact project root in the official "
        "host and manually accept its Trust request?"
    ),
    "nested_cwd_resolution_observed": (
        "Did you verify nested-CWD resolution returned to that project root?"
    ),
    "project_config_reload_observed": (
        "Did you observe the project configuration reload take effect?"
    ),
    "package_launcher_observed": (
        "Did you verify the exact package-owned launcher was used?"
    ),
    "sandbox_approval_observed": (
        "Did you observe the independent Codex sandbox approval layer?"
    ),
    "form_accept_observed": (
        "Did you personally observe the action-only form accept path?"
    ),
    "form_decline_observed": (
        "Did you personally observe the action-only form decline path?"
    ),
    "form_cancel_observed": (
        "Did you personally observe the action-only form cancel path?"
    ),
    "form_timeout_observed": (
        "Did you personally observe the action-only form timeout path?"
    ),
    "foreign_project_config_rejected": (
        "Did you verify a foreign-project config was rejected?"
    ),
    "foreign_project_root_rejected": (
        "Did you verify a foreign project root was rejected?"
    ),
    "foreign_project_cwd_rejected": (
        "Did you verify a foreign-project CWD was rejected?"
    ),
    "package_digest_mismatch_rejected_before_launch": (
        "Did you verify package digest mismatch was rejected before launch?"
    ),
    "package_version_mismatch_rejected_before_launch": (
        "Did you verify package version mismatch was rejected before launch?"
    ),
    "cross_project_fallback_absent": (
        "Did you verify there was no cross-project fallback?"
    ),
    "developer_bypass_absent": (
        "Did you verify no developer bypass was enabled?"
    ),
}

METADATA_BINDING_FIELDS = frozenset(
    {
        "package_source_commit",
        "package_manifest_sha256",
        "compatibility_matrix_sha256",
        "host_coordinate_profile_sha256",
        "project_fixture_sha256",
        "registry_sha256",
        "prompt_pack_sha256",
        "host_artifact_sha256",
        "client_artifact_sha256",
        "host_provenance_sha256",
        "mcp_binary_sha256",
        "godot_artifact_sha256",
    }
)
INTERNAL_PACKAGE_FIELDS = frozenset(
    {
        "build_provenance",
        "checksums_path",
        "compatibility_matrix_sha256",
        "contents",
        "godot_prerequisite",
        "package_version",
        "registry_sha256",
        "schema_version",
        "source_commit",
        "target",
        "third_party_licenses_sha256",
    }
)


class SurfaceArtifactError(RuntimeError):
    """An input cannot safely produce a qualifying surface artifact."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SurfaceArtifactError(message)


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
        raise SurfaceArtifactError("document is not canonical JSON") from error


def canonical_artifact(value: Mapping[str, Any]) -> bytes:
    return canonical_json(value) + b"\n"


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def _strict_json(data: bytes, *, label: str, maximum: int) -> dict[str, Any]:
    require(len(data) <= maximum, f"{label} exceeds its byte bound")

    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in items:
            require(key not in result, f"{label} has a duplicate member")
            result[key] = value
        return result

    def reject_constant(_value: str) -> None:
        raise SurfaceArtifactError(f"{label} has a non-finite number")

    try:
        value = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=pairs,
            parse_constant=reject_constant,
        )
    except (UnicodeError, json.JSONDecodeError) as error:
        raise SurfaceArtifactError(f"{label} is invalid JSON") from error
    require(isinstance(value, dict), f"{label} root is not an object")
    return value


def _stable_stat(left: os.stat_result, right: os.stat_result) -> bool:
    fields = (
        "st_dev",
        "st_ino",
        "st_mode",
        "st_uid",
        "st_size",
        "st_mtime_ns",
        "st_ctime_ns",
    )
    return all(getattr(left, field) == getattr(right, field) for field in fields)


@dataclass(frozen=True)
class _ReadResult:
    payload: bytes
    identity: tuple[int, int, int, int, int, int, int]


def _identity(value: os.stat_result) -> tuple[int, int, int, int, int, int, int]:
    return (
        value.st_dev,
        value.st_ino,
        value.st_mode,
        value.st_uid,
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


def _canonical_path(path: Path, *, label: str, directory: bool) -> Path:
    require(path.is_absolute(), f"{label} path is not absolute")
    try:
        lexical = Path(os.path.abspath(path))
        resolved = path.resolve(strict=True)
        metadata = path.lstat()
    except OSError as error:
        raise SurfaceArtifactError(f"{label} path is unavailable") from error
    expected = stat.S_ISDIR if directory else stat.S_ISREG
    require(
        lexical == resolved
        and not stat.S_ISLNK(metadata.st_mode)
        and expected(metadata.st_mode),
        f"{label} path is not canonical and symlink-free",
    )
    return resolved


def _read_regular_once(path: Path, *, label: str, maximum: int) -> _ReadResult:
    path = _canonical_path(path, label=label, directory=False)
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise SurfaceArtifactError(f"{label} cannot be opened safely") from error
    chunks: list[bytes] = []
    total = 0
    try:
        before = os.fstat(descriptor)
        require(
            stat.S_ISREG(before.st_mode) and 0 <= before.st_size <= maximum,
            f"{label} identity or size differs",
        )
        while True:
            chunk = os.read(descriptor, min(1024 * 1024, maximum - total + 1))
            if not chunk:
                break
            total += len(chunk)
            require(total <= maximum, f"{label} exceeds its byte bound")
            chunks.append(chunk)
        after = os.fstat(descriptor)
        current = path.lstat()
        require(
            total == before.st_size
            and _stable_stat(before, after)
            and _stable_stat(before, current),
            f"{label} changed or was swapped while reading",
        )
        return _ReadResult(b"".join(chunks), _identity(before))
    except OSError as error:
        raise SurfaceArtifactError(f"{label} cannot be read safely") from error
    finally:
        os.close(descriptor)


def stable_read_file(path: Path, *, label: str, maximum: int) -> bytes:
    """Read one regular file twice through O_NOFOLLOW and reject instability."""

    first = _read_regular_once(path, label=label, maximum=maximum)
    second = _read_regular_once(path, label=label, maximum=maximum)
    require(
        first == second,
        f"{label} changed between stable measurements",
    )
    return first.payload


def _relative_file(
    root: Path,
    relative: str,
    *,
    label: str,
    maximum: int,
) -> bytes:
    pure = PurePosixPath(relative)
    require(
        relative
        and not pure.is_absolute()
        and ".." not in pure.parts
        and "\\" not in relative
        and "\0" not in relative,
        f"{label} path is unsafe",
    )
    root = _canonical_path(root, label=f"{label} root", directory=True)
    current = root
    for part in pure.parts[:-1]:
        current /= part
        try:
            metadata = current.lstat()
        except OSError as error:
            raise SurfaceArtifactError(f"{label} ancestor is unavailable") from error
        require(
            stat.S_ISDIR(metadata.st_mode) and not stat.S_ISLNK(metadata.st_mode),
            f"{label} ancestor is not a symlink-free directory",
        )
    candidate = root.joinpath(*pure.parts)
    return stable_read_file(candidate, label=label, maximum=maximum)


def _canonical_input_json(
    path: Path,
    *,
    label: str,
    maximum: int = MAX_JSON_BYTES,
) -> tuple[dict[str, Any], bytes]:
    payload = stable_read_file(path, label=label, maximum=maximum)
    document = _strict_json(payload, label=label, maximum=maximum)
    return document, payload


def _digest(value: Any, *, label: str) -> str:
    require(
        isinstance(value, str) and DIGEST_RE.fullmatch(value) is not None,
        f"{label} digest differs",
    )
    return cast(str, value)


def _commit(value: Any, *, label: str) -> str:
    require(
        isinstance(value, str) and COMMIT_RE.fullmatch(value) is not None,
        f"{label} commit differs",
    )
    return cast(str, value)


def _exact(value: Any, fields: set[str] | frozenset[str], *, label: str) -> dict[str, Any]:
    require(
        isinstance(value, dict) and set(value) == set(fields),
        f"{label} fields differ",
    )
    return cast(dict[str, Any], value)


def _git_environment() -> dict[str, str]:
    return {
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_OPTIONAL_LOCKS": "0",
        "LANG": "C",
        "LC_ALL": "C",
        "PATH": "/usr/bin:/bin",
    }


def _git_blob(repository: Path, commit: str, relative: str) -> bytes:
    repository = _canonical_path(
        repository,
        label="repository root",
        directory=True,
    )
    _commit(commit, label="package source")
    pure = PurePosixPath(relative)
    require(
        relative
        and not pure.is_absolute()
        and ".." not in pure.parts
        and "\\" not in relative
        and "\0" not in relative,
        "Git blob path is unsafe",
    )
    try:
        result = subprocess.run(
            [
                "/usr/bin/git",
                "--no-replace-objects",
                "-C",
                str(repository),
                "cat-file",
                "blob",
                f"{commit}:{relative}",
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=GIT_TIMEOUT_SECONDS,
            env=_git_environment(),
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise SurfaceArtifactError("Git source blob cannot be read") from error
    require(
        result.returncode == 0
        and len(result.stdout) <= MAX_GIT_BLOB_BYTES
        and len(result.stderr) <= 64 * 1024,
        f"Git source blob is unavailable: {relative}",
    )
    return result.stdout


def _git_json(repository: Path, commit: str, relative: str) -> tuple[dict[str, Any], bytes]:
    payload = _git_blob(repository, commit, relative)
    document = _strict_json(
        payload,
        label=f"Git blob {relative}",
        maximum=MAX_GIT_BLOB_BYTES,
    )
    return document, payload


def _signature(value: Any, *, label: str) -> dict[str, Any] | None:
    if value is None:
        return None
    record = _exact(
        value,
        {"verified", "mode", "identifier", "team_id", "cdhash"},
        label=label,
    )
    require(
        record["verified"] is True
        and record["mode"] in {"strict", "deep_strict"}
        and all(
            isinstance(record[field], str)
            and 1 <= len(record[field].encode("utf-8")) <= 128
            for field in ("identifier", "team_id", "cdhash")
        ),
        f"{label} differs",
    )
    return record


def _expected_signature(value: Any, *, label: str) -> dict[str, Any] | None:
    if value is None:
        return None
    record = _exact(
        value,
        {"mode", "identifier", "team_id", "cdhash"},
        label=label,
    )
    require(
        record["mode"] in {"strict", "deep_strict"}
        and all(
            isinstance(record[field], str)
            and 1 <= len(record[field].encode("utf-8")) <= 128
            for field in ("identifier", "team_id", "cdhash")
        ),
        f"{label} differs",
    )
    return record


def _validate_measurement(
    measurement: Mapping[str, Any],
    *,
    profile: Mapping[str, Any],
    profile_sha256: str,
    surface: str,
) -> tuple[dict[str, Any], dict[str, Any]]:
    root = _exact(
        measurement,
        {
            "schema_version",
            "capture_kind",
            "status",
            "architecture",
            "host_coordinate_profile_sha256",
            "surfaces",
            "redaction",
        },
        label="host provenance measurement",
    )
    require(
        root["schema_version"] == "s11-host-provenance/1.0"
        and root["capture_kind"] == "measured_local_host_provenance"
        and root["status"] == "passed"
        and root["architecture"] == "arm64"
        and root["host_coordinate_profile_sha256"] == profile_sha256,
        "host provenance measurement authority differs",
    )
    redaction = _exact(
        root["redaction"],
        {
            "absolute_paths_absent",
            "account_identity_absent",
            "artifact_content_absent",
            "environment_secrets_absent",
        },
        label="host provenance redaction",
    )
    require(
        all(value is True for value in redaction.values()),
        "host provenance redaction differs",
    )
    coordinates = profile.get("surfaces")
    measurements = root["surfaces"]
    require(
        isinstance(coordinates, list)
        and isinstance(measurements, list)
        and len(coordinates) == len(measurements) == 3,
        "host surface coverage differs",
    )
    coordinate_map: dict[str, Mapping[str, Any]] = {}
    measured_map: dict[str, Mapping[str, Any]] = {}
    for coordinate in coordinates:
        require(isinstance(coordinate, dict), "host coordinate differs")
        name = coordinate.get("surface")
        require(
            name in {"app", "cli", "ide"} and name not in coordinate_map,
            "host coordinate is missing or duplicated",
        )
        coordinate_map[cast(str, name)] = coordinate
    for item in measurements:
        require(isinstance(item, dict), "host measurement differs")
        name = item.get("surface")
        require(
            name in {"app", "cli", "ide"} and name not in measured_map,
            "host measurement is missing or duplicated",
        )
        measured_map[cast(str, name)] = item
    require(
        set(coordinate_map) == set(measured_map) == {"app", "cli", "ide"},
        "host surface set differs",
    )
    for name in ("app", "cli", "ide"):
        coordinate = coordinate_map[name]
        observed = measured_map[name]
        for field in (
            "host_artifact_sha256",
            "host_metadata_sha256",
            "client_artifact_sha256",
            "ide_shell_artifact_sha256",
        ):
            expected = coordinate.get(field)
            if expected is not None:
                _digest(expected, label=f"{name} {field}")
            require(
                observed.get(field) == expected,
                f"{name} measured {field} differs",
            )
        for field in (
            "host_code_signature",
            "client_code_signature",
            "ide_shell_code_signature",
        ):
            expected = _expected_signature(
                coordinate.get(field),
                label=f"{name} expected {field}",
            )
            actual = _signature(
                observed.get(field),
                label=f"{name} observed {field}",
            )
            require(
                actual is None
                if expected is None
                else {
                    key: actual.get(key)
                    for key in ("mode", "identifier", "team_id", "cdhash")
                }
                == expected,
                f"{name} measured {field} differs",
            )
    coordinate = coordinate_map[surface]
    observed = measured_map[surface]
    host = {
        "name": coordinate["host_name"],
        "identifier": coordinate["host_identifier"],
        "artifact_kind": coordinate["host_artifact_kind"],
        "version": coordinate["host_version"],
        "build": coordinate["host_build"],
        "commit": coordinate["host_commit"],
        "client_version": coordinate["client_version"],
        "host_metadata_sha256": observed["host_metadata_sha256"],
        "host_code_signature": observed["host_code_signature"],
        "client_code_signature": observed["client_code_signature"],
        "ide_host_version": coordinate["ide_host_version"],
        "ide_shell_identifier": coordinate["ide_shell_identifier"],
        "ide_shell_artifact_sha256": observed["ide_shell_artifact_sha256"],
        "ide_shell_team_id": coordinate["ide_shell_team_id"],
        "ide_shell_code_signature": observed["ide_shell_code_signature"],
        "architecture": "arm64",
    }
    return host, {
        "host_artifact_sha256": observed["host_artifact_sha256"],
        "client_artifact_sha256": observed["client_artifact_sha256"],
    }


def _manifest_records(value: Any) -> dict[str, dict[str, Any]]:
    require(
        isinstance(value, list) and 2 <= len(value) <= 512,
        "detached package contents differ",
    )
    records: dict[str, dict[str, Any]] = {}
    for item in value:
        record = _exact(
            item,
            {"bytes", "mode", "path", "sha256"},
            label="detached package content",
        )
        path = record["path"]
        require(
            isinstance(path, str)
            and path
            and not PurePosixPath(path).is_absolute()
            and ".." not in PurePosixPath(path).parts
            and "\\" not in path
            and path not in records,
            "detached package content path differs",
        )
        _digest(record["sha256"], label=f"package content {path}")
        require(
            isinstance(record["bytes"], int)
            and not isinstance(record["bytes"], bool)
            and 0 <= record["bytes"] <= MAX_FILE_BYTES
            and record["mode"] in {"0644", "0755"},
            f"package content metadata differs: {path}",
        )
        records[path] = record
    return records


def _verify_installed_package(
    *,
    detached: Mapping[str, Any],
    detached_sha256: str,
    data_root: Path,
) -> tuple[dict[str, Any], dict[str, bytes]]:
    version = detached.get("package_version")
    source_commit = detached.get("source_commit")
    require(
        isinstance(version, str) and VERSION_RE.fullmatch(version) is not None,
        "detached package version differs",
    )
    _commit(source_commit, label="detached package source")
    records = _manifest_records(detached.get("contents"))
    require(
        set(CAPTURE_REQUIRED_PACKAGE_MODES).issubset(records)
        and all(
            records[path]["mode"] == expected_mode
            for path, expected_mode in CAPTURE_REQUIRED_PACKAGE_MODES.items()
        ),
        "detached package omits a capture-required file or mode",
    )
    data_root = _canonical_path(data_root, label="package data root", directory=True)
    package_root = _canonical_path(
        data_root / "versions" / version,
        label="installed versioned package",
        directory=True,
    )
    require(
        package_root.parent.name == "versions" and package_root.name == version,
        "installed versioned package coordinate differs",
    )
    payloads: dict[str, bytes] = {}
    for path, record in sorted(records.items()):
        payload = _relative_file(
            package_root,
            path,
            label=f"installed package {path}",
            maximum=MAX_FILE_BYTES,
        )
        candidate = package_root.joinpath(*PurePosixPath(path).parts)
        mode = stat.S_IMODE(candidate.lstat().st_mode)
        require(
            len(payload) == record["bytes"]
            and sha256_bytes(payload) == record["sha256"]
            and mode == int(record["mode"], 8),
            f"installed package content differs: {path}",
        )
        payloads[path] = payload
    require(
        payloads.get("VERSION") == (version + "\n").encode("ascii")
        and payloads.get("SOURCE_COMMIT")
        == (cast(str, source_commit) + "\n").encode("ascii"),
        "installed package identity files differ",
    )
    internal_manifest = payloads.get("package-manifest.json")
    cli = payloads.get("bin/godot-codex")
    mcp = payloads.get("bin/godot-codex-mcp")
    require(
        internal_manifest is not None and cli is not None and mcp is not None,
        "installed package lacks required package/CLI/MCP files",
    )
    internal = _exact(
        _strict_json(
            internal_manifest,
            label="installed internal package manifest",
            maximum=MAX_JSON_BYTES,
        ),
        INTERNAL_PACKAGE_FIELDS,
        label="installed internal package manifest",
    )
    require(
        canonical_artifact(internal) == internal_manifest,
        "installed internal package manifest is not canonical",
    )
    internal_records = _manifest_records(internal["contents"])
    require(
        internal["schema_version"] == "godot-codex-package/1.0"
        and internal["package_version"] == version
        and internal["source_commit"] == source_commit
        and internal["checksums_path"] == "checksums.sha256"
        and internal["target"] == {"architecture": "arm64", "os": "macos"}
        and internal["compatibility_matrix_sha256"]
        == detached.get("compatibility_matrix_sha256")
        and internal["registry_sha256"] == detached.get("registry_sha256")
        and internal["third_party_licenses_sha256"]
        == detached.get("third_party_licenses_sha256")
        and isinstance(internal["build_provenance"], dict)
        and isinstance(internal["godot_prerequisite"], dict),
        "installed internal package identity/source binding differs",
    )
    require(
        set(CAPTURE_REQUIRED_PACKAGE_MODES).issubset(internal_records)
        and all(
            internal_records[path] == records[path]
            for path in CAPTURE_REQUIRED_PACKAGE_MODES
        ),
        "internal and detached capture-required records differ",
    )
    marker = _relative_file(
        package_root,
        ".godot-codex-owned",
        label="package ownership marker",
        maximum=128,
    )
    require(
        marker
        == (
            sha256_bytes(internal_manifest).removeprefix("sha256:") + "\n"
        ).encode("ascii"),
        "installed package ownership marker differs",
    )
    return {
        "version": version,
        "detached_manifest_sha256": detached_sha256,
        "internal_manifest_sha256": sha256_bytes(internal_manifest),
        "mcp_binary_sha256": sha256_bytes(mcp),
    }, payloads


def _registry_projection(
    registry: Mapping[str, Any],
    *,
    instructions: bytes,
) -> dict[str, Any]:
    require(
        registry.get("schema_version") == "godot-codex-registry-profile/1.0"
        and registry.get("tool_count") == 41
        and registry.get("fixed_resource_count") == 4
        and registry.get("resource_template_count") == 1,
        "Git-bound registry identity/count differs",
    )
    result: dict[str, Any] = {}
    for source, target, count in (
        ("tools", "tools", 41),
        ("fixed_resources", "fixed_resources", 4),
        ("resource_templates", "resource_templates", 1),
    ):
        values = registry.get(source)
        require(
            isinstance(values, list)
            and len(values) == count
            and values == sorted(set(values))
            and all(isinstance(item, str) for item in values),
            f"Git-bound registry {source} differs",
        )
        result[target] = values
    require(
        instructions.endswith(b"\n")
        and not instructions.endswith(b"\n\n")
        and b"\0" not in instructions,
        "Git-bound server instructions differ",
    )
    result["instructions_sha256"] = sha256_bytes(instructions[:-1])
    return result


def _verify_fixture(
    *,
    fixture_root: Path,
    repository: Path,
    source_commit: str,
) -> str:
    manifest, manifest_bytes = _git_json(
        repository,
        source_commit,
        FIXTURE_MANIFEST_PATH,
    )
    records = manifest.get("files")
    require(
        manifest.get("schema_version") == 1
        and isinstance(records, list)
        and records,
        "Git-bound fixture manifest differs",
    )
    fixture_root = _canonical_path(
        fixture_root,
        label="runtime fixture copy",
        directory=True,
    )
    projected: list[dict[str, str]] = []
    seen: set[str] = set()
    for item in records:
        record = _exact(item, {"path", "sha256"}, label="fixture file record")
        relative = record["path"]
        expected = record["sha256"]
        require(
            isinstance(relative, str)
            and relative
            and relative not in seen
            and isinstance(expected, str)
            and re.fullmatch(r"[0-9a-f]{64}", expected) is not None,
            "fixture file record differs",
        )
        seen.add(relative)
        source = _git_blob(
            repository,
            source_commit,
            f"{FIXTURE_SOURCE_ROOT}/{relative}",
        )
        require(
            hashlib.sha256(source).hexdigest() == expected,
            f"Git-bound fixture digest differs: {relative}",
        )
        copied = _relative_file(
            fixture_root,
            relative,
            label=f"runtime fixture copy {relative}",
            maximum=MAX_FIXTURE_FILE_BYTES,
        )
        require(copied == source, f"runtime fixture copy differs: {relative}")
        projected.append({"path": relative, "sha256": "sha256:" + expected})
    require(
        [item["path"] for item in projected] == sorted(seen),
        "fixture manifest paths are not canonical",
    )
    return sha256_bytes(
        FIXTURE_BINDING_DOMAIN
        + canonical_json(
            {
                "manifest_sha256": sha256_bytes(manifest_bytes),
                "files": projected,
            }
        )
    )


def _machine_bindings(
    *,
    fixture_root: Path,
    package: Mapping[str, Any],
) -> dict[str, str]:
    fixture_root = _canonical_path(
        fixture_root,
        label="runtime fixture copy",
        directory=True,
    )
    config = _relative_file(
        fixture_root,
        ".codex/config.toml",
        label="project Codex config",
        maximum=256 * 1024,
    )
    receipt_bytes = _relative_file(
        fixture_root,
        ".godot/codex/setup-receipt-v1.json",
        label="project setup receipt",
        maximum=256 * 1024,
    )
    receipt = _strict_json(
        receipt_bytes,
        label="project setup receipt",
        maximum=256 * 1024,
    )
    require(
        receipt.get("schema_version") == "godot-codex-setup-receipt/1.1"
        and receipt.get("package_version") == package["version"]
        and receipt.get("launcher_file_sha256")
        == package["mcp_binary_sha256"],
        "project setup receipt package binding differs",
    )
    canonical = os.fsencode(fixture_root)
    project_identity = sha256_bytes(PROJECT_IDENTITY_DOMAIN + canonical)
    return {
        "project_identity_sha256": project_identity,
        "package_launcher_sha256": cast(
            str,
            package["mcp_binary_sha256"],
        ),
        "project_config_sha256": sha256_bytes(config),
        "setup_receipt_sha256": sha256_bytes(receipt_bytes),
    }


def prepare_metadata(
    *,
    surface: str,
    package_manifest: Path,
    data_root: Path,
    measurement_path: Path,
    fixture_root: Path,
    repository_root: Path,
) -> dict[str, Any]:
    """Measure and bind every non-human input for one surface capture."""

    require(surface in {"app", "cli", "ide"}, "surface differs")
    detached, detached_bytes = _canonical_input_json(
        package_manifest,
        label="detached package manifest",
    )
    require(
        set(detached)
        == {
            "archive",
            "build_provenance",
            "compatibility_matrix_sha256",
            "contents",
            "godot_prerequisite",
            "package_version",
            "registry_sha256",
            "schema_version",
            "source_commit",
            "third_party_licenses_sha256",
        }
        and detached["schema_version"] == "s11-package-manifest/1.0",
        "detached package manifest fields/identity differ",
    )
    source_commit = _commit(
        detached["source_commit"],
        label="detached package source",
    )
    detached_sha256 = sha256_bytes(detached_bytes)
    package, installed = _verify_installed_package(
        detached=detached,
        detached_sha256=detached_sha256,
        data_root=data_root,
    )
    repository_root = _canonical_path(
        repository_root,
        label="repository root",
        directory=True,
    )
    registry, registry_bytes = _git_json(
        repository_root,
        source_commit,
        REGISTRY_PATH,
    )
    prompt, prompt_bytes = _git_json(
        repository_root,
        source_commit,
        PROMPT_PATH,
    )
    matrix, matrix_bytes = _git_json(
        repository_root,
        source_commit,
        MATRIX_PATH,
    )
    profile, profile_bytes = _git_json(
        repository_root,
        source_commit,
        PROFILE_PATH,
    )
    instructions = _git_blob(
        repository_root,
        source_commit,
        INSTRUCTIONS_PATH,
    )
    require(
        prompt.get("schema_version") == "s11-prompt-pack/1.0",
        "Git-bound prompt pack differs",
    )
    registry_sha256 = sha256_bytes(registry_bytes)
    matrix_sha256 = sha256_bytes(matrix_bytes)
    profile_sha256 = sha256_bytes(profile_bytes)
    prompt_sha256 = sha256_bytes(prompt_bytes)
    require(
        detached["registry_sha256"] == registry_sha256
        and detached["compatibility_matrix_sha256"] == matrix_sha256,
        "detached package Git product binding differs",
    )
    product_files = {
        REGISTRY_PATH: "share/godot-codex/product/registry-profile.v1.json",
        MATRIX_PATH: "share/godot-codex/product/compatibility-matrix.v1.json",
        PROFILE_PATH: "share/godot-codex/product/host-coordinate-profile.v1.json",
        INSTRUCTIONS_PATH: "share/godot-codex/product/server-instructions.v1.txt",
        **CAPTURE_SCHEMA_PACKAGE_PATHS,
    }
    git_payloads = {
        REGISTRY_PATH: registry_bytes,
        MATRIX_PATH: matrix_bytes,
        PROFILE_PATH: profile_bytes,
        INSTRUCTIONS_PATH: instructions,
        **{
            source_path: _git_blob(
                repository_root,
                source_commit,
                source_path,
            )
            for source_path in CAPTURE_SCHEMA_PACKAGE_PATHS
        },
    }
    for source_path, package_path in product_files.items():
        require(
            installed.get(package_path) == git_payloads[source_path],
            f"installed package product differs from Git: {source_path}",
        )
    prerequisite = detached.get("godot_prerequisite")
    require(
        isinstance(prerequisite, dict),
        "detached Godot prerequisite differs",
    )
    godot_sha256 = _digest(
        prerequisite.get("sha256"),
        label="Godot prerequisite",
    )
    require(
        matrix.get("package", {}).get("version") == package["version"]
        and matrix.get("godot", {}).get("artifact_sha256")
        == godot_sha256.removeprefix("sha256:"),
        "package/matrix version or Godot binding differs",
    )
    profile_matrix = profile.get("compatibility_matrix")
    profile_instructions = profile.get("server_instructions")
    require(
        isinstance(profile_matrix, dict)
        and profile_matrix.get("path") == MATRIX_PATH
        and profile_matrix.get("sha256") == matrix_sha256
        and isinstance(profile_instructions, dict)
        and profile_instructions.get("path") == INSTRUCTIONS_PATH
        and profile_instructions.get("file_sha256")
        == sha256_bytes(instructions)
        and profile_instructions.get("wire_sha256")
        == sha256_bytes(instructions[:-1]),
        "Git host profile product bindings differ",
    )
    measurement, measurement_bytes = _canonical_input_json(
        measurement_path,
        label="host provenance measurement",
        maximum=256 * 1024,
    )
    host, measured = _validate_measurement(
        measurement,
        profile=profile,
        profile_sha256=profile_sha256,
        surface=surface,
    )
    fixture_sha256 = _verify_fixture(
        fixture_root=fixture_root,
        repository=repository_root,
        source_commit=source_commit,
    )
    machine_bindings = _machine_bindings(
        fixture_root=fixture_root,
        package=package,
    )
    registry_projection = _registry_projection(
        registry,
        instructions=instructions,
    )
    bindings = {
        "package_source_commit": source_commit,
        "package_manifest_sha256": detached_sha256,
        "compatibility_matrix_sha256": matrix_sha256,
        "host_coordinate_profile_sha256": profile_sha256,
        "project_fixture_sha256": fixture_sha256,
        "registry_sha256": registry_sha256,
        "prompt_pack_sha256": prompt_sha256,
        "host_artifact_sha256": measured["host_artifact_sha256"],
        "client_artifact_sha256": measured["client_artifact_sha256"],
        "host_provenance_sha256": sha256_bytes(measurement_bytes),
        "mcp_binary_sha256": package["mcp_binary_sha256"],
        "godot_artifact_sha256": godot_sha256,
    }
    return {
        "schema_version": METADATA_SCHEMA_VERSION,
        "surface": surface,
        "package": package,
        "host": host,
        "bindings": bindings,
        "machine_bindings": machine_bindings,
        "registry": registry_projection,
        "redaction": {
            "absolute_paths_absent": True,
            "account_identity_absent": True,
            "host_controls_absent": True,
            "secrets_absent": True,
        },
    }


HOST_FIELDS = frozenset(
    {
        "name",
        "identifier",
        "artifact_kind",
        "version",
        "build",
        "commit",
        "client_version",
        "host_metadata_sha256",
        "host_code_signature",
        "client_code_signature",
        "ide_host_version",
        "ide_shell_identifier",
        "ide_shell_artifact_sha256",
        "ide_shell_team_id",
        "ide_shell_code_signature",
        "architecture",
    }
)
REGISTRY_PROJECTION_FIELDS = frozenset(
    {
        "tools",
        "fixed_resources",
        "resource_templates",
        "instructions_sha256",
    }
)
JOURNAL_FIELDS = frozenset(
    {
        "schema_version",
        "capture_kind",
        "status",
        "surface",
        "host",
        "bindings",
        "metadata_sha256",
        "protocol_version",
        "events",
        "semantic_projections",
        "transport_origin",
        "integrity",
        "redaction",
    }
)
JOURNAL_EVENT_FIELDS = frozenset(
    {
        "class",
        "direction",
        "error_code",
        "instructions_sha256",
        "kind",
        "method",
        "protocol_version",
        "registry_names",
        "seq",
        "tool",
        "tool_observation",
    }
)
RAW_CAPTURE_FIELDS = frozenset(
    {
        "schema_version",
        "outcome",
        "metadata_file",
        "metadata_sha256",
        "journal",
    }
)
RAW_JOURNAL_FIELDS = frozenset(
    {
        "schema_version",
        "run_id",
        "transport_origin",
        "surface",
        "project_identity",
        "metadata_sha256",
        "package_launcher_sha256",
        "project_config_sha256",
        "setup_receipt_sha256",
        "event_capacity",
        "observed_event_count",
        "dropped_event_count",
        "final_event_sha256",
        "events",
    }
)
RAW_EVENT_REQUIRED_FIELDS = frozenset(
    {
        "sequence",
        "observed_at_unix_ms",
        "direction",
        "class",
        "phase",
        "previous_event_sha256",
        "event_sha256",
    }
)
RAW_EVENT_OPTIONAL_FIELDS = frozenset(
    {
        "client_name",
        "client_version",
        "method",
        "tool",
        "registry_names",
        "protocol_version",
        "error_code",
        "instructions_sha256",
        "semantic_projection",
        "tool_observation",
    }
)

# This is the same closed top-level vocabulary used by
# crates/surface-capture/src/observation.rs.  A qualifying capture must not
# depend on an unsupported tool response or on a field that the Rust recorder
# did not deliberately classify as safe.
TOOL_REQUEST_FIELDS: dict[str, frozenset[str]] = {
    "godot_get_connection_status": frozenset(),
    "godot_get_current_scene": frozenset(
        {
            "expected_editor_session_id",
            "expected_event_seq",
            "expected_scene_revision",
        }
    ),
    "godot_get_scene_graph": frozenset({"scene", "limit", "cursor"}),
    "godot_run_project": frozenset(),
    "godot_run_current_scene": frozenset(),
    "godot_stop_project": frozenset(
        {"runtime_session_id", "expected_runtime_event_seq"}
    ),
    "godot_get_runtime_tree": frozenset(
        {
            "runtime_session_id",
            "expected_runtime_event_seq",
            "limit",
            "cursor",
        }
    ),
    "godot_get_stack_trace": frozenset(
        {
            "runtime_session_id",
            "runtime_stack_id",
            "expected_runtime_event_seq",
        }
    ),
    "godot_prepare_change_set": frozenset(
        {
            "project_id",
            "idempotency_key_redacted",
            "idempotency_key_digest",
            "coordinates",
            "operations",
            "save_scope",
            "validation_policy",
        }
    ),
    "godot_apply_transaction": frozenset(
        {
            "transaction_id",
            "preview_digest",
            "expected_scene_revision",
            "expected_operation_seq",
        }
    ),
    "godot_get_transaction_status": frozenset({"transaction_id"}),
    "godot_undo_transaction": frozenset(
        {
            "transaction_id",
            "expected_transaction_seq",
            "expected_scene_revision",
            "expected_operation_seq",
        }
    ),
    "godot_get_validation_report": frozenset({"report_id", "page"}),
}

TOOL_RESULT_FIELDS: dict[str, frozenset[str]] = {
    "godot_get_connection_status": frozenset(
        {
            "schema_version",
            "status",
            "project_scope",
            "package_version",
            "compatibility",
            "bridge",
            "static_cache",
            "recovery",
            "components",
            "diagnostic",
            "remediation_id",
            "next_action_redacted",
            "next_action_digest",
            "limits_applied",
            "evidence",
            "is_error",
            "error",
        }
    ),
    "godot_get_current_scene": frozenset(
        {
            "schema_version",
            "project_id",
            "editor_session_id",
            "snapshot_id",
            "revision_vector",
            "status",
            "freshness",
            "truncated",
            "evidence",
            "capabilities_used",
            "partial_reasons",
            "diagnostics",
            "limits_applied",
            "entities",
            "facts",
            "scene",
            "nodes",
            "is_error",
            "error",
        }
    ),
    "godot_get_scene_graph": frozenset(
        {
            "project_id",
            "schema_version",
            "generation_id",
            "index_revision",
            "resource_revision",
            "scene_graph_revision",
            "freshness",
            "offline_cached",
            "status",
            "scene",
            "query_redacted",
            "query_digest",
            "nodes",
            "project_context",
            "project_context_truncated",
            "diagnostics",
            "partial_reasons",
            "truncated",
            "next_cursor",
            "live_overlay",
            "conflicts",
            "validated_checkpoint",
            "evidence",
            "is_error",
            "error",
        }
    ),
    **{
        name: frozenset(
            {
                "schema_version",
                "project_id",
                "editor_session_id",
                "runtime_session_id",
                "runtime_event_seq",
                "state",
                "origin",
                "target",
                "scene_path",
                "is_error",
                "error",
            }
        )
        for name in (
            "godot_run_project",
            "godot_run_current_scene",
            "godot_stop_project",
        )
    },
    "godot_get_runtime_tree": frozenset(
        {
            "schema_version",
            "project_id",
            "editor_session_id",
            "runtime_session_id",
            "runtime_event_seq",
            "state",
            "snapshot_id",
            "limit",
            "offset",
            "total",
            "nodes",
            "truncated",
            "limits_applied",
            "next_cursor",
            "evidence",
            "is_error",
            "error",
        }
    ),
    "godot_get_stack_trace": frozenset(
        {
            "schema_version",
            "runtime_session_id",
            "runtime_event_seq",
            "state",
            "stack",
            "is_error",
            "error",
        }
    ),
    "godot_prepare_change_set": frozenset(
        {
            "schema_version",
            "change_set_id",
            "state",
            "transaction_seq",
            "preview_digest",
            "request_digest",
            "preview",
            "risk",
            "scope",
            "operation_count",
            "created_at_ms",
            "expires_at_ms",
            "limits_applied",
            "coordinates",
            "is_error",
            "error",
        }
    ),
    **{
        name: frozenset(
            {
                "schema_version",
                "transaction_id",
                "change_set_id",
                "editor_session_id",
                "scene_id",
                "state",
                "transaction_seq",
                "operation_kind",
                "risk",
                "scope",
                "preview_digest",
                "request_digest",
                "current_scene_revision",
                "current_operation_seq",
                "outcome",
                "undo_eligibility",
                "committed_entities",
                "updated_at_ms",
                "limits_applied",
                "truncated",
                "preview",
                "operation_count",
                "created_at_ms",
                "expires_at_ms",
                "coordinates",
                "postimage_digest",
                "validation_report_id",
                "terminal_error",
                "is_error",
                "error",
            }
        )
        for name in (
            "godot_apply_transaction",
            "godot_get_transaction_status",
            "godot_undo_transaction",
        )
    },
    "godot_get_validation_report": frozenset(
        {
            "schema_version",
            "report_id",
            "report_digest",
            "page",
            "page_count",
            "content_bytes",
            "limits_applied",
            "content_projection",
            "is_error",
            "error",
        }
    ),
}

OBSERVATION_SENSITIVE_FIELDS = frozenset(
    {
        "name",
        "value",
        "script",
        "script_ref",
        "binds",
        "replacement",
        "properties",
        "message",
        "safe_message",
        "summary",
        "query",
        "next_action",
        "idempotency_key",
        "fact_value",
        "source_text",
        "native_id",
        "object_id",
    }
)
OBSERVATION_OBJECT_FIELDS = frozenset(
    {
        "project_scope",
        "compatibility",
        "bridge",
        "static_cache",
        "recovery",
        "components",
        "diagnostic",
        "evidence",
        "scene",
        "revision_vector",
        "stack",
        "source",
        "coordinates",
        "preview",
        "validation_report",
        "outcome",
        "error",
        "undo_eligibility",
        "limits_applied",
        "live_overlay",
        "save_scope",
        "validation_policy",
        "terminal_error",
    }
)
OBSERVATION_ARRAY_FIELDS = frozenset(
    {
        "nodes",
        "facts",
        "entities",
        "frames",
        "operations",
        "committed_entities",
        "partial_reasons",
        "capabilities_used",
        "conflicts",
        "paths",
        "checks",
        "capabilities",
        "evidence",
        "diagnostics",
    }
)
OBSERVATION_INTEGER_FIELDS = frozenset(
    {
        "line",
        "page",
        "page_count",
        "limit",
        "offset",
        "total",
        "flags",
        "unbinds",
        "insertion_index",
        "start_byte",
        "end_byte",
    }
)
OBSERVATION_BOOLEAN_FIELDS = frozenset(
    {
        "retryable",
        "truncated",
        "keep_global_transform",
        "eligible",
        "project_context_truncated",
        "matched",
        "affected_closure_only",
        "available",
        "visible",
        "visible_in_tree",
        # Added directly by diagnostic_projection rather than project_field.
        "redacted",
    }
)
OBSERVATION_TOKEN_FIELDS = frozenset(
    {
        "schema_version",
        "status",
        "state",
        "outcome",
        "condition",
        "risk",
        "scope",
        "freshness",
        "confidence",
        "remediation_id",
        "project_scope",
        "recovery",
        "editor",
        "transactions",
        "authority",
        "check",
        "code",
        "severity",
        "source",
        "schema",
        "generation",
        "negotiated_protocol",
        "kind",
        "operation_kind",
        "godot_type",
        "property_name",
        "signal_name",
        "method_name",
        "origin",
        "target",
        "reason",
        "terminal_reason",
        "cursor",
        "next_cursor",
        "fact_code",
        "rollback",
        "warnings",
        "runtime",
        "resource_class",
        "alias",
        # Injected beside the redacted WritableVariant digest by the recorder.
        "value_type",
    }
)
PENDING_TRACE_FIELDS = frozenset(
    {
        "schema_version",
        "capture_kind",
        "status",
        "surface",
        "host",
        "bindings",
        "registry",
        "assertions",
        "form_outcomes",
        "revision_timeline",
        "redaction",
        "attestation_requirements",
    }
)
FINAL_TRACE_FIELDS = PENDING_TRACE_FIELDS - {"attestation_requirements"}


def _validate_host(value: Any, *, label: str) -> dict[str, Any]:
    host = _exact(value, HOST_FIELDS, label=label)
    for field in (
        "name",
        "identifier",
        "artifact_kind",
        "version",
        "build",
        "client_version",
    ):
        require(
            isinstance(host[field], str)
            and 1 <= len(host[field].encode("utf-8")) <= 128,
            f"{label} {field} differs",
        )
    for field in (
        "commit",
        "ide_host_version",
        "ide_shell_identifier",
        "ide_shell_team_id",
    ):
        require(
            host[field] is None
            or (
                isinstance(host[field], str)
                and len(host[field].encode("utf-8")) <= 128
            ),
            f"{label} {field} differs",
        )
    if host["commit"] is not None:
        _commit(host["commit"], label=f"{label} host")
    for field in ("host_metadata_sha256", "ide_shell_artifact_sha256"):
        if host[field] is not None:
            _digest(host[field], label=f"{label} {field}")
    for field in (
        "host_code_signature",
        "client_code_signature",
        "ide_shell_code_signature",
    ):
        _signature(host[field], label=f"{label} {field}")
    require(host["architecture"] == "arm64", f"{label} architecture differs")
    return host


def _validate_bindings(value: Any, *, label: str) -> dict[str, Any]:
    bindings = _exact(value, METADATA_BINDING_FIELDS, label=label)
    _commit(bindings["package_source_commit"], label=f"{label} source")
    for field in METADATA_BINDING_FIELDS - {"package_source_commit"}:
        _digest(bindings[field], label=f"{label} {field}")
    return bindings


def _validate_registry(value: Any, *, label: str) -> dict[str, Any]:
    registry = _exact(value, REGISTRY_PROJECTION_FIELDS, label=label)
    for field, count in (
        ("tools", 41),
        ("fixed_resources", 4),
        ("resource_templates", 1),
    ):
        items = registry[field]
        require(
            isinstance(items, list)
            and len(items) == count
            and items == sorted(set(items))
            and all(
                isinstance(item, str)
                and SEMANTIC_TOKEN_RE.fullmatch(item) is not None
                for item in items
            ),
            f"{label} {field} differs",
        )
    _digest(registry["instructions_sha256"], label=f"{label} instructions")
    return registry


def validate_metadata(value: Any) -> dict[str, Any]:
    metadata = _exact(
        value,
        {
            "schema_version",
            "surface",
            "package",
            "host",
            "bindings",
            "machine_bindings",
            "registry",
            "redaction",
        },
        label="surface metadata",
    )
    require(
        metadata["schema_version"] == METADATA_SCHEMA_VERSION
        and metadata["surface"] in {"app", "cli", "ide"},
        "surface metadata identity differs",
    )
    package = _exact(
        metadata["package"],
        {
            "version",
            "detached_manifest_sha256",
            "internal_manifest_sha256",
            "mcp_binary_sha256",
        },
        label="metadata package",
    )
    require(
        isinstance(package["version"], str)
        and VERSION_RE.fullmatch(package["version"]) is not None,
        "metadata package version differs",
    )
    for field in (
        "detached_manifest_sha256",
        "internal_manifest_sha256",
        "mcp_binary_sha256",
    ):
        _digest(package[field], label=f"metadata package {field}")
    host = _validate_host(metadata["host"], label="metadata host")
    bindings = _validate_bindings(metadata["bindings"], label="metadata bindings")
    machine_bindings = _exact(
        metadata["machine_bindings"],
        {
            "project_identity_sha256",
            "package_launcher_sha256",
            "project_config_sha256",
            "setup_receipt_sha256",
        },
        label="metadata machine bindings",
    )
    for field in machine_bindings:
        _digest(
            machine_bindings[field],
            label=f"metadata machine binding {field}",
        )
    registry = _validate_registry(metadata["registry"], label="metadata registry")
    redaction = _exact(
        metadata["redaction"],
        {
            "absolute_paths_absent",
            "account_identity_absent",
            "host_controls_absent",
            "secrets_absent",
        },
        label="metadata redaction",
    )
    require(
        all(value is True for value in redaction.values())
        and package["detached_manifest_sha256"]
        == bindings["package_manifest_sha256"]
        and package["mcp_binary_sha256"] == bindings["mcp_binary_sha256"],
        "metadata package/redaction binding differs",
    )
    require(
        machine_bindings["package_launcher_sha256"]
        == bindings["mcp_binary_sha256"],
        "metadata launcher machine binding differs",
    )
    acceptance.safe_evidence_scan(metadata)
    return {
        **metadata,
        "package": package,
        "host": host,
        "bindings": bindings,
        "machine_bindings": machine_bindings,
        "registry": registry,
    }


def _canonical_document(
    path: Path,
    *,
    label: str,
    maximum: int = MAX_JSON_BYTES,
) -> tuple[dict[str, Any], bytes]:
    document, payload = _canonical_input_json(
        path,
        label=label,
        maximum=maximum,
    )
    require(
        payload == canonical_artifact(document),
        f"{label} is not canonical JSON with one LF",
    )
    return document, payload


def load_metadata(path: Path) -> tuple[dict[str, Any], bytes]:
    value, payload = _canonical_document(path, label="surface metadata")
    return validate_metadata(value), payload


def recorder_event_chain_sha256(events: Sequence[Mapping[str, Any]]) -> str:
    state = hashlib.sha256(EVENT_CHAIN_DOMAIN).digest()
    for event in events:
        state = hashlib.sha256(state + b"\0" + canonical_json(event)).digest()
    return "sha256:" + state.hex()


def _bounded_integer(
    value: Any,
    *,
    label: str,
    minimum: int = 0,
    maximum: int = 9_007_199_254_740_991,
) -> int:
    require(
        isinstance(value, int)
        and not isinstance(value, bool)
        and minimum <= value <= maximum,
        f"{label} integer differs",
    )
    return cast(int, value)


def _validate_journal_events(
    value: Any,
    *,
    registry: Mapping[str, Any],
) -> tuple[
    list[dict[str, Any]],
    set[int],
    list[_ObservedResponse],
]:
    require(
        isinstance(value, list) and 1 <= len(value) <= MAX_EVENTS,
        "journal event count differs",
    )
    events: list[dict[str, Any]] = []
    semantic_sources: set[int] = set()
    response_methods: set[str] = set()
    registry_seen: set[str] = set()
    instructions_seen = False
    tool_response_seen = False
    form_seen = False
    observed_responses: list[_ObservedResponse] = []
    for index, item in enumerate(value):
        require(
            isinstance(item, dict)
            and set(item).issubset(JOURNAL_EVENT_FIELDS)
            and {
                "seq",
                "direction",
                "kind",
                "class",
                "registry_names",
            }.issubset(item),
            "journal event fields differ",
        )
        event = cast(dict[str, Any], item)
        require(
            event["seq"] == index
            and event["direction"] in {
                "client_to_server",
                "server_to_client",
            }
            and event["kind"] in {"request", "response", "notification"},
            "journal event identity/order differs",
        )
        require(
            event["class"]
            in {
                "initialize",
                "registry",
                "tool",
                "form",
                "status",
                "error",
                "protocol",
            }
            and isinstance(event["registry_names"], list)
            and len(event["registry_names"]) <= 256
            and all(
                isinstance(item, str)
                and SEMANTIC_TOKEN_RE.fullmatch(item) is not None
                for item in event["registry_names"]
            ),
            "journal event class/registry names differ",
        )
        method = event.get("method")
        if method is not None:
            require(
                isinstance(method, str)
                and SEMANTIC_TOKEN_RE.fullmatch(method) is not None,
                "journal method differs",
            )
        for field in (
            "tool",
            "protocol_version",
        ):
            if field in event:
                require(
                    isinstance(event[field], str)
                    and SEMANTIC_TOKEN_RE.fullmatch(event[field]) is not None,
                    f"journal event {field} differs",
                )
        if "error_code" in event:
            _bounded_integer(
                event["error_code"],
                label="journal event error code",
                minimum=-(1 << 31),
                maximum=(1 << 31) - 1,
            )
        if event["kind"] == "response":
            semantic_sources.add(index)
            if isinstance(method, str):
                response_methods.add(method)
                if method == "tools/call":
                    tool_response_seen = True
        if "instructions_sha256" in event:
            require(
                event["instructions_sha256"]
                == registry["instructions_sha256"],
                "journal server instructions differ",
            )
            instructions_seen = True
        expected_field = {
            "tools/list": "tools",
            "resources/list": "fixed_resources",
            "resources/templates/list": "resource_templates",
        }.get(cast(str | None, method))
        if event["kind"] == "response" and expected_field is not None:
            require(
                event["registry_names"] == registry[expected_field],
                f"journal registry {expected_field} differs",
            )
            registry_seen.add(expected_field)
        if event["class"] == "form":
            form_seen = True
        observation = event.get("tool_observation")
        if observation is not None:
            checked = _validate_tool_observation(
                {
                    "class": event["class"],
                    "phase": (
                        "error"
                        if event["class"] == "error"
                        else event["kind"]
                    ),
                    "tool": event.get("tool"),
                    "tool_observation": observation,
                }
            )
            require(
                checked is not None and event["kind"] == "response",
                "journal tool observation is not response-bound",
            )
            observed_responses.append(
                _ObservedResponse(
                    raw_sequence=index,
                    transport_sequence=index,
                    tool=(
                        "elicitation/create"
                        if event["class"] == "form"
                        else cast(str, event.get("tool"))
                    ),
                    phase=(
                        "error"
                        if event["class"] == "error"
                        else "response"
                    ),
                    request=cast(Mapping[str, Any], checked["request"]),
                    result=cast(Mapping[str, Any], checked["result"]),
                )
            )
        if (
            event["kind"] == "response"
            and (
                event.get("tool") in TOOL_REQUEST_FIELDS
                or event["class"] == "form"
            )
        ):
            require(
                observation is not None,
                "journal supported response lacks a tool observation",
            )
        events.append(event)
    require(
        {
            "initialize",
            "tools/list",
            "resources/list",
            "resources/templates/list",
        }.issubset(response_methods)
        and registry_seen == {"tools", "fixed_resources", "resource_templates"}
        and instructions_seen
        and tool_response_seen
        and form_seen,
        "journal omits required transport observations",
    )
    return events, semantic_sources, observed_responses


def _semantic_projection_map(
    value: Any,
    *,
    source_events: set[int],
) -> tuple[dict[str, Any], dict[str, int]]:
    require(
        isinstance(value, list)
        and len(value) == len(SEMANTIC_PROJECTION_IDS),
        "semantic projection count differs",
    )
    result: dict[str, Any] = {}
    sources: dict[str, int] = {}
    for item in value:
        record = _exact(
            item,
            {"projection_id", "source_event_seq", "value"},
            label="semantic projection",
        )
        projection_id = record["projection_id"]
        require(
            isinstance(projection_id, str)
            and projection_id in SEMANTIC_PROJECTION_IDS
            and projection_id not in result,
            "semantic projection is unknown, duplicated, or ambiguous",
        )
        source = _bounded_integer(
            record["source_event_seq"],
            label=f"{projection_id} source event",
            maximum=MAX_EVENTS - 1,
        )
        require(
            source in source_events,
            f"{projection_id} is not bound to a response event",
        )
        require(
            isinstance(record["value"], dict),
            f"{projection_id} value is not an object",
        )
        acceptance.safe_evidence_scan(record["value"])
        result[projection_id] = record["value"]
        sources[projection_id] = source
    require(
        set(result) == SEMANTIC_PROJECTION_IDS,
        "semantic projection coverage differs",
    )
    return result, sources


def _validate_transport_assertions(
    assertions: Mapping[str, Mapping[str, Any]],
) -> None:
    require(
        set(assertions) == TRANSPORT_ASSERTION_IDS,
        "transport assertion coverage differs",
    )
    acceptance._validate_approval_assertions(assertions)
    acceptance._validate_connection_assertion(assertions["connection.status"])
    acceptance._validate_multi_project_assertion(
        assertions["multi_project.reject"]
    )
    acceptance._validate_offline_assertion(assertions["offline.saved_query"])
    acceptance._validate_runtime_assertion(assertions["runtime.error_stack"])
    acceptance._validate_saved_assertion(assertions["saved.current_scene"])
    offline = _exact(
        assertions["host.offline_status"],
        {
            "state",
            "freshness",
            "live_state_claimed",
            "remediation_id",
        },
        label="host.offline_status",
    )
    require(
        offline
        == {
            "state": "offline",
            "freshness": "offline_cached",
            "live_state_claimed": False,
            "remediation_id": "start_matching_editor",
        },
        "host offline assertion differs",
    )
    acceptance._validate_preview(assertions)
    (
        apply_editor,
        apply_transaction,
        apply_revisions,
    ) = acceptance._validate_transaction_result(
        assertions["transaction.apply"],
        assertion_id="transaction.apply",
        expected_state="committed",
    )
    (
        undo_editor,
        undo_transaction,
        undo_revisions,
    ) = acceptance._validate_transaction_result(
        assertions["transaction.undo"],
        assertion_id="transaction.undo",
        expected_state="undone",
    )
    acceptance._validate_validation_assertion(assertions["validation.result"])
    preview = assertions["transaction.preview"]
    approved = assertions["approval.accept"]
    validation = assertions["validation.result"]
    require(
        apply_editor == undo_editor
        and apply_transaction
        == undo_transaction
        == preview["transaction_id"]
        == approved["transaction_id"]
        == validation["transaction_id"]
        and all(
            undo > applied
            for undo, applied in zip(undo_revisions, apply_revisions)
        ),
        "transaction preview/apply/validation/undo relation differs",
    )


def _validate_host_assertions(
    assertions: Mapping[str, Mapping[str, Any]],
    *,
    mcp_binary_sha256: str,
) -> None:
    require(
        set(assertions) == HOST_ATTESTED_ASSERTIONS,
        "host assertion coverage differs",
    )
    require(
        assertions["host.approval_layers"]
        == {
            "sandbox_approval": "observed",
            "mcp_approval": "action_only_form",
            "independent": True,
        }
        and assertions["host.config_reload"]
        == {
            "config_reloaded": True,
            "root_rebound": True,
            "restart_required": False,
        }
        and assertions["host.launcher"]
        == {
            "path": "bin/godot-codex-mcp",
            "sha256": mcp_binary_sha256,
            "path_lookup_used": False,
            "private_absolute_path_recorded": False,
        },
        "host-only attested assertion differs",
    )


def _projection_parts(
    projections: Mapping[str, Any],
) -> tuple[
    dict[str, Mapping[str, Any]],
    list[dict[str, Any]],
    list[dict[str, Any]],
]:
    assertions = {
        identifier.removeprefix("assertion."): cast(
            Mapping[str, Any],
            projections[identifier],
        )
        for identifier in projections
        if identifier.startswith("assertion.")
    }
    forms = [
        {
            **cast(
                dict[str, Any],
                projections[f"form_outcome.{scenario}"],
            )
        }
        for scenario in sorted(FORM_SCENARIOS)
    ]
    revisions = [
        {
            **cast(dict[str, Any], projections[f"revision.{step}"])
        }
        for step in (
            "initial",
            "prepared",
            "applied",
            "restarted",
            "stale_guard_rejected",
        )
    ]
    _validate_transport_assertions(assertions)
    acceptance._validate_form_outcomes(forms)
    acceptance._validate_revision_timeline(revisions)
    return assertions, forms, revisions


def _compact_json_in_order(value: Any) -> bytes:
    try:
        return json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=False,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise SurfaceArtifactError("raw capture is not compact JSON") from error


def load_raw_capture(path: Path) -> tuple[dict[str, Any], bytes]:
    payload = stable_read_file(
        path,
        label="private Rust capture journal",
        maximum=MAX_JSON_BYTES,
    )
    value = _strict_json(
        payload,
        label="private Rust capture journal",
        maximum=MAX_JSON_BYTES,
    )
    require(
        payload == _compact_json_in_order(value),
        "private Rust capture journal is not its exact compact encoding",
    )
    return value, payload


def _raw_event_hash(event: Mapping[str, Any]) -> str:
    payload: dict[str, Any] = {
        "sequence": event["sequence"],
        "direction": event["direction"],
        "class": event["class"],
        "phase": event["phase"],
        "method": event.get("method"),
        "tool": event.get("tool"),
        "registry_names": event.get("registry_names", []),
        "protocol_version": event.get("protocol_version"),
        "client_name": event.get("client_name"),
        "client_version": event.get("client_version"),
        "instructions_sha256": event.get("instructions_sha256"),
        "error_code": event.get("error_code"),
        "semantic_projection": event.get("semantic_projection"),
        "tool_observation": event.get("tool_observation"),
        "previous_event_sha256": event["previous_event_sha256"],
    }
    return sha256_bytes(RAW_EVENT_HASH_DOMAIN + _compact_json_in_order(payload))


def _safe_observation_token(value: Any) -> bool:
    if not isinstance(value, str):
        return False
    lower = value.lower()
    return (
        1 <= len(value) <= 512
        and not value.startswith("/")
        and not value.startswith("~/")
        and "\\" not in value
        and "://" not in value
        and "secret" not in lower
        and "password" not in lower
        and "token" not in lower
        and "authorization" not in lower
        and not lower.startswith("sk-")
        and all(
            character.isascii()
            and (
                character.isalnum()
                or character in "_-.:/@{}"
            )
            for character in value
        )
    )


def _safe_observation_path(value: Any) -> bool:
    if not isinstance(value, str):
        return False
    path = (
        value.removeprefix("res://")
        if value.startswith("res://")
        else value.removeprefix("scene:res://")
        if value.startswith("scene:res://")
        else None
    )
    return (
        path is not None
        and path
        and len(value) <= 1_024
        and not path.startswith("/")
        and "\\" not in path
        and not any(
            not segment or segment == ".."
            for segment in path.split("/")
        )
        and not any(
            ord(character) < 32 or ord(character) == 127
            for character in value
        )
    )


def _observation_digest_field(field: str) -> bool:
    return (
        field.endswith("_digest")
        or field.endswith("_sha256")
        or field in {"sha256", "expected_hash"}
    )


def _observation_integer_field(field: str) -> bool:
    return (
        field.endswith("_seq")
        or field.endswith("_revision")
        or field.endswith("_count")
        or field.endswith("_bytes")
        or field.endswith("_ms")
        or field in OBSERVATION_INTEGER_FIELDS
    )


def _observation_boolean_field(field: str) -> bool:
    return (
        field.startswith("is_")
        or field.endswith("_cached")
        or field.endswith("_claimed")
        or field.endswith("_observed")
        or field.endswith("_redacted")
        or field.endswith("_verified")
        or field in OBSERVATION_BOOLEAN_FIELDS
    )


def _observation_token_field(field: str) -> bool:
    return (
        field.endswith("_id")
        or field.endswith("_code")
        or field in OBSERVATION_TOKEN_FIELDS
    )


def _validate_validation_content_projection(
    value: Any,
    *,
    label: str,
) -> None:
    projection = _mapping(value, label=label)
    require(
        set(projection).issubset(
            {"transaction_id", "outcome", "checks", "diagnostics"}
        )
        and {"transaction_id", "outcome", "checks"}.issubset(projection)
        and _safe_observation_token(projection["transaction_id"])
        and projection["outcome"]
        in {"passed", "failed", "inconclusive", "timed_out"},
        f"{label} fields differ",
    )
    checks = _sequence(projection["checks"], label=f"{label} checks")
    require(len(checks) <= 64, f"{label} check count differs")
    for item in checks:
        check = _exact(
            item,
            {"check", "authority", "outcome"},
            label=f"{label} check",
        )
        require(
            check["check"]
            in {
                "intrinsic",
                "persistence",
                "reload_reparse",
                "index_convergence",
                "semantic_graph",
                "diagnostics",
                "runtime",
            }
            and check["authority"] in {"required", "optional"}
            and check["outcome"]
            in {
                "pending",
                "passed",
                "failed",
                "inconclusive",
                "timed_out",
                "skipped",
                "stale",
                "truncated",
            },
            f"{label} check differs",
        )
    diagnostics_value = projection.get("diagnostics")
    if diagnostics_value is None:
        return
    diagnostics = _mapping(
        diagnostics_value,
        label=f"{label} diagnostics",
    )
    require(
        set(diagnostics).issubset(
            {
                "pre_existing",
                "resolved",
                "introduced",
                "introduced_errors",
                "introduced_warnings",
            }
        ),
        f"{label} diagnostic fields differ",
    )
    for field in ("introduced_errors", "introduced_warnings"):
        if field in diagnostics:
            _bounded_integer(
                diagnostics[field],
                label=f"{label} {field}",
            )
    for field in ("pre_existing", "resolved", "introduced"):
        if field not in diagnostics:
            continue
        fingerprints = _sequence(
            diagnostics[field],
            label=f"{label} {field}",
        )
        require(len(fingerprints) <= 256, f"{label} {field} differs")
        for item in fingerprints:
            fingerprint = _mapping(
                item,
                label=f"{label} diagnostic fingerprint",
            )
            require(
                set(fingerprint).issubset(
                    {
                        "fingerprint_sha256",
                        "severity",
                        "id",
                        "entity_id",
                        "message_digest",
                    }
                )
                and {"fingerprint_sha256", "severity"}.issubset(
                    fingerprint
                )
                and fingerprint["severity"] in {"warning", "error"},
                f"{label} diagnostic fingerprint differs",
            )
            _digest(
                fingerprint["fingerprint_sha256"],
                label=f"{label} diagnostic fingerprint",
            )
            if "message_digest" in fingerprint:
                _digest(
                    fingerprint["message_digest"],
                    label=f"{label} diagnostic message",
                )
            for identity in ("id", "entity_id"):
                if identity in fingerprint:
                    require(
                        _safe_observation_token(fingerprint[identity]),
                        f"{label} diagnostic identity differs",
                    )


def _validate_observation_member(
    field: str,
    value: Any,
    *,
    label: str,
    depth: int,
) -> None:
    require(
        depth <= 8 and value is not None,
        f"{label} depth/null differs",
    )
    if field == "content_projection":
        _validate_validation_content_projection(value, label=label)
        return
    if field == "scene_revisions":
        revisions = _mapping(value, label=label)
        require(len(revisions) <= 256, f"{label} count differs")
        for identity, revision in revisions.items():
            require(
                _safe_observation_token(identity)
                or _safe_observation_path(identity),
                f"{label} key differs",
            )
            _bounded_integer(revision, label=f"{label} revision")
        return
    if isinstance(value, dict) and field in OBSERVATION_OBJECT_FIELDS:
        require(len(value) <= 256, f"{label} object bound differs")
        for child, child_value in value.items():
            _validate_observation_member(
                child,
                child_value,
                label=f"{label}.{child}",
                depth=depth + 1,
            )
        return
    if isinstance(value, list) and field in OBSERVATION_ARRAY_FIELDS:
        require(len(value) <= 256, f"{label} array bound differs")
        for index, item in enumerate(value):
            item_label = f"{label}[{index}]"
            if isinstance(item, dict):
                require(len(item) <= 256, f"{item_label} object bound differs")
                for child, child_value in item.items():
                    _validate_observation_member(
                        child,
                        child_value,
                        label=f"{item_label}.{child}",
                        depth=depth + 1,
                    )
            else:
                require(
                    _safe_observation_token(item)
                    or _safe_observation_path(item)
                    or (
                        isinstance(item, int)
                        and not isinstance(item, bool)
                        and 0 <= item <= 9_007_199_254_740_991
                    ),
                    f"{item_label} differs",
                )
        return
    if _observation_digest_field(field):
        _digest(value, label=label)
        return
    if field.endswith("_path") or field in {
        "path",
        "scene",
        "resource",
        "resolved_path",
        "source_path",
    }:
        require(_safe_observation_path(value), f"{label} path differs")
        return
    if _observation_integer_field(field):
        _bounded_integer(value, label=label)
        return
    if _observation_boolean_field(field):
        require(isinstance(value, bool), f"{label} boolean differs")
        return
    if _observation_token_field(field):
        require(
            _safe_observation_token(value)
            or (field.endswith("_id") and _safe_observation_path(value)),
            f"{label} token differs",
        )
        return
    raise SurfaceArtifactError(f"{label} is not in the safe observation vocabulary")


def _validate_tool_observation(
    event: Mapping[str, Any],
) -> dict[str, Any] | None:
    value = event.get("tool_observation")
    if value is None:
        return None
    observation = _exact(
        value,
        {"request", "result"},
        label="raw tool observation",
    )
    request = observation["request"]
    result = observation["result"]
    require(
        isinstance(request, dict)
        and isinstance(result, dict)
        and len(canonical_json(observation)) <= 64 * 1024,
        "raw tool observation shape or byte bound differs",
    )
    if event["class"] == "form":
        require(
            event["phase"] == "response"
            and set(request) == {"parent_tool", "transaction_id"}
            and request["parent_tool"] == "godot_apply_transaction"
            and isinstance(request["transaction_id"], str)
            and set(result) == {"action", "content_recorded"}
            and result["action"] in {"accept", "decline", "cancel"}
            and result["content_recorded"] is False,
            "raw form observation differs",
        )
    else:
        tool = event.get("tool")
        request_allowed = set(
            TOOL_REQUEST_FIELDS.get(cast(str, tool), frozenset())
        )
        result_allowed = set(
            TOOL_RESULT_FIELDS.get(cast(str, tool), frozenset())
        )
        for allowed in (request_allowed, result_allowed):
            for field in tuple(allowed & OBSERVATION_SENSITIVE_FIELDS):
                allowed.remove(field)
                allowed.add(f"{field}_redacted")
                allowed.add(f"{field}_digest")
        require(
            isinstance(tool, str)
            and tool in TOOL_REQUEST_FIELDS
            and set(request).issubset(request_allowed)
            and set(result).issubset(result_allowed),
            "raw tool observation is not in the closed tool/field allowlist",
        )
        for container_label, container in (
            ("raw tool request observation", request),
            ("raw tool result observation", result),
        ):
            for field, member in container.items():
                _validate_observation_member(
                    field,
                    member,
                    label=f"{container_label}.{field}",
                    depth=0,
                )
    acceptance.safe_evidence_scan(observation)
    return observation


def _validate_raw_event(
    value: Any,
    *,
    index: int,
    previous: str,
) -> dict[str, Any]:
    require(
        isinstance(value, dict)
        and RAW_EVENT_REQUIRED_FIELDS.issubset(value)
        and set(value).issubset(
            RAW_EVENT_REQUIRED_FIELDS | RAW_EVENT_OPTIONAL_FIELDS
        ),
        "raw capture event fields differ",
    )
    event = cast(dict[str, Any], value)
    require(
        event["sequence"] == index + 1
        and event["direction"] in {"client_to_server", "server_to_client"}
        and event["class"]
        in {
            "initialize",
            "registry",
            "tool",
            "form",
            "status",
            "error",
            "protocol",
            "semantic_projection",
        }
        and event["phase"]
        in {"request", "response", "notification", "error", "projection"}
        and event["previous_event_sha256"] == previous,
        "raw capture event sequence/chain identity differs",
    )
    _bounded_integer(
        event["observed_at_unix_ms"],
        label="raw capture observation time",
        minimum=1,
        maximum=(1 << 63) - 1,
    )
    _digest(event["previous_event_sha256"], label="raw previous event")
    _digest(event["event_sha256"], label="raw event")
    for field in (
        "method",
        "tool",
        "protocol_version",
        "client_name",
        "client_version",
    ):
        if field in event:
            require(
                isinstance(event[field], str)
                and SEMANTIC_TOKEN_RE.fullmatch(event[field]) is not None,
                f"raw capture event {field} differs",
            )
    names = event.get("registry_names", [])
    require(
        isinstance(names, list)
        and len(names) <= 256
        and all(
            isinstance(item, str)
            and SEMANTIC_TOKEN_RE.fullmatch(item) is not None
            for item in names
        ),
        "raw capture registry names differ",
    )
    if "instructions_sha256" in event:
        _digest(
            event["instructions_sha256"],
            label="raw capture instructions",
        )
    if "error_code" in event:
        _bounded_integer(
            event["error_code"],
            label="raw capture error code",
            minimum=-(1 << 31),
            maximum=(1 << 31) - 1,
        )
    projection = event.get("semantic_projection")
    if event["class"] == "semantic_projection":
        require(
            event["phase"] == "projection"
            and event["direction"] == "server_to_client"
            and isinstance(projection, dict)
            and set(projection)
            == {"projection_id", "source_event_seq", "value"}
            and not any(
                field in event
                for field in (
                    "method",
                    "tool",
                    "protocol_version",
                    "error_code",
                    "instructions_sha256",
                )
            )
            and names == [],
            "raw semantic projection event differs",
        )
    else:
        require(
            projection is None,
            "raw transport event contains a semantic projection",
        )
    observation = _validate_tool_observation(event)
    if observation is not None:
        require(
            event["phase"] in {"response", "error"}
            and event["class"] != "semantic_projection",
            "raw tool observation is not response-bound",
        )
    if (
        event["phase"] in {"response", "error"}
        and (
            event.get("tool") in TOOL_REQUEST_FIELDS
            or event["class"] == "form"
        )
    ):
        require(
            observation is not None,
            "supported raw response lacks an automatic tool observation",
        )
    require(
        event["event_sha256"] == _raw_event_hash(event),
        "raw capture event hash differs",
    )
    return event


@dataclass(frozen=True)
class _ObservedResponse:
    raw_sequence: int
    transport_sequence: int
    tool: str
    phase: str
    request: Mapping[str, Any]
    result: Mapping[str, Any]


def _semantic_identity(value: Any, *, prefix: str, label: str) -> str:
    require(
        isinstance(value, str)
        and 1 <= len(value.encode("utf-8")) <= 1_024,
        f"{label} identity differs",
    )
    if value.startswith(prefix):
        require(
            SEMANTIC_TOKEN_RE.fullmatch(value) is not None,
            f"{label} semantic identity differs",
        )
        return value
    return (
        prefix
        + "sha256:"
        + hashlib.sha256(
            b"godot-codex/s11-semantic-identity/v1\0"
            + prefix.encode("ascii")
            + b"\0"
            + value.encode("utf-8")
        ).hexdigest()
    )


def _mapping(value: Any, *, label: str) -> Mapping[str, Any]:
    require(isinstance(value, dict), f"{label} is not an object")
    return cast(Mapping[str, Any], value)


def _sequence(value: Any, *, label: str) -> Sequence[Any]:
    require(isinstance(value, list), f"{label} is not an array")
    return cast(Sequence[Any], value)


def _first_present(
    root: Mapping[str, Any],
    paths: Sequence[tuple[str, ...]],
    *,
    label: str,
) -> Any:
    values: list[Any] = []
    for path in paths:
        current: Any = root
        for part in path:
            if not isinstance(current, dict) or part not in current:
                break
            current = current[part]
        else:
            if current is not None:
                values.append(current)
    require(values, f"{label} is missing")
    encoded = {canonical_json(value) for value in values}
    require(len(encoded) == 1, f"{label} is ambiguous")
    return values[0]


def _optional_present(
    root: Mapping[str, Any],
    paths: Sequence[tuple[str, ...]],
    *,
    label: str,
) -> Any | None:
    values: list[Any] = []
    for path in paths:
        current: Any = root
        for part in path:
            if not isinstance(current, dict) or part not in current:
                break
            current = current[part]
        else:
            if current is not None:
                values.append(current)
    if not values:
        return None
    encoded = {canonical_json(value) for value in values}
    require(len(encoded) == 1, f"{label} is ambiguous")
    return values[0]


def _candidate(
    values: Sequence[tuple[int, int, dict[str, Any]]],
    *,
    label: str,
    prefer_latest: bool = False,
) -> tuple[int, int, dict[str, Any]]:
    require(values, f"{label} observation is missing")
    groups: dict[bytes, list[tuple[int, int, dict[str, Any]]]] = {}
    for item in values:
        groups.setdefault(canonical_json(item[2]), []).append(item)
    require(len(groups) == 1, f"{label} observation is ambiguous")
    ordered = sorted(next(iter(groups.values())), key=lambda item: item[0])
    return ordered[-1] if prefer_latest else ordered[0]


def _raw_transaction_id(observation: _ObservedResponse) -> str:
    value = _optional_present(
        observation.result,
        (
            ("transaction_id",),
            ("change_set_id",),
        ),
        label=f"{observation.tool} result transaction",
    )
    request_value = observation.request.get("transaction_id")
    require(
        value is not None or request_value is not None,
        f"{observation.tool} transaction is missing",
    )
    if value is None:
        value = request_value
    elif request_value is not None:
        require(
            request_value == value,
            f"{observation.tool} request/result transaction differs",
        )
    require(
        isinstance(value, str),
        f"{observation.tool} transaction differs",
    )
    return cast(str, value)


def _error_projection(result: Mapping[str, Any]) -> Mapping[str, Any] | None:
    error = result.get("error")
    if error is None:
        return None
    return _mapping(error, label="tool error observation")


def _error_code(result: Mapping[str, Any]) -> str | None:
    error = _error_projection(result)
    if error is None:
        return None
    value = _optional_present(
        error,
        (("code",), ("error_code",)),
        label="tool semantic error code",
    )
    require(
        value is None or isinstance(value, str),
        "tool semantic error code differs",
    )
    return cast(str | None, value)


def _coordinates_from(
    result: Mapping[str, Any],
    *,
    label: str,
    fallback: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    sources: list[Mapping[str, Any]] = [result]
    for field in ("coordinates", "revision_vector"):
        value = result.get(field)
        if isinstance(value, dict):
            sources.append(cast(Mapping[str, Any], value))
    if fallback is not None:
        sources.append(fallback)
        coordinates = fallback.get("coordinates")
        if isinstance(coordinates, dict):
            sources.append(cast(Mapping[str, Any], coordinates))

    def collect(names: Sequence[str]) -> Any:
        values: list[Any] = []
        for source in sources:
            for name in names:
                value = source.get(name)
                if value is not None:
                    values.append(value)
        require(values, f"{label} {names[0]} is missing")
        encoded = {canonical_json(value) for value in values}
        require(len(encoded) == 1, f"{label} {names[0]} is ambiguous")
        return values[0]

    editor = collect(("editor_session_id",))
    result_value = {
        "editor_session_id": _semantic_identity(
            editor,
            prefix="editor-session:",
            label=f"{label} editor",
        ),
        "event_seq": _bounded_integer(
            collect(("event_seq",)),
            label=f"{label} event_seq",
        ),
        "scene_revision": _bounded_integer(
            collect(("scene_revision", "current_scene_revision")),
            label=f"{label} scene_revision",
        ),
        "operation_seq": _bounded_integer(
            collect(("operation_seq", "current_operation_seq")),
            label=f"{label} operation_seq",
        ),
    }
    return result_value


def _live_scene_coordinates(
    result: Mapping[str, Any],
    *,
    scene_identity: str,
    label: str,
) -> dict[str, Any]:
    revision = _mapping(
        result.get("revision_vector"),
        label=f"{label} revision vector",
    )
    editor = _first_present(
        result,
        (
            ("editor_session_id",),
            ("revision_vector", "editor_session_id"),
        ),
        label=f"{label} editor",
    )
    coordinates: dict[str, Any] = {
        "editor_session_id": _semantic_identity(
            editor,
            prefix="editor-session:",
            label=f"{label} editor",
        ),
        "event_seq": _bounded_integer(
            revision.get("event_seq"),
            label=f"{label} event_seq",
        ),
        "operation_seq": _bounded_integer(
            revision.get("operation_seq"),
            label=f"{label} operation_seq",
        ),
    }
    scene_revision = revision.get("scene_revision")
    if scene_revision is None:
        scene_revisions = revision.get("scene_revisions")
        if isinstance(scene_revisions, dict):
            candidates = [
                value
                for key, value in scene_revisions.items()
                if _semantic_identity(
                    key,
                    prefix="scene:",
                    label=f"{label} scene revision key",
                )
                == scene_identity
            ]
            require(
                len(candidates) <= 1,
                f"{label} scene revision is ambiguous",
            )
            if candidates:
                scene_revision = candidates[0]
    if scene_revision is None:
        scene = result.get("scene")
        if isinstance(scene, dict):
            scene_revision = scene.get("scene_revision")
    if scene_revision is not None:
        coordinates["scene_revision"] = _bounded_integer(
            scene_revision,
            label=f"{label} scene_revision",
        )
    return coordinates


def _normalize_preview_operation(value: Any) -> dict[str, Any]:
    operation = _mapping(value, label="observed preview operation")
    kind = operation.get("kind")
    require(
        isinstance(kind, str) and kind in acceptance.PREVIEW_OPERATION_FIELDS,
        "observed preview operation kind differs",
    )

    def identity(source: str, target: str) -> tuple[str, str]:
        raw = _first_present(
            operation,
            ((target,), (source,)),
            label=f"observed preview {source}",
        )
        return target, _semantic_identity(
            raw,
            prefix="scene-node:",
            label=f"observed preview {source}",
        )

    normalized: dict[str, Any] = {"kind": kind}
    if kind == "create_node":
        target, parent = identity("parent_node_id", "parent_scene_node_id")
        normalized[target] = parent
        godot_type = operation.get("godot_type")
        require(
            isinstance(godot_type, str),
            "observed create-node Godot type differs",
        )
        normalized["godot_type"] = godot_type
        require(
            operation.get("name_redacted") is True,
            "observed create-node name is not redacted",
        )
        normalized["name_redacted"] = True
        normalized["name_digest"] = _digest(
            operation.get("name_digest"),
            label="observed create-node name",
        )
    elif kind == "delete_node":
        target, node = identity("node_id", "scene_node_id")
        normalized[target] = node
        normalized["subtree_node_count"] = _bounded_integer(
            operation.get("subtree_node_count"),
            label="observed delete-node subtree count",
            minimum=1,
            maximum=10_000,
        )
    elif kind == "reparent_node":
        target, node = identity("node_id", "scene_node_id")
        normalized[target] = node
        target, parent = identity(
            "new_parent_node_id",
            "new_parent_scene_node_id",
        )
        normalized[target] = parent
    elif kind == "set_property":
        target, node = identity("node_id", "scene_node_id")
        normalized[target] = node
        property_name = _first_present(
            operation,
            (("property_name",),),
            label="observed set-property name",
        )
        value_type = _first_present(
            operation,
            (("value_type",),),
            label="observed set-property type",
        )
        require(
            isinstance(property_name, str) and isinstance(value_type, str),
            "observed set-property semantics differ",
        )
        normalized["property_name"] = property_name
        normalized["value_type"] = value_type
        require(
            operation.get("value_redacted") is True,
            "observed set-property value is not redacted",
        )
        normalized["value_redacted"] = True
        normalized["value_digest"] = _digest(
            operation.get("value_digest"),
            label="observed set-property value",
        )
    elif kind in {"attach_script", "detach_script"}:
        target, node = identity("node_id", "scene_node_id")
        normalized[target] = node
        marker = operation.get("script_redacted")
        digest = operation.get("script_digest")
        if marker is None:
            marker = operation.get("script_ref_redacted")
            digest = operation.get("script_ref_digest")
        require(marker is True, "observed script reference is not redacted")
        normalized["script_redacted"] = True
        normalized["script_digest"] = _digest(
            digest,
            label="observed script reference",
        )
    elif kind in {"connect_signal", "disconnect_signal"}:
        target, emitter = identity(
            "emitter_node_id",
            "emitter_scene_node_id",
        )
        normalized[target] = emitter
        target, receiver = identity(
            "receiver_node_id",
            "receiver_scene_node_id",
        )
        normalized[target] = receiver
        for field in ("signal_name", "method_name"):
            token = operation.get(field)
            require(
                isinstance(token, str),
                f"observed signal operation {field} differs",
            )
            normalized[field] = token
        require(
            operation.get("binds_redacted") is True,
            "observed signal binds are not redacted",
        )
        normalized["binds_redacted"] = True
        normalized["binds_digest"] = _digest(
            operation.get("binds_digest"),
            label="observed signal binds",
        )
    acceptance._validate_preview_operation(normalized)
    return normalized


def _normalize_preview(observation: _ObservedResponse) -> dict[str, Any]:
    result = observation.result
    preview = _mapping(result.get("preview"), label="observed change-set preview")
    coordinates = _coordinates_from(
        result,
        label="observed prepared change set",
        fallback=observation.request,
    )
    operations = _sequence(
        preview.get("operations"),
        label="observed preview operations",
    )
    require(
        1 <= len(operations) <= 64,
        "observed preview operation count differs",
    )
    risk = result.get("risk")
    scope = result.get("scope")
    require(
        risk in {"low", "medium", "high"}
        and scope == "change_set.atomic"
        and preview.get("risk", risk) == risk,
        "observed preview risk/scope differs",
    )
    canonical_preview = {
        "editor_session_id": coordinates["editor_session_id"],
        "coordinates": {
            field: coordinates[field]
            for field in ("event_seq", "scene_revision", "operation_seq")
        },
        "operations": [
            _normalize_preview_operation(item) for item in operations
        ],
        "risk": risk,
        "scope": scope,
    }
    return {
        "transaction_id": _semantic_identity(
            _raw_transaction_id(observation),
            prefix="transaction:",
            label="observed preview transaction",
        ),
        "preview": canonical_preview,
        "preview_digest": sha256_bytes(canonical_json(canonical_preview)),
        "risk": risk,
        "scope": scope,
    }


def _transaction_result_projection(
    observation: _ObservedResponse,
    *,
    expected_state: str,
) -> dict[str, Any]:
    require(
        observation.result.get("state") == expected_state,
        f"observed transaction is not {expected_state}",
    )
    coordinates = _coordinates_from(
        observation.result,
        label=f"observed {expected_state} transaction",
        fallback=observation.request,
    )
    return {
        "editor_session_id": coordinates["editor_session_id"],
        "transaction_id": _semantic_identity(
            _raw_transaction_id(observation),
            prefix="transaction:",
            label=f"observed {expected_state} transaction",
        ),
        "event_seq": coordinates["event_seq"],
        "scene_revision": coordinates["scene_revision"],
        "operation_seq": coordinates["operation_seq"],
        "state": expected_state,
    }


def _derive_semantic_observations(
    responses: Sequence[_ObservedResponse],
) -> dict[str, tuple[int, int, dict[str, Any]]]:
    by_tool: dict[str, list[_ObservedResponse]] = {}
    forms: list[_ObservedResponse] = []
    for observation in responses:
        if observation.tool == "elicitation/create":
            forms.append(observation)
        else:
            by_tool.setdefault(observation.tool, []).append(observation)

    derived: dict[str, tuple[int, int, dict[str, Any]]] = {}
    status_events = by_tool.get("godot_get_connection_status", [])
    ready_candidates: list[tuple[int, int, dict[str, Any]]] = []
    offline_candidates: list[tuple[int, int, dict[str, Any]]] = []
    for item in status_events:
        project_scope = item.result.get("project_scope")
        if isinstance(project_scope, str):
            project_id = project_scope
        elif isinstance(project_scope, dict):
            project_id = project_scope.get("project_id")
        else:
            continue
        if not isinstance(project_id, str):
            continue
        if item.result.get("status") == "ready":
            ready_candidates.append(
                (
                    item.raw_sequence,
                    item.transport_sequence,
                    {
                        "project_id": _semantic_identity(
                            project_id,
                            prefix="project:",
                            label="ready project",
                        ),
                        "state": "ready",
                        "remediation_id": item.result.get(
                            "remediation_id"
                        ),
                    },
                )
            )
        if item.result.get("status") == "offline_cached":
            offline_candidates.append(
                (
                    item.raw_sequence,
                    item.transport_sequence,
                    {
                        "state": "offline",
                        "freshness": "offline_cached",
                        "live_state_claimed": False,
                        "remediation_id": item.result.get(
                            "remediation_id"
                        ),
                    },
                )
            )
    ready = _candidate(ready_candidates, label="connection.status")
    require(
        ready[2]["remediation_id"] == "none",
        "ready connection remediation differs",
    )
    derived["assertion.connection.status"] = ready
    offline_status = _candidate(
        offline_candidates,
        label="host.offline_status",
    )
    require(
        offline_status[2]["remediation_id"] == "start_matching_editor",
        "offline connection remediation differs",
    )
    derived["assertion.host.offline_status"] = offline_status
    project_id = cast(str, ready[2]["project_id"])

    offline_scene_candidates: list[tuple[int, int, dict[str, Any]]] = []
    for item in by_tool.get("godot_get_scene_graph", []):
        if item.result.get("freshness") != "offline_cached":
            continue
        observed_project = _semantic_identity(
            item.result.get("project_id"),
            prefix="project:",
            label="offline scene project",
        )
        require(
            observed_project == project_id,
            "offline scene project differs from ready project",
        )
        scene = item.result.get("scene")
        scene_value: Any | None = None
        if isinstance(scene, dict):
            scene_value = _optional_present(
                scene,
                (
                    ("scene_id",),
                    ("scene_entity_id",),
                    ("entity_id",),
                    ("path",),
                    ("comparison_path",),
                ),
                label="offline scene identity",
            )
        if scene_value is None:
            scene_value = item.request.get("scene")
        scene_id = _semantic_identity(
            scene_value,
            prefix="scene:",
            label="offline scene",
        )
        cursor = item.result.get("next_cursor")
        if cursor is None:
            cursor = (
                "cursor:sha256:"
                + hashlib.sha256(
                    b"godot-codex/s11-offline-query/v1\0"
                    + canonical_json(
                        {
                            "request": item.request,
                            "result": item.result,
                        }
                    )
                ).hexdigest()
            )
        offline_scene_candidates.append(
            (
                item.raw_sequence,
                item.transport_sequence,
                {
                    "project_id": project_id,
                    "scene_id": scene_id,
                    "freshness": "offline_cached",
                    "cursor": _semantic_identity(
                        cursor,
                        prefix="cursor:",
                        label="offline cursor",
                    ),
                },
            )
        )
    derived["assertion.offline.saved_query"] = _candidate(
        offline_scene_candidates,
        label="offline.saved_query",
    )

    current_scene_candidates: list[tuple[int, int, dict[str, Any]]] = []
    current_scene_rows: list[tuple[_ObservedResponse, dict[str, Any]]] = []
    for item in by_tool.get("godot_get_current_scene", []):
        if (
            item.result.get("freshness") != "current"
            or item.result.get("is_error") is True
            or _error_projection(item.result) is not None
        ):
            continue
        observed_project = _semantic_identity(
            item.result.get("project_id"),
            prefix="project:",
            label="current scene project",
        )
        require(
            observed_project == project_id,
            "current scene project differs from ready project",
        )
        scene = _mapping(
            item.result.get("scene"),
            label="current scene identity",
        )
        scene_id = _semantic_identity(
            _first_present(
                scene,
                (
                    ("scene_id",),
                    ("scene_entity_id",),
                    ("entity_id",),
                    ("path",),
                ),
                label="current scene identity",
            ),
            prefix="scene:",
            label="current scene",
        )
        nodes = _sequence(item.result.get("nodes"), label="current scene nodes")
        roots = [
            _mapping(node, label="current scene node")
            for node in nodes
            if isinstance(node, dict)
            and (
                node.get("depth") == 0
                or node.get("node_path") == "."
                or node.get("path") == "."
                or node.get("parent_node_id") is None
            )
        ]
        require(len(roots) == 1, "current scene root node is ambiguous")
        root = roots[0]
        scene_node_id = _semantic_identity(
            _first_present(
                root,
                (
                    ("scene_node_id",),
                    ("node_entity_id",),
                    ("entity_id",),
                    ("node_id",),
                ),
                label="current scene root identity",
            ),
            prefix="scene-node:",
            label="current scene root",
        )
        facts = [
            _mapping(fact, label="current scene fact")
            for fact in _sequence(
                item.result.get("facts"),
                label="current scene facts",
            )
            if isinstance(fact, dict)
            and fact.get("fact_code") == "current_scene.root_type"
        ]
        require(len(facts) <= 1, "current scene root-type fact is ambiguous")
        if facts:
            fact = facts[0]
            evidence_id = _semantic_identity(
                fact.get("evidence_id"),
                prefix="evidence:",
                label="current scene evidence",
            )
            fact_value_digest = _digest(
                fact.get("fact_value_digest"),
                label="current scene root-type fact",
            )
            require(
                fact.get("fact_value_redacted") is True,
                "current scene root-type fact is not redacted",
            )
            confidence = fact.get("confidence")
        else:
            godot_type = root.get("godot_type")
            require(
                isinstance(godot_type, str),
                "current scene root type is missing",
            )
            evidence_id = _semantic_identity(
                item.result.get("snapshot_id"),
                prefix="evidence:",
                label="current scene snapshot evidence",
            )
            fact_value_digest = sha256_bytes(canonical_json(godot_type))
            confidence = "authoritative"
        value = {
            "project_id": project_id,
            "scene_id": scene_id,
            "scene_node_id": scene_node_id,
            "evidence_id": evidence_id,
            "fact_code": "current_scene.root_type",
            "fact_value_redacted": True,
            "fact_value_digest": fact_value_digest,
            "confidence": confidence,
            "freshness": "current",
        }
        current_scene_candidates.append(
            (item.raw_sequence, item.transport_sequence, value)
        )
        current_scene_rows.append(
            (
                item,
                _live_scene_coordinates(
                    item.result,
                    scene_identity=scene_id,
                    label="current scene revision",
                ),
            )
        )
    derived["assertion.saved.current_scene"] = _candidate(
        current_scene_candidates,
        label="saved.current_scene",
    )

    run_events = [
        item
        for tool in ("godot_run_project", "godot_run_current_scene")
        for item in by_tool.get(tool, [])
        if item.result.get("state") in {"running", "paused"}
        and isinstance(item.result.get("runtime_session_id"), str)
    ]
    run_events.sort(key=lambda item: item.raw_sequence)
    sessions: list[tuple[_ObservedResponse, str]] = []
    for item in run_events:
        raw = cast(str, item.result["runtime_session_id"])
        if not sessions or sessions[-1][1] != raw:
            sessions.append((item, raw))
    require(
        len(sessions) >= 2 and sessions[0][1] != sessions[-1][1],
        "runtime restart observations are missing",
    )
    before_runtime = sessions[0][1]
    after_runtime = sessions[-1][1]
    tree_candidates = [
        item
        for item in by_tool.get("godot_get_runtime_tree", [])
        if item.result.get("runtime_session_id") == after_runtime
        and item.result.get("state") in {"running", "paused"}
    ]
    require(len(tree_candidates) == 1, "runtime tree observation is ambiguous")
    tree = tree_candidates[0]
    runtime_nodes = _sequence(
        tree.result.get("nodes"),
        label="runtime tree nodes",
    )
    require(runtime_nodes, "runtime tree is empty")
    runtime_node = _mapping(runtime_nodes[0], label="runtime tree node")
    runtime_node_id = _semantic_identity(
        _first_present(
            runtime_node,
            (
                ("runtime_node_id",),
                ("runtime_object_id",),
                ("entity_id",),
            ),
            label="runtime node identity",
        ),
        prefix="runtime-node:",
        label="runtime node",
    )
    stack_candidates = [
        item
        for item in by_tool.get("godot_get_stack_trace", [])
        if item.result.get("runtime_session_id") == after_runtime
    ]
    require(len(stack_candidates) == 1, "runtime stack observation is ambiguous")
    stack_event = stack_candidates[0]
    stack = _mapping(stack_event.result.get("stack"), label="runtime stack")
    frames = _sequence(stack.get("frames"), label="runtime stack frames")
    frame_candidates = [
        _mapping(frame, label="runtime stack frame")
        for frame in frames
        if isinstance(frame, dict)
        and (
            frame.get("error_code") is not None
            or frame.get("code") is not None
        )
    ]
    require(
        len(frame_candidates) == 1,
        "runtime error stack frame is ambiguous",
    )
    frame = frame_candidates[0]
    source = _mapping(frame.get("source"), label="runtime stack source")
    runtime_projection = {
        "runtime_session_before": _semantic_identity(
            before_runtime,
            prefix="runtime-session:",
            label="runtime session before",
        ),
        "runtime_session_after": _semantic_identity(
            after_runtime,
            prefix="runtime-session:",
            label="runtime session after",
        ),
        "runtime_node_id": runtime_node_id,
        "stack_frame_id": _semantic_identity(
            _first_present(
                frame,
                (("stack_frame_id",), ("entity_id",), ("frame_id",)),
                label="runtime stack frame",
            ),
            prefix="stack-frame:",
            label="runtime stack frame",
        ),
        "error_code": _first_present(
            frame,
            (("error_code",), ("code",)),
            label="runtime error code",
        ),
        "source": {
            "path": source.get("path"),
            "line": source.get("line"),
        },
    }
    derived["assertion.runtime.error_stack"] = (
        stack_event.raw_sequence,
        stack_event.transport_sequence,
        runtime_projection,
    )

    form_actions: dict[str, tuple[_ObservedResponse, str]] = {}
    for item in forms:
        transaction = item.request.get("transaction_id")
        action = item.result.get("action")
        require(
            isinstance(transaction, str)
            and isinstance(action, str)
            and transaction not in form_actions,
            "form transaction/action is missing or duplicated",
        )
        form_actions[transaction] = (item, action)

    transaction_observations = [
        item
        for tool in (
            "godot_apply_transaction",
            "godot_get_transaction_status",
            "godot_undo_transaction",
        )
        for item in by_tool.get(tool, [])
        if (
            item.request.get("transaction_id") is not None
            or item.result.get("transaction_id") is not None
            or item.result.get("change_set_id") is not None
        )
    ]
    by_transaction: dict[str, list[_ObservedResponse]] = {}
    for item in transaction_observations:
        raw_id = _raw_transaction_id(item)
        by_transaction.setdefault(raw_id, []).append(item)

    scenario_transactions: dict[str, tuple[str, _ObservedResponse]] = {}
    negative_codes = {
        "approval_declined": "decline",
        "approval_cancelled": "cancel",
        "approval_timeout": "timeout",
    }
    for raw_id, items in by_transaction.items():
        committed = [
            item for item in items if item.result.get("state") == "committed"
        ]
        if committed:
            action = form_actions.get(raw_id)
            require(
                action is not None and action[1] == "accept",
                "committed transaction lacks the matching accepted form",
            )
            committed_projections = [
                (
                    item.raw_sequence,
                    item.transport_sequence,
                    _transaction_result_projection(
                        item,
                        expected_state="committed",
                    ),
                )
                for item in committed
            ]
            _candidate(
                committed_projections,
                label="committed transaction terminal",
                prefer_latest=True,
            )
            report_ids = {
                item.result.get("validation_report_id")
                for item in committed
                if isinstance(
                    item.result.get("validation_report_id"),
                    str,
                )
            }
            require(
                len(report_ids) == 1,
                "committed transaction validation binding is ambiguous",
            )
            terminal_candidates = [
                item
                for item in committed
                if item.result.get("validation_report_id")
                == next(iter(report_ids))
            ]
            require(
                "accept" not in scenario_transactions,
                "accepted transaction observation is ambiguous",
            )
            scenario_transactions["accept"] = (
                raw_id,
                max(
                    terminal_candidates,
                    key=lambda item: item.raw_sequence,
                ),
            )
        for item in items:
            code = _error_code(item.result)
            if code not in negative_codes:
                continue
            scenario = negative_codes[cast(str, code)]
            expected_form = form_actions.get(raw_id)
            if scenario == "timeout":
                require(
                    expected_form is None,
                    "approval timeout unexpectedly has a form completion",
                )
            else:
                require(
                    expected_form is not None
                    and expected_form[1] == scenario,
                    f"approval {scenario} lacks the matching form completion",
                )
            require(
                scenario not in scenario_transactions,
                f"approval {scenario} transaction is ambiguous",
            )
            scenario_transactions[scenario] = (raw_id, item)
    require(
        set(scenario_transactions) == FORM_SCENARIOS,
        "approval scenario observation coverage differs",
    )
    semantic_transactions = {
        scenario: _semantic_identity(
            raw_id,
            prefix="transaction:",
            label=f"approval {scenario} transaction",
        )
        for scenario, (raw_id, _item) in scenario_transactions.items()
    }
    require(
        len(set(semantic_transactions.values())) == 4,
        "approval transactions overlap",
    )
    for scenario in sorted(FORM_SCENARIOS):
        raw_id, tool_event = scenario_transactions[scenario]
        source_event = (
            form_actions[raw_id][0]
            if scenario in {"accept", "decline", "cancel"}
            else tool_event
        )
        state = "committed" if scenario == "accept" else "not_applied"
        derived[f"assertion.approval.{scenario}"] = (
            source_event.raw_sequence,
            source_event.transport_sequence,
            {
                "transaction_id": semantic_transactions[scenario],
                "action": scenario,
                "state": state,
            },
        )
        derived[f"form_outcome.{scenario}"] = (
            source_event.raw_sequence,
            source_event.transport_sequence,
            {
                "scenario": scenario,
                "action": scenario,
                "semantic_state": state,
                "mutation_observed": scenario == "accept",
                "content_recorded": False,
            },
        )

    accepted_raw, accepted_event = scenario_transactions["accept"]
    prepare_candidates = [
        item
        for item in by_tool.get("godot_prepare_change_set", [])
        if (
            item.result.get("change_set_id") == accepted_raw
            and item.result.get("state") in {"previewed", "prepared"}
        )
    ]
    require(
        len(prepare_candidates) == 1,
        "accepted prepared change set is missing or ambiguous",
    )
    prepared = prepare_candidates[0]
    preview_projection = _normalize_preview(prepared)
    require(
        preview_projection["transaction_id"]
        == semantic_transactions["accept"],
        "accepted preview transaction differs",
    )
    observed_preview_digest = _digest(
        prepared.result.get("preview_digest"),
        label="observed prepared preview",
    )
    accepted_apply_candidates = [
        item
        for item in by_transaction[accepted_raw]
        if item.tool == "godot_apply_transaction"
        and item.result.get("state") in {"validating", "committed"}
        and _error_projection(item.result) is None
    ]
    require(
        len(accepted_apply_candidates) == 1,
        "accepted apply request is missing or ambiguous",
    )
    accepted_apply = accepted_apply_candidates[0]
    require(
        accepted_apply.request.get("preview_digest")
        == observed_preview_digest,
        "accepted apply preview digest differs from prepared preview",
    )
    require(
        accepted_apply.raw_sequence
        > form_actions[accepted_raw][0].raw_sequence,
        "accepted apply response precedes approval completion",
    )
    derived["assertion.transaction.preview"] = (
        prepared.raw_sequence,
        prepared.transport_sequence,
        preview_projection,
    )
    apply_projection = _transaction_result_projection(
        accepted_event,
        expected_state="committed",
    )
    derived["assertion.transaction.apply"] = (
        accepted_event.raw_sequence,
        accepted_event.transport_sequence,
        apply_projection,
    )

    undo_candidates = [
        item
        for item in by_transaction[accepted_raw]
        if item.tool == "godot_undo_transaction"
        and item.result.get("state") == "undone"
    ]
    require(
        len(undo_candidates) == 1,
        "accepted transaction Undo is missing or ambiguous",
    )
    undo_event = undo_candidates[0]
    undo_projection = _transaction_result_projection(
        undo_event,
        expected_state="undone",
    )
    derived["assertion.transaction.undo"] = (
        undo_event.raw_sequence,
        undo_event.transport_sequence,
        undo_projection,
    )

    report_id = accepted_event.result.get("validation_report_id")
    require(
        isinstance(report_id, str),
        "accepted validation result binding differs",
    )
    report_candidates = [
        item
        for item in by_tool.get("godot_get_validation_report", [])
        if item.request.get("report_id") == report_id
        and item.result.get("report_id") == report_id
        and item.result.get("page") == 0
    ]
    require(
        len(report_candidates) == 1,
        "validation report observation is missing or ambiguous",
    )
    report = report_candidates[0]
    content_projection = _mapping(
        report.result.get("content_projection"),
        label="validation report content projection",
    )
    require(
        content_projection.get("transaction_id") == accepted_raw
        and content_projection.get("outcome") == "passed",
        "validation report transaction/outcome differs",
    )
    checks = _sequence(
        content_projection.get("checks"),
        label="validation report checks",
    )
    require(
        checks
        and all(
            isinstance(check, dict)
            and (
                check.get("authority") != "required"
                or check.get("outcome") == "passed"
            )
            for check in checks
        ),
        "validation report required checks differ",
    )
    diagnostic_summary = content_projection.get("diagnostics", {})
    require(
        isinstance(diagnostic_summary, dict),
        "validation report diagnostics differ",
    )
    introduced = diagnostic_summary.get("introduced", [])
    require(
        isinstance(introduced, list)
        and diagnostic_summary.get("introduced_errors", 0) == 0,
        "validation report introduced diagnostics differ",
    )
    validation_projection = {
        "transaction_id": semantic_transactions["accept"],
        "validation_report_id": _semantic_identity(
            report_id,
            prefix="validation-report:",
            label="validation report",
        ),
        "outcome": "passed",
        "diagnostics": list(introduced),
    }
    derived["assertion.validation.result"] = (
        report.raw_sequence,
        report.transport_sequence,
        validation_projection,
    )

    prepared_coordinates = _coordinates_from(
        prepared.result,
        label="prepared revision",
        fallback=prepared.request,
    )
    initial_rows = [
        (item, row)
        for item, row in current_scene_rows
        if item.raw_sequence < prepared.raw_sequence
        and row["editor_session_id"]
        == prepared_coordinates["editor_session_id"]
        and all(
            row[field] == prepared_coordinates[field]
            for field in ("event_seq", "operation_seq")
        )
        and (
            "scene_revision" not in row
            or row["scene_revision"]
            == prepared_coordinates["scene_revision"]
        )
    ]
    require(
        initial_rows,
        "initial revision observation is missing",
    )
    initial_event, _initial_coordinates = initial_rows[-1]
    initial_projection = {"step": "initial", **prepared_coordinates}
    prepared_projection = {"step": "prepared", **prepared_coordinates}
    applied_projection = {
        "step": "applied",
        **{
            field: apply_projection[field]
            for field in (
                "editor_session_id",
                "event_seq",
                "scene_revision",
                "operation_seq",
            )
        },
    }
    restarted_rows = [
        (item, row)
        for item, row in current_scene_rows
        if item.raw_sequence > accepted_event.raw_sequence
        and row["editor_session_id"]
        != apply_projection["editor_session_id"]
    ]
    restarted_candidate = _candidate(
        [
            (
                item.raw_sequence,
                item.transport_sequence,
                row,
            )
            for item, row in restarted_rows
        ],
        label="post-apply restarted revision",
    )
    restarted_event = next(
        item
        for item, _row in restarted_rows
        if item.raw_sequence == restarted_candidate[0]
        and item.transport_sequence == restarted_candidate[1]
    )
    restarted_coordinates = restarted_candidate[2]
    if "scene_revision" not in restarted_coordinates:
        auxiliary_coordinates: list[dict[str, Any]] = []
        for item in responses:
            coordinates = item.result.get("coordinates")
            if (
                item.raw_sequence >= restarted_event.raw_sequence
                and isinstance(coordinates, dict)
                and coordinates.get("editor_session_id") is not None
                and _semantic_identity(
                    coordinates["editor_session_id"],
                    prefix="editor-session:",
                    label="post-restart coordinate editor",
                )
                == restarted_coordinates["editor_session_id"]
            ):
                try:
                    candidate = _coordinates_from(
                        item.result,
                        label="post-restart coordinate",
                        fallback=item.request,
                    )
                except SurfaceArtifactError:
                    continue
                if all(
                    candidate[field] == restarted_coordinates[field]
                    for field in ("event_seq", "operation_seq")
                ):
                    auxiliary_coordinates.append(candidate)
        auxiliary = _candidate(
            [
                (index, index, value)
                for index, value in enumerate(auxiliary_coordinates)
            ],
            label="post-restart scene revision",
        )
        restarted_coordinates = auxiliary[2]

    stale_candidates: list[_ObservedResponse] = []
    for item in transaction_observations:
        if item.raw_sequence <= restarted_event.raw_sequence:
            continue
        if _error_code(item.result) not in {
            "stale_coordinates",
            "stale_revision",
            "scene_revision_mismatch",
        }:
            continue
        stale_candidates.append(item)
    require(
        len(stale_candidates) == 1,
        "stale-guard rejection observation is missing or ambiguous",
    )
    stale_event = stale_candidates[0]
    for step, source, row in (
        ("initial", initial_event, initial_projection),
        ("prepared", prepared, prepared_projection),
        ("applied", accepted_event, applied_projection),
        (
            "restarted",
            restarted_event,
            {"step": "restarted", **restarted_coordinates},
        ),
        (
            "stale_guard_rejected",
            stale_event,
            {"step": "stale_guard_rejected", **restarted_coordinates},
        ),
    ):
        derived[f"revision.{step}"] = (
            source.raw_sequence,
            source.transport_sequence,
            row,
        )

    multi_candidates: list[tuple[int, int, dict[str, Any]]] = []
    for items in by_tool.values():
        for item in items:
            error = _error_projection(item.result)
            if error is None or _error_code(item.result) not in {
                "project_binding_mismatch",
                "wrong_project",
            }:
                continue
            foreign = (
                error.get("foreign_project_id")
                or item.request.get("project_id")
            )
            foreign_project_id = _semantic_identity(
                foreign,
                prefix="project:",
                label="foreign project",
            )
            if foreign_project_id == project_id:
                continue
            require(
                error.get("retryable") is False
                and error.get("remediation_id") == "select_bound_project",
                "multi-project rejection semantics differ",
            )
            multi_candidates.append(
                (
                    item.raw_sequence,
                    item.transport_sequence,
                    {
                        "project_id": project_id,
                        "foreign_project_id": foreign_project_id,
                        "error": {
                            "code": "project_binding_mismatch",
                            "retryable": False,
                            "remediation_id": "select_bound_project",
                        },
                    },
                )
            )
    derived["assertion.multi_project.reject"] = _candidate(
        multi_candidates,
        label="multi_project.reject",
    )

    require(
        set(derived) == SEMANTIC_PROJECTION_IDS,
        "automatic tool observation coverage differs",
    )
    _projection_parts(
        {identifier: item[2] for identifier, item in derived.items()}
    )
    return derived


def canonical_journal_from_raw_capture(
    *,
    metadata: Mapping[str, Any],
    metadata_payload: bytes,
    capture: Mapping[str, Any],
    capture_payload: bytes,
) -> dict[str, Any]:
    """Validate the private Rust capture and remove time/run-specific fields."""

    checked_metadata = validate_metadata(metadata)
    require(
        metadata_payload == canonical_artifact(checked_metadata),
        "capture metadata bytes differ",
    )
    artifact = _exact(
        capture,
        RAW_CAPTURE_FIELDS,
        label="private Rust capture artifact",
    )
    metadata_sha256 = sha256_bytes(metadata_payload)
    require(
        artifact["schema_version"] == RAW_CAPTURE_SCHEMA_VERSION
        and artifact["outcome"] == "completed"
        and artifact["metadata_file"] == "metadata.json"
        and artifact["metadata_sha256"] == metadata_sha256,
        "private Rust capture artifact identity differs",
    )
    journal = _exact(
        artifact["journal"],
        RAW_JOURNAL_FIELDS,
        label="private Rust capture journal",
    )
    machine = cast(Mapping[str, Any], checked_metadata["machine_bindings"])
    require(
        journal["schema_version"] == RAW_JOURNAL_SCHEMA_VERSION
        and isinstance(journal["run_id"], str)
        and re.fullmatch(r"[0-9a-f]{64}", journal["run_id"]) is not None
        and journal["transport_origin"] == "official_host_stdio"
        and journal["surface"] == checked_metadata["surface"]
        and journal["project_identity"]
        == machine["project_identity_sha256"]
        and journal["metadata_sha256"] == metadata_sha256
        and journal["package_launcher_sha256"]
        == machine["package_launcher_sha256"]
        and journal["project_config_sha256"]
        == machine["project_config_sha256"]
        and journal["setup_receipt_sha256"]
        == machine["setup_receipt_sha256"],
        "private Rust capture lease/machine binding differs",
    )
    for field in (
        "project_identity",
        "metadata_sha256",
        "package_launcher_sha256",
        "project_config_sha256",
        "setup_receipt_sha256",
        "final_event_sha256",
    ):
        _digest(journal[field], label=f"raw capture {field}")
    require(
        journal["event_capacity"] == MAX_EVENTS
        and isinstance(journal["events"], list)
        and 1 <= len(journal["events"]) <= MAX_EVENTS
        and journal["observed_event_count"] == len(journal["events"])
        and journal["dropped_event_count"] == 0,
        "private Rust capture is truncated, dropped, or ambiguous",
    )
    previous = (
        "sha256:"
        + "0" * 64
    )
    raw_events: list[dict[str, Any]] = []
    for index, item in enumerate(journal["events"]):
        event = _validate_raw_event(item, index=index, previous=previous)
        previous = event["event_sha256"]
        raw_events.append(event)
    require(
        journal["final_event_sha256"] == previous,
        "private Rust capture final event hash differs",
    )

    transport_events: list[dict[str, Any]] = []
    raw_to_transport: dict[int, int] = {}
    raw_projections: list[tuple[int, dict[str, Any]]] = []
    observed_responses: list[_ObservedResponse] = []
    protocols: set[str] = set()
    for raw in raw_events:
        sequence = cast(int, raw["sequence"])
        projection = raw.get("semantic_projection")
        if projection is not None:
            raw_projections.append(
                (sequence, cast(dict[str, Any], projection))
            )
            continue
        projected: dict[str, Any] = {
            "seq": len(transport_events),
            "direction": raw["direction"],
            "kind": (
                "response" if raw["phase"] == "error" else raw["phase"]
            ),
            "class": raw["class"],
            "registry_names": raw.get("registry_names", []),
        }
        for field in (
            "method",
            "tool",
            "protocol_version",
            "instructions_sha256",
            "error_code",
            "tool_observation",
        ):
            if field in raw:
                projected[field] = raw[field]
        if (
            raw["class"] == "initialize"
            and raw["phase"] == "response"
            and isinstance(raw.get("protocol_version"), str)
        ):
            protocols.add(cast(str, raw["protocol_version"]))
        raw_to_transport[sequence] = len(transport_events)
        observation = raw.get("tool_observation")
        if observation is not None:
            checked_observation = _exact(
                observation,
                {"request", "result"},
                label="raw tool observation",
            )
            tool = (
                "elicitation/create"
                if raw["class"] == "form"
                else raw.get("tool")
            )
            require(
                isinstance(tool, str),
                "raw tool observation lacks a bound tool",
            )
            observed_responses.append(
                _ObservedResponse(
                    raw_sequence=sequence,
                    transport_sequence=len(transport_events),
                    tool=tool,
                    phase=cast(str, raw["phase"]),
                    request=cast(
                        Mapping[str, Any],
                        checked_observation["request"],
                    ),
                    result=cast(
                        Mapping[str, Any],
                        checked_observation["result"],
                    ),
                )
            )
        transport_events.append(projected)
    require(
        len(protocols) == 1
        and next(iter(protocols)) in {"2025-06-18", "2025-11-25"},
        "private Rust capture negotiated protocol is missing or ambiguous",
    )
    independently_derived = _derive_semantic_observations(
        observed_responses
    )
    seen_explicit: set[str] = set()
    for projection_sequence, projection in raw_projections:
        record = _exact(
            projection,
            {"projection_id", "source_event_seq", "value"},
            label="raw semantic projection",
        )
        projection_id = record["projection_id"]
        source = record["source_event_seq"]
        require(
            isinstance(projection_id, str)
            and projection_id in SEMANTIC_PROJECTION_IDS
            and projection_id not in seen_explicit
            and isinstance(source, int)
            and not isinstance(source, bool)
            and source in raw_to_transport
            and source < projection_sequence,
            "raw semantic projection is missing, duplicated, or unbound",
        )
        source_event = raw_events[source - 1]
        derived_source, _transport_source, derived_value = (
            independently_derived[cast(str, projection_id)]
        )
        require(
            source_event["phase"] in {"response", "error"}
            and source_event["class"] != "semantic_projection"
            and isinstance(record["value"], dict)
            and source == derived_source
            and record["value"] == derived_value,
            "raw semantic projection does not equal its independent tool observation",
        )
        acceptance.safe_evidence_scan(record["value"])
        seen_explicit.add(cast(str, projection_id))
    semantic = [
        {
            "projection_id": projection_id,
            "source_event_seq": transport_source,
            "value": value,
        }
        for projection_id, (
            _raw_source,
            transport_source,
            value,
        ) in sorted(independently_derived.items())
    ]
    capture_sha256 = sha256_bytes(capture_payload)
    canonical: dict[str, Any] = {
        "schema_version": JOURNAL_SCHEMA_VERSION,
        "capture_kind": CAPTURE_KIND,
        "status": JOURNAL_STATUS,
        "surface": checked_metadata["surface"],
        "host": checked_metadata["host"],
        "bindings": checked_metadata["bindings"],
        "metadata_sha256": metadata_sha256,
        "protocol_version": next(iter(protocols)),
        "events": transport_events,
        "semantic_projections": semantic,
        "transport_origin": {
            "capture_schema_version": RAW_CAPTURE_SCHEMA_VERSION,
            "capture_journal_sha256": capture_sha256,
            "transport": "official_host_stdio",
            "project_identity_sha256": journal["project_identity"],
            "package_launcher_sha256": journal[
                "package_launcher_sha256"
            ],
            "project_config_sha256": journal["project_config_sha256"],
            "setup_receipt_sha256": journal["setup_receipt_sha256"],
            "final_capture_event_sha256": journal[
                "final_event_sha256"
            ],
            "observed_event_count": journal["observed_event_count"],
        },
        "integrity": {
            "event_count": len(transport_events),
            "event_chain_sha256": recorder_event_chain_sha256(
                transport_events
            ),
            "semantic_projection_count": len(semantic),
            "passthrough_mode": True,
            "recorder_errors": [],
            "truncated": False,
        },
        "redaction": {
            "request_arguments_reduced_to_allowlist": True,
            "tool_results_reduced_to_allowlist": True,
            "elicitation_content_absent": True,
            "source_content_absent": True,
            "opaque_native_handles_absent": True,
            "absolute_paths_absent": True,
            "secrets_absent": True,
        },
    }
    validate_journal(
        canonical,
        metadata=checked_metadata,
        metadata_sha256=metadata_sha256,
    )
    acceptance.safe_evidence_scan(canonical)
    return canonical


def validate_journal(
    value: Any,
    *,
    metadata: Mapping[str, Any],
    metadata_sha256: str,
) -> tuple[dict[str, Any], dict[str, Any]]:
    journal = _exact(value, JOURNAL_FIELDS, label="recorder journal")
    require(
        journal["schema_version"] == JOURNAL_SCHEMA_VERSION
        and journal["capture_kind"] == CAPTURE_KIND
        and journal["status"] == JOURNAL_STATUS
        and journal["surface"] == metadata["surface"]
        and journal["metadata_sha256"] == metadata_sha256,
        "recorder journal identity/metadata binding differs",
    )
    _digest(journal["metadata_sha256"], label="journal metadata")
    host = _validate_host(journal["host"], label="journal host")
    bindings = _validate_bindings(journal["bindings"], label="journal bindings")
    require(
        host == metadata["host"] and bindings == metadata["bindings"],
        "journal metadata projection differs",
    )
    require(
        journal["protocol_version"] in {"2025-06-18", "2025-11-25"},
        "journal negotiated protocol differs",
    )
    events, semantic_sources, observed_responses = _validate_journal_events(
        journal["events"],
        registry=cast(Mapping[str, Any], metadata["registry"]),
    )
    projections, projection_sources = _semantic_projection_map(
        journal["semantic_projections"],
        source_events=semantic_sources,
    )
    independently_derived = _derive_semantic_observations(
        observed_responses
    )
    require(
        set(independently_derived) == set(projections)
        and all(
            independently_derived[projection_id][1]
            == projection_sources[projection_id]
            and independently_derived[projection_id][2] == projection
            for projection_id, projection in projections.items()
        ),
        "journal semantic projection differs from its independent tool observation",
    )
    origin = _exact(
        journal["transport_origin"],
        {
            "capture_schema_version",
            "capture_journal_sha256",
            "transport",
            "project_identity_sha256",
            "package_launcher_sha256",
            "project_config_sha256",
            "setup_receipt_sha256",
            "final_capture_event_sha256",
            "observed_event_count",
        },
        label="journal transport origin",
    )
    require(
        origin["capture_schema_version"] == RAW_CAPTURE_SCHEMA_VERSION
        and origin["transport"] == "official_host_stdio",
        "journal transport origin identity differs",
    )
    for field in (
        "capture_journal_sha256",
        "project_identity_sha256",
        "package_launcher_sha256",
        "project_config_sha256",
        "setup_receipt_sha256",
        "final_capture_event_sha256",
    ):
        _digest(origin[field], label=f"journal transport origin {field}")
    _bounded_integer(
        origin["observed_event_count"],
        label="journal transport observed event count",
        minimum=1,
        maximum=MAX_EVENTS,
    )
    machine = cast(Mapping[str, Any], metadata["machine_bindings"])
    require(
        origin["project_identity_sha256"]
        == machine["project_identity_sha256"]
        and origin["package_launcher_sha256"]
        == machine["package_launcher_sha256"]
        and origin["project_config_sha256"]
        == machine["project_config_sha256"]
        and origin["setup_receipt_sha256"]
        == machine["setup_receipt_sha256"],
        "journal machine binding differs",
    )
    integrity = _exact(
        journal["integrity"],
        {
            "event_count",
            "event_chain_sha256",
            "semantic_projection_count",
            "passthrough_mode",
            "recorder_errors",
            "truncated",
        },
        label="journal integrity",
    )
    for field in ("event_count", "semantic_projection_count"):
        _bounded_integer(
            integrity[field],
            label=f"journal {field}",
            maximum=MAX_EVENTS,
        )
    require(
        integrity["event_count"] == len(events)
        and integrity["event_chain_sha256"]
        == recorder_event_chain_sha256(events)
        and integrity["semantic_projection_count"]
        == len(SEMANTIC_PROJECTION_IDS)
        and integrity["passthrough_mode"] is True
        and integrity["recorder_errors"] == []
        and integrity["truncated"] is False,
        "journal event chain/integrity differs",
    )
    redaction = _exact(
        journal["redaction"],
        {
            "request_arguments_reduced_to_allowlist",
            "tool_results_reduced_to_allowlist",
            "elicitation_content_absent",
            "source_content_absent",
            "opaque_native_handles_absent",
            "absolute_paths_absent",
            "secrets_absent",
        },
        label="journal redaction",
    )
    require(
        all(value is True for value in redaction.values()),
        "journal redaction differs",
    )
    acceptance.safe_evidence_scan(journal)
    return journal, projections


def derive_pending_trace(
    *,
    metadata: Mapping[str, Any],
    metadata_payload: bytes,
    journal: Mapping[str, Any],
    journal_payload: bytes,
) -> dict[str, Any]:
    """Derive an explicitly non-qualifying trace from one complete journal."""

    checked_metadata = validate_metadata(metadata)
    require(
        metadata_payload == canonical_artifact(checked_metadata),
        "metadata bytes differ from the validated canonical document",
    )
    checked_journal, projections = validate_journal(
        journal,
        metadata=checked_metadata,
        metadata_sha256=sha256_bytes(metadata_payload),
    )
    require(
        journal_payload == canonical_artifact(checked_journal),
        "journal bytes differ from the validated canonical document",
    )
    assertions, forms, revisions = _projection_parts(projections)
    integrity = cast(Mapping[str, Any], checked_journal["integrity"])
    bindings = {
        **cast(dict[str, Any], checked_metadata["bindings"]),
        "recorder_journal_sha256": sha256_bytes(journal_payload),
        "recorder_event_chain_sha256": integrity["event_chain_sha256"],
        "recorder_event_count": integrity["event_count"],
    }
    pending: dict[str, Any] = {
        "schema_version": PENDING_TRACE_SCHEMA_VERSION,
        "capture_kind": CAPTURE_KIND,
        "status": PENDING_STATUS,
        "surface": checked_metadata["surface"],
        "host": checked_metadata["host"],
        "bindings": bindings,
        "registry": checked_metadata["registry"],
        "assertions": [
            {"assertion_id": name, "projection": assertions[name]}
            for name in sorted(assertions)
        ],
        "form_outcomes": forms,
        "revision_timeline": revisions,
        "attestation_requirements": {
            "final_schema_version": FINAL_TRACE_SCHEMA_VERSION,
            "missing_assertions": sorted(HOST_ATTESTED_ASSERTIONS),
            "authority_observations": list(AUTHORITY_OBSERVATIONS),
        },
    }
    pending["redaction"] = acceptance.derive_surface_trace_redaction(pending)
    _validate_pending_trace(pending)
    acceptance.safe_evidence_scan(pending)
    return pending


def _assertion_map(
    value: Any,
    *,
    expected: frozenset[str],
    label: str,
) -> dict[str, Mapping[str, Any]]:
    require(
        isinstance(value, list) and len(value) == len(expected),
        f"{label} assertion count differs",
    )
    result: dict[str, Mapping[str, Any]] = {}
    for item in value:
        record = _exact(
            item,
            {"assertion_id", "projection"},
            label=f"{label} assertion",
        )
        name = record["assertion_id"]
        require(
            isinstance(name, str)
            and name in expected
            and name not in result
            and isinstance(record["projection"], dict),
            f"{label} assertion differs or is duplicated",
        )
        result[name] = cast(Mapping[str, Any], record["projection"])
    require(set(result) == expected, f"{label} assertion coverage differs")
    return result


def _validate_trace_bindings(value: Any) -> dict[str, Any]:
    expected = METADATA_BINDING_FIELDS | {
        "recorder_journal_sha256",
        "recorder_event_chain_sha256",
        "recorder_event_count",
    }
    bindings = _exact(value, expected, label="trace bindings")
    _commit(bindings["package_source_commit"], label="trace source")
    for field in expected - {
        "package_source_commit",
        "recorder_event_count",
    }:
        _digest(bindings[field], label=f"trace {field}")
    _bounded_integer(
        bindings["recorder_event_count"],
        label="trace recorder event count",
        minimum=1,
        maximum=MAX_EVENTS,
    )
    return bindings


def _validate_pending_trace(value: Any) -> dict[str, Any]:
    pending = _exact(value, PENDING_TRACE_FIELDS, label="pending trace")
    require(
        pending["schema_version"] == PENDING_TRACE_SCHEMA_VERSION
        and pending["capture_kind"] == CAPTURE_KIND
        and pending["status"] == PENDING_STATUS
        and pending["surface"] in {"app", "cli", "ide"},
        "pending trace identity differs",
    )
    _validate_host(pending["host"], label="pending trace host")
    _validate_trace_bindings(pending["bindings"])
    _validate_registry(pending["registry"], label="pending trace registry")
    assertions = _assertion_map(
        pending["assertions"],
        expected=TRANSPORT_ASSERTION_IDS,
        label="pending trace",
    )
    _validate_transport_assertions(assertions)
    acceptance._validate_form_outcomes(pending["form_outcomes"])
    acceptance._validate_revision_timeline(pending["revision_timeline"])
    requirements = _exact(
        pending["attestation_requirements"],
        {
            "final_schema_version",
            "missing_assertions",
            "authority_observations",
        },
        label="pending attestation requirements",
    )
    require(
        requirements["final_schema_version"] == FINAL_TRACE_SCHEMA_VERSION
        and requirements["missing_assertions"]
        == sorted(HOST_ATTESTED_ASSERTIONS)
        and requirements["authority_observations"]
        == list(AUTHORITY_OBSERVATIONS),
        "pending attestation requirements differ",
    )
    redaction = _exact(
        pending["redaction"],
        {
            "absolute_paths_absent",
            "approval_content_absent",
            "native_ids_absent",
            "secrets_absent",
            "source_content_absent",
            "truncated",
        },
        label="pending trace redaction",
    )
    require(
        redaction == acceptance.derive_surface_trace_redaction(pending),
        "pending trace redaction differs",
    )
    return pending


def _host_assertion_records(mcp_binary_sha256: str) -> dict[str, dict[str, Any]]:
    _digest(mcp_binary_sha256, label="attested launcher")
    return {
        "host.approval_layers": {
            "sandbox_approval": "observed",
            "mcp_approval": "action_only_form",
            "independent": True,
        },
        "host.config_reload": {
            "config_reloaded": True,
            "root_rebound": True,
            "restart_required": False,
        },
        "host.launcher": {
            "path": "bin/godot-codex-mcp",
            "sha256": mcp_binary_sha256,
            "path_lookup_used": False,
            "private_absolute_path_recorded": False,
        },
    }


def _finalize_trace(
    pending: Mapping[str, Any],
    *,
    observations: Mapping[str, bool],
) -> dict[str, Any]:
    require(
        set(observations) == set(AUTHORITY_OBSERVATIONS)
        and all(observations.values()),
        "external observation checklist is incomplete",
    )
    checked = _validate_pending_trace(pending)
    transport = _assertion_map(
        checked["assertions"],
        expected=TRANSPORT_ASSERTION_IDS,
        label="pending trace",
    )
    bindings = cast(Mapping[str, Any], checked["bindings"])
    host = _host_assertion_records(bindings["mcp_binary_sha256"])
    _validate_host_assertions(
        host,
        mcp_binary_sha256=bindings["mcp_binary_sha256"],
    )
    assertions = {**transport, **host}
    final = {
        key: value
        for key, value in checked.items()
        if key not in {"attestation_requirements", "redaction"}
    }
    final["schema_version"] = FINAL_TRACE_SCHEMA_VERSION
    final["status"] = FINAL_STATUS
    final["assertions"] = [
        {"assertion_id": name, "projection": assertions[name]}
        for name in sorted(assertions)
    ]
    final["redaction"] = acceptance.derive_surface_trace_redaction(final)
    _validate_final_trace(final)
    acceptance.safe_evidence_scan(final)
    return final


def _validate_final_trace(value: Any) -> dict[str, Any]:
    trace = _exact(value, FINAL_TRACE_FIELDS, label="final trace")
    require(
        trace["schema_version"] == FINAL_TRACE_SCHEMA_VERSION
        and trace["capture_kind"] == CAPTURE_KIND
        and trace["status"] == FINAL_STATUS
        and trace["surface"] in {"app", "cli", "ide"},
        "final trace identity differs",
    )
    _validate_host(trace["host"], label="final trace host")
    bindings = _validate_trace_bindings(trace["bindings"])
    _validate_registry(trace["registry"], label="final trace registry")
    assertions = _assertion_map(
        trace["assertions"],
        expected=FINAL_ASSERTION_IDS,
        label="final trace",
    )
    _validate_transport_assertions(
        {
            key: value
            for key, value in assertions.items()
            if key in TRANSPORT_ASSERTION_IDS
        }
    )
    _validate_host_assertions(
        {
            key: value
            for key, value in assertions.items()
            if key in HOST_ATTESTED_ASSERTIONS
        },
        mcp_binary_sha256=bindings["mcp_binary_sha256"],
    )
    acceptance._validate_form_outcomes(trace["form_outcomes"])
    acceptance._validate_revision_timeline(trace["revision_timeline"])
    redaction = _exact(
        trace["redaction"],
        {
            "absolute_paths_absent",
            "approval_content_absent",
            "native_ids_absent",
            "secrets_absent",
            "source_content_absent",
            "truncated",
        },
        label="final trace redaction",
    )
    require(
        redaction == acceptance.derive_surface_trace_redaction(trace),
        "final trace redaction differs",
    )
    return trace


def open_controlling_tty() -> TextIO:
    """Open only the process controlling TTY; stdin is never an authority."""

    flags = (
        os.O_RDWR
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    try:
        descriptor = os.open("/dev/tty", flags)
    except OSError as error:
        raise SurfaceArtifactError(
            "attestation requires a controlling TTY"
        ) from error
    try:
        metadata = os.fstat(descriptor)
        require(
            stat.S_ISCHR(metadata.st_mode) and os.isatty(descriptor),
            "attestation input is not a controlling TTY",
        )
        return os.fdopen(
            descriptor,
            "r+",
            encoding="utf-8",
            errors="strict",
            buffering=1,
            closefd=True,
        )
    except BaseException:
        os.close(descriptor)
        raise


def _tty_write(tty: TextIO, value: str) -> None:
    try:
        tty.write(value)
        tty.flush()
    except (OSError, UnicodeError) as error:
        raise SurfaceArtifactError("controlling TTY write failed") from error


def _tty_line(tty: TextIO, prompt: str) -> str:
    require(tty.isatty(), "attestation input is not a controlling TTY")
    _tty_write(tty, prompt)
    try:
        line = tty.readline()
    except (OSError, UnicodeError) as error:
        raise SurfaceArtifactError("controlling TTY read failed") from error
    require(line != "", "attestation checklist ended before completion")
    require(
        line.endswith("\n") and "\0" not in line,
        "attestation response is incomplete",
    )
    return line[:-1].rstrip("\r")


def _operator_id(tty: TextIO) -> str:
    value = _tty_line(
        tty,
        "External operator identifier (stored only as a domain-separated hash): ",
    ).strip()
    encoded = value.encode("utf-8")
    require(
        1 <= len(encoded) <= MAX_OPERATOR_ID_BYTES
        and not any(character.isspace() and character != " " for character in value),
        "operator identifier differs",
    )
    return "operator:sha256:" + hashlib.sha256(
        OPERATOR_ID_DOMAIN + encoded
    ).hexdigest()


def collect_attestation(tty: TextIO) -> tuple[str, dict[str, bool]]:
    """Collect one operator id and exactly one explicit yes/no per observation."""

    require(tty.isatty(), "attestation requires a controlling TTY")
    operator = _operator_id(tty)
    observations: dict[str, bool] = {}
    for name in AUTHORITY_OBSERVATIONS:
        response = _tty_line(
            tty,
            f"{OBSERVATION_PROMPTS[name]} Type exactly yes or no: ",
        ).strip().lower()
        require(
            response in {"yes", "no"},
            f"attestation response for {name} is not explicit yes/no",
        )
        if response == "no":
            raise SurfaceArtifactError(
                f"attestation declined: {name}; no artifacts were published"
            )
        observations[name] = True
    require(
        set(observations) == set(AUTHORITY_OBSERVATIONS),
        "attestation checklist is incomplete",
    )
    return operator, observations


def _surface_artifact_paths(surface: str) -> dict[str, str]:
    require(surface in {"app", "cli", "ide"}, "surface differs")
    directory = f"tests/codex/acquisition/sprint11/surfaces/{surface}"
    return {
        "directory": directory,
        "journal": f"{directory}/recorder-journal.json",
        "trace": f"{directory}/trace.json",
        "authority": f"{directory}/authority.json",
    }


def _authority_document(
    *,
    operator_id: str,
    observations: Mapping[str, bool],
    trace: Mapping[str, Any],
    trace_payload: bytes,
    journal_payload: bytes,
) -> dict[str, Any]:
    require(
        re.fullmatch(r"operator:sha256:[0-9a-f]{64}", operator_id) is not None,
        "operator identifier hash differs",
    )
    require(
        set(observations) == set(AUTHORITY_OBSERVATIONS)
        and all(observations.values()),
        "authority observations differ",
    )
    surface = cast(str, trace["surface"])
    paths = _surface_artifact_paths(surface)
    trace_bindings = cast(Mapping[str, Any], trace["bindings"])
    projection: dict[str, Any] = {
        "schema_version": AUTHORITY_SCHEMA_VERSION,
        "authority_kind": "external_operator_attestation",
        "trust_boundary": AUTHORITY_TRUST_BOUNDARY,
        "operator_id": operator_id,
        "surface": surface,
        "artifact_directory": paths["directory"],
        "bindings": {
            "package_source_commit": trace_bindings[
                "package_source_commit"
            ],
            "package_manifest_sha256": trace_bindings[
                "package_manifest_sha256"
            ],
            "host_provenance_sha256": trace_bindings[
                "host_provenance_sha256"
            ],
            "host_artifact_sha256": trace_bindings[
                "host_artifact_sha256"
            ],
            "client_artifact_sha256": trace_bindings[
                "client_artifact_sha256"
            ],
            "mcp_binary_sha256": trace_bindings["mcp_binary_sha256"],
            "project_fixture_sha256": trace_bindings[
                "project_fixture_sha256"
            ],
            "prompt_pack_sha256": trace_bindings["prompt_pack_sha256"],
            "trace_path": paths["trace"],
            "trace_sha256": sha256_bytes(trace_payload),
            "recorder_journal_path": paths["journal"],
            "recorder_journal_sha256": sha256_bytes(journal_payload),
        },
        "observations": {
            name: observations[name] for name in AUTHORITY_OBSERVATIONS
        },
    }
    authority = {
        **projection,
        "attestation_projection_sha256": sha256_bytes(
            AUTHORITY_DOMAIN + canonical_json(projection)
        ),
    }
    acceptance.safe_evidence_scan(authority)
    return authority


def _write_staged_file(path: Path, payload: bytes) -> None:
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    try:
        descriptor = os.open(path, flags, 0o600)
    except OSError as error:
        raise SurfaceArtifactError("staged artifact cannot be created") from error
    try:
        offset = 0
        while offset < len(payload):
            written = os.write(descriptor, payload[offset:])
            require(written > 0, "staged artifact write did not advance")
            offset += written
        os.fsync(descriptor)
        metadata = os.fstat(descriptor)
        require(
            stat.S_ISREG(metadata.st_mode)
            and metadata.st_size == len(payload)
            and stat.S_IMODE(metadata.st_mode) == 0o600,
            "staged artifact identity differs",
        )
    except OSError as error:
        raise SurfaceArtifactError("staged artifact write failed") from error
    finally:
        os.close(descriptor)


def _ensure_surface_parent(repository_root: Path) -> Path:
    """Create only the canonical ``surfaces`` parent without following links."""

    sprint_root = (
        repository_root
        / "tests"
        / "codex"
        / "acquisition"
        / "sprint11"
    )
    sprint_root = _canonical_path(
        sprint_root,
        label="Sprint 11 acquisition root",
        directory=True,
    )
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    require(
        getattr(os, "O_NOFOLLOW", 0) != 0
        and getattr(os, "O_DIRECTORY", 0) != 0,
        "safe surface parent creation is unavailable",
    )
    try:
        parent_descriptor = os.open(sprint_root, flags)
    except OSError as error:
        raise SurfaceArtifactError(
            "Sprint 11 acquisition root cannot be opened safely"
        ) from error
    try:
        before = os.fstat(parent_descriptor)
        require(
            stat.S_ISDIR(before.st_mode)
            and before.st_uid == os.geteuid(),
            "Sprint 11 acquisition root ownership differs",
        )
        try:
            os.mkdir(
                "surfaces",
                mode=0o700,
                dir_fd=parent_descriptor,
            )
            os.fsync(parent_descriptor)
        except FileExistsError:
            pass
        except OSError as error:
            raise SurfaceArtifactError(
                "surface acquisition parent cannot be created safely"
            ) from error
        try:
            surface_descriptor = os.open(
                "surfaces",
                flags,
                dir_fd=parent_descriptor,
            )
        except OSError as error:
            raise SurfaceArtifactError(
                "surface acquisition parent cannot be opened safely"
            ) from error
        try:
            surface_metadata = os.fstat(surface_descriptor)
            require(
                stat.S_ISDIR(surface_metadata.st_mode)
                and surface_metadata.st_uid == os.geteuid()
                and stat.S_IMODE(surface_metadata.st_mode) & 0o022 == 0,
                "surface acquisition parent identity differs",
            )
        finally:
            os.close(surface_descriptor)
    finally:
        os.close(parent_descriptor)
    return sprint_root / "surfaces"


def publish_attested_surface(
    *,
    repository_root: Path,
    output_directory: Path,
    metadata: Mapping[str, Any],
    metadata_payload: bytes,
    journal: Mapping[str, Any],
    journal_payload: bytes,
    pending: Mapping[str, Any],
    pending_payload: bytes,
    tty: TextIO,
) -> dict[str, Any]:
    """Collect authority, then atomically publish one new canonical directory."""

    checked_metadata = validate_metadata(metadata)
    require(
        metadata_payload == canonical_artifact(checked_metadata),
        "attestation metadata bytes differ",
    )
    expected_pending = derive_pending_trace(
        metadata=checked_metadata,
        metadata_payload=metadata_payload,
        journal=journal,
        journal_payload=journal_payload,
    )
    require(
        pending_payload == canonical_artifact(expected_pending)
        and pending == expected_pending,
        "pending trace is not the deterministic journal derivation",
    )
    repository_root = _canonical_path(
        repository_root,
        label="repository root",
        directory=True,
    )
    surface = cast(str, expected_pending["surface"])
    expected = (
        repository_root
        / "tests"
        / "codex"
        / "acquisition"
        / "sprint11"
        / "surfaces"
        / surface
    )
    require(
        output_directory.is_absolute()
        and Path(os.path.abspath(output_directory)) == expected,
        "surface output directory is not the canonical repository path",
    )
    try:
        output_directory.lstat()
    except FileNotFoundError:
        pass
    except OSError as error:
        raise SurfaceArtifactError(
            "surface output directory is unavailable"
        ) from error
    else:
        raise SurfaceArtifactError(
            "surface output directory already exists"
        )
    operator_id, observations = collect_attestation(tty)
    final_trace = _finalize_trace(
        expected_pending,
        observations=observations,
    )
    trace_payload = canonical_artifact(final_trace)
    authority = _authority_document(
        operator_id=operator_id,
        observations=observations,
        trace=final_trace,
        trace_payload=trace_payload,
        journal_payload=journal_payload,
    )
    authority_payload = canonical_artifact(authority)

    surface_parent = _ensure_surface_parent(repository_root)
    require(
        output_directory.parent == surface_parent,
        "surface output parent differs",
    )
    try:
        staged = acquisition_paths.prepare_repository_directory(
            output_directory,
            repository=repository_root,
            prefix=f".s11-{surface}-attest-",
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise SurfaceArtifactError(
            "surface output directory is unsafe or already exists"
        ) from error
    published = False
    try:
        _write_staged_file(
            staged.staging / "recorder-journal.json",
            journal_payload,
        )
        _write_staged_file(staged.staging / "trace.json", trace_payload)
        _write_staged_file(
            staged.staging / "authority.json",
            authority_payload,
        )
        staged.publish()
        published = True
    except acquisition_paths.AcquisitionPathError as error:
        raise SurfaceArtifactError(
            "surface output publication failed closed"
        ) from error
    finally:
        if not published and staged.staging.exists():
            shutil.rmtree(staged.staging)
    return {
        "schema_version": "s11-surface-publication/1.0",
        "status": "published",
        "surface": surface,
        "artifact_directory": _surface_artifact_paths(surface)["directory"],
        "recorder_journal_sha256": sha256_bytes(journal_payload),
        "trace_sha256": sha256_bytes(trace_payload),
        "authority_sha256": sha256_bytes(authority_payload),
    }


def _write_new_json(path: Path, document: Mapping[str, Any], *, prefix: str) -> None:
    try:
        acquisition_paths.atomic_write_new_file(
            path,
            canonical_artifact(document),
            prefix=prefix,
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise SurfaceArtifactError(
            "output path is unsafe or already exists"
        ) from error


def _publish_derivation_bundle(
    output_directory: Path,
    *,
    journal_payload: bytes,
    pending_payload: bytes,
) -> None:
    require(
        output_directory.is_absolute()
        and output_directory.name not in {"", ".", ".."},
        "derivation output directory is not an absolute new path",
    )
    lexical = Path(os.path.abspath(output_directory))
    try:
        parent = lexical.parent.resolve(strict=True)
        parent_metadata = parent.lstat()
    except OSError as error:
        raise SurfaceArtifactError(
            "derivation output parent is unavailable"
        ) from error
    require(
        stat.S_ISDIR(parent_metadata.st_mode)
        and not stat.S_ISLNK(parent_metadata.st_mode),
        "derivation output parent is unsafe",
    )
    target = parent / lexical.name
    try:
        target.lstat()
    except FileNotFoundError:
        pass
    except OSError as error:
        raise SurfaceArtifactError(
            "derivation output target is unavailable"
        ) from error
    else:
        raise SurfaceArtifactError(
            "derivation output directory already exists"
        )
    staging = Path(
        tempfile.mkdtemp(prefix=".s11-derived-", dir=parent)
    )
    os.chmod(staging, 0o700)
    published = False
    try:
        _write_staged_file(
            staging / "recorder-journal.json",
            journal_payload,
        )
        _write_staged_file(
            staging / "pending-trace.json",
            pending_payload,
        )
        try:
            acquisition_paths._exclusive_rename(staging, target)
        except acquisition_paths.AcquisitionPathError as error:
            raise SurfaceArtifactError(
                "derivation output publication failed closed"
            ) from error
        published = True
    finally:
        if not published and staging.exists():
            shutil.rmtree(staging)


def _prepare_command(arguments: argparse.Namespace) -> dict[str, Any]:
    metadata = prepare_metadata(
        surface=arguments.surface,
        package_manifest=arguments.package_manifest,
        data_root=arguments.data_root,
        measurement_path=arguments.measurement,
        fixture_root=arguments.fixture_root,
        repository_root=arguments.repository_root,
    )
    _write_new_json(arguments.output, metadata, prefix=".s11-metadata-")
    return {
        "schema_version": "s11-surface-artifact-command/1.0",
        "stage": "prepare",
        "status": "prepared",
        "surface": arguments.surface,
        "output_sha256": sha256_bytes(canonical_artifact(metadata)),
    }


def _derive_command(arguments: argparse.Namespace) -> dict[str, Any]:
    metadata, metadata_payload = load_metadata(arguments.metadata)
    capture, capture_payload = load_raw_capture(arguments.capture)
    journal = canonical_journal_from_raw_capture(
        metadata=metadata,
        metadata_payload=metadata_payload,
        capture=capture,
        capture_payload=capture_payload,
    )
    journal_payload = canonical_artifact(journal)
    pending = derive_pending_trace(
        metadata=metadata,
        metadata_payload=metadata_payload,
        journal=journal,
        journal_payload=journal_payload,
    )
    pending_payload = canonical_artifact(pending)
    _publish_derivation_bundle(
        arguments.output_directory,
        journal_payload=journal_payload,
        pending_payload=pending_payload,
    )
    return {
        "schema_version": "s11-surface-artifact-command/1.0",
        "stage": "derive",
        "status": PENDING_STATUS,
        "surface": metadata["surface"],
        "capture_journal_sha256": sha256_bytes(capture_payload),
        "recorder_journal_sha256": sha256_bytes(journal_payload),
        "pending_trace_sha256": sha256_bytes(pending_payload),
    }


def _attest_command(
    arguments: argparse.Namespace,
    *,
    tty_factory: Callable[[], TextIO] = open_controlling_tty,
) -> dict[str, Any]:
    metadata, metadata_payload = load_metadata(arguments.metadata)
    journal, journal_payload = _canonical_document(
        arguments.journal,
        label="recorder journal",
    )
    pending, pending_payload = _canonical_document(
        arguments.trace,
        label="pending trace",
    )
    with tty_factory() as tty:
        return publish_attested_surface(
            repository_root=arguments.repository_root,
            output_directory=arguments.output_directory,
            metadata=metadata,
            metadata_payload=metadata_payload,
            journal=journal,
            journal_payload=journal_payload,
            pending=pending,
            pending_payload=pending_payload,
            tty=tty,
        )


def parser() -> argparse.ArgumentParser:
    command = argparse.ArgumentParser(
        description="Create fail-closed Sprint 11 surface artifacts.",
    )
    subcommands = command.add_subparsers(dest="stage", required=True)

    prepare = subcommands.add_parser(
        "prepare",
        help="measure non-human package, Git, host, and fixture inputs",
    )
    prepare.add_argument("--surface", choices=("app", "cli", "ide"), required=True)
    prepare.add_argument(
        "--package-manifest",
        type=Path,
        required=True,
        help="exact detached sprint11-package-manifest.json",
    )
    prepare.add_argument(
        "--data-root",
        type=Path,
        required=True,
        help="installed package data root containing versions/<version>",
    )
    prepare.add_argument(
        "--measurement",
        type=Path,
        required=True,
        help="exact measured host-provenance measurement.json",
    )
    prepare.add_argument(
        "--fixture-root",
        type=Path,
        required=True,
        help="canonical runtime fixture copy",
    )
    prepare.add_argument("--repository-root", type=Path, required=True)
    prepare.add_argument("--output", type=Path, required=True)

    derive = subcommands.add_parser(
        "derive",
        help=(
            "validate one private Rust capture and derive canonical journal/"
            "pending trace"
        ),
    )
    derive.add_argument("--metadata", type=Path, required=True)
    derive.add_argument(
        "--capture",
        type=Path,
        required=True,
        help="private sprint11-surface-capture-artifact/1.1",
    )
    derive.add_argument(
        "--output-directory",
        type=Path,
        required=True,
        help="new directory for recorder-journal.json and pending-trace.json",
    )

    attest = subcommands.add_parser(
        "attest",
        help="interactively attest and publish one canonical surface directory",
    )
    attest.add_argument("--metadata", type=Path, required=True)
    attest.add_argument("--journal", type=Path, required=True)
    attest.add_argument("--trace", type=Path, required=True)
    attest.add_argument("--repository-root", type=Path, required=True)
    attest.add_argument("--output-directory", type=Path, required=True)
    return command


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        if arguments.stage == "prepare":
            result = _prepare_command(arguments)
        elif arguments.stage == "derive":
            result = _derive_command(arguments)
        elif arguments.stage == "attest":
            result = _attest_command(arguments)
        else:  # pragma: no cover - argparse closes this branch.
            raise SurfaceArtifactError("surface artifact stage differs")
    except SurfaceArtifactError as error:
        print(
            canonical_json(
                {
                    "schema_version": "s11-surface-artifact-error/1.0",
                    "status": "rejected",
                    "error": str(error),
                }
            ).decode("utf-8"),
            file=sys.stderr,
        )
        return 2
    print(canonical_json(result).decode("utf-8"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
