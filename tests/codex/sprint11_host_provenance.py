#!/usr/bin/env python3
"""Acquire bounded, measured provenance for Sprint 11 Codex host surfaces.

The host-coordinate profile is an expected coordinate, not evidence that the
installed host still matches it.  This acquisition reads the installed
artifacts itself, verifies their macOS code signatures, hashes the complete
official IDE extension tree, and emits only content-free measurements.

No path supplied to this program is retained in the receipt.  Symlinks,
special files, unstable trees, oversized artifacts, signature failures, and
profile mismatches fail closed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import signal
import stat
import subprocess
import sys
import tempfile
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final

try:
    from tests.codex import sprint11_acquisition_paths as acquisition_paths
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_acquisition_paths as acquisition_paths

SCHEMA_VERSION: Final = "s11-host-provenance/1.0"
CAPTURE_KIND: Final = "measured_local_host_provenance"
TREE_DOMAIN: Final = b"godot-codex/s11-host-tree/v1\0"
MAX_PROFILE_BYTES: Final = 256 * 1024
MAX_OUTPUT_BYTES: Final = 64 * 1024
MAX_CODESIGN_OUTPUT_BYTES: Final = 128 * 1024
MAX_CODESIGN_SECONDS: Final = 30.0
MAX_FILE_BYTES: Final = 512 * 1024 * 1024
MAX_TREE_BYTES: Final = 1024 * 1024 * 1024
MAX_TREE_ENTRIES: Final = 16_384
MAX_PATH_BYTES: Final = 2_048
READ_CHUNK_BYTES: Final = 1024 * 1024
SEGMENT_RE: Final = re.compile(r"[A-Za-z0-9._+@(),= -]{1,255}\Z")
DIGEST_RE: Final = re.compile(r"sha256:[0-9a-f]{64}\Z")
TOKEN_RE: Final = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+:-]{0,127}\Z")
CDHASH_RE: Final = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?\Z")
PROCESS_SCOPE_ENV_RE: Final = re.compile(
    r"GODOT_CODEX_PROCESS_SCOPE_[0-9a-f]{48}\Z"
)


class HostProvenanceError(RuntimeError):
    """The installed host coordinate cannot produce qualifying evidence."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise HostProvenanceError(message)


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
        raise HostProvenanceError("host provenance is not canonical JSON") from error


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
        raise HostProvenanceError(f"{label} contains a non-finite number")

    try:
        value = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=pairs,
            parse_constant=reject_constant,
        )
    except (UnicodeError, json.JSONDecodeError) as error:
        raise HostProvenanceError(f"{label} is invalid JSON") from error
    require(isinstance(value, dict), f"{label} root must be an object")
    return value


def _canonical_nonsymlink_path(path: Path, *, kind: str) -> Path:
    require(path.is_absolute(), f"{kind} path must be absolute")
    try:
        lexical = Path(os.path.abspath(path))
        resolved = path.resolve(strict=True)
        metadata = path.lstat()
    except OSError as error:
        raise HostProvenanceError(f"{kind} path is unavailable") from error
    require(
        lexical == resolved and not stat.S_ISLNK(metadata.st_mode),
        f"{kind} path must be canonical and symlink-free",
    )
    return resolved


