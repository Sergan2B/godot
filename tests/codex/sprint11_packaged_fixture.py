#!/usr/bin/env python3
"""Provision copied live fixtures through the installed Sprint 11 package.

The legacy Sprint 6-10 live runners also remain useful against a checkout
binary, so package provisioning is an explicit opt-in.  A qualifying package
runner activates this module with the installer-owned operations launcher and
data root.  Every copied fixture is then configured through the public
``setup --dry-run`` / exact ``--apply-plan`` lifecycle before its sidecar is
started.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import signal
import stat
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final, cast

import tomllib

try:
    from tests.codex import sprint11_process_scope as process_scope
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_process_scope as process_scope

SETUP_PLAN_SCHEMA: Final = "godot-codex-setup-plan/1.3"
SETUP_RESULT_SCHEMA: Final = "godot-codex-setup-result/1.1"
MAX_SETUP_OUTPUT_BYTES: Final = 256 * 1024
MAX_SETUP_DIFF_BYTES: Final = 64 * 1024
MAX_SETUP_SECONDS: Final = 30.0
MAX_PROJECT_FILES: Final = 4_096
MAX_PROJECT_FILE_BYTES: Final = 16 * 1024 * 1024
MAX_PROJECT_BYTES: Final = 64 * 1024 * 1024
MAX_PACKAGE_FILE_BYTES: Final = 256 * 1024 * 1024
MAX_PACKAGE_MANIFEST_BYTES: Final = 512 * 1024
PROCESS_READ_BYTES: Final = 64 * 1024
PROCESS_POLL_SECONDS: Final = 0.01
PROCESS_TERMINATE_SECONDS: Final = 1.0
DIGEST_RE: Final = re.compile(r"sha256:[0-9a-f]{64}\Z")
MISSING_DIGEST: Final = "sha256:missing"
PROJECT_ID_RE: Final = re.compile(r"project:sha256:[0-9a-f]{64}\Z")
EXPECTED_CHANGED_PATHS: Final = {
    ".codex/config.toml",
    ".godot/codex/setup-receipt-v1.json",
}
SETUP_LOCK_PATH: Final = ".godot/codex/setup-operation-v1.lock"
EXPECTED_DIRECTORY_PATHS: Final = {".codex", ".godot", ".godot/codex"}
REPOSITORY_ROOT: Final = Path(__file__).resolve().parents[2]
REGISTRY_PROFILE_PATH: Final = (
    REPOSITORY_ROOT / "godot-codex-mcp/product/registry-profile.v1.json"
)
CONFIG_OWNERSHIP_PREFIX: Final = "# godot-codex-setup-owner: "
SETUP_CHANGE_FIELDS: Final = {
    "path",
    "action",
    "before_digest",
    "after_digest",
    "diff",
}
CONFIG_TOP_LEVEL_KEYS: Final = {"mcp_servers"}
CONFIG_SERVER_KEYS: Final = {"godot_editor"}
GODOT_EDITOR_TABLE_KEYS: Final = {
    "command",
    "args",
    "env",
    "required",
    "startup_timeout_sec",
    "tool_timeout_sec",
    "default_tools_approval_mode",
    "enabled_tools",
}
UNSAFE_SERVER_KEYS: Final = {
    "authorization",
    "bearer_token",
    "environment",
    "env_vars",
    "headers",
    "http_headers",
    "token",
}
UNSAFE_DIFF_MARKERS: Final = (
    "/Users/",
    "/home/",
    "/private/",
    "/tmp/",
    "C:\\",
    "file://",
    "Authorization:",
    "Bearer ",
    "session.token",
)
SETUP_RECEIPT_FIELDS: Final = {
    "schema_version",
    "project_id",
    "package_version",
    "profile",
    "guidance",
    "package_identity_digest",
    "launcher_path_sha256",
    "launcher_file_sha256",
    "config_ownership_marker",
    "config_table_digest",
    "config_created",
    "config_file_mode",
    "agents_block_digest",
    "agents_separator",
    "agents_file_mode",
    "skill_file_digest",
    "skill_file_mode",
    "plan_digest",
}


class PackagedFixtureError(RuntimeError):
    """Raised when package-owned fixture provisioning cannot be proven."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise PackagedFixtureError(message)


