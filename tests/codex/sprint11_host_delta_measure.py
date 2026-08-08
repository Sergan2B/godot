#!/usr/bin/env python3
"""Measure a rolling Codex host coordinate without changing package state.

The measurement is intentionally independent from package installation and
project setup.  It verifies the three official macOS surfaces, extracts only
the app-server and feature semantics needed by the Godot MCP workflow, and
publishes a content-free, path-free receipt as one atomic file.
"""

from __future__ import annotations

import dataclasses
import errno
import hashlib
import json
import os
import platform
import plistlib
import re
import signal
import stat
import subprocess
import tempfile
import time
import uuid
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any, Final

try:
    from tests.codex import sprint11_host_delta as host_delta
    from tests.codex.sprint11_host_provenance import (
        HostProvenanceError,
        _canonical_nonsymlink_path,
        _safe_file_digest,
        command_environment,
        measure_code_signature,
        stable_tree_digest,
    )
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_host_delta as host_delta
    from sprint11_host_provenance import (
        HostProvenanceError,
        _canonical_nonsymlink_path,
        _safe_file_digest,
        command_environment,
        measure_code_signature,
        stable_tree_digest,
    )


MEASUREMENT_SCHEMA: Final = "s11-host-delta-measurement/1.0"
FEATURE_SCHEMA: Final = "s11-codex-feature-projection/1.0"
FEATURE_SET_SCHEMA: Final = "s11-codex-feature-projection-set/1.0"
APP_SERVER_SCHEMA: Final = "s11-codex-app-server-projection/1.0"
APP_SERVER_SET_SCHEMA: Final = "s11-codex-app-server-projection-set/1.0"
TARGET: Final = {"os": "macos", "architecture": "arm64"}
MAX_JSON_BYTES: Final = 2 * 1024 * 1024
MAX_COMMAND_BYTES: Final = 256 * 1024
MAX_VERSION_BYTES: Final = 16 * 1024
MAX_DURATION_MS: Final = 10 * 60 * 1000
VERSION_RE: Final = re.compile(r"[0-9][0-9A-Za-z.+-]{0,63}\Z")
MATURITY_RE: Final = re.compile(r"[a-z][a-z -]{0,31}\Z")
FEATURE_LINE_RE: Final = re.compile(
    r"(?P<name>[a-z][a-z0-9_]*)\s{2,}(?P<maturity>[a-z][a-z -]*?)\s{2,}(?P<enabled>true|false)\s*\Z"
)

RELEVANT_FEATURES: Final = (
    "auth_elicitation",
    "exec_permission_approvals",
    "guardian_approval",
    "request_permissions_tool",
    "tool_call_mcp_elicitation",
)
APP_SERVER_FILES: Final = (
    "v1/InitializeParams.json",
    "v1/InitializeResponse.json",
    "v2/ThreadStartParams.json",
    "v2/ListMcpServerStatusParams.json",
    "v2/ListMcpServerStatusResponse.json",
    "v2/McpServerToolCallParams.json",
    "v2/McpServerToolCallResponse.json",
    "McpServerElicitationRequestParams.json",
    "McpServerElicitationRequestResponse.json",
)
APP_SERVER_TITLES: Final = {
    name: Path(name).stem for name in APP_SERVER_FILES
}
APP_SERVER_REQUIRED: Final = {
    "v1/InitializeParams.json": {"clientInfo"},
    "v1/InitializeResponse.json": {
        "codexHome",
        "platformFamily",
        "platformOs",
        "userAgent",
    },
    "v2/ThreadStartParams.json": set(),
    "v2/ListMcpServerStatusParams.json": set(),
    "v2/ListMcpServerStatusResponse.json": {"data"},
    "v2/McpServerToolCallParams.json": {"server", "threadId", "tool"},
    "v2/McpServerToolCallResponse.json": {"content"},
    "McpServerElicitationRequestParams.json": {"serverName", "threadId"},
    "McpServerElicitationRequestResponse.json": {"action"},
}
APP_SERVER_PROPERTIES: Final = {
    "v1/InitializeParams.json": {"capabilities", "clientInfo"},
    "v1/InitializeResponse.json": {
        "codexHome",
        "platformFamily",
        "platformOs",
        "userAgent",
    },
    "v2/ThreadStartParams.json": {"approvalPolicy", "cwd"},
    "v2/ListMcpServerStatusParams.json": {"threadId"},
    "v2/ListMcpServerStatusResponse.json": {"data"},
    "v2/McpServerToolCallParams.json": {"arguments", "server", "threadId", "tool"},
    "v2/McpServerToolCallResponse.json": {"content", "isError", "structuredContent"},
    "McpServerElicitationRequestParams.json": {"serverName", "threadId"},
    "McpServerElicitationRequestResponse.json": {"action", "content"},
}
APP_SERVER_METHODS: Final = (
    "initialize",
    "thread/start",
    "mcpServerStatus/list",
    "mcpServer/tool/call",
    "openai/form",
)
REQUIRED_ELICITATION_ACTIONS: Final = ("accept", "decline", "cancel")
SCHEMA_KEYS: Final = frozenset(
    {
        "$ref",
        "additionalProperties",
        "allOf",
        "anyOf",
        "const",
        "definitions",
        "enum",
        "items",
        "oneOf",
        "properties",
        "required",
        "title",
        "type",
    }
)


