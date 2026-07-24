#!/usr/bin/env python3
"""Acquire source-bound Sprint 11 external qualification receipts.

These profiles are deliberately separate from the short acceptance preflight.
Every real profile requires a clean package-source checkout. Reproducibility
invokes only the public ``packaging/build_macos.py`` CLI and binds both rebuilds
to the already-qualified release package. Host provenance invokes only the
public, content-minimizing host measurement CLI.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import pwd
import selectors
import signal
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Final, Iterator, cast

try:
    from tests.codex import sprint11_acquisition_paths as acquisition_paths
    from tests.codex import sprint11_host_provenance as host_provenance
    from tests.codex import sprint11_multi_project as multi_project
    from tests.codex import sprint11_packaged_regressions as package_contract
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_acquisition_paths as acquisition_paths
    import sprint11_host_provenance as host_provenance
    import sprint11_multi_project as multi_project
    import sprint11_packaged_regressions as package_contract

SCRIPT_DIR: Final = Path(__file__).resolve().parent
REPOSITORY_ROOT: Final = SCRIPT_DIR.parent.parent
MULTI_RECEIPT_SCHEMA: Final = "s11-multi-project-receipt/1.0"
REPRO_RECEIPT_SCHEMA: Final = "s11-reproducibility-receipt/1.0"
HOST_ACQUISITION_SCHEMA: Final = "s11-host-provenance-acquisition/1.0"
MULTI_CAPTURE_KIND: Final = "real_package_multi_project"
REPRO_CAPTURE_KIND: Final = "real_clean_rebuild"
HOST_CAPTURE_KIND: Final = "real_local_host_provenance"
MULTI_RUNNER: Final = "tests/codex/sprint11_multi_project.py"
BUILD_RUNNER: Final = "godot-codex-mcp/packaging/build_macos.py"
HOST_RUNNER: Final = "tests/codex/sprint11_host_provenance.py"
HOST_PROFILE_PATH: Final = (
    "godot-codex-mcp/product/host-coordinate-profile.v1.json"
)
HOST_MEASUREMENT_SCHEMA_PATH: Final = (
    "godot-codex-mcp/schemas/godot_codex/"
    "sprint11-host-provenance.schema.json"
)
HOST_ACQUISITION_SCHEMA_PATH: Final = (
    "godot-codex-mcp/schemas/godot_codex/"
    "sprint11-host-provenance-acquisition.schema.json"
)
PACKAGE_MANIFEST_NAME: Final = "sprint11-package-manifest.json"
MAX_RECEIPT_BYTES: Final = 262_144
MAX_REPORT_BYTES: Final = multi_project.REPORT_LIMIT
MAX_OUTPUT_BYTES: Final = 1_048_576
MAX_COMMAND_SECONDS: Final = 180
MAX_PACKAGE_FILE_BYTES: Final = 256 * 1024 * 1024
MAX_PACKAGE_TREE_BYTES: Final = 512 * 1024 * 1024
GIT_EXECUTABLE: Final = "/usr/bin/git"
SAFE_SYSTEM_PATHS: Final = (
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/Library/Apple/usr/bin",
)
SAFE_LOCATION_VARIABLES: Final = (
    "DEVELOPER_DIR",
    "SDKROOT",
)
SAFE_SCALAR_VARIABLES: Final = ("MACOSX_DEPLOYMENT_TARGET",)
MULTI_ASSERTION_FIELDS: Final = frozenset(
    {
        "two_real_editors",
        "two_package_sidecars",
        "separate_action_only_approvals",
        "validation_reports_isolated",
        "readback_and_targeted_undo",
        "foreign_ids_rejected",
        "target_fault_isolated",
        "isolation_matrix_complete",
        "copied_and_swapped_discovery_token_rejected",
        "config_root_cwd_swaps_rejected",
        "editor_restart_isolated",
        "cache_rebuild_isolated",
        "package_version_mismatch_rejected",
        "no_cross_project_leakage",
        "source_unchanged",
    }
)
MULTI_CLEANUP_FIELDS: Final = frozenset(
    {
        "editor_processes_stopped",
        "game_processes_stopped",
        "sidecar_processes_stopped",
        "temporary_workspaces_removed",
    }
)
MULTI_REDACTION_FIELDS: Final = frozenset(
    {
        "absolute_paths_absent",
        "native_ids_absent",
        "secrets_absent",
        "source_content_absent",
    }
)


class AcquisitionError(RuntimeError):
    """Raised when an external acquisition cannot qualify."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AcquisitionError(message)


