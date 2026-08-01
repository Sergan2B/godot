from __future__ import annotations

import copy
import io
import json
import os
import stat
import subprocess
import tempfile
import unittest
from collections import deque
from pathlib import Path
from typing import Any
from unittest import mock

from tests.codex import sprint11_acceptance as acceptance
from tests.codex import sprint11_surface_artifacts as artifacts
from tests.codex.test_sprint11_acceptance import synthetic_trace


class FakeTTY:
    def __init__(self, lines: list[str], *, terminal: bool = True) -> None:
        self.lines = deque(line + "\n" for line in lines)
        self.terminal = terminal
        self.prompts: list[str] = []
        self.closed = False

    def isatty(self) -> bool:
        return self.terminal

    def write(self, value: str) -> int:
        self.prompts.append(value)
        return len(value)

    def flush(self) -> None:
        return None

    def readline(self) -> str:
        return self.lines.popleft() if self.lines else ""

    def close(self) -> None:
        self.closed = True

    def __enter__(self) -> FakeTTY:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()


def _git_blob(
    commit: str,
    relative: str,
    *,
    repository: Path = artifacts.REPOSITORY_ROOT,
) -> bytes:
    return subprocess.run(
        [
            "/usr/bin/git",
            "--no-replace-objects",
            "-C",
            str(repository),
            "cat-file",
            "blob",
            f"{commit}:{relative}",
        ],
        check=True,
        stdout=subprocess.PIPE,
    ).stdout


def _digest(value: bytes) -> str:
    return artifacts.sha256_bytes(value)


def _signature(value: Any) -> dict[str, Any] | None:
    if value is None:
        return None
    return {**value, "verified": True}


