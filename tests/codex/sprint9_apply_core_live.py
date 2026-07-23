#!/usr/bin/env python3
"""Run the local S9-04 apply/status/Undo acceptance gate."""

from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import os
import shutil
import struct
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

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
    wait_for,
)


def lp(output: bytearray, value: str) -> None:
    encoded = value.encode()
    output.extend(struct.pack(">I", len(encoded)))
    output.extend(encoded)


def approval_receipt(
    client: BridgeClient,
    preview: dict[str, Any],
    *,
    nonce: bytes | None = None,
    issued_at_ms: int | None = None,
) -> dict[str, Any]:
    coordinates = preview["coordinates"]
    nonce = nonce or os.urandom(32)
    require(len(nonce) == 32, "approval nonce must be 32 bytes")
    issued_at_ms = issued_at_ms or int(time.time() * 1000)
    expires_at_ms = issued_at_ms + 30_000
    material = bytearray(b"godot-codex/approval-receipt/v1\0")
    for value in [
        client.project_id,
        client.editor_session_id,
        str(coordinates["scene_id"]),
        str(coordinates["transaction_id"]),
        str(preview["preview_digest"]),
        str(preview["scope"]),
        str(preview["risk"]),
    ]:
        lp(material, value)
    for value in [
        int(coordinates["scene_revision"]),
        int(coordinates["operation_seq"]),
        issued_at_ms,
        expires_at_ms,
    ]:
        material.extend(struct.pack(">Q", value))
    material.extend(nonce)
    approval_key = hmac.new(client.token, b"godot-codex/approval-key/v1", hashlib.sha256).digest()
    mac = hmac.new(approval_key, bytes(material), hashlib.sha256).digest()
    return {
        "kind": "mcp_form_v1",
        "scope": preview["scope"],
        "nonce": base64.urlsafe_b64encode(nonce).decode().rstrip("="),
        "issued_at_ms": issued_at_ms,
        "expires_at_ms": expires_at_ms,
        "mac": base64.urlsafe_b64encode(mac).decode().rstrip("="),
    }


def apply_params(preview: dict[str, Any], receipt: dict[str, Any]) -> dict[str, Any]:
    coordinates = preview["coordinates"]
    return {
        "transaction_id": coordinates["transaction_id"],
        "preview_digest": preview["preview_digest"],
        "expected_scene_revision": coordinates["scene_revision"],
        "expected_operation_seq": coordinates["operation_seq"],
        "approval": receipt,
    }


def property_prepare(key: int, coordinates: dict[str, Any], value: list[float]) -> dict[str, Any]:
    return operation_params(
        key,
        coordinates,
        {
            "kind": "set_property",
            "node_id": coordinates["player_node_id"],
            "property": "position",
            "value": {"type": "vector2", "value": value},
        },
    )


def assert_safe(value: Any) -> None:
    forbidden = {"native_history_id", "object_id", "pointer", "handle", "nonce", "mac", "approval"}
    if isinstance(value, dict):
        require(not (forbidden & set(value)), f"transaction result exposes forbidden fields: {forbidden & set(value)}")
        for child in value.values():
            assert_safe(child)
    elif isinstance(value, list):
        for child in value:
            assert_safe(child)


def marker_probe(
    marker: Path,
    phase_path: Path,
    previous_probe: int,
    timeout: float,
    *,
    position: list[float] | None = None,
    phase: int | None = None,
) -> dict[str, Any]:
    marker.write_text("go\n", encoding="utf-8")
    return wait_for(
        phase_path,
        lambda value: int(value.get("probe_seq", -1)) > previous_probe
        and (position is None or value.get("position") == position)
        and (phase is None or value.get("phase") == phase),
        timeout,
    )