def _safe_file_digest(path: Path, *, label: str) -> tuple[str, int]:
    path = _canonical_nonsymlink_path(path, kind=label)
    try:
        metadata = path.lstat()
    except OSError as error:
        raise HostProvenanceError(f"{label} is unavailable") from error
    require(stat.S_ISREG(metadata.st_mode), f"{label} is not a regular file")
    require(
        0 <= metadata.st_size <= MAX_FILE_BYTES,
        f"{label} exceeds its byte bound",
    )
    digest = hashlib.sha256()
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise HostProvenanceError(f"{label} cannot be opened safely") from error
    try:
        before = os.fstat(descriptor)
        require(
            stat.S_ISREG(before.st_mode)
            and before.st_dev == metadata.st_dev
            and before.st_ino == metadata.st_ino
            and before.st_size == metadata.st_size,
            f"{label} changed before hashing",
        )
        remaining = MAX_FILE_BYTES + 1
        total = 0
        while True:
            chunk = os.read(descriptor, min(READ_CHUNK_BYTES, remaining))
            if not chunk:
                break
            total += len(chunk)
            remaining -= len(chunk)
            require(remaining >= 0, f"{label} exceeds its byte bound")
            digest.update(chunk)
        after = os.fstat(descriptor)
        require(
            total == before.st_size
            and (
                after.st_dev,
                after.st_ino,
                after.st_size,
                after.st_mtime_ns,
                after.st_ctime_ns,
            )
            == (
                before.st_dev,
                before.st_ino,
                before.st_size,
                before.st_mtime_ns,
                before.st_ctime_ns,
            ),
            f"{label} changed while hashing",
        )
    except OSError as error:
        raise HostProvenanceError(f"{label} cannot be read safely") from error
    finally:
        os.close(descriptor)
    return "sha256:" + digest.hexdigest(), total


@dataclass(frozen=True)
class TreeMeasurement:
    sha256: str
    file_count: int
    directory_count: int
    total_bytes: int


@dataclass
class _TreeState:
    digest: Any
    file_count: int = 0
    directory_count: int = 0
    total_bytes: int = 0


def _tree_record(state: _TreeState, record: Mapping[str, Any]) -> None:
    state.digest.update(canonical_json(record))
    state.digest.update(b"\n")


def _safe_segment(name: str) -> bytes:
    encoded = name.encode("utf-8", errors="strict")
    require(
        name not in {"", ".", ".."}
        and "/" not in name
        and "\x00" not in name
        and SEGMENT_RE.fullmatch(name) is not None,
        "extension tree contains an unsafe path segment",
    )
    return encoded


def _stable_stat(before: os.stat_result, after: os.stat_result) -> bool:
    return (
        before.st_dev,
        before.st_ino,
        before.st_mode,
        before.st_size,
        before.st_mtime_ns,
        before.st_ctime_ns,
    ) == (
        after.st_dev,
        after.st_ino,
        after.st_mode,
        after.st_size,
        after.st_mtime_ns,
        after.st_ctime_ns,
    )