def _strict_json(payload: bytes, *, label: str) -> dict[str, Any]:
    require(
        0 < len(payload) <= MAX_SETUP_OUTPUT_BYTES,
        f"{label} output exceeds its bound",
    )

    def reject_duplicate(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise PackagedFixtureError(f"{label} contains duplicate JSON members")
            result[key] = value
        return result

    def reject_constant(value: str) -> None:
        raise PackagedFixtureError(
            f"{label} contains a non-finite JSON value: {value}"
        )

    try:
        value = json.loads(
            payload,
            object_pairs_hook=reject_duplicate,
            parse_constant=reject_constant,
        )
    except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as error:
        raise PackagedFixtureError(f"{label} is not strict JSON") from error
    require(isinstance(value, dict), f"{label} is not a JSON object")
    return cast(dict[str, Any], value)


@dataclass(frozen=True)
class _ProcessResult:
    returncode: int
    stdout: bytes
    stderr: bytes


def _process_group_exists(process_group: int) -> bool:
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return False
    return True


def _terminate_process(process: subprocess.Popen[bytes]) -> None:
    if os.name == "posix":
        if _process_group_exists(process.pid):
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                pass
            deadline = time.monotonic() + PROCESS_TERMINATE_SECONDS
            while (
                _process_group_exists(process.pid)
                and time.monotonic() < deadline
            ):
                process.poll()
                time.sleep(PROCESS_POLL_SECONDS)
            if _process_group_exists(process.pid):
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
    elif process.poll() is None:
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


def _bounded_process(
    argv: list[str],
    *,
    environment: dict[str, str],
) -> _ProcessResult:
    options: dict[str, Any] = {}
    if os.name == "posix":
        options["start_new_session"] = True
    elif os.name == "nt":
        process_group = getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)
        if process_group:
            options["creationflags"] = process_group
    try:
        scoped_environment, command_scope = process_scope.bind_environment(
            environment
        )
    except process_scope.ProcessScopeError as error:
        raise PackagedFixtureError(
            "package-owned setup process scope could not be created"
        ) from error
    try:
        scoped_command = process_scope.scoped_argv(
            argv,
            command_scope,
        )
    except process_scope.ProcessScopeError as error:
        raise PackagedFixtureError(
            "package-owned setup command scope differs"
        ) from error
    try:
        process = subprocess.Popen(
            scoped_command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=scoped_environment,
            **options,
        )
    except OSError as error:
        raise PackagedFixtureError(
            "package-owned setup command failed"
        ) from error
    try:
        process_scope.activate_scope(command_scope, process)
    except process_scope.ProcessScopeError as error:
        failure = PackagedFixtureError(
            "package-owned setup process scope could not be activated"
        )
        failure.__cause__ = error
        _terminate_process(process)
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                try:
                    stream.close()
                except (OSError, ValueError) as cleanup_error:
                    failure.add_note(
                        "cleanup failure: package-owned setup capture "
                        f"could not be closed: {cleanup_error}"
                    )
        try:
            process_scope.close_scope(command_scope)
        except process_scope.ProcessScopeError as cleanup_error:
            failure.add_note(
                "cleanup failure: package-owned setup process scope "
                f"could not be closed: {cleanup_error}"
            )
        raise failure
    require(
        process.stdout is not None and process.stderr is not None,
        "package-owned setup capture pipes differ",
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
                remaining = MAX_SETUP_OUTPUT_BYTES + 1 - len(buffer)
                if remaining > 0:
                    buffer.extend(chunk[:remaining])
                if len(chunk) > remaining or len(buffer) > MAX_SETUP_OUTPUT_BYTES:
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
    deadline = time.monotonic() + MAX_SETUP_SECONDS
    terminated = False
    try:
        while True:
            try:
                process_scope.require_scope_healthy(command_scope)
            except process_scope.ProcessScopeError as error:
                raise PackagedFixtureError(
                    "package-owned setup process scope tracker failed"
                ) from error
            if exceeded.is_set():
                raise PackagedFixtureError(
                    "package-owned setup command output exceeds its bound"
                )
            with failure_lock:
                if reader_failures:
                    raise PackagedFixtureError(
                        "package-owned setup output capture failed"
                    )
            returncode = process.poll()
            if returncode is not None and returncode != 0 and not terminated:
                _terminate_process(process)
                terminated = True
            if returncode is not None and all(
                event.is_set() for event in completed
            ):
                break
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise PackagedFixtureError(
                    "package-owned setup command timed out"
                )
            time.sleep(min(PROCESS_POLL_SECONDS, remaining))
    except BaseException:
        if not terminated:
            _terminate_process(process)
            terminated = True
        raise
    finally:
        active_error = sys.exc_info()[1]
        if process.poll() is None and not terminated:
            _terminate_process(process)
        for reader in readers:
            reader.join(timeout=PROCESS_TERMINATE_SECONDS)
        escaped_scope = False
        try:
            escaped_scope = process_scope.close_scope(command_scope)
        except process_scope.ProcessScopeError as error:
            cleanup_error = PackagedFixtureError(
                "package-owned setup detached process scope could not be closed"
            )
            cleanup_error.__cause__ = error
            if active_error is None:
                raise cleanup_error
            active_error.add_note(f"cleanup failure: {cleanup_error}")
        if escaped_scope and active_error is None:
            raise PackagedFixtureError(
                "package-owned setup detached descendants escaped the process group"
            )
    require(
        not any(reader.is_alive() for reader in readers),
        "package-owned setup output capture did not stop",
    )
    with failure_lock:
        require(
            not reader_failures,
            "package-owned setup output capture failed",
        )
    require(
        not exceeded.is_set(),
        "package-owned setup command output exceeds its bound",
    )
    returncode = process.poll()
    require(returncode is not None, "package-owned setup command did not stop")
    if (
        returncode == 0
        and os.name == "posix"
        and _process_group_exists(process.pid)
    ):
        _terminate_process(process)
        raise PackagedFixtureError(
            "package-owned setup descendants remained running"
        )
    return _ProcessResult(
        returncode=returncode,
        stdout=bytes(buffers[0]),
        stderr=bytes(buffers[1]),
    )


def _run(
    argv: list[str],
    *,
    environment: dict[str, str],
    accepted_exit_codes: tuple[int, ...] = (0,),
) -> dict[str, Any]:
    result = _bounded_process(argv, environment=environment)
    require(
        result.returncode in accepted_exit_codes,
        "package-owned setup command returned an unexpected status",
    )
    return _strict_json(result.stdout, label="package-owned setup")


def _plain_directory(path: Path) -> bool:
    try:
        metadata = path.lstat()
    except OSError:
        return False
    return stat.S_ISDIR(metadata.st_mode) and not path.is_symlink()


def _plain_executable(path: Path) -> bool:
    try:
        metadata = path.lstat()
    except OSError:
        return False
    return (
        stat.S_ISREG(metadata.st_mode)
        and not path.is_symlink()
        and metadata.st_mode & 0o111 != 0
    )


def _digest_bytes(payload: bytes) -> str:
    return "sha256:" + hashlib.sha256(payload).hexdigest()


def _digest_file(path: Path, *, maximum_bytes: int) -> str:
    try:
        before = path.lstat()
    except OSError as error:
        raise PackagedFixtureError("package identity input is unavailable") from error
    require(
        stat.S_ISREG(before.st_mode)
        and not path.is_symlink()
        and 0 <= before.st_size <= maximum_bytes,
        "package identity input differs",
    )
    digest = hashlib.sha256()
    total = 0
    try:
        with path.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                total += len(chunk)
                require(total <= maximum_bytes, "package identity input exceeds its bound")
                digest.update(chunk)
        after = path.lstat()
    except OSError as error:
        raise PackagedFixtureError("package identity input is unreadable") from error
    require(
        stat.S_ISREG(after.st_mode)
        and not path.is_symlink()
        and total == before.st_size == after.st_size
        and (before.st_dev, before.st_ino) == (after.st_dev, after.st_ino),
        "package identity input changed while it was read",
    )
    return "sha256:" + digest.hexdigest()


def _canonical_full_beta_tools() -> tuple[str, ...]:
    try:
        payload = REGISTRY_PROFILE_PATH.read_bytes()
    except OSError as error:
        raise PackagedFixtureError("canonical registry profile is unavailable") from error
    profile = _strict_json(payload, label="canonical registry profile")
    tools = profile.get("tools")
    read_only_tools = profile.get("read_only_tools")
    fixed_resources = profile.get("fixed_resources")
    resource_templates = profile.get("resource_templates")
    require(
        set(profile)
        == {
            "schema_version",
            "profile_id",
            "tool_count",
            "tools",
            "read_only_tools",
            "fixed_resource_count",
            "fixed_resources",
            "resource_template_count",
            "resource_templates",
            "digest",
        }
        and profile.get("schema_version")
        == "godot-codex-registry-profile/1.0"
        and profile.get("profile_id") == "external-codex-beta-v1"
        and profile.get("tool_count") == 41
        and isinstance(tools, list)
        and tools == sorted(set(tools))
        and len(tools) == 41
        and all(isinstance(item, str) for item in tools)
        and isinstance(read_only_tools, list)
        and read_only_tools == sorted(set(read_only_tools))
        and set(read_only_tools).issubset(tools)
        and isinstance(fixed_resources, list)
        and profile.get("fixed_resource_count") == len(fixed_resources) == 4
        and isinstance(resource_templates, list)
        and profile.get("resource_template_count")
        == len(resource_templates)
        == 1,
        "canonical full-beta registry profile differs",
    )
    digest_payload = {
        "profile_id": profile["profile_id"],
        "tools": tools,
        "read_only_tools": read_only_tools,
        "fixed_resources": fixed_resources,
        "resource_templates": resource_templates,
    }
    digest = hashlib.sha256(
        json.dumps(
            digest_payload,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8")
    ).hexdigest()
    require(
        profile.get("digest") == digest,
        "canonical registry profile digest differs",
    )
    return tuple(cast(list[str], tools))


def _project_state(project_root: Path) -> dict[str, tuple[str, int, int, str]]:
    state: dict[str, tuple[str, int, int, str]] = {}
    total_bytes = 0
    for path in sorted(project_root.rglob("*")):
        try:
            metadata = path.lstat()
        except OSError as error:
            raise PackagedFixtureError("copied fixture state is unavailable") from error
        relative = path.relative_to(project_root).as_posix()
        require(
            len(state) < MAX_PROJECT_FILES
            and not stat.S_ISLNK(metadata.st_mode),
            "copied fixture state exceeds its path bound",
        )
        mode = stat.S_IMODE(metadata.st_mode)
        if stat.S_ISDIR(metadata.st_mode):
            state[relative] = ("directory", mode, 0, "")
            continue
        require(
            stat.S_ISREG(metadata.st_mode)
            and metadata.st_size <= MAX_PROJECT_FILE_BYTES,
            "copied fixture contains an unsupported entry",
        )
        try:
            payload = path.read_bytes()
        except OSError as error:
            raise PackagedFixtureError("copied fixture file is unreadable") from error
        total_bytes += len(payload)
        require(
            len(payload) == metadata.st_size and total_bytes <= MAX_PROJECT_BYTES,
            "copied fixture state exceeds its byte bound",
        )
        state[relative] = (
            "file",
            mode,
            len(payload),
            "sha256:" + hashlib.sha256(payload).hexdigest(),
        )
    return state


def _fixture_source_state(
    source: Path,
) -> dict[str, tuple[str, int, int, str, int, int, int, int]]:
    state: dict[
        str,
        tuple[str, int, int, str, int, int, int, int],
    ] = {}
    total_bytes = 0
    for path in sorted(source.rglob("*")):
        relative_path = path.relative_to(source)
        if relative_path.parts and relative_path.parts[0] == ".godot":
            continue
        try:
            metadata = path.lstat()
        except OSError as error:
            raise PackagedFixtureError(
                "fixture source changed during copy"
            ) from error
        relative = relative_path.as_posix()
        require(
            len(state) < MAX_PROJECT_FILES
            and not stat.S_ISLNK(metadata.st_mode),
            "fixture source exceeds its path bound",
        )
        mode = stat.S_IMODE(metadata.st_mode)
        if stat.S_ISDIR(metadata.st_mode):
            state[relative] = (
                "directory",
                mode,
                0,
                "",
                metadata.st_dev,
                metadata.st_ino,
                metadata.st_mtime_ns,
                metadata.st_ctime_ns,
            )
            continue
        require(
            stat.S_ISREG(metadata.st_mode)
            and 0 <= metadata.st_size <= MAX_PROJECT_FILE_BYTES,
            "fixture source contains an unsupported entry",
        )
        descriptor = -1
        try:
            flags = os.O_RDONLY
            if hasattr(os, "O_CLOEXEC"):
                flags |= os.O_CLOEXEC
            if hasattr(os, "O_NOFOLLOW"):
                flags |= os.O_NOFOLLOW
            descriptor = os.open(path, flags)
            before = os.fstat(descriptor)
            chunks: list[bytes] = []
            remaining = MAX_PROJECT_FILE_BYTES + 1
            while remaining:
                chunk = os.read(descriptor, min(1024 * 1024, remaining))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
            payload = b"".join(chunks)
            after = os.fstat(descriptor)
        except OSError as error:
            raise PackagedFixtureError(
                "fixture source file is unreadable"
            ) from error
        finally:
            if descriptor >= 0:
                os.close(descriptor)
        identity_before = (
            before.st_dev,
            before.st_ino,
            before.st_mode,
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        )
        identity_after = (
            after.st_dev,
            after.st_ino,
            after.st_mode,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        )
        require(
            identity_before == identity_after
            and stat.S_ISREG(before.st_mode)
            and len(payload) == before.st_size,
            "fixture source changed during copy",
        )
        total_bytes += len(payload)
        require(
            total_bytes <= MAX_PROJECT_BYTES,
            "fixture source exceeds its byte bound",
        )
        state[relative] = (
            "file",
            mode,
            len(payload),
            "sha256:" + hashlib.sha256(payload).hexdigest(),
            before.st_dev,
            before.st_ino,
            before.st_mtime_ns,
            before.st_ctime_ns,
        )
    return state


def _remove_partial_fixture(destination: Path) -> None:
    """Remove a copytree result even after read-only source modes were copied."""

    try:
        destination_metadata = destination.lstat()
    except FileNotFoundError:
        return
    except OSError as error:
        raise PackagedFixtureError(
            "partial fixture cleanup endpoint is unavailable"
        ) from error
    try:
        if stat.S_ISLNK(destination_metadata.st_mode) or not stat.S_ISDIR(
            destination_metadata.st_mode
        ):
            destination.unlink()
        else:
            def fail_walk(error: OSError) -> None:
                raise error

            for current_root, directories, files in os.walk(
                destination,
                topdown=True,
                followlinks=False,
                onerror=fail_walk,
            ):
                current = Path(current_root)
                current_metadata = current.lstat()
                require(
                    stat.S_ISDIR(current_metadata.st_mode)
                    and not stat.S_ISLNK(current_metadata.st_mode),
                    "partial fixture cleanup topology differs",
                )
                current.chmod(stat.S_IMODE(current_metadata.st_mode) | 0o700)
                for name in (*directories, *files):
                    child = current / name
                    child_metadata = child.lstat()
                    if stat.S_ISLNK(child_metadata.st_mode):
                        continue
                    child.chmod(
                        stat.S_IMODE(child_metadata.st_mode)
                        | (
                            0o700
                            if stat.S_ISDIR(child_metadata.st_mode)
                            else 0o600
                        )
                    )
            shutil.rmtree(destination)
    except OSError as error:
        raise PackagedFixtureError(
            "partial fixture cleanup failed"
        ) from error
    try:
        destination.lstat()
    except FileNotFoundError:
        return
    except OSError as error:
        raise PackagedFixtureError(
            "partial fixture cleanup postcondition is unavailable"
        ) from error
    raise PackagedFixtureError("partial fixture cleanup left its endpoint")


def copy_project_fixture(source: Path, destination: Path) -> None:
    """Copy a verified fixture into a private writable runtime tree."""

    try:
        source_metadata = source.lstat()
    except OSError as error:
        raise PackagedFixtureError("fixture source is unavailable") from error
    require(
        stat.S_ISDIR(source_metadata.st_mode)
        and not stat.S_ISLNK(source_metadata.st_mode)
        and not destination.exists(),
        "fixture copy endpoints differ",
    )
    source_state = _fixture_source_state(source)

    def ignore_root_cache(path: str, names: list[str]) -> set[str]:
        return {".godot"} if Path(path) == source and ".godot" in names else set()

    try:
        shutil.copytree(
            source,
            destination,
            symlinks=True,
            ignore=ignore_root_cache,
        )
        require(
            _fixture_source_state(source) == source_state,
            "fixture source changed during copy",
        )
        destination.chmod(0o700)
        copied_paths = {
            path.relative_to(destination).as_posix()
            for path in destination.rglob("*")
        }
        require(
            copied_paths == set(source_state),
            "copied fixture topology differs",
        )
        for path in sorted(destination.rglob("*")):
            metadata = path.lstat()
            relative = path.relative_to(destination).as_posix()
            expected = source_state.get(relative)
            require(
                expected is not None
                and not stat.S_ISLNK(metadata.st_mode),
                "copied fixture topology differs",
            )
            if stat.S_ISDIR(metadata.st_mode):
                require(
                    expected[0] == "directory",
                    "copied fixture topology differs",
                )
                path.chmod(0o700)
            else:
                require(
                    expected[0] == "file"
                    and stat.S_ISREG(metadata.st_mode)
                    and metadata.st_size == expected[2],
                    "copied fixture contains an unsupported entry",
                )
                try:
                    payload = path.read_bytes()
                except OSError as error:
                    raise PackagedFixtureError(
                        "copied fixture file is unreadable"
                    ) from error
                require(
                    len(payload) == expected[2]
                    and "sha256:" + hashlib.sha256(payload).hexdigest()
                    == expected[3],
                    "copied fixture content differs",
                )
                path.chmod(
                    0o700
                    if expected[1] & 0o111
                    else 0o600
                )
        require(
            _fixture_source_state(source) == source_state,
            "fixture source changed during copy",
        )
    except Exception as error:
        try:
            _remove_partial_fixture(destination)
        except PackagedFixtureError as cleanup_error:
            raise cleanup_error from error
        if isinstance(error, PackagedFixtureError):
            raise
        raise PackagedFixtureError(
            "fixture copy could not be normalized"
        ) from error


def _validate_setup_diff(
    *,
    path: str,
    diff: Any,
    project_root: Path,
    data_root: Path,
) -> None:
    require(isinstance(diff, str), "package-owned setup preview diff differs")
    try:
        encoded = diff.encode("utf-8")
    except UnicodeError as error:
        raise PackagedFixtureError(
            "package-owned setup preview diff is not UTF-8"
        ) from error
    header = f"--- a/{path}\n+++ b/{path}\n@@ owned content @@\n"
    payload = diff[len(header) :] if diff.startswith(header) else ""
    require(
        0 < len(encoded) <= MAX_SETUP_DIFF_BYTES
        and diff.startswith(header)
        and diff.endswith("\n")
        and "\r" not in diff
        and "\0" not in diff
        and bool(payload)
        and all(
            line.startswith(("+", "-"))
            for line in payload.splitlines()
        ),
        "package-owned setup preview diff differs",
    )
    forbidden = (
        *UNSAFE_DIFF_MARKERS,
        str(project_root),
        str(data_root),
    )
    require(
        all(marker not in diff for marker in forbidden if marker),
        "package-owned setup preview diff exposes unsafe material",
    )
    if path == ".codex/config.toml":
        require(
            '+command = "<package-launcher>"\n' in diff,
            "package-owned setup preview launcher is not redacted",
        )


def _validate_setup_changes(
    *,
    changes: Any,
    before: dict[str, tuple[str, int, int, str]],
    project_root: Path,
    data_root: Path,
) -> dict[str, dict[str, Any]]:
    require(
        isinstance(changes, list)
        and len(changes) == len(EXPECTED_CHANGED_PATHS),
        "package-owned setup preview scope differs",
    )
    records: dict[str, dict[str, Any]] = {}
    for value in changes:
        require(
            isinstance(value, dict) and set(value) == SETUP_CHANGE_FIELDS,
            "package-owned setup preview change contract differs",
        )
        record = cast(dict[str, Any], value)
        path = record.get("path")
        require(
            isinstance(path, str)
            and path in EXPECTED_CHANGED_PATHS
            and path not in records,
            "package-owned setup preview scope differs",
        )
        previous = before.get(path)
        require(
            previous is None or previous[0] == "file",
            "package-owned setup preview input type differs",
        )
        expected_action = "create" if previous is None else "update"
        expected_before_digest = (
            MISSING_DIGEST if previous is None else previous[3]
        )
        before_digest = record.get("before_digest")
        after_digest = record.get("after_digest")
        require(
            record.get("action") == expected_action
            and before_digest == expected_before_digest
            and isinstance(after_digest, str)
            and DIGEST_RE.fullmatch(after_digest) is not None,
            "package-owned setup preview change binding differs",
        )
        _validate_setup_diff(
            path=path,
            diff=record.get("diff"),
            project_root=project_root,
            data_root=data_root,
        )
        records[path] = record
    require(
        list(records) == sorted(EXPECTED_CHANGED_PATHS),
        "package-owned setup preview changes are not canonical",
    )
    return records


def _redact_config_launcher(value: str) -> str:
    redacted: list[str] = []
    for line in value.splitlines():
        key = line.split("=", 1)[0].strip() if "=" in line else ""
        if key == "command":
            redacted.append('command = "<package-launcher>"')
        elif key == "cwd":
            redacted.append('cwd = "<project-root>"')
        elif key == "env":
            redacted.append(
                'env = { GODOT_CODEX_DATA_ROOT = "<package-data-root>" }'
            )
        else:
            redacted.append(line)
    return "".join(line + "\n" for line in redacted)


def _canonical_setup_diff(path: str, before: str, after: str) -> str:
    diff = f"--- a/{path}\n+++ b/{path}\n@@ owned content @@\n"
    diff += "".join("-" + line + "\n" for line in before.splitlines())
    diff += "".join("+" + line + "\n" for line in after.splitlines())
    require(
        0 < len(diff.encode("utf-8")) <= MAX_SETUP_DIFF_BYTES,
        "canonical package-owned setup diff exceeds its bound",
    )
    return diff


def _setup_before_displays(
    *,
    project_root: Path,
    before: dict[str, tuple[str, int, int, str]],
) -> dict[str, str]:
    displays = {
        ".godot/codex/setup-receipt-v1.json": (
            "<existing owned receipt>\n"
            if ".godot/codex/setup-receipt-v1.json" in before
            else "<absent>\n"
        )
    }
    config_state = before.get(".codex/config.toml")
    if config_state is None:
        displays[".codex/config.toml"] = "<absent>\n"
        return displays
    require(
        config_state[0] == "file",
        "package-owned setup config input type differs",
    )
    try:
        config_bytes = (
            project_root / ".codex/config.toml"
        ).read_bytes()
        config_text = config_bytes.decode("utf-8")
    except (OSError, UnicodeError) as error:
        raise PackagedFixtureError(
            "package-owned setup config input is unreadable"
        ) from error
    require(
        len(config_bytes) == config_state[2]
        and _digest_bytes(config_bytes) == config_state[3],
        "package-owned setup config input changed",
    )
    displays[".codex/config.toml"] = _redact_config_launcher(
        _config_table_stanza(config_text)
    )
    return displays


def _validate_setup_change_outputs(
    *,
    changes: dict[str, dict[str, Any]],
    after: dict[str, tuple[str, int, int, str]],
    before_displays: dict[str, str],
    project_root: Path,
) -> None:
    require(
        all(
            after.get(path, ("", 0, 0, ""))[0] == "file"
            and after[path][3] == record["after_digest"]
            for path, record in changes.items()
        ),
        "package-owned setup output differs from its preview",
    )
    try:
        config_text = (
            project_root / ".codex/config.toml"
        ).read_bytes().decode("utf-8")
        receipt_text = (
            project_root
            / ".godot/codex/setup-receipt-v1.json"
        ).read_bytes().decode("utf-8")
    except (OSError, UnicodeError) as error:
        raise PackagedFixtureError(
            "package-owned setup diff output is unreadable"
        ) from error
    after_displays = {
        ".codex/config.toml": _redact_config_launcher(
            _config_table_stanza(config_text)
        ),
        ".godot/codex/setup-receipt-v1.json": receipt_text,
    }
    require(
        set(before_displays) == EXPECTED_CHANGED_PATHS
        and all(
            record["diff"]
            == _canonical_setup_diff(
                path,
                before_displays[path],
                after_displays[path],
            )
            for path, record in changes.items()
        ),
        "package-owned setup preview diff differs from applied output",
    )


def _validate_project_scope(
    *,
    before: dict[str, tuple[str, int, int, str]],
    after: dict[str, tuple[str, int, int, str]],
) -> None:
    changed_paths = {
        path for path in set(before) | set(after) if before.get(path) != after.get(path)
    }
    require(
        all(
            path in after
            and (
                path not in before
                or before[path][0] == after[path][0]
            )
            for path in changed_paths
        ),
        "package-owned setup deleted or replaced a project entry",
    )
    changed_files = {
        path for path in changed_paths if after[path][0] == "file"
    }
    changed_directories = {
        path for path in changed_paths if after[path][0] == "directory"
    }
    require(
        changed_files == EXPECTED_CHANGED_PATHS | {SETUP_LOCK_PATH}
        and changed_directories.issubset(EXPECTED_DIRECTORY_PATHS)
        and changed_paths == changed_files | changed_directories,
        "package-owned setup changed an unexpected project path",
    )
    expected_new_directory_modes = {
        ".codex": 0o755,
        ".godot": 0o700,
        ".godot/codex": 0o700,
    }
    require(
        all(
            (
                after.get(path) == before[path]
                if path in before
                else (
                    path not in after
                    or after[path]
                    == (
                        "directory",
                        expected_new_directory_modes[path],
                        0,
                        "",
                    )
                )
            )
            for path in EXPECTED_DIRECTORY_PATHS
        ),
        "package-owned setup changed an existing directory mode or created an unsafe directory",
    )


def _project_id(project_root: Path) -> str:
    try:
        canonical = project_root.resolve(strict=True)
    except OSError as error:
        raise PackagedFixtureError("copied fixture identity is unavailable") from error
    identity = str(canonical).rstrip("/").encode("utf-8")
    return "project:sha256:" + hashlib.sha256(
        b"godot-codex-project-id/v1\0" + identity
    ).hexdigest()


def _package_identity(data_root: Path) -> tuple[str, str, str, str]:
    try:
        package_root = (data_root / "current").resolve(strict=True)
    except OSError as error:
        raise PackagedFixtureError("installed package identity is unavailable") from error
    paths = {
        "operations": package_root / "bin/godot-codex",
        "sidecar": package_root / "bin/godot-codex-mcp",
        "manifest": package_root / "package-manifest.json",
    }
    states: dict[str, dict[str, Any]] = {}
    for name, path in paths.items():
        maximum = (
            MAX_PACKAGE_MANIFEST_BYTES
            if name == "manifest"
            else MAX_PACKAGE_FILE_BYTES
        )
        digest = _digest_file(path, maximum_bytes=maximum)
        try:
            mode = stat.S_IMODE(path.lstat().st_mode)
        except OSError as error:
            raise PackagedFixtureError("installed package mode is unavailable") from error
        states[name] = {"exists": True, "digest": digest, "mode": mode}
    try:
        manifest = _strict_json(
            paths["manifest"].read_bytes(),
            label="installed package manifest",
        )
    except OSError as error:
        raise PackagedFixtureError("installed package manifest is unreadable") from error
    package_version = manifest.get("package_version")
    require(
        isinstance(package_version, str)
        and package_version == package_root.name
        and 0 < len(package_version.encode("utf-8")) <= 64,
        "installed package version binding differs",
    )
    identity = _digest_bytes(
        json.dumps(
            states,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8")
    )
    launcher = data_root / "current/bin/godot-codex-mcp"
    launcher_path = _digest_bytes(str(launcher).encode("utf-8"))
    return identity, launcher_path, states["sidecar"]["digest"], package_version


def _config_table_stanza(config_text: str) -> str:
    marker = "[mcp_servers.godot_editor]"
    offset = 0
    start: int | None = None
    end = len(config_text)
    for line in config_text.splitlines(keepends=True):
        trimmed = line.rstrip("\r\n")
        if start is None:
            if trimmed == marker:
                start = offset
        elif trimmed.startswith("["):
            end = offset
            break
        offset += len(line)
    require(start is not None, "package-owned config stanza is absent")
    return config_text[start:end]


def _validate_setup_receipt(
    *,
    receipt: dict[str, Any],
    project_root: Path,
    data_root: Path,
    config_text: str,
    config_created: bool,
    config_file_mode: int,
    plan_digest: str,
    package_version: str,
) -> None:
    stanza = _config_table_stanza(config_text)
    markers = [
        line.strip().removeprefix(CONFIG_OWNERSHIP_PREFIX)
        for line in stanza.splitlines()
        if line.strip().startswith(CONFIG_OWNERSHIP_PREFIX)
    ]
    package_identity, launcher_path, launcher_file, installed_version = (
        _package_identity(data_root)
    )
    expected_project_id = _project_id(project_root)
    require(
        set(receipt) == SETUP_RECEIPT_FIELDS
        and receipt.get("schema_version") == "godot-codex-setup-receipt/1.1"
        and isinstance(receipt.get("project_id"), str)
        and PROJECT_ID_RE.fullmatch(receipt["project_id"]) is not None
        and receipt["project_id"] == expected_project_id
        and receipt.get("package_version") == package_version == installed_version
        and receipt.get("profile") == "full-beta"
        and receipt.get("guidance") == "none"
        and receipt.get("package_identity_digest") == package_identity
        and receipt.get("launcher_path_sha256") == launcher_path
        and receipt.get("launcher_file_sha256") == launcher_file
        and len(markers) == 1
        and DIGEST_RE.fullmatch(markers[0]) is not None
        and receipt.get("config_ownership_marker") == markers[0]
        and receipt.get("config_table_digest")
        == _digest_bytes(stanza.rstrip(" \t\r\n").encode("utf-8"))
        and receipt.get("config_created") is config_created
        and receipt.get("config_file_mode") == config_file_mode
        and receipt.get("agents_block_digest") is None
        and receipt.get("agents_separator") is None
        and receipt.get("agents_file_mode") is None
        and receipt.get("skill_file_digest") is None
        and receipt.get("skill_file_mode") is None
        and receipt.get("plan_digest") == plan_digest,
        "package-owned setup receipt differs",
    )


def _validate_applied_project(
    *,
    project_root: Path,
    before: dict[str, tuple[str, int, int, str]],
    data_root: Path,
    plan_digest: str,
    package_version: str,
) -> None:
    after = _project_state(project_root)
    _validate_project_scope(before=before, after=after)
    before_config = before.get(".codex/config.toml")
    config_created = before_config is None
    config_file_mode = 0o644 if before_config is None else before_config[1]
    require(
        after.get(SETUP_LOCK_PATH)
        == (
            "file",
            0o600,
            0,
            "sha256:"
            + hashlib.sha256(b"").hexdigest(),
        ),
        "package-owned setup lock state differs",
    )
    require(
        after.get(".codex/config.toml", ("", 0, 0, ""))[0:2]
        == ("file", config_file_mode)
        and after.get(
            ".godot/codex/setup-receipt-v1.json",
            ("", 0, 0, ""),
        )[0:2]
        == ("file", 0o600),
        "package-owned setup file permissions differ",
    )
    config_path = project_root / ".codex/config.toml"
    receipt_path = project_root / ".godot/codex/setup-receipt-v1.json"
    try:
        config_bytes = config_path.read_bytes()
        receipt_bytes = receipt_path.read_bytes()
        config_text = config_bytes.decode("utf-8")
        config = tomllib.loads(config_text)
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise PackagedFixtureError(
            "package-owned setup output is unreadable"
        ) from error
    require(
        0 < len(config_bytes) <= MAX_SETUP_OUTPUT_BYTES
        and 0 < len(receipt_bytes) <= MAX_SETUP_OUTPUT_BYTES,
        "package-owned setup output exceeds its bound",
    )
    servers = config.get("mcp_servers")
    server = servers.get("godot_editor") if isinstance(servers, dict) else None
    require(
        isinstance(server, dict)
        and set(server) == GODOT_EDITOR_TABLE_KEYS
        and not (set(server) & UNSAFE_SERVER_KEYS)
        and (
            not config_created
            or (
                set(config) == CONFIG_TOP_LEVEL_KEYS
                and isinstance(servers, dict)
                and set(servers) == CONFIG_SERVER_KEYS
            )
        )
        and server.get("command")
        == str(data_root / "current/bin/godot-codex")
        and server.get("args") == ["mcp", "--project-root", "."]
        and server.get("env")
        == {"GODOT_CODEX_DATA_ROOT": str(data_root)}
        and server.get("required") is True
        and server.get("startup_timeout_sec") == 10
        and server.get("tool_timeout_sec") == 60
        and server.get("default_tools_approval_mode") == "writes"
        and server.get("enabled_tools") == list(_canonical_full_beta_tools()),
        "package-owned full-beta configuration differs",
    )
    receipt = _strict_json(receipt_bytes, label="package-owned setup receipt")
    _validate_setup_receipt(
        receipt=receipt,
        project_root=project_root,
        data_root=data_root,
        config_text=config_text,
        config_created=config_created,
        config_file_mode=config_file_mode,
        plan_digest=plan_digest,
        package_version=package_version,
    )


@dataclass(frozen=True)
class PackagedFixtureProvisioner:
    operations: Path
    data_root: Path

    @classmethod
    def validate(
        cls,
        *,
        operations: Path,
        data_root: Path,
    ) -> PackagedFixtureProvisioner:
        require(
            operations.is_absolute() and data_root.is_absolute(),
            "packaged fixture paths must be absolute",
        )
        require(_plain_directory(data_root), "package data root is unavailable")
        current = data_root / "current"
        try:
            current_metadata = current.lstat()
            current_target = current.readlink()
        except OSError as error:
            raise PackagedFixtureError("installed current launcher is unavailable") from error
        require(
            stat.S_ISLNK(current_metadata.st_mode) and current_target.is_absolute(),
            "installed current launcher is not installer-owned",
        )
        versions = data_root / "versions"
        try:
            relative_target = current_target.relative_to(versions)
        except ValueError as error:
            raise PackagedFixtureError(
                "installed current launcher escapes the version root"
            ) from error
        require(
            len(relative_target.parts) == 1
            and relative_target.name not in {"", ".", ".."}
            and _plain_directory(current_target),
            "installed current launcher target differs",
        )
        expected_operations = current / "bin/godot-codex"
        expected_sidecar = current / "bin/godot-codex-mcp"
        require(
            operations == expected_operations
            and _plain_executable(expected_operations)
            and _plain_executable(expected_sidecar)
            and expected_operations.resolve(strict=True)
            == (current_target / "bin/godot-codex").resolve(strict=True)
            and expected_sidecar.resolve(strict=True)
            == (current_target / "bin/godot-codex-mcp").resolve(strict=True),
            "installed package launcher binding differs",
        )
        return cls(operations=operations, data_root=data_root)

    def activate(self) -> None:
        existing = os.environ.get("GODOT_CODEX_DATA_ROOT")
        require(
            existing in {None, str(self.data_root)},
            "package data-root environment conflicts with the fixture binding",
        )
        os.environ["GODOT_CODEX_DATA_ROOT"] = str(self.data_root)

    def configure_project(self, project_root: Path) -> None:
        require(
            project_root.is_absolute()
            and _plain_directory(project_root)
            and (project_root / "project.godot").is_file()
            and not (project_root / "project.godot").is_symlink(),
            "copied fixture project root is invalid",
        )
        environment = os.environ.copy()
        environment["GODOT_CODEX_DATA_ROOT"] = str(self.data_root)
        project_before = _project_state(project_root)
        before_displays = _setup_before_displays(
            project_root=project_root,
            before=project_before,
        )
        preview = _run(
            [
                str(self.operations),
                "setup",
                "--project-root",
                str(project_root),
                "--profile",
                "full-beta",
                "--guidance",
                "none",
                "--dry-run",
                "--json",
            ],
            environment=environment,
        )
        require(
            set(preview)
            == {
                "schema_version",
                "mode",
                "plan_digest",
                "expires_in_seconds",
                "package_version",
                "profile",
                "guidance",
                "restart_required",
                "project_trust_unchanged",
                "changes",
            }
            and preview["schema_version"] == SETUP_PLAN_SCHEMA
            and preview["mode"] == "configure"
            and preview["profile"] == "full-beta"
            and preview["guidance"] == "none"
            and preview["restart_required"] is True
            and preview["project_trust_unchanged"] is True
            and isinstance(preview["expires_in_seconds"], int)
            and 0 < preview["expires_in_seconds"] <= 600
            and isinstance(preview["package_version"], str)
            and 0 < len(preview["package_version"].encode("utf-8")) <= 64,
            "package-owned setup preview contract differs",
        )
        digest = preview["plan_digest"]
        require(
            isinstance(digest, str) and DIGEST_RE.fullmatch(digest) is not None,
            "package-owned setup preview digest differs",
        )
        changes = _validate_setup_changes(
            changes=preview["changes"],
            before=project_before,
            project_root=project_root,
            data_root=self.data_root,
        )
        require(
            _project_state(project_root) == project_before,
            "package-owned setup preview mutated the copied fixture",
        )
        result = _run(
            [
                str(self.operations),
                "setup",
                "--apply-plan",
                digest,
                "--json",
            ],
            environment=environment,
        )
        changed_paths = result.get("changed_paths")
        require(
            set(result)
            == {
                "schema_version",
                "mode",
                "plan_digest",
                "status",
                "profile",
                "guidance",
                "changed_paths",
                "restart_required",
            }
            and result["schema_version"] == SETUP_RESULT_SCHEMA
            and result["mode"] == "configure"
            and result["plan_digest"] == digest
            and result["status"] == "applied"
            and result["profile"] == "full-beta"
            and result["guidance"] == "none"
            and result["restart_required"] is True
            and isinstance(changed_paths, list)
            and all(isinstance(path, str) for path in changed_paths)
            and changed_paths == sorted(EXPECTED_CHANGED_PATHS),
            "package-owned setup apply result differs",
        )
        _validate_setup_change_outputs(
            changes=changes,
            after=_project_state(project_root),
            before_displays=before_displays,
            project_root=project_root,
        )
        _validate_applied_project(
            project_root=project_root,
            before=project_before,
            data_root=self.data_root,
            plan_digest=digest,
            package_version=preview["package_version"],
        )


_ACTIVE: PackagedFixtureProvisioner | None = None


def add_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--package-operations",
        type=Path,
        help="installed current/bin/godot-codex used to configure copied fixtures",
    )
    parser.add_argument(
        "--package-data-root",
        type=Path,
        help="private installer-owned data root inherited by package sidecars",
    )


def activate_from_arguments(arguments: argparse.Namespace) -> None:
    global _ACTIVE
    operations = cast(Path | None, getattr(arguments, "package_operations", None))
    data_root = cast(Path | None, getattr(arguments, "package_data_root", None))
    require(
        (operations is None) == (data_root is None),
        "package fixture operations and data root must be supplied together",
    )
    if operations is None or data_root is None:
        return
    provisioner = PackagedFixtureProvisioner.validate(
        operations=operations,
        data_root=data_root,
    )
    require(
        _ACTIVE is None or _ACTIVE == provisioner,
        "package fixture provisioner was rebound",
    )
    _ACTIVE = provisioner
    provisioner.activate()


def configure_project_if_requested(project_root: Path) -> None:
    if _ACTIVE is not None:
        _ACTIVE.configure_project(project_root)


def provisioning_is_active() -> bool:
    return _ACTIVE is not None


def reset_for_tests() -> None:
    """Clear process-global opt-in state in isolated unit tests."""

    global _ACTIVE
    if _ACTIVE is not None:
        os.environ.pop("GODOT_CODEX_DATA_ROOT", None)
    _ACTIVE = None