def _exact(value: Any, fields: frozenset[str], label: str) -> dict[str, Any]:
    require(
        isinstance(value, dict) and set(value) == set(fields),
        f"{label} fields differ",
    )
    return cast(dict[str, Any], value)


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
        raise AcquisitionError("acquisition value is not canonical JSON") from error


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def _sha256_file_record(
    path: Path,
    *,
    maximum: int = MAX_PACKAGE_TREE_BYTES,
) -> tuple[str, int]:
    digest = hashlib.sha256()
    try:
        size = acquisition_paths.update_digest_from_regular_file(
            digest,
            path,
            maximum=maximum,
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise AcquisitionError("acquisition artifact cannot be hashed") from error
    return "sha256:" + digest.hexdigest(), size


def sha256_file(
    path: Path,
    *,
    maximum: int = MAX_PACKAGE_TREE_BYTES,
) -> str:
    return _sha256_file_record(path, maximum=maximum)[0]


def require_regular(path: Path, *, executable: bool = False) -> None:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise AcquisitionError("required acquisition artifact is missing") from error
    require(
        stat.S_ISREG(metadata.st_mode) and not path.is_symlink(),
        "acquisition artifact is not a regular file",
    )
    if executable:
        require(metadata.st_mode & 0o111 != 0, "acquisition artifact is not executable")


def _strict_json_payload(
    path: Path,
    *,
    maximum: int,
    label: str,
) -> tuple[dict[str, Any], bytes]:
    try:
        value = acquisition_paths.read_regular_file(path, maximum=maximum)
    except acquisition_paths.AcquisitionPathError as error:
        raise AcquisitionError(f"{label} is unreadable") from error
    require(value, f"{label} is empty")
    try:
        document = package_contract.strict_json_bytes(
            value,
            label=label,
            maximum=maximum,
        )
    except package_contract.PackagedRegressionError as error:
        raise AcquisitionError(f"{label} is invalid") from error
    return document, value


def strict_json(path: Path, *, maximum: int, label: str) -> dict[str, Any]:
    return _strict_json_payload(
        path,
        maximum=maximum,
        label=label,
    )[0]


def repository_head() -> str:
    result = _run_git(REPOSITORY_ROOT, "rev-parse", "--verify", "HEAD")
    try:
        value = result.stdout.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise AcquisitionError("repository coordinate is unavailable") from error
    require(
        result.exit_code == 0
        and package_contract.COMMIT_RE.fullmatch(value) is not None,
        "repository HEAD differs",
    )
    return value


def require_clean_checkout(
    *,
    allowed_untracked_roots: Sequence[Path] = (),
) -> None:
    result = _run_git(
        REPOSITORY_ROOT,
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
    )
    allowed_prefixes: list[bytes] = []
    for root in allowed_untracked_roots:
        try:
            relative = root.relative_to(REPOSITORY_ROOT)
        except ValueError as error:
            raise AcquisitionError(
                "allowed acquisition output escapes repository"
            ) from error
        allowed_prefixes.append(os.fsencode(relative.as_posix()))
    records = [record for record in result.stdout.split(b"\0") if record]
    allowed = all(
        record.startswith(b"?? ")
        and any(
            record[3:] == prefix or record[3:].startswith(prefix + b"/")
            for prefix in allowed_prefixes
        )
        for record in records
    )
    require(
        result.exit_code == 0
        and (not records or bool(allowed_prefixes) and allowed),
        "external acquisition requires a completely clean checkout",
    )


def fixture_records(
    repository: Path = REPOSITORY_ROOT,
) -> list[dict[str, Any]]:
    fixture_relative = multi_project.s9.FIXTURE_ROOT.relative_to(
        REPOSITORY_ROOT
    )
    golden_relative = multi_project.s9.GOLDEN_PATH.relative_to(
        REPOSITORY_ROOT
    )
    root = repository / fixture_relative
    paths = [
        path
        for path in sorted(root.rglob("*"))
        if path.is_file() and ".godot" not in path.parts
    ]
    paths.append(repository / golden_relative)
    records: list[dict[str, Any]] = []
    for path in sorted(set(paths)):
        relative = path.relative_to(repository).as_posix()
        records.append({"path": relative, "sha256": sha256_file(path)})
    require(1 <= len(records) <= 128, "multi-project fixture set differs")
    return records


@dataclass(frozen=True)
class Execution:
    exit_code: int
    stdout: bytes
    stderr: bytes
    duration_ms: int


Executor = Callable[[tuple[str, ...], Path, float], Execution]


class _DigestFanout:
    def __init__(self, *digests: Any) -> None:
        self._digests = digests

    def update(self, content: bytes) -> None:
        for digest in self._digests:
            digest.update(content)


def _process_group_state(process_id: int) -> str:
    try:
        os.killpg(process_id, 0)
    except ProcessLookupError:
        return "missing"
    except PermissionError:
        return "inaccessible"
    except OSError as error:
        raise AcquisitionError(
            "external acquisition process-group state is unavailable"
        ) from error
    return "present"


def _process_group_exists(process_id: int) -> bool:
    return _process_group_state(process_id) != "missing"


def _reap_process_leader(process: subprocess.Popen[bytes]) -> None:
    """Always reap the direct child, even when group cleanup itself fails."""

    try:
        process.wait(timeout=2.0)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        process.kill()
    except ProcessLookupError:
        pass
    except OSError as error:
        raise AcquisitionError(
            "external acquisition process leader cannot be terminated"
        ) from error
    try:
        process.wait(timeout=2.0)
    except subprocess.TimeoutExpired as error:
        raise AcquisitionError(
            "external acquisition process leader was not reaped"
        ) from error


def _stop_process_group(process: subprocess.Popen[bytes]) -> None:
    """Bounded termination for the acquisition command and every descendant."""

    try:
        for requested_signal, wait_seconds in (
            (signal.SIGTERM, 2.0),
            (signal.SIGKILL, 2.0),
        ):
            leader_exited = process.poll() is not None
            state = _process_group_state(process.pid)
            if state == "missing" or (
                state == "inaccessible" and leader_exited
            ):
                return
            try:
                os.killpg(process.pid, requested_signal)
            except ProcessLookupError:
                return
            except PermissionError as error:
                if process.poll() is not None:
                    return
                raise AcquisitionError(
                    "external acquisition process group cannot be terminated"
                ) from error
            except OSError as error:
                raise AcquisitionError(
                    "external acquisition process group cannot be terminated"
                ) from error
            deadline = time.monotonic() + wait_seconds
            while time.monotonic() < deadline:
                leader_exited = process.poll() is not None
                state = _process_group_state(process.pid)
                if state == "missing" or (
                    state == "inaccessible" and leader_exited
                ):
                    return
                time.sleep(0.01)
            if requested_signal == signal.SIGKILL:
                # A successfully delivered SIGKILL cannot be ignored. The
                # group may remain briefly observable only through zombies.
                return
        raise AcquisitionError(
            "external acquisition process group did not stop"
        )
    finally:
        _reap_process_leader(process)


def _close_process_streams(process: subprocess.Popen[bytes]) -> None:
    for stream in (process.stdout, process.stderr):
        if stream is not None:
            stream.close()


def _execute(
    argv: tuple[str, ...],
    cwd: Path,
    timeout: float,
    *,
    environment: Mapping[str, str],
) -> Execution:
    started = time.monotonic()
    process: subprocess.Popen[bytes] | None = None
    selector = selectors.DefaultSelector()
    stdout_bytes = bytearray()
    stderr_bytes = bytearray()
    deadline = started + timeout
    try:
        process = subprocess.Popen(
            argv,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=dict(environment),
            start_new_session=True,
            bufsize=0,
        )
        require(
            process.stdout is not None and process.stderr is not None,
            "external acquisition command streams are unavailable",
        )
        for stream, label in (
            (process.stdout, "stdout"),
            (process.stderr, "stderr"),
        ):
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ, label)
        while selector.get_map() or process.poll() is None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                _stop_process_group(process)
                raise AcquisitionError("external acquisition command timed out")
            events = selector.select(min(remaining, 0.1))
            for key, _mask in events:
                stream = key.fileobj
                try:
                    chunk = os.read(stream.fileno(), 64 * 1024)
                except BlockingIOError:
                    continue
                if not chunk:
                    selector.unregister(stream)
                    stream.close()
                    continue
                destination = (
                    stdout_bytes if key.data == "stdout" else stderr_bytes
                )
                if len(destination) + len(chunk) > MAX_OUTPUT_BYTES:
                    _stop_process_group(process)
                    raise AcquisitionError(
                        "external acquisition output exceeds byte bound"
                    )
                destination.extend(chunk)
            if process.poll() is not None and _process_group_exists(process.pid):
                _stop_process_group(process)
                raise AcquisitionError(
                    "external acquisition command left child processes"
                )
        exit_code = process.wait(timeout=max(deadline - time.monotonic(), 0.001))
        if _process_group_exists(process.pid):
            _stop_process_group(process)
            raise AcquisitionError(
                "external acquisition command left child processes"
            )
    except AcquisitionError:
        if process is not None and _process_group_exists(process.pid):
            _stop_process_group(process)
        raise
    except (OSError, subprocess.TimeoutExpired) as error:
        if process is not None and _process_group_exists(process.pid):
            _stop_process_group(process)
        raise AcquisitionError("external acquisition command failed") from error
    finally:
        selector.close()
        if process is not None:
            _close_process_streams(process)
    return Execution(
        exit_code=exit_code,
        stdout=bytes(stdout_bytes),
        stderr=bytes(stderr_bytes),
        duration_ms=int((time.monotonic() - started) * 1_000),
    )