def _walk_tree(
    descriptor: int,
    *,
    relative_parts: tuple[str, ...],
    state: _TreeState,
) -> None:
    try:
        before_directory = os.fstat(descriptor)
        names = os.listdir(descriptor)
    except OSError as error:
        raise HostProvenanceError("extension tree cannot be enumerated") from error
    require(
        stat.S_ISDIR(before_directory.st_mode),
        "extension tree contains a non-directory root",
    )
    for name in sorted(names, key=_safe_segment):
        _safe_segment(name)
        parts = (*relative_parts, name)
        relative = "/".join(parts)
        require(
            len(relative.encode("utf-8")) <= MAX_PATH_BYTES,
            "extension tree path exceeds its byte bound",
        )
        require(
            state.file_count + state.directory_count < MAX_TREE_ENTRIES,
            "extension tree exceeds its entry bound",
        )
        try:
            observed = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
        except OSError as error:
            raise HostProvenanceError("extension tree entry is unavailable") from error
        require(
            observed.st_uid == os.getuid() and observed.st_mode & 0o022 == 0,
            "extension tree ownership or permissions differ",
        )
        if stat.S_ISDIR(observed.st_mode):
            flags = (
                os.O_RDONLY
                | getattr(os, "O_CLOEXEC", 0)
                | getattr(os, "O_NOFOLLOW", 0)
                | getattr(os, "O_DIRECTORY", 0)
            )
            try:
                child = os.open(name, flags, dir_fd=descriptor)
            except OSError as error:
                raise HostProvenanceError(
                    "extension directory cannot be opened safely"
                ) from error
            try:
                opened = os.fstat(child)
                require(
                    _stable_stat(observed, opened),
                    "extension directory changed before hashing",
                )
                state.directory_count += 1
                _tree_record(
                    state,
                    {
                        "kind": "directory",
                        "mode": stat.S_IMODE(opened.st_mode),
                        "path": relative,
                    },
                )
                _walk_tree(child, relative_parts=parts, state=state)
                require(
                    _stable_stat(opened, os.fstat(child)),
                    "extension directory changed while hashing",
                )
            finally:
                os.close(child)
            continue
        require(
            stat.S_ISREG(observed.st_mode),
            "extension tree contains a symlink or special file",
        )
        require(
            0 <= observed.st_size <= MAX_FILE_BYTES,
            "extension file exceeds its byte bound",
        )
        flags = (
            os.O_RDONLY
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_NOFOLLOW", 0)
        )
        try:
            child = os.open(name, flags, dir_fd=descriptor)
        except OSError as error:
            raise HostProvenanceError(
                "extension file cannot be opened safely"
            ) from error
        try:
            opened = os.fstat(child)
            require(
                stat.S_ISREG(opened.st_mode) and _stable_stat(observed, opened),
                "extension file changed before hashing",
            )
            file_digest = hashlib.sha256()
            total = 0
            while True:
                chunk = os.read(child, READ_CHUNK_BYTES)
                if not chunk:
                    break
                total += len(chunk)
                require(
                    total <= MAX_FILE_BYTES,
                    "extension file exceeds its byte bound",
                )
                file_digest.update(chunk)
            after = os.fstat(child)
            require(
                total == opened.st_size and _stable_stat(opened, after),
                "extension file changed while hashing",
            )
        except OSError as error:
            raise HostProvenanceError(
                "extension file cannot be read safely"
            ) from error
        finally:
            os.close(child)
        state.file_count += 1
        state.total_bytes += total
        require(
            state.total_bytes <= MAX_TREE_BYTES,
            "extension tree exceeds its aggregate byte bound",
        )
        _tree_record(
            state,
            {
                "bytes": total,
                "kind": "file",
                "mode": stat.S_IMODE(opened.st_mode),
                "path": relative,
                "sha256": "sha256:" + file_digest.hexdigest(),
            },
        )
    try:
        after_directory = os.fstat(descriptor)
    except OSError as error:
        raise HostProvenanceError("extension tree stability is unavailable") from error
    require(
        _stable_stat(before_directory, after_directory),
        "extension directory changed while hashing",
    )


def _tree_digest_once(root: Path) -> TreeMeasurement:
    root = _canonical_nonsymlink_path(root, kind="extension root")
    try:
        metadata = root.lstat()
    except OSError as error:
        raise HostProvenanceError("extension root is unavailable") from error
    require(
        stat.S_ISDIR(metadata.st_mode)
        and metadata.st_uid == os.getuid()
        and metadata.st_mode & 0o022 == 0,
        "extension root ownership or permissions differ",
    )
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_DIRECTORY", 0)
    )
    try:
        descriptor = os.open(root, flags)
    except OSError as error:
        raise HostProvenanceError("extension root cannot be opened safely") from error
    try:
        opened = os.fstat(descriptor)
        require(
            _stable_stat(metadata, opened),
            "extension root changed before hashing",
        )
        digest = hashlib.sha256(TREE_DOMAIN)
        state = _TreeState(digest=digest)
        _walk_tree(descriptor, relative_parts=(), state=state)
        require(
            _stable_stat(opened, os.fstat(descriptor)),
            "extension root changed while hashing",
        )
    finally:
        os.close(descriptor)
    require(state.file_count > 0, "extension tree is empty")
    return TreeMeasurement(
        sha256="sha256:" + state.digest.hexdigest(),
        file_count=state.file_count,
        directory_count=state.directory_count,
        total_bytes=state.total_bytes,
    )


def stable_tree_digest(root: Path) -> TreeMeasurement:
    """Hash a symlink-free tree twice and require one stable measurement."""

    first = _tree_digest_once(root)
    second = _tree_digest_once(root)
    require(first == second, "extension tree changed between measurements")
    return first


@dataclass(frozen=True)
class CommandResult:
    returncode: int
    stdout: bytes
    stderr: bytes


CommandRunner = Callable[[Sequence[str], float], CommandResult]