def wait_status(client: BridgeClient, transaction_id: str, state: str, timeout: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        response, _ = client.request("transaction.status", {"transaction_id": transaction_id})
        if "error" not in response:
            last = expect_result(response)
            if last.get("state") == state:
                return last
        time.sleep(0.05)
    raise AcceptanceError(f"transaction {transaction_id} did not reach {state}: {last!r}")


def run_fault_case(godot: Path, timeout: float, fault: str, expected_state: str) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="s9f-") as temporary_path:
        project_root = Path(temporary_path) / "p"
        shutil.copytree(FIXTURE_ROOT, project_root, ignore=shutil.ignore_patterns(".godot"))
        phase_path = project_root / ".godot/codex-s9-prepare-phase.json"
        done_path = project_root / ".godot/codex-s9-prepare-done"
        verify_path = project_root / ".godot/codex-s9-prepare-verify"
        before_files = file_hashes(project_root)
        environment = os.environ.copy()
        environment["CODEX_S9_PREPARE_AUTOMATION"] = "1"
        environment["GODOT_CODEX_TRANSACTION_TEST_EXECUTOR"] = "1"
        environment["GODOT_CODEX_TRANSACTION_TEST_FAULT"] = fault
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
                wait_for(project_root / ".godot/codex/bridge.json", lambda value: value.get("editor_session_id"), timeout)
                client = BridgeClient(project_root)
                client.initialize()
                snapshot, entities = client.snapshot()
                coordinates, _ = snapshot_evidence(snapshot, entities)
                preview = expect_result(client.request("transaction.prepare", property_prepare(400, coordinates, [70.0, 80.0]))[0])
                transaction_id = str(preview["coordinates"]["transaction_id"])
                apply_response = client.request("transaction.apply", apply_params(preview, approval_receipt(client, preview)))[0]
                if fault == "preflight":
                    expect_error(apply_response, "transaction_apply_failed")
                else:
                    result = expect_result(apply_response)
                    require(result["state"] == expected_state, f"{fault} apply returned {result['state']}")
                status = wait_status(client, transaction_id, expected_state, timeout)
                phase = marker_probe(verify_path, phase_path, int(phase["probe_seq"]), timeout, position=[8.0, 12.0])
                require(file_hashes(project_root) == before_files, f"{fault} changed a fixture source file")
                assert_safe(status)
                return {
                    "state": status["state"],
                    "error": status.get("error", {}).get("code"),
                    "operation_seq_delta": int(status["current_operation_seq"]) - int(coordinates["operation_seq"]),
                    "position": phase["position"],
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
    temporary = tempfile.TemporaryDirectory(prefix="s9a-")
    project_root = Path(temporary.name) / "p"
    shutil.copytree(FIXTURE_ROOT, project_root, ignore=shutil.ignore_patterns(".godot"))
    phase_path = project_root / ".godot/codex-s9-prepare-phase.json"
    markers = {
        "verify": project_root / ".godot/codex-s9-prepare-verify",
        "change": project_root / ".godot/codex-s9-prepare-change",
        "bounded": project_root / ".godot/codex-s9-prepare-bounded",
        "undo": project_root / ".godot/codex-s9-native-undo",
        "redo": project_root / ".godot/codex-s9-native-redo",
        "done": project_root / ".godot/codex-s9-prepare-done",
    }
    before_files = file_hashes(project_root)
    environment = os.environ.copy()
    environment["CODEX_S9_PREPARE_AUTOMATION"] = "1"
    environment["GODOT_CODEX_TRANSACTION_TEST_EXECUTOR"] = "1"
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
            wait_for(project_root / ".godot/codex/bridge.json", lambda value: value.get("editor_session_id"), arguments.timeout)
            client = BridgeClient(project_root)
            initialized = client.initialize()
            capability = next(value for value in initialized["capabilities"] if value.get("name") == "transaction.scene_v1")
            require(capability["readiness"] == "ready", "transaction capability is not ready")
            require(capability["transaction_readiness"]["approval_state"] == "available", "approval verifier is unavailable")

            snapshot, entities = client.snapshot()
            coordinates, _ = snapshot_evidence(snapshot, entities)
            first = expect_result(client.request("transaction.prepare", property_prepare(300, coordinates, [20.0, 30.0]))[0])
            first_nonce = os.urandom(32)
            first_receipt = approval_receipt(client, first, nonce=first_nonce)

            wrong_scope = dict(first_receipt)
            wrong_scope["scope"] = "scene.node.create"
            expect_error(client.request("transaction.apply", apply_params(first, wrong_scope))[0], "approval_scope_mismatch")
            wrong_mac = dict(first_receipt)
            original_mac = str(wrong_mac["mac"])
            wrong_mac["mac"] = ("A" if original_mac[0] != "A" else "B") + original_mac[1:]
            expect_error(client.request("transaction.apply", apply_params(first, wrong_mac))[0], "approval_invalid")

            first_apply_params = apply_params(first, first_receipt)
            first_request_id, first_request = client.request_message("transaction.apply", first_apply_params, request_id="req:s9-concurrent-one")
            joined_request_id, joined_request = client.request_message("transaction.apply", first_apply_params, request_id="req:s9-concurrent-two")
            client.send(first_request)
            client.send(joined_request)
            joined_responses = client.wait_responses({first_request_id, joined_request_id})
            first_status = expect_result(joined_responses[first_request_id][0])
            joined_status = expect_result(joined_responses[joined_request_id][0])
            require(canonical(first_status) == canonical(joined_status), "concurrent duplicate apply did not join one terminal result")
            require(first_status["state"] == "committed", "first apply did not commit")
            assert_safe(first_status)
            phase = marker_probe(markers["verify"], phase_path, int(phase["probe_seq"]), arguments.timeout, position=[20.0, 30.0])
            after_first_snapshot, after_first_entities = client.snapshot()
            after_first_coordinates, _ = snapshot_evidence(after_first_snapshot, after_first_entities)
            require(after_first_coordinates["operation_seq"] == coordinates["operation_seq"] + 1, "first apply did not create exactly one native action")

            replay_preview = expect_result(client.request("transaction.prepare", property_prepare(301, after_first_coordinates, [21.0, 31.0]))[0])
            replay_receipt = approval_receipt(client, replay_preview, nonce=first_nonce)
            expect_error(client.request("transaction.apply", apply_params(replay_preview, replay_receipt))[0], "approval_replayed")

            snapshot, entities = client.snapshot()
            coordinates, _ = snapshot_evidence(snapshot, entities)
            second = expect_result(client.request("transaction.prepare", property_prepare(302, coordinates, [40.0, 50.0]))[0])
            second_params = apply_params(second, approval_receipt(client, second))
            lost_request_id, lost_request = client.request_message("transaction.apply", second_params, request_id="req:s9-response-loss")
            client.send(lost_request)
            for _ in range(20):
                phase = marker_probe(markers["verify"], phase_path, int(phase["probe_seq"]), arguments.timeout)
                if phase.get("position") == [40.0, 50.0]:
                    break
            require(phase.get("position") == [40.0, 50.0], "response-loss apply did not reach its postcondition")
            client.close()
            client = BridgeClient(project_root)
            client.initialize()
            second_id = str(second["coordinates"]["transaction_id"])
            recovered = wait_status(client, second_id, "committed", arguments.timeout)
            recovered_seq = int(recovered["coordinates"]["transaction_seq"])
            before_replay_snapshot, before_replay_entities = client.snapshot()
            before_replay_coordinates, before_replay_evidence = snapshot_evidence(before_replay_snapshot, before_replay_entities)
            replayed = expect_result(client.request("transaction.apply", second_params)[0])
            require(replayed["state"] == "committed", "idempotent apply replay did not return committed")
            require(int(replayed["coordinates"]["transaction_seq"]) == recovered_seq, "idempotent replay advanced transaction_seq")
            after_replay_snapshot, after_replay_entities = client.snapshot()
            after_replay_coordinates, after_replay_evidence = snapshot_evidence(after_replay_snapshot, after_replay_entities)
            require(after_replay_coordinates["operation_seq"] == before_replay_coordinates["operation_seq"], "idempotent replay created another native action")
            require(canonical(after_replay_evidence) == canonical(before_replay_evidence), "idempotent replay changed editor or history evidence")

            undo_params = {
                "transaction_id": second_id,
                "expected_transaction_seq": recovered["coordinates"]["transaction_seq"],
                "expected_scene_revision": recovered["current_scene_revision"],
                "expected_operation_seq": recovered["current_operation_seq"],
            }
            undone_second = expect_result(client.request("transaction.undo", undo_params)[0])
            require(undone_second["state"] == "undone", "targeted Undo did not reach undone")
            phase = marker_probe(markers["verify"], phase_path, int(phase["probe_seq"]), arguments.timeout, position=[20.0, 30.0])

            markers["change"].write_text("change\n", encoding="utf-8")
            phase = wait_for(phase_path, lambda value: value.get("phase") == 2 and value.get("position") == [9.0, 13.0], arguments.timeout)
            first_id = str(first["coordinates"]["transaction_id"])
            current_first = expect_result(client.request("transaction.status", {"transaction_id": first_id})[0])
            require(current_first["undo_eligibility"]["reason"] == "not_newest_action", "unrelated action did not block targeted Undo")
            blocked_undo = {
                "transaction_id": first_id,
                "expected_transaction_seq": current_first["coordinates"]["transaction_seq"],
                "expected_scene_revision": current_first["current_scene_revision"],
                "expected_operation_seq": current_first["current_operation_seq"],
            }
            expect_error(client.request("transaction.undo", blocked_undo)[0], "transaction_not_undoable")

            phase = marker_probe(markers["undo"], phase_path, int(phase["probe_seq"]), arguments.timeout, position=[20.0, 30.0])
            phase = marker_probe(markers["undo"], phase_path, int(phase["probe_seq"]), arguments.timeout, position=[8.0, 12.0])
            native_undone = wait_status(client, first_id, "undone", arguments.timeout)
            phase = marker_probe(markers["redo"], phase_path, int(phase["probe_seq"]), arguments.timeout, position=[20.0, 30.0])
            native_redone = wait_status(client, first_id, "committed", arguments.timeout)
            before_bounded_snapshot, before_bounded_entities = client.snapshot()
            cancel_coordinates, _ = snapshot_evidence(before_bounded_snapshot, before_bounded_entities)

            markers["bounded"].write_text("bounded\n", encoding="utf-8")
            phase = wait_for(phase_path, lambda value: value.get("phase") == 3, arguments.timeout)
            cancelled_preview = expect_result(client.request("transaction.prepare", property_prepare(303, cancel_coordinates, [60.0, 70.0]))[0])
            cancelled_params = apply_params(cancelled_preview, approval_receipt(client, cancelled_preview))
            cancel_id, cancel_request = client.request_message("transaction.apply", cancelled_params, request_id="req:s9-apply-cancel")
            client.send(cancel_request)
            client.send(
                {
                    "protocol_version": "1.7",
                    "kind": "cancel",
                    "request_id": cancel_id,
                    "reason": "s9-04-precommit-cancel",
                    "context": client.context,
                }
            )
            expect_error(client.wait_response(cancel_id)[0], "cancelled")
            cancelled_id = str(cancelled_preview["coordinates"]["transaction_id"])
            cancelled_status = wait_status(client, cancelled_id, "failed", arguments.timeout)
            require(cancelled_status["error"]["code"] == "approval_cancelled", "cancelled apply has the wrong terminal error")
            phase = marker_probe(markers["verify"], phase_path, int(phase["probe_seq"]), arguments.timeout)
            require(phase["position"] == [20.0, 30.0], "cancelled apply mutated the fixture")
            expect_result(client.request("bridge.ping", {"echo": "drain-events"})[0])

            require(file_hashes(project_root) == before_files, "S9-04 changed a fixture source file")
            for value in [recovered, replayed, undone_second, current_first, native_undone, native_redone, cancelled_status]:
                assert_safe(value)
                require(len(canonical(value)) <= 65_536, "transaction status exceeds the wire limit")
            transaction_events = [value for value in client.notifications if value.get("method") == "transaction.event"]
            require(transaction_events, "transaction lifecycle notifications were not published")
            event_coordinates = [value.get("params", {}).get("coordinates", {}) for value in transaction_events]
            event_keys = [(value.get("transaction_id"), value.get("transaction_seq")) for value in event_coordinates]
            require(len(event_keys) == len(set(event_keys)), "a transaction transition event was published more than once")
            require(any(value.get("transaction_id") == cancelled_id and event.get("params", {}).get("state") == "failed" for value, event in zip(event_coordinates, transaction_events)), "cancelled apply terminal event is missing")
            require(any(value.get("transaction_id") == first_id and event.get("params", {}).get("state") == "undone" for value, event in zip(event_coordinates, transaction_events)), "native Undo event is missing")
            for event in transaction_events:
                assert_safe(event)
                require(len(canonical(event)) <= 65_536, "transaction event exceeds the wire limit")
            fault_results = {
                fault: run_fault_case(godot, arguments.timeout, fault, expected_state)
                for fault, expected_state in {
                    "preflight": "failed",
                    "registration": "in_doubt",
                    "postcondition": "failed_rolled_back",
                    "rollback_proof": "in_doubt",
                }.items()
            }
            require(fault_results["preflight"]["operation_seq_delta"] == 0, "preflight failure created a native action")

            evidence = {
                "schema_version": "s9-04-acceptance/1.0",
                "status": "passed",
                "protocol_version": "1.7",
                "approval_binding": ["scope_mismatch", "invalid_mac", "replayed_nonce"],
                "first_apply_native_actions": 1,
                "response_loss_recovered": True,
                "duplicate_apply_transaction_seq_unchanged": True,
                "duplicate_apply_operation_seq_unchanged": True,
                "concurrent_duplicate_joined": True,
                "targeted_undo": "undone",
                "unrelated_action_blocked": True,
                "native_undo_state": native_undone["state"],
                "native_redo_state": native_redone["state"],
                "precommit_cancel_state": cancelled_status["state"],
                "precommit_cancel_error": cancelled_status["error"]["code"],
                "final_operation_seq": cancelled_status["current_operation_seq"],
                "transaction_event_count": len(transaction_events),
                "fault_results": fault_results,
                "status_bytes_max": max(len(canonical(value)) for value in [recovered, replayed, undone_second, current_first, native_undone, native_redone, cancelled_status]),
                "source_hashes": before_files,
            }
            arguments.evidence.parent.mkdir(parents=True, exist_ok=True)
            arguments.evidence.write_text(json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8")
            print(json.dumps(evidence, indent=2, sort_keys=True))
        except Exception:
            log.seek(0)
            output = log.read()[-16_000:]
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
    require(file_hashes(project_root) == before_files, "editor fixture changed a source file")
    temporary.cleanup()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
