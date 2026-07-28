#!/usr/bin/env python3
"""Prove deterministic ownership for two MCP tasks on one Godot project.

The emitted report is bounded and contains only artifact digests and boolean
assertions. Project roots, process identifiers, discovery endpoints, tokens,
and live editor identities never leave the runner.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Mapping, cast

SCRIPT_IMPORT_ROOT = Path(__file__).resolve().parent
if str(SCRIPT_IMPORT_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPT_IMPORT_ROOT))

try:
    from tests.codex import sprint9_model_free_live as s9
    from tests.codex import sprint10_model_free_live as s10
    from tests.codex import sprint11_acquisition_paths as acquisition_paths
    from tests.codex import sprint11_multi_project as multi_project
    from tests.codex import sprint11_packaged_fixture as packaged_fixture
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint9_model_free_live as s9
    import sprint10_model_free_live as s10
    import sprint11_acquisition_paths as acquisition_paths
    import sprint11_multi_project as multi_project
    import sprint11_packaged_fixture as packaged_fixture

REPORT_SCHEMA = "s11-same-project-live/1.0"
CONNECTION_SCHEMA = "godot-connection-status/1.1"
REPORT_LIMIT = 65_536
BUSY_ACTION = (
    "Close the other Codex task for this project, or wait for it to finish."
)
BUSY_PROBE_TOOLS: tuple[tuple[str, dict[str, Any]], ...] = (
    (
        "godot_get_resource_dependencies",
        {"resource": "res://main.tscn", "limit": 1},
    ),
    ("godot_get_open_scenes", {"limit": 1}),
    ("godot_run_project", {}),
    (
        "godot_get_transaction_status",
        {"transaction_id": "change-set:" + "0" * 32},
    ),
)


def _exact(
    value: Any,
    fields: set[str],
    label: str,
) -> dict[str, Any]:
    s9.require(isinstance(value, dict), f"{label} is not an object")
    record = cast(dict[str, Any], value)
    s9.require(set(record) == fields, f"{label} fields differ")
    return record


def _start_sidecar(
    sidecar: Path,
    project_root: Path,
    timeout: float,
) -> tuple[s9.LineProcess, s9.ModelFreeMcpClient]:
    process = s9.LineProcess(
        [
            str(sidecar),
            "--project-root",
            str(project_root),
        ],
        cwd=project_root,
        environment=os.environ.copy(),
    )
    client = s9.ModelFreeMcpClient(
        process,
        project_root=project_root,
        timeout=timeout,
    )
    client.initialize()
    s9.require(
        isinstance(client, s10.Sprint10McpClient),
        "production-equivalent approval client is unavailable",
    )
    return process, client


def _status_code(status: Mapping[str, Any]) -> str | None:
    diagnostic = status.get("diagnostic")
    if not isinstance(diagnostic, dict):
        return None
    code = diagnostic.get("code")
    return code if isinstance(code, str) else None


def _busy_projection_is_exact(status: Mapping[str, Any]) -> bool:
    bridge = status.get("bridge")
    cache = status.get("static_cache")
    components = status.get("components")
    return (
        status.get("schema_version") == CONNECTION_SCHEMA
        and status.get("status") == "project_session_busy"
        and _status_code(status) == "project_session_busy"
        and status.get("remediation_id") == "wait_for_project_session"
        and status.get("next_action") == BUSY_ACTION
        and isinstance(bridge, dict)
        and bridge.get("condition") == "ready"
        and isinstance(cache, dict)
        and cache.get("condition") == "unavailable"
        and cache.get("schema") is None
        and cache.get("generation") is None
        and cache.get("revisions") is None
        and cache.get("source_hashes_verified") is False
        and cache.get("age_seconds") is None
        and isinstance(components, dict)
        and components.get("editor") == "ready"
        and components.get("runtime") == "unavailable"
        and components.get("transactions") == "unavailable"
    )


def _ready_projection_is_exact(status: Mapping[str, Any]) -> bool:
    bridge = status.get("bridge")
    cache = status.get("static_cache")
    components = status.get("components")
    return (
        status.get("schema_version") == CONNECTION_SCHEMA
        and status.get("status") == "ready"
        and isinstance(bridge, dict)
        and bridge.get("condition") == "ready"
        and bridge.get("negotiated_protocol") == "1.8"
        and isinstance(cache, dict)
        and cache.get("condition") == "online_current"
        and cache.get("schema") == "1.3"
        and isinstance(cache.get("generation"), str)
        and isinstance(cache.get("revisions"), dict)
        and isinstance(components, dict)
        and components.get("editor") == "ready"
        and components.get("transactions") == "ready"
    )


def _wait_for_projection(
    client: s9.ModelFreeMcpClient,
    *,
    expected: str,
    timeout: float,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    latest: dict[str, Any] = {}
    while time.monotonic() < deadline:
        latest, is_error, _ = client.tool("godot_get_connection_status", {})
        matches = (
            _busy_projection_is_exact(latest)
            if expected == "project_session_busy"
            else _ready_projection_is_exact(latest)
        )
        if not is_error and matches:
            return latest
        time.sleep(0.05)
    raise s9.WorkflowError(
        f"same-project {expected} projection did not converge: "
        f"status={latest.get('status')}, code={_status_code(latest)}"
    )


def _assert_busy_tools(client: s9.ModelFreeMcpClient) -> None:
    for name, arguments in BUSY_PROBE_TOOLS:
        result, is_error, _ = client.tool(name, arguments)
        error = result.get("error")
        s9.require(
            is_error
            and isinstance(error, dict)
            and error.get("code") == "project_session_busy"
            and error.get("status") == "project_session_busy"
            and error.get("diagnostic_code") == "project_session_busy"
            and error.get("retryable") is True
            and error.get("remediation_id") == "wait_for_project_session",
            f"{name} did not fail closed for the standby task",
        )


def _assert_transaction_coordinator_ready(
    client: s9.ModelFreeMcpClient,
) -> None:
    result, is_error, _ = client.tool(
        "godot_get_transaction_status",
        {"transaction_id": "change-set:" + "0" * 32},
    )
    error = result.get("error")
    s9.require(
        is_error
        and isinstance(error, dict)
        and error.get("code") == "transaction_not_found",
        "takeover did not activate the transaction coordinator",
    )


def _wait_for_single_owner(
    candidates: tuple[
        tuple[s9.LineProcess, s9.ModelFreeMcpClient],
        tuple[s9.LineProcess, s9.ModelFreeMcpClient],
    ],
    *,
    timeout: float,
) -> tuple[
    tuple[s9.LineProcess, s9.ModelFreeMcpClient],
    tuple[s9.LineProcess, s9.ModelFreeMcpClient],
]:
    deadline = time.monotonic() + timeout
    latest: list[dict[str, Any]] = [{}, {}]
    while time.monotonic() < deadline:
        for index, (_, client) in enumerate(candidates):
            latest[index], is_error, _ = client.tool(
                "godot_get_connection_status",
                {},
            )
            s9.require(not is_error, "same-project status unexpectedly failed")
        ready = [
            index
            for index, status in enumerate(latest)
            if _ready_projection_is_exact(status)
        ]
        busy = [
            index
            for index, status in enumerate(latest)
            if _busy_projection_is_exact(status)
        ]
        if len(ready) == 1 and len(busy) == 1:
            return candidates[ready[0]], candidates[busy[0]]
        time.sleep(0.05)
    raise s9.WorkflowError(
        "same-project competing standbys did not elect exactly one owner: "
        + ", ".join(
            f"status={status.get('status')}, code={_status_code(status)}"
            for status in latest
        )
    )


def _finish_gracefully(process: s9.LineProcess, timeout: float) -> None:
    stdin = process.process.stdin
    s9.require(stdin is not None, "owner stdin is unavailable")
    stdin.close()
    try:
        process.process.wait(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        raise s9.WorkflowError("owner did not exit after MCP EOF") from error
    s9.require(
        process.process.returncode == 0,
        "owner did not stop gracefully after MCP EOF",
    )


def _kill(process: s9.LineProcess, timeout: float) -> None:
    if process.process.poll() is None:
        process.process.kill()
        process.process.wait(timeout=timeout)


def _safe_report(report: Mapping[str, Any]) -> None:
    encoded = json.dumps(
        report,
        allow_nan=False,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )
    s9.require(
        len(encoded.encode("utf-8")) <= REPORT_LIMIT
        and "/tmp/" not in encoded
        and "/Users/" not in encoded
        and "session.token" not in encoded
        and ".sock" not in encoded
        and "project:sha256:" not in encoded
        and "editor:" not in encoded
        and "runtime:" not in encoded
        and "transaction:" not in encoded,
        "same-project report leaked a private binding or exceeded its bound",
    )


def validate_report(value: Any) -> dict[str, Any]:
    root = _exact(
        value,
        {
            "schema_version",
            "status",
            "platform",
            "protocol",
            "bridge_rpc",
            "package_version",
            "artifacts",
            "registry",
            "assertions",
            "source_unchanged",
            "cleanup",
            "redaction",
        },
        "same-project report",
    )
    artifacts = _exact(
        root["artifacts"],
        {"godot_sha256", "sidecar_sha256"},
        "same-project artifacts",
    )
    registry = _exact(
        root["registry"],
        {"tools", "digest"},
        "same-project registry",
    )
    assertions = _exact(
        root["assertions"],
        {
            "exactly_one_owner",
            "busy_not_syncing",
            "busy_projection_fail_closed",
            "diagnostic_only_standby",
            "graceful_takeover",
            "crash_takeover",
            "transaction_coordinator_follows_index_lease",
        },
        "same-project assertions",
    )
    cleanup = _exact(
        root["cleanup"],
        {"editor_stopped", "owner_stopped", "standby_stopped", "successor_stopped"},
        "same-project cleanup",
    )
    s9.require(
        root["schema_version"] == REPORT_SCHEMA
        and root["status"] == "passed"
        and root["platform"] == "macos-arm64"
        and root["protocol"] == "2025-11-25"
        and root["bridge_rpc"] == "1.8"
        and isinstance(root["package_version"], str)
        and bool(root["package_version"])
        and all(
            isinstance(artifacts[field], str)
            and artifacts[field].startswith("sha256:")
            and len(artifacts[field]) == 71
            for field in artifacts
        )
        and registry["tools"] == 41
        and isinstance(registry["digest"], str)
        and registry["digest"].startswith("sha256:")
        and len(registry["digest"]) == 71
        and all(value is True for value in assertions.values())
        and root["source_unchanged"] is True
        and all(value is True for value in cleanup.values())
        and root["redaction"] is True,
        "same-project proof differs",
    )
    _safe_report(root)
    return root


def run_live(
    godot: Path,
    sidecar: Path,
    timeout: float,
    *,
    expected_sidecar_sha256: str,
    expected_package_version: str,
) -> dict[str, Any]:
    observed_sidecar_sha256 = multi_project._artifact_digest(sidecar)
    observed_package_version = multi_project._probe_sidecar_version(sidecar, timeout)
    multi_project._require_package_authority(
        sidecar,
        expected_sha256=expected_sidecar_sha256,
        expected_version=expected_package_version,
        observed_sha256=observed_sidecar_sha256,
        observed_version=observed_package_version,
    )
    profile = multi_project._install_sprint11_client()
    session = s9.FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    owner: s9.LineProcess | None = None
    standby: s9.LineProcess | None = None
    successor: s9.LineProcess | None = None
    source_unchanged = False
    proof_complete = False
    try:
        with multi_project._managed_fixture(session):
            s9.require(
                session.project_root is not None
                and session.client is not None
                and session.sidecar_process is not None,
                "same-project owner fixture is incomplete",
            )
            project_root = session.project_root
            owner = session.sidecar_process
            source_before = multi_project._project_source_hashes(project_root)

            standby, standby_client = _start_sidecar(
                sidecar,
                project_root,
                timeout,
            )
            successor, successor_client = _start_sidecar(
                sidecar,
                project_root,
                timeout,
            )
            _wait_for_projection(
                standby_client,
                expected="project_session_busy",
                timeout=min(timeout, 5.0),
            )
            _wait_for_projection(
                successor_client,
                expected="project_session_busy",
                timeout=min(timeout, 5.0),
            )
            _assert_busy_tools(standby_client)
            _assert_busy_tools(successor_client)

            _finish_gracefully(owner, min(timeout, 10.0))
            winner, waiting = _wait_for_single_owner(
                (
                    (standby, standby_client),
                    (successor, successor_client),
                ),
                timeout=timeout,
            )
            winner_process, winner_client = winner
            waiting_process, waiting_client = waiting
            _assert_transaction_coordinator_ready(winner_client)
            _kill(winner_process, min(timeout, 10.0))
            _wait_for_projection(
                waiting_client,
                expected="ready",
                timeout=timeout,
            )
            _assert_transaction_coordinator_ready(waiting_client)
            s9.require(
                waiting_process.process.poll() is None,
                "surviving standby exited during crash takeover",
            )
            source_unchanged = (
                multi_project._project_source_hashes(project_root) == source_before
            )
            proof_complete = True
    finally:
        for process in (successor, standby, owner):
            if process is not None:
                process.stop()

    report = {
        "schema_version": REPORT_SCHEMA,
        "status": "passed",
        "platform": "macos-arm64",
        "protocol": "2025-11-25",
        "bridge_rpc": "1.8",
        "package_version": observed_package_version,
        "artifacts": {
            "godot_sha256": multi_project._artifact_digest(godot),
            "sidecar_sha256": observed_sidecar_sha256,
        },
        "registry": {
            "tools": profile["tool_count"],
            "digest": "sha256:" + str(profile["digest"]),
        },
        "assertions": {
            "exactly_one_owner": proof_complete,
            "busy_not_syncing": proof_complete,
            "busy_projection_fail_closed": proof_complete,
            "diagnostic_only_standby": proof_complete,
            "graceful_takeover": proof_complete,
            "crash_takeover": proof_complete,
            "transaction_coordinator_follows_index_lease": proof_complete,
        },
        "source_unchanged": source_unchanged,
        "cleanup": {
            "editor_stopped": session.editor is None
            or session.editor.poll() is not None,
            "owner_stopped": owner is None or owner.process.poll() is not None,
            "standby_stopped": standby is None
            or standby.process.poll() is not None,
            "successor_stopped": successor is None
            or successor.process.poll() is not None,
        },
        "redaction": True,
    }
    return validate_report(report)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", type=Path, required=True)
    parser.add_argument("--sidecar", type=Path, required=True)
    parser.add_argument("--expected-sidecar-sha256", required=True)
    parser.add_argument("--expected-package-version", required=True)
    parser.add_argument("--additive-sprint11-registry", action="store_true")
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--output", type=Path)
    packaged_fixture.add_arguments(parser)
    arguments = parser.parse_args()
    try:
        packaged_fixture.activate_from_arguments(arguments)
        godot = arguments.godot.resolve(strict=True)
        sidecar = arguments.sidecar.resolve(strict=True)
        report = run_live(
            godot,
            sidecar,
            arguments.timeout,
            expected_sidecar_sha256=arguments.expected_sidecar_sha256,
            expected_package_version=arguments.expected_package_version,
        )
        encoded = json.dumps(
            report,
            ensure_ascii=False,
            indent=2,
            allow_nan=False,
            sort_keys=True,
        ) + "\n"
        if arguments.output is not None:
            acquisition_paths.atomic_write_new_file(
                arguments.output,
                encoded.encode("utf-8"),
                prefix=".s11-same-project-report.",
            )
        print(encoded, end="")
        return 0
    except (
        OSError,
        json.JSONDecodeError,
        subprocess.TimeoutExpired,
        s9.WorkflowError,
        acquisition_paths.AcquisitionPathError,
        packaged_fixture.PackagedFixtureError,
    ) as error:
        print(f"Sprint 11 same-project gate failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