def command_environment() -> dict[str, str]:
    environment = {
        "HOME": os.environ.get("HOME", ""),
        "LANG": "C",
        "LC_ALL": "C",
        "PATH": "/usr/bin:/bin",
    }
    for name, value in os.environ.items():
        if not name.startswith("GODOT_CODEX_PROCESS_SCOPE_"):
            continue
        require(
            PROCESS_SCOPE_ENV_RE.fullmatch(name) is not None
            and value == "1",
            "process scope environment differs",
        )
        environment[name] = value
    return environment


def run_bounded_command(argv: Sequence[str], timeout: float) -> CommandResult:
    require(argv and timeout > 0, "code-sign command differs")
    process: subprocess.Popen[bytes] | None = None
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        try:
            process = subprocess.Popen(
                list(argv),
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                env=command_environment(),
                start_new_session=True,
            )
            try:
                returncode = process.wait(timeout=timeout)
            except subprocess.TimeoutExpired as error:
                _stop_process_group(process)
                raise HostProvenanceError("code-sign command timed out") from error
            try:
                os.killpg(process.pid, 0)
            except ProcessLookupError:
                pass
            except OSError as error:
                raise HostProvenanceError(
                    "code-sign process-group state is unavailable"
                ) from error
            else:
                _stop_process_group(process)
                raise HostProvenanceError(
                    "code-sign command left a running descendant"
                )
        except OSError as error:
            if process is not None:
                _stop_process_group(process)
            raise HostProvenanceError("code-sign command could not run") from error
        stdout_size = os.fstat(stdout.fileno()).st_size
        stderr_size = os.fstat(stderr.fileno()).st_size
        require(
            stdout_size <= MAX_CODESIGN_OUTPUT_BYTES
            and stderr_size <= MAX_CODESIGN_OUTPUT_BYTES,
            "code-sign output exceeds its byte bound",
        )
        stdout.seek(0)
        stderr.seek(0)
        return CommandResult(
            returncode=returncode,
            stdout=stdout.read(MAX_CODESIGN_OUTPUT_BYTES + 1),
            stderr=stderr.read(MAX_CODESIGN_OUTPUT_BYTES + 1),
        )


def _stop_process_group(process: subprocess.Popen[bytes]) -> None:
    for requested_signal in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(process.pid, requested_signal)
        except ProcessLookupError:
            return
        except OSError as error:
            raise HostProvenanceError(
                "code-sign process group cannot be stopped"
            ) from error
        try:
            process.wait(timeout=1.0)
        except subprocess.TimeoutExpired:
            continue
        return
    raise HostProvenanceError("code-sign process group did not stop")


def parse_codesign_display(output: bytes, *, mode: str) -> dict[str, Any]:
    require(mode in {"strict", "deep_strict"}, "code-sign mode differs")
    try:
        text = output.decode("utf-8")
    except UnicodeError as error:
        raise HostProvenanceError("code-sign display is not UTF-8") from error
    values: dict[str, list[str]] = {
        "Identifier": [],
        "TeamIdentifier": [],
        "CDHash": [],
    }
    for line in text.splitlines():
        for key in values:
            prefix = f"{key}="
            if line.startswith(prefix):
                values[key].append(line[len(prefix) :])
    require(
        all(len(items) == 1 for items in values.values()),
        "code-sign coordinate is incomplete or ambiguous",
    )
    identifier = values["Identifier"][0]
    team_id = values["TeamIdentifier"][0]
    cdhash = values["CDHash"][0]
    require(
        TOKEN_RE.fullmatch(identifier) is not None
        and TOKEN_RE.fullmatch(team_id) is not None
        and CDHASH_RE.fullmatch(cdhash) is not None,
        "code-sign coordinate contains an unsafe value",
    )
    return {
        "verified": True,
        "mode": mode,
        "identifier": identifier,
        "team_id": team_id,
        "cdhash": cdhash,
    }


