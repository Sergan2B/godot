#!/usr/bin/env python3
"""Run the local macOS S9-06/S9-07 property/script/signal acceptance gate."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any, Callable

from sprint9_apply_core_live import (
    apply_params,
    approval_receipt,
    assert_safe,
    marker_probe,
    wait_status,
)
from sprint9_prepare_live import (
    AcceptanceError,
    BridgeClient,
    FIXTURE_ROOT,
    canonical,
    expect_error,
    expect_result,
    file_hashes,
    operation_params,
    require,
    snapshot_evidence,
    validate_preview,
    wait_for,
)

BIND_SENTINEL = "S9_BIND_SECRET_SENTINEL"
FIXTURE_SCRIPT = "res://scripts/fixture_endpoint.gd"
REPLACEMENT_SCRIPT = "res://scripts/replacement_endpoint.gd"


def snapshot(client: BridgeClient) -> tuple[dict[str, Any], dict[str, Any]]:
    result, entities = client.snapshot()
    return snapshot_evidence(result, entities)


def row(phase: dict[str, Any], path: str) -> dict[str, Any] | None:
    return next(
        (value for value in phase.get("tree_shape", []) if value.get("path") == path),
        None,
    )


def connection(
    phase: dict[str, Any],
    method: str,
    *,
    signal: str = "pulse",
) -> dict[str, Any] | None:
    return next(
        (
            value
            for value in phase.get("connections", [])
            if value.get("emitter") == "Emitter"
            and value.get("signal") == signal
            and value.get("receiver") == "Receiver"
            and value.get("method") == method
        ),
        None,
    )


def probe_until(
    marker: Path,
    phase_path: Path,
    phase: dict[str, Any],
    timeout: float,
    predicate: Callable[[dict[str, Any]], bool],
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    current = phase
    while time.monotonic() < deadline:
        current = marker_probe(
            marker,
            phase_path,
            int(current["probe_seq"]),
            min(5.0, max(0.1, deadline - time.monotonic())),
        )
        if predicate(current):
            return current
    raise AcceptanceError(f"fixture binding postcondition was not observed: {current!r}")


def prepare(
    client: BridgeClient,
    key: int,
    coordinates: dict[str, Any],
    operation: dict[str, Any],
    forbidden_text: list[str] | None = None,
) -> dict[str, Any]:
    preview = expect_result(
        client.request(
            "transaction.prepare", operation_params(key, coordinates, operation)
        )[0]
    )
    validate_preview(preview)
    encoded = canonical(preview)
    for value in forbidden_text or []:
        require(
            value.encode("utf-8") not in encoded,
            f"immutable preview leaked redacted input: {value}",
        )
    assert_safe(preview)
    return preview


def undo(client: BridgeClient, status: dict[str, Any]) -> dict[str, Any]:
    return expect_result(
        client.request(
            "transaction.undo",
            {
                "transaction_id": status["coordinates"]["transaction_id"],
                "expected_transaction_seq": status["coordinates"]["transaction_seq"],
                "expected_scene_revision": status["current_scene_revision"],
                "expected_operation_seq": status["current_operation_seq"],
            },
        )[0]
    )


def exercise_cycle(
    client: BridgeClient,
    markers: dict[str, Path],
    phase_path: Path,
    phase: dict[str, Any],
    timeout: float,
    preview: dict[str, Any],
    applied: Callable[[dict[str, Any]], bool],
    restored: Callable[[dict[str, Any]], bool],
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    before_coordinates, _ = snapshot(client)
    status = expect_result(
        client.request(
            "transaction.apply",
            apply_params(preview, approval_receipt(client, preview)),
        )[0]
    )
    require(status["state"] == "committed", "production transaction did not commit")
    phase = probe_until(markers["verify"], phase_path, phase, timeout, applied)
    after_coordinates, _ = snapshot(client)
    require(
        after_coordinates["operation_seq"] == before_coordinates["operation_seq"] + 1,
        "operation did not create exactly one native action",
    )
    transaction_id = str(preview["coordinates"]["transaction_id"])
    targeted = undo(client, status)
    require(targeted["state"] == "undone", "targeted Undo did not reach undone")
    phase = probe_until(markers["verify"], phase_path, phase, timeout, restored)

    phase = marker_probe(
        markers["redo"], phase_path, int(phase["probe_seq"]), timeout
    )
    require(applied(phase), "native Redo did not reproduce the postcondition")
    native_redo = wait_status(client, transaction_id, "committed", timeout)
    phase = marker_probe(
        markers["undo"], phase_path, int(phase["probe_seq"]), timeout
    )
    require(restored(phase), "native Undo did not restore the exact prestate")
    native_undo = wait_status(client, transaction_id, "undone", timeout)
    phase = marker_probe(
        markers["redo"], phase_path, int(phase["probe_seq"]), timeout
    )
    require(applied(phase), "final native Redo did not reproduce the postcondition")
    final_redo = wait_status(client, transaction_id, "committed", timeout)
    statuses = [status, targeted, native_redo, native_undo, final_redo]
    for value in statuses:
        assert_safe(value)
        require(len(canonical(value)) <= 65_536, "binding status exceeds wire limit")
    return phase, statuses


def fault_operation(
    kind: str,
    coordinates: dict[str, Any],
    phase: dict[str, Any],
) -> dict[str, Any]:
    node_ids = coordinates["node_ids"]
    operations = {
        "set_property": {
            "kind": "set_property",
            "node_id": coordinates["player_node_id"],
            "property": "position",
            "value": {"type": "vector2", "value": [70.0, 80.0]},
        },
        "attach_script": {
            "kind": "attach_script",
            "node_id": node_ids["Scriptless"],
            "script_ref": {"uid_missing": True, "path": FIXTURE_SCRIPT},
        },
        "detach_script": {
            "kind": "detach_script",
            "node_id": node_ids["Scripted"],
        },
        "connect_signal": {
            "kind": "connect_signal",
            "emitter_node_id": node_ids["Emitter"],
            "signal": "pulse",
            "receiver_node_id": node_ids["Receiver"],
            "method": "_on_bound",
            "flags": 3,
            "unbinds": 0,
            "binds": [{"type": "string", "value": BIND_SENTINEL}],
        },
    }
    if kind == "disconnect_signal":
        existing = connection(phase, "_on_pulse")
        require(existing is not None, "fault fixture connection is missing")
        return {
            "kind": kind,
            "emitter_node_id": node_ids["Emitter"],
            "signal": "pulse",
            "receiver_node_id": node_ids["Receiver"],
            "method": "_on_pulse",
            "flags": existing["flags"],
            "unbinds": existing["unbinds"],
            "binds": [],
        }
    return operations[kind]


def run_fault_case(
    godot: Path,
    timeout: float,
    operation_kind: str,
    stage: str,
) -> dict[str, Any]:
    expected_state = {
        "preflight": "failed",
        "registration": "in_doubt",
        "postcondition": "failed_rolled_back",
        "rollback_proof": "in_doubt",
    }[stage]
    with tempfile.TemporaryDirectory(prefix="s9bf-", dir="/tmp") as temporary_path:
        project_root = Path(temporary_path) / "p"
        shutil.copytree(
            FIXTURE_ROOT, project_root, ignore=shutil.ignore_patterns(".godot")
        )
        phase_path = project_root / ".godot/codex-s9-prepare-phase.json"
        verify_path = project_root / ".godot/codex-s9-prepare-verify"
        done_path = project_root / ".godot/codex-s9-prepare-done"
        before_files = file_hashes(project_root)
        environment = os.environ.copy()
        environment["CODEX_S9_PREPARE_AUTOMATION"] = "1"
        fault_variable = (
            "GODOT_CODEX_PROPERTY_TRANSACTION_TEST_FAULT"
            if operation_kind == "set_property"
            else "GODOT_CODEX_BINDING_TRANSACTION_TEST_FAULT"
        )
        environment[fault_variable] = f"{operation_kind}:{stage}"
        with tempfile.TemporaryFile(mode="w+t", encoding="utf-8") as log:
            editor = subprocess.Popen(
                [str(godot), "--editor", "--headless", "--path", str(project_root)],
                cwd=project_root,
                env=environment,
                stdout=log,
                stderr=subprocess.STDOUT,
                text=True,
            )
            client: BridgeClient | None = None
            try:
                phase = wait_for(
                    phase_path, lambda value: value.get("phase") == 1, timeout
                )
                wait_for(
                    project_root / ".godot/codex/bridge.json",
                    lambda value: value.get("editor_session_id"),
                    timeout,
                )
                client = BridgeClient(project_root)
                client.initialize()
                coordinates, baseline_snapshot = snapshot(client)
                baseline_tree = canonical(phase["tree_shape"])
                baseline_connections = canonical(phase["connections"])
                operation = fault_operation(operation_kind, coordinates, phase)
                preview = prepare(
                    client,
                    900,
                    coordinates,
                    operation,
                    [BIND_SENTINEL, FIXTURE_SCRIPT, REPLACEMENT_SCRIPT],
                )
                transaction_id = str(preview["coordinates"]["transaction_id"])
                response = client.request(
                    "transaction.apply",
                    apply_params(preview, approval_receipt(client, preview)),
                )[0]
                if stage == "preflight":
                    expect_error(response, "transaction_apply_failed")
                else:
                    result = expect_result(response)
                    require(
                        result["state"] == expected_state,
                        f"{operation_kind}:{stage} returned {result['state']}",
                    )
                status = wait_status(client, transaction_id, expected_state, timeout)
                phase = marker_probe(
                    verify_path, phase_path, int(phase["probe_seq"]), timeout
                )
                after_coordinates, after_snapshot = snapshot(client)
                require(
                    canonical(phase["tree_shape"]) == baseline_tree,
                    f"{operation_kind}:{stage} did not restore the exact node/script/property state",
                )
                require(
                    canonical(phase["connections"]) == baseline_connections,
                    f"{operation_kind}:{stage} did not restore exact signal connections",
                )
                if stage == "preflight":
                    require(
                        after_coordinates["operation_seq"]
                        == coordinates["operation_seq"],
                        "final-preflight fault created a native action",
                    )
                    require(
                        canonical(after_snapshot) == canonical(baseline_snapshot),
                        "final-preflight fault changed editor evidence",
                    )
                ping = expect_result(
                    client.request("bridge.ping", {"echo": "responsive"})[0]
                )
                require(ping["echo"] == "responsive", "Bridge stopped after a fault")
                require(
                    file_hashes(project_root) == before_files,
                    "fault case changed fixture source files",
                )
                assert_safe(status)
                return {
                    "state": status["state"],
                    "error": status.get("error", {}).get("code"),
                    "operation_seq_delta": int(status["current_operation_seq"])
                    - int(coordinates["operation_seq"]),
                    "bridge_responsive": True,
                }
            except Exception:
                log.seek(0)
                output = log.read()[-16_000:]
                if output:
                    print(output)
                raise
            finally:
                if client is not None:
                    client.close()
                done_path.parent.mkdir(parents=True, exist_ok=True)
                done_path.write_text("done\n", encoding="utf-8")
                try:
                    editor.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    editor.terminate()
                    try:
                        editor.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        editor.kill()
                        editor.wait(timeout=5)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument(
        "--skip-fault-matrix",
        action="store_true",
        help="Run only the primary workflow (not valid for final acceptance).",
    )
    arguments = parser.parse_args()
    godot = arguments.godot.resolve(strict=True)
    temporary = tempfile.TemporaryDirectory(prefix="s9b-", dir="/tmp")
    project_root = Path(temporary.name) / "p"
    shutil.copytree(FIXTURE_ROOT, project_root, ignore=shutil.ignore_patterns(".godot"))
    phase_path = project_root / ".godot/codex-s9-prepare-phase.json"
    markers = {
        "verify": project_root / ".godot/codex-s9-prepare-verify",
        "undo": project_root / ".godot/codex-s9-native-undo",
        "redo": project_root / ".godot/codex-s9-native-redo",
        "done": project_root / ".godot/codex-s9-prepare-done",
    }
    before_files = file_hashes(project_root)
    environment = os.environ.copy()
    environment["CODEX_S9_PREPARE_AUTOMATION"] = "1"
    all_statuses: list[dict[str, Any]] = []
    with tempfile.TemporaryFile(mode="w+t", encoding="utf-8") as log:
        editor = subprocess.Popen(
            [str(godot), "--editor", "--headless", "--path", str(project_root)],
            cwd=project_root,
            env=environment,
            stdout=log,
            stderr=subprocess.STDOUT,
            text=True,
        )
        client: BridgeClient | None = None
        try:
            phase = wait_for(
                phase_path,
                lambda value: value.get("phase") == 1,
                arguments.timeout,
            )
            wait_for(
                project_root / ".godot/codex/bridge.json",
                lambda value: value.get("editor_session_id"),
                arguments.timeout,
            )
            client = BridgeClient(project_root)
            initialized = client.initialize()
            capability = next(
                value
                for value in initialized["capabilities"]
                if value.get("name") == "transaction.scene_v1"
            )
            require(capability["readiness"] == "ready", "transaction capability is not ready")
            coordinates, initial_snapshot = snapshot(client)
            node_ids = coordinates["node_ids"]
            original_connection = connection(phase, "_on_pulse")
            require(original_connection is not None, "saved pulse connection is missing")
            original_scripted = row(phase, "Scripted")
            require(
                original_scripted is not None
                and original_scripted["script"] == FIXTURE_SCRIPT
                and original_scripted["custom_value"] == 41,
                "script fixture prestate differs",
            )

            invalid_operations = [
                (
                    400,
                    {
                        "kind": "set_property",
                        "node_id": node_ids["Scripted"],
                        "property": "custom_value",
                        "value": {"type": "int", "value": 99},
                    },
                    "property_not_writable",
                ),
                (
                    401,
                    {
                        "kind": "set_property",
                        "node_id": coordinates["player_node_id"],
                        "property": "position",
                        "value": {"type": "int", "value": 99},
                    },
                    "property_value_unsupported",
                ),
                (
                    402,
                    {
                        "kind": "attach_script",
                        "node_id": node_ids["Scriptless"],
                        "script_ref": {
                            "uid_missing": True,
                            "path": "res://scripts/missing.gd",
                        },
                    },
                    "script_incompatible",
                ),
                (
                    403,
                    {
                        "kind": "attach_script",
                        "node_id": node_ids["Scriptless"],
                        "script_ref": {
                            "uid_missing": True,
                            "path": "res://scripts/incompatible.gd",
                        },
                    },
                    "script_incompatible",
                ),
                (
                    404,
                    {
                        "kind": "connect_signal",
                        "emitter_node_id": node_ids["Emitter"],
                        "signal": "pulse",
                        "receiver_node_id": node_ids["Receiver"],
                        "method": "_on_bound",
                        "flags": 2,
                        "unbinds": 0,
                        "binds": [],
                    },
                    "signal_connection_invalid",
                ),
            ]
            for key, operation, code in invalid_operations:
                expect_error(
                    client.request(
                        "transaction.prepare",
                        operation_params(key, coordinates, operation),
                    )[0],
                    code,
                )
            for key, flags in [(405, 0), (406, 10)]:
                invalid_flags = {
                    "kind": "connect_signal",
                    "emitter_node_id": node_ids["Emitter"],
                    "signal": "pulse",
                    "receiver_node_id": node_ids["Receiver"],
                    "method": "_on_pulse",
                    "flags": flags,
                    "unbinds": 0,
                    "binds": [],
                }
                expect_error(
                    client.request(
                        "transaction.prepare",
                        operation_params(key, coordinates, invalid_flags),
                    )[0],
                    "invalid_request",
                )
            phase = marker_probe(
                markers["verify"],
                phase_path,
                int(phase["probe_seq"]),
                arguments.timeout,
            )
            after_invalid_coordinates, after_invalid_snapshot = snapshot(client)
            require(
                canonical(after_invalid_snapshot) == canonical(initial_snapshot),
                "rejected operations changed editor evidence",
            )
            require(
                after_invalid_coordinates["operation_seq"] == coordinates["operation_seq"],
                "rejected operations created a native action",
            )

            coordinates, _ = snapshot(client)
            property_preview = prepare(
                client,
                500,
                coordinates,
                {
                    "kind": "set_property",
                    "node_id": coordinates["player_node_id"],
                    "property": "position",
                    "value": {"type": "vector2", "value": [123.25, -45.5]},
                },
                ["123.25", "-45.5"],
            )
            phase, statuses = exercise_cycle(
                client,
                markers,
                phase_path,
                phase,
                arguments.timeout,
                property_preview,
                lambda value: value.get("position") == [123.25, -45.5],
                lambda value: value.get("position") == [8.0, 12.0],
            )
            all_statuses.extend(statuses)

            coordinates, _ = snapshot(client)
            node_ids = coordinates["node_ids"]
            attach_preview = prepare(
                client,
                510,
                coordinates,
                {
                    "kind": "attach_script",
                    "node_id": node_ids["Scriptless"],
                    "script_ref": {"uid_missing": True, "path": FIXTURE_SCRIPT},
                },
                [FIXTURE_SCRIPT],
            )
            phase, statuses = exercise_cycle(
                client,
                markers,
                phase_path,
                phase,
                arguments.timeout,
                attach_preview,
                lambda value: row(value, "Scriptless")["script"] == FIXTURE_SCRIPT,
                lambda value: row(value, "Scriptless")["script"] == "",
            )
            all_statuses.extend(statuses)

            coordinates, _ = snapshot(client)
            node_ids = coordinates["node_ids"]
            replace_preview = prepare(
                client,
                520,
                coordinates,
                {
                    "kind": "attach_script",
                    "node_id": node_ids["Scripted"],
                    "script_ref": {
                        "uid_missing": True,
                        "path": REPLACEMENT_SCRIPT,
                    },
                },
                [REPLACEMENT_SCRIPT, FIXTURE_SCRIPT],
            )
            phase, statuses = exercise_cycle(
                client,
                markers,
                phase_path,
                phase,
                arguments.timeout,
                replace_preview,
                lambda value: row(value, "Scripted")["script"]
                == REPLACEMENT_SCRIPT,
                lambda value: row(value, "Scripted")["script"] == FIXTURE_SCRIPT
                and row(value, "Scripted")["custom_value"] == 41,
            )
            all_statuses.extend(statuses)

            coordinates, _ = snapshot(client)
            node_ids = coordinates["node_ids"]
            detach_preview = prepare(
                client,
                530,
                coordinates,
                {"kind": "detach_script", "node_id": node_ids["Scripted"]},
            )
            phase, statuses = exercise_cycle(
                client,
                markers,
                phase_path,
                phase,
                arguments.timeout,
                detach_preview,
                lambda value: row(value, "Scripted")["script"] == "",
                lambda value: row(value, "Scripted")["script"]
                == REPLACEMENT_SCRIPT
                and row(value, "Scripted")["replacement_value"] == "replacement",
            )
            all_statuses.extend(statuses)

            coordinates, _ = snapshot(client)
            node_ids = coordinates["node_ids"]
            connect_preview = prepare(
                client,
                540,
                coordinates,
                {
                    "kind": "connect_signal",
                    "emitter_node_id": node_ids["Emitter"],
                    "signal": "pulse",
                    "receiver_node_id": node_ids["Receiver"],
                    "method": "_on_bound",
                    "flags": 3,
                    "unbinds": 0,
                    "binds": [{"type": "string", "value": BIND_SENTINEL}],
                },
                [BIND_SENTINEL],
            )
            expected_bound_connection = {
                "emitter": "Emitter",
                "signal": "pulse",
                "receiver": "Receiver",
                "method": "_on_bound",
                "flags": 3,
                "unbinds": 0,
                "binds": [BIND_SENTINEL],
            }
            phase, statuses = exercise_cycle(
                client,
                markers,
                phase_path,
                phase,
                arguments.timeout,
                connect_preview,
                lambda value: connection(value, "_on_bound")
                == expected_bound_connection,
                lambda value: connection(value, "_on_bound") is None,
            )
            all_statuses.extend(statuses)
            coordinates, _ = snapshot(client)
            duplicate_connect = operation_params(
                541,
                coordinates,
                {
                    "kind": "connect_signal",
                    "emitter_node_id": coordinates["node_ids"]["Emitter"],
                    "signal": "pulse",
                    "receiver_node_id": coordinates["node_ids"]["Receiver"],
                    "method": "_on_bound",
                    "flags": 3,
                    "unbinds": 0,
                    "binds": [{"type": "string", "value": BIND_SENTINEL}],
                },
            )
            expect_error(
                client.request("transaction.prepare", duplicate_connect)[0],
                "signal_connection_invalid",
            )

            coordinates, _ = snapshot(client)
            node_ids = coordinates["node_ids"]
            disconnect_preview = prepare(
                client,
                550,
                coordinates,
                {
                    "kind": "disconnect_signal",
                    "emitter_node_id": node_ids["Emitter"],
                    "signal": "pulse",
                    "receiver_node_id": node_ids["Receiver"],
                    "method": "_on_pulse",
                    "flags": original_connection["flags"],
                    "unbinds": original_connection["unbinds"],
                    "binds": [],
                },
            )
            phase, statuses = exercise_cycle(
                client,
                markers,
                phase_path,
                phase,
                arguments.timeout,
                disconnect_preview,
                lambda value: connection(value, "_on_pulse") is None,
                lambda value: connection(value, "_on_pulse") == original_connection,
            )
            all_statuses.extend(statuses)
            require(
                connection(phase, "_on_bound") == expected_bound_connection,
                "disconnect changed an unrelated same-signal connection",
            )
            require(
                file_hashes(project_root) == before_files,
                "production binding workflow changed fixture source files",
            )
        except Exception:
            log.seek(0)
            output = log.read()[-20_000:]
            if output:
                print(output)
            raise
        finally:
            if client is not None:
                client.close()
            markers["done"].parent.mkdir(parents=True, exist_ok=True)
            markers["done"].write_text("done\n", encoding="utf-8")
            try:
                editor.wait(timeout=10)
            except subprocess.TimeoutExpired:
                editor.terminate()
                try:
                    editor.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    editor.kill()
                    editor.wait(timeout=5)
    require(file_hashes(project_root) == before_files, "editor changed fixture sources")
    temporary.cleanup()

    fault_results: dict[str, dict[str, Any]] = {}
    if not arguments.skip_fault_matrix:
        fault_results = {
            f"{operation_kind}:{stage}": run_fault_case(
                godot, arguments.timeout, operation_kind, stage
            )
            for operation_kind in [
                "set_property",
                "attach_script",
                "detach_script",
                "connect_signal",
                "disconnect_signal",
            ]
            for stage in ["preflight", "registration", "postcondition", "rollback_proof"]
        }
    evidence = {
        "schema_version": "s9-06-s9-07-acceptance/1.0",
        "status": "passed",
        "protocol_version": "1.7",
        "production_operations": [
            "set_property",
            "attach_script",
            "replace_script",
            "detach_script",
            "connect_signal",
            "disconnect_signal",
        ],
        "rejected_zero_action_cases": 7,
        "redacted_preview_cases": 5,
        "targeted_undo_cases": 6,
        "native_undo_redo_cases": 6,
        "exact_exported_value_restored": True,
        "exact_signal_callable_restored": True,
        "fault_results": fault_results,
        "status_bytes_max": max(len(canonical(value)) for value in all_statuses),
        "source_hashes": before_files,
    }
    arguments.evidence.parent.mkdir(parents=True, exist_ok=True)
    arguments.evidence.write_text(
        json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps(evidence, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
