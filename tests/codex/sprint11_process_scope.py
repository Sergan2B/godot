#!/usr/bin/env python3
"""Fail-closed lifecycle tracking for local Sprint 11 acquisitions.

Each fixed, source-bound child tree receives an unrecorded marker.  A stopped
launcher handshake lets the parent begin tracking before the requested
executable runs.  The tracker records stable PID/start-time identities across
forks, so observed children remain contained after ``setsid()`` or a later
environment replacement.  Nested bounded runners retain outer markers and add
their own independent scope.  This is lifecycle tracking for the qualified,
source-bound runner graph, not a sandbox for adversarial executables.  Every
owned clean-environment boundary must preserve the unrecorded scope markers;
clearing them is outside the qualified command graph.
"""

from __future__ import annotations

import ctypes
import os
import re
import secrets
import select
import selectors
import signal
import subprocess
import sys
import threading
import time
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any, Final, Sequence

SCOPE_ENVIRONMENT_PREFIX: Final = "GODOT_CODEX_PROCESS_SCOPE_"
SCOPE_KEY_RE: Final = re.compile(
    rf"{SCOPE_ENVIRONMENT_PREFIX}[0-9a-f]{{48}}\Z"
)
MAX_PROCESS_TABLE_BYTES: Final = 32 * 1024 * 1024
MAX_PROCESS_TABLE_STDERR_BYTES: Final = 64 * 1024
MAX_TRACKED_PROCESSES: Final = 65_536
PROCESS_TABLE_SECONDS: Final = 5.0
TRACK_INTERVAL_SECONDS: Final = 1.0
TRACKER_JOIN_SECONDS: Final = 6.0
TERMINATE_SECONDS: Final = 2.0
STOP_HANDSHAKE_SECONDS: Final = 5.0
DARWIN_EXEC_SETTLE_SECONDS: Final = 0.02
FORK_QUIESCENCE_SECONDS: Final = 0.1
FORK_QUIESCENCE_SAMPLE_SECONDS: Final = 0.01
STOPPED_LAUNCHER: Final = (
    "/bin/sh",
    "-c",
    'kill -STOP "$$" || exit 125\nexec "$@"',
    "godot-codex-process-scope",
)
DARWIN_UNIQUE_IDENTIFIER_FLAVOR: Final = 17


class ProcessScopeError(RuntimeError):
    pass


@dataclass(frozen=True)
class _ProcessRecord:
    process_id: int
    parent_id: int
    started: str
    command: bytes
    unique_id: int | None = None
    parent_unique_id: int | None = None
    id_version: int | None = None
    original_parent_id_version: int | None = None


class _DarwinUniqueIdentifierInfo(ctypes.Structure):
    _fields_ = [
        ("executable_uuid", ctypes.c_ubyte * 16),
        ("unique_id", ctypes.c_uint64),
        ("parent_unique_id", ctypes.c_uint64),
        ("id_version", ctypes.c_int32),
        ("original_parent_id_version", ctypes.c_int32),
        ("reserved_2", ctypes.c_uint64),
        ("reserved_3", ctypes.c_uint64),
    ]


@dataclass
class ProcessScope:
    marker_key: str | None
    root_pid: int | None = None
    root_identity: str | None = None
    root_unique_id: int | None = None
    root_id_version: int | None = None
    identities: dict[int, str] = field(default_factory=dict)
    unique_ids: set[int] = field(default_factory=set)
    id_versions: set[int] = field(default_factory=set)
    failures: list[BaseException] = field(default_factory=list)
    lock: threading.Lock = field(default_factory=threading.Lock)
    stop: threading.Event = field(default_factory=threading.Event)
    tracker: threading.Thread | None = None
    event_queue: Any | None = None
    watched: dict[int, str] = field(default_factory=dict)
    excluded_pids: set[int] = field(default_factory=set)


_DARWIN_LIBPROC: Any | None = None


def _require_scope_bound_locked(scope: ProcessScope) -> None:
    if (
        len(scope.identities) > MAX_TRACKED_PROCESSES
        or len(scope.unique_ids) > MAX_TRACKED_PROCESSES
        or len(scope.id_versions) > MAX_TRACKED_PROCESSES
        or len(scope.watched) > MAX_TRACKED_PROCESSES
    ):
        raise ProcessScopeError(
            "process scope exceeds its process bound"
        )


