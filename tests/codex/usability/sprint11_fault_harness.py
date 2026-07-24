#!/usr/bin/env python3
"""Marker-bound, reversible fault harness for Sprint 11 human acquisition.

All five faults operate only below a fresh mode-0700 run root.  Every mutation
has a private phase journal, an idempotent recovery path, and a pre/post state
digest.  Discovery secrets and native process/path data never leave the
disposable root.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import secrets
import signal
import socket
import stat
import sys
import tomllib
from pathlib import Path
from types import FrameType
from typing import Any, Mapping, Sequence, cast

MAX_MARKER_BYTES = 64 * 1024
MAX_CONFIG_BYTES = 256 * 1024
MAX_PACKAGE_FILE_BYTES = 256 * 1024 * 1024
MAX_PACKAGE_FILES = 512
MARKER_NAME = ".s11-usability-run-root"
STATE_MARKER_NAME = "fault-state.json"
RESET_RECEIPT_NAME = "reset-receipt.json"
CONFIG_BACKUP_NAME = "config.original"
PACKAGE_BACKUP_NAME = ".s11-usability-sidecar.backup"
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
SCENARIOS = {
    "missing_binary": ("binary_missing", "upgrade_godot_codex"),
    "stale_discovery": ("bridge_discovery_stale", "start_matching_editor"),
    "version_mismatch": (
        "bridge_version_incompatible",
        "upgrade_godot_bridge",
    ),
    "authentication_failure": (
        "bridge_authentication_failed",
        "start_matching_editor",
    ),
    "invalid_project_config": ("project_config_invalid", "repair_project_config"),
}
DISCOVERY_SCENARIOS = {
    "stale_discovery",
    "version_mismatch",
    "authentication_failure",
}
RUN_ROOT_MARKER = {
    "schema_version": "s11-usability-run-root/1.0",
    "purpose": "disposable_human_usability_faults",
    "qualification": False,
}


class FaultHarnessError(RuntimeError):
    """Raised when a fault cannot be injected or reset without ambiguity."""


def _canonical_json(value: object) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
        + "\n"
    ).encode("utf-8")


def _digest_bytes(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def _read_regular(path: Path, maximum: int) -> bytes:
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags)
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.geteuid()
            or before.st_size < 0
            or before.st_size > maximum
        ):
            raise FaultHarnessError("fault harness file is unsafe or oversized")
        chunks: list[bytes] = []
        remaining = maximum + 1
        while remaining:
            block = os.read(descriptor, min(remaining, 65536))
            if not block:
                break
            chunks.append(block)
            remaining -= len(block)
        value = b"".join(chunks)
        after = os.fstat(descriptor)
        if len(value) > maximum or (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
        ) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
        ):
            raise FaultHarnessError("fault harness file changed during read")
        return value
    finally:
        os.close(descriptor)


def _digest_file(path: Path, maximum: int = MAX_MARKER_BYTES) -> str:
    return _digest_bytes(_read_regular(path, maximum))


def _require_plain_ancestors(path: Path) -> None:
    if not path.is_absolute():
        raise FaultHarnessError("fault harness paths must be absolute")
    current = Path(path.anchor)
    for component in path.parts[1:]:
        current /= component
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            break
        if stat.S_ISLNK(metadata.st_mode) and metadata.st_uid != 0:
            raise FaultHarnessError(
                "fault harness path ancestry contains a user-owned symlink"
            )


def _require_private_directory(path: Path, label: str) -> Path:
    _require_plain_ancestors(path)
    resolved = path.resolve(strict=True)
    metadata = os.lstat(resolved)
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.geteuid()
        or stat.S_IMODE(metadata.st_mode) != 0o700
    ):
        raise FaultHarnessError(f"{label} must be an owned mode-0700 directory")
    return resolved


def _require_run_root(path: Path) -> Path:
    root = _require_private_directory(path, "run root")
    if _digest_file(root / MARKER_NAME) != _digest_bytes(
        _canonical_json(RUN_ROOT_MARKER)
    ):
        raise FaultHarnessError("run root marker is missing or differs")
    return root


def initialize_run_root(path: Path) -> None:
    """Create the only marker that authorizes a disposable run root."""

    _require_plain_ancestors(path)
    parent = path.parent.resolve(strict=True)
    if path.exists() or path.is_symlink():
        raise FaultHarnessError("run root already exists")
    os.mkdir(path, 0o700)
    try:
        if path.resolve(strict=True).parent != parent:
            raise FaultHarnessError("run root escaped its checked parent")
        _write_exclusive(path / MARKER_NAME, _canonical_json(RUN_ROOT_MARKER), 0o600)
        _fsync_directory(path)
    except BaseException:
        try:
            (path / MARKER_NAME).unlink()
        except FileNotFoundError:
            pass
        path.rmdir()
        raise


def _require_descendant(path: Path, root: Path, label: str) -> Path:
    _require_plain_ancestors(path)
    resolved = path.resolve(strict=True)
    if resolved == root or root not in resolved.parents:
        raise FaultHarnessError(f"{label} must be a strict run-root descendant")
    return resolved


def _new_state_directory(path: Path, root: Path) -> Path:
    _require_plain_ancestors(path)
    if path.exists() or path.is_symlink():
        raise FaultHarnessError("fault state directory already exists")
    if path.parent.resolve(strict=True) != root:
        raise FaultHarnessError("fault state directory must be a run-root child")
    os.mkdir(path, 0o700)
    return path.resolve(strict=True)


def _existing_state_directory(path: Path, root: Path) -> Path:
    state = _require_private_directory(path, "fault state directory")
    if state.parent != root:
        raise FaultHarnessError("fault state directory must be a run-root child")
    return state


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _write_exclusive(path: Path, value: bytes, mode: int) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags, mode)
    try:
        written = 0
        while written < len(value):
            written += os.write(descriptor, value[written:])
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _replace_file(path: Path, value: bytes, mode: int) -> None:
    temporary = path.parent / f".{path.name}.s11-tmp-{secrets.token_hex(8)}"
    _write_exclusive(temporary, value, mode)
    try:
        os.replace(temporary, path)
        _fsync_directory(path.parent)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def _write_state(state_directory: Path, state: Mapping[str, Any]) -> None:
    marker = state_directory / STATE_MARKER_NAME
    value = _canonical_json(state)
    if marker.exists():
        _replace_file(marker, value, 0o600)
    else:
        _write_exclusive(marker, value, 0o600)
        _fsync_directory(state_directory)


def _load_state(state_directory: Path) -> dict[str, Any]:
    marker = state_directory / STATE_MARKER_NAME
    if not marker.exists():
        raise FaultHarnessError("fault state marker is missing")
    try:
        value = json.loads(_read_regular(marker, MAX_MARKER_BYTES))
    except json.JSONDecodeError as error:
        raise FaultHarnessError("fault state marker is invalid") from error
    if not isinstance(value, dict):
        raise FaultHarnessError("fault state marker must be an object")
    state = cast(dict[str, Any], value)
    if (
        state.get("schema_version") != "s11-fault-state/1.0"
        or state.get("scenario") not in SCENARIOS
        or state.get("target") not in ("discovery", "package", "config")
        or state.get("qualification") is not False
        or not isinstance(state.get("phase"), str)
        or not isinstance(state.get("pre_state_sha256"), str)
        or DIGEST.fullmatch(cast(str, state["pre_state_sha256"])) is None
    ):
        raise FaultHarnessError("fault state marker contract differs")
    return state


def _load_reset_receipt(state_directory: Path) -> dict[str, Any] | None:
    path = state_directory / RESET_RECEIPT_NAME
    if not path.exists():
        return None
    try:
        value = json.loads(_read_regular(path, MAX_MARKER_BYTES))
    except json.JSONDecodeError as error:
        raise FaultHarnessError("reset receipt is invalid") from error
    if not isinstance(value, dict):
        raise FaultHarnessError("reset receipt must be an object")
    receipt = cast(dict[str, Any], value)
    required = {
        "schema_version",
        "scenario",
        "pre_state_sha256",
        "post_state_sha256",
        "exact_state_restored",
        "qualification",
        "synthetic_state_removed",
        "original_state_restored",
        "created_godot_removed",
        "token_destroyed",
    }
    allowed = required | {"setup_receipt_refreshed"}
    if (
        not required <= set(receipt) <= allowed
        or receipt.get("schema_version") != "s11-fault-reset-receipt/1.0"
        or receipt.get("scenario") not in SCENARIOS
        or receipt.get("pre_state_sha256") != receipt.get("post_state_sha256")
        or not isinstance(receipt.get("pre_state_sha256"), str)
        or DIGEST.fullmatch(cast(str, receipt["pre_state_sha256"])) is None
        or receipt.get("exact_state_restored") is not True
        or receipt.get("qualification") is not False
        or any(
            not isinstance(receipt.get(field), bool)
            for field in allowed - {
                "schema_version",
                "scenario",
                "pre_state_sha256",
                "post_state_sha256",
            }
            if field in receipt
        )
    ):
        raise FaultHarnessError("reset receipt contract differs")
    return receipt


def _finalize_reset(
    state_directory: Path,
    state: Mapping[str, Any],
    post_state_sha256: str,
    details: Mapping[str, bool],
) -> dict[str, Any]:
    if post_state_sha256 != state["pre_state_sha256"]:
        raise FaultHarnessError("post-reset state differs from the exact pre-state")
    completed_state = dict(state)
    completed_state["phase"] = "reset_complete"
    completed_state["post_state_sha256"] = post_state_sha256
    _write_state(state_directory, completed_state)
    receipt: dict[str, Any] = {
        "schema_version": "s11-fault-reset-receipt/1.0",
        "scenario": state["scenario"],
        "pre_state_sha256": state["pre_state_sha256"],
        "post_state_sha256": post_state_sha256,
        "exact_state_restored": True,
        "qualification": False,
        **details,
    }
    marker = state_directory / STATE_MARKER_NAME
    receipt_path = state_directory / RESET_RECEIPT_NAME
    existing = _load_reset_receipt(state_directory)
    if existing is None:
        _write_exclusive(receipt_path, _canonical_json(receipt), 0o600)
    elif existing != receipt:
        raise FaultHarnessError("existing reset receipt differs")
    _fsync_directory(state_directory)
    if marker.exists():
        marker.unlink()
        _fsync_directory(state_directory)
    return {
        **receipt,
        "reset_projection_sha256": _digest_bytes(_canonical_json(receipt)),
    }


def _project_root(path: Path, root: Path) -> Path:
    project = _require_descendant(path, root, "project root")
    project_file = project / "project.godot"
    if project_file.is_symlink() or not project_file.is_file():
        raise FaultHarnessError("disposable project must contain plain project.godot")
    return project


def _project_id(project_root: Path) -> str:
    identity = str(project_root).rstrip("/").encode("utf-8")
    digest = hashlib.sha256(b"godot-codex-project-id/v1\0" + identity).hexdigest()
    return f"project:sha256:{digest}"


def _bridge_process_is_live(codex: Path) -> bool:
    lock = codex / "bridge.lock"
    if not lock.exists():
        return False
    try:
        value = json.loads(_read_regular(lock, MAX_MARKER_BYTES))
    except json.JSONDecodeError as error:
        raise FaultHarnessError("existing Bridge lock is invalid") from error
    if (
        not isinstance(value, dict)
        or isinstance(value.get("pid"), bool)
        or not isinstance(value.get("pid"), int)
        or value["pid"] <= 0
    ):
        raise FaultHarnessError("existing Bridge lock PID is invalid")
    try:
        os.kill(value["pid"], 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _tree_digest(path: Path, *, maximum_files: int = 128) -> str:
    if not path.exists():
        return _digest_bytes(b"absent\n")
    root_metadata = os.lstat(path)
    if stat.S_ISLNK(root_metadata.st_mode) or not stat.S_ISDIR(root_metadata.st_mode):
        raise FaultHarnessError("fault target tree is unsafe")
    records: list[dict[str, Any]] = []
    pending = [path]
    while pending:
        directory = pending.pop()
        entries = sorted(directory.iterdir(), key=lambda item: os.fsencode(item.name))
        for entry in entries:
            relative = entry.relative_to(path).as_posix()
            metadata = os.lstat(entry)
            if stat.S_ISLNK(metadata.st_mode):
                raise FaultHarnessError("fault target tree contains a symlink")
            if stat.S_ISDIR(metadata.st_mode):
                records.append(
                    {
                        "path": relative,
                        "type": "directory",
                        "mode": stat.S_IMODE(metadata.st_mode),
                    }
                )
                pending.append(entry)
            elif stat.S_ISREG(metadata.st_mode):
                records.append(
                    {
                        "path": relative,
                        "type": "file",
                        "mode": stat.S_IMODE(metadata.st_mode),
                        "bytes": metadata.st_size,
                        "sha256": _digest_file(entry, MAX_PACKAGE_FILE_BYTES),
                    }
                )
            elif stat.S_ISSOCK(metadata.st_mode):
                records.append(
                    {
                        "path": relative,
                        "type": "socket",
                        "mode": stat.S_IMODE(metadata.st_mode),
                    }
                )
            else:
                raise FaultHarnessError("fault target tree has unsupported entry")
            if len(records) > maximum_files:
                raise FaultHarnessError("fault target tree exceeds file bound")
    records.sort(key=lambda item: cast(str, item["path"]).encode("utf-8"))
    envelope = {
        "root_mode": stat.S_IMODE(root_metadata.st_mode),
        "entries": records,
    }
    return _digest_bytes(_canonical_json(envelope))


def _discovery_state_digest(project: Path) -> str:
    godot = project / ".godot"
    if not godot.exists():
        return _digest_bytes(_canonical_json({"godot": "absent"}))
    metadata = os.lstat(godot)
    if (
        stat.S_ISLNK(metadata.st_mode)
        or not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.geteuid()
    ):
        raise FaultHarnessError("disposable .godot path is unsafe")
    siblings = []
    for entry in sorted(godot.iterdir(), key=lambda item: os.fsencode(item.name)):
        if entry.name == "codex":
            continue
        child = os.lstat(entry)
        if stat.S_ISLNK(child.st_mode):
            raise FaultHarnessError("disposable .godot sibling is a symlink")
        kind = (
            "directory"
            if stat.S_ISDIR(child.st_mode)
            else "file"
            if stat.S_ISREG(child.st_mode)
            else "other"
        )
        siblings.append(
            {
                "name_sha256": _digest_bytes(os.fsencode(entry.name)),
                "type": kind,
                "mode": stat.S_IMODE(child.st_mode),
                "size": child.st_size,
            }
        )
    value = {
        "godot": "present",
        "mode": stat.S_IMODE(metadata.st_mode),
        "siblings": siblings,
        "codex_sha256": _tree_digest(godot / "codex"),
    }
    return _digest_bytes(_canonical_json(value))


def _remove_synthetic_codex(codex: Path, state: Mapping[str, Any]) -> None:
    if not codex.exists():
        return
    allowed = {"bridge.json", "bridge.lock", "run", "session.token"}
    entries = {entry.name for entry in codex.iterdir()}
    if not entries <= allowed:
        raise FaultHarnessError("synthetic Bridge state has an unexpected entry")
    digests = state.get("synthetic_digests", {})
    if not isinstance(digests, dict):
        raise FaultHarnessError("synthetic digest journal differs")
    for name in ("bridge.json", "bridge.lock", "session.token"):
        path = codex / name
        if path.exists():
            expected = digests.get(name)
            if expected is not None and _digest_file(path, 8192) != expected:
                raise FaultHarnessError("synthetic Bridge file changed before reset")
            path.unlink()
    run = codex / "run"
    if run.exists():
        run_entries = {entry.name for entry in run.iterdir()}
        if not run_entries <= {"s11-fault.sock"}:
            raise FaultHarnessError("synthetic endpoint state changed before reset")
        endpoint = run / "s11-fault.sock"
        if endpoint.exists():
            if not stat.S_ISSOCK(os.lstat(endpoint).st_mode):
                raise FaultHarnessError("synthetic endpoint type changed")
            endpoint.unlink()
        run.rmdir()
    codex.rmdir()


class DiscoveryFault:
    """Crash-recoverable synthetic discovery fault."""

    def __init__(
        self,
        *,
        run_root: Path,
        project_root: Path,
        state_directory: Path,
        scenario: str,
    ) -> None:
        if scenario not in DISCOVERY_SCENARIOS:
            raise FaultHarnessError("unsupported discovery scenario")
        self.run_root = _require_run_root(run_root)
        self.project = _project_root(project_root, self.run_root)
        self.state_directory_input = state_directory
        self.state_directory: Path | None = None
        self.scenario = scenario
        self.listener: socket.socket | None = None

    def inject(self) -> dict[str, Any]:
        state_directory = _new_state_directory(
            self.state_directory_input, self.run_root
        )
        self.state_directory = state_directory
        godot = self.project / ".godot"
        codex = godot / "codex"
        backup = state_directory / "original-codex"
        created_godot = not godot.exists()
        had_original = codex.exists()
        if had_original and _bridge_process_is_live(codex):
            raise FaultHarnessError("matching editor is live; stop it before injection")
        pre_state = _discovery_state_digest(self.project)
        state: dict[str, Any] = {
            "schema_version": "s11-fault-state/1.0",
            "scenario": self.scenario,
            "target": "discovery",
            "phase": "prepared",
            "pre_state_sha256": pre_state,
            "created_godot": created_godot,
            "had_original": had_original,
            "synthetic_digests": {},
            "qualification": False,
        }
        _write_state(state_directory, state)
        try:
            if created_godot:
                os.mkdir(godot, 0o700)
            if had_original:
                os.rename(codex, backup)
            state["phase"] = "backup_moved"
            _write_state(state_directory, state)
            os.mkdir(codex, 0o700)
            run = codex / "run"
            os.mkdir(run, 0o700)
            endpoint = run / "s11-fault.sock"
            if len(os.fsencode(endpoint)) >= 100:
                raise FaultHarnessError("run root is too long for a Unix socket")
            self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self.listener.bind(str(endpoint))
            os.chmod(endpoint, 0o600)
            self.listener.listen(1)
            session = f"editor:{secrets.token_hex(16)}"
            pid = (
                2_147_483_647
                if self.scenario == "stale_discovery"
                else os.getpid()
            )
            versions = ["2.0"] if self.scenario == "version_mismatch" else ["1.8"]
            token_length = 31 if self.scenario == "authentication_failure" else 32
            discovery = {
                "discovery_schema": 1,
                "created_at": "content-minimized",
                "transport": "uds",
                "endpoint": ".godot/codex/run/s11-fault.sock",
                "token_file": ".godot/codex/session.token",
                "project_id": _project_id(self.project),
                "editor_session_id": session,
                "pid": pid,
                "protocol_versions": versions,
            }
            _write_exclusive(codex / "bridge.json", _canonical_json(discovery), 0o600)
            _write_exclusive(
                codex / "session.token", secrets.token_bytes(token_length), 0o600
            )
            _write_exclusive(
                codex / "bridge.lock",
                _canonical_json({"editor_session_id": session, "pid": pid}),
                0o600,
            )
            state["synthetic_digests"] = {
                name: _digest_file(codex / name, 8192)
                for name in ("bridge.json", "session.token", "bridge.lock")
            }
            state["phase"] = "active"
            _write_state(state_directory, state)
            return _ready_projection(state)
        except BaseException:
            if self.listener is not None:
                self.listener.close()
                self.listener = None
            try:
                recover_fault(
                    run_root=self.run_root,
                    state_directory=state_directory,
                    project_root=self.project,
                    package_root=None,
                )
            except BaseException:
                pass
            raise

    def reset(self) -> dict[str, Any]:
        if self.state_directory is None:
            raise FaultHarnessError("discovery fault was not injected")
        if self.listener is not None:
            self.listener.close()
            self.listener = None
        return recover_fault(
            run_root=self.run_root,
            state_directory=self.state_directory,
            project_root=self.project,
            package_root=None,
        )


def _recover_discovery(
    project: Path,
    state_directory: Path,
    state: dict[str, Any],
) -> dict[str, Any]:
    if state.get("target") != "discovery":
        raise FaultHarnessError("fault target is not discovery")
    codex = project / ".godot" / "codex"
    godot = project / ".godot"
    backup = state_directory / "original-codex"
    state["phase"] = "resetting"
    _write_state(state_directory, state)
    had_original = state.get("had_original") is True
    created_godot = state.get("created_godot") is True
    if backup.exists():
        _remove_synthetic_codex(codex, state)
        os.rename(backup, codex)
        _fsync_directory(godot)
    elif had_original:
        if _discovery_state_digest(project) != state["pre_state_sha256"]:
            raise FaultHarnessError("original Bridge backup is missing")
    else:
        _remove_synthetic_codex(codex, state)
    if created_godot and godot.exists():
        try:
            godot.rmdir()
        except OSError as error:
            raise FaultHarnessError(
                "harness-created .godot is not empty after reset"
            ) from error
    post_state = _discovery_state_digest(project)
    return _finalize_reset(
        state_directory,
        state,
        post_state,
        {
            "synthetic_state_removed": True,
            "original_state_restored": had_original,
            "created_godot_removed": created_godot,
            "token_destroyed": True,
        },
    )


def _safe_package_path(value: str) -> bool:
    path = Path(value)
    return (
        bool(value)
        and not path.is_absolute()
        and all(part not in ("", ".", "..") for part in path.parts)
    )


def _package_snapshot(package_root: Path) -> dict[str, Any]:
    checksums_bytes = _read_regular(
        package_root / "checksums.sha256", MAX_MARKER_BYTES
    )
    try:
        text = checksums_bytes.decode("utf-8")
    except UnicodeError as error:
        raise FaultHarnessError("package checksums are not UTF-8") from error
    if not text.endswith("\n"):
        raise FaultHarnessError("package checksums are not canonical")
    expected: dict[str, str] = {}
    for line in text.splitlines():
        if len(line) < 67 or line[64:66] != "  ":
            raise FaultHarnessError("package checksum line is invalid")
        digest, relative = line[:64], line[66:]
        if (
            re.fullmatch(r"[0-9a-f]{64}", digest) is None
            or not _safe_package_path(relative)
            or relative in expected
        ):
            raise FaultHarnessError("package checksum entry is invalid")
        expected[relative] = digest
    if (
        len(expected) > MAX_PACKAGE_FILES
        or "bin/godot-codex" not in expected
        or "bin/godot-codex-mcp" not in expected
    ):
        raise FaultHarnessError("package checksum inventory differs")
    actual_files: set[str] = set()
    pending = [package_root]
    while pending:
        directory = pending.pop()
        for entry in directory.iterdir():
            metadata = os.lstat(entry)
            if stat.S_ISLNK(metadata.st_mode):
                raise FaultHarnessError("installed package contains a symlink")
            if stat.S_ISDIR(metadata.st_mode):
                pending.append(entry)
            elif stat.S_ISREG(metadata.st_mode):
                actual_files.add(entry.relative_to(package_root).as_posix())
            else:
                raise FaultHarnessError("installed package entry type differs")
    expected_actual = set(expected) | {"checksums.sha256", ".godot-codex-owned"}
    if actual_files != expected_actual:
        raise FaultHarnessError("installed package file inventory differs")
    manifest = _read_regular(
        package_root / "package-manifest.json", MAX_PACKAGE_FILE_BYTES
    )
    owner = _read_regular(package_root / ".godot-codex-owned", MAX_MARKER_BYTES)
    expected_owner = hashlib.sha256(manifest).hexdigest().encode("ascii") + b"\n"
    if owner != expected_owner:
        raise FaultHarnessError("installed package ownership marker differs")
    records = []
    for relative, expected_digest in sorted(
        expected.items(), key=lambda item: item[0].encode("utf-8")
    ):
        path = package_root / relative
        if path.is_symlink() or not path.is_file():
            raise FaultHarnessError("package inventory contains an unsafe file")
        metadata = os.lstat(path)
        observed = _digest_file(path, MAX_PACKAGE_FILE_BYTES)
        if observed != f"sha256:{expected_digest}":
            raise FaultHarnessError("package file digest differs")
        records.append(
            {
                "path": relative,
                "sha256": observed,
                "bytes": metadata.st_size,
                "mode": stat.S_IMODE(metadata.st_mode),
            }
        )
    for relative, value in (
        ("checksums.sha256", checksums_bytes),
        (".godot-codex-owned", owner),
    ):
        metadata = os.lstat(package_root / relative)
        records.append(
            {
                "path": relative,
                "sha256": _digest_bytes(value),
                "bytes": metadata.st_size,
                "mode": stat.S_IMODE(metadata.st_mode),
            }
        )
    records.sort(key=lambda item: cast(str, item["path"]).encode("utf-8"))
    inventory = _digest_bytes(_canonical_json(records))
    sidecar = package_root / "bin/godot-codex-mcp"
    sidecar_metadata = os.lstat(sidecar)
    return {
        "tree_sha256": inventory,
        "sidecar_sha256": _digest_file(sidecar, MAX_PACKAGE_FILE_BYTES),
        "sidecar_mode": stat.S_IMODE(sidecar_metadata.st_mode),
        "checksums_sha256": _digest_bytes(checksums_bytes),
    }


def inject_missing_binary(
    *,
    run_root: Path,
    package_root: Path,
    state_directory: Path,
) -> dict[str, Any]:
    root = _require_run_root(run_root)
    package = _require_descendant(package_root, root, "package root")
    state_dir = _new_state_directory(state_directory, root)
    snapshot = _package_snapshot(package)
    sidecar = package / "bin/godot-codex-mcp"
    backup = package / "bin" / PACKAGE_BACKUP_NAME
    if backup.exists() or backup.is_symlink():
        raise FaultHarnessError("package fault backup already exists")
    state: dict[str, Any] = {
        "schema_version": "s11-fault-state/1.0",
        "scenario": "missing_binary",
        "target": "package",
        "phase": "prepared",
        "pre_state_sha256": snapshot["tree_sha256"],
        "sidecar_sha256": snapshot["sidecar_sha256"],
        "sidecar_mode": snapshot["sidecar_mode"],
        "checksums_sha256": snapshot["checksums_sha256"],
        "qualification": False,
    }
    _write_state(state_dir, state)
    os.rename(sidecar, backup)
    _fsync_directory(sidecar.parent)
    state["phase"] = "active"
    _write_state(state_dir, state)
    return _ready_projection(state)


def _recover_package(
    package: Path,
    state_directory: Path,
    state: dict[str, Any],
) -> dict[str, Any]:
    if state.get("target") != "package" or state.get("scenario") != "missing_binary":
        raise FaultHarnessError("fault target is not the missing binary")
    sidecar = package / "bin/godot-codex-mcp"
    backup = package / "bin" / PACKAGE_BACKUP_NAME
    state["phase"] = "resetting"
    _write_state(state_directory, state)
    sidecar_exists = sidecar.exists() or sidecar.is_symlink()
    backup_exists = backup.exists() or backup.is_symlink()
    if backup_exists and not sidecar_exists:
        if (
            backup.is_symlink()
            or not backup.is_file()
            or _digest_file(backup, MAX_PACKAGE_FILE_BYTES)
            != state.get("sidecar_sha256")
        ):
            raise FaultHarnessError("package fault backup differs")
        os.rename(backup, sidecar)
        os.chmod(sidecar, cast(int, state["sidecar_mode"]))
        _fsync_directory(sidecar.parent)
    elif sidecar_exists and not backup_exists:
        if (
            sidecar.is_symlink()
            or not sidecar.is_file()
            or _digest_file(sidecar, MAX_PACKAGE_FILE_BYTES)
            != state.get("sidecar_sha256")
        ):
            raise FaultHarnessError("restored sidecar differs")
    else:
        raise FaultHarnessError("package fault target/backup state is ambiguous")
    snapshot = _package_snapshot(package)
    if snapshot["checksums_sha256"] != state.get("checksums_sha256"):
        raise FaultHarnessError("package checksum manifest changed")
    return _finalize_reset(
        state_directory,
        state,
        cast(str, snapshot["tree_sha256"]),
        {
            "synthetic_state_removed": True,
            "original_state_restored": True,
            "created_godot_removed": False,
            "token_destroyed": True,
        },
    )


def _config_stanza(text: str) -> tuple[int, int, str]:
    marker = "[mcp_servers.godot_editor]"
    offset = 0
    start: int | None = None
    end = len(text)
    for line in text.splitlines(keepends=True):
        trimmed = line.rstrip("\r\n")
        if start is None:
            if trimmed == marker:
                start = offset
        elif trimmed.startswith("["):
            end = offset
            break
        offset += len(line)
    if start is None:
        raise FaultHarnessError("owned godot_editor stanza is missing")
    return start, end, text[start:end]


def _config_stanza_digest(stanza: str) -> str:
    return _digest_bytes(stanza.rstrip(" \t\r\n").encode("utf-8"))


def _config_snapshot(project: Path) -> tuple[dict[str, Any], bytes, dict[str, Any]]:
    config = project / ".codex/config.toml"
    receipt_path = project / ".godot/codex/setup-receipt-v1.json"
    config_bytes = _read_regular(config, MAX_CONFIG_BYTES)
    receipt_bytes = _read_regular(receipt_path, MAX_MARKER_BYTES)
    try:
        text = config_bytes.decode("utf-8")
        document = tomllib.loads(text)
        receipt_value = json.loads(receipt_bytes)
    except (UnicodeError, tomllib.TOMLDecodeError, json.JSONDecodeError) as error:
        raise FaultHarnessError("config or setup receipt is invalid") from error
    if not isinstance(receipt_value, dict):
        raise FaultHarnessError("setup receipt must be an object")
    receipt = cast(dict[str, Any], receipt_value)
    table = (
        document.get("mcp_servers", {}).get("godot_editor")
        if isinstance(document.get("mcp_servers"), dict)
        else None
    )
    if not isinstance(table, dict) or table.get("cwd") != "..":
        raise FaultHarnessError("owned config is not in the healthy pre-fault state")
    _, _, stanza = _config_stanza(text)
    metadata = os.lstat(config)
    expected_receipt_fields = {
        "schema_version",
        "project_id",
        "package_version",
        "profile",
        "guidance",
        "launcher_path_sha256",
        "launcher_file_sha256",
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
    if (
        set(receipt) != expected_receipt_fields
        or receipt.get("schema_version") != "godot-codex-setup-receipt/1.0"
        or receipt.get("project_id") != _project_id(project)
        or receipt.get("config_table_digest") != _config_stanza_digest(stanza)
        or receipt.get("config_file_mode") != stat.S_IMODE(metadata.st_mode)
        or any(
            not isinstance(receipt.get(field), str)
            or DIGEST.fullmatch(cast(str, receipt[field])) is None
            for field in (
                "launcher_path_sha256",
                "launcher_file_sha256",
                "config_table_digest",
                "plan_digest",
            )
        )
    ):
        raise FaultHarnessError("setup receipt does not prove config ownership")
    snapshot = {
        "config_sha256": _digest_bytes(config_bytes),
        "config_mode": stat.S_IMODE(metadata.st_mode),
        "receipt_sha256": _digest_bytes(receipt_bytes),
    }
    snapshot["tree_sha256"] = _digest_bytes(
        _canonical_json(
            {
                "config_sha256": snapshot["config_sha256"],
                "config_mode": snapshot["config_mode"],
            }
        )
    )
    return snapshot, config_bytes, receipt


def inject_invalid_config(
    *,
    run_root: Path,
    project_root: Path,
    state_directory: Path,
) -> dict[str, Any]:
    root = _require_run_root(run_root)
    project = _project_root(project_root, root)
    state_dir = _new_state_directory(state_directory, root)
    snapshot, config_bytes, _receipt = _config_snapshot(project)
    text = config_bytes.decode("utf-8")
    start, end, stanza = _config_stanza(text)
    healthy = 'cwd = ".."'
    if stanza.count(healthy) != 1:
        raise FaultHarnessError("owned cwd field is not uniquely replaceable")
    drifted_stanza = stanza.replace(healthy, 'cwd = "."', 1)
    drifted_text = text[:start] + drifted_stanza + text[end:]
    tomllib.loads(drifted_text)
    drifted_bytes = drifted_text.encode("utf-8")
    state: dict[str, Any] = {
        "schema_version": "s11-fault-state/1.0",
        "scenario": "invalid_project_config",
        "target": "config",
        "phase": "prepared",
        "pre_state_sha256": snapshot["tree_sha256"],
        "config_sha256": snapshot["config_sha256"],
        "config_mode": snapshot["config_mode"],
        "receipt_sha256": snapshot["receipt_sha256"],
        "drifted_config_sha256": _digest_bytes(drifted_bytes),
        "qualification": False,
    }
    _write_state(state_dir, state)
    _write_exclusive(state_dir / CONFIG_BACKUP_NAME, config_bytes, 0o600)
    _replace_file(
        project / ".codex/config.toml",
        drifted_bytes,
        cast(int, snapshot["config_mode"]),
    )
    state["phase"] = "active"
    _write_state(state_dir, state)
    return _ready_projection(state)


def _recover_config(
    project: Path,
    state_directory: Path,
    state: dict[str, Any],
) -> dict[str, Any]:
    if (
        state.get("target") != "config"
        or state.get("scenario") != "invalid_project_config"
    ):
        raise FaultHarnessError("fault target is not project config")
    config = project / ".codex/config.toml"
    backup = state_directory / CONFIG_BACKUP_NAME
    state["phase"] = "resetting"
    _write_state(state_directory, state)
    observed = _digest_file(config, MAX_CONFIG_BYTES)
    if observed == state.get("drifted_config_sha256"):
        backup_bytes = _read_regular(backup, MAX_CONFIG_BYTES)
        if _digest_bytes(backup_bytes) != state.get("config_sha256"):
            raise FaultHarnessError("config fault backup differs")
        _replace_file(config, backup_bytes, cast(int, state["config_mode"]))
    elif observed != state.get("config_sha256"):
        raise FaultHarnessError("config changed outside repair/reset contract")
    metadata = os.lstat(config)
    if (
        stat.S_IMODE(metadata.st_mode) != state.get("config_mode")
        or _digest_file(config, MAX_CONFIG_BYTES) != state.get("config_sha256")
    ):
        raise FaultHarnessError("config did not restore exactly")
    post_snapshot, _config_bytes, _receipt = _config_snapshot(project)
    if (
        post_snapshot["config_sha256"] != state["config_sha256"]
        or post_snapshot["config_mode"] != state["config_mode"]
    ):
        raise FaultHarnessError("repaired config ownership projection differs")
    current_receipt = cast(str, post_snapshot["receipt_sha256"])
    # A successful product repair may refresh the private receipt.  The exact
    # mutation target for this fault is the owned config bytes/mode; receipt
    # provenance is validated and bound separately in the reset projection.
    post_state = cast(str, post_snapshot["tree_sha256"])
    try:
        backup.unlink()
    except FileNotFoundError:
        pass
    return _finalize_reset(
        state_directory,
        state,
        post_state,
        {
            "synthetic_state_removed": True,
            "original_state_restored": True,
            "created_godot_removed": False,
            "token_destroyed": True,
            "setup_receipt_refreshed": current_receipt != state["receipt_sha256"],
        },
    )


def _ready_projection(state: Mapping[str, Any]) -> dict[str, Any]:
    scenario = cast(str, state["scenario"])
    projection = {
        "schema_version": "s11-fault-ready/1.0",
        "scenario": scenario,
        "expected_diagnostic_code": SCENARIOS[scenario][0],
        "expected_remediation_id": SCENARIOS[scenario][1],
        "pre_state_sha256": state["pre_state_sha256"],
        "state_projection_sha256": _digest_bytes(_canonical_json(state)),
        "qualification": False,
    }
    return projection


def recover_fault(
    *,
    run_root: Path,
    state_directory: Path,
    project_root: Path | None,
    package_root: Path | None,
) -> dict[str, Any]:
    root = _require_run_root(run_root)
    state_dir = _existing_state_directory(state_directory, root)
    completed = _load_reset_receipt(state_dir)
    if completed is not None:
        marker = state_dir / STATE_MARKER_NAME
        if marker.exists():
            pending = _load_state(state_dir)
            if (
                pending["phase"] != "reset_complete"
                or pending["scenario"] != completed["scenario"]
                or pending["pre_state_sha256"] != completed["pre_state_sha256"]
            ):
                raise FaultHarnessError("reset receipt and pending journal differ")
            marker.unlink()
            _fsync_directory(state_dir)
        return {
            **completed,
            "reset_projection_sha256": _digest_bytes(
                _canonical_json(completed)
            ),
        }
    state = _load_state(state_dir)
    target = state["target"]
    if target == "package":
        if package_root is None or project_root is not None:
            raise FaultHarnessError("package recovery requires only --package-root")
        package = _require_descendant(package_root, root, "package root")
        return _recover_package(package, state_dir, state)
    if project_root is None or package_root is not None:
        raise FaultHarnessError("project recovery requires only --project-root")
    project = _project_root(project_root, root)
    if target == "discovery":
        return _recover_discovery(project, state_dir, state)
    if target == "config":
        return _recover_config(project, state_dir, state)
    raise FaultHarnessError("fault target differs")


def _serve(options: argparse.Namespace) -> int:
    fault = DiscoveryFault(
        run_root=options.run_root,
        project_root=options.project_root,
        state_directory=options.state_directory,
        scenario=options.scenario,
    )
    ready = fault.inject()
    print(json.dumps(ready, sort_keys=True, separators=(",", ":")), flush=True)
    stopping = False

    def stop(_signal: int, _frame: FrameType | None) -> None:
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    while not stopping:
        signal.pause()
    reset = fault.reset()
    print(json.dumps(reset, sort_keys=True, separators=(",", ":")), flush=True)
    return 0


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="marker-bound Sprint 11 five-fault usability harness"
    )
    commands = parser.add_subparsers(dest="command", required=True)
    initialize = commands.add_parser(
        "init-run-root", help="mark one new private directory as disposable"
    )
    initialize.add_argument("--run-root", type=Path, required=True)

    serve = commands.add_parser(
        "serve", help="hold one synthetic discovery fault until interrupted"
    )
    serve.add_argument("--run-root", type=Path, required=True)
    serve.add_argument("--project-root", type=Path, required=True)
    serve.add_argument("--state-directory", type=Path, required=True)
    serve.add_argument(
        "--scenario", choices=sorted(DISCOVERY_SCENARIOS), required=True
    )

    inject = commands.add_parser(
        "inject", help="inject one persistent package or config fault"
    )
    inject.add_argument("--run-root", type=Path, required=True)
    inject.add_argument("--state-directory", type=Path, required=True)
    inject.add_argument(
        "--scenario",
        choices=("missing_binary", "invalid_project_config"),
        required=True,
    )
    inject.add_argument("--project-root", type=Path)
    inject.add_argument("--package-root", type=Path)

    recover = commands.add_parser(
        "recover", help="idempotently reset a journaled fault after exit/crash"
    )
    recover.add_argument("--run-root", type=Path, required=True)
    recover.add_argument("--state-directory", type=Path, required=True)
    recover.add_argument("--project-root", type=Path)
    recover.add_argument("--package-root", type=Path)
    return parser


def main(arguments: Sequence[str] | None = None) -> int:
    options = _parser().parse_args(arguments)
    try:
        if options.command == "init-run-root":
            initialize_run_root(options.run_root)
            result = {
                "schema_version": "s11-usability-run-root-ready/1.0",
                "qualification": False,
            }
        elif options.command == "serve":
            return _serve(options)
        elif options.command == "inject":
            if options.scenario == "missing_binary":
                if options.package_root is None or options.project_root is not None:
                    raise FaultHarnessError(
                        "missing_binary requires only --package-root"
                    )
                result = inject_missing_binary(
                    run_root=options.run_root,
                    package_root=options.package_root,
                    state_directory=options.state_directory,
                )
            else:
                if options.project_root is None or options.package_root is not None:
                    raise FaultHarnessError(
                        "invalid_project_config requires only --project-root"
                    )
                result = inject_invalid_config(
                    run_root=options.run_root,
                    project_root=options.project_root,
                    state_directory=options.state_directory,
                )
        elif options.command == "recover":
            result = recover_fault(
                run_root=options.run_root,
                state_directory=options.state_directory,
                project_root=options.project_root,
                package_root=options.package_root,
            )
        else:
            raise FaultHarnessError("unknown command")
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 0
    except (FaultHarnessError, OSError, ValueError) as error:
        print(f"Sprint 11 fault harness failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