def _validated_location(
    value: str | None,
    *,
    label: str,
) -> str | None:
    if value is None:
        return None
    require(
        value
        and "\0" not in value
        and len(value.encode("utf-8")) <= 4096
        and Path(value).is_absolute(),
        f"{label} environment location is invalid",
    )
    return value


def _tool_home(
    fallback_name: str,
) -> str | None:
    try:
        account_home = Path(pwd.getpwuid(os.getuid()).pw_dir)
    except (KeyError, OSError) as error:
        raise AcquisitionError("toolchain account home is unavailable") from error
    require(account_home.is_absolute(), "toolchain account home differs")
    candidate = account_home / fallback_name
    return str(candidate) if candidate.is_dir() else None


def _acquisition_environment(
    *,
    private_root: Path,
    include_toolchain: bool,
    source: Mapping[str, str] | None = None,
) -> dict[str, str]:
    inherited = os.environ if source is None else source
    home = private_root / "home"
    temporary = private_root / "tmp"
    home.mkdir(mode=0o700)
    temporary.mkdir(mode=0o700)
    environment = {
        "HOME": str(home),
        "LANG": "C",
        "LC_ALL": "C",
        "NO_COLOR": "1",
        "PATH": ":".join(SAFE_SYSTEM_PATHS),
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONHASHSEED": "0",
        "TMPDIR": str(temporary),
        "TZ": "UTC",
    }
    for name in SAFE_LOCATION_VARIABLES:
        value = _validated_location(inherited.get(name), label=name)
        if value is not None:
            environment[name] = value
    for name in SAFE_SCALAR_VARIABLES:
        value = inherited.get(name)
        if value is not None:
            require(
                value
                and value.isascii()
                and len(value) <= 64
                and all(
                    character.isdigit() or character == "."
                    for character in value
                ),
                f"{name} environment value is invalid",
            )
            environment[name] = value
    if include_toolchain:
        cargo_home = _tool_home(".cargo")
        rustup_home = _tool_home(".rustup")
        tool_paths: list[str] = []
        for name, value in (
            ("CARGO_HOME", cargo_home),
            ("RUSTUP_HOME", rustup_home),
        ):
            if value is not None:
                environment[name] = value
                binary_directory = Path(value) / "bin"
                if binary_directory.is_dir():
                    tool_paths.append(str(binary_directory))
        environment["PATH"] = ":".join(
            dict.fromkeys((*tool_paths, *SAFE_SYSTEM_PATHS))
        )
    return environment


def _safe_temporary_root(prefix: str) -> tempfile.TemporaryDirectory[str]:
    try:
        parent = Path("/tmp").resolve(strict=True)
    except OSError as error:
        raise AcquisitionError(
            "canonical private temporary parent is unavailable"
        ) from error
    require(
        parent.is_dir() and parent.is_absolute(),
        "canonical private temporary parent differs",
    )
    return tempfile.TemporaryDirectory(prefix=prefix, dir=parent)


def execute(argv: tuple[str, ...], cwd: Path, timeout: float) -> Execution:
    with _safe_temporary_root(".s11-acquisition-process.") as private:
        environment = _acquisition_environment(
            private_root=Path(private),
            include_toolchain=True,
        )
        return _execute(
            argv,
            cwd,
            timeout,
            environment=environment,
        )


def execute_host(argv: tuple[str, ...], cwd: Path, timeout: float) -> Execution:
    """Run host measurement without forwarding account or API credentials."""

    with _safe_temporary_root(".s11-host-process.") as private:
        environment = _acquisition_environment(
            private_root=Path(private),
            include_toolchain=False,
        )
        return _execute(
            argv,
            cwd,
            timeout,
            environment=environment,
        )


def _git_environment() -> dict[str, str]:
    return {
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_OPTIONAL_LOCKS": "0",
        "GIT_TERMINAL_PROMPT": "0",
        "HOME": "/var/empty",
        "LANG": "C",
        "LC_ALL": "C",
        "NO_COLOR": "1",
        "PATH": "/usr/bin:/bin",
    }


def _run_git(repository: Path, *arguments: str) -> Execution:
    return _execute(
        (
            GIT_EXECUTABLE,
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-C",
            str(repository),
            *arguments,
        ),
        repository,
        60,
        environment=_git_environment(),
    )


@dataclass(frozen=True)
class SourceSnapshot:
    root: Path
    source_commit: str