class PreparedInputs:
    def __init__(self, root: Path) -> None:
        self.root = root
        base_commit = subprocess.run(
            [
                "/usr/bin/git",
                "-C",
                str(artifacts.REPOSITORY_ROOT),
                "rev-parse",
                "HEAD",
            ],
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        ).stdout.strip()
        self.repository_root = root / "source-repository"
        self.source_commit = self._write_source_repository(base_commit)
        self.registry = _git_blob(
            self.source_commit,
            artifacts.REGISTRY_PATH,
            repository=self.repository_root,
        )
        self.matrix = _git_blob(
            self.source_commit,
            artifacts.MATRIX_PATH,
            repository=self.repository_root,
        )
        self.profile = _git_blob(
            self.source_commit,
            artifacts.PROFILE_PATH,
            repository=self.repository_root,
        )
        self.instructions = _git_blob(
            self.source_commit,
            artifacts.INSTRUCTIONS_PATH,
            repository=self.repository_root,
        )
        self.prompt = _git_blob(
            self.source_commit,
            artifacts.PROMPT_PATH,
            repository=self.repository_root,
        )
        self.matrix_document = json.loads(self.matrix)
        self.profile_document = json.loads(self.profile)
        self.version = self.matrix_document["package"]["version"]
        self.data_root = root / "data"
        self.package_root = (
            self.data_root / "versions" / self.version
        )
        self.package_root.mkdir(parents=True)
        self.fixture_root = root / "project"
        self.fixture_root.mkdir()
        self._write_fixture()
        self._write_package()
        self.measurement = root / "measurement.json"
        self._write_measurement()

    def _write_source_repository(self, base_commit: str) -> str:
        fixture_manifest = json.loads(
            _git_blob(base_commit, artifacts.FIXTURE_MANIFEST_PATH)
        )
        tracked_paths = {
            artifacts.REGISTRY_PATH,
            artifacts.MATRIX_PATH,
            artifacts.PROFILE_PATH,
            artifacts.INSTRUCTIONS_PATH,
            artifacts.PROMPT_PATH,
            artifacts.FIXTURE_MANIFEST_PATH,
            *(
                f"{artifacts.FIXTURE_SOURCE_ROOT}/{record['path']}"
                for record in fixture_manifest["files"]
            ),
        }
        source_payloads = {
            relative: _git_blob(base_commit, relative)
            for relative in tracked_paths
        }
        for relative in artifacts.CAPTURE_SCHEMA_PACKAGE_PATHS:
            source_payloads[relative] = (
                artifacts.REPOSITORY_ROOT.joinpath(
                    *Path(relative).parts
                ).read_bytes()
            )
        self.repository_root.mkdir()
        for relative, payload in source_payloads.items():
            destination = self.repository_root.joinpath(
                *Path(relative).parts
            )
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(payload)
        subprocess.run(
            ["/usr/bin/git", "init", "--quiet", str(self.repository_root)],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        subprocess.run(
            [
                "/usr/bin/git",
                "-C",
                str(self.repository_root),
                "add",
                "--all",
            ],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        environment = {
            **os.environ,
            "GIT_AUTHOR_DATE": "2000-01-01T00:00:00Z",
            "GIT_COMMITTER_DATE": "2000-01-01T00:00:00Z",
        }
        subprocess.run(
            [
                "/usr/bin/git",
                "-c",
                "user.name=Surface Fixture",
                "-c",
                "user.email=surface-fixture@example.invalid",
                "-C",
                str(self.repository_root),
                "commit",
                "--quiet",
                "-m",
                "surface fixture",
            ],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            env=environment,
        )
        return subprocess.run(
            [
                "/usr/bin/git",
                "-C",
                str(self.repository_root),
                "rev-parse",
                "HEAD",
            ],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        ).stdout.strip()

    def _write_fixture(self) -> None:
        manifest = json.loads(
            _git_blob(
                self.source_commit,
                artifacts.FIXTURE_MANIFEST_PATH,
                repository=self.repository_root,
            )
        )
        for record in manifest["files"]:
            path = self.fixture_root / record["path"]
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(
                _git_blob(
                    self.source_commit,
                    f"{artifacts.FIXTURE_SOURCE_ROOT}/{record['path']}",
                    repository=self.repository_root,
                )
            )
        config = self.fixture_root / ".codex" / "config.toml"
        config.parent.mkdir()
        config.write_text(
            "[mcp_servers.godot_editor]\n"
            "command = \"/private/not-retained/godot-codex-mcp\"\n"
            "args = [\"--project-root\", \".\"]\n",
            encoding="utf-8",
        )
        receipt = {
            "schema_version": "godot-codex-setup-receipt/1.1",
            "package_version": self.version,
            "launcher_file_sha256": "",
        }
        receipt_path = (
            self.fixture_root
            / ".godot"
            / "codex"
            / "setup-receipt-v1.json"
        )
        receipt_path.parent.mkdir(parents=True)
        self.receipt = receipt
        self.receipt_path = receipt_path

    def _write_file(
        self,
        relative: str,
        payload: bytes,
        mode: int,
    ) -> dict[str, Any]:
        path = self.package_root.joinpath(*Path(relative).parts)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(payload)
        path.chmod(mode)
        return {
            "path": relative,
            "sha256": _digest(payload),
            "bytes": len(payload),
            "mode": f"{mode:04o}",
        }

    def _write_package(self) -> None:
        cli = b"test-package-cli\n"
        mcp = b"test-package-mcp\n"
        files = {
            "SOURCE_COMMIT": (self.source_commit + "\n").encode(),
            "VERSION": (self.version + "\n").encode(),
            "bin/godot-codex": cli,
            "bin/godot-codex-mcp": mcp,
            "share/godot-codex/product/registry-profile.v1.json":
                self.registry,
            "share/godot-codex/product/compatibility-matrix.v1.json":
                self.matrix,
            "share/godot-codex/product/host-coordinate-profile.v1.json":
                self.profile,
            "share/godot-codex/product/server-instructions.v1.txt":
                self.instructions,
            **{
                package_path: _git_blob(
                    self.source_commit,
                    source_path,
                    repository=self.repository_root,
                )
                for (
                    source_path,
                    package_path,
                ) in artifacts.CAPTURE_SCHEMA_PACKAGE_PATHS.items()
            },
        }
        records = [
            self._write_file(
                relative,
                payload,
                0o755 if relative.startswith("bin/") else 0o644,
            )
            for relative, payload in sorted(files.items())
        ]
        godot = self.matrix_document["godot"]["artifact_sha256"]
        internal_document = {
            "schema_version": "godot-codex-package/1.0",
            "package_version": self.version,
            "source_commit": self.source_commit,
            "target": {"architecture": "arm64", "os": "macos"},
            "build_provenance": {},
            "godot_prerequisite": {"sha256": f"sha256:{godot}"},
            "compatibility_matrix_sha256": _digest(self.matrix),
            "registry_sha256": _digest(self.registry),
            "third_party_licenses_sha256": "sha256:" + "0" * 64,
            "contents": records,
            "checksums_path": "checksums.sha256",
        }
        internal_manifest = artifacts.canonical_artifact(
            internal_document
        )
        records.append(
            self._write_file(
                "package-manifest.json",
                internal_manifest,
                0o644,
            )
        )
        marker = self.package_root / ".godot-codex-owned"
        marker.write_text(
            _digest(internal_manifest).removeprefix("sha256:") + "\n",
            encoding="ascii",
        )
        marker.chmod(0o600)
        self.receipt["launcher_file_sha256"] = _digest(mcp)
        self.receipt_path.write_bytes(
            artifacts.canonical_artifact(self.receipt)
        )
        self.detached = {
            "archive": {},
            "build_provenance": {},
            "compatibility_matrix_sha256": _digest(self.matrix),
            "contents": records,
            "godot_prerequisite": {"sha256": f"sha256:{godot}"},
            "package_version": self.version,
            "registry_sha256": _digest(self.registry),
            "schema_version": "s11-package-manifest/1.0",
            "source_commit": self.source_commit,
            "third_party_licenses_sha256": "sha256:" + "0" * 64,
        }
        self.package_manifest = self.root / "detached-manifest.json"
        self.package_manifest.write_bytes(
            artifacts.canonical_artifact(self.detached)
        )

    def _write_measurement(self) -> None:
        measurements = []
        for coordinate in self.profile_document["surfaces"]:
            measurements.append(
                {
                    "surface": coordinate["surface"],
                    "host_artifact_sha256": coordinate[
                        "host_artifact_sha256"
                    ],
                    "host_metadata_sha256": coordinate[
                        "host_metadata_sha256"
                    ],
                    "client_artifact_sha256": coordinate[
                        "client_artifact_sha256"
                    ],
                    "ide_shell_artifact_sha256": coordinate[
                        "ide_shell_artifact_sha256"
                    ],
                    "host_code_signature": _signature(
                        coordinate["host_code_signature"]
                    ),
                    "client_code_signature": _signature(
                        coordinate["client_code_signature"]
                    ),
                    "ide_shell_code_signature": _signature(
                        coordinate["ide_shell_code_signature"]
                    ),
                }
            )
        value = {
            "schema_version": "s11-host-provenance/1.0",
            "capture_kind": "measured_local_host_provenance",
            "status": "passed",
            "architecture": "arm64",
            "host_coordinate_profile_sha256": _digest(self.profile),
            "surfaces": measurements,
            "redaction": {
                "absolute_paths_absent": True,
                "account_identity_absent": True,
                "artifact_content_absent": True,
                "environment_secrets_absent": True,
            },
        }
        self.measurement.write_bytes(artifacts.canonical_artifact(value))

    def prepare(self, surface: str = "cli") -> dict[str, Any]:
        return artifacts.prepare_metadata(
            surface=surface,
            package_manifest=self.package_manifest,
            data_root=self.data_root,
            measurement_path=self.measurement,
            fixture_root=self.fixture_root,
            repository_root=self.repository_root,
        )


def _semantic_values() -> dict[str, Any]:
    trace = synthetic_trace("cli", "surface-artifacts", 100)
    assertions = {
        f"assertion.{item['assertion_id']}": item["projection"]
        for item in trace["assertions"]
        if item["assertion_id"] in artifacts.TRANSPORT_ASSERTION_IDS
    }
    forms = {
        f"form_outcome.{item['scenario']}": item
        for item in trace["form_outcomes"]
    }
    revisions = {
        f"revision.{item['step']}": item
        for item in trace["revision_timeline"]
    }
    result = {**assertions, **forms, **revisions}
    assert set(result) == artifacts.SEMANTIC_PROJECTION_IDS
    return result


def _raw_event(
    events: list[dict[str, Any]],
    *,
    direction: str,
    event_class: str,
    phase: str,
    method: str | None = None,
    tool: str | None = None,
    registry_names: list[str] | None = None,
    protocol_version: str | None = None,
    instructions_sha256: str | None = None,
    error_code: int | None = None,
    semantic_projection: dict[str, Any] | None = None,
    tool_observation: dict[str, Any] | None = None,
    client_name: str | None = None,
    client_version: str | None = None,
) -> int:
    event: dict[str, Any] = {
        "sequence": len(events) + 1,
        "observed_at_unix_ms": 1_700_000_000_000 + len(events),
        "direction": direction,
        "class": event_class,
        "phase": phase,
    }
    if method is not None:
        event["method"] = method
    if tool is not None:
        event["tool"] = tool
    if registry_names:
        event["registry_names"] = registry_names
    if protocol_version is not None:
        event["protocol_version"] = protocol_version
    if instructions_sha256 is not None:
        event["instructions_sha256"] = instructions_sha256
    if client_name is not None:
        event["client_name"] = client_name
    if client_version is not None:
        event["client_version"] = client_version
    if error_code is not None:
        event["error_code"] = error_code
    if semantic_projection is not None:
        event["semantic_projection"] = semantic_projection
    if tool_observation is not None:
        event["tool_observation"] = tool_observation
    event["previous_event_sha256"] = (
        events[-1]["event_sha256"]
        if events
        else "sha256:" + "0" * 64
    )
    event["event_sha256"] = artifacts._raw_event_hash(event)
    events.append(event)
    return event["sequence"]


def _tool_exchange(
    events: list[dict[str, Any]],
    *,
    tool: str,
    request: dict[str, Any],
    result: dict[str, Any],
    error: bool = False,
) -> int:
    event_class = (
        "status" if tool == "godot_get_connection_status" else "tool"
    )
    _raw_event(
        events,
        direction="client_to_server",
        event_class=event_class,
        phase="request",
        method="tools/call",
        tool=tool,
    )
    return _raw_event(
        events,
        direction="server_to_client",
        event_class="error" if error else event_class,
        phase="error" if error else "response",
        method="tools/call",
        tool=tool,
        error_code=-32603 if error else None,
        tool_observation={"request": request, "result": result},
    )


def _form_exchange(
    events: list[dict[str, Any]],
    *,
    transaction_id: str,
    action: str,
) -> int:
    _raw_event(
        events,
        direction="server_to_client",
        event_class="form",
        phase="request",
        method="elicitation/create",
    )
    return _raw_event(
        events,
        direction="client_to_server",
        event_class="form",
        phase="response",
        method="elicitation/create",
        tool_observation={
            "request": {
                "parent_tool": "godot_apply_transaction",
                "transaction_id": transaction_id,
            },
            "result": {
                "action": action,
                "content_recorded": False,
            },
        },
    )


def raw_capture(
    metadata: dict[str, Any],
    metadata_payload: bytes,
) -> tuple[dict[str, Any], bytes]:
    semantic = _semantic_values()
    connection = semantic["assertion.connection.status"]
    saved = semantic["assertion.saved.current_scene"]
    offline = semantic["assertion.offline.saved_query"]
    runtime = semantic["assertion.runtime.error_stack"]
    preview = semantic["assertion.transaction.preview"]
    applied = semantic["assertion.transaction.apply"]
    undone = semantic["assertion.transaction.undo"]
    validation = semantic["assertion.validation.result"]
    revisions = {
        semantic[f"revision.{step}"]["step"]: semantic[f"revision.{step}"]
        for step in artifacts.REVISION_STEPS
    }
    events: list[dict[str, Any]] = []
    _raw_event(
        events,
        direction="server_to_client",
        event_class="initialize",
        phase="response",
        method="initialize",
        protocol_version="2025-11-25",
        client_name="codex",
        client_version="1.0.0",
        instructions_sha256=metadata["registry"]["instructions_sha256"],
    )
    _raw_event(
        events,
        direction="server_to_client",
        event_class="registry",
        phase="response",
        method="tools/list",
        registry_names=metadata["registry"]["tools"],
    )
    _raw_event(
        events,
        direction="server_to_client",
        event_class="registry",
        phase="response",
        method="resources/list",
        registry_names=metadata["registry"]["fixed_resources"],
    )
    _raw_event(
        events,
        direction="server_to_client",
        event_class="registry",
        phase="response",
        method="resources/templates/list",
        registry_names=metadata["registry"]["resource_templates"],
    )

    _tool_exchange(
        events,
        tool="godot_get_connection_status",
        request={},
        result={
            "status": "ready",
            "project_scope": connection["project_id"],
            "remediation_id": "none",
        },
    )
    initial = revisions["initial"]
    current_scene_result = {
        "project_id": saved["project_id"],
        "editor_session_id": initial["editor_session_id"],
        "snapshot_id": saved["evidence_id"],
        "revision_vector": {
            "editor_session_id": initial["editor_session_id"],
            "event_seq": initial["event_seq"],
            "operation_seq": initial["operation_seq"],
            "scene_revisions": {
                saved["scene_id"]: initial["scene_revision"]
            },
        },
        "status": "ready",
        "freshness": "current",
        "scene": {"scene_id": saved["scene_id"]},
        "nodes": [
            {
                "scene_node_id": saved["scene_node_id"],
                "godot_type": "Node",
            }
        ],
        "facts": [],
    }
    _tool_exchange(
        events,
        tool="godot_get_connection_status",
        request={},
        result={
            "status": "offline_cached",
            "project_scope": connection["project_id"],
            "remediation_id": "start_matching_editor",
        },
    )
    _tool_exchange(
        events,
        tool="godot_get_scene_graph",
        request={"scene": "res://main.tscn", "limit": 50},
        result={
            "project_id": offline["project_id"],
            "freshness": "offline_cached",
            "offline_cached": True,
            "status": "ready",
            "scene": {"scene_id": offline["scene_id"]},
            "nodes": [],
            "next_cursor": offline["cursor"],
        },
    )

    _tool_exchange(
        events,
        tool="godot_get_current_scene",
        request={},
        result=current_scene_result,
    )
    _tool_exchange(
        events,
        tool="godot_run_project",
        request={},
        result={
            "project_id": saved["project_id"],
            "editor_session_id": initial["editor_session_id"],
            "runtime_session_id": runtime["runtime_session_before"],
            "runtime_event_seq": 1,
            "state": "running",
            "origin": "project",
            "target": "main_scene",
        },
    )
    _tool_exchange(
        events,
        tool="godot_stop_project",
        request={
            "runtime_session_id": runtime["runtime_session_before"],
            "expected_runtime_event_seq": 1,
        },
        result={
            "project_id": saved["project_id"],
            "editor_session_id": initial["editor_session_id"],
            "runtime_session_id": runtime["runtime_session_before"],
            "runtime_event_seq": 2,
            "state": "stopped",
            "origin": "project",
            "target": "main_scene",
        },
    )
    _tool_exchange(
        events,
        tool="godot_run_current_scene",
        request={},
        result={
            "project_id": saved["project_id"],
            "editor_session_id": initial["editor_session_id"],
            "runtime_session_id": runtime["runtime_session_after"],
            "runtime_event_seq": 3,
            "state": "running",
            "origin": "editor",
            "target": "current_scene",
        },
    )
    _tool_exchange(
        events,
        tool="godot_get_runtime_tree",
        request={
            "runtime_session_id": runtime["runtime_session_after"],
            "expected_runtime_event_seq": 3,
            "limit": 50,
        },
        result={
            "project_id": saved["project_id"],
            "editor_session_id": initial["editor_session_id"],
            "runtime_session_id": runtime["runtime_session_after"],
            "runtime_event_seq": 3,
            "state": "running",
            "nodes": [
                {"runtime_node_id": runtime["runtime_node_id"]}
            ],
            "truncated": False,
        },
    )
    _tool_exchange(
        events,
        tool="godot_get_stack_trace",
        request={
            "runtime_session_id": runtime["runtime_session_after"],
            "runtime_stack_id": "runtime-stack:fixture",
            "expected_runtime_event_seq": 3,
        },
        result={
            "runtime_session_id": runtime["runtime_session_after"],
            "runtime_event_seq": 3,
            "state": "running",
            "stack": {
                "runtime_stack_id": "runtime-stack:fixture",
                "frames": [
                    {
                        "stack_frame_id": runtime["stack_frame_id"],
                        "error_code": runtime["error_code"],
                        "source": runtime["source"],
                    }
                ],
            },
        },
    )

    accepted_raw = preview["transaction_id"]
    _tool_exchange(
        events,
        tool="godot_prepare_change_set",
        request={
            "project_id": saved["project_id"],
            "coordinates": {
                "editor_session_id": initial["editor_session_id"],
                "event_seq": initial["event_seq"],
                "scene_revision": initial["scene_revision"],
                "operation_seq": initial["operation_seq"],
            },
            "operations": preview["preview"]["operations"],
            "save_scope": {"paths": []},
            "validation_policy": {
                "rollback": "on_required_failure",
                "warnings": "allow",
                "runtime": "skip",
            },
        },
        result={
            "schema_version": "change-set/1.0",
            "change_set_id": accepted_raw,
            "state": "prepared",
            "preview_digest": "sha256:" + "1" * 64,
            "request_digest": "sha256:" + "2" * 64,
            "preview": preview["preview"],
            "risk": preview["risk"],
            "scope": preview["scope"],
            "operation_count": len(preview["preview"]["operations"]),
            "coordinates": {
                "editor_session_id": initial["editor_session_id"],
                "event_seq": initial["event_seq"],
                "scene_revision": initial["scene_revision"],
                "operation_seq": initial["operation_seq"],
            },
        },
    )
    _form_exchange(
        events,
        transaction_id=accepted_raw,
        action="accept",
    )
    _tool_exchange(
        events,
        tool="godot_apply_transaction",
        request={
            "transaction_id": accepted_raw,
            "preview_digest": "sha256:" + "1" * 64,
            "expected_scene_revision": initial["scene_revision"],
            "expected_operation_seq": initial["operation_seq"],
        },
        result={
            "transaction_id": accepted_raw,
            "editor_session_id": applied["editor_session_id"],
            "state": "validating",
            "coordinates": {
                "editor_session_id": applied["editor_session_id"],
                "event_seq": applied["event_seq"],
                "scene_revision": applied["scene_revision"],
                "operation_seq": applied["operation_seq"],
            },
        },
    )
    _tool_exchange(
        events,
        tool="godot_get_transaction_status",
        request={"transaction_id": accepted_raw},
        result={
            "transaction_id": accepted_raw,
            "editor_session_id": applied["editor_session_id"],
            "state": "committed",
            "coordinates": {
                "editor_session_id": applied["editor_session_id"],
                "event_seq": applied["event_seq"],
                "scene_revision": applied["scene_revision"],
                "operation_seq": applied["operation_seq"],
            },
            "outcome": {"status": "passed"},
            "validation_report_id": validation["validation_report_id"],
        },
    )
    _tool_exchange(
        events,
        tool="godot_get_validation_report",
        request={"report_id": validation["validation_report_id"], "page": 0},
        result={
            "schema_version": "validation-report-page/1.0",
            "report_id": validation["validation_report_id"],
            "report_digest": "sha256:" + "3" * 64,
            "page": 0,
            "page_count": 1,
            "content_bytes": 128,
            "content_projection": {
                "transaction_id": accepted_raw,
                "outcome": "passed",
                "checks": [
                    {
                        "check": "intrinsic",
                        "authority": "required",
                        "outcome": "passed",
                    }
                ],
                "diagnostics": {
                    "introduced": [],
                    "introduced_errors": 0,
                    "introduced_warnings": 0,
                },
            },
        },
    )
    _tool_exchange(
        events,
        tool="godot_undo_transaction",
        request={
            "transaction_id": accepted_raw,
            "expected_transaction_seq": 2,
            "expected_scene_revision": applied["scene_revision"],
            "expected_operation_seq": applied["operation_seq"],
        },
        result={
            "transaction_id": accepted_raw,
            "editor_session_id": undone["editor_session_id"],
            "state": "undone",
            "coordinates": {
                "editor_session_id": undone["editor_session_id"],
                "event_seq": undone["event_seq"],
                "scene_revision": undone["scene_revision"],
                "operation_seq": undone["operation_seq"],
            },
        },
    )

    for scenario, error_code in (
        ("decline", "approval_declined"),
        ("cancel", "approval_cancelled"),
    ):
        transaction_id = semantic[f"assertion.approval.{scenario}"][
            "transaction_id"
        ]
        _form_exchange(
            events,
            transaction_id=transaction_id,
            action=scenario,
        )
        _tool_exchange(
            events,
            tool="godot_apply_transaction",
            request={
                "transaction_id": transaction_id,
                "preview_digest": "sha256:" + "4" * 64,
                "expected_scene_revision": applied["scene_revision"],
                "expected_operation_seq": applied["operation_seq"],
            },
            result={
                "is_error": True,
                "error": {
                    "code": error_code,
                    "retryable": False,
                },
            },
            error=True,
        )
    timeout_id = semantic["assertion.approval.timeout"]["transaction_id"]
    _tool_exchange(
        events,
        tool="godot_apply_transaction",
        request={
            "transaction_id": timeout_id,
            "preview_digest": "sha256:" + "5" * 64,
            "expected_scene_revision": applied["scene_revision"],
            "expected_operation_seq": applied["operation_seq"],
        },
        result={
            "is_error": True,
            "error": {
                "code": "approval_timeout",
                "retryable": False,
            },
        },
        error=True,
    )

    restarted = revisions["restarted"]
    restarted_scene = json.loads(json.dumps(current_scene_result))
    restarted_scene["editor_session_id"] = restarted[
        "editor_session_id"
    ]
    restarted_scene["revision_vector"] = {
        "editor_session_id": restarted["editor_session_id"],
        "event_seq": restarted["event_seq"],
        "operation_seq": restarted["operation_seq"],
        "scene_revisions": {
            saved["scene_id"]: restarted["scene_revision"]
        },
    }
    _tool_exchange(
        events,
        tool="godot_get_current_scene",
        request={},
        result=restarted_scene,
    )
    _tool_exchange(
        events,
        tool="godot_apply_transaction",
        request={
            "transaction_id": "transaction:surface-artifacts:stale",
            "preview_digest": "sha256:" + "6" * 64,
            "expected_scene_revision": applied["scene_revision"],
            "expected_operation_seq": applied["operation_seq"],
        },
        result={
            "is_error": True,
            "error": {
                "code": "stale_coordinates",
                "retryable": False,
            },
        },
        error=True,
    )
    foreign = semantic["assertion.multi_project.reject"]
    _tool_exchange(
        events,
        tool="godot_prepare_change_set",
        request={
            "project_id": foreign["foreign_project_id"],
            "coordinates": {},
            "operations": [],
            "save_scope": {"paths": []},
            "validation_policy": {},
        },
        result={
            "is_error": True,
            "error": {
                "code": "wrong_project",
                "retryable": False,
                "remediation_id": "select_bound_project",
                "foreign_project_id": foreign["foreign_project_id"],
            },
        },
        error=True,
    )
    machine = metadata["machine_bindings"]
    journal = {
        "schema_version": artifacts.RAW_JOURNAL_SCHEMA_VERSION,
        "run_id": "1" * 64,
        "transport_origin": "official_host_stdio",
        "surface": metadata["surface"],
        "project_identity": machine["project_identity_sha256"],
        "metadata_sha256": _digest(metadata_payload),
        "package_launcher_sha256": machine[
            "package_launcher_sha256"
        ],
        "project_config_sha256": machine["project_config_sha256"],
        "setup_receipt_sha256": machine["setup_receipt_sha256"],
        "event_capacity": artifacts.MAX_EVENTS,
        "observed_event_count": len(events),
        "dropped_event_count": 0,
        "final_event_sha256": events[-1]["event_sha256"],
        "events": events,
    }
    capture = {
        "schema_version": artifacts.RAW_CAPTURE_SCHEMA_VERSION,
        "outcome": "completed",
        "metadata_file": "metadata.json",
        "metadata_sha256": _digest(metadata_payload),
        "journal": journal,
    }
    return capture, artifacts._compact_json_in_order(capture)


def build_fixture_bundle() -> dict[str, Any]:
    """Build a complete, standalone semantic fixture for cross-suite tests."""
    with tempfile.TemporaryDirectory(
        dir=artifacts.REPOSITORY_ROOT
    ) as directory:
        inputs = PreparedInputs(Path(directory))
        metadata = inputs.prepare()
        metadata_payload = artifacts.canonical_artifact(metadata)
        capture, capture_payload = raw_capture(
            metadata,
            metadata_payload,
        )
        journal = artifacts.canonical_journal_from_raw_capture(
            metadata=metadata,
            metadata_payload=metadata_payload,
            capture=capture,
            capture_payload=capture_payload,
        )
        journal_payload = artifacts.canonical_artifact(journal)
        pending = artifacts.derive_pending_trace(
            metadata=metadata,
            metadata_payload=metadata_payload,
            journal=journal,
            journal_payload=journal_payload,
        )
        pending_payload = artifacts.canonical_artifact(pending)
        final = artifacts._finalize_trace(
            pending,
            observations={
                name: True
                for name in artifacts.AUTHORITY_OBSERVATIONS
            },
        )
    return {
        "metadata": metadata,
        "metadata_payload": metadata_payload,
        "capture": capture,
        "capture_payload": capture_payload,
        "journal": journal,
        "journal_payload": journal_payload,
        "pending": pending,
        "pending_payload": pending_payload,
        "final": final,
        "final_payload": artifacts.canonical_artifact(final),
    }


class Sprint11SurfaceArtifactTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(
            dir=artifacts.REPOSITORY_ROOT
        )
        self.root = Path(self.temporary.name)
        self.inputs = PreparedInputs(self.root)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def prepared(self) -> tuple[dict[str, Any], bytes]:
        metadata = self.inputs.prepare()
        return metadata, artifacts.canonical_artifact(metadata)

    def derived(
        self,
    ) -> tuple[
        dict[str, Any],
        bytes,
        dict[str, Any],
        bytes,
        dict[str, Any],
        bytes,
    ]:
        metadata, metadata_payload = self.prepared()
        capture, capture_payload = raw_capture(metadata, metadata_payload)
        journal = artifacts.canonical_journal_from_raw_capture(
            metadata=metadata,
            metadata_payload=metadata_payload,
            capture=capture,
            capture_payload=capture_payload,
        )
        journal_payload = artifacts.canonical_artifact(journal)
        pending = artifacts.derive_pending_trace(
            metadata=metadata,
            metadata_payload=metadata_payload,
            journal=journal,
            journal_payload=journal_payload,
        )
        return (
            metadata,
            metadata_payload,
            journal,
            journal_payload,
            pending,
            artifacts.canonical_artifact(pending),
        )

    def test_prepare_is_deterministic_and_has_no_host_controls(self) -> None:
        first = self.inputs.prepare()
        second = self.inputs.prepare()
        self.assertEqual(
            artifacts.canonical_artifact(first),
            artifacts.canonical_artifact(second),
        )
        encoded = artifacts.canonical_artifact(first)
        self.assertNotIn(b'"host_controls"', encoded)
        self.assertNotIn(
            os.fsencode(self.inputs.fixture_root),
            encoded,
        )

    def test_prepare_rejects_symlinked_fixture_file(self) -> None:
        project_file = self.inputs.fixture_root / "project.godot"
        original = project_file.read_bytes()
        project_file.unlink()
        target = self.root / "project-target.godot"
        target.write_bytes(original)
        project_file.symlink_to(target)
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "symlink|canonical",
        ):
            self.inputs.prepare()

    def test_prepare_rejects_fixture_and_package_mismatch(self) -> None:
        fixture = self.inputs.fixture_root / "project.godot"
        fixture.write_bytes(fixture.read_bytes() + b"\n")
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "fixture copy differs",
        ):
            self.inputs.prepare()
        fixture.write_bytes(
            _git_blob(
                self.inputs.source_commit,
                f"{artifacts.FIXTURE_SOURCE_ROOT}/project.godot",
                repository=self.inputs.repository_root,
            )
        )
        binary = (
            self.inputs.package_root / "bin" / "godot-codex-mcp"
        )
        binary.write_bytes(b"changed\n")
        binary.chmod(0o755)
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "package content differs",
        ):
            self.inputs.prepare()

    def test_prepare_rejects_each_missing_capture_package_file(self) -> None:
        for package_path in artifacts.CAPTURE_REQUIRED_PACKAGE_MODES:
            changed = copy.deepcopy(self.inputs.detached)
            changed["contents"] = [
                record
                for record in changed["contents"]
                if record["path"] != package_path
            ]
            self.inputs.package_manifest.write_bytes(
                artifacts.canonical_artifact(changed)
            )
            with self.subTest(package_path=package_path):
                with self.assertRaisesRegex(
                    artifacts.SurfaceArtifactError,
                    "capture-required",
                ):
                    self.inputs.prepare()

    def test_prepare_rejects_superseded_tracked_package_manifest(self) -> None:
        superseded = (
            artifacts.REPOSITORY_ROOT
            / "tests"
            / "codex"
            / "acquisition"
            / "sprint11"
            / "package"
            / "sprint11-package-manifest.json"
        ).read_bytes()
        document = json.loads(superseded)
        self.assertEqual(
            document["package_version"],
            "0.1.2",
        )
        self.assertNotEqual(document["source_commit"], self.inputs.source_commit)
        self.inputs.package_manifest.write_bytes(superseded)
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "installed versioned package path is unavailable",
        ):
            self.inputs.prepare()

    def test_prepare_binds_internal_manifest_to_detached_source(self) -> None:
        internal_path = self.inputs.package_root / "package-manifest.json"
        internal = json.loads(internal_path.read_bytes())
        internal["source_commit"] = (
            "82ab6f4ec9dbb1c8f510781ca4dded48e5e4953a"
        )
        internal_payload = artifacts.canonical_artifact(internal)
        internal_path.write_bytes(internal_payload)
        internal_path.chmod(0o644)
        marker = self.inputs.package_root / ".godot-codex-owned"
        marker.write_text(
            _digest(internal_payload).removeprefix("sha256:") + "\n",
            encoding="ascii",
        )
        detached = copy.deepcopy(self.inputs.detached)
        record = next(
            item
            for item in detached["contents"]
            if item["path"] == "package-manifest.json"
        )
        record["sha256"] = _digest(internal_payload)
        record["bytes"] = len(internal_payload)
        self.inputs.package_manifest.write_bytes(
            artifacts.canonical_artifact(detached)
        )
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "identity/source binding",
        ):
            self.inputs.prepare()

    def test_prepare_binds_capture_schema_bytes_to_package_source(self) -> None:
        source_path, package_path = next(
            iter(artifacts.CAPTURE_SCHEMA_PACKAGE_PATHS.items())
        )
        changed_schema = b'{"schema_version":"tampered"}\n'
        installed_schema = self.inputs.package_root.joinpath(
            *Path(package_path).parts
        )
        installed_schema.write_bytes(changed_schema)
        installed_schema.chmod(0o644)

        internal_path = self.inputs.package_root / "package-manifest.json"
        internal = json.loads(internal_path.read_bytes())
        internal_record = next(
            item
            for item in internal["contents"]
            if item["path"] == package_path
        )
        internal_record["sha256"] = _digest(changed_schema)
        internal_record["bytes"] = len(changed_schema)
        internal_payload = artifacts.canonical_artifact(internal)
        internal_path.write_bytes(internal_payload)
        internal_path.chmod(0o644)
        marker = self.inputs.package_root / ".godot-codex-owned"
        marker.write_text(
            _digest(internal_payload).removeprefix("sha256:") + "\n",
            encoding="ascii",
        )

        detached = copy.deepcopy(self.inputs.detached)
        for record in detached["contents"]:
            if record["path"] == package_path:
                record["sha256"] = _digest(changed_schema)
                record["bytes"] = len(changed_schema)
            elif record["path"] == "package-manifest.json":
                record["sha256"] = _digest(internal_payload)
                record["bytes"] = len(internal_payload)
        self.inputs.package_manifest.write_bytes(
            artifacts.canonical_artifact(detached)
        )
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            f"installed package product differs from Git: {source_path}",
        ):
            self.inputs.prepare()

    def test_prepare_rejects_toctou_between_stable_reads(self) -> None:
        original = artifacts._read_regular_once
        calls = 0

        def changing_read(
            path: Path,
            *,
            label: str,
            maximum: int,
        ) -> artifacts._ReadResult:
            nonlocal calls
            result = original(path, label=label, maximum=maximum)
            if path == self.inputs.package_manifest:
                calls += 1
                if calls == 1:
                    path.write_bytes(result.payload + b" ")
            return result

        with mock.patch.object(
            artifacts,
            "_read_regular_once",
            side_effect=changing_read,
        ):
            with self.assertRaisesRegex(
                artifacts.SurfaceArtifactError,
                "stable measurements",
            ):
                self.inputs.prepare()

    def test_projection_contract_is_explicit_and_excludes_host_facts(self) -> None:
        self.assertEqual(len(artifacts.TRANSPORT_ASSERTION_IDS), 14)
        self.assertEqual(len(artifacts.HOST_ATTESTED_ASSERTIONS), 3)
        self.assertEqual(len(artifacts.SEMANTIC_PROJECTION_IDS), 23)
        self.assertNotIn(
            "assertion.host.unsupported_form",
            artifacts.SEMANTIC_PROJECTION_IDS,
        )
        for item in artifacts.HOST_ATTESTED_ASSERTIONS:
            self.assertNotIn(
                f"assertion.{item}",
                artifacts.SEMANTIC_PROJECTION_IDS,
            )
        all_observed_fields = set().union(
            *artifacts.TOOL_REQUEST_FIELDS.values(),
            *artifacts.TOOL_RESULT_FIELDS.values(),
        )
        self.assertTrue(
            all_observed_fields.isdisjoint(
                artifacts.OBSERVATION_SENSITIVE_FIELDS
            )
        )
        for pair in (
            {"next_action_redacted", "next_action_digest"},
            {"query_redacted", "query_digest"},
            {"idempotency_key_redacted", "idempotency_key_digest"},
        ):
            self.assertTrue(pair.issubset(all_observed_fields))
        self.assertIn(
            "content_projection",
            artifacts.TOOL_RESULT_FIELDS[
                "godot_get_validation_report"
            ],
        )
        self.assertNotIn(
            "content",
            artifacts.TOOL_RESULT_FIELDS[
                "godot_get_validation_report"
            ],
        )

    def test_derive_is_deterministic_and_binds_raw_capture(self) -> None:
        (
            metadata,
            metadata_payload,
            journal,
            journal_payload,
            pending,
            pending_payload,
        ) = self.derived()
        capture, capture_payload = raw_capture(metadata, metadata_payload)
        second_journal = artifacts.canonical_journal_from_raw_capture(
            metadata=metadata,
            metadata_payload=metadata_payload,
            capture=capture,
            capture_payload=capture_payload,
        )
        second_pending = artifacts.derive_pending_trace(
            metadata=metadata,
            metadata_payload=metadata_payload,
            journal=second_journal,
            journal_payload=artifacts.canonical_artifact(second_journal),
        )
        self.assertEqual(journal_payload, artifacts.canonical_artifact(second_journal))
        self.assertEqual(pending_payload, artifacts.canonical_artifact(second_pending))
        self.assertEqual(
            journal["transport_origin"]["capture_journal_sha256"],
            _digest(capture_payload),
        )
        self.assertNotIn(b"observed_at_unix_ms", journal_payload)
        self.assertEqual(
            pending["status"],
            artifacts.PENDING_STATUS,
        )

    def test_derive_rejects_missing_and_duplicate_tool_observation(self) -> None:
        metadata, metadata_payload = self.prepared()
        capture, _ = raw_capture(metadata, metadata_payload)
        stack_events = [
            event
            for event in capture["journal"]["events"]
            if event.get("tool") == "godot_get_stack_trace"
            and event.get("tool_observation") is not None
        ]
        self.assertEqual(len(stack_events), 1)
        capture["journal"]["events"].remove(stack_events[0])
        self._rehash_raw(capture)
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "runtime stack",
        ):
            artifacts.canonical_journal_from_raw_capture(
                metadata=metadata,
                metadata_payload=metadata_payload,
                capture=capture,
                capture_payload=artifacts._compact_json_in_order(capture),
            )

    def test_derive_allows_identical_duplicate_post_restart_read(self) -> None:
        metadata, metadata_payload = self.prepared()
        capture, _ = raw_capture(metadata, metadata_payload)
        initial_editor = next(
            event["tool_observation"]["result"]["editor_session_id"]
            for event in capture["journal"]["events"]
            if event.get("tool") == "godot_get_current_scene"
            and event.get("tool_observation") is not None
        )
        restarted = next(
            event
            for event in capture["journal"]["events"]
            if event.get("tool") == "godot_get_current_scene"
            and event.get("tool_observation") is not None
            and event["tool_observation"]["result"]["editor_session_id"]
            != initial_editor
        )
        capture["journal"]["events"].append(copy.deepcopy(restarted))
        self._rehash_raw(capture)
        artifacts.canonical_journal_from_raw_capture(
            metadata=metadata,
            metadata_payload=metadata_payload,
            capture=capture,
            capture_payload=artifacts._compact_json_in_order(capture),
        )

        capture, _ = raw_capture(metadata, metadata_payload)
        conflicting = next(
            copy.deepcopy(event)
            for event in capture["journal"]["events"]
            if event.get("tool") == "godot_get_current_scene"
            and event.get("tool_observation") is not None
            and event["tool_observation"]["result"]["editor_session_id"]
            != initial_editor
        )
        conflicting_result = conflicting["tool_observation"]["result"]
        conflicting_result["editor_session_id"] = (
            "editor-session:conflicting-restart"
        )
        conflicting_result["revision_vector"]["editor_session_id"] = (
            "editor-session:conflicting-restart"
        )
        capture["journal"]["events"].append(conflicting)
        self._rehash_raw(capture)
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "post-apply restarted revision.*ambiguous",
        ):
            artifacts.canonical_journal_from_raw_capture(
                metadata=metadata,
                metadata_payload=metadata_payload,
                capture=capture,
                capture_payload=artifacts._compact_json_in_order(capture),
            )

        capture, _ = raw_capture(metadata, metadata_payload)
        stack_events = [
            event
            for event in capture["journal"]["events"]
            if event.get("tool") == "godot_get_stack_trace"
            and event.get("tool_observation") is not None
        ]
        capture["journal"]["events"].append(copy.deepcopy(stack_events[0]))
        self._rehash_raw(capture)
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "runtime stack.*ambiguous",
        ):
            artifacts.canonical_journal_from_raw_capture(
                metadata=metadata,
                metadata_payload=metadata_payload,
                capture=capture,
                capture_payload=artifacts._compact_json_in_order(capture),
            )

    def test_derive_rejects_bad_chain_revision_and_preview_digest(self) -> None:
        metadata, metadata_payload = self.prepared()
        capture, _ = raw_capture(metadata, metadata_payload)
        capture["journal"]["events"][1]["previous_event_sha256"] = (
            "sha256:" + "f" * 64
        )
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "chain|hash",
        ):
            artifacts.canonical_journal_from_raw_capture(
                metadata=metadata,
                metadata_payload=metadata_payload,
                capture=capture,
                capture_payload=artifacts._compact_json_in_order(capture),
            )

        capture, _ = raw_capture(metadata, metadata_payload)
        committed = self._tool_observation(
            capture,
            "godot_get_transaction_status",
            state="committed",
        )
        committed["result"]["coordinates"]["event_seq"] = 100
        self._rehash_raw(capture)
        with self.assertRaises(
            (acceptance.AcceptanceError, artifacts.SurfaceArtifactError)
        ):
            artifacts.canonical_journal_from_raw_capture(
                metadata=metadata,
                metadata_payload=metadata_payload,
                capture=capture,
                capture_payload=artifacts._compact_json_in_order(capture),
            )

        capture, _ = raw_capture(metadata, metadata_payload)
        committed = self._tool_observation(
            capture,
            "godot_apply_transaction",
            state="validating",
        )
        committed["request"]["preview_digest"] = "sha256:" + "f" * 64
        self._rehash_raw(capture)
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "preview digest",
        ):
            artifacts.canonical_journal_from_raw_capture(
                metadata=metadata,
                metadata_payload=metadata_payload,
                capture=capture,
                capture_payload=artifacts._compact_json_in_order(capture),
            )

    def test_explicit_projection_is_only_an_exact_redundant_crosscheck(
        self,
    ) -> None:
        metadata, metadata_payload = self.prepared()
        capture, _ = raw_capture(metadata, metadata_payload)
        ready_event = next(
            event
            for event in capture["journal"]["events"]
            if event.get("tool") == "godot_get_connection_status"
            and event.get("tool_observation", {})
            .get("result", {})
            .get("status")
            == "ready"
        )
        _raw_event(
            capture["journal"]["events"],
            direction="server_to_client",
            event_class="semantic_projection",
            phase="projection",
            semantic_projection={
                "projection_id": "assertion.connection.status",
                "source_event_seq": ready_event["sequence"],
                "value": _semantic_values()[
                    "assertion.connection.status"
                ],
            },
        )
        self._rehash_raw(capture)
        artifacts.canonical_journal_from_raw_capture(
            metadata=metadata,
            metadata_payload=metadata_payload,
            capture=capture,
            capture_payload=artifacts._compact_json_in_order(capture),
        )
        capture["journal"]["events"][-1]["semantic_projection"]["value"][
            "state"
        ] = "offline"
        self._rehash_raw(capture)
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "independent tool observation",
        ):
            artifacts.canonical_journal_from_raw_capture(
                metadata=metadata,
                metadata_payload=metadata_payload,
                capture=capture,
                capture_payload=artifacts._compact_json_in_order(capture),
            )

    def test_canonical_revalidation_rejects_projection_observation_and_source_tamper(
        self,
    ) -> None:
        (
            metadata,
            metadata_payload,
            journal,
            _journal_payload,
            _pending,
            _pending_payload,
        ) = self.derived()

        projection_tamper = copy.deepcopy(journal)
        connection = next(
            item
            for item in projection_tamper["semantic_projections"]
            if item["projection_id"] == "assertion.connection.status"
        )
        connection["value"]["state"] = "offline"
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "independent tool observation",
        ):
            artifacts.validate_journal(
                projection_tamper,
                metadata=metadata,
                metadata_sha256=_digest(metadata_payload),
            )

        observation_tamper = copy.deepcopy(journal)
        stack_event = next(
            event
            for event in observation_tamper["events"]
            if event.get("tool") == "godot_get_stack_trace"
            and event.get("tool_observation") is not None
        )
        stack_event["tool_observation"]["result"]["stack"]["frames"][0][
            "source"
        ]["line"] += 1
        observation_tamper["integrity"]["event_chain_sha256"] = (
            artifacts.recorder_event_chain_sha256(
                observation_tamper["events"]
            )
        )
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "independent tool observation",
        ):
            artifacts.validate_journal(
                observation_tamper,
                metadata=metadata,
                metadata_sha256=_digest(metadata_payload),
            )

        source_tamper = copy.deepcopy(journal)
        connection = next(
            item
            for item in source_tamper["semantic_projections"]
            if item["projection_id"] == "assertion.connection.status"
        )
        original_source = connection["source_event_seq"]
        alternate_source = next(
            event["seq"]
            for event in source_tamper["events"]
            if event["kind"] == "response"
            and event["seq"] != original_source
        )
        connection["source_event_seq"] = alternate_source
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "independent tool observation",
        ):
            artifacts.validate_journal(
                source_tamper,
                metadata=metadata,
                metadata_sha256=_digest(metadata_payload),
            )

    def test_safe_scan_rejects_observation_content_and_absolute_path(self) -> None:
        metadata, metadata_payload = self.prepared()
        capture, _ = raw_capture(metadata, metadata_payload)
        ready = self._tool_observation(
            capture,
            "godot_get_connection_status",
            status="ready",
        )
        ready["result"]["components"] = {
            "source_content": "extends Node"
        }
        self._rehash_raw(capture)
        with self.assertRaises(
            (acceptance.AcceptanceError, artifacts.SurfaceArtifactError)
        ):
            artifacts.canonical_journal_from_raw_capture(
                metadata=metadata,
                metadata_payload=metadata_payload,
                capture=capture,
                capture_payload=artifacts._compact_json_in_order(capture),
            )

        capture, _ = raw_capture(metadata, metadata_payload)
        stack = self._tool_observation(
            capture,
            "godot_get_stack_trace",
        )
        stack["result"]["stack"]["frames"][0]["source"]["path"] = (
            "/Users/alice/private.gd"
        )
        self._rehash_raw(capture)
        with self.assertRaises(
            (acceptance.AcceptanceError, artifacts.SurfaceArtifactError)
        ):
            artifacts.canonical_journal_from_raw_capture(
                metadata=metadata,
                metadata_payload=metadata_payload,
                capture=capture,
                capture_payload=artifacts._compact_json_in_order(capture),
            )

    def test_attestation_requires_tty_and_rejects_batch_switch(self) -> None:
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "controlling TTY",
        ):
            artifacts.collect_attestation(FakeTTY([], terminal=False))
        with mock.patch("sys.stderr", new=io.StringIO()):
            with self.assertRaises(SystemExit):
                artifacts.parser().parse_args(
                    [
                        "attest",
                        "--metadata",
                        "/tmp/m",
                        "--journal",
                        "/tmp/j",
                        "--trace",
                        "/tmp/t",
                        "--repository-root",
                        "/tmp/r",
                        "--output-directory",
                        "/tmp/o",
                        "--yes",
                    ]
                )

    def test_project_trust_requires_personal_manual_attestation(self) -> None:
        trust_observation = "project_trust_reviewed_and_accepted"
        self.assertIn(
            trust_observation,
            artifacts.AUTHORITY_OBSERVATIONS,
        )
        self.assertEqual(
            artifacts.OBSERVATION_PROMPTS[trust_observation],
            "Did you personally review that exact project root in the "
            "official host and manually accept its Trust request?",
        )
        trust_index = artifacts.AUTHORITY_OBSERVATIONS.index(
            trust_observation
        )
        declined = [
            "operator-1",
            *(["yes"] * trust_index),
            "no",
        ]
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "attestation declined: project_trust_reviewed_and_accepted",
        ):
            artifacts.collect_attestation(FakeTTY(declined))

        _operator, observations = artifacts.collect_attestation(
            FakeTTY(
                [
                    "operator-1",
                    *(["yes"] * len(artifacts.AUTHORITY_OBSERVATIONS)),
                ]
            )
        )
        self.assertIs(observations[trust_observation], True)

    def test_decline_and_eof_publish_nothing(self) -> None:
        values = self.derived()
        publication_root, output = self._publication_root()
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "declined",
        ):
            artifacts.publish_attested_surface(
                repository_root=publication_root,
                output_directory=output,
                metadata=values[0],
                metadata_payload=values[1],
                journal=values[2],
                journal_payload=values[3],
                pending=values[4],
                pending_payload=values[5],
                tty=FakeTTY(["operator-1", "no"]),
            )
        self.assertFalse(output.exists())
        self.assertFalse(output.parent.exists())
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "before completion",
        ):
            artifacts.publish_attested_surface(
                repository_root=publication_root,
                output_directory=output,
                metadata=values[0],
                metadata_payload=values[1],
                journal=values[2],
                journal_payload=values[3],
                pending=values[4],
                pending_payload=values[5],
                tty=FakeTTY(["operator-1", "yes"]),
            )
        self.assertFalse(output.exists())
        self.assertFalse(output.parent.exists())

    def test_binding_roundtrip_publication_and_existing_output(self) -> None:
        values = self.derived()
        publication_root, output = self._publication_root()
        tty = FakeTTY(
            ["operator-1", *(["yes"] * len(artifacts.AUTHORITY_OBSERVATIONS))]
        )
        result = artifacts.publish_attested_surface(
            repository_root=publication_root,
            output_directory=output,
            metadata=values[0],
            metadata_payload=values[1],
            journal=values[2],
            journal_payload=values[3],
            pending=values[4],
            pending_payload=values[5],
            tty=tty,
        )
        self.assertEqual(result["status"], "published")
        self.assertEqual(stat.S_IMODE(output.parent.stat().st_mode), 0o700)
        final = json.loads((output / "trace.json").read_bytes())
        authority = json.loads((output / "authority.json").read_bytes())
        journal = (output / "recorder-journal.json").read_bytes()
        self.assertEqual(
            len(final["assertions"]),
            len(artifacts.FINAL_ASSERTION_IDS),
        )
        self.assertEqual(
            authority["bindings"]["trace_sha256"],
            _digest((output / "trace.json").read_bytes()),
        )
        self.assertEqual(
            authority["bindings"]["recorder_journal_sha256"],
            _digest(journal),
        )
        self.assertIs(
            authority["observations"][
                "project_trust_reviewed_and_accepted"
            ],
            True,
        )
        self.assertEqual(
            final["bindings"]["recorder_journal_sha256"],
            _digest(journal),
        )
        self.assertNotIn("operator-1", (output / "authority.json").read_text())
        with self.assertRaisesRegex(
            artifacts.SurfaceArtifactError,
            "already exists",
        ):
            artifacts.publish_attested_surface(
                repository_root=publication_root,
                output_directory=output,
                metadata=values[0],
                metadata_payload=values[1],
                journal=values[2],
                journal_payload=values[3],
                pending=values[4],
                pending_payload=values[5],
                tty=FakeTTY([]),
            )

    def _publication_root(self) -> tuple[Path, Path]:
        root = self.root / "publication-repository"
        sprint_root = (
            root
            / "tests"
            / "codex"
            / "acquisition"
            / "sprint11"
        )
        sprint_root.mkdir(parents=True)
        return root, sprint_root / "surfaces" / "cli"

    @staticmethod
    def _tool_observation(
        capture: dict[str, Any],
        tool: str,
        **result_fields: Any,
    ) -> dict[str, Any]:
        matches = [
            event["tool_observation"]
            for event in capture["journal"]["events"]
            if event.get("tool") == tool
            and event.get("tool_observation") is not None
            and all(
                event["tool_observation"]["result"].get(field) == value
                for field, value in result_fields.items()
            )
        ]
        assert len(matches) == 1
        return matches[0]

    @staticmethod
    def _rehash_raw(capture: dict[str, Any]) -> None:
        previous = "sha256:" + "0" * 64
        events = capture["journal"]["events"]
        for index, event in enumerate(events):
            event["sequence"] = index + 1
            event["previous_event_sha256"] = previous
            event["event_sha256"] = artifacts._raw_event_hash(event)
            previous = event["event_sha256"]
        capture["journal"]["observed_event_count"] = len(events)
        capture["journal"]["final_event_sha256"] = previous


if __name__ == "__main__":
    unittest.main()
