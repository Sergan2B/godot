#!/usr/bin/env python3
"""Run the local macOS S9-05 structural transaction acceptance gate."""

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


def snapshot(client: BridgeClient) -> tuple[dict[str, Any], dict[str, Any]]:
    result, entities = client.snapshot()
    return snapshot_evidence(result, entities)


def prepare(
    client: BridgeClient,
    key: int,
    coordinates: dict[str, Any],
    operation: dict[str, Any],
) -> dict[str, Any]:
    preview = expect_result(
        client.request("transaction.prepare", operation_params(key, coordinates, operation))[0]
    )
    validate_preview(preview)
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
    raise AcceptanceError(f"fixture structural postcondition was not observed: {current!r}")


def row(phase: dict[str, Any], path: str) -> dict[str, Any] | None:
    return next(
        (value for value in phase.get("tree_shape", []) if value.get("path") == path),
        None,
    )


def connection(phase: dict[str, Any], emitter: str) -> dict[str, Any] | None:
    return next(
        (
            value
            for value in phase.get("connections", [])
            if value.get("emitter") == emitter
            and value.get("signal") == "tree_entered"
            and value.get("receiver") == "Receiver"
            and value.get("method") == "_on_tree_entered"
        ),
        None,
    )


def assert_committed_entity(
    status: dict[str, Any], role: str, expected_node_id: str
) -> None:
    entities = status.get("committed_entities")
    require(isinstance(entities, list) and len(entities) == 1, "committed entity evidence is missing")
    require(
        entities[0] == {"node_id": expected_node_id, "role": role},
        f"unexpected committed entity evidence: {entities!r}",
    )


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
    with tempfile.TemporaryDirectory(prefix="s9sf-", dir="/tmp") as temporary_path:
        project_root = Path(temporary_path) / "p"
        shutil.copytree(FIXTURE_ROOT, project_root, ignore=shutil.ignore_patterns(".godot"))
        phase_path = project_root / ".godot/codex-s9-prepare-phase.json"
        verify_path = project_root / ".godot/codex-s9-prepare-verify"
        done_path = project_root / ".godot/codex-s9-prepare-done"
        before_files = file_hashes(project_root)
        environment = os.environ.copy()
        environment["CODEX_S9_PREPARE_AUTOMATION"] = "1"
        environment["GODOT_CODEX_STRUCTURAL_TRANSACTION_TEST_FAULT"] = (
            f"{operation_kind}:{stage}"
        )
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
                phase = wait_for(phase_path, lambda value: value.get("phase") == 1, timeout)
                wait_for(
                    project_root / ".godot/codex/bridge.json",
                    lambda value: value.get("editor_session_id"),
                    timeout,
                )
                client = BridgeClient(project_root)
                client.initialize()
                coordinates, _ = snapshot(client)
                node_ids = coordinates["node_ids"]
                operations = {
                    "create_node": {
                        "kind": "create_node",
                        "parent_node_id": coordinates["root_node_id"],
                        "godot_type": "Node",
                        "name": "FaultCandidate",
                    },
                    "reparent_node": {
                        "kind": "reparent_node",
                        "node_id": node_ids["ReparentTarget"],
                        "new_parent_node_id": node_ids["NewParent"],
                        "insertion_index": 0,
                        "keep_global_transform": False,
                    },
                    "delete_node": {
                        "kind": "delete_node",
                        "node_id": node_ids["Deletable"],
                    },
                }
                preview = prepare(
                    client,
                    900,
                    coordinates,
                    operations[operation_kind],
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
                require(
                    row(phase, "FaultCandidate") is None,
                    f"{operation_kind}:{stage} left a created node",
                )
                require(
                    row(phase, "ReparentTarget") is not None
                    and row(phase, "NewParent/ReparentTarget") is None,
                    f"{operation_kind}:{stage} left a reparented node",
                )
                require(
                    row(phase, "Deletable") is not None
                    and row(phase, "Deletable/Descendant") is not None,
                    f"{operation_kind}:{stage} left a deleted subtree",
                )
                ping = expect_result(client.request("bridge.ping", {"echo": "responsive"})[0])
                require(ping["echo"] == "responsive", "Bridge stopped responding after fault")
                require(file_hashes(project_root) == before_files, "fault case changed fixture sources")
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
    arguments = parser.parse_args()
    godot = arguments.godot.resolve(strict=True)
    temporary = tempfile.TemporaryDirectory(prefix="s9s-", dir="/tmp")
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
            phase = wait_for(phase_path, lambda value: value.get("phase") == 1, arguments.timeout)
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
            initial_tree = canonical(phase["tree_shape"])
            initial_connections = canonical(phase["connections"])
            node_ids = coordinates["node_ids"]

            invalid_operations = [
                (
                    500,
                    {
                        "kind": "create_node",
                        "parent_node_id": coordinates["root_node_id"],
                        "godot_type": "Node",
                        "name": "Player",
                    },
                    "scene_operation_unsupported",
                ),
                (
                    501,
                    {
                        "kind": "create_node",
                        "parent_node_id": coordinates["root_node_id"],
                        "godot_type": "Resource",
                        "name": "InvalidType",
                    },
                    "scene_operation_unsupported",
                ),
                (
                    502,
                    {
                        "kind": "create_node",
                        "parent_node_id": coordinates["root_node_id"],
                        "godot_type": "Node",
                        "name": "InvalidIndex",
                        "insertion_index": 999,
                    },
                    "scene_operation_unsupported",
                ),
                (
                    503,
                    {"kind": "delete_node", "node_id": coordinates["root_node_id"]},
                    "scene_operation_unsupported",
                ),
                (
                    504,
                    {"kind": "delete_node", "node_id": node_ids["Instance/LockedChild"]},
                    "node_not_editable",
                ),
                (
                    505,
                    {
                        "kind": "reparent_node",
                        "node_id": node_ids["ReparentTarget"],
                        "new_parent_node_id": node_ids["NewParent"],
                        "insertion_index": 0,
                        "keep_global_transform": True,
                    },
                    "scene_operation_unsupported",
                ),
                (
                    506,
                    {
                        "kind": "reparent_node",
                        "node_id": node_ids["Deletable"],
                        "new_parent_node_id": node_ids["Deletable/Descendant"],
                        "insertion_index": 0,
                        "keep_global_transform": False,
                    },
                    "scene_operation_unsupported",
                ),
            ]
            for key, operation, error_code in invalid_operations:
                expect_error(
                    client.request("transaction.prepare", operation_params(key, coordinates, operation))[0],
                    error_code,
                )
            phase = marker_probe(
                markers["verify"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            after_invalid_coordinates, after_invalid_snapshot = snapshot(client)
            require(canonical(phase["tree_shape"]) == initial_tree, "preflight rejection changed the tree")
            require(
                canonical(after_invalid_snapshot) == canonical(initial_snapshot),
                "preflight rejection changed editor/history evidence",
            )
            require(
                after_invalid_coordinates["operation_seq"] == coordinates["operation_seq"],
                "preflight rejection created a native action",
            )

            create_preview = prepare(
                client,
                600,
                coordinates,
                {
                    "kind": "create_node",
                    "parent_node_id": coordinates["root_node_id"],
                    "godot_type": "Node2D",
                    "name": "CreatedStructural",
                    "insertion_index": 0,
                },
            )
            require(
                create_preview["affected_entities"]
                == [{"node_id": coordinates["root_node_id"], "role": "parent"}],
                "create preview exposed a future node identity",
            )
            phase = marker_probe(
                markers["verify"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            after_prepare_coordinates, after_prepare_snapshot = snapshot(client)
            require(canonical(phase["tree_shape"]) == initial_tree, "create prepare mutated the tree")
            require(
                canonical(after_prepare_snapshot) == canonical(initial_snapshot),
                "create prepare changed editor/history evidence",
            )
            create_apply = apply_params(
                create_preview, approval_receipt(client, create_preview)
            )
            _, create_request = client.request_message(
                "transaction.apply", create_apply, request_id="req:s9-05-create-response-loss"
            )
            client.send(create_request)
            phase = probe_until(
                markers["verify"],
                phase_path,
                phase,
                arguments.timeout,
                lambda value: row(value, "CreatedStructural") is not None,
            )
            client.close()
            client = BridgeClient(project_root)
            client.initialize()
            create_id = str(create_preview["coordinates"]["transaction_id"])
            create_status = wait_status(client, create_id, "committed", arguments.timeout)
            created_coordinates, _ = snapshot(client)
            created_node_id = created_coordinates["node_ids"]["CreatedStructural"]
            assert_committed_entity(create_status, "created", created_node_id)
            require(row(phase, "CreatedStructural")["index"] == 0, "created node index differs")
            require(row(phase, "CreatedStructural")["owner"] == ".", "created node owner differs")
            require(
                created_coordinates["operation_seq"] == coordinates["operation_seq"] + 1,
                "create did not create exactly one native action",
            )
            replayed_create = expect_result(client.request("transaction.apply", create_apply)[0])
            replay_coordinates, _ = snapshot(client)
            require(replayed_create["state"] == "committed", "duplicate create did not replay status")
            require(
                replay_coordinates["operation_seq"] == created_coordinates["operation_seq"],
                "duplicate create produced another action",
            )
            create_undone = undo(client, create_status)
            require(create_undone["state"] == "undone", "targeted create Undo failed")
            assert_committed_entity(create_undone, "created", created_node_id)
            phase = probe_until(
                markers["verify"],
                phase_path,
                phase,
                arguments.timeout,
                lambda value: row(value, "CreatedStructural") is None,
            )
            phase = marker_probe(
                markers["redo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            create_redone = wait_status(client, create_id, "committed", arguments.timeout)
            require(row(phase, "CreatedStructural") is not None, "native Redo did not recreate node")
            phase = marker_probe(
                markers["undo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            create_native_undone = wait_status(client, create_id, "undone", arguments.timeout)
            require(row(phase, "CreatedStructural") is None, "native Undo did not remove created node")
            phase = marker_probe(
                markers["redo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            create_final_redone = wait_status(client, create_id, "committed", arguments.timeout)
            require(row(phase, "CreatedStructural") is not None, "final create Redo failed")

            coordinates, _ = snapshot(client)
            node_ids = coordinates["node_ids"]
            reparent_before = row(phase, "ReparentTarget")
            reparent_connection_before = connection(phase, "ReparentTarget")
            require(reparent_before is not None, "reparent target baseline is missing")
            require(reparent_connection_before is not None, "reparent connection baseline is missing")
            reparent_preview = prepare(
                client,
                700,
                coordinates,
                {
                    "kind": "reparent_node",
                    "node_id": node_ids["ReparentTarget"],
                    "new_parent_node_id": node_ids["NewParent"],
                    "insertion_index": 0,
                    "keep_global_transform": False,
                },
            )
            reparent_status = expect_result(
                client.request(
                    "transaction.apply",
                    apply_params(reparent_preview, approval_receipt(client, reparent_preview)),
                )[0]
            )
            require(reparent_status["state"] == "committed", "reparent did not commit")
            phase = probe_until(
                markers["verify"],
                phase_path,
                phase,
                arguments.timeout,
                lambda value: row(value, "NewParent/ReparentTarget") is not None,
            )
            reparented_coordinates, _ = snapshot(client)
            reparented_id = reparented_coordinates["node_ids"]["NewParent/ReparentTarget"]
            assert_committed_entity(reparent_status, "target", reparented_id)
            require(
                row(phase, "NewParent/ReparentTarget")["index"] == 0,
                "reparent final index differs",
            )
            require(
                connection(phase, "NewParent/ReparentTarget") == reparent_connection_before
                | {"emitter": "NewParent/ReparentTarget"},
                "reparent did not preserve the signal connection",
            )
            reparent_id = str(reparent_preview["coordinates"]["transaction_id"])
            reparent_undone = undo(client, reparent_status)
            assert_committed_entity(reparent_undone, "target", reparented_id)
            phase = probe_until(
                markers["verify"],
                phase_path,
                phase,
                arguments.timeout,
                lambda value: row(value, "ReparentTarget") is not None,
            )
            require(row(phase, "ReparentTarget") == reparent_before, "targeted reparent Undo was not exact")
            require(connection(phase, "ReparentTarget") == reparent_connection_before, "Undo lost connection")
            phase = marker_probe(
                markers["redo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            reparent_redone = wait_status(client, reparent_id, "committed", arguments.timeout)
            require(row(phase, "NewParent/ReparentTarget") is not None, "reparent Redo failed")
            phase = marker_probe(
                markers["undo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            reparent_native_undone = wait_status(client, reparent_id, "undone", arguments.timeout)
            require(row(phase, "ReparentTarget") == reparent_before, "native reparent Undo was not exact")
            phase = marker_probe(
                markers["redo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            reparent_final_redone = wait_status(client, reparent_id, "committed", arguments.timeout)
            require(row(phase, "NewParent/ReparentTarget") is not None, "final reparent Redo failed")

            coordinates, _ = snapshot(client)
            node_ids = coordinates["node_ids"]
            reparent_3d_before = row(phase, "Parent3D/ReparentTarget3D")
            require(reparent_3d_before is not None, "Node3D reparent baseline is missing")
            reparent_3d_preview = prepare(
                client,
                750,
                coordinates,
                {
                    "kind": "reparent_node",
                    "node_id": node_ids["Parent3D/ReparentTarget3D"],
                    "new_parent_node_id": node_ids["NewParent3D"],
                    "insertion_index": 0,
                    "keep_global_transform": True,
                },
            )
            reparent_3d_status = expect_result(
                client.request(
                    "transaction.apply",
                    apply_params(
                        reparent_3d_preview,
                        approval_receipt(client, reparent_3d_preview),
                    ),
                )[0]
            )
            require(reparent_3d_status["state"] == "committed", "Node3D reparent failed")
            phase = probe_until(
                markers["verify"],
                phase_path,
                phase,
                arguments.timeout,
                lambda value: row(value, "NewParent3D/ReparentTarget3D") is not None,
            )
            reparent_3d_after = row(phase, "NewParent3D/ReparentTarget3D")
            require(
                reparent_3d_after["transform"]["global"]
                == reparent_3d_before["transform"]["global"],
                "Node3D global transform changed during reparent",
            )
            reparented_3d_coordinates, _ = snapshot(client)
            reparented_3d_id = reparented_3d_coordinates["node_ids"][
                "NewParent3D/ReparentTarget3D"
            ]
            assert_committed_entity(reparent_3d_status, "target", reparented_3d_id)
            reparent_3d_id = str(reparent_3d_preview["coordinates"]["transaction_id"])
            reparent_3d_undone = undo(client, reparent_3d_status)
            assert_committed_entity(reparent_3d_undone, "target", reparented_3d_id)
            phase = probe_until(
                markers["verify"],
                phase_path,
                phase,
                arguments.timeout,
                lambda value: row(value, "Parent3D/ReparentTarget3D") is not None,
            )
            require(
                row(phase, "Parent3D/ReparentTarget3D") == reparent_3d_before,
                "Node3D targeted Undo did not restore its exact transform",
            )
            phase = marker_probe(
                markers["redo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            reparent_3d_redone = wait_status(
                client, reparent_3d_id, "committed", arguments.timeout
            )
            require(
                row(phase, "NewParent3D/ReparentTarget3D")["transform"]["global"]
                == reparent_3d_before["transform"]["global"],
                "Node3D native Redo changed global transform",
            )
            phase = marker_probe(
                markers["undo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            reparent_3d_native_undone = wait_status(
                client, reparent_3d_id, "undone", arguments.timeout
            )
            require(
                row(phase, "Parent3D/ReparentTarget3D") == reparent_3d_before,
                "Node3D native Undo was not exact",
            )
            phase = marker_probe(
                markers["redo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            reparent_3d_final_redone = wait_status(
                client, reparent_3d_id, "committed", arguments.timeout
            )

            coordinates, _ = snapshot(client)
            node_ids = coordinates["node_ids"]
            delete_before = [
                value
                for value in phase["tree_shape"]
                if value.get("path") in {"Deletable", "Deletable/Descendant"}
            ]
            delete_connection_before = connection(phase, "Deletable")
            require(len(delete_before) == 2, "delete subtree baseline is missing")
            require(delete_connection_before is not None, "delete connection baseline is missing")
            delete_preview = prepare(
                client,
                800,
                coordinates,
                {"kind": "delete_node", "node_id": node_ids["Deletable"]},
            )
            delete_status = expect_result(
                client.request(
                    "transaction.apply",
                    apply_params(delete_preview, approval_receipt(client, delete_preview)),
                )[0]
            )
            require(delete_status["state"] == "committed", "delete did not commit")
            require("committed_entities" not in delete_status, "delete exposed nonexistent post-commit node evidence")
            phase = probe_until(
                markers["verify"],
                phase_path,
                phase,
                arguments.timeout,
                lambda value: row(value, "Deletable") is None,
            )
            require(row(phase, "Deletable/Descendant") is None, "delete left a visible descendant")
            require(connection(phase, "Deletable") is None, "deleted node remained in the scene oracle")
            delete_id = str(delete_preview["coordinates"]["transaction_id"])
            delete_undone = undo(client, delete_status)
            phase = probe_until(
                markers["verify"],
                phase_path,
                phase,
                arguments.timeout,
                lambda value: row(value, "Deletable/Descendant") is not None,
            )
            restored_delete = [
                value
                for value in phase["tree_shape"]
                if value.get("path") in {"Deletable", "Deletable/Descendant"}
            ]
            require(restored_delete == delete_before, "targeted delete Undo did not restore the exact subtree")
            require(connection(phase, "Deletable") == delete_connection_before, "delete Undo lost connection")
            phase = marker_probe(
                markers["redo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            delete_redone = wait_status(client, delete_id, "committed", arguments.timeout)
            require(row(phase, "Deletable") is None, "delete native Redo failed")
            phase = marker_probe(
                markers["undo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            delete_native_undone = wait_status(client, delete_id, "undone", arguments.timeout)
            require(connection(phase, "Deletable") == delete_connection_before, "native delete Undo lost connection")
            phase = marker_probe(
                markers["redo"], phase_path, int(phase["probe_seq"]), arguments.timeout
            )
            delete_final_redone = wait_status(client, delete_id, "committed", arguments.timeout)
            require(row(phase, "Deletable") is None, "final delete Redo failed")

            statuses = [
                create_status,
                replayed_create,
                create_undone,
                create_redone,
                create_native_undone,
                create_final_redone,
                reparent_status,
                reparent_undone,
                reparent_redone,
                reparent_native_undone,
                reparent_final_redone,
                reparent_3d_status,
                reparent_3d_undone,
                reparent_3d_redone,
                reparent_3d_native_undone,
                reparent_3d_final_redone,
                delete_status,
                delete_undone,
                delete_redone,
                delete_native_undone,
                delete_final_redone,
            ]
            for status in statuses:
                assert_safe(status)
                require(len(canonical(status)) <= 65_536, "structural status exceeds wire limit")
            require(file_hashes(project_root) == before_files, "structural workflow changed fixture sources")
            fault_results = {
                f"{operation_kind}:{stage}": run_fault_case(
                    godot, arguments.timeout, operation_kind, stage
                )
                for operation_kind in ["create_node", "reparent_node", "delete_node"]
                for stage in ["preflight", "registration", "postcondition", "rollback_proof"]
            }
            require(
                all(
                    value["operation_seq_delta"] == 0
                    for key, value in fault_results.items()
                    if key.endswith(":preflight")
                ),
                "a final-preflight structural fault created a native action",
            )
            evidence = {
                "schema_version": "s9-05-acceptance/1.0",
                "status": "passed",
                "protocol_version": "1.7",
                "preflight_zero_action_cases": len(invalid_operations),
                "create_response_loss_recovered": True,
                "create_duplicate_action_prevented": True,
                "create_committed_entity": created_node_id,
                "create_targeted_undo": create_undone["state"],
                "create_native_redo": create_redone["state"],
                "create_native_undo": create_native_undone["state"],
                "reparent_committed_entity": reparented_id,
                "reparent_targeted_undo": reparent_undone["state"],
                "reparent_native_redo": reparent_redone["state"],
                "reparent_native_undo": reparent_native_undone["state"],
                "reparent_3d_committed_entity": reparented_3d_id,
                "reparent_3d_global_transform_preserved": True,
                "delete_targeted_undo": delete_undone["state"],
                "delete_native_redo": delete_redone["state"],
                "delete_native_undo": delete_native_undone["state"],
                "connections_preserved": True,
                "fault_results": fault_results,
                "source_hashes": before_files,
                "status_bytes_max": max(len(canonical(value)) for value in statuses),
                "initial_connections": json.loads(initial_connections),
            }
            arguments.evidence.parent.mkdir(parents=True, exist_ok=True)
            arguments.evidence.write_text(
                json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8"
            )
            print(json.dumps(evidence, indent=2, sort_keys=True))
        except Exception:
            log.seek(0)
            output = log.read()[-24_000:]
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
    require(file_hashes(project_root) == before_files, "editor changed fixture sources on shutdown")
    temporary.cleanup()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