def _darwin_process_unique_ids(
    process_id: int,
) -> tuple[
    int | None,
    int | None,
    int | None,
    int | None,
]:
    """Return stable original-parent coordinates across exec and reparent."""

    global _DARWIN_LIBPROC
    if sys.platform != "darwin":
        return None, None, None, None
    if _DARWIN_LIBPROC is None:
        try:
            library = ctypes.CDLL(
                "/usr/lib/libSystem.B.dylib",
                use_errno=True,
            )
        except OSError as error:
            raise ProcessScopeError(
                "Darwin process identity API is unavailable"
            ) from error
        library.proc_pidinfo.argtypes = (
            ctypes.c_int,
            ctypes.c_int,
            ctypes.c_uint64,
            ctypes.c_void_p,
            ctypes.c_int,
        )
        library.proc_pidinfo.restype = ctypes.c_int
        _DARWIN_LIBPROC = library
    identity = _DarwinUniqueIdentifierInfo()
    copied = _DARWIN_LIBPROC.proc_pidinfo(
        process_id,
        DARWIN_UNIQUE_IDENTIFIER_FLAVOR,
        0,
        ctypes.byref(identity),
        ctypes.sizeof(identity),
    )
    if copied == 0:
        return None, None, None, None
    if (
        copied != ctypes.sizeof(identity)
        or identity.unique_id <= 0
        or identity.id_version <= 0
        or identity.original_parent_id_version < 0
    ):
        raise ProcessScopeError("Darwin process identity differs")
    return (
        identity.unique_id,
        identity.parent_unique_id,
        identity.id_version,
        identity.original_parent_id_version,
    )


def _inherited_scope_markers() -> dict[str, str]:
    markers: dict[str, str] = {}
    for key, value in os.environ.items():
        if not key.startswith(SCOPE_ENVIRONMENT_PREFIX):
            continue
        if SCOPE_KEY_RE.fullmatch(key) is None or value != "1":
            raise ProcessScopeError("inherited process scope marker is invalid")
        markers[key] = value
    return markers


def bind_environment(
    environment: Mapping[str, str],
) -> tuple[dict[str, str], ProcessScope]:
    try:
        result = dict(environment)
    except (TypeError, ValueError) as error:
        raise ProcessScopeError("process scope environment differs") from error
    if os.name != "posix":
        return result, ProcessScope(marker_key=None)
    inherited = _inherited_scope_markers()
    for key, value in tuple(result.items()):
        if not key.startswith(SCOPE_ENVIRONMENT_PREFIX):
            continue
        if SCOPE_KEY_RE.fullmatch(key) is None or value != "1":
            raise ProcessScopeError("process scope marker is invalid")
    for key, value in inherited.items():
        existing = result.get(key)
        if existing not in {None, value}:
            raise ProcessScopeError("nested process scope marker differs")
        result[key] = value
    marker_key = SCOPE_ENVIRONMENT_PREFIX + secrets.token_hex(24)
    if marker_key in result:
        raise ProcessScopeError("process scope marker collided")
    result[marker_key] = "1"
    return result, ProcessScope(marker_key=marker_key)


def scoped_argv(
    argv: Sequence[str | os.PathLike[str]],
    scope: ProcessScope,
) -> tuple[str | os.PathLike[str], ...]:
    if scope.marker_key is None:
        return tuple(argv)
    if not argv:
        raise ProcessScopeError("scoped process command is empty")
    return (*STOPPED_LAUNCHER, *argv)


def require_scope_healthy(scope: ProcessScope) -> None:
    """Fail promptly when the containment observer can no longer reconcile."""

    with scope.lock:
        failure = scope.failures[0] if scope.failures else None
    if failure is not None:
        raise ProcessScopeError("process scope tracker failed") from failure


def _terminate_process(process: subprocess.Popen[bytes]) -> None:
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    try:
        process.wait(timeout=2)
    except (OSError, subprocess.TimeoutExpired):
        pass