def _snapshot_file_digest(
    snapshot: SourceSnapshot,
    relative_path: str,
) -> str:
    path = snapshot.root / relative_path
    require_regular(path)
    digest = sha256_file(path, maximum=MAX_PACKAGE_FILE_BYTES)
    committed = _run_git(
        snapshot.root,
        "show",
        f"{snapshot.source_commit}:{relative_path}",
    )
    require(
        committed.exit_code == 0
        and sha256_bytes(committed.stdout) == digest,
        "snapshot runner differs from declared source commit",
    )
    return digest


def _verify_snapshot_file(
    snapshot: SourceSnapshot,
    relative_path: str,
    expected_digest: str,
) -> None:
    require(
        _snapshot_file_digest(snapshot, relative_path) == expected_digest,
        "snapshot runner changed during acquisition",
    )


def _set_snapshot_writable(root: Path, writable: bool) -> None:
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


def _verify_snapshot_coordinate(snapshot: SourceSnapshot) -> None:
    head = _run_git(snapshot.root, "rev-parse", "--verify", "HEAD")
    status = _run_git(
        snapshot.root,
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
    )
    require(
        head.exit_code == 0
        and head.stdout.decode("ascii", errors="strict").strip()
        == snapshot.source_commit
        and status.exit_code == 0
        and status.stdout == b"",
        "private source snapshot changed during acquisition",
    )


@contextlib.contextmanager
def source_snapshot(
    source_commit: str,
    *,
    repository: Path = REPOSITORY_ROOT,
) -> Iterator[SourceSnapshot]:
    require(
        package_contract.COMMIT_RE.fullmatch(source_commit) is not None,
        "snapshot source commit differs",
    )
    with _safe_temporary_root(".s11-source-snapshot.") as private:
        root = Path(private) / "source"
        added = False
        try:
            execution = _run_git(
                repository,
                "worktree",
                "add",
                "--detach",
                "--quiet",
                str(root),
                source_commit,
            )
            require(
                execution.exit_code == 0,
                "private source snapshot could not be created",
            )
            added = True
            snapshot = SourceSnapshot(root=root, source_commit=source_commit)
            _verify_snapshot_coordinate(snapshot)
            _set_snapshot_writable(root, False)
            yield snapshot
            _verify_snapshot_coordinate(snapshot)
        finally:
            if added:
                try:
                    _set_snapshot_writable(root, True)
                except OSError:
                    pass
                removal = _run_git(
                    repository,
                    "worktree",
                    "remove",
                    "--force",
                    str(root),
                )
                if removal.exit_code != 0:
                    shutil.rmtree(root, ignore_errors=True)
                    prune = _run_git(repository, "worktree", "prune")
                    require(
                        prune.exit_code == 0 and not root.exists(),
                        "private source snapshot could not be removed",
                    )
                require(
                    not root.exists(),
                    "private source snapshot was not removed",
                )


def _command_record(
    *,
    command_id: str,
    template: Sequence[str],
    runner_path: str,
    runner_sha256: str,
    execution: Execution,
) -> dict[str, Any]:
    require(
        execution.exit_code == 0
        and 0 <= execution.duration_ms <= MAX_COMMAND_SECONDS * 1_000,
        f"{command_id} failed or exceeded its bound",
    )
    require(
        package_contract.DIGEST_RE.fullmatch(runner_sha256) is not None,
        f"{command_id} runner binding differs",
    )
    return {
        "id": command_id,
        "cwd": ".",
        "argv_template": list(template),
        "command_sha256": sha256_bytes(
            canonical_json({"cwd": ".", "argv": list(template)})
        ),
        "runner_path": runner_path,
        "runner_sha256": runner_sha256,
        "stdout_sha256": sha256_bytes(execution.stdout),
        "stderr_sha256": sha256_bytes(execution.stderr),
        "exit_code": 0,
        "duration_ms": execution.duration_ms,
    }


def multi_command_template() -> tuple[str, ...]:
    return (
        "{python}",
        MULTI_RUNNER,
        "--godot",
        "{godot}",
        "--sidecar",
        "{package_sidecar}",
        "--expected-sidecar-sha256",
        "{package_sidecar_sha256}",
        "--expected-package-version",
        "{package_version}",
        "--timeout",
        "{timeout}",
        "--report",
        "{report}",
    )


def host_provenance_command_template() -> tuple[str, ...]:
    return (
        "{python}",
        HOST_RUNNER,
        "--profile",
        "{profile}",
        "--app-bundle",
        "{app_bundle}",
        "--app-executable",
        "{app_executable}",
        "--app-client",
        "{app_client}",
        "--vscode-bundle",
        "{vscode_bundle}",
        "--vscode-executable",
        "{vscode_executable}",
        "--extension-root",
        "{extension_root}",
        "--extension-package-json",
        "{extension_package_json}",
        "--ide-client",
        "{ide_client}",
        "--output",
        "{measurement}",
    )


def validate_multi_project_receipt(
    value: Any,
    *,
    report: Mapping[str, Any],
    expected_bindings: Mapping[str, Any],
) -> dict[str, Any]:
    receipt = _exact(
        value,
        frozenset(
            {
                "schema_version",
                "capture_kind",
                "status",
                "bindings",
                "fixtures",
                "command",
                "assertions",
                "cleanup",
                "redaction",
            }
        ),
        "multi-project receipt",
    )
    require(
        receipt["schema_version"] == MULTI_RECEIPT_SCHEMA
        and receipt["capture_kind"] == MULTI_CAPTURE_KIND
        and receipt["status"] == "passed"
        and len(canonical_json(receipt)) <= MAX_RECEIPT_BYTES,
        "multi-project receipt identity differs",
    )
    require(
        receipt["bindings"] == dict(expected_bindings)
        and set(cast(Mapping[str, Any], receipt["bindings"]))
        == {
            "package_source_commit",
            "package_version",
            "package_manifest_sha256",
            "package_build_provenance_sha256",
            "package_archive_sha256",
            "package_sidecar_path",
            "package_sidecar_sha256",
            "godot_version",
            "godot_commit",
            "godot_artifact_sha256",
            "registry_sha256",
        },
        "multi-project receipt package binding differs",
    )
    assertions = _exact(
        receipt["assertions"],
        MULTI_ASSERTION_FIELDS,
        "multi-project receipt assertions",
    )
    cleanup = _exact(
        receipt["cleanup"],
        MULTI_CLEANUP_FIELDS,
        "multi-project receipt cleanup",
    )
    redaction = _exact(
        receipt["redaction"],
        MULTI_REDACTION_FIELDS,
        "multi-project receipt redaction",
    )
    require(
        all(flag is True for flag in assertions.values())
        and all(flag is True for flag in cleanup.values())
        and all(flag is True for flag in redaction.values()),
        "multi-project receipt assertions differ",
    )
    try:
        validated_report = multi_project.validate_report(dict(report))
    except multi_project.s9.WorkflowError as error:
        raise AcquisitionError("multi-project live report differs") from error
    report_artifacts = cast(Mapping[str, Any], validated_report["artifacts"])
    require(
        report_artifacts["godot_sha256"]
        == expected_bindings["godot_artifact_sha256"]
        and report_artifacts["sidecar_sha256"]
        == expected_bindings["package_sidecar_sha256"],
        "multi-project receipt/report artifact binding differs",
    )
    matrix = cast(
        Mapping[str, Any],
        validated_report["assertions"],
    )["fault_matrix"]
    require(
        tuple(item["case_id"] for item in matrix)
        == multi_project.ISOLATION_CASE_ORDER,
        "multi-project receipt matrix binding differs",
    )
    return receipt


