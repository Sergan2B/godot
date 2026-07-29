#!/usr/bin/env python3
"""Acquire one complete real Sprint 11 surface transport journal.

The workflow keeps one MCP session alive while it observes the closed registry,
the four approval outcomes, an unsupported-form error, and verified offline
cache behavior. Project-content files must remain byte-for-byte unchanged.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Mapping, cast

import sprint9_model_free_live as s9
import sprint10_model_free_live as s10


SCRIPT_DIR = Path(__file__).resolve().parent
MCP_PROTOCOL = "2025-11-25"
SAFE_RUN_ID = re.compile(r"[a-z0-9][a-z0-9._-]{0,63}\Z")
TERMINAL_STATES = {
    "committed",
    "failed",
    "in_doubt",
    "rolled_back",
    "rollback_blocked",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise s9.WorkflowError(message)


def strict_json(path: Path) -> dict[str, Any]:
    return s9.strict_json_text(path.read_text(encoding="utf-8"))


def project_hashes(root: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for path in sorted(root.rglob("*")):
        if not path.is_file() or ".godot" in path.parts:
            continue
        relative = path.relative_to(root).as_posix()
        result[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def wait_for_discovery(
    path: Path,
    *,
    previous_session: str,
    editor: subprocess.Popen[bytes],
    timeout: float,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        if editor.poll() is not None:
            raise s9.WorkflowError(
                f"Godot Editor exited before discovery (code {editor.returncode})"
            )
        try:
            last = strict_json(path)
        except (OSError, s9.WorkflowError):
            time.sleep(0.05)
            continue
        session = last.get("editor_session_id")
        if isinstance(session, str) and session and session != previous_session:
            return last
        time.sleep(0.05)
    raise s9.WorkflowError(f"Godot discovery did not refresh: {last}")


def start_editor(
    godot: Path,
    project_root: Path,
    timeout: float,
) -> tuple[subprocess.Popen[bytes], Path]:
    discovery_path = project_root / ".godot/codex/bridge.json"
    previous_session = ""
    if discovery_path.is_file():
        try:
            previous_session = str(strict_json(discovery_path)["editor_session_id"])
        except (KeyError, OSError, s9.WorkflowError):
            previous_session = ""
    descriptor, log_name = tempfile.mkstemp(prefix="s11-surface-editor-", suffix=".log")
    log_path = Path(log_name)
    environment = os.environ.copy()
    environment["GODOT_CODEX_EVIDENCE_TELEMETRY"] = "1"
    log = os.fdopen(descriptor, "wb", buffering=0)
    try:
        editor = subprocess.Popen(
            [
                str(godot),
                "--editor",
                "--headless",
                "--path",
                str(project_root),
            ],
            cwd=project_root,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    finally:
        log.close()
    wait_for_discovery(
        discovery_path,
        previous_session=previous_session,
        editor=editor,
        timeout=timeout,
    )
    return editor, log_path


def stop_editor(editor: subprocess.Popen[bytes], *, timeout: float) -> None:
    if editor.poll() is not None:
        return
    editor.terminate()
    try:
        editor.wait(timeout=min(timeout, 10.0))
    except subprocess.TimeoutExpired:
        editor.kill()
        editor.wait(timeout=5)


def close_recorder(process: s9.LineProcess, *, timeout: float) -> None:
    stdin = process.process.stdin
    require(stdin is not None, "recorder stdin is unavailable")
    stdin.close()
    try:
        process.process.wait(timeout=min(timeout, 15.0))
    except subprocess.TimeoutExpired as error:
        process.stop()
        raise s9.WorkflowError("recorder did not stop after MCP EOF") from error
    require(
        process.process.returncode == 0,
        "recorder exited unsuccessfully: "
        + " | ".join(process.stderr_tail[-20:]),
    )


def configure_registry() -> None:
    s10._install_sprint10_client(additive_sprint11_registry=True)


def start_recorder(
    *,
    recorder: Path,
    metadata: Path,
    journal: Path,
    sidecar: Path,
    project_root: Path,
    timeout: float,
) -> tuple[s9.LineProcess, s10.Sprint10McpClient]:
    environment = os.environ.copy()
    versioned_package = sidecar.parent.parent
    require(
        versioned_package.parent.name == "versions",
        "sidecar is not in a versioned package",
    )
    environment["GODOT_CODEX_DATA_ROOT"] = str(versioned_package.parent.parent)
    process = s9.LineProcess(
        [
            sys.executable,
            str(recorder),
            "--metadata",
            str(metadata),
            "--journal",
            str(journal),
            "--",
            str(sidecar),
            "--project-root",
            str(project_root),
        ],
        cwd=project_root,
        environment=environment,
    )
    client = s10.Sprint10McpClient(
        process,
        project_root=project_root,
        timeout=timeout,
        supports_form=True,
    )
    client.initialize()
    return process, client


def list_registry(client: s10.Sprint10McpClient) -> None:
    resources_response = client.request("resources/list", {})
    templates_response = client.request("resources/templates/list", {})
    resources = resources_response.get("result", {}).get("resources")
    templates = templates_response.get("result", {}).get("resourceTemplates")
    require(
        isinstance(resources, list) and len(resources) == 4,
        "fixed resource registry differs",
    )
    require(
        isinstance(templates, list) and len(templates) == 1,
        "resource template registry differs",
    )


def wait_for_status(
    client: s10.Sprint10McpClient,
    expected: str,
    timeout: float,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        last, error, _ = client.tool("godot_get_connection_status", {})
        if not error and last.get("status") == expected:
            return last
        time.sleep(0.5)
    raise s9.WorkflowError(f"connection status did not reach {expected}: {last}")


def coordinates(
    client: s10.Sprint10McpClient,
    timeout: float,
) -> dict[str, Any]:
    current, history, state, _ = s9.stable_read(client, timeout)
    return s10._coordinates(current, history, state)


def idempotency_key(run_id: str, scenario: str) -> str:
    digest = hashlib.sha256(f"{run_id}:{scenario}".encode()).hexdigest()[:32]
    return f"idempotency:{digest}"


def wait_terminal(
    client: s10.Sprint10McpClient,
    transaction_id: str,
    timeout: float,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        last, error, _ = client.tool(
            "godot_get_transaction_status",
            {"transaction_id": transaction_id},
        )
        if not error and last.get("state") in TERMINAL_STATES:
            return last
        time.sleep(0.05)
    raise s9.WorkflowError(f"transaction did not become terminal: {last}")


def run_accepted_change_set(
    client: s10.Sprint10McpClient,
    *,
    run_id: str,
    timeout: float,
) -> dict[str, Any]:
    before = coordinates(client, timeout)
    root_id = cast(Mapping[str, str], before["node_ids"])["."]
    node_name = "Sprint11SurfaceAccepted"
    prepared, prepare_error, _ = client.tool(
        "godot_prepare_change_set",
        {
            "project_id": before["project_id"],
            "idempotency_key": idempotency_key(run_id, "accept"),
            "coordinates": {
                "editor_session_id": before["editor_session_id"],
                "scene_id": before["scene_id"],
                "scene_revision": before["scene_revision"],
                "operation_seq": before["operation_seq"],
                "resource_revision": before["resource_revision"],
                "script_graph_revision": before["script_graph_revision"],
            },
            "operations": [
                {
                    "kind": "create_node",
                    "alias": "alias:accepted",
                    "parent_node_id": root_id,
                    "godot_type": "Node",
                    "name": node_name,
                },
                {
                    "kind": "set_property",
                    "node_id": "alias:accepted",
                    "property": "process_priority",
                    "value": {"type": "int", "value": 7},
                },
            ],
            "save_scope": {"paths": []},
            "validation_policy": {
                "rollback": "on_required_failure",
                "warnings": "allow",
                "runtime": "skip",
            },
        },
    )
    require(not prepare_error, f"accepted prepare failed: {prepared}")
    transaction_id = str(prepared.get("change_set_id"))
    client.expect_change_set(prepared)
    applied, apply_error, _ = client.tool(
        "godot_apply_transaction",
        {
            "transaction_id": transaction_id,
            "preview_digest": prepared["preview_digest"],
            "expected_scene_revision": before["scene_revision"],
            "expected_operation_seq": before["operation_seq"],
        },
        timeout=max(timeout, 15.0),
    )
    approval = client.clear_approval()
    require(
        not apply_error
        and approval is not None
        and approval.calls == 1,
        f"accepted apply failed: {applied}",
    )
    terminal = wait_terminal(client, transaction_id, max(timeout, 15.0))
    require(terminal.get("state") == "committed", f"change set failed: {terminal}")
    report_id = terminal.get("validation_report_id")
    require(isinstance(report_id, str), "validation report identity is missing")
    report, report_error, _ = client.tool(
        "godot_get_validation_report",
        {"report_id": report_id, "page": 0},
    )
    require(
        not report_error and report.get("report_id") == report_id,
        f"validation report failed: {report}",
    )
    after_current, after_history, after_state = s10._wait_scene_node(
        client,
        node_name,
        True,
        timeout,
    )
    after = s10._coordinates(after_current, after_history, after_state)
    undone, undo_error, _ = client.tool(
        "godot_undo_transaction",
        {
            "transaction_id": transaction_id,
            "expected_transaction_seq": terminal["transaction_seq"],
            "expected_scene_revision": after["scene_revision"],
            "expected_operation_seq": after["operation_seq"],
        },
    )
    require(
        not undo_error and undone.get("state") == "undone",
        f"exact Undo failed: {undone}",
    )
    s10._wait_scene_node(
        client,
        node_name,
        False,
        timeout,
    )
    return {
        "transaction_id": transaction_id,
        "state": "committed",
        "validation_report_id": report_id,
        "undo_state": "undone",
    }


def run_negative(
    client: s10.Sprint10McpClient,
    *,
    run_id: str,
    decision: str,
    expected_code: str,
    timeout: float,
) -> dict[str, Any]:
    before = coordinates(client, timeout)
    root_id = cast(Mapping[str, str], before["node_ids"])["."]
    prepared, prepare_error, _ = client.tool(
        "godot_prepare_create_node",
        {
            "project_id": before["project_id"],
            "editor_session_id": before["editor_session_id"],
            "scene_id": before["scene_id"],
            "history_id": before["history_id"],
            "scene_revision": before["scene_revision"],
            "operation_seq": before["operation_seq"],
            "idempotency_key": idempotency_key(run_id, decision),
            "parent_node_id": root_id,
            "godot_type": "Node",
            "name": f"Sprint11Surface{decision.title()}",
        },
    )
    require(not prepare_error, f"{decision} prepare failed: {prepared}")
    client.expect_approval(prepared, decision=decision)
    result, apply_error, _ = client.tool(
        "godot_apply_transaction",
        {
            "transaction_id": prepared["transaction_id"],
            "preview_digest": prepared["preview_digest"],
            "expected_scene_revision": prepared["scene_revision"],
            "expected_operation_seq": prepared["operation_seq"],
        },
        timeout=max(timeout, 130.0) if decision == "timeout" else timeout,
    )
    approval = client.clear_approval()
    require(
        apply_error
        and result.get("error", {}).get("code") == expected_code
        and approval is not None
        and approval.calls == 1,
        f"{decision} result differs: {result}",
    )
    after = coordinates(client, timeout)
    require(
        after["operation_seq"] == before["operation_seq"]
        and after["action_count"] == before["action_count"],
        f"{decision} changed native history",
    )
    return {
        "transaction_id": prepared["transaction_id"],
        "decision": decision,
        "error_code": expected_code,
        "state": "not_applied",
    }


def run_workflow(arguments: argparse.Namespace) -> dict[str, Any]:
    project_root = arguments.project_root.resolve(strict=True)
    godot = arguments.godot.resolve(strict=True)
    sidecar = arguments.sidecar.resolve(strict=True)
    recorder = arguments.recorder.resolve(strict=True)
    metadata = arguments.metadata.resolve(strict=True)
    journal = arguments.journal.absolute()
    require(project_root.joinpath("project.godot").is_file(), "project root differs")
    require(not journal.exists(), "journal output already exists")
    require(SAFE_RUN_ID.fullmatch(arguments.run_id) is not None, "run ID differs")
    configure_registry()
    source_before = project_hashes(project_root)
    editor: subprocess.Popen[bytes] | None = None
    editor_log: Path | None = None
    recorder_process: s9.LineProcess | None = None
    recorder_closed = False
    try:
        editor, editor_log = start_editor(godot, project_root, arguments.timeout)
        recorder_process, client = start_recorder(
            recorder=recorder,
            metadata=metadata,
            journal=journal,
            sidecar=sidecar,
            project_root=project_root,
            timeout=arguments.timeout,
        )
        list_registry(client)
        ready = wait_for_status(client, "ready", arguments.timeout)
        s9.wait_for_live(client, arguments.timeout)
        coordinates(client, arguments.timeout)
        stop_editor(editor, timeout=arguments.timeout)
        offline = wait_for_status(client, "offline_cached", arguments.timeout)
        offline_scene, offline_error, _ = client.tool("godot_get_current_scene", {})
        require(
            not offline_error
            and offline_scene.get("freshness") == "offline_cached",
            f"offline saved scene is unavailable: {offline_scene}",
        )
        if editor_log is not None:
            editor_log.unlink(missing_ok=True)
        editor, editor_log = start_editor(godot, project_root, arguments.timeout)
        restarted = wait_for_status(client, "ready", arguments.timeout)
        s9.wait_for_live(client, arguments.timeout)
        coordinates(client, arguments.timeout)
        accepted = run_accepted_change_set(
            client,
            run_id=arguments.run_id,
            timeout=arguments.timeout,
        )
        negatives = [
            run_negative(
                client,
                run_id=arguments.run_id,
                decision=decision,
                expected_code=code,
                timeout=arguments.timeout,
            )
            for decision, code in (
                ("decline", "approval_declined"),
                ("cancel", "approval_cancelled"),
                ("unsupported", "approval_host_unsupported"),
                ("timeout", "approval_timeout"),
            )
        ]
        require(
            source_before == project_hashes(project_root),
            "surface workflow changed project-content bytes",
        )
        close_recorder(recorder_process, timeout=arguments.timeout)
        recorder_closed = True
        document = strict_json(journal)
        require(document.get("status") == "complete", "recorder journal is incomplete")
        return {
            "schema_version": "s11-surface-transport-live/1.0",
            "status": "passed",
            "surface": arguments.surface,
            "protocol": MCP_PROTOCOL,
            "ready_status": ready.get("status"),
            "restarted_status": restarted.get("status"),
            "offline_status": offline.get("status"),
            "accepted": accepted,
            "negatives": negatives,
            "registry": {
                "tools": 41,
                "resources": 4,
                "resource_templates": 1,
            },
            "source_unchanged": True,
            "journal_status": document["status"],
            "journal_event_count": document["integrity"]["event_count"],
        }
    finally:
        if editor is not None:
            stop_editor(editor, timeout=arguments.timeout)
        if recorder_process is not None and not recorder_closed:
            recorder_process.stop()
            if recorder_process.stderr_tail:
                print(
                    "recorder stderr tail: "
                    + " | ".join(recorder_process.stderr_tail[-20:]),
                    file=sys.stderr,
                )
        if editor_log is not None:
            editor_log.unlink(missing_ok=True)


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--surface", choices=("app", "cli", "ide"), required=True)
    parser.add_argument("--project-root", type=Path, required=True)
    parser.add_argument("--godot", type=Path, required=True)
    parser.add_argument("--sidecar", type=Path, required=True)
    parser.add_argument(
        "--recorder",
        type=Path,
        default=SCRIPT_DIR / "sprint11_surface_recorder.py",
    )
    parser.add_argument("--metadata", type=Path, required=True)
    parser.add_argument("--journal", type=Path, required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--timeout", type=float, default=90.0)
    return parser.parse_args()


def main() -> int:
    try:
        result = run_workflow(parse_arguments())
    except (OSError, s9.WorkflowError, KeyError, TypeError, ValueError) as error:
        print(f"sprint11 surface transport: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