def activate_scope(
    scope: ProcessScope,
    process: subprocess.Popen[bytes],
) -> None:
    if scope.marker_key is None:
        return
    deadline = time.monotonic() + STOP_HANDSHAKE_SECONDS
    while True:
        try:
            waited_pid, status = os.waitpid(
                process.pid,
                os.WUNTRACED | os.WNOHANG,
            )
        except (ChildProcessError, OSError) as error:
            _terminate_process(process)
            raise ProcessScopeError(
                "scoped launcher handshake failed"
            ) from error
        if waited_pid == process.pid:
            if not os.WIFSTOPPED(status):
                process.returncode = os.waitstatus_to_exitcode(status)
                raise ProcessScopeError(
                    "scoped launcher stopped before activation"
                )
            break
        if time.monotonic() >= deadline:
            _terminate_process(process)
            raise ProcessScopeError("scoped launcher did not stop")
        time.sleep(0.005)
    scope.root_pid = process.pid
    try:
        _sample_scope(scope)
        with scope.lock:
            if (
                scope.root_identity is None
                or (
                    sys.platform == "darwin"
                    and (
                        scope.root_unique_id is None
                        or scope.root_id_version is None
                    )
                )
            ):
                raise ProcessScopeError(
                    "scoped launcher identity is unavailable"
                )
        if hasattr(select, "kqueue"):
            scope.event_queue = select.kqueue()
            _register_process_events(scope, _process_table())
        tracker = threading.Thread(
            target=_track_scope,
            args=(scope,),
            name="sprint11-process-scope",
            daemon=True,
        )
        scope.tracker = tracker
        tracker.start()
    except BaseException:
        _terminate_process(process)
        raise
    try:
        _continue_scope_root(scope, process)
    except ProcessScopeError:
        scope.stop.set()
        _terminate_process(process)
        raise


def _continue_scope_root(
    scope: ProcessScope,
    process: subprocess.Popen[bytes],
) -> None:
    with scope.lock:
        root_unique_id = scope.root_unique_id
        initial_id_versions = set(scope.id_versions)
    try:
        os.kill(process.pid, signal.SIGCONT)
    except OSError as error:
        raise ProcessScopeError(
            "scoped launcher could not continue"
        ) from error
    if sys.platform != "darwin":
        return
    if root_unique_id is None or not initial_id_versions:
        raise ProcessScopeError(
            "scoped launcher identity is unavailable"
        )
    deadline = time.monotonic() + STOP_HANDSHAKE_SECONDS
    changed_at: float | None = None
    while time.monotonic() < deadline:
        (
            unique_id,
            _parent_unique_id,
            id_version,
            _original_parent_id_version,
        ) = _darwin_process_unique_ids(process.pid)
        if unique_id is None or id_version is None:
            if changed_at is not None:
                return
            raise ProcessScopeError(
                "scoped launcher identity vanished during activation"
            )
        if unique_id != root_unique_id:
            raise ProcessScopeError(
                "scoped launcher identity changed during activation"
            )
        if id_version not in initial_id_versions:
            with scope.lock:
                scope.id_versions.add(id_version)
                _require_scope_bound_locked(scope)
            initial_id_versions.add(id_version)
            changed_at = time.monotonic()
        if (
            changed_at is not None
            and time.monotonic() - changed_at
            >= DARWIN_EXEC_SETTLE_SECONDS
        ):
            return
        time.sleep(0.0005)
    raise ProcessScopeError(
        "scoped launcher did not exec during activation"
    )