def acquire_multi_project(
    *,
    artifact_root: Path,
    package_manifest: Path,
    godot: Path,
    output_root: Path,
    timeout: float,
    executor: Executor = execute,
    version_probe: Callable[[Path], str] = package_contract.probe_godot_version,
    check_repository: bool = True,
) -> dict[str, Any]:
    require(1 <= timeout <= MAX_COMMAND_SECONDS, "timeout differs")
    artifact_root = artifact_root.resolve(strict=True)
    package_manifest = package_manifest.resolve(strict=True)
    godot = godot.resolve(strict=True)
    try:
        bindings, sidecar = package_contract._manifest_bindings(
            package_manifest,
            artifact_root,
            godot,
            version_probe=version_probe,
        )
    except package_contract.PackagedRegressionError as error:
        raise AcquisitionError("package binding differs") from error
    package_document = strict_json(
        package_manifest,
        maximum=256 * 1024,
        label="detached package manifest",
    )
    package_version = package_document.get("package_version")
    require(
        isinstance(package_version, str)
        and multi_project.PACKAGE_VERSION_RE.fullmatch(package_version)
        is not None,
        "package version binding differs",
    )
    bindings = {**bindings, "package_version": package_version}
    if check_repository:
        require(
            repository_head() == bindings["package_source_commit"],
            "multi-project source differs from package source",
        )
        require_clean_checkout()
    try:
        destination = acquisition_paths.prepare_repository_directory(
            output_root,
            repository=REPOSITORY_ROOT,
            prefix=".s11-multi-project.",
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise AcquisitionError(
            "multi-project output must be a safe new repository-local directory"
        ) from error
    output_root = destination.target
    staging = destination.staging
    try:
        report_path = staging / "report.json"
        template = multi_command_template()
        substitutions = {
            "{python}": sys.executable,
            "{godot}": str(godot),
            "{package_sidecar}": str(sidecar),
            "{package_sidecar_sha256}": str(
                bindings["package_sidecar_sha256"]
            ),
            "{package_version}": package_version,
            "{timeout}": str(timeout),
            "{report}": str(report_path),
        }
        argv = tuple(substitutions.get(item, item) for item in template)
        with source_snapshot(
            cast(str, bindings["package_source_commit"])
        ) as snapshot:
            runner_sha256 = _snapshot_file_digest(snapshot, MULTI_RUNNER)
            execution = executor(argv, snapshot.root, timeout)
            _verify_snapshot_file(
                snapshot,
                MULTI_RUNNER,
                runner_sha256,
            )
            fixtures = fixture_records(snapshot.root)
        command = _command_record(
            command_id="multi_project_live",
            template=template,
            runner_path=MULTI_RUNNER,
            runner_sha256=runner_sha256,
            execution=execution,
        )
        report = strict_json(
            report_path,
            maximum=MAX_REPORT_BYTES,
            label="multi-project live report",
        )
        try:
            validated = multi_project.validate_report(report)
        except multi_project.s9.WorkflowError as error:
            raise AcquisitionError("multi-project live report differs") from error
        artifacts = cast(Mapping[str, Any], validated["artifacts"])
        require(
            artifacts["godot_sha256"] == bindings["godot_artifact_sha256"]
            and artifacts["sidecar_sha256"]
            == bindings["package_sidecar_sha256"],
            "multi-project live artifact binding differs",
        )
        final_report = (output_root / "report.json").relative_to(
            REPOSITORY_ROOT
        ).as_posix()
        command.update(
            {
                "report_path": final_report,
                "report_sha256": sha256_file(report_path),
            }
        )
        if check_repository:
            require(
                repository_head() == bindings["package_source_commit"],
                "repository coordinate changed during multi-project acquisition",
            )
            require_clean_checkout(allowed_untracked_roots=(staging,))
        receipt = {
            "schema_version": MULTI_RECEIPT_SCHEMA,
            "capture_kind": MULTI_CAPTURE_KIND,
            "status": "passed",
            "bindings": bindings,
            "fixtures": fixtures,
            "command": command,
            "assertions": {
                "two_real_editors": True,
                "two_package_sidecars": True,
                "separate_action_only_approvals": True,
                "validation_reports_isolated": True,
                "readback_and_targeted_undo": True,
                "foreign_ids_rejected": True,
                "target_fault_isolated": True,
                "isolation_matrix_complete": True,
                "copied_and_swapped_discovery_token_rejected": True,
                "config_root_cwd_swaps_rejected": True,
                "editor_restart_isolated": True,
                "cache_rebuild_isolated": True,
                "package_version_mismatch_rejected": True,
                "no_cross_project_leakage": True,
                "source_unchanged": True,
            },
            "cleanup": {
                "editor_processes_stopped": True,
                "game_processes_stopped": True,
                "sidecar_processes_stopped": True,
                "temporary_workspaces_removed": True,
            },
            "redaction": {
                "absolute_paths_absent": True,
                "native_ids_absent": True,
                "secrets_absent": True,
                "source_content_absent": True,
            },
        }
        validate_multi_project_receipt(
            receipt,
            report=validated,
            expected_bindings=bindings,
        )
        encoded = canonical_json(receipt) + b"\n"
        require(len(encoded) <= MAX_RECEIPT_BYTES, "multi-project receipt is unbounded")
        (staging / "receipt.json").write_bytes(encoded)
        destination.publish()
        return receipt
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def acquire_host_provenance(
    *,
    app_bundle: Path,
    app_executable: Path,
    app_client: Path,
    vscode_bundle: Path,
    vscode_executable: Path,
    extension_root: Path,
    extension_package_json: Path,
    ide_client: Path,
    output_root: Path,
    timeout: float,
    executor: Executor = execute_host,
) -> dict[str, Any]:
    require(1 <= timeout <= MAX_COMMAND_SECONDS, "timeout differs")
    source_commit = repository_head()
    require_clean_checkout()
    try:
        destination = acquisition_paths.prepare_repository_directory(
            output_root,
            repository=REPOSITORY_ROOT,
            prefix=".s11-host-provenance.",
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise AcquisitionError(
            "host provenance output must be a safe new repository-local directory"
        ) from error
    output_root = destination.target
    staging = destination.staging
    try:
        measurement_path = staging / "measurement.json"
        template = host_provenance_command_template()
        substitutions = {
            "{python}": sys.executable,
            "{app_bundle}": str(app_bundle),
            "{app_executable}": str(app_executable),
            "{app_client}": str(app_client),
            "{vscode_bundle}": str(vscode_bundle),
            "{vscode_executable}": str(vscode_executable),
            "{extension_root}": str(extension_root),
            "{extension_package_json}": str(extension_package_json),
            "{ide_client}": str(ide_client),
            "{measurement}": str(measurement_path),
        }
        with source_snapshot(source_commit) as snapshot:
            runner = snapshot.root / HOST_RUNNER
            profile = snapshot.root / HOST_PROFILE_PATH
            measurement_schema = (
                snapshot.root / HOST_MEASUREMENT_SCHEMA_PATH
            )
            acquisition_schema = (
                snapshot.root / HOST_ACQUISITION_SCHEMA_PATH
            )
            for source in (
                runner,
                profile,
                measurement_schema,
                acquisition_schema,
            ):
                require_regular(source)
            runner_sha256 = _snapshot_file_digest(snapshot, HOST_RUNNER)
            substitutions["{profile}"] = str(profile)
            argv = tuple(
                substitutions.get(item, item)
                for item in template
            )
            execution = executor(argv, snapshot.root, timeout)
            require(
                execution.exit_code == 0,
                "host provenance runner failed",
            )
            _verify_snapshot_file(
                snapshot,
                HOST_RUNNER,
                runner_sha256,
            )
            measurement = strict_json(
                measurement_path,
                maximum=host_provenance.MAX_OUTPUT_BYTES,
                label="host provenance measurement",
            )
            try:
                validated = (
                    host_provenance.validate_host_provenance_document(
                        measurement,
                        profile_path=profile,
                    )
                )
            except host_provenance.HostProvenanceError as error:
                raise AcquisitionError(
                    "host provenance measurement differs"
                ) from error
            measurement_schema_sha256 = sha256_file(
                measurement_schema,
                maximum=MAX_RECEIPT_BYTES,
            )
            acquisition_schema_sha256 = sha256_file(
                acquisition_schema,
                maximum=MAX_RECEIPT_BYTES,
            )
        command = _command_record(
            command_id="host_provenance",
            template=template,
            runner_path=HOST_RUNNER,
            runner_sha256=runner_sha256,
            execution=execution,
        )
        measurement_sha256 = sha256_file(
            measurement_path,
            maximum=host_provenance.MAX_OUTPUT_BYTES,
        )
        final_measurement_path = (
            output_root / "measurement.json"
        ).relative_to(REPOSITORY_ROOT).as_posix()
        command.update(
            {
                "report_path": final_measurement_path,
                "report_sha256": measurement_sha256,
            }
        )
        require(
            repository_head() == source_commit,
            "repository coordinate changed during host acquisition",
        )
        require_clean_checkout(allowed_untracked_roots=(staging,))
        receipt = {
            "schema_version": HOST_ACQUISITION_SCHEMA,
            "capture_kind": HOST_CAPTURE_KIND,
            "status": "passed",
            "source_commit": source_commit,
            "bindings": {
                "runner_path": HOST_RUNNER,
                "runner_sha256": runner_sha256,
                "measurement_schema_path": HOST_MEASUREMENT_SCHEMA_PATH,
                "measurement_schema_sha256": measurement_schema_sha256,
                "acquisition_schema_path": HOST_ACQUISITION_SCHEMA_PATH,
                "acquisition_schema_sha256": acquisition_schema_sha256,
                "host_coordinate_profile_path": HOST_PROFILE_PATH,
                "host_coordinate_profile_sha256": validated[
                    "host_coordinate_profile_sha256"
                ],
                "measurement_path": final_measurement_path,
                "measurement_sha256": measurement_sha256,
            },
            "command": command,
            "assertions": {
                "profile_matched": True,
                "extension_tree_complete": True,
                "code_signatures_verified": True,
                "symlinks_rejected": True,
                "source_checkout_unchanged": True,
            },
            "redaction": {
                "absolute_paths_absent": True,
                "account_identity_absent": True,
                "artifact_content_absent": True,
                "environment_secrets_absent": True,
            },
        }
        encoded = canonical_json(receipt) + b"\n"
        require(
            len(encoded) <= MAX_RECEIPT_BYTES,
            "host provenance acquisition receipt is unbounded",
        )
        (staging / "receipt.json").write_bytes(encoded)
        destination.publish()
        return receipt
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def reproducibility_command_template(build_id: str) -> tuple[str, ...]:
    require(build_id in {"a", "b"}, "rebuild ID differs")
    return (
        "{python}",
        BUILD_RUNNER,
        "--repository-root",
        ".",
        "--workspace",
        "godot-codex-mcp",
        "--output-dir",
        f"{{build_{build_id}}}",
        "--source-commit",
        "{source_commit}",
        "--godot-prerequisite",
        "{godot}",
    )


def _package_tree_digest(root: Path, manifest: Mapping[str, Any]) -> str:
    contents = manifest.get("contents")
    require(isinstance(contents, list) and contents, "package contents differ")
    digest = hashlib.sha256()
    seen: set[str] = set()
    total_bytes = 0
    for item in contents:
        record = _exact(
            item,
            frozenset({"bytes", "mode", "path", "sha256"}),
            "package content record",
        )
        relative = package_contract.safe_relative_path(
            record["path"],
            label="package content",
        )
        require(relative not in seen, "package content path is duplicated")
        seen.add(relative)
        path = root.joinpath(*PurePosixPath(relative).parts)
        try:
            metadata = path.lstat()
        except OSError as error:
            raise AcquisitionError(
                "rebuilt package content cannot be inspected"
            ) from error
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        file_digest = hashlib.sha256()
        try:
            size = acquisition_paths.update_digest_from_regular_file(
                _DigestFanout(digest, file_digest),
                path,
                maximum=MAX_PACKAGE_FILE_BYTES,
            )
        except acquisition_paths.AcquisitionPathError as error:
            raise AcquisitionError(
                "rebuilt package content cannot be read safely"
            ) from error
        require(
            isinstance(record["bytes"], int)
            and not isinstance(record["bytes"], bool)
            and record["bytes"] == size
            and record["sha256"]
            == "sha256:" + file_digest.hexdigest()
            and isinstance(record["mode"], str)
            and record["mode"] in {"0644", "0755"}
            and stat.S_IMODE(metadata.st_mode) == int(record["mode"], 8),
            "rebuilt package content binding differs",
        )
        total_bytes += size
        require(
            total_bytes <= MAX_PACKAGE_TREE_BYTES,
            "rebuilt package tree exceeds its aggregate bound",
        )
        digest.update(b"\0")
    return "sha256:" + digest.hexdigest()


def _build_output_record(
    output: Path,
    build_id: str,
    *,
    expected_source_commit: str,
) -> dict[str, Any]:
    try:
        output_metadata = output.lstat()
    except OSError as error:
        raise AcquisitionError("package output root is unavailable") from error
    require(
        stat.S_ISDIR(output_metadata.st_mode) and not output.is_symlink(),
        "package output root is unsafe",
    )
    manifest_path = output / PACKAGE_MANIFEST_NAME
    manifest_document, manifest_bytes = _strict_json_payload(
        manifest_path,
        maximum=package_contract.MAX_REPORT_BYTES,
        label=f"rebuild {build_id} manifest",
    )
    manifest = _exact(
        manifest_document,
        frozenset(
            {
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
        ),
        f"rebuild {build_id} manifest",
    )
    require(
        manifest.get("schema_version")
        == package_contract.PACKAGE_MANIFEST_SCHEMA,
        "rebuilt package manifest schema differs",
    )
    require(
        manifest["source_commit"] == expected_source_commit,
        "rebuilt package source commit differs",
    )
    provenance = manifest.get("build_provenance")
    require(
        isinstance(provenance, dict)
        and set(provenance) == package_contract.BUILD_PROVENANCE_FIELDS,
        "rebuilt package provenance fields differ",
    )
    for field in ("cargo_lock_sha256", "rust_toolchain_sha256"):
        require(
            isinstance(provenance[field], str)
            and package_contract.DIGEST_RE.fullmatch(provenance[field])
            is not None,
            "rebuilt package provenance digest differs",
        )
    require(
        isinstance(provenance["cargo_version"], str)
        and 0 < len(provenance["cargo_version"].encode("utf-8")) <= 256
        and isinstance(provenance["rustc_release"], str)
        and 0 < len(provenance["rustc_release"].encode("utf-8")) <= 128
        and isinstance(provenance["rustc_commit"], str)
        and package_contract.COMMIT_RE.fullmatch(provenance["rustc_commit"])
        is not None
        and provenance["fresh_target"] is True
        and provenance["target_triple"] == "aarch64-apple-darwin",
        "rebuilt package provenance differs",
    )
    archive = _exact(
        manifest.get("archive"),
        frozenset({"bytes", "path", "sha256"}),
        "rebuilt package archive record",
    )
    archive_relative = package_contract.safe_relative_path(
        archive.get("path"),
        label="rebuilt package archive",
    )
    archive_path = output.joinpath(*PurePosixPath(archive_relative).parts)
    archive_sha256, archive_bytes = _sha256_file_record(
        archive_path,
        maximum=MAX_PACKAGE_TREE_BYTES,
    )
    require(
        archive["sha256"] == archive_sha256
        and archive["bytes"] == archive_bytes,
        "rebuilt package archive binding differs",
    )
    return {
        "id": build_id,
        "manifest_sha256": sha256_bytes(manifest_bytes),
        "build_provenance_sha256": sha256_bytes(
            canonical_json(provenance)
        ),
        "archive_sha256": archive_sha256,
        "archive_bytes": archive_bytes,
        "package_tree_sha256": _package_tree_digest(output, manifest),
    }


def _comparable_output(record: Mapping[str, Any]) -> dict[str, Any]:
    return {key: record[key] for key in record if key != "id"}


def acquire_reproducibility(
    *,
    qualified_artifact_root: Path,
    godot_prerequisite: Path,
    build_root: Path,
    output_root: Path,
    timeout: float,
    executor: Executor = execute,
    check_repository: bool = True,
) -> dict[str, Any]:
    require(1 <= timeout <= MAX_COMMAND_SECONDS, "timeout differs")
    qualified_artifact_root = qualified_artifact_root.resolve(strict=True)
    godot = godot_prerequisite.resolve(strict=True)
    source_commit = repository_head()
    if check_repository:
        require_clean_checkout()
    qualified_output = _build_output_record(
        qualified_artifact_root,
        "qualified",
        expected_source_commit=source_commit,
    )
    try:
        build_root = acquisition_paths.create_external_build_directory(
            build_root,
            repository=REPOSITORY_ROOT,
        )
        destination = acquisition_paths.prepare_repository_directory(
            output_root,
            repository=REPOSITORY_ROOT,
            prefix=".s11-reproducibility.",
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise AcquisitionError("reproducibility output roots differ") from error
    output_root = destination.target
    staging = destination.staging
    command_records: list[dict[str, Any]] = []
    output_records: list[dict[str, Any]] = []
    try:
        with source_snapshot(source_commit) as snapshot:
            runner_sha256 = _snapshot_file_digest(snapshot, BUILD_RUNNER)
            for build_id in ("a", "b"):
                build_output = build_root / build_id
                template = reproducibility_command_template(build_id)
                substitutions = {
                    "{python}": sys.executable,
                    f"{{build_{build_id}}}": str(build_output),
                    "{source_commit}": source_commit,
                    "{godot}": str(godot),
                }
                argv = tuple(
                    substitutions.get(item, item) for item in template
                )
                execution = executor(argv, snapshot.root, timeout)
                _verify_snapshot_file(
                    snapshot,
                    BUILD_RUNNER,
                    runner_sha256,
                )
                command_records.append(
                    _command_record(
                        command_id=f"clean_rebuild_{build_id}",
                        template=template,
                        runner_path=BUILD_RUNNER,
                        runner_sha256=runner_sha256,
                        execution=execution,
                    )
                )
                output_records.append(
                    _build_output_record(
                        build_output,
                        build_id,
                        expected_source_commit=source_commit,
                    )
                )
        first, second = output_records
        require(
            _comparable_output(first) == _comparable_output(second),
            "clean rebuilds are not byte-for-byte reproducible",
        )
        require(
            _comparable_output(first)
            == _comparable_output(qualified_output),
            "clean rebuilds differ from qualified release package",
        )
        if check_repository:
            require(
                repository_head() == source_commit,
                "repository coordinate changed during clean rebuild",
            )
            require_clean_checkout(allowed_untracked_roots=(staging,))
        receipt = {
            "schema_version": REPRO_RECEIPT_SCHEMA,
            "capture_kind": REPRO_CAPTURE_KIND,
            "status": "passed",
            "bindings": {
                "source_commit": source_commit,
                "godot_artifact_sha256": sha256_file(godot),
                "build_runner_path": BUILD_RUNNER,
                "build_runner_sha256": runner_sha256,
                "qualified_manifest_sha256": qualified_output[
                    "manifest_sha256"
                ],
                "qualified_archive_sha256": qualified_output[
                    "archive_sha256"
                ],
                "qualified_archive_bytes": qualified_output["archive_bytes"],
                "qualified_package_tree_sha256": qualified_output[
                    "package_tree_sha256"
                ],
                "qualified_build_provenance_sha256": qualified_output[
                    "build_provenance_sha256"
                ],
            },
            "commands": command_records,
            "outputs": output_records,
            "reproducibility": {
                "manifest_bytes_equal": True,
                "archive_bytes_equal": True,
                "package_tree_equal": True,
                "source_checkout_unchanged": True,
            },
            "cleanup": {
                "build_processes_stopped": True,
                "repository_unchanged": True,
            },
            "redaction": {
                "absolute_paths_absent": True,
                "environment_secrets_absent": True,
                "source_content_absent": True,
            },
        }
        encoded = canonical_json(receipt) + b"\n"
        require(len(encoded) <= MAX_RECEIPT_BYTES, "reproducibility receipt is unbounded")
        (staging / "receipt.json").write_bytes(encoded)
        destination.publish()
        return receipt
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="profile", required=True)
    multi = subparsers.add_parser("multi-project")
    multi.add_argument("--artifact-root", type=Path, required=True)
    multi.add_argument("--package-manifest", type=Path, required=True)
    multi.add_argument("--godot", type=Path, required=True)
    multi.add_argument("--output-root", type=Path, required=True)
    multi.add_argument("--timeout", type=float, default=180.0)
    reproducibility = subparsers.add_parser("reproducibility")
    reproducibility.add_argument(
        "--qualified-artifact-root",
        type=Path,
        required=True,
    )
    reproducibility.add_argument("--godot-prerequisite", type=Path, required=True)
    reproducibility.add_argument("--build-root", type=Path, required=True)
    reproducibility.add_argument("--output-root", type=Path, required=True)
    reproducibility.add_argument("--timeout", type=float, default=180.0)
    host = subparsers.add_parser("host-provenance")
    host.add_argument("--app-bundle", type=Path, required=True)
    host.add_argument("--app-executable", type=Path, required=True)
    host.add_argument("--app-client", type=Path, required=True)
    host.add_argument("--vscode-bundle", type=Path, required=True)
    host.add_argument("--vscode-executable", type=Path, required=True)
    host.add_argument("--extension-root", type=Path, required=True)
    host.add_argument("--extension-package-json", type=Path, required=True)
    host.add_argument("--ide-client", type=Path, required=True)
    host.add_argument("--output-root", type=Path, required=True)
    host.add_argument("--timeout", type=float, default=180.0)
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    if arguments.profile == "multi-project":
        receipt = acquire_multi_project(
            artifact_root=arguments.artifact_root,
            package_manifest=arguments.package_manifest,
            godot=arguments.godot,
            output_root=arguments.output_root,
            timeout=arguments.timeout,
        )
    elif arguments.profile == "reproducibility":
        receipt = acquire_reproducibility(
            qualified_artifact_root=arguments.qualified_artifact_root,
            godot_prerequisite=arguments.godot_prerequisite,
            build_root=arguments.build_root,
            output_root=arguments.output_root,
            timeout=arguments.timeout,
        )
    else:
        receipt = acquire_host_provenance(
            app_bundle=arguments.app_bundle,
            app_executable=arguments.app_executable,
            app_client=arguments.app_client,
            vscode_bundle=arguments.vscode_bundle,
            vscode_executable=arguments.vscode_executable,
            extension_root=arguments.extension_root,
            extension_package_json=arguments.extension_package_json,
            ide_client=arguments.ide_client,
            output_root=arguments.output_root,
            timeout=arguments.timeout,
        )
    print(
        json.dumps(
            {
                "schema_version": receipt["schema_version"],
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
    except AcquisitionError as error:
        print(f"Sprint 11 external acquisition failed: {error}", file=sys.stderr)
        raise SystemExit(1)