def measure_code_signature(
    path: Path,
    *,
    deep: bool,
    runner: CommandRunner = run_bounded_command,
) -> dict[str, Any]:
    path = _canonical_nonsymlink_path(path, kind="signed artifact")
    metadata = path.lstat()
    require(
        stat.S_ISDIR(metadata.st_mode) if deep else stat.S_ISREG(metadata.st_mode),
        "signed artifact kind differs",
    )
    verify = ["/usr/bin/codesign", "--verify"]
    if deep:
        verify.append("--deep")
    verify.extend(["--strict", "--verbose=2", str(path)])
    verified = runner(verify, MAX_CODESIGN_SECONDS)
    require(verified.returncode == 0, "installed artifact code signature is invalid")
    display = runner(
        ["/usr/bin/codesign", "-d", "--verbose=4", str(path)],
        MAX_CODESIGN_SECONDS,
    )
    require(display.returncode == 0, "installed artifact signature cannot be read")
    return parse_codesign_display(
        display.stdout + b"\n" + display.stderr,
        mode="deep_strict" if deep else "strict",
    )


def _profile(path: Path) -> tuple[dict[str, Any], str]:
    path = _canonical_nonsymlink_path(path, kind="host-coordinate profile")
    try:
        payload = acquisition_paths.read_regular_file(
            path,
            maximum=MAX_PROFILE_BYTES,
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise HostProvenanceError("host-coordinate profile is unavailable") from error
    document = _strict_json(
        payload,
        label="host-coordinate profile",
        maximum=MAX_PROFILE_BYTES,
    )
    surfaces = document.get("surfaces")
    require(
        document.get("schema_version")
        == "godot-codex-host-coordinate-profile/1.0"
        and isinstance(surfaces, list)
        and len(surfaces) == 3,
        "host-coordinate profile shape differs",
    )
    return document, sha256_bytes(payload)


def _signature_expectation(value: Any, *, label: str) -> dict[str, Any] | None:
    if value is None:
        return None
    require(
        isinstance(value, dict)
        and set(value) == {"mode", "identifier", "team_id", "cdhash"},
        f"{label} signature coordinate differs",
    )
    require(
        value["mode"] in {"strict", "deep_strict"}
        and isinstance(value["identifier"], str)
        and TOKEN_RE.fullmatch(value["identifier"]) is not None
        and isinstance(value["team_id"], str)
        and TOKEN_RE.fullmatch(value["team_id"]) is not None
        and isinstance(value["cdhash"], str)
        and CDHASH_RE.fullmatch(value["cdhash"]) is not None,
        f"{label} signature coordinate is unsafe",
    )
    return value


def _matches_signature(observed: Any, expected: Any, *, label: str) -> None:
    coordinate = _signature_expectation(expected, label=label)
    if coordinate is None:
        require(observed is None, f"{label} unexpectedly has a signature")
        return
    require(
        isinstance(observed, dict)
        and observed.get("verified") is True
        and {
            key: observed.get(key)
            for key in ("mode", "identifier", "team_id", "cdhash")
        }
        == coordinate,
        f"{label} signature differs from the frozen profile",
    )


def _profile_surfaces(profile_document: Mapping[str, Any]) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    surfaces = profile_document["surfaces"]
    assert isinstance(surfaces, list)
    for value in surfaces:
        require(isinstance(value, dict), "host profile surface differs")
        surface = value.get("surface")
        require(
            surface in {"app", "cli", "ide"} and surface not in result,
            "host profile surface coverage differs",
        )
        result[surface] = value
    require(set(result) == {"app", "cli", "ide"}, "host profile surface set differs")
    return result


def validate_measurements_against_profile(
    profile_document: Mapping[str, Any],
    measurements: Sequence[Mapping[str, Any]],
) -> None:
    expected = _profile_surfaces(profile_document)
    observed: dict[str, Mapping[str, Any]] = {}
    for measurement in measurements:
        surface = measurement.get("surface")
        require(
            surface in expected and surface not in observed,
            "measured host surface coverage differs",
        )
        observed[str(surface)] = measurement
    require(set(observed) == set(expected), "measured host surface set differs")
    fields = (
        "host_artifact_sha256",
        "host_metadata_sha256",
        "client_artifact_sha256",
        "ide_shell_artifact_sha256",
    )
    for surface in ("app", "cli", "ide"):
        coordinate = expected[surface]
        measurement = observed[surface]
        for field in fields:
            expected_value = coordinate.get(field)
            require(
                expected_value is None
                or (
                    isinstance(expected_value, str)
                    and DIGEST_RE.fullmatch(expected_value) is not None
                ),
                f"{surface} {field} profile value differs",
            )
            require(
                measurement.get(field) == expected_value,
                f"{surface} {field} differs from the frozen profile",
            )
        _matches_signature(
            measurement.get("host_code_signature"),
            coordinate.get("host_code_signature"),
            label=f"{surface} host",
        )
        _matches_signature(
            measurement.get("client_code_signature"),
            coordinate.get("client_code_signature"),
            label=f"{surface} client",
        )
        _matches_signature(
            measurement.get("ide_shell_code_signature"),
            coordinate.get("ide_shell_code_signature"),
            label=f"{surface} IDE shell",
        )


def _observed_signature(value: Any, *, label: str) -> None:
    require(
        isinstance(value, dict)
        and set(value)
        == {"verified", "mode", "identifier", "team_id", "cdhash"}
        and value["verified"] is True
        and value["mode"] in {"strict", "deep_strict"}
        and isinstance(value["identifier"], str)
        and TOKEN_RE.fullmatch(value["identifier"]) is not None
        and isinstance(value["team_id"], str)
        and TOKEN_RE.fullmatch(value["team_id"]) is not None
        and isinstance(value["cdhash"], str)
        and CDHASH_RE.fullmatch(value["cdhash"]) is not None,
        f"{label} observed signature differs",
    )


def _measurement_counts(
    value: Any,
    *,
    kind: str,
    files: int | None,
    directories: int | None,
) -> None:
    require(
        isinstance(value, dict)
        and set(value) == {"kind", "bytes", "files", "directories"}
        and value["kind"] == kind
        and isinstance(value["bytes"], int)
        and not isinstance(value["bytes"], bool)
        and 1 <= value["bytes"] <= MAX_TREE_BYTES
        and isinstance(value["files"], int)
        and not isinstance(value["files"], bool)
        and 1 <= value["files"] <= MAX_TREE_ENTRIES
        and isinstance(value["directories"], int)
        and not isinstance(value["directories"], bool)
        and 0 <= value["directories"] <= MAX_TREE_ENTRIES
        and (files is None or value["files"] == files)
        and (directories is None or value["directories"] == directories),
        "host artifact measurement differs",
    )


def validate_host_provenance_document(
    value: Any,
    *,
    profile_path: Path,
) -> dict[str, Any]:
    """Validate a child-produced receipt against independently read authority."""

    require(
        isinstance(value, dict)
        and set(value)
        == {
            "schema_version",
            "capture_kind",
            "status",
            "architecture",
            "host_coordinate_profile_sha256",
            "surfaces",
            "redaction",
        },
        "host provenance receipt fields differ",
    )
    profile_document, profile_digest = _profile(profile_path)
    require(
        value["schema_version"] == SCHEMA_VERSION
        and value["capture_kind"] == CAPTURE_KIND
        and value["status"] == "passed"
        and value["architecture"] == "arm64"
        and value["host_coordinate_profile_sha256"] == profile_digest,
        "host provenance receipt authority differs",
    )
    redaction = value["redaction"]
    require(
        isinstance(redaction, dict)
        and set(redaction)
        == {
            "absolute_paths_absent",
            "account_identity_absent",
            "artifact_content_absent",
            "environment_secrets_absent",
        }
        and all(item is True for item in redaction.values()),
        "host provenance redaction projection differs",
    )
    surfaces = value["surfaces"]
    require(
        isinstance(surfaces, list) and len(surfaces) == 3,
        "host provenance surface count differs",
    )
    common_fields = {
        "surface",
        "host_artifact_sha256",
        "host_metadata_sha256",
        "client_artifact_sha256",
        "ide_shell_artifact_sha256",
        "host_code_signature",
        "client_code_signature",
        "ide_shell_code_signature",
        "artifact_measurement",
        "client_bytes",
    }
    for index, item in enumerate(surfaces):
        require(isinstance(item, dict), "host provenance surface differs")
        surface = ("app", "cli", "ide")[index]
        expected_fields = common_fields | (
            {"metadata_bytes", "ide_shell_bytes"} if surface == "ide" else set()
        )
        require(
            set(item) == expected_fields and item["surface"] == surface,
            f"{surface} host provenance fields differ",
        )
        for field in ("host_artifact_sha256", "client_artifact_sha256"):
            require(
                isinstance(item[field], str)
                and DIGEST_RE.fullmatch(item[field]) is not None,
                f"{surface} host provenance digest differs",
            )
        for field in ("host_metadata_sha256", "ide_shell_artifact_sha256"):
            require(
                item[field] is None
                or (
                    isinstance(item[field], str)
                    and DIGEST_RE.fullmatch(item[field]) is not None
                ),
                f"{surface} optional provenance digest differs",
            )
        for field in (
            "client_bytes",
            *(
                ("metadata_bytes", "ide_shell_bytes")
                if surface == "ide"
                else ()
            ),
        ):
            require(
                isinstance(item[field], int)
                and not isinstance(item[field], bool)
                and 1 <= item[field] <= MAX_FILE_BYTES,
                f"{surface} host artifact size differs",
            )
        if surface == "ide":
            require(
                item["host_code_signature"] is None,
                "IDE extension unexpectedly claims a bundle signature",
            )
            _observed_signature(
                item["client_code_signature"],
                label="IDE client",
            )
            _observed_signature(
                item["ide_shell_code_signature"],
                label="IDE shell",
            )
            _measurement_counts(
                item["artifact_measurement"],
                kind="directory_tree",
                files=None,
                directories=None,
            )
        else:
            _observed_signature(
                item["host_code_signature"],
                label=f"{surface} host",
            )
            _observed_signature(
                item["client_code_signature"],
                label=f"{surface} client",
            )
            require(
                item["ide_shell_code_signature"] is None,
                f"{surface} unexpectedly claims an IDE shell signature",
            )
            _measurement_counts(
                item["artifact_measurement"],
                kind="regular_file",
                files=1,
                directories=0,
            )
    validate_measurements_against_profile(profile_document, surfaces)
    require(
        len(canonical_json(value)) <= MAX_OUTPUT_BYTES,
        "host provenance receipt exceeds its byte bound",
    )
    return value


@dataclass(frozen=True)
class HostPaths:
    app_bundle: Path
    app_executable: Path
    app_client: Path
    vscode_bundle: Path
    vscode_executable: Path
    extension_root: Path
    extension_package_json: Path
    ide_client: Path


def acquire_host_provenance(
    *,
    profile_path: Path,
    paths: HostPaths,
    signature_probe: Callable[..., dict[str, Any]] = measure_code_signature,
    tree_probe: Callable[[Path], TreeMeasurement] = stable_tree_digest,
) -> dict[str, Any]:
    require(
        sys.platform == "darwin" and platform.machine() == "arm64",
        "host provenance requires native macOS arm64",
    )
    profile_document, profile_digest = _profile(profile_path)
    app_host_digest, app_host_bytes = _safe_file_digest(
        paths.app_executable,
        label="App executable",
    )
    app_client_digest, app_client_bytes = _safe_file_digest(
        paths.app_client,
        label="App client",
    )
    vscode_digest, vscode_bytes = _safe_file_digest(
        paths.vscode_executable,
        label="VS Code executable",
    )
    extension_metadata_digest, extension_metadata_bytes = _safe_file_digest(
        paths.extension_package_json,
        label="extension package metadata",
    )
    ide_client_digest, ide_client_bytes = _safe_file_digest(
        paths.ide_client,
        label="IDE client",
    )
    tree = tree_probe(paths.extension_root)
    app_host_signature = signature_probe(paths.app_bundle, deep=True)
    app_client_signature = signature_probe(paths.app_client, deep=False)
    vscode_signature = signature_probe(paths.vscode_bundle, deep=True)
    ide_client_signature = signature_probe(paths.ide_client, deep=False)
    measurements: list[dict[str, Any]] = [
        {
            "surface": "app",
            "host_artifact_sha256": app_host_digest,
            "host_metadata_sha256": None,
            "client_artifact_sha256": app_client_digest,
            "ide_shell_artifact_sha256": None,
            "host_code_signature": app_host_signature,
            "client_code_signature": app_client_signature,
            "ide_shell_code_signature": None,
            "artifact_measurement": {
                "kind": "regular_file",
                "bytes": app_host_bytes,
                "files": 1,
                "directories": 0,
            },
            "client_bytes": app_client_bytes,
        },
        {
            "surface": "cli",
            "host_artifact_sha256": app_client_digest,
            "host_metadata_sha256": None,
            "client_artifact_sha256": app_client_digest,
            "ide_shell_artifact_sha256": None,
            "host_code_signature": app_client_signature,
            "client_code_signature": app_client_signature,
            "ide_shell_code_signature": None,
            "artifact_measurement": {
                "kind": "regular_file",
                "bytes": app_client_bytes,
                "files": 1,
                "directories": 0,
            },
            "client_bytes": app_client_bytes,
        },
        {
            "surface": "ide",
            "host_artifact_sha256": tree.sha256,
            "host_metadata_sha256": extension_metadata_digest,
            "client_artifact_sha256": ide_client_digest,
            "ide_shell_artifact_sha256": vscode_digest,
            "host_code_signature": None,
            "client_code_signature": ide_client_signature,
            "ide_shell_code_signature": vscode_signature,
            "artifact_measurement": {
                "kind": "directory_tree",
                "bytes": tree.total_bytes,
                "files": tree.file_count,
                "directories": tree.directory_count,
            },
            "client_bytes": ide_client_bytes,
            "metadata_bytes": extension_metadata_bytes,
            "ide_shell_bytes": vscode_bytes,
        },
    ]
    validate_measurements_against_profile(profile_document, measurements)
    document = {
        "schema_version": SCHEMA_VERSION,
        "capture_kind": CAPTURE_KIND,
        "status": "passed",
        "architecture": "arm64",
        "host_coordinate_profile_sha256": profile_digest,
        "surfaces": measurements,
        "redaction": {
            "absolute_paths_absent": True,
            "account_identity_absent": True,
            "artifact_content_absent": True,
            "environment_secrets_absent": True,
        },
    }
    require(
        len(canonical_json(document)) <= MAX_OUTPUT_BYTES,
        "host provenance receipt exceeds its byte bound",
    )
    return validate_host_provenance_document(
        document,
        profile_path=profile_path,
    )


def parse_arguments(arguments: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--app-bundle", type=Path, required=True)
    parser.add_argument("--app-executable", type=Path, required=True)
    parser.add_argument("--app-client", type=Path, required=True)
    parser.add_argument("--vscode-bundle", type=Path, required=True)
    parser.add_argument("--vscode-executable", type=Path, required=True)
    parser.add_argument("--extension-root", type=Path, required=True)
    parser.add_argument("--extension-package-json", type=Path, required=True)
    parser.add_argument("--ide-client", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args(arguments)


def main(arguments: Sequence[str] | None = None) -> int:
    options = parse_arguments(arguments or sys.argv[1:])
    receipt = acquire_host_provenance(
        profile_path=options.profile,
        paths=HostPaths(
            app_bundle=options.app_bundle,
            app_executable=options.app_executable,
            app_client=options.app_client,
            vscode_bundle=options.vscode_bundle,
            vscode_executable=options.vscode_executable,
            extension_root=options.extension_root,
            extension_package_json=options.extension_package_json,
            ide_client=options.ide_client,
        ),
    )
    try:
        acquisition_paths.atomic_write_new_file(
            options.output,
            canonical_json(receipt) + b"\n",
            prefix=".s11-host-provenance-",
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise HostProvenanceError(
            "host provenance output path is unsafe or already exists"
        ) from error
    print(
        json.dumps(
            {
                "schema_version": SCHEMA_VERSION,
                "status": "passed",
                "sha256": sha256_bytes(canonical_json(receipt) + b"\n"),
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except HostProvenanceError as error:
        print(f"Sprint 11 host provenance failed: {error}", file=sys.stderr)
        raise SystemExit(1)
