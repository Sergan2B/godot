#!/usr/bin/env python3
"""Run a deterministic Codex app-server MCP smoke without a model turn."""

from __future__ import annotations

import dataclasses
import hashlib
import json
import os
import re
import selectors
import signal
import stat
import subprocess
import tempfile
import time
from collections.abc import Mapping
from pathlib import Path
from typing import Any, Final

try:
    from tests.codex import sprint11_host_delta as host_delta
    from tests.codex import sprint11_host_delta_measure as host_measure
    from tests.codex.sprint11_host_provenance import (
        HostProvenanceError,
        _canonical_nonsymlink_path,
    )
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_host_delta as host_delta
    import sprint11_host_delta_measure as host_measure
    from sprint11_host_provenance import (
        HostProvenanceError,
        _canonical_nonsymlink_path,
    )


MAX_LINE_BYTES: Final = 2 * 1024 * 1024
MAX_TOTAL_OUTPUT_BYTES: Final = 16 * 1024 * 1024
MAX_NOTIFICATIONS: Final = 4096
MAX_REGISTRY_BYTES: Final = 512 * 1024
MAX_PROJECT_FILES: Final = 16_384
MAX_PROJECT_BYTES: Final = 512 * 1024 * 1024
READ_BYTES: Final = 64 * 1024
PROCESS_SCOPE_RE: Final = re.compile(r"GODOT_CODEX_PROCESS_SCOPE_[0-9a-f]{48}\Z")
PROJECT_ID_RE: Final = re.compile(r"project:sha256:([0-9a-f]{64})\Z")
REGISTRY_FIELDS: Final = {
    "schema_version",
    "profile_id",
    "tool_count",
    "fixed_resource_count",
    "resource_template_count",
    "tools",
    "read_only_tools",
    "fixed_resources",
    "resource_templates",
    "digest",
}
PROJECT_IGNORED_ROOTS: Final = frozenset({".codex", ".git", ".godot"})