class HostMeasurementError(RuntimeError):
    """A host coordinate could not produce bounded qualifying evidence."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise HostMeasurementError(message)


@dataclasses.dataclass(frozen=True)
class MeasureOptions:
    app_bundle: Path
    app_executable: Path
    app_client: Path
    cli_client: Path
    vscode_bundle: Path
    vscode_executable: Path
    extension_root: Path
    extension_package_json: Path
    ide_client: Path
    embedded_profile: Path
    embedded_matrix: Path
    output: Path
    timeout_seconds: float = 30.0


@dataclasses.dataclass(frozen=True)
class CommandResult:
    operation: str
    returncode: int
    duration_ms: int
    stdout: bytes
    stderr: bytes


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
        raise HostMeasurementError("host measurement is not canonical JSON") from error


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def safe_file_digest(path: Path, *, label: str) -> tuple[str, int]:
    try:
        return _safe_file_digest(path, label=label)
    except HostProvenanceError as error:
        raise HostMeasurementError(str(error)) from error


def _canonical_path(path: Path, *, kind: str) -> Path:
    try:
        return _canonical_nonsymlink_path(path, kind=kind)
    except HostProvenanceError as error:
        raise HostMeasurementError(str(error)) from error


def _read_regular_file(path: Path, *, label: str, maximum: int) -> bytes:
    digest_before, size = safe_file_digest(path, label=label)
    require(size <= maximum, f"{label} exceeds its byte bound")
    try:
        value = path.read_bytes()
    except OSError as error:
        raise HostMeasurementError(f"{label} cannot be read") from error
    require(
        len(value) == size and sha256_bytes(value) == digest_before,
        f"{label} changed while reading",
    )
    digest_after, size_after = safe_file_digest(path, label=label)
    require(
        (digest_after, size_after) == (digest_before, size),
        f"{label} changed after reading",
    )
    return value


def _strict_json(data: bytes, *, label: str) -> dict[str, Any]:
    try:
        return host_delta.strict_json_bytes(
            data,
            maximum_bytes=MAX_JSON_BYTES,
            label=label,
        )
    except host_delta.HostDeltaError as error:
        raise HostMeasurementError(str(error)) from error


def _stop_process_group(process: subprocess.Popen[bytes]) -> None:
    for requested_signal in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(process.pid, requested_signal)
        except ProcessLookupError:
            return
        except OSError as error:
            raise HostMeasurementError("host command process group cannot be stopped") from error
        try:
            process.wait(timeout=1.0)
        except subprocess.TimeoutExpired:
            continue
        return
    raise HostMeasurementError("host command process group did not stop")


def run_bounded_command(
    argv: tuple[str, ...],
    timeout: float,
    operation: str,
) -> CommandResult:
    require(argv and timeout > 0, "host command coordinate differs")
    require(
        re.fullmatch(r"[a-z][a-z0-9_]{0,63}", operation) is not None,
        "host command operation differs",
    )
    process: subprocess.Popen[bytes] | None = None
    started = time.monotonic()
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
                raise HostMeasurementError("host command timed out") from error
            try:
                os.killpg(process.pid, 0)
            except ProcessLookupError:
                pass
            except OSError as error:
                raise HostMeasurementError("host command process-group state is unavailable") from error
            else:
                _stop_process_group(process)
                raise HostMeasurementError("host command left a running descendant")
        except OSError as error:
            if process is not None:
                _stop_process_group(process)
            raise HostMeasurementError("host command could not run") from error
        duration_ms = max(0, round((time.monotonic() - started) * 1000))
        stdout_size = os.fstat(stdout.fileno()).st_size
        stderr_size = os.fstat(stderr.fileno()).st_size
        require(
            stdout_size <= MAX_COMMAND_BYTES and stderr_size <= MAX_COMMAND_BYTES,
            "host command output exceeds its byte bound",
        )
        stdout.seek(0)
        stderr.seek(0)
        return CommandResult(
            operation=operation,
            returncode=returncode,
            duration_ms=duration_ms,
            stdout=stdout.read(MAX_COMMAND_BYTES + 1),
            stderr=stderr.read(MAX_COMMAND_BYTES + 1),
        )


def _command_binding(result: CommandResult, *, executable_sha256: str) -> dict[str, Any]:
    require(result.returncode == 0, f"{result.operation} failed")
    require(
        0 <= result.duration_ms <= MAX_DURATION_MS,
        f"{result.operation} duration differs",
    )
    require(
        len(result.stdout) <= MAX_COMMAND_BYTES and len(result.stderr) <= MAX_COMMAND_BYTES,
        f"{result.operation} output exceeds its byte bound",
    )
    return {
        "operation": result.operation,
        "executable_sha256": executable_sha256,
        "exit_code": result.returncode,
        "duration_ms": result.duration_ms,
        "stdout_sha256": sha256_bytes(result.stdout),
        "stderr_sha256": sha256_bytes(result.stderr),
    }


def parse_codex_version(output: bytes) -> str:
    require(len(output) <= MAX_VERSION_BYTES, "Codex version output exceeds its byte bound")
    try:
        value = output.decode("utf-8")
    except UnicodeError as error:
        raise HostMeasurementError("Codex version output is not UTF-8") from error
    match = re.fullmatch(r"codex-cli ([0-9][0-9A-Za-z.+-]{0,63})\n?", value)
    require(match is not None, "Codex version output differs")
    return match.group(1)


def parse_vscode_version(output: bytes) -> str:
    require(len(output) <= MAX_VERSION_BYTES, "VS Code version output exceeds its byte bound")
    try:
        lines = output.decode("utf-8").splitlines()
    except UnicodeError as error:
        raise HostMeasurementError("VS Code version output is not UTF-8") from error
    require(lines and VERSION_RE.fullmatch(lines[0]) is not None, "VS Code version output differs")
    return lines[0]


def parse_feature_rows(output: bytes) -> list[dict[str, object]]:
    require(len(output) <= MAX_COMMAND_BYTES, "feature output exceeds its byte bound")
    try:
        text = output.decode("utf-8")
    except UnicodeError as error:
        raise HostMeasurementError("feature output is not UTF-8") from error
    rows: dict[str, dict[str, object]] = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        match = FEATURE_LINE_RE.fullmatch(line)
        if match is None:
            first = line.split(maxsplit=1)[0] if line.split(maxsplit=1) else ""
            require(first not in RELEVANT_FEATURES, "feature row differs")
            continue
        name = match.group("name")
        if name not in RELEVANT_FEATURES:
            continue
        require(name not in rows, "feature row is duplicated")
        maturity = match.group("maturity").strip()
        require(MATURITY_RE.fullmatch(maturity) is not None, "feature maturity differs")
        rows[name] = {
            "name": name,
            "maturity": maturity,
            "enabled": match.group("enabled") == "true",
        }
    require(set(rows) == set(RELEVANT_FEATURES), "relevant feature rows are incomplete")
    return [rows[name] for name in RELEVANT_FEATURES]


def project_features(rows: Sequence[Mapping[str, object]]) -> dict[str, Any]:
    found: dict[str, dict[str, object]] = {}
    for raw in rows:
        require(
            isinstance(raw, Mapping) and set(raw) == {"name", "maturity", "enabled"},
            "feature projection row differs",
        )
        name = raw["name"]
        maturity = raw["maturity"]
        enabled = raw["enabled"]
        require(name in RELEVANT_FEATURES, "feature projection name differs")
        require(name not in found, "feature projection row is duplicated")
        require(
            isinstance(maturity, str) and MATURITY_RE.fullmatch(maturity) is not None,
            "feature projection maturity differs",
        )
        require(isinstance(enabled, bool), "feature projection state differs")
        found[str(name)] = {"name": name, "maturity": maturity, "enabled": enabled}
    require(set(found) == set(RELEVANT_FEATURES), "feature projection is incomplete")
    core = {
        "schema_version": FEATURE_SCHEMA,
        "complete": True,
        "rows": [found[name] for name in RELEVANT_FEATURES],
    }
    return {**core, "sha256": sha256_bytes(canonical_json(core))}


def _project_schema(value: Any, *, member: str | None = None) -> Any:
    if isinstance(value, dict):
        projected: dict[str, Any] = {}
        for key in sorted(set(value) & SCHEMA_KEYS):
            child = value[key]
            if key in {"properties", "definitions"}:
                require(isinstance(child, dict), "app-server schema map differs")
                projected[key] = {
                    name: _project_schema(child[name], member=key)
                    for name in sorted(child)
                }
            elif key in {"required", "enum"}:
                require(isinstance(child, list), "app-server schema list differs")
                if all(isinstance(item, str) for item in child):
                    projected[key] = sorted(child)
                else:
                    projected[key] = [_project_schema(item, member=key) for item in child]
            else:
                projected[key] = _project_schema(child, member=key)
        return projected
    if isinstance(value, list):
        return [_project_schema(item, member=member) for item in value]
    require(
        value is None or isinstance(value, (bool, int, float, str)),
        "app-server schema primitive differs",
    )
    return value


def _enum_values(value: Any) -> list[tuple[str, ...]]:
    found: list[tuple[str, ...]] = []
    if isinstance(value, dict):
        enum_value = value.get("enum")
        if isinstance(enum_value, list) and all(isinstance(item, str) for item in enum_value):
            found.append(tuple(enum_value))
        for child in value.values():
            found.extend(_enum_values(child))
    elif isinstance(value, list):
        for child in value:
            found.extend(_enum_values(child))
    return found


def project_app_server(documents: Mapping[str, Mapping[str, Any]]) -> dict[str, Any]:
    require(all(name in documents for name in APP_SERVER_FILES), "app-server schema set is incomplete")
    projected: dict[str, Any] = {}
    for name in APP_SERVER_FILES:
        document = documents[name]
        require(isinstance(document, Mapping), "app-server schema document differs")
        require(document.get("title") == APP_SERVER_TITLES[name], "app-server schema title differs")
        require(document.get("type") == "object", "app-server schema root differs")
        required = document.get("required", [])
        properties = document.get("properties")
        require(
            isinstance(required, list)
            and set(required) == APP_SERVER_REQUIRED[name]
            and isinstance(properties, Mapping)
            and APP_SERVER_PROPERTIES[name] <= set(properties),
            "app-server method contract differs",
        )
        projected[name] = _project_schema(dict(document))
    initialize = documents["v1/InitializeParams.json"]
    require(
        "mcpServerOpenaiFormElicitation" in canonical_json(initialize).decode("utf-8"),
        "openai/form capability is missing",
    )
    request = documents["McpServerElicitationRequestParams.json"]
    require(
        any("openai/form" in values for values in _enum_values(request)),
        "openai/form request mode is missing",
    )
    response = documents["McpServerElicitationRequestResponse.json"]
    action_definition = response.get("definitions", {}).get("McpServerElicitationAction", {})
    actions = action_definition.get("enum") if isinstance(action_definition, Mapping) else None
    require(
        actions == list(REQUIRED_ELICITATION_ACTIONS),
        "elicitation action set differs",
    )
    core = {
        "schema_version": APP_SERVER_SCHEMA,
        "complete": True,
        "methods": list(APP_SERVER_METHODS),
        "elicitation_actions": list(REQUIRED_ELICITATION_ACTIONS),
        "schemas": projected,
    }
    return {**core, "sha256": sha256_bytes(canonical_json(core))}


def project_contract(
    documents: Mapping[str, Mapping[str, Any]],
    rows: Sequence[Mapping[str, object]],
) -> dict[str, Any]:
    feature_projection = project_features(rows)
    app_server_projection = project_app_server(documents)
    binding = {
        "feature_projection_sha256": feature_projection["sha256"],
        "app_server_projection_sha256": app_server_projection["sha256"],
    }
    return {
        "complete": True,
        "feature_projection": feature_projection,
        "app_server_projection": app_server_projection,
        "contract_projection_sha256": sha256_bytes(canonical_json(binding)),
    }


def client_groups(surfaces: Sequence[Mapping[str, Any]]) -> tuple[tuple[str, tuple[str, ...]], ...]:
    by_surface: dict[str, str] = {}
    for value in surfaces:
        require(isinstance(value, Mapping), "measured host surface differs")
        surface = value.get("surface")
        digest = value.get("client_artifact_sha256")
        require(surface in host_delta.SURFACE_SET, "measured host surface differs")
        require(surface not in by_surface, "measured host surface is duplicated")
        require(
            isinstance(digest, str) and host_delta.DIGEST_RE.fullmatch(digest) is not None,
            "measured client digest differs",
        )
        by_surface[str(surface)] = digest
    require(set(by_surface) == host_delta.SURFACE_SET, "measured host surfaces are incomplete")
    grouped: dict[str, list[str]] = {}
    for surface in host_delta.SURFACES:
        grouped.setdefault(by_surface[surface], []).append(surface)
    order = {name: index for index, name in enumerate(host_delta.SURFACES)}
    return tuple(
        (digest, tuple(names))
        for digest, names in sorted(grouped.items(), key=lambda item: order[item[1][0]])
    )


def _signature_coordinate(
    path: Path,
    *,
    deep: bool,
    identifier: str,
    team_id: str,
) -> dict[str, str]:
    try:
        measured = measure_code_signature(path, deep=deep)
    except HostProvenanceError as error:
        raise HostMeasurementError(str(error)) from error
    require(
        isinstance(measured, dict)
        and set(measured) == {"verified", "mode", "identifier", "team_id", "cdhash"}
        and measured["verified"] is True
        and measured["mode"] == ("deep_strict" if deep else "strict")
        and measured["identifier"] == identifier
        and measured["team_id"] == team_id,
        "host code-signature coordinate differs",
    )
    return {
        "mode": measured["mode"],
        "identifier": measured["identifier"],
        "team_id": measured["team_id"],
        "cdhash": measured["cdhash"],
    }


def _plist_coordinate(app_bundle: Path) -> tuple[str, str]:
    path = app_bundle / "Contents/Info.plist"
    raw = _read_regular_file(path, label="Codex app Info.plist", maximum=MAX_JSON_BYTES)
    try:
        value = plistlib.loads(raw)
    except (plistlib.InvalidFileException, ValueError) as error:
        raise HostMeasurementError("Codex app Info.plist differs") from error
    require(isinstance(value, dict), "Codex app Info.plist differs")
    version = value.get("CFBundleShortVersionString")
    build = value.get("CFBundleVersion")
    require(
        value.get("CFBundleIdentifier") == "com.openai.codex"
        and isinstance(version, str)
        and VERSION_RE.fullmatch(version) is not None
        and isinstance(build, str)
        and VERSION_RE.fullmatch(build) is not None,
        "Codex app bundle coordinate differs",
    )
    return version, build


def _extension_coordinate(path: Path) -> str:
    raw = _read_regular_file(path, label="Codex IDE package metadata", maximum=MAX_JSON_BYTES)
    value = _strict_json(raw, label="Codex IDE package metadata")
    version = value.get("version")
    require(
        value.get("name") == "chatgpt"
        and value.get("publisher") == "openai"
        and isinstance(version, str)
        and VERSION_RE.fullmatch(version) is not None,
        "Codex IDE extension coordinate differs",
    )
    return version


def _load_inputs(options: MeasureOptions) -> tuple[dict[str, Any], dict[str, Any], bytes]:
    profile_raw = _read_regular_file(
        options.embedded_profile,
        label="embedded host profile",
        maximum=MAX_JSON_BYTES,
    )
    matrix_raw = _read_regular_file(
        options.embedded_matrix,
        label="embedded compatibility matrix",
        maximum=MAX_JSON_BYTES,
    )
    profile = _strict_json(profile_raw, label="embedded host profile")
    matrix = _strict_json(matrix_raw, label="embedded compatibility matrix")
    package = matrix.get("package")
    require(
        matrix.get("schema_version") == "godot-codex-compatibility-matrix/1.0"
        and isinstance(package, dict)
        and package.get("target") == TARGET
        and isinstance(package.get("version"), str)
        and VERSION_RE.fullmatch(package["version"]) is not None,
        "embedded compatibility matrix differs",
    )
    try:
        host_delta.build_host_profile(
            embedded_profile=profile,
            measured_surfaces=profile.get("surfaces", []),
            qualification="candidate",
            profile_id=profile.get("profile_id", "invalid"),
        )
    except host_delta.HostDeltaError as error:
        raise HostMeasurementError(str(error)) from error
    require(
        profile["compatibility_matrix"]["sha256"] == sha256_bytes(matrix_raw),
        "embedded matrix binding differs",
    )
    return profile, matrix, matrix_raw


def _read_schema_documents(root: Path) -> dict[str, dict[str, Any]]:
    root = _canonical_path(root, kind="generated app-server schema root")
    require(root.is_dir(), "generated app-server schema root differs")
    documents: dict[str, dict[str, Any]] = {}
    for name in APP_SERVER_FILES:
        path = root.joinpath(*name.split("/"))
        raw = _read_regular_file(path, label=f"app-server schema {Path(name).name}", maximum=MAX_JSON_BYTES)
        documents[name] = _strict_json(raw, label=f"app-server schema {Path(name).name}")
    return documents


def _contract_set(
    client_entries: Sequence[tuple[str, tuple[str, ...], dict[str, Any]]],
) -> tuple[dict[str, Any], dict[str, Any], str]:
    feature_clients: list[dict[str, Any]] = []
    app_server_clients: list[dict[str, Any]] = []
    contract_clients: list[dict[str, str]] = []
    for digest, surfaces, contract in client_entries:
        feature = contract["feature_projection"]
        app_server = contract["app_server_projection"]
        feature_clients.append(
            {
                "client_artifact_sha256": digest,
                "surfaces": list(surfaces),
                "rows": feature["rows"],
                "projection_sha256": feature["sha256"],
            }
        )
        app_server_clients.append(
            {
                "client_artifact_sha256": digest,
                "surfaces": list(surfaces),
                "methods": app_server["methods"],
                "elicitation_actions": app_server["elicitation_actions"],
                "projection_sha256": app_server["sha256"],
            }
        )
        contract_clients.append(
            {
                "client_artifact_sha256": digest,
                "contract_projection_sha256": contract["contract_projection_sha256"],
            }
        )
    feature_core = {
        "schema_version": FEATURE_SET_SCHEMA,
        "complete": True,
        "clients": feature_clients,
    }
    app_server_core = {
        "schema_version": APP_SERVER_SET_SCHEMA,
        "complete": True,
        "clients": app_server_clients,
    }
    feature_set = {**feature_core, "sha256": sha256_bytes(canonical_json(feature_core))}
    app_server_set = {**app_server_core, "sha256": sha256_bytes(canonical_json(app_server_core))}
    contract_binding = {
        "feature_projection_sha256": feature_set["sha256"],
        "app_server_projection_sha256": app_server_set["sha256"],
        "clients": contract_clients,
    }
    return feature_set, app_server_set, sha256_bytes(canonical_json(contract_binding))


def publish_measurement(output: Path, document: Mapping[str, Any]) -> None:
    require(output.is_absolute(), "measurement output path must be absolute")
    require(not output.exists() and not output.is_symlink(), "measurement output already exists")
    parent = _canonical_path(output.parent, kind="measurement output directory")
    require(parent.is_dir(), "measurement output directory differs")
    payload = canonical_json(document) + b"\n"
    staging = parent / f".{output.name}.staging-{uuid.uuid4().hex}"
    descriptor: int | None = None
    linked = False
    staging_removed = False
    try:
        descriptor = os.open(
            staging,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0),
            0o600,
        )
        view = memoryview(payload)
        while view:
            written = os.write(descriptor, view)
            require(written > 0, "measurement output write stalled")
            view = view[written:]
        os.fsync(descriptor)
        os.close(descriptor)
        descriptor = None
        os.link(staging, output)
        linked = True
        staging.unlink()
        staging_removed = True
        directory = os.open(parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except FileExistsError as error:
        raise HostMeasurementError("measurement output already exists") from error
    except OSError as error:
        if error.errno == errno.EEXIST:
            raise HostMeasurementError("measurement output already exists") from error
        if linked:
            try:
                output.unlink()
            except OSError:
                pass
        raise HostMeasurementError("measurement output cannot be published") from error
    finally:
        if descriptor is not None:
            os.close(descriptor)
        if not staging_removed:
            try:
                staging.unlink()
            except OSError:
                pass


def measure_host_delta(options: MeasureOptions) -> dict[str, Any]:
    require(
        isinstance(options.timeout_seconds, (int, float))
        and not isinstance(options.timeout_seconds, bool)
        and 0 < options.timeout_seconds <= 120,
        "host measurement timeout differs",
    )
    require(
        platform.system() == "Darwin" and platform.machine() == "arm64",
        "host measurement requires macOS arm64",
    )
    require(not options.output.exists(), "measurement output already exists")
    profile, matrix, matrix_raw = _load_inputs(options)

    app_bundle = _canonical_path(options.app_bundle, kind="Codex app bundle")
    app_executable = _canonical_path(options.app_executable, kind="Codex app executable")
    app_client = _canonical_path(options.app_client, kind="Codex app client")
    cli_client = _canonical_path(options.cli_client, kind="Codex CLI client")
    vscode_bundle = _canonical_path(options.vscode_bundle, kind="VS Code bundle")
    vscode_executable = _canonical_path(options.vscode_executable, kind="VS Code executable")
    extension_root = _canonical_path(options.extension_root, kind="Codex IDE extension root")
    extension_package = _canonical_path(
        options.extension_package_json,
        kind="Codex IDE package metadata",
    )
    ide_client = _canonical_path(options.ide_client, kind="Codex IDE client")
    require(
        all(path.is_dir() for path in (app_bundle, vscode_bundle, extension_root)),
        "host bundle kind differs",
    )

    app_host_digest, _ = safe_file_digest(app_executable, label="Codex app executable")
    app_client_digest, _ = safe_file_digest(app_client, label="Codex app client")
    cli_digest, _ = safe_file_digest(cli_client, label="Codex CLI client")
    vscode_digest, _ = safe_file_digest(vscode_executable, label="VS Code executable")
    ide_client_digest, _ = safe_file_digest(ide_client, label="Codex IDE client")
    extension_metadata_digest, _ = safe_file_digest(
        extension_package,
        label="Codex IDE package metadata",
    )
    try:
        extension_tree = stable_tree_digest(extension_root)
    except HostProvenanceError as error:
        raise HostMeasurementError(str(error)) from error

    app_signature = _signature_coordinate(
        app_bundle,
        deep=True,
        identifier="com.openai.codex",
        team_id="2DC432GLL2",
    )
    app_client_signature = _signature_coordinate(
        app_client,
        deep=False,
        identifier="codex",
        team_id="2DC432GLL2",
    )
    cli_signature = _signature_coordinate(
        cli_client,
        deep=False,
        identifier="codex",
        team_id="2DC432GLL2",
    )
    vscode_signature = _signature_coordinate(
        vscode_bundle,
        deep=True,
        identifier="com.microsoft.VSCode",
        team_id="UBF8T346G9",
    )
    ide_client_signature = _signature_coordinate(
        ide_client,
        deep=False,
        identifier="codex",
        team_id="2DC432GLL2",
    )

    app_version, app_build = _plist_coordinate(app_bundle)
    extension_version = _extension_coordinate(extension_package)
    commands: list[dict[str, Any]] = []
    app_version_result = run_bounded_command(
        (str(app_client), "--version"),
        options.timeout_seconds,
        "app_client_version",
    )
    commands.append(_command_binding(app_version_result, executable_sha256=app_client_digest))
    app_client_version = parse_codex_version(app_version_result.stdout)
    cli_version_result = run_bounded_command(
        (str(cli_client), "--version"),
        options.timeout_seconds,
        "cli_client_version",
    )
    commands.append(_command_binding(cli_version_result, executable_sha256=cli_digest))
    cli_version = parse_codex_version(cli_version_result.stdout)
    ide_version_result = run_bounded_command(
        (str(ide_client), "--version"),
        options.timeout_seconds,
        "ide_client_version",
    )
    commands.append(_command_binding(ide_version_result, executable_sha256=ide_client_digest))
    ide_client_version = parse_codex_version(ide_version_result.stdout)
    vscode_version_result = run_bounded_command(
        (str(vscode_executable), "--version"),
        options.timeout_seconds,
        "vscode_version",
    )
    commands.append(_command_binding(vscode_version_result, executable_sha256=vscode_digest))
    vscode_version = parse_vscode_version(vscode_version_result.stdout)

    surfaces = [
        {
            "surface": "app",
            "host_name": "codex-desktop",
            "host_identifier": "com.openai.codex",
            "host_artifact_kind": "macos_bundle_executable",
            "host_version": app_version,
            "host_build": app_build,
            "host_commit": None,
            "host_artifact_sha256": app_host_digest,
            "host_metadata_sha256": None,
            "host_code_signature": app_signature,
            "client_version": app_client_version,
            "client_artifact_sha256": app_client_digest,
            "client_code_signature": app_client_signature,
            "ide_host_version": None,
            "ide_shell_identifier": None,
            "ide_shell_artifact_sha256": None,
            "ide_shell_team_id": None,
            "ide_shell_code_signature": None,
            "qualification": "candidate",
        },
        {
            "surface": "cli",
            "host_name": "codex-cli",
            "host_identifier": "codex-cli",
            "host_artifact_kind": "standalone_executable",
            "host_version": cli_version,
            "host_build": cli_version,
            "host_commit": None,
            "host_artifact_sha256": cli_digest,
            "host_metadata_sha256": None,
            "host_code_signature": cli_signature,
            "client_version": cli_version,
            "client_artifact_sha256": cli_digest,
            "client_code_signature": cli_signature,
            "ide_host_version": None,
            "ide_shell_identifier": None,
            "ide_shell_artifact_sha256": None,
            "ide_shell_team_id": None,
            "ide_shell_code_signature": None,
            "qualification": "candidate",
        },
        {
            "surface": "ide",
            "host_name": "openai.chatgpt",
            "host_identifier": "openai.chatgpt",
            "host_artifact_kind": "extension_tree",
            "host_version": extension_version,
            "host_build": extension_version,
            "host_commit": None,
            "host_artifact_sha256": extension_tree.sha256,
            "host_metadata_sha256": extension_metadata_digest,
            "host_code_signature": None,
            "client_version": ide_client_version,
            "client_artifact_sha256": ide_client_digest,
            "client_code_signature": ide_client_signature,
            "ide_host_version": vscode_version,
            "ide_shell_identifier": "com.microsoft.VSCode",
            "ide_shell_artifact_sha256": vscode_digest,
            "ide_shell_team_id": "UBF8T346G9",
            "ide_shell_code_signature": vscode_signature,
            "qualification": "candidate",
        },
    ]
    try:
        host_delta.build_host_profile(
            embedded_profile=profile,
            measured_surfaces=surfaces,
            qualification="candidate",
            profile_id="host-delta-measurement-candidate",
        )
    except host_delta.HostDeltaError as error:
        raise HostMeasurementError(str(error)) from error

    groups = client_groups(surfaces)
    client_path_by_surface = {"app": app_client, "cli": cli_client, "ide": ide_client}
    client_entries: list[tuple[str, tuple[str, ...], dict[str, Any]]] = []
    for index, (client_digest, group_surfaces) in enumerate(groups, start=1):
        client_path = client_path_by_surface[group_surfaces[0]]
        features_result = run_bounded_command(
            (str(client_path), "features", "list"),
            options.timeout_seconds,
            f"client_{index}_features",
        )
        commands.append(_command_binding(features_result, executable_sha256=client_digest))
        rows = parse_feature_rows(features_result.stdout)
        with tempfile.TemporaryDirectory(prefix="godot-codex-host-schema-") as temporary:
            schema_root = Path(temporary).resolve()
            os.chmod(schema_root, 0o700)
            schema_result = run_bounded_command(
                (
                    str(client_path),
                    "app-server",
                    "generate-json-schema",
                    "--out",
                    str(schema_root),
                ),
                options.timeout_seconds,
                f"client_{index}_schema",
            )
            commands.append(_command_binding(schema_result, executable_sha256=client_digest))
            documents = _read_schema_documents(schema_root)
        client_entries.append((client_digest, group_surfaces, project_contract(documents, rows)))

    feature_projection, app_server_projection, contract_digest = _contract_set(client_entries)
    try:
        extension_after = stable_tree_digest(extension_root)
    except HostProvenanceError as error:
        raise HostMeasurementError(str(error)) from error
    require(extension_after == extension_tree, "Codex IDE extension changed during measurement")
    for path, label, expected in (
        (app_executable, "Codex app executable", app_host_digest),
        (app_client, "Codex app client", app_client_digest),
        (cli_client, "Codex CLI client", cli_digest),
        (vscode_executable, "VS Code executable", vscode_digest),
        (ide_client, "Codex IDE client", ide_client_digest),
        (extension_package, "Codex IDE package metadata", extension_metadata_digest),
    ):
        require(safe_file_digest(path, label=label)[0] == expected, f"{label} changed during measurement")

    document = {
        "schema_version": MEASUREMENT_SCHEMA,
        "target": dict(TARGET),
        "package": {
            "version": matrix["package"]["version"],
            "baseline_matrix_sha256": sha256_bytes(matrix_raw),
        },
        "surfaces": surfaces,
        "client_groups": [
            {"client_artifact_sha256": digest, "surfaces": list(names)}
            for digest, names in groups
        ],
        "feature_projection": feature_projection,
        "app_server_projection": app_server_projection,
        "contract_projection_sha256": contract_digest,
        "commands": commands,
        "assertions": {
            "package_unchanged": True,
            "all_required_surfaces_measured": True,
            "signatures_verified": True,
            "manual_interaction_absent": True,
        },
        "redaction": {
            "absolute_paths_absent": True,
            "account_identity_absent": True,
            "artifact_content_absent": True,
            "environment_secrets_absent": True,
        },
    }
    canonical_json(document)
    publish_measurement(options.output, document)
    return document