def _capture_process_table(*, include_environment: bool = True) -> bytes:
    try:
        process = subprocess.Popen(
            [
                "/bin/ps",
                "eww" if include_environment else "ww",
                "-axo",
                "pid=,ppid=,lstart=,command=",
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={
                "HOME": "/var/empty",
                "LANG": "C",
                "LC_ALL": "C",
                "PATH": "/usr/bin:/bin",
            },
            start_new_session=True,
        )
    except OSError as error:
        raise ProcessScopeError(
            "process scope table is unavailable"
        ) from error
    if process.stdout is None or process.stderr is None:
        _terminate_process(process)
        raise ProcessScopeError("process scope table pipes differ")
    selector = selectors.DefaultSelector()
    stdout_fd = process.stdout.fileno()
    stderr_fd = process.stderr.fileno()
    buffers = {stdout_fd: bytearray(), stderr_fd: bytearray()}
    limits = {
        stdout_fd: MAX_PROCESS_TABLE_BYTES,
        stderr_fd: MAX_PROCESS_TABLE_STDERR_BYTES,
    }
    for stream in (process.stdout, process.stderr):
        os.set_blocking(stream.fileno(), False)
        selector.register(stream, selectors.EVENT_READ)
    deadline = time.monotonic() + PROCESS_TABLE_SECONDS
    try:
        while selector.get_map() or process.poll() is None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ProcessScopeError("process scope table timed out")
            for key, _mask in selector.select(min(remaining, 0.05)):
                stream = key.fileobj
                try:
                    chunk = os.read(stream.fileno(), 64 * 1024)
                except BlockingIOError:
                    continue
                if not chunk:
                    selector.unregister(stream)
                    stream.close()
                    continue
                buffer = buffers[stream.fileno()]
                limit = limits[stream.fileno()]
                remaining_bytes = limit + 1 - len(buffer)
                if remaining_bytes > 0:
                    buffer.extend(chunk[:remaining_bytes])
                if len(chunk) > remaining_bytes or len(buffer) > limit:
                    raise ProcessScopeError(
                        "process scope table exceeds its byte bound"
                    )
        returncode = process.wait(
            timeout=max(deadline - time.monotonic(), 0.001)
        )
    except (OSError, subprocess.TimeoutExpired):
        _terminate_process(process)
        raise ProcessScopeError("process scope table failed")
    except ProcessScopeError:
        _terminate_process(process)
        raise
    finally:
        selector.close()
        for stream in (process.stdout, process.stderr):
            try:
                stream.close()
            except OSError:
                pass
    stderr = bytes(buffers[stderr_fd])
    if returncode != 0 or stderr:
        raise ProcessScopeError("process scope table differs")
    return bytes(buffers[stdout_fd])


def _process_table(
    *,
    include_environment: bool = True,
) -> dict[int, _ProcessRecord]:
    records: dict[int, _ProcessRecord] = {}
    for line in _capture_process_table(
        include_environment=include_environment
    ).splitlines():
        fields = line.lstrip().split(maxsplit=7)
        if len(fields) < 8:
            raise ProcessScopeError("process scope record is malformed")
        try:
            process_id = int(fields[0])
            parent_id = int(fields[1])
            started = b" ".join(fields[2:7]).decode("ascii")
        except (UnicodeDecodeError, ValueError) as error:
            raise ProcessScopeError(
                "process scope identity is malformed"
            ) from error
        if process_id <= 0 or parent_id < 0 or process_id in records:
            raise ProcessScopeError("process scope PID is unsafe")
        (
            unique_id,
            parent_unique_id,
            id_version,
            original_parent_id_version,
        ) = _darwin_process_unique_ids(process_id)
        records[process_id] = _ProcessRecord(
            process_id=process_id,
            parent_id=parent_id,
            started=started,
            command=fields[7],
            unique_id=unique_id,
            parent_unique_id=parent_unique_id,
            id_version=id_version,
            original_parent_id_version=(
                original_parent_id_version
            ),
        )
    if not records:
        raise ProcessScopeError("process scope table is empty")
    return records


def _sample_scope(
    scope: ProcessScope,
    *,
    table: dict[int, _ProcessRecord] | None = None,
) -> dict[int, _ProcessRecord]:
    if scope.marker_key is None:
        return {}
    current = _process_table() if table is None else table
    marker = f"{scope.marker_key}=1".encode("ascii")
    with scope.lock:
        if scope.root_identity is None and scope.root_pid in current:
            root = current[scope.root_pid]
            scope.root_identity = root.started
            scope.identities[root.process_id] = root.started
            scope.root_unique_id = root.unique_id
            scope.root_id_version = root.id_version
            if root.unique_id is not None:
                scope.unique_ids.add(root.unique_id)
            if root.id_version is not None:
                scope.id_versions.add(root.id_version)
        elif (
            scope.root_pid in current
            and scope.root_unique_id is not None
            and current[scope.root_pid].unique_id
            == scope.root_unique_id
            and current[scope.root_pid].id_version is not None
        ):
            scope.id_versions.add(
                current[scope.root_pid].id_version
            )

        def remember(record: _ProcessRecord) -> None:
            scope.identities[record.process_id] = record.started
            if record.unique_id is not None:
                scope.unique_ids.add(record.unique_id)
            if record.id_version is not None:
                scope.id_versions.add(record.id_version)
            _require_scope_bound_locked(scope)

        for record in current.values():
            if (
                record.unique_id is not None
                and record.unique_id in scope.unique_ids
            ):
                remember(record)
        for record in current.values():
            if (
                record.process_id not in scope.excluded_pids
                and marker in record.command
            ):
                remember(record)
        changed = True
        while changed:
            changed = False
            for record in current.values():
                parent = current.get(record.parent_id)
                if (
                    record.process_id not in scope.identities
                    and record.process_id not in scope.excluded_pids
                    and (
                        (
                            parent is not None
                            and scope.identities.get(record.parent_id)
                            == parent.started
                        )
                        or (
                            record.parent_unique_id is not None
                            and record.parent_unique_id
                            in scope.unique_ids
                        )
                        or (
                            record.original_parent_id_version
                            is not None
                            and record.original_parent_id_version
                            in scope.id_versions
                        )
                    )
                ):
                    remember(record)
                    changed = True
    return current


def _track_scope(scope: ProcessScope) -> None:
    try:
        last_sample = time.monotonic()
        while True:
            if scope.event_queue is None:
                scope.stop.wait(TRACK_INTERVAL_SECONDS)
                events: list[Any] = []
            else:
                events = scope.event_queue.control(
                    None,
                    256,
                    TRACK_INTERVAL_SECONDS,
                )
            _record_event_identities(scope, events)
            if not events:
                remaining = TRACK_INTERVAL_SECONDS - (
                    time.monotonic() - last_sample
                )
                if remaining > 0:
                    scope.stop.wait(remaining)
            table = _sample_scope(
                scope,
                table=_process_table(include_environment=False),
            )
            last_sample = time.monotonic()
            _register_process_events(scope, table)
            if scope.stop.is_set():
                break
    except BaseException as error:
        with scope.lock:
            scope.failures.append(error)
        scope.stop.set()


def _record_event_identities(
    scope: ProcessScope,
    events: list[Any],
) -> None:
    if sys.platform != "darwin":
        return
    for event in events:
        process_id = int(event.ident)
        event_flags = int(event.fflags)
        (
            unique_id,
            _parent_unique_id,
            id_version,
            _original_parent_id_version,
        ) = _darwin_process_unique_ids(process_id)
        if unique_id is None or id_version is None:
            continue
        with scope.lock:
            tracked = unique_id in scope.unique_ids
            if tracked:
                scope.id_versions.add(id_version)
                _require_scope_bound_locked(scope)
        if (
            not tracked
            or not event_flags & select.KQ_NOTE_EXEC
        ):
            continue
        stable_since = time.monotonic()
        while (
            time.monotonic() - stable_since
            < DARWIN_EXEC_SETTLE_SECONDS
        ):
            time.sleep(0.0005)
            (
                current_unique_id,
                _current_parent_unique_id,
                current_id_version,
                _current_original_parent_id_version,
            ) = _darwin_process_unique_ids(process_id)
            if (
                current_unique_id is None
                or current_id_version is None
                or current_unique_id != unique_id
            ):
                break
            if current_id_version == id_version:
                continue
            id_version = current_id_version
            stable_since = time.monotonic()
            with scope.lock:
                scope.id_versions.add(id_version)
                _require_scope_bound_locked(scope)


def _register_process_events(
    scope: ProcessScope,
    table: dict[int, _ProcessRecord],
) -> None:
    if scope.event_queue is None:
        return
    with scope.lock:
        identities = dict(scope.identities)
        watched = dict(scope.watched)
    for process_id, started in identities.items():
        record = table.get(process_id)
        if (
            record is None
            or record.started != started
            or watched.get(process_id) == started
        ):
            continue
        event = select.kevent(
            process_id,
            filter=select.KQ_FILTER_PROC,
            flags=(
                select.KQ_EV_ADD
                | select.KQ_EV_ENABLE
                | select.KQ_EV_CLEAR
            ),
            fflags=(
                select.KQ_NOTE_FORK
                | select.KQ_NOTE_EXEC
                | select.KQ_NOTE_EXIT
            ),
        )
        try:
            scope.event_queue.control([event], 0, 0)
        except (FileNotFoundError, ProcessLookupError):
            continue
        with scope.lock:
            scope.watched[process_id] = started
            _require_scope_bound_locked(scope)


def _alive_scoped_processes(
    scope: ProcessScope,
    table: dict[int, _ProcessRecord],
) -> set[int]:
    if scope.marker_key is None:
        return set()
    marker = f"{scope.marker_key}=1".encode("ascii")
    with scope.lock:
        identities = dict(scope.identities)
        unique_ids = set(scope.unique_ids)
    alive = {
        process_id
        for process_id, started in identities.items()
        if process_id in table and table[process_id].started == started
    }
    alive.update(
        record.process_id
        for record in table.values()
        if (
            record.unique_id is not None
            and record.unique_id in unique_ids
        )
    )
    alive.update(
        record.process_id
        for record in table.values()
        if marker in record.command
    )
    alive.discard(os.getpid())
    return alive


def _terminate_scope_identities(scope: ProcessScope) -> bool:
    table = _process_table()
    try:
        _sample_scope(scope, table=table)
    except ProcessScopeError:
        pass
    remaining = _alive_scoped_processes(scope, table)
    observed = bool(remaining)
    if not remaining:
        deadline = time.monotonic() + FORK_QUIESCENCE_SECONDS
        while time.monotonic() < deadline:
            time.sleep(FORK_QUIESCENCE_SAMPLE_SECONDS)
            table = _process_table()
            try:
                _sample_scope(scope, table=table)
            except ProcessScopeError:
                pass
            remaining = _alive_scoped_processes(scope, table)
            if remaining:
                observed = True
                break
    for requested_signal, duration in (
        (signal.SIGTERM, TERMINATE_SECONDS / 2),
        (signal.SIGKILL, TERMINATE_SECONDS),
    ):
        for process_id in sorted(remaining):
            if sys.platform == "darwin":
                record = table.get(process_id)
                expected_unique_id = (
                    record.unique_id if record is not None else None
                )
                current_unique_id = _darwin_process_unique_ids(
                    process_id
                )[0]
                if (
                    expected_unique_id is None
                    or current_unique_id is None
                    or current_unique_id != expected_unique_id
                ):
                    continue
            try:
                os.kill(process_id, requested_signal)
            except ProcessLookupError:
                pass
            except (PermissionError, OSError) as error:
                raise ProcessScopeError(
                    "escaped process cannot be terminated"
                ) from error
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            table = _process_table()
            remaining = _alive_scoped_processes(scope, table)
            if not remaining:
                break
            time.sleep(0.01)
        if not remaining:
            break
    if remaining:
        raise ProcessScopeError("escaped process scope did not stop")
    return observed


def close_scope(scope: ProcessScope) -> bool:
    """Terminate escaped descendants and return whether any were observed."""

    if scope.marker_key is None:
        return False
    scope.stop.set()
    observed = False
    primary_error: ProcessScopeError | None = None
    try:
        observed = _terminate_scope_identities(scope)
    except ProcessScopeError as error:
        primary_error = error
    if scope.tracker is not None:
        scope.tracker.join(timeout=TRACKER_JOIN_SECONDS)
        if scope.tracker.is_alive():
            tracker_error = ProcessScopeError(
                "process scope tracker did not stop"
            )
            if primary_error is None:
                primary_error = tracker_error
            else:
                primary_error.add_note(str(tracker_error))
    if scope.event_queue is not None:
        scope.event_queue.close()
        scope.event_queue = None
    with scope.lock:
        tracker_failure = scope.failures[0] if scope.failures else None
    if tracker_failure is not None:
        tracker_error = ProcessScopeError(
            "process scope tracker failed"
        )
        tracker_error.__cause__ = tracker_failure
        if primary_error is None:
            primary_error = tracker_error
        else:
            primary_error.add_note(str(tracker_error))
    if primary_error is not None:
        raise primary_error
    return observed
