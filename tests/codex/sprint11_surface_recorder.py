#!/usr/bin/env python3
"""Fail-closed, content-minimizing stdio recorder for Sprint 11 surface runs.

The recorder is intentionally a transparent byte proxy. It never writes to
stdout except for bytes produced by the wrapped MCP server, and it never puts
request arguments, tool content, elicitation content, source text, or native
handles in its journal. The journal proves one package-bound transport
capture; it does not prove which GUI or CLI process supplied stdin. The
canonical semantic trace is produced separately and must bind the exact
journal artifact SHA-256, ordered event hash-chain, and event count. A
qualifying run additionally requires a separate Git-bound external-operator
authority for the observed App/CLI/IDE session.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import signal
import stat
import subprocess
import sys
import threading
import time
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any, BinaryIO

try:
    from tests.codex import sprint11_acquisition_paths as acquisition_paths
    from tests.codex import sprint11_host_provenance as host_provenance
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_acquisition_paths as acquisition_paths
    import sprint11_host_provenance as host_provenance

SCHEMA_VERSION = "s11-recorder-journal/1.0"
EVENT_CHAIN_DOMAIN = b"godot-codex/s11-recorder-event-chain/v1\0"
MAX_FRAME_BYTES = 1_048_576
MAX_EVENTS = 4_096
MAX_JOURNAL_BYTES = 524_288
MAX_METADATA_BYTES = 32_768
MAX_TOKEN_BYTES = 256
PUMP_JOIN_SECONDS = 0.5
PROCESS_TERM_SECONDS = 1.0
MAX_CHILD_SECONDS = 180.0
MAX_EXECUTABLE_BYTES = 512 * 1024 * 1024
MAX_PACKAGE_MANIFEST_BYTES = 16 * 1024 * 1024
REPOSITORY_ROOT = Path(__file__).resolve().parent.parent.parent
HOST_PROFILE_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "product"
    / "host-coordinate-profile.v1.json"
)
HOST_PROVENANCE_NAME = "measurement.json"

DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")
TOKEN_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:/{}-]{0,255}\Z")
HOST_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9 ._+()-]{0,127}\Z")
TOOL_RE = re.compile(r"godot_[a-z0-9_]+\Z")
RESOURCE_RE = re.compile(r"godot://[a-z0-9_/{}/.-]+\Z")

CLIENT_TO_SERVER = "client_to_server"
SERVER_TO_CLIENT = "server_to_client"


class RecorderError(RuntimeError):
    """The acquisition is not safe or complete enough to qualify."""


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        allow_nan=False,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def event_chain_sha256(events: Sequence[Mapping[str, Any]]) -> str:
    """Bind the exact projected event order without retaining private payloads."""

    state = hashlib.sha256(EVENT_CHAIN_DOMAIN).digest()
    for event in events:
        state = hashlib.sha256(
            state + b"\0" + canonical_json(event)
        ).digest()
    return "sha256:" + state.hex()


def _strict_json(data: bytes, *, label: str, maximum: int) -> dict[str, Any]:
    if len(data) > maximum:
        raise RecorderError(f"{label}: byte bound exceeded")

    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in items:
            if key in result:
                raise RecorderError(f"{label}: duplicate member")
            result[key] = value
        return result

    def reject_constant(_value: str) -> None:
        raise RecorderError(f"{label}: non-finite number")

    try:
        value = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=pairs,
            parse_constant=reject_constant,
        )
    except (UnicodeError, json.JSONDecodeError) as error:
        raise RecorderError(f"{label}: invalid JSON") from error
    if not isinstance(value, dict):
        raise RecorderError(f"{label}: root must be an object")
    return value


def _bounded_token(value: Any, *, label: str) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode("utf-8")) > MAX_TOKEN_BYTES
        or TOKEN_RE.fullmatch(value) is None
    ):
        raise RecorderError(f"{label}: unsafe token")
    return value


def _bounded_host(value: Any, *, label: str) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode("utf-8")) > 128
        or HOST_RE.fullmatch(value) is None
    ):
        raise RecorderError(f"{label}: unsafe host coordinate")
    return value


def _digest(value: Any, *, label: str) -> str:
    if not isinstance(value, str) or DIGEST_RE.fullmatch(value) is None:
        raise RecorderError(f"{label}: digest differs")
    return value


def _validated_host_provenance(
    metadata_path: Path,
    *,
    surface: str,
    binding: Mapping[str, Any],
) -> tuple[dict[str, Any], dict[str, Any]]:
    if set(binding) != {"path", "sha256"}:
        raise RecorderError("host provenance binding fields differ")
    if binding["path"] != HOST_PROVENANCE_NAME:
        raise RecorderError("host provenance path differs")
    expected_digest = _digest(binding["sha256"], label="host provenance")
    receipt_path = metadata_path.parent / HOST_PROVENANCE_NAME
    try:
        payload = acquisition_paths.read_regular_file(
            receipt_path,
            maximum=host_provenance.MAX_OUTPUT_BYTES,
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise RecorderError("host provenance receipt is unavailable") from error
    if sha256_bytes(payload) != expected_digest:
        raise RecorderError("host provenance receipt digest differs")
    receipt = _strict_json(
        payload,
        label="host provenance measurement",
        maximum=host_provenance.MAX_OUTPUT_BYTES,
    )
    try:
        validated = host_provenance.validate_host_provenance_document(
            receipt,
            profile_path=HOST_PROFILE_PATH,
        )
        profile, profile_digest = host_provenance._profile(HOST_PROFILE_PATH)
    except host_provenance.HostProvenanceError as error:
        raise RecorderError("host provenance measurement differs") from error
    measurements = validated["surfaces"]
    coordinate = next(
        item
        for item in profile["surfaces"]
        if item["surface"] == surface
    )
    measurement = next(
        item
        for item in measurements
        if item["surface"] == surface
    )
    host = {
        "name": coordinate["host_name"],
        "identifier": coordinate["host_identifier"],
        "artifact_kind": coordinate["host_artifact_kind"],
        "version": coordinate["host_version"],
        "build": coordinate["host_build"],
        "commit": coordinate["host_commit"],
        "client_version": coordinate["client_version"],
        "host_metadata_sha256": measurement["host_metadata_sha256"],
        "host_code_signature": measurement["host_code_signature"],
        "client_code_signature": measurement["client_code_signature"],
        "ide_host_version": coordinate["ide_host_version"],
        "ide_shell_identifier": coordinate["ide_shell_identifier"],
        "ide_shell_artifact_sha256": measurement[
            "ide_shell_artifact_sha256"
        ],
        "ide_shell_team_id": coordinate["ide_shell_team_id"],
        "ide_shell_code_signature": measurement[
            "ide_shell_code_signature"
        ],
        "architecture": "arm64",
    }
    return host, {
        "host_provenance_sha256": expected_digest,
        "host_artifact_sha256": measurement["host_artifact_sha256"],
        "client_artifact_sha256": measurement["client_artifact_sha256"],
        "host_coordinate_profile_sha256": profile_digest,
    }


def load_metadata(path: Path) -> dict[str, Any]:
    try:
        document = _strict_json(
            acquisition_paths.read_regular_file(
                path,
                maximum=MAX_METADATA_BYTES,
            ),
            label="recorder metadata",
            maximum=MAX_METADATA_BYTES,
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise RecorderError("recorder metadata is unavailable") from error
    if set(document) != {
        "surface",
        "host_provenance",
        "bindings",
        "host_controls",
    }:
        raise RecorderError("recorder metadata fields differ")
    surface = document["surface"]
    if surface not in {"app", "cli", "ide"}:
        raise RecorderError("recorder surface differs")
    raw_bindings = document["bindings"]
    expected = {
        "package_source_commit",
        "package_manifest_sha256",
        "compatibility_matrix_sha256",
        "host_coordinate_profile_sha256",
        "project_fixture_sha256",
        "registry_sha256",
        "prompt_pack_sha256",
        "mcp_binary_sha256",
        "godot_artifact_sha256",
    }
    if not isinstance(raw_bindings, dict) or set(raw_bindings) != expected:
        raise RecorderError("recorder binding fields differ")
    if (
        not isinstance(raw_bindings["package_source_commit"], str)
        or COMMIT_RE.fullmatch(raw_bindings["package_source_commit"]) is None
    ):
        raise RecorderError("recorder source commit differs")
    for field in expected - {"package_source_commit"}:
        _digest(raw_bindings[field], label=field)
    provenance_binding = document["host_provenance"]
    if not isinstance(provenance_binding, dict):
        raise RecorderError("host provenance binding differs")
    host, measured = _validated_host_provenance(
        path,
        surface=surface,
        binding=provenance_binding,
    )
    if (
        raw_bindings["host_coordinate_profile_sha256"]
        != measured["host_coordinate_profile_sha256"]
    ):
        raise RecorderError("metadata host profile binding differs")
    bindings = {**raw_bindings, **measured}
    controls = document["host_controls"]
    if not isinstance(controls, dict) or set(controls) != {
        "sandbox_approval_observed",
        "config_reload_observed",
        "root_binding_observed",
        "package_launcher_observed",
        "package_launcher_path",
        "package_launcher_sha256",
    }:
        raise RecorderError("recorder host-control fields differ")
    for field in (
        "sandbox_approval_observed",
        "config_reload_observed",
        "root_binding_observed",
        "package_launcher_observed",
    ):
        if controls[field] is not True:
            raise RecorderError("recorder host-control observation differs")
    if controls["package_launcher_path"] != "bin/godot-codex-mcp":
        raise RecorderError("recorder package launcher path differs")
    if (
        _digest(
            controls["package_launcher_sha256"],
            label="package launcher",
        )
        != bindings["mcp_binary_sha256"]
    ):
        raise RecorderError("recorder package launcher binding differs")
    return {
        "surface": surface,
        "host": host,
        "bindings": bindings,
        "host_controls": controls,
    }


def _id_key(value: Any) -> bytes | None:
    if isinstance(value, (str, int)) and not isinstance(value, bool):
        return canonical_json(value)
    return None


def _safe_method(value: Any) -> str | None:
    if not isinstance(value, str) or len(value.encode("utf-8")) > MAX_TOKEN_BYTES:
        return None
    return value if TOKEN_RE.fullmatch(value) is not None else None


def _tool_name(params: Any) -> str | None:
    if not isinstance(params, dict):
        return None
    value = params.get("name")
    return value if isinstance(value, str) and TOOL_RE.fullmatch(value) else None


def _resource_uri(params: Any) -> str | None:
    if not isinstance(params, dict):
        return None
    value = params.get("uri")
    return value if isinstance(value, str) and RESOURCE_RE.fullmatch(value) else None


def _string_list(
    value: Any,
    *,
    member: str,
    pattern: re.Pattern[str],
    maximum: int,
) -> list[str] | None:
    if not isinstance(value, list) or len(value) > maximum:
        return None
    result: list[str] = []
    for item in value:
        if not isinstance(item, dict):
            return None
        candidate = item.get(member)
        if not isinstance(candidate, str) or pattern.fullmatch(candidate) is None:
            return None
        result.append(candidate)
    return sorted(set(result))


class ProtocolRecorder:
    """Thread-safe bounded projection of an MCP stdio exchange."""

    def __init__(
        self,
        metadata: Mapping[str, Any],
        *,
        capture_kind: str = "synthetic_contract_fixture",
    ) -> None:
        if capture_kind not in {
            "surface_transport_capture",
            "synthetic_contract_fixture",
        }:
            raise RecorderError("recorder capture kind differs")
        self.metadata = dict(metadata)
        self.capture_kind = capture_kind
        self.lock = threading.Lock()
        self.events: list[dict[str, Any]] = []
        self.requests: dict[
            tuple[str, bytes],
            tuple[str, str | None, str | None],
        ] = {}
        self.form_parents: dict[
            tuple[str, bytes],
            tuple[str, bytes] | None,
        ] = {}
        self.input_bytes = 0
        self.output_bytes = 0
        self.input_frames = 0
        self.output_frames = 0
        self.protocol_version: str | None = None
        self.truncated = False
        self.errors: list[str] = []

    def note_transport(self, direction: str, frame: bytes) -> None:
        with self.lock:
            if direction == CLIENT_TO_SERVER:
                self.input_bytes += len(frame)
                self.input_frames += 1
            else:
                self.output_bytes += len(frame)
                self.output_frames += 1
            if len(frame) > MAX_FRAME_BYTES:
                self._fault("frame_bound_exceeded")
                return
        try:
            message = _strict_json(
                frame.rstrip(b"\r\n"),
                label="MCP frame",
                maximum=MAX_FRAME_BYTES,
            )
        except RecorderError:
            self._append_event(
                {
                    "direction": direction,
                    "kind": "unparsed_frame",
                    "frame_bytes": len(frame),
                }
            )
            return
        self._record_message(direction, message, len(frame))

    def _fault(self, code: str) -> None:
        if code not in self.errors:
            self.errors.append(code)
        self.truncated = True

    def _append_event(self, event: dict[str, Any]) -> None:
        with self.lock:
            if len(self.events) >= MAX_EVENTS:
                self._fault("event_bound_exceeded")
                return
            event["seq"] = len(self.events)
            self.events.append(event)

    def _record_message(
        self,
        direction: str,
        message: Mapping[str, Any],
        frame_bytes: int,
    ) -> None:
        method = _safe_method(message.get("method"))
        request_key = _id_key(message.get("id"))
        params = message.get("params")
        if method is not None:
            tool = _tool_name(params) if method == "tools/call" else None
            resource = _resource_uri(params) if method == "resources/read" else None
            if request_key is not None:
                response_direction = (
                    SERVER_TO_CLIENT
                    if direction == CLIENT_TO_SERVER
                    else CLIENT_TO_SERVER
                )
                with self.lock:
                    response_key = (response_direction, request_key)
                    self.requests[response_key] = (
                        method,
                        tool,
                        resource,
                    )
                    if method == "elicitation/create":
                        candidates = [
                            key
                            for key, pending in self.requests.items()
                            if key[0] == SERVER_TO_CLIENT
                            and pending[0] == "tools/call"
                            and pending[1] == "godot_apply_transaction"
                        ]
                        self.form_parents[response_key] = (
                            candidates[0] if len(candidates) == 1 else None
                        )
            kind = "request" if request_key is not None else "notification"
            event: dict[str, Any] = {
                "direction": direction,
                "kind": kind,
                "method": method,
                "frame_bytes": frame_bytes,
            }
            if tool is not None:
                event["tool"] = tool
            if resource is not None:
                event["resource"] = resource
            if method == "elicitation/create":
                event["form_mode"] = (
                    "form"
                    if isinstance(params, dict) and params.get("mode") == "form"
                    else "unsupported"
                )
            self._append_event(event)
            return

        if request_key is None:
            self._append_event(
                {
                    "direction": direction,
                    "kind": "message_without_method_or_id",
                    "frame_bytes": frame_bytes,
                }
            )
            return
        response_key = (direction, request_key)
        with self.lock:
            request = self.requests.pop(response_key, None)
            if request is not None and request[0] == "elicitation/create":
                self.form_parents.pop(response_key, None)
        if request is None:
            self._append_event(
                {
                    "direction": direction,
                    "kind": "unmatched_response",
                    "frame_bytes": frame_bytes,
                }
            )
            return
        request_method, tool, resource = request
        event = {
            "direction": direction,
            "kind": "response",
            "method": request_method,
            "frame_bytes": frame_bytes,
            "is_error": "error" in message,
        }
        if tool is not None:
            event["tool"] = tool
        if resource is not None:
            event["resource"] = resource
        self._response_projection(event, request_method, message)
        if (
            request_method == "tools/call"
            and event.get("error_code") == "approval_timeout"
        ):
            with self.lock:
                correlated = [
                    form_key
                    for form_key, parent_key in self.form_parents.items()
                    if parent_key == response_key
                ]
                if len(correlated) == 1:
                    form_key = correlated[0]
                    self.form_parents.pop(form_key, None)
                    self.requests.pop(form_key, None)
                    event["form_action"] = "timeout"
                    event["content_recorded"] = False
        self._append_event(event)

    def _response_projection(
        self,
        event: dict[str, Any],
        method: str,
        message: Mapping[str, Any],
    ) -> None:
        result = message.get("result")
        if method == "initialize" and isinstance(result, dict):
            version = result.get("protocolVersion")
            if isinstance(version, str) and TOKEN_RE.fullmatch(version):
                self.protocol_version = version
                event["protocol_version"] = version
            instructions = result.get("instructions")
            if isinstance(instructions, str):
                event["instructions_sha256"] = sha256_bytes(
                    instructions.encode("utf-8")
                )
            return
        if method == "tools/list" and isinstance(result, dict):
            names = _string_list(
                result.get("tools"),
                member="name",
                pattern=TOOL_RE,
                maximum=256,
            )
            if names is not None:
                event["tools"] = names
            return
        if method == "resources/list" and isinstance(result, dict):
            uris = _string_list(
                result.get("resources"),
                member="uri",
                pattern=RESOURCE_RE,
                maximum=64,
            )
            if uris is not None:
                event["resources"] = uris
            return
        if method == "resources/templates/list" and isinstance(result, dict):
            templates = _string_list(
                result.get("resourceTemplates"),
                member="uriTemplate",
                pattern=RESOURCE_RE,
                maximum=64,
            )
            if templates is not None:
                event["resource_templates"] = templates
            return
        if method == "elicitation/create" and isinstance(result, dict):
            action = result.get("action")
            if action in {"accept", "decline", "cancel"}:
                event["form_action"] = action
            else:
                event["form_action"] = "unsupported"
            event["content_recorded"] = False
            return
        if method == "tools/call":
            structured: Mapping[str, Any] | None = None
            if isinstance(result, dict):
                event["tool_error"] = result.get("isError") is True
                candidate = result.get("structuredContent")
                if isinstance(candidate, dict):
                    structured = candidate
            error = message.get("error")
            if structured is None and isinstance(error, dict):
                data = error.get("data")
                if isinstance(data, dict):
                    structured = data
                else:
                    structured = error
            if structured is not None:
                code = structured.get("code")
                state = structured.get("state")
                status = structured.get("status")
                for key, candidate in (
                    ("error_code", code),
                    ("semantic_state", state),
                    ("semantic_status", status),
                ):
                    if isinstance(candidate, str) and TOKEN_RE.fullmatch(candidate):
                        event[key] = candidate

    def document(self, child_exit_code: int, pump_errors: Sequence[str]) -> dict[str, Any]:
        with self.lock:
            errors = sorted(set([*self.errors, *pump_errors]))
            truncated = self.truncated
            events = list(self.events)
            outstanding_forms = sum(
                1
                for method, _tool, _resource in self.requests.values()
                if method == "elicitation/create"
            )
            if outstanding_forms:
                errors.append("elicitation_response_missing")
                truncated = True
            document = {
                "schema_version": SCHEMA_VERSION,
                "capture_kind": self.capture_kind,
                "status": (
                    "complete"
                    if child_exit_code == 0 and not errors and not truncated
                    else "incomplete"
                ),
                "surface": self.metadata["surface"],
                "host": self.metadata["host"],
                "bindings": self.metadata["bindings"],
                "host_controls": self.metadata["host_controls"],
                "protocol_version": self.protocol_version or "unobserved",
                "events": events,
                "integrity": {
                    "input_bytes": self.input_bytes,
                    "output_bytes": self.output_bytes,
                    "input_frames": self.input_frames,
                    "output_frames": self.output_frames,
                    "event_count": len(events),
                    "event_chain_sha256": event_chain_sha256(events),
                    "child_exit_code": child_exit_code,
                    "passthrough_mode": True,
                    "recorder_errors": sorted(set(errors)),
                    "truncated": truncated,
                },
                "redaction": {
                    "request_arguments_absent": True,
                    "tool_content_absent": True,
                    "elicitation_content_absent": True,
                    "source_content_absent": True,
                    "native_ids_absent": True,
                    "absolute_paths_absent": True,
                    "secrets_absent": True,
                },
            }
        if len(canonical_json(document)) > MAX_JOURNAL_BYTES:
            document["status"] = "incomplete"
            document["events"] = []
            document["integrity"]["event_count"] = 0
            document["integrity"]["event_chain_sha256"] = event_chain_sha256([])
            document["integrity"]["recorder_errors"] = sorted(
                set(
                    [
                        *document["integrity"]["recorder_errors"],
                        "journal_bound_exceeded",
                    ]
                )
            )
            document["integrity"]["truncated"] = True
        return document


def _pump(
    source: BinaryIO,
    destination: BinaryIO,
    *,
    direction: str,
    recorder: ProtocolRecorder,
    errors: list[str],
    close_destination: bool,
) -> None:
    try:
        while True:
            frame = source.readline()
            if not frame:
                break
            destination.write(frame)
            destination.flush()
            recorder.note_transport(direction, frame)
    except (BrokenPipeError, OSError):
        errors.append(f"{direction}_pump_failed")
    finally:
        if close_destination:
            try:
                destination.close()
            except (BrokenPipeError, OSError):
                errors.append(f"{direction}_close_failed")


def _atomic_write(path: Path, document: Mapping[str, Any]) -> None:
    payload = canonical_json(document) + b"\n"
    try:
        acquisition_paths.atomic_write_new_file(
            path,
            payload,
            prefix=".s11-recorder-",
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise RecorderError("journal output path is unsafe or already exists") from error


def _canonical_path(path: Path, *, label: str, directory: bool) -> Path:
    if not path.is_absolute():
        raise RecorderError(f"{label} path is not absolute")
    try:
        lexical = Path(os.path.abspath(path))
        resolved = path.resolve(strict=True)
        metadata = path.lstat()
    except OSError as error:
        raise RecorderError(f"{label} path is unavailable") from error
    expected = stat.S_ISDIR if directory else stat.S_ISREG
    if (
        lexical != resolved
        or stat.S_ISLNK(metadata.st_mode)
        or not expected(metadata.st_mode)
    ):
        raise RecorderError(f"{label} path is not canonical and symlink-free")
    return resolved


def _regular_file_bytes(path: Path, *, label: str, maximum: int) -> bytes:
    try:
        return acquisition_paths.read_regular_file(path, maximum=maximum)
    except acquisition_paths.AcquisitionPathError as error:
        raise RecorderError(f"{label} is unavailable") from error


def _open_verified_executable(path: Path, *, expected_sha256: str) -> int:
    path = _canonical_path(path, label="package MCP executable", directory=False)
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise RecorderError(
            "package MCP executable cannot be opened safely"
        ) from error
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or before.st_mode & 0o022
            or before.st_mode & 0o111 == 0
            or not 1 <= before.st_size <= MAX_EXECUTABLE_BYTES
        ):
            raise RecorderError("package MCP executable metadata differs")
        digest = hashlib.sha256()
        remaining = MAX_EXECUTABLE_BYTES + 1
        while True:
            chunk = os.read(descriptor, min(1024 * 1024, remaining))
            if not chunk:
                break
            digest.update(chunk)
            remaining -= len(chunk)
            if remaining < 0:
                raise RecorderError(
                    "package MCP executable exceeds its byte bound"
                )
        after = os.fstat(descriptor)
        if (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        ) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        ):
            raise RecorderError("package MCP executable changed while hashing")
        if "sha256:" + digest.hexdigest() != expected_sha256:
            raise RecorderError("package MCP executable digest differs")
        os.lseek(descriptor, 0, os.SEEK_SET)
        return descriptor
    except Exception:
        os.close(descriptor)
        raise


def _qualifying_command(
    command: Sequence[str],
    metadata: Mapping[str, Any],
) -> tuple[list[str], int]:
    if (
        len(command) != 3
        or not all(
            isinstance(item, str) and "\0" not in item for item in command
        )
        or command[1] != "--project-root"
    ):
        raise RecorderError("wrapped MCP argv differs from the closed profile")
    executable = _canonical_path(
        Path(command[0]),
        label="package MCP executable",
        directory=False,
    )
    if executable.name != "godot-codex-mcp" or executable.parent.name != "bin":
        raise RecorderError("wrapped MCP executable is not package-owned")
    package_root = executable.parent.parent
    if package_root.parent.name != "versions":
        raise RecorderError(
            "wrapped MCP executable is not in a versioned install"
        )
    package_root = _canonical_path(
        package_root,
        label="versioned package",
        directory=True,
    )
    version = _regular_file_bytes(
        package_root / "VERSION",
        label="package VERSION",
        maximum=256,
    )
    try:
        version_text = version.decode("ascii").strip()
    except UnicodeError as error:
        raise RecorderError("package VERSION differs") from error
    if (
        not version.endswith(b"\n")
        or re.fullmatch(
            r"[0-9A-Za-z][0-9A-Za-z.+-]{0,127}",
            version_text,
        )
        is None
        or package_root.name != version_text
    ):
        raise RecorderError("versioned package coordinate differs")
    manifest = _regular_file_bytes(
        package_root / "package-manifest.json",
        label="package manifest",
        maximum=MAX_PACKAGE_MANIFEST_BYTES,
    )
    manifest_sha256 = sha256_bytes(manifest)
    bindings = metadata.get("bindings")
    if (
        not isinstance(bindings, Mapping)
        or manifest_sha256 != bindings.get("package_manifest_sha256")
    ):
        raise RecorderError("package manifest binding differs")
    ownership = _regular_file_bytes(
        package_root / ".godot-codex-owned",
        label="package ownership marker",
        maximum=128,
    )
    if ownership != (
        manifest_sha256.removeprefix("sha256:") + "\n"
    ).encode():
        raise RecorderError("package ownership marker differs")
    project_root = _canonical_path(
        Path(command[2]),
        label="project root",
        directory=True,
    )
    _regular_file_bytes(
        project_root / "project.godot",
        label="project.godot",
        maximum=16 * 1024 * 1024,
    )
    descriptor = _open_verified_executable(
        executable,
        expected_sha256=_digest(
            bindings.get("mcp_binary_sha256"),
            label="package MCP executable",
        ),
    )
    return [str(executable), "--project-root", str(project_root)], descriptor


def _terminate_process_group(
    child: subprocess.Popen[bytes],
    errors: list[str],
) -> None:
    process_group = child.pid
    try:
        os.killpg(process_group, signal.SIGTERM)
    except ProcessLookupError:
        return
    except OSError:
        errors.append("child_process_group_terminate_failed")
    deadline = time.monotonic() + PROCESS_TERM_SECONDS
    while child.poll() is None and time.monotonic() < deadline:
        try:
            child.wait(
                timeout=min(
                    0.05,
                    max(0.0, deadline - time.monotonic()),
                )
            )
        except subprocess.TimeoutExpired:
            pass
    try:
        os.killpg(process_group, signal.SIGKILL)
    except ProcessLookupError:
        pass
    except OSError:
        errors.append("child_process_group_kill_failed")
    try:
        child.wait(timeout=PROCESS_TERM_SECONDS)
    except subprocess.TimeoutExpired:
        errors.append("child_process_survived_kill")
    try:
        os.killpg(process_group, 0)
    except (ProcessLookupError, PermissionError):
        return
    except OSError:
        return
    errors.append("child_process_group_survived")


def _write_failed_journal(
    recorder: ProtocolRecorder,
    journal_path: Path,
    error_code: str,
) -> None:
    _atomic_write(journal_path, recorder.document(126, [error_code]))


def run_proxy(
    command: Sequence[str],
    *,
    metadata: Mapping[str, Any],
    journal_path: Path,
    stdin: BinaryIO,
    stdout: BinaryIO,
    child_timeout: float = MAX_CHILD_SECONDS,
    synthetic_command: bool = False,
) -> int:
    if not command:
        raise RecorderError("wrapped command is missing")
    if (
        isinstance(child_timeout, bool)
        or not isinstance(child_timeout, (int, float))
        or not 1 <= child_timeout <= MAX_CHILD_SECONDS
    ):
        raise RecorderError("wrapped MCP timeout differs")
    recorder = ProtocolRecorder(
        metadata,
        capture_kind=(
            "synthetic_contract_fixture"
            if synthetic_command
            else "surface_transport_capture"
        ),
    )
    pump_errors: list[str] = []
    executable_descriptor: int | None = None
    qualified_command = list(command)
    if not synthetic_command:
        try:
            qualified_command, executable_descriptor = _qualifying_command(
                command,
                metadata,
            )
        except RecorderError:
            _write_failed_journal(
                recorder,
                journal_path,
                "command_validation_failed",
            )
            raise
    try:
        child = subprocess.Popen(
            qualified_command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=None,
            start_new_session=True,
        )
    except OSError as error:
        _write_failed_journal(recorder, journal_path, "child_start_failed")
        raise RecorderError("wrapped MCP server could not start") from error
    finally:
        if executable_descriptor is not None:
            os.close(executable_descriptor)
    assert child.stdin is not None
    assert child.stdout is not None
    inbound = threading.Thread(
        target=_pump,
        args=(stdin, child.stdin),
        kwargs={
            "direction": CLIENT_TO_SERVER,
            "recorder": recorder,
            "errors": pump_errors,
            "close_destination": True,
        },
        name="s11-recorder-input",
        daemon=True,
    )
    inbound.start()
    outbound = threading.Thread(
        target=_pump,
        args=(child.stdout, stdout),
        kwargs={
            "direction": SERVER_TO_CLIENT,
            "recorder": recorder,
            "errors": pump_errors,
            "close_destination": False,
        },
        name="s11-recorder-output",
        daemon=True,
    )
    outbound.start()
    try:
        exit_code = child.wait(timeout=child_timeout)
    except subprocess.TimeoutExpired:
        pump_errors.append("child_timeout")
        _terminate_process_group(child, pump_errors)
        exit_code = child.returncode if child.returncode is not None else 124
    outbound.join(PUMP_JOIN_SECONDS)
    if outbound.is_alive():
        pump_errors.append("child_output_not_closed")
        _terminate_process_group(child, pump_errors)
        outbound.join(PUMP_JOIN_SECONDS)
    try:
        child.stdout.close()
    except OSError:
        pump_errors.append("child_output_close_failed")
    inbound.join(PUMP_JOIN_SECONDS)
    if inbound.is_alive():
        pump_errors.append("client_input_not_closed")
    document = recorder.document(exit_code, pump_errors)
    try:
        _atomic_write(journal_path, document)
    except OSError as error:
        raise RecorderError("recorder journal could not be committed") from error
    if document["status"] != "complete":
        return exit_code if exit_code != 0 else 70
    return 0


def parse_arguments(arguments: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--metadata", required=True, type=Path)
    parser.add_argument("--journal", required=True, type=Path)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    result = parser.parse_args(arguments)
    if result.command[:1] == ["--"]:
        result.command = result.command[1:]
    if not result.command:
        parser.error("wrapped command is required after --")
    return result


def main(arguments: Sequence[str] | None = None) -> int:
    options = parse_arguments(arguments or sys.argv[1:])
    try:
        metadata = load_metadata(options.metadata)
        return run_proxy(
            options.command,
            metadata=metadata,
            journal_path=options.journal,
            stdin=sys.stdin.buffer,
            stdout=sys.stdout.buffer,
        )
    except RecorderError as error:
        print(f"sprint11 surface recorder: {error}", file=sys.stderr)
        return 64


if __name__ == "__main__":
    raise SystemExit(main())