class AppServerProtocolError(RuntimeError):
    """The model-free app-server flow failed closed."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AppServerProtocolError(message)


@dataclasses.dataclass(frozen=True)
class SmokeOptions:
    client: Path
    expected_client_sha256: str
    launcher: Path
    project_root: Path
    expected_project_id: str
    expected_registry: Path
    output: Path
    surfaces: tuple[str, ...]
    timeout_seconds: float = 60.0


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
        raise AppServerProtocolError("app-server evidence is not canonical JSON") from error


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def safe_file_digest(path: Path, *, label: str) -> tuple[str, int]:
    try:
        return host_measure.safe_file_digest(path, label=label)
    except host_measure.HostMeasurementError as error:
        raise AppServerProtocolError(str(error)) from error


def _canonical_path(path: Path, *, kind: str) -> Path:
    try:
        return _canonical_nonsymlink_path(path, kind=kind)
    except HostProvenanceError as error:
        raise AppServerProtocolError(str(error)) from error


def _strict_json(raw: bytes, *, label: str, maximum: int) -> dict[str, Any]:
    try:
        return host_delta.strict_json_bytes(
            raw,
            maximum_bytes=maximum,
            label=label,
        )
    except host_delta.HostDeltaError as error:
        raise AppServerProtocolError(str(error)) from error


def _read_registry(path: Path) -> dict[str, Any]:
    path = _canonical_path(path, kind="registry profile")
    digest, size = safe_file_digest(path, label="registry profile")
    del digest
    require(size <= MAX_REGISTRY_BYTES, "registry profile exceeds its byte bound")
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise AppServerProtocolError("registry profile cannot be read") from error
    require(len(raw) == size, "registry profile changed while reading")
    profile = _strict_json(raw, label="registry profile", maximum=MAX_REGISTRY_BYTES)
    require(set(profile) == REGISTRY_FIELDS, "registry profile fields differ")
    require(
        profile["schema_version"] == "godot-codex-registry-profile/1.0"
        and profile["profile_id"] == "external-codex-beta-v1",
        "registry profile identity differs",
    )
    for field, count in (
        ("tools", 41),
        ("fixed_resources", 4),
        ("resource_templates", 1),
    ):
        values = profile[field]
        require(
            isinstance(values, list)
            and len(values) == count
            and values == sorted(set(values))
            and all(isinstance(value, str) for value in values),
            f"registry profile {field} differs",
        )
    read_only = profile["read_only_tools"]
    require(
        isinstance(read_only, list)
        and read_only == sorted(set(read_only))
        and set(read_only) <= set(profile["tools"])
        and "godot_get_connection_status" in read_only
        and "godot_get_current_scene" in read_only,
        "registry read-only profile differs",
    )
    require(
        profile["tool_count"] == 41
        and profile["fixed_resource_count"] == 4
        and profile["resource_template_count"] == 1,
        "registry profile counts differ",
    )
    semantic = {
        "profile_id": profile["profile_id"],
        "tools": profile["tools"],
        "read_only_tools": read_only,
        "fixed_resources": profile["fixed_resources"],
        "resource_templates": profile["resource_templates"],
    }
    semantic_digest = hashlib.sha256(
        json.dumps(
            semantic,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8")
    ).hexdigest()
    require(
        re.fullmatch(r"[0-9a-f]{64}", profile["digest"]) is not None
        and profile["digest"] == semantic_digest,
        "registry profile semantic digest differs",
    )
    return profile


def _project_source_digest(root: Path) -> str:
    """Hash non-derived project files without retaining names or content."""

    records: list[dict[str, Any]] = []
    total_bytes = 0

    def walk(directory: Path, relative: tuple[str, ...]) -> None:
        nonlocal total_bytes
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except OSError as error:
            raise AppServerProtocolError("project source tree cannot be read") from error
        for entry in entries:
            if not relative and entry.name in PROJECT_IGNORED_ROOTS:
                continue
            require("/" not in entry.name and "\x00" not in entry.name, "project source path differs")
            parts = (*relative, entry.name)
            require(len(records) < MAX_PROJECT_FILES, "project source tree exceeds its entry bound")
            try:
                metadata = entry.stat(follow_symlinks=False)
            except OSError as error:
                raise AppServerProtocolError("project source entry is unavailable") from error
            require(not stat.S_ISLNK(metadata.st_mode), "project source tree contains a symlink")
            relative_name = "/".join(parts)
            if stat.S_ISDIR(metadata.st_mode):
                records.append({"kind": "directory", "path": relative_name, "mode": stat.S_IMODE(metadata.st_mode)})
                walk(Path(entry.path), parts)
            elif stat.S_ISREG(metadata.st_mode):
                digest, size = safe_file_digest(Path(entry.path), label="project source file")
                total_bytes += size
                require(total_bytes <= MAX_PROJECT_BYTES, "project source tree exceeds its byte bound")
                records.append(
                    {
                        "kind": "file",
                        "path": relative_name,
                        "mode": stat.S_IMODE(metadata.st_mode),
                        "bytes": size,
                        "sha256": digest,
                    }
                )
            else:
                raise AppServerProtocolError("project source tree contains a special file")

    walk(root, ())
    return sha256_bytes(canonical_json(records))


def _stable_project_source_digest(root: Path) -> str:
    first = _project_source_digest(root)
    second = _project_source_digest(root)
    require(first == second, "project source tree is unstable")
    return first


def _strict_line(raw: bytes) -> dict[str, Any]:
    require(raw and len(raw) <= MAX_LINE_BYTES, "app-server JSON line exceeds its byte bound")
    return _strict_json(raw, label="app-server JSON line", maximum=MAX_LINE_BYTES)


class JsonRpcLineClient:
    """One-outstanding-request JSON-RPC client over one exact child process."""

    def __init__(self, process: subprocess.Popen[bytes], *, deadline: float) -> None:
        require(process.stdin is not None and process.stdout is not None and process.stderr is not None, "app-server pipes differ")
        self.process = process
        self.deadline = deadline
        self.selector = selectors.DefaultSelector()
        self.selector.register(process.stdout, selectors.EVENT_READ, "stdout")
        self.selector.register(process.stderr, selectors.EVENT_READ, "stderr")
        self.stdout_buffer = bytearray()
        self.stderr_buffer = bytearray()
        self.total_output = 0
        self.notifications = 0
        self.next_id = 1
        self.completed_ids: set[int] = set()
        self.outstanding = False
        self.closed = False
        self.clean_shutdown = False

    def _remaining(self) -> float:
        remaining = self.deadline - time.monotonic()
        require(remaining > 0, "app-server deadline exceeded")
        return remaining

    def _send(self, value: Mapping[str, Any]) -> None:
        require(not self.closed and self.process.stdin is not None, "app-server client is closed")
        payload = canonical_json(value) + b"\n"
        require(len(payload) <= MAX_LINE_BYTES, "app-server request exceeds its byte bound")
        try:
            self.process.stdin.write(payload)
            self.process.stdin.flush()
        except (BrokenPipeError, OSError) as error:
            raise AppServerProtocolError("app-server transport closed while sending") from error

    def _read_message(self) -> dict[str, Any]:
        while True:
            newline = self.stdout_buffer.find(b"\n")
            if newline >= 0:
                line = bytes(self.stdout_buffer[:newline])
                del self.stdout_buffer[: newline + 1]
                return _strict_line(line)
            require(len(self.stdout_buffer) <= MAX_LINE_BYTES, "app-server JSON line exceeds its byte bound")
            events = self.selector.select(self._remaining())
            require(events, "app-server deadline exceeded")
            for key, _mask in events:
                try:
                    chunk = os.read(key.fileobj.fileno(), READ_BYTES)
                except OSError as error:
                    raise AppServerProtocolError("app-server output cannot be read") from error
                if not chunk:
                    try:
                        self.selector.unregister(key.fileobj)
                    except (KeyError, ValueError):
                        pass
                    if key.data == "stdout":
                        require(not self.stdout_buffer, "app-server emitted a truncated JSON line")
                        raise AppServerProtocolError("app-server closed before its response")
                    continue
                self.total_output += len(chunk)
                require(self.total_output <= MAX_TOTAL_OUTPUT_BYTES, "app-server output exceeds its total bound")
                if key.data == "stdout":
                    self.stdout_buffer.extend(chunk)
                else:
                    self.stderr_buffer.extend(chunk)
                    require(len(self.stderr_buffer) <= MAX_TOTAL_OUTPUT_BYTES, "app-server stderr exceeds its bound")

    def request(self, method: str, params: Mapping[str, Any]) -> dict[str, Any]:
        require(not self.outstanding, "app-server allows only one outstanding request")
        request_id = self.next_id
        self.next_id += 1
        self.outstanding = True
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": dict(params)})
        try:
            while True:
                message = self._read_message()
                require(
                    message.get("jsonrpc", "2.0") == "2.0",
                    "app-server JSON-RPC version differs",
                )
                if "id" not in message:
                    require(isinstance(message.get("method"), str), "app-server notification differs")
                    self.notifications += 1
                    require(self.notifications <= MAX_NOTIFICATIONS, "app-server notification bound exceeded")
                    continue
                response_id = message["id"]
                require(isinstance(response_id, int) and not isinstance(response_id, bool), "app-server response ID differs")
                require(response_id not in self.completed_ids, "app-server response ID is duplicated")
                require(response_id == request_id, "app-server response ID is unexpected")
                require("method" not in message, "unexpected app-server request")
                self.completed_ids.add(response_id)
                if "error" in message:
                    raise AppServerProtocolError("app-server returned a JSON-RPC error")
                result = message.get("result")
                require(isinstance(result, dict), "app-server result differs")
                return result
        finally:
            self.outstanding = False

    def notify(self, method: str, params: Mapping[str, Any]) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": dict(params)})

    def close(self) -> None:
        if self.closed:
            return
        self.closed = True
        graceful = False
        if self.process.stdin is not None:
            try:
                self.process.stdin.close()
            except OSError:
                pass
        try:
            returncode = self.process.wait(timeout=0.35)
            graceful = returncode == 0
        except subprocess.TimeoutExpired:
            for action, wait_seconds in (
                (lambda: self.process.send_signal(signal.SIGINT), 0.25),
                (self.process.terminate, 0.25),
                (self.process.kill, 0.5),
            ):
                try:
                    action()
                except ProcessLookupError:
                    break
                try:
                    self.process.wait(timeout=wait_seconds)
                    break
                except subprocess.TimeoutExpired:
                    continue
            else:
                raise AppServerProtocolError("app-server exact child could not be stopped")
        self.clean_shutdown = graceful
        try:
            self.selector.close()
        finally:
            for stream in (self.process.stdout, self.process.stderr):
                if stream is not None:
                    try:
                        stream.close()
                    except OSError:
                        pass


def _isolated_environment(codex_home: Path) -> dict[str, str]:
    environment = {
        "CODEX_HOME": str(codex_home),
        "HOME": str(codex_home),
        "LANG": "C",
        "LC_ALL": "C",
        "PATH": "/usr/bin:/bin",
    }
    for name, value in os.environ.items():
        if not name.startswith("GODOT_CODEX_PROCESS_SCOPE_"):
            continue
        require(
            PROCESS_SCOPE_RE.fullmatch(name) is not None and value == "1",
            "process scope environment differs",
        )
        environment[name] = value
    return environment


def _toml_string(value: str) -> str:
    require("\x00" not in value and "\n" not in value and "\r" not in value, "app-server config path differs")
    return json.dumps(value, ensure_ascii=False)


def _tool_payload(result: Mapping[str, Any]) -> dict[str, Any]:
    require(result.get("isError") in {None, False}, "Godot MCP tool returned an error")
    structured = result.get("structuredContent")
    if isinstance(structured, dict):
        return structured
    content = result.get("content")
    require(isinstance(content, list) and len(content) == 1, "Godot MCP tool result differs")
    item = content[0]
    require(
        isinstance(item, dict)
        and item.get("type") == "text"
        and isinstance(item.get("text"), str),
        "Godot MCP text result differs",
    )
    return _strict_json(item["text"].encode("utf-8"), label="Godot MCP tool JSON", maximum=MAX_LINE_BYTES)


def _validate_inventory(result: Mapping[str, Any], registry: Mapping[str, Any]) -> dict[str, int | str]:
    data = result.get("data")
    require(
        isinstance(data, list)
        and len(data) == 1
        and result.get("nextCursor") is None,
        "app-server MCP inventory pagination differs",
    )
    server = data[0]
    require(isinstance(server, dict) and server.get("name") == "godot_editor", "godot_editor inventory is absent")
    tools = server.get("tools")
    resources = server.get("resources")
    templates = server.get("resourceTemplates")
    require(isinstance(tools, dict), "Godot MCP tool inventory differs")
    require(
        set(tools) == set(registry["tools"]),
        "Godot MCP registry tool names differ",
    )
    require(isinstance(resources, list), "Godot MCP resource inventory differs")
    resource_names = [item.get("uri") for item in resources if isinstance(item, dict)]
    require(
        len(resource_names) == len(resources)
        and sorted(resource_names) == registry["fixed_resources"],
        "Godot MCP fixed resources differ",
    )
    require(isinstance(templates, list), "Godot MCP template inventory differs")
    template_names = [item.get("uriTemplate") for item in templates if isinstance(item, dict)]
    require(
        len(template_names) == len(templates)
        and sorted(template_names) == registry["resource_templates"],
        "Godot MCP resource templates differ",
    )
    return {
        "digest": registry["digest"],
        "tools": len(tools),
        "fixed_resources": len(resources),
        "resource_templates": len(templates),
    }


def run_client_smoke(options: SmokeOptions) -> dict[str, Any]:
    require(
        isinstance(options.timeout_seconds, (int, float))
        and not isinstance(options.timeout_seconds, bool)
        and 0 < options.timeout_seconds <= 180,
        "app-server smoke timeout differs",
    )
    require(not options.output.exists(), "host smoke output already exists")
    require(
        options.surfaces
        and len(options.surfaces) == len(set(options.surfaces))
        and set(options.surfaces) <= host_delta.SURFACE_SET,
        "host smoke surface membership differs",
    )
    client = _canonical_path(options.client, kind="Codex client")
    launcher = _canonical_path(options.launcher, kind="package-owned launcher")
    project = _canonical_path(options.project_root, kind="qualification project")
    require(client.is_file() and launcher.is_file() and project.is_dir(), "host smoke input kind differs")
    require(
        client.stat().st_mode & 0o111 and launcher.stat().st_mode & 0o111,
        "host smoke executable permission differs",
    )
    client_digest = safe_file_digest(client, label="Codex client")[0]
    launcher_digest = safe_file_digest(launcher, label="package-owned launcher")[0]
    require(
        host_delta.DIGEST_RE.fullmatch(options.expected_client_sha256) is not None
        and client_digest == options.expected_client_sha256,
        "Codex client digest differs",
    )
    project_match = PROJECT_ID_RE.fullmatch(options.expected_project_id)
    require(project_match is not None, "expected project ID differs")
    project_scope = project_match.group(1)
    registry = _read_registry(options.expected_registry)
    before_source = _stable_project_source_digest(project)
    started = time.monotonic()
    deadline = started + float(options.timeout_seconds)
    process: subprocess.Popen[bytes] | None = None
    protocol: JsonRpcLineClient | None = None
    primary_error: BaseException | None = None
    report_parts: dict[str, Any] | None = None
    with tempfile.TemporaryDirectory(prefix="godot-codex-app-server-") as temporary:
        codex_home = Path(temporary).resolve()
        os.chmod(codex_home, 0o700)
        argv = (
            str(client),
            "app-server",
            "--stdio",
            "--strict-config",
            "-c",
            f"mcp_servers.godot_editor.command={_toml_string(str(launcher))}",
            "-c",
            'mcp_servers.godot_editor.args=["mcp"]',
            "-c",
            f"mcp_servers.godot_editor.cwd={_toml_string(str(project))}",
            "-c",
            "mcp_servers.godot_editor.required=true",
        )
        command_projection = {
            "client_sha256": client_digest,
            "launcher_sha256": launcher_digest,
            "project_scope_sha256": sha256_bytes(project_scope.encode("ascii")),
            "arguments": [
                "app-server",
                "--stdio",
                "--strict-config",
                "required-godot-editor-with-package-launcher",
            ],
        }
        try:
            try:
                process = subprocess.Popen(
                    list(argv),
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    cwd=project,
                    env=_isolated_environment(codex_home),
                    start_new_session=False,
                )
            except OSError as error:
                raise AppServerProtocolError("Codex app-server could not start") from error
            protocol = JsonRpcLineClient(process, deadline=deadline)
            protocol.request(
                "initialize",
                {
                    "clientInfo": {"name": "godot-codex-host-smoke", "version": "1.0.0"},
                    "capabilities": {
                        "experimentalApi": True,
                        "mcpServerOpenaiFormElicitation": True,
                    },
                },
            )
            protocol.notify("notifications/initialized", {})
            thread = protocol.request(
                "thread/start",
                {
                    "cwd": str(project),
                    "ephemeral": True,
                    "sandbox": "read-only",
                    "approvalPolicy": "never",
                },
            )
            thread_value = thread.get("thread")
            thread_id = thread_value.get("id") if isinstance(thread_value, dict) else None
            require(isinstance(thread_id, str) and 1 <= len(thread_id) <= 128, "app-server thread ID differs")
            inventory = protocol.request(
                "mcpServerStatus/list",
                {"threadId": thread_id, "detail": "full"},
            )
            registry_projection = _validate_inventory(inventory, registry)
            status_result = protocol.request(
                "mcpServer/tool/call",
                {
                    "server": "godot_editor",
                    "threadId": thread_id,
                    "tool": "godot_get_connection_status",
                    "arguments": {},
                },
            )
            status = _tool_payload(status_result)
            bridge = status.get("bridge")
            cache = status.get("static_cache")
            require(
                status.get("schema_version") == "godot-connection-status/1.1"
                and status.get("status") == "ready"
                and status.get("project_scope") == project_scope
                and isinstance(bridge, dict)
                and bridge.get("negotiated_protocol") == "1.8"
                and isinstance(cache, dict)
                and cache.get("condition") == "online_current",
                "Godot MCP connection status differs",
            )
            scene_result = protocol.request(
                "mcpServer/tool/call",
                {
                    "server": "godot_editor",
                    "threadId": thread_id,
                    "tool": "godot_get_current_scene",
                    "arguments": {},
                },
            )
            scene = _tool_payload(scene_result)
            evidence = scene.get("evidence")
            require(
                scene.get("project_id") == options.expected_project_id
                and scene.get("freshness") == "current"
                and isinstance(scene.get("snapshot_id"), str)
                and 1 <= len(scene["snapshot_id"]) <= 256
                and isinstance(evidence, list)
                and evidence
                and all(isinstance(item, dict) for item in evidence)
                and any(item.get("freshness") == "current" for item in evidence),
                "Godot MCP semantic evidence differs",
            )
            semantic_projection = {
                "project_id": scene["project_id"],
                "snapshot_id": scene["snapshot_id"],
                "freshness": scene["freshness"],
                "evidence": evidence,
            }
            report_parts = {
                "registry": registry_projection,
                "connection": {
                    "schema_version": "godot-connection-status/1.1",
                    "status": "ready",
                    "bridge_protocol": "1.8",
                    "static_cache": "online_current",
                    "project_scope_sha256": sha256_bytes(project_scope.encode("ascii")),
                },
                "semantic_fact": {
                    "freshness": "current",
                    "evidence_sha256": sha256_bytes(canonical_json(semantic_projection)),
                },
                "execution": {
                    "command_sha256": sha256_bytes(canonical_json(command_projection)),
                    "duration_ms": 0,
                },
            }
        except BaseException as error:  # Preserve the first protocol failure through cleanup.
            primary_error = error
        finally:
            if protocol is not None:
                try:
                    protocol.close()
                except BaseException as cleanup_error:
                    if primary_error is None:
                        primary_error = cleanup_error
            elif process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=1.0)
        if primary_error is not None:
            if isinstance(primary_error, AppServerProtocolError):
                raise primary_error
            raise AppServerProtocolError("app-server smoke failed") from primary_error
        require(protocol is not None and protocol.clean_shutdown, "Codex app-server did not shut down cleanly")

    require(report_parts is not None, "app-server smoke report is incomplete")
    after_source = _stable_project_source_digest(project)
    require(before_source == after_source, "qualification project source changed during smoke")
    duration_ms = max(0, round((time.monotonic() - started) * 1000))
    report_parts["execution"]["duration_ms"] = duration_ms
    report = {
        "schema_version": "s11-host-smoke/1.0",
        "status": "passed",
        "client_artifact_sha256": client_digest,
        "surfaces": list(options.surfaces),
        "protocol": "2025-11-25",
        "registry": report_parts["registry"],
        "connection": report_parts["connection"],
        "semantic_fact": report_parts["semantic_fact"],
        "execution": report_parts["execution"],
        "assertions": {
            "model_turn_absent": True,
            "project_integrity_preserved": True,
            "isolated_codex_home": True,
            "exact_child_shutdown": True,
            "clean_shutdown": True,
        },
        "redaction": {
            "absolute_paths_absent": True,
            "account_identity_absent": True,
            "artifact_content_absent": True,
            "environment_secrets_absent": True,
        },
    }
    try:
        host_delta.validate_smoke_report(report)
    except host_delta.HostDeltaError as error:
        raise AppServerProtocolError(str(error)) from error
    try:
        host_measure.publish_measurement(options.output, report)
    except host_measure.HostMeasurementError as error:
        raise AppServerProtocolError(str(error)) from error
    return report
