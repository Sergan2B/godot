#!/usr/bin/env python3
"""Acquire a source-bound Sprint 6-10 live receipt from the detached package.

This runner is intentionally separate from the short Sprint 11 contract
preflight.  It executes the real Godot prerequisite and the exact sidecar
listed as ``bin/godot-codex-mcp`` in the detached package manifest.  Sprint 9
is sharded so every child has the same fail-closed 180 second maximum.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import re
import selectors
import shutil
import signal
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import unicodedata
from collections.abc import Callable, Iterator, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Final, cast

try:
    from tests.codex import sprint11_acquisition_paths as acquisition_paths
    from tests.codex import sprint11_process_scope as process_scope
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_acquisition_paths as acquisition_paths
    import sprint11_process_scope as process_scope

SCRIPT_DIR: Final = Path(__file__).resolve().parent
REPOSITORY_ROOT: Final = SCRIPT_DIR.parent.parent
RECEIPT_SCHEMA: Final = "s11-packaged-regression-receipt/1.0"
CAPTURE_KIND: Final = "real_package_live"
PACKAGE_MANIFEST_SCHEMA: Final = "s11-package-manifest/1.0"
INTERNAL_PACKAGE_MANIFEST_SCHEMA: Final = "godot-codex-package/1.0"
PACKAGE_SIDECAR_PATH: Final = "bin/godot-codex-mcp"
PACKAGED_FIXTURE_RUNNER: Final = "tests/codex/sprint11_packaged_fixture.py"
ISOLATED_PYTHON_FLAGS: Final = ("-E", "-s", "-S")
MAX_REPORT_BYTES: Final = 2 * 1024 * 1024
MAX_RECEIPT_BYTES: Final = 128 * 1024
MAX_OUTPUT_BYTES: Final = 1024 * 1024
MAX_ARTIFACT_BYTES: Final = 512 * 1024 * 1024
MAX_COMMANDS: Final = 16
MAX_PACKAGE_FILES: Final = 256
MAX_INSTALLER_OUTPUT_BYTES: Final = 64 * 1024
PROCESS_READ_BYTES: Final = 64 * 1024
PROCESS_POLL_SECONDS: Final = 0.01
PROCESS_TERMINATE_SECONDS: Final = 1.0
GIT_EXECUTABLE: Final = Path("/usr/bin/git")
MAX_GIT_OUTPUT_BYTES: Final = 64 * 1024
MAX_SOURCE_TREE_INDEX_BYTES: Final = 8 * 1024 * 1024
MAX_SOURCE_FILES: Final = 50_000
MAX_SOURCE_FILE_BYTES: Final = 64 * 1024 * 1024
MAX_SOURCE_TREE_BYTES: Final = 1024 * 1024 * 1024
MAX_SOURCE_MATERIALIZE_SECONDS: Final = 180.0
MACHO_CPU_TYPE_ARM64: Final = 0x0100000C
COMMIT_RE: Final = re.compile(r"[0-9a-f]{40}\Z")
DIGEST_RE: Final = re.compile(r"sha256:[0-9a-f]{64}\Z")
VERSION_RE: Final = re.compile(
    r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?\Z"
)
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
class InstalledPackage:
    """One private installer-owned package lifetime used by live runners."""

    operations: Path
    sidecar: Path
    data_root: Path
    package_version: str


@dataclass(frozen=True)
class _BoundedProcessResult:
    returncode: int
    stdout: bytes
    stderr: bytes


def _posix_process_group_exists(process_group: int) -> bool:
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # The child group can disappear and its numeric id can become foreign
        # between poll and signal. Never treat an inaccessible PGID as owned.
        return False
    return True


def _terminate_process_group(process: subprocess.Popen[bytes]) -> None:
    """Best-effort bounded termination of the child and its descendants."""

    if os.name == "posix":
        process_group = process.pid
        if _posix_process_group_exists(process_group):
            try:
                os.killpg(process_group, signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                pass
            deadline = time.monotonic() + PROCESS_TERMINATE_SECONDS
            while (
                _posix_process_group_exists(process_group)
                and time.monotonic() < deadline
            ):
                process.poll()
                time.sleep(PROCESS_POLL_SECONDS)
            if _posix_process_group_exists(process_group):
                try:
                    os.killpg(process_group, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
    else:
        # CREATE_NEW_PROCESS_GROUP is requested on Windows when available.
        # Python has no dependency-free Job Object API, so retain a safe
        # parent-process fallback if group signalling is unavailable.
        if process.poll() is None:
            control_break = getattr(signal, "CTRL_BREAK_EVENT", None)
            if control_break is not None:
                try:
                    process.send_signal(control_break)
                    process.wait(timeout=PROCESS_TERMINATE_SECONDS)
                except (OSError, subprocess.TimeoutExpired):
                    pass
            if process.poll() is None:
                try:
                    process.terminate()
                    process.wait(timeout=PROCESS_TERMINATE_SECONDS)
                except (OSError, subprocess.TimeoutExpired):
                    pass
            if process.poll() is None:
                try:
                    process.kill()
                except OSError:
                    pass
    try:
        process.wait(timeout=PROCESS_TERMINATE_SECONDS)
    except (OSError, subprocess.TimeoutExpired):
        if process.poll() is None:
            try:
                process.kill()
            except OSError:
                pass
            try:
                process.wait(timeout=PROCESS_TERMINATE_SECONDS)
            except (OSError, subprocess.TimeoutExpired):
                pass


def _run_bounded_process(
    argv: Sequence[str | os.PathLike[str]],
    *,
    cwd: Path | None = None,
    environment: Mapping[str, str] | None = None,
    timeout: float,
    output_limit: int,
    label: str,
) -> _BoundedProcessResult:
    """Capture two pipes incrementally without allowing unbounded output."""

    require(timeout > 0, f"{label} timeout differs")
    require(output_limit >= 0, f"{label} output bound differs")
    popen_options: dict[str, Any] = {}
    if os.name == "posix":
        popen_options["start_new_session"] = True
    elif os.name == "nt":
        creation_flag = getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)
        if creation_flag:
            popen_options["creationflags"] = creation_flag
    try:
        scoped_environment, command_scope = process_scope.bind_environment(
            os.environ if environment is None else environment
        )
    except process_scope.ProcessScopeError as error:
        raise PackagedRegressionError(
            f"{label} process scope could not be created"
        ) from error
    try:
        scoped_command = process_scope.scoped_argv(
            argv,
            command_scope,
        )
    except process_scope.ProcessScopeError as error:
        raise PackagedRegressionError(
            f"{label} command scope differs"
        ) from error
    try:
        process = subprocess.Popen(
            scoped_command,
            cwd=cwd,
            env=scoped_environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            **popen_options,
        )
    except OSError as error:
        raise PackagedRegressionError(f"{label} could not start") from error
    try:
        process_scope.activate_scope(command_scope, process)
    except process_scope.ProcessScopeError as error:
        failure = PackagedRegressionError(
            f"{label} process scope could not be activated"
        )
        failure.__cause__ = error
        _terminate_process_group(process)
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                try:
                    stream.close()
                except (OSError, ValueError) as cleanup_error:
                    failure.add_note(
                        f"cleanup failure: {label} capture could not be "
                        f"closed: {cleanup_error}"
                    )
        try:
            process_scope.close_scope(command_scope)
        except process_scope.ProcessScopeError as cleanup_error:
            failure.add_note(
                f"cleanup failure: {label} process scope could not be closed: "
                f"{cleanup_error}"
            )
        raise failure
    require(
        process.stdout is not None and process.stderr is not None,
        f"{label} capture pipes differ",
    )
    buffers = (bytearray(), bytearray())
    completed = (threading.Event(), threading.Event())
    exceeded = threading.Event()
    reader_failures: list[BaseException] = []
    failure_lock = threading.Lock()

    def read_stream(
        stream: Any,
        buffer: bytearray,
        done: threading.Event,
    ) -> None:
        try:
            while True:
                chunk = stream.read(PROCESS_READ_BYTES)
                if not chunk:
                    return
                remaining = output_limit + 1 - len(buffer)
                if remaining > 0:
                    buffer.extend(chunk[:remaining])
                if len(chunk) > remaining or len(buffer) > output_limit:
                    exceeded.set()
                    return
        except BaseException as error:
            with failure_lock:
                reader_failures.append(error)
        finally:
            try:
                stream.close()
            except OSError:
                pass
            done.set()

    readers = (
        threading.Thread(
            target=read_stream,
            args=(process.stdout, buffers[0], completed[0]),
            daemon=True,
        ),
        threading.Thread(
            target=read_stream,
            args=(process.stderr, buffers[1], completed[1]),
            daemon=True,
        ),
    )
    for reader in readers:
        reader.start()
    deadline = time.monotonic() + timeout
    terminated = False
    try:
        while True:
            try:
                process_scope.require_scope_healthy(command_scope)
            except process_scope.ProcessScopeError as error:
                raise PackagedRegressionError(
                    f"{label} process scope tracker failed"
                ) from error
            if exceeded.is_set():
                raise PackagedRegressionError(f"{label} output exceeds byte bound")
            with failure_lock:
                has_reader_failure = bool(reader_failures)
            if has_reader_failure:
                raise PackagedRegressionError(f"{label} output capture failed")
            returncode = process.poll()
            if returncode is not None and returncode != 0 and not terminated:
                _terminate_process_group(process)
                terminated = True
            if returncode is not None and all(event.is_set() for event in completed):
                break
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise PackagedRegressionError(f"{label} timed out")
            time.sleep(min(PROCESS_POLL_SECONDS, remaining))
    except BaseException:
        if not terminated:
            _terminate_process_group(process)
            terminated = True
        raise
    finally:
        active_error = sys.exc_info()[1]
        if process.poll() is None and not terminated:
            _terminate_process_group(process)
        for reader in readers:
            reader.join(timeout=PROCESS_TERMINATE_SECONDS)
        escaped_scope = False
        try:
            escaped_scope = process_scope.close_scope(command_scope)
        except process_scope.ProcessScopeError as error:
            cleanup_error = PackagedRegressionError(
                f"{label} detached process scope could not be closed"
            )
            cleanup_error.__cause__ = error
            if active_error is None:
                raise cleanup_error
            active_error.add_note(f"cleanup failure: {cleanup_error}")
        if escaped_scope and active_error is None:
            raise PackagedRegressionError(
                f"{label} detached descendants escaped the process group"
            )
    if any(reader.is_alive() for reader in readers):
        if not terminated:
            _terminate_process_group(process)
        raise PackagedRegressionError(f"{label} output capture did not stop")
    with failure_lock:
        has_reader_failure = bool(reader_failures)
    if has_reader_failure:
        if not terminated:
            _terminate_process_group(process)
        raise PackagedRegressionError(f"{label} output capture failed")
    if exceeded.is_set():
        if not terminated:
            _terminate_process_group(process)
        raise PackagedRegressionError(f"{label} output exceeds byte bound")
    returncode = process.poll()
    require(returncode is not None, f"{label} process did not stop")
    if (
        returncode == 0
        and os.name == "posix"
        and _posix_process_group_exists(process.pid)
    ):
        _terminate_process_group(process)
        raise PackagedRegressionError(f"{label} descendants remained running")
    return _BoundedProcessResult(
        returncode=returncode,
        stdout=bytes(buffers[0]),
        stderr=bytes(buffers[1]),
    )


def _git_environment() -> dict[str, str]:
    """Return a host-independent Git environment with no ambient redirection."""

    environment = {
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_NO_LAZY_FETCH": "1",
        "GIT_NO_REPLACE_OBJECTS": "1",
        "GIT_OPTIONAL_LOCKS": "0",
        "GIT_TERMINAL_PROMPT": "0",
        "HOME": "/var/empty",
        "LANG": "C",
        "LC_ALL": "C",
        "NO_COLOR": "1",
        "PATH": "/usr/bin:/bin",
    }
    for name, value in os.environ.items():
        if not name.startswith(process_scope.SCOPE_ENVIRONMENT_PREFIX):
            continue
        require(
            process_scope.SCOPE_KEY_RE.fullmatch(name) is not None
            and value == "1",
            "process scope environment differs",
        )
        environment[name] = value
    return environment


def _git_command(repository: Path, *arguments: str) -> list[str | Path]:
    require(
        GIT_EXECUTABLE.is_file()
        and not GIT_EXECUTABLE.is_symlink()
        and os.access(GIT_EXECUTABLE, os.X_OK),
        "system Git executable differs",
    )
    return [
        GIT_EXECUTABLE,
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-C",
        repository,
        *arguments,
    ]


def _run_git(
    repository: Path,
    *arguments: str,
    output_limit: int = MAX_GIT_OUTPUT_BYTES,
) -> _BoundedProcessResult:
    return _run_bounded_process(
        _git_command(repository, *arguments),
        cwd=repository,
        environment=_git_environment(),
        timeout=60.0,
        output_limit=output_limit,
        label="source Git command",
    )


@dataclass(frozen=True)
class SourceSnapshot:
    root: Path
    source_commit: str
    repository: Path = REPOSITORY_ROOT


@dataclass(frozen=True)
class _SourceTreeEntry:
    path: str
    mode: int
    object_id: str
    size: int


def _safe_source_path(value: bytes) -> str:
    try:
        path = value.decode("utf-8")
    except UnicodeDecodeError as error:
        raise PackagedRegressionError(
            "package source path is not UTF-8"
        ) from error
    logical = PurePosixPath(path)
    require(
        path
        and len(value) <= 4096
        and not logical.is_absolute()
        and logical.as_posix() == path
        and "\\" not in path
        and unicodedata.normalize("NFC", path) == path
        and all(
            part not in {"", ".", ".."}
            and part.casefold() != ".git"
            for part in logical.parts
        ),
        "package source path is unsafe",
    )
    return path


def _source_tree_entries(snapshot: SourceSnapshot) -> tuple[_SourceTreeEntry, ...]:
    object_type = _run_git(
        snapshot.repository,
        "cat-file",
        "-t",
        snapshot.source_commit,
    )
    require(
        object_type.returncode == 0 and object_type.stdout == b"commit\n",
        "package source coordinate is not a raw commit",
    )
    tree = _run_git(
        snapshot.repository,
        "ls-tree",
        "-rlz",
        "--full-tree",
        snapshot.source_commit,
        output_limit=MAX_SOURCE_TREE_INDEX_BYTES,
    )
    require(
        tree.returncode == 0 and tree.stdout,
        "package source tree is unavailable",
    )
    records: list[_SourceTreeEntry] = []
    normalized_paths: set[str] = set()
    total_bytes = 0
    object_id_length: int | None = None
    for raw_record in tree.stdout.split(b"\0"):
        if not raw_record:
            continue
        try:
            metadata, raw_path = raw_record.split(b"\t", 1)
        except ValueError as error:
            raise PackagedRegressionError(
                "package source tree record differs"
            ) from error
        fields = metadata.split()
        require(
            len(fields) == 4,
            "package source tree metadata differs",
        )
        raw_mode, raw_type, raw_object_id, raw_size = fields
        require(
            raw_mode in {b"100644", b"100755"}
            and raw_type == b"blob"
            and raw_size.isdigit(),
            "package source contains a link, submodule, or unsupported entry",
        )
        try:
            object_id = raw_object_id.decode("ascii")
            size = int(raw_size)
            mode = int(raw_mode, 8)
        except (UnicodeDecodeError, ValueError) as error:
            raise PackagedRegressionError(
                "package source tree metadata is invalid"
            ) from error
        require(
            len(object_id) in {40, 64}
            and all(
                character in "0123456789abcdef"
                for character in object_id
            )
            and (object_id_length is None or len(object_id) == object_id_length)
            and 0 <= size <= MAX_SOURCE_FILE_BYTES,
            "package source blob identity or size differs",
        )
        object_id_length = len(object_id)
        path = _safe_source_path(raw_path)
        normalized = unicodedata.normalize("NFC", path).casefold()
        require(
            normalized not in normalized_paths,
            "package source paths collide on the target filesystem",
        )
        normalized_paths.add(normalized)
        total_bytes += size
        require(
            len(records) < MAX_SOURCE_FILES
            and total_bytes <= MAX_SOURCE_TREE_BYTES,
            "package source tree exceeds its bound",
        )
        records.append(
            _SourceTreeEntry(
                path=path,
                mode=mode,
                object_id=object_id,
                size=size,
            )
        )
    require(records, "package source tree is empty")
    return tuple(records)


def _source_directories(
    entries: Sequence[_SourceTreeEntry],
) -> set[str]:
    directories: set[str] = set()
    for entry in entries:
        parent = PurePosixPath(entry.path).parent
        while parent != PurePosixPath("."):
            directories.add(parent.as_posix())
            parent = parent.parent
    return directories


def _materialize_source_blobs(
    *,
    snapshot: SourceSnapshot,
    entries: Sequence[_SourceTreeEntry],
) -> None:
    """Materialize raw Git blobs without checkout filters or attributes."""

    require(
        os.name == "posix",
        "raw package source materialization requires POSIX pipes",
    )
    snapshot.root.mkdir(mode=0o700)
    for relative in sorted(
        _source_directories(entries),
        key=lambda item: (item.count("/"), item),
    ):
        snapshot.root.joinpath(
            *PurePosixPath(relative).parts
        ).mkdir(mode=0o700)
    try:
        process = subprocess.Popen(
            _git_command(
                snapshot.repository,
                "cat-file",
                "--batch",
            ),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=_git_environment(),
            start_new_session=True,
        )
    except OSError as error:
        raise PackagedRegressionError(
            "raw package source reader could not start"
        ) from error
    require(
        process.stdin is not None
        and process.stdout is not None
        and process.stderr is not None,
        "raw package source reader pipes differ",
    )
    stderr_buffer = bytearray()
    stderr_exceeded = threading.Event()
    stderr_failure: list[BaseException] = []
    stderr_lock = threading.Lock()

    def read_stderr() -> None:
        try:
            while chunk := process.stderr.read(PROCESS_READ_BYTES):
                remaining = MAX_GIT_OUTPUT_BYTES + 1 - len(stderr_buffer)
                if remaining > 0:
                    stderr_buffer.extend(chunk[:remaining])
                if (
                    len(chunk) > remaining
                    or len(stderr_buffer) > MAX_GIT_OUTPUT_BYTES
                ):
                    stderr_exceeded.set()
                    return
        except BaseException as error:
            with stderr_lock:
                stderr_failure.append(error)

    stderr_reader = threading.Thread(target=read_stderr, daemon=True)
    stderr_reader.start()
    selector = selectors.DefaultSelector()
    output_descriptor = process.stdout.fileno()
    os.set_blocking(output_descriptor, False)
    selector.register(output_descriptor, selectors.EVENT_READ)
    output_buffer = bytearray()
    deadline = time.monotonic() + MAX_SOURCE_MATERIALIZE_SECONDS

    def read_more() -> bool:
        while True:
            require(
                not stderr_exceeded.is_set(),
                "raw package source reader stderr exceeds its bound",
            )
            with stderr_lock:
                require(
                    not stderr_failure,
                    "raw package source reader stderr capture failed",
                )
            remaining = deadline - time.monotonic()
            require(
                remaining > 0,
                "raw package source reader timed out",
            )
            events = selector.select(min(remaining, 0.1))
            if not events:
                require(
                    process.poll() is None,
                    "raw package source reader stopped early",
                )
                continue
            chunk = os.read(output_descriptor, PROCESS_READ_BYTES)
            if not chunk:
                return False
            output_buffer.extend(chunk)
            return True

    def read_line() -> bytes:
        while True:
            offset = output_buffer.find(b"\n")
            if offset >= 0:
                value = bytes(output_buffer[: offset + 1])
                del output_buffer[: offset + 1]
                require(
                    len(value) <= 256,
                    "raw package source header exceeds its bound",
                )
                return value
            require(
                len(output_buffer) <= 256 and read_more(),
                "raw package source header is truncated",
            )

    def write_blob(output: Any, size: int) -> None:
        remaining = size
        while remaining:
            if not output_buffer:
                require(
                    read_more(),
                    "raw package source blob is truncated",
                )
            count = min(remaining, len(output_buffer))
            output.write(output_buffer[:count])
            del output_buffer[:count]
            remaining -= count

    completed = False
    try:
        materialized_bytes = 0
        for entry in entries:
            process.stdin.write((entry.object_id + "\n").encode("ascii"))
            process.stdin.flush()
            require(
                read_line()
                == (
                    f"{entry.object_id} blob {entry.size}\n"
                ).encode("ascii"),
                "raw package source blob header differs",
            )
            destination = snapshot.root.joinpath(
                *PurePosixPath(entry.path).parts
            )
            try:
                with destination.open("xb") as output:
                    write_blob(output, entry.size)
                require(
                    read_line() == b"\n",
                    "raw package source blob framing differs",
                )
                destination.chmod(
                    0o755 if entry.mode == 0o100755 else 0o644
                )
            except OSError as error:
                raise PackagedRegressionError(
                    "raw package source blob could not be materialized"
                ) from error
            materialized_bytes += entry.size
            require(
                materialized_bytes <= MAX_SOURCE_TREE_BYTES,
                "raw package source materialization exceeds its bound",
            )
        process.stdin.close()
        while read_more():
            require(
                not output_buffer,
                "raw package source reader returned trailing output",
            )
        require(
            not output_buffer,
            "raw package source reader returned trailing output",
        )
        remaining = max(deadline - time.monotonic(), 0.001)
        returncode = process.wait(timeout=remaining)
        stderr_reader.join(timeout=PROCESS_TERMINATE_SECONDS)
        require(
            returncode == 0
            and not stderr_reader.is_alive()
            and not stderr_buffer
            and not _posix_process_group_exists(process.pid),
            "raw package source reader failed or left descendants",
        )
        completed = True
    except (BrokenPipeError, OSError, subprocess.TimeoutExpired) as error:
        raise PackagedRegressionError(
            "raw package source reader failed"
        ) from error
    finally:
        selector.close()
        if not completed:
            _terminate_process_group(process)
        for stream in (process.stdin, process.stdout, process.stderr):
            try:
                stream.close()
            except OSError:
                pass
        stderr_reader.join(timeout=PROCESS_TERMINATE_SECONDS)


def _git_blob_object_id(
    path: Path,
    *,
    entry: _SourceTreeEntry,
) -> str:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise PackagedRegressionError(
            "package source file is unavailable"
        ) from error
    require(
        stat.S_ISREG(metadata.st_mode)
        and not path.is_symlink()
        and metadata.st_size == entry.size
        and bool(metadata.st_mode & 0o111) == (entry.mode == 0o100755),
        "package source file identity, mode, or size differs",
    )
    algorithm = "sha1" if len(entry.object_id) == 40 else "sha256"
    digest = hashlib.new(algorithm)
    digest.update(f"blob {entry.size}\0".encode("ascii"))
    try:
        count = acquisition_paths.update_digest_from_regular_file(
            digest,
            path,
            maximum=MAX_SOURCE_FILE_BYTES,
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise PackagedRegressionError(
            "package source file changed during verification"
        ) from error
    require(
        count == entry.size,
        "package source file byte count differs",
    )
    return digest.hexdigest()


def _set_source_snapshot_writable(root: Path, writable: bool) -> None:
    file_mode = 0o600 if writable else 0o400
    executable_mode = 0o700 if writable else 0o500
    directory_mode = 0o700 if writable else 0o500
    for current_root, directories, files in os.walk(root, topdown=writable):
        current = Path(current_root)
        if writable:
            os.chmod(current, directory_mode)
        for name in files:
            path = current / name
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                continue
            os.chmod(
                path,
                executable_mode
                if metadata.st_mode & 0o111
                else file_mode,
            )
        if not writable:
            for name in directories:
                path = current / name
                if not path.is_symlink():
                    os.chmod(path, directory_mode)
            os.chmod(current, directory_mode)


def _verify_source_snapshot(snapshot: SourceSnapshot) -> None:
    entries = _source_tree_entries(snapshot)
    expected_files = {entry.path for entry in entries}
    expected_directories = _source_directories(entries)
    actual_files: set[str] = set()
    actual_directories: set[str] = set()
    for current_root, directories, files in os.walk(snapshot.root):
        current = Path(current_root)
        try:
            current_metadata = current.lstat()
        except OSError as error:
            raise PackagedRegressionError(
                "private source snapshot is unavailable"
            ) from error
        require(
            stat.S_ISDIR(current_metadata.st_mode)
            and not current.is_symlink(),
            "private source snapshot contains an unsafe directory",
        )
        for name in directories:
            child = current / name
            relative = child.relative_to(snapshot.root).as_posix()
            try:
                metadata = child.lstat()
            except OSError as error:
                raise PackagedRegressionError(
                    "private source snapshot directory is unavailable"
                ) from error
            require(
                stat.S_ISDIR(metadata.st_mode)
                and not child.is_symlink()
                and relative in expected_directories,
                "private source snapshot directory differs",
            )
            actual_directories.add(relative)
        for name in files:
            child = current / name
            relative = child.relative_to(snapshot.root).as_posix()
            try:
                metadata = child.lstat()
            except OSError as error:
                raise PackagedRegressionError(
                    "private source snapshot file is unavailable"
                ) from error
            require(
                stat.S_ISREG(metadata.st_mode)
                and not child.is_symlink()
                and relative in expected_files,
                "private source snapshot file differs",
            )
            actual_files.add(relative)
    require(
        actual_files == expected_files
        and actual_directories == expected_directories,
        "private source snapshot changed during acquisition",
    )
    for entry in entries:
        path = snapshot.root.joinpath(
            *PurePosixPath(entry.path).parts
        )
        require(
            _git_blob_object_id(path, entry=entry) == entry.object_id,
            "private source snapshot differs from raw commit blobs",
        )


@contextlib.contextmanager
def source_snapshot(
    source_commit: str,
    *,
    repository: Path = REPOSITORY_ROOT,
) -> Iterator[SourceSnapshot]:
    """Execute live runners from raw, verified package-source commit blobs."""

    require(
        COMMIT_RE.fullmatch(source_commit) is not None,
        "snapshot source commit differs",
    )
    try:
        temporary_parent = Path("/tmp").resolve(strict=True)
    except OSError as error:
        raise PackagedRegressionError(
            "private source snapshot parent is unavailable"
        ) from error
    require(
        temporary_parent.is_absolute() and temporary_parent.is_dir(),
        "private source snapshot parent differs",
    )
    with tempfile.TemporaryDirectory(
        prefix=".s11-package-source.",
        dir=temporary_parent,
    ) as private:
        root = Path(private) / "source"
        snapshot = SourceSnapshot(
            root=root,
            source_commit=source_commit,
            repository=repository.resolve(strict=True),
        )
        try:
            entries = _source_tree_entries(snapshot)
            _materialize_source_blobs(
                snapshot=snapshot,
                entries=entries,
            )
            _verify_source_snapshot(snapshot)
            _set_source_snapshot_writable(root, False)
            yield snapshot
            _verify_source_snapshot(snapshot)
        finally:
            if root.exists():
                try:
                    _set_source_snapshot_writable(root, True)
                except OSError:
                    pass


def _installer_environment(
    *,
    private_root: Path,
    data_root: Path,
    binary_root: Path,
) -> dict[str, str]:
    home = private_root / "home"
    temporary = private_root / "tmp"
    home.mkdir(mode=0o700)
    temporary.mkdir(mode=0o700)
    return {
        "GODOT_CODEX_BIN_DIR": str(binary_root),
        "GODOT_CODEX_DATA_ROOT": str(data_root),
        "HOME": str(home),
        "LANG": "C",
        "LC_ALL": "C",
        "NO_COLOR": "1",
        "PATH": (
            "/usr/bin:/bin:/usr/sbin:/sbin:/Library/Apple/usr/bin"
        ),
        "TMPDIR": str(temporary),
        "TZ": "UTC",
    }


def _run_installer(
    installer: Path,
    action: str,
    *,
    environment: Mapping[str, str],
    timeout: float,
) -> None:
    require(
        action in {"install", "verify", "uninstall"},
        "installer action differs",
    )
    result = _run_bounded_process(
        [installer, action],
        environment=environment,
        timeout=min(timeout, 60.0),
        output_limit=MAX_INSTALLER_OUTPUT_BYTES,
        label="private package installer",
    )
    require(
        result.returncode == 0
        and len(result.stdout) <= MAX_INSTALLER_OUTPUT_BYTES
        and len(result.stderr) <= MAX_INSTALLER_OUTPUT_BYTES,
        "private package installer result differs",
    )


def _require_identity_bytes(
    root: Path,
    relative: str,
    expected: bytes,
    *,
    records: Mapping[str, Mapping[str, Any]],
    label: str,
) -> None:
    path = root.joinpath(*PurePosixPath(relative).parts)
    try:
        payload = acquisition_paths.read_regular_file(
            path,
            maximum=max(len(expected), 256 * 1024),
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise PackagedRegressionError(f"{label} is unreadable") from error
    record = records.get(relative)
    require(
        record is not None
        and payload == expected
        and len(payload) == record.get("bytes")
        and sha256_bytes(payload) == record.get("sha256"),
        f"{label} binding differs",
    )


def _validate_package_identity(
    root: Path,
    *,
    package_version: str,
    source_commit: str,
    detached: Mapping[str, Any],
    records: Mapping[str, Mapping[str, Any]],
) -> None:
    """Cross-bind the detached, archive, and installed package coordinates."""

    _require_identity_bytes(
        root,
        "VERSION",
        f"{package_version}\n".encode("utf-8"),
        records=records,
        label="package VERSION",
    )
    _require_identity_bytes(
        root,
        "SOURCE_COMMIT",
        f"{source_commit}\n".encode("ascii"),
        records=records,
        label="package SOURCE_COMMIT",
    )
    internal_path = root / "package-manifest.json"
    try:
        internal_payload = acquisition_paths.read_regular_file(
            internal_path,
            maximum=256 * 1024,
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise PackagedRegressionError(
            "internal package manifest is unreadable"
        ) from error
    internal = strict_json_bytes(
        internal_payload,
        label="internal package manifest",
        maximum=256 * 1024,
    )
    internal = exact_fields(
        internal,
        {
            "schema_version",
            "package_version",
            "source_commit",
            "target",
            "build_provenance",
            "godot_prerequisite",
            "compatibility_matrix_sha256",
            "registry_sha256",
            "third_party_licenses_sha256",
            "contents",
            "checksums_path",
        },
        label="internal package manifest",
    )
    target = exact_fields(
        internal.get("target"),
        {"architecture", "os"},
        label="internal package target",
    )
    require(
        internal["schema_version"] == INTERNAL_PACKAGE_MANIFEST_SCHEMA
        and internal["package_version"] == package_version
        and internal["source_commit"] == source_commit
        and target == {"architecture": "arm64", "os": "macos"}
        and internal["checksums_path"] == "checksums.sha256"
        and internal["build_provenance"] == detached.get("build_provenance")
        and internal["godot_prerequisite"] == detached.get("godot_prerequisite")
        and internal["compatibility_matrix_sha256"]
        == detached.get("compatibility_matrix_sha256")
        and internal["registry_sha256"] == detached.get("registry_sha256")
        and internal["third_party_licenses_sha256"]
        == detached.get("third_party_licenses_sha256"),
        "internal/detached package manifest binding differs",
    )
    internal_contents = internal.get("contents")
    require(
        isinstance(internal_contents, list)
        and 1 <= len(internal_contents) <= MAX_PACKAGE_FILES,
        "internal package content bound differs",
    )
    internal_records: dict[str, Mapping[str, Any]] = {}
    for value in internal_contents:
        record = exact_fields(
            value,
            {"path", "sha256", "bytes", "mode"},
            label="internal package content",
        )
        relative = safe_relative_path(
            record.get("path"),
            label="internal package content",
        )
        require(
            relative not in internal_records
            and records.get(relative) == record,
            "internal/detached package content binding differs",
        )
        internal_records[relative] = record
    require(
        set(internal_records)
        == set(records) - {"package-manifest.json", "checksums.sha256"},
        "internal/detached package content set differs",
    )
    _require_identity_bytes(
        root,
        "package-manifest.json",
        internal_payload,
        records=records,
        label="internal package manifest",
    )


def _extract_verified_package(
    *,
    artifact_root: Path,
    package_manifest: Path,
    destination: Path,
) -> tuple[str, Mapping[str, Mapping[str, Any]]]:
    document = exact_fields(
        strict_json_load(
            package_manifest,
            label="detached package manifest",
            maximum=256 * 1024,
        ),
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
        document.get("schema_version") == PACKAGE_MANIFEST_SCHEMA,
        "detached package manifest schema differs",
    )
    package_version = document.get("package_version")
    require(
        isinstance(package_version, str)
        and 0 < len(package_version.encode("utf-8")) <= 128
        and VERSION_RE.fullmatch(package_version) is not None,
        "detached package version differs",
    )
    source_commit = document.get("source_commit")
    require(
        isinstance(source_commit, str)
        and COMMIT_RE.fullmatch(source_commit) is not None,
        "detached package source commit differs",
    )
    archive = exact_fields(
        document.get("archive"),
        {"path", "sha256", "bytes"},
        label="detached package archive",
    )
    archive_relative = safe_relative_path(
        archive.get("path"),
        label="detached package archive",
    )
    archive_path = artifact_root.joinpath(
        *PurePosixPath(archive_relative).parts
    )
    require_regular(archive_path)
    archive_bytes = archive.get("bytes")
    require(
        isinstance(archive_bytes, int)
        and not isinstance(archive_bytes, bool)
        and 0 < archive_bytes <= MAX_ARTIFACT_BYTES
        and archive_path.stat().st_size == archive_bytes
        and sha256_file(archive_path)
        == digest(archive.get("sha256"), label="detached package archive"),
        "detached package archive binding differs",
    )
    contents = document.get("contents")
    require(
        isinstance(contents, list)
        and 1 <= len(contents) <= MAX_PACKAGE_FILES,
        "detached package content bound differs",
    )
    records: dict[str, Mapping[str, Any]] = {}
    total_bytes = 0
    for value in contents:
        record = exact_fields(
            value,
            {"path", "sha256", "bytes", "mode"},
            label="detached package content",
        )
        relative = safe_relative_path(
            record.get("path"),
            label="detached package content",
        )
        size = record.get("bytes")
        require(
            relative not in records
            and isinstance(size, int)
            and not isinstance(size, bool)
            and 0 <= size <= 256 * 1024 * 1024
            and record.get("mode") in {"0644", "0755"},
            "detached package content metadata differs",
        )
        digest(record.get("sha256"), label="detached package content")
        total_bytes += size
        require(
            total_bytes <= MAX_ARTIFACT_BYTES,
            "detached package expands beyond its bound",
        )
        records[relative] = record
    require(
        {
            "VERSION",
            "SOURCE_COMMIT",
            "bin/godot-codex",
            PACKAGE_SIDECAR_PATH,
            "checksums.sha256",
            "install.sh",
            "package-manifest.json",
        }.issubset(records),
        "detached package omits an install requirement",
    )
    archive_name = PurePosixPath(archive_relative).name
    expected_archive_root = (
        f"godot-codex-{package_version}-macos-arm64"
    )
    require(
        archive_name == f"{expected_archive_root}.tar.gz",
        "detached package archive/version binding differs",
    )
    archive_root = archive_name[: -len(".tar.gz")]
    expected_directories = {
        PurePosixPath(*PurePosixPath(relative).parts[:index]).as_posix()
        for relative in records
        for index in range(1, len(PurePosixPath(relative).parts))
    }
    seen_members: set[str] = set()
    seen_files: set[str] = set()
    destination.mkdir(mode=0o700)
    try:
        with tarfile.open(archive_path, mode="r:gz") as package_archive:
            for member_index, member in enumerate(package_archive, start=1):
                require(
                    member_index <= 1024 and member.name not in seen_members,
                    "detached package archive member bound differs",
                )
                seen_members.add(member.name)
                member_path = PurePosixPath(member.name)
                require(
                    not member_path.is_absolute()
                    and ".." not in member_path.parts
                    and member_path.parts
                    and member_path.parts[0] == archive_root
                    and member.uid == 0
                    and member.gid == 0
                    and member.mtime == 0
                    and member.uname == "root"
                    and member.gname == "root",
                    "detached package archive metadata differs",
                )
                relative_parts = member_path.parts[1:]
                if member.isdir():
                    relative = (
                        PurePosixPath(*relative_parts).as_posix()
                        if relative_parts
                        else ""
                    )
                    require(
                        member.mode == 0o755
                        and (not relative or relative in expected_directories),
                        "detached package archive directory differs",
                    )
                    target = destination.joinpath(*relative_parts)
                    target.mkdir(parents=True, exist_ok=True, mode=0o755)
                    os.chmod(target, 0o755)
                    continue
                require(
                    member.isfile() and bool(relative_parts),
                    "detached package archive contains a non-regular member",
                )
                relative = PurePosixPath(*relative_parts).as_posix()
                record = records.get(relative)
                require(
                    record is not None
                    and relative not in seen_files
                    and member.mode == int(cast(str, record["mode"]), 8)
                    and member.size == record["bytes"],
                    "detached package archive content differs",
                )
                stream = package_archive.extractfile(member)
                require(stream is not None, "detached package member is unreadable")
                payload = stream.read(cast(int, record["bytes"]) + 1)
                require(
                    len(payload) == record["bytes"]
                    and sha256_bytes(payload) == record["sha256"],
                    "detached package member digest differs",
                )
                target = destination.joinpath(*relative_parts)
                target.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
                require(not target.exists(), "detached package member is duplicated")
                try:
                    with target.open("xb") as output:
                        output.write(payload)
                    os.chmod(target, int(cast(str, record["mode"]), 8))
                except OSError as error:
                    raise PackagedRegressionError(
                        "detached package member cannot be materialized"
                    ) from error
                seen_files.add(relative)
    except (OSError, tarfile.TarError) as error:
        raise PackagedRegressionError("detached package archive is invalid") from error
    require(
        seen_files == set(records),
        "detached package archive/content bijection differs",
    )
    _validate_package_identity(
        destination,
        package_version=package_version,
        source_commit=source_commit,
        detached=document,
        records=records,
    )
    return package_version, records


@contextlib.contextmanager
def installed_package(
    artifact_root: Path,
    package_manifest: Path,
    timeout: float,
) -> Iterator[InstalledPackage]:
    """Install the detached archive in a private managed root and remove it."""

    try:
        temporary_parent = Path("/tmp").resolve(strict=True)
    except OSError as error:
        raise PackagedRegressionError(
            "private package temporary parent is unavailable"
        ) from error
    require(
        temporary_parent.is_absolute() and temporary_parent.is_dir(),
        "private package temporary parent differs",
    )
    with tempfile.TemporaryDirectory(
        prefix="s11pkg.",
        dir=temporary_parent,
    ) as temporary:
        private_root = Path(temporary)
        package_source = private_root / "package"
        package_version, records = _extract_verified_package(
            artifact_root=artifact_root,
            package_manifest=package_manifest,
            destination=package_source,
        )
        data_root = private_root / "data"
        binary_root = private_root / "bin"
        environment = _installer_environment(
            private_root=private_root,
            data_root=data_root,
            binary_root=binary_root,
        )
        installer = package_source / "install.sh"
        require_regular(installer, executable=True)
        _run_installer(
            installer,
            "install",
            environment=environment,
            timeout=timeout,
        )
        _run_installer(
            installer,
            "verify",
            environment=environment,
            timeout=timeout,
        )
        detached = strict_json_load(
            package_manifest,
            label="detached package manifest",
            maximum=256 * 1024,
        )
        source_commit = detached.get("source_commit")
        require(
            detached.get("package_version") == package_version
            and isinstance(source_commit, str)
            and COMMIT_RE.fullmatch(source_commit) is not None,
            "installed/detached package coordinate differs",
        )
        versions_root = data_root / "versions"
        version_target = versions_root / package_version
        current = data_root / "current"
        try:
            versions_metadata = versions_root.lstat()
            target_metadata = version_target.lstat()
            current_metadata = current.lstat()
            current_target = os.readlink(current)
            resolved_current = current.resolve(strict=True)
            resolved_target = version_target.resolve(strict=True)
            version_entries = list(versions_root.iterdir())
        except OSError as error:
            raise PackagedRegressionError(
                "installed package version topology is unavailable"
            ) from error
        require(
            stat.S_ISDIR(versions_metadata.st_mode)
            and not versions_root.is_symlink()
            and stat.S_ISDIR(target_metadata.st_mode)
            and not version_target.is_symlink()
            and version_entries == [version_target]
            and stat.S_ISLNK(current_metadata.st_mode)
            and current_target == str(version_target)
            and resolved_current == resolved_target,
            "installed package current/version binding differs",
        )
        _validate_package_identity(
            version_target,
            package_version=package_version,
            source_commit=source_commit,
            detached=detached,
            records=records,
        )
        try:
            ownership = acquisition_paths.read_regular_file(
                version_target / ".godot-codex-owned",
                maximum=128,
            )
        except acquisition_paths.AcquisitionPathError as error:
            raise PackagedRegressionError(
                "installed package ownership marker is unreadable"
            ) from error
        require(
            ownership
            == (
                sha256_file(version_target / "package-manifest.json")
                .removeprefix("sha256:")
                + "\n"
            ).encode("ascii"),
            "installed package ownership binding differs",
        )
        operations = current / "bin/godot-codex"
        sidecar = current / "bin/godot-codex-mcp"
        require(
            operations.resolve(strict=True)
            == (version_target / "bin/godot-codex").resolve(strict=True)
            and sidecar.resolve(strict=True)
            == (version_target / PACKAGE_SIDECAR_PATH).resolve(strict=True)
            and sha256_file(operations)
            == records["bin/godot-codex"]["sha256"]
            and sha256_file(sidecar)
            == records[PACKAGE_SIDECAR_PATH]["sha256"],
            "installed package executable binding differs",
        )
        try:
            yield InstalledPackage(
                operations=operations,
                sidecar=sidecar,
                data_root=data_root,
                package_version=package_version,
            )
        finally:
            _run_installer(
                installer,
                "uninstall",
                environment=environment,
                timeout=timeout,
            )
            require(
                not data_root.exists()
                and not (data_root / "current").is_symlink()
                and not binary_root.exists(),
                "private installed package cleanup differs",
            )


@dataclass(frozen=True)
class CommandSpec:
    command_id: str
    runner_path: str
    report_name: str
    arguments: tuple[str, ...]

    def argv_template(self) -> tuple[str, ...]:
        return (
            "{python}",
            *ISOLATED_PYTHON_FLAGS,
            self.runner_path,
            "--godot",
            "{godot}",
            "--sidecar",
            "{package_sidecar}",
            "--package-operations",
            "{package_operations}",
            "--package-data-root",
            "{package_data_root}",
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
                "--skip-approval-timeout",
            ),
        ),
        CommandSpec(
            "s9_approval_timeout",
            "tests/codex/sprint9_model_free_live.py",
            "sprint9-approval-timeout.json",
            (
                *common_s9,
                "--operations",
                "--skip-faults",
                "--only-approval-timeout",
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
    operations: Path,
    data_root: Path,
    report_root: Path,
    timeout: float,
) -> tuple[str, ...]:
    substitutions = {
        "{python}": sys.executable,
        "{godot}": str(godot),
        "{package_sidecar}": str(sidecar),
        "{package_operations}": str(operations),
        "{package_data_root}": str(data_root),
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
PackageSessionFactory = Callable[
    [Path, Path, float],
    contextlib.AbstractContextManager[InstalledPackage],
]
SourceSessionFactory = Callable[
    [str],
    contextlib.AbstractContextManager[SourceSnapshot],
]


def _package_live_environment(private_root: Path) -> dict[str, str]:
    home = private_root / "home"
    temporary = private_root / "tmp"
    home.mkdir(mode=0o700)
    temporary.mkdir(mode=0o700)
    environment = {
        "HOME": str(home),
        "LANG": "C",
        "LC_ALL": "C",
        "NO_COLOR": "1",
        "PATH": (
            "/usr/bin:/bin:/usr/sbin:/sbin:/Library/Apple/usr/bin"
        ),
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONHASHSEED": "0",
        "TMPDIR": str(temporary),
        "TZ": "UTC",
    }
    return environment


def execute_command(
    _spec: CommandSpec,
    argv: tuple[str, ...],
    cwd: Path,
    timeout: float,
) -> Execution:
    started = time.monotonic()
    try:
        temporary_parent = Path("/tmp").resolve(strict=True)
    except OSError as error:
        raise PackagedRegressionError(
            "package-live temporary parent is unavailable"
        ) from error
    require(
        temporary_parent.is_absolute() and temporary_parent.is_dir(),
        "package-live temporary parent differs",
    )
    with tempfile.TemporaryDirectory(
        prefix="s11live.",
        dir=temporary_parent,
    ) as temporary:
        result = _run_bounded_process(
            argv,
            cwd=cwd,
            timeout=timeout,
            output_limit=MAX_OUTPUT_BYTES,
            environment=_package_live_environment(Path(temporary)),
            label="package-live command",
        )
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
                == {"confirm_false", "decline", "cancel", "unsupported"},
                "Sprint 9 negative matrix differs",
            )
        elif command_id == "s9_approval_timeout":
            approval = negatives.get("approval")
            decisions = {
                item.get("decision")
                for item in approval
                if isinstance(item, dict)
            } if isinstance(approval, list) else set()
            require(
                not operations
                and not faults
                and set(negatives) == {"approval"}
                and decisions == {"timeout"},
                "Sprint 9 approval timeout shard differs",
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
    approval = [
        *cast(list[dict[str, Any]], negatives["approval"]),
        *cast(
            list[dict[str, Any]],
            cast(
                dict[str, Any],
                reports["s9_approval_timeout"]["negatives"],
            )["approval"],
        ),
    ]
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
    package_version = manifest.get("package_version")
    require(
        isinstance(package_version, str)
        and len(package_version.encode("utf-8")) <= 128
        and VERSION_RE.fullmatch(package_version) is not None,
        "detached package version differs",
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
    archive_bytes = archive.get("bytes")
    require(
        archive_relative
        == f"godot-codex-{package_version}-macos-arm64.tar.gz"
        and isinstance(archive_bytes, int)
        and not isinstance(archive_bytes, bool)
        and 0 < archive_bytes <= MAX_ARTIFACT_BYTES
        and archive_path.stat().st_size == archive_bytes
        and sha256_file(archive_path)
        == digest(archive["sha256"], label="package archive"),
        "package archive/version binding differs",
    )
    contents = manifest.get("contents")
    require(
        isinstance(contents, list)
        and 1 <= len(contents) <= MAX_PACKAGE_FILES,
        "package contents are missing or exceed their bound",
    )
    content_records: dict[str, Mapping[str, Any]] = {}
    for value in contents:
        record = exact_fields(
            value,
            {"path", "sha256", "bytes", "mode"},
            label="package content",
        )
        relative = safe_relative_path(
            record.get("path"),
            label="package content",
        )
        size = record.get("bytes")
        require(
            relative not in content_records
            and isinstance(size, int)
            and not isinstance(size, bool)
            and 0 <= size <= 256 * 1024 * 1024
            and record.get("mode") in {"0644", "0755"},
            "package content metadata differs",
        )
        digest(record.get("sha256"), label="package content")
        content_records[relative] = record
    require(
        PACKAGE_SIDECAR_PATH in content_records,
        "package sidecar content record is missing or duplicated",
    )
    _validate_package_identity(
        artifact_root,
        package_version=package_version,
        source_commit=source_commit,
        detached=manifest,
        records=content_records,
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
    result = _run_bounded_process(
        [godot, "--version"],
        timeout=30,
        output_limit=4096,
        label="Godot version probe",
    )
    require(
        result.returncode == 0
        and 0 < len(result.stdout) <= 256,
        "Godot version probe differs",
    )
    try:
        return result.stdout.decode("utf-8").strip()
    except UnicodeDecodeError as error:
        raise PackagedRegressionError(
            "Godot version probe is not UTF-8"
        ) from error


def repository_head() -> str:
    result = _run_git(REPOSITORY_ROOT, "rev-parse", "--verify", "HEAD")
    require(
        result.returncode == 0,
        "repository source coordinate is unavailable",
    )
    try:
        value = result.stdout.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise PackagedRegressionError(
            "repository source coordinate is invalid"
        ) from error
    require(COMMIT_RE.fullmatch(value) is not None, "repository HEAD differs")
    return value


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
    package_session_factory: PackageSessionFactory | None = None,
    source_session_factory: SourceSessionFactory | None = None,
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
        session_factory = package_session_factory or installed_package
        snapshot_factory = source_session_factory or source_snapshot
        with snapshot_factory(
            cast(str, bindings["package_source_commit"])
        ) as snapshot:
            require(
                snapshot.source_commit == bindings["package_source_commit"],
                "private source snapshot binding differs",
            )
            with session_factory(
                artifact_root,
                package_manifest,
                timeout,
            ) as installed:
                require(
                    installed.package_version
                    == strict_json_load(
                        package_manifest,
                        label="detached package manifest",
                        maximum=256 * 1024,
                    ).get("package_version")
                    and sha256_file(installed.sidecar) == original_sidecar_sha256,
                    "installed package identity differs",
                )
                for spec in specs:
                    runner = snapshot.root.joinpath(
                        *PurePosixPath(spec.runner_path).parts
                    )
                    require_regular(runner)
                    argv = expand_argv(
                        spec,
                        godot=godot,
                        sidecar=installed.sidecar,
                        operations=installed.operations,
                        data_root=installed.data_root,
                        report_root=staging,
                        timeout=timeout,
                    )
                    execution = executor(spec, argv, snapshot.root, timeout)
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
                        godot_sha256=cast(
                            str,
                            bindings["godot_artifact_sha256"],
                        ),
                        sidecar_sha256=cast(
                            str,
                            bindings["package_sidecar_sha256"],
                        ),
                    )
                    final_report_path = output_root / spec.report_name
                    report_relative = final_report_path.relative_to(
                        repository
                    ).as_posix()
                    template = list(spec.argv_template())
                    command_records.append(
                        {
                            "id": spec.command_id,
                            "cwd": ".",
                            "argv_template": template,
                            "command_sha256": sha256_bytes(
                                canonical_json(
                                    {"cwd": ".", "argv": template}
                                )
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
            fixture_records = [
                {
                    "id": PurePosixPath(relative).parent.name
                    + ":"
                    + PurePosixPath(relative).name,
                    "path": relative,
                    "sha256": sha256_file(
                        snapshot.root.joinpath(
                            *PurePosixPath(relative).parts
                        )
                    ),
                }
                for relative in FIXTURE_PATHS
            ]
        require(
            sha256_file(sidecar) == original_sidecar_sha256
            and sha256_file(godot) == original_godot_sha256
            and sha256_file(package_manifest) == original_manifest_sha256,
            "package or Godot artifact changed during acquisition",
        )
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
