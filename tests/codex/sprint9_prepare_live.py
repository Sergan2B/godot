#!/usr/bin/env python3
"""Run the local macOS Bridge RPC 1.7 transaction.prepare acceptance gate."""

from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import os
import re
import shutil
import socket
import struct
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

SCRIPT_ROOT = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_ROOT.parent.parent
FIXTURE_ROOT = SCRIPT_ROOT / "fixtures" / "transaction_prepare_project"
TRANSACTION_ID = re.compile(r"^transaction:[0-9a-f]{32}$")


class AcceptanceError(RuntimeError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AcceptanceError(message)


def canonical(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode()


def append_field(output: bytearray, value: bytes) -> None:
    output.extend(struct.pack(">I", len(value)))
    output.extend(value)


def transcript(
    offered: list[str],
    selected: str,
    project_id: str,
    editor_session_id: str,
    client_nonce: bytes,
    server_nonce: bytes,
) -> bytes:
    output = bytearray(b"godot-codex-bridge/handshake-transcript/v1\0")
    append_field(output, b"1.0")
    output.extend(struct.pack(">I", len(offered)))
    for version in offered:
        append_field(output, version.encode())
    append_field(output, selected.encode())
    append_field(output, project_id.encode())
    append_field(output, editor_session_id.encode())
    append_field(output, client_nonce)
    append_field(output, server_nonce)
    return bytes(output)


def base64url(value: bytes) -> str:
    return base64.urlsafe_b64encode(value).decode().rstrip("=")


def decode_base64url(value: str) -> bytes:
    return base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))


class BridgeClient:
    def __init__(self, project_root: Path):
        discovery = json.loads((project_root / ".godot/codex/bridge.json").read_text(encoding="utf-8"))
        endpoint = Path(discovery["endpoint"])
        require(not endpoint.is_absolute() and ".." not in endpoint.parts, "unsafe bridge endpoint")
        self.project_id = str(discovery["project_id"])
        self.editor_session_id = str(discovery["editor_session_id"])
        self.context = {
            "project_id": self.project_id,
            "editor_session_id": self.editor_session_id,
        }
        token_path = project_root / str(discovery["token_file"])
        token = token_path.read_bytes()
        require(len(token) == 32, "session token is not 32 bytes")
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(15.0)
        self.socket.connect(str(project_root / endpoint))
        self.next_request = 1
        self._handshake(token)

    def close(self) -> None:
        self.socket.close()

    def send(self, value: dict[str, Any]) -> None:
        payload = canonical(value)
        require(0 < len(payload) <= 1_048_576, "outgoing frame is outside protocol bounds")
        self.socket.sendall(struct.pack(">I", len(payload)) + payload)

    def receive(self) -> tuple[dict[str, Any], bytes]:
        prefix = self._read_exact(4)
        length = struct.unpack(">I", prefix)[0]
        require(0 < length <= 1_048_576, "incoming frame is outside protocol bounds")
        payload = self._read_exact(length)
        value = json.loads(payload)
        require(isinstance(value, dict), "incoming frame is not an object")
        return value, payload

    def _read_exact(self, length: int) -> bytes:
        output = bytearray()
        while len(output) < length:
            chunk = self.socket.recv(length - len(output))
            if not chunk:
                raise AcceptanceError("bridge connection closed unexpectedly")
            output.extend(chunk)
        return bytes(output)

    def _handshake(self, token: bytes) -> None:
        offered = ["1.7"]
        client_nonce = os.urandom(32)
        hello = {
            "handshake_version": "1.0",
            "kind": "handshake.client_hello",
            "supported_protocol_versions": offered,
            "project_id": self.project_id,
            "editor_session_id": self.editor_session_id,
            "client_nonce": base64url(client_nonce),
        }
        self.send(hello)
        challenge, _ = self.receive()
        require(challenge.get("kind") == "handshake.server_challenge", "bridge rejected RPC 1.7 hello")
        selected = str(challenge.get("selected_protocol_version"))
        require(selected == "1.7", "bridge did not select RPC 1.7")
        server_nonce = decode_base64url(str(challenge["server_nonce"]))
        material = transcript(offered, selected, self.project_id, self.editor_session_id, client_nonce, server_nonce)
        expected_server = hmac.new(token, b"godot-codex-bridge/server-proof/v1\0" + material, hashlib.sha256).digest()
        require(hmac.compare_digest(expected_server, decode_base64url(str(challenge["server_proof"]))), "server proof differs")
        client_proof = hmac.new(token, b"godot-codex-bridge/client-proof/v1\0" + material, hashlib.sha256).digest()
        self.send(
            {
                "handshake_version": "1.0",
                "kind": "handshake.client_authenticate",
                "selected_protocol_version": selected,
                "project_id": self.project_id,
                "editor_session_id": self.editor_session_id,
                "client_proof": base64url(client_proof),
            }
        )
        ready, _ = self.receive()
        require(ready.get("kind") == "handshake.server_ready", "bridge authentication did not complete")

    def request_message(self, method: str, params: dict[str, Any], *, request_id: str | None = None, deadline_ms: int = 5000) -> tuple[str, dict[str, Any]]:
        if request_id is None:
            request_id = f"req:s9-{self.next_request:08d}"
            self.next_request += 1
        message = {
            "protocol_version": "1.7",
            "kind": "request",
            "request_id": request_id,
            "method": method,
            "deadline_ms": deadline_ms,
            "params": params,
            "context": self.context,
        }
        return request_id, message

    def request(self, method: str, params: dict[str, Any], *, deadline_ms: int = 5000) -> tuple[dict[str, Any], bytes]:
        request_id, message = self.request_message(method, params, deadline_ms=deadline_ms)
        self.send(message)
        return self.wait_response(request_id)

    def wait_response(self, request_id: str) -> tuple[dict[str, Any], bytes]:
        while True:
            message, payload = self.receive()
            if message.get("kind") == "response" and message.get("request_id") == request_id:
                return message, payload

    def wait_responses(self, request_ids: set[str]) -> dict[str, tuple[dict[str, Any], bytes]]:
        pending = set(request_ids)
        responses: dict[str, tuple[dict[str, Any], bytes]] = {}
        while pending:
            message, payload = self.receive()
            request_id = message.get("request_id")
            if message.get("kind") == "response" and request_id in pending:
                responses[str(request_id)] = (message, payload)
                pending.remove(str(request_id))
        return responses

    def initialize(self) -> dict[str, Any]:
        response, _ = self.request(
            "bridge.initialize",
            {
                "client": {"name": "s9-prepare-acceptance", "version": "1.0"},
                "requested_capabilities": ["transaction.scene_v1"],
            },
        )
        result = response.get("result")
        require(isinstance(result, dict) and result.get("protocol_version") == "1.7", "RPC 1.7 initialize failed")
        return result

    def snapshot(self) -> tuple[dict[str, Any], list[dict[str, Any]]]:
        request_id, request = self.request_message("editor.snapshot.get", {})
        self.send(request)
        entities: list[dict[str, Any]] = []
        snapshot_id = ""
        response_result: dict[str, Any] | None = None
        stream_complete = False
        while response_result is None or not stream_complete:
            message, _ = self.receive()
            if message.get("method") == "snapshot.begin":
                snapshot_id = str(message["params"]["snapshot_id"])
            elif message.get("kind") == "chunk" and message.get("snapshot_id") == snapshot_id:
                chunk_entities = message.get("payload", {}).get("entities", [])
                entities.extend(value for value in chunk_entities if isinstance(value, dict))
                self.send(
                    {
                        "protocol_version": "1.7",
                        "kind": "ack",
                        "ack_id": f"ack:s9-{self.next_request:08d}",
                        "params": {
                            "snapshot_id": snapshot_id,
                            "through_chunk": int(message["chunk_index"]),
                        },
                        "context": self.context,
                    }
                )
                self.next_request += 1
            elif message.get("method") == "snapshot.end" and message.get("params", {}).get("snapshot_id") == snapshot_id:
                stream_complete = True
            elif message.get("kind") == "response" and message.get("request_id") == request_id:
                response_result = message.get("result")
                if isinstance(response_result, dict):
                    snapshot_id = str(response_result.get("snapshot_id", snapshot_id))
        require(isinstance(response_result, dict), "editor snapshot response is missing")
        return response_result, entities


def wait_for(path: Path, predicate: Any, timeout: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: Any = None
    while time.monotonic() < deadline:
        try:
            last = json.loads(path.read_text(encoding="utf-8"))
            if isinstance(last, dict) and "error" in last:
                raise AcceptanceError(str(last["error"]))
            if isinstance(last, dict) and predicate(last):
                return last
        except (FileNotFoundError, json.JSONDecodeError):
            pass
        time.sleep(0.05)
    raise AcceptanceError(f"fixture wait timed out: {path.name}; last={last!r}")


def file_hashes(project_root: Path) -> dict[str, str]:
    paths = sorted(
        path
        for path in project_root.rglob("*")
        if path.is_file() and ".godot" not in path.parts and path.suffix in {".tscn", ".tres", ".gd", ".cs"}
    )
    return {path.relative_to(project_root).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}


def snapshot_evidence(result: dict[str, Any], entities: list[dict[str, Any]]) -> tuple[dict[str, Any], dict[str, Any]]:
    scene = next((entity for entity in entities if entity.get("kind") == "scene" and entity.get("path") == "res://main.tscn"), None)
    player = next((entity for entity in entities if entity.get("kind") == "node" and entity.get("node_path") == "Player"), None)
    root = next((entity for entity in entities if entity.get("kind") == "node" and entity.get("node_path") == "."), None)
    observed = [
        {
            "kind": entity.get("kind"),
            "path": entity.get("path"),
            "node_path": entity.get("node_path"),
        }
        for entity in entities[:32]
    ]
    require(
        isinstance(scene, dict) and isinstance(player, dict) and isinstance(root, dict),
        f"fixture scene nodes are missing from snapshot; observed={observed!r}",
    )
    revision = result.get("revisions")
    require(isinstance(revision, dict), "snapshot revisions are missing")
    node_ids = {
        str(entity["node_path"]): str(entity["entity_id"])
        for entity in entities
        if entity.get("kind") == "node" and isinstance(entity.get("node_path"), str) and isinstance(entity.get("entity_id"), str)
    }
    coordinates = {
        "scene_id": scene["entity_id"],
        "history_id": scene["history_id"],
        "scene_revision": int(scene["scene_revision"]),
        "operation_seq": int(revision["operation_seq"]),
    }
    stable_kinds = {"editor_state", "scene", "node", "inspector_state", "history_state", "editor_history"}
    stable_entities = [entity for entity in entities if entity.get("kind") in stable_kinds]
    stable_entities.sort(key=lambda value: (str(value.get("kind")), str(value.get("entity_id"))))
    evidence = {
        "entities": stable_entities,
        "revisions": revision,
    }
    return coordinates | {"player_node_id": player["entity_id"], "root_node_id": root["entity_id"], "node_ids": node_ids}, evidence


def prepare_params(key_number: int, coordinates: dict[str, Any], node_id: str, *, name: str = "PreparedChild") -> dict[str, Any]:
    return {
        "idempotency_key": f"idempotency:{key_number:032x}",
        "coordinates": {
            "scene_id": coordinates["scene_id"],
            "history_id": coordinates["history_id"],
            "scene_revision": coordinates["scene_revision"],
            "operation_seq": coordinates["operation_seq"],
        },
        "operation": {
            "kind": "create_node",
            "parent_node_id": node_id,
            "godot_type": "Node",
            "name": name,
        },
    }


def operation_params(key_number: int, coordinates: dict[str, Any], operation: dict[str, Any]) -> dict[str, Any]:
    return {
        "idempotency_key": f"idempotency:{key_number:032x}",
        "coordinates": {
            "scene_id": coordinates["scene_id"],
            "history_id": coordinates["history_id"],
            "scene_revision": coordinates["scene_revision"],
            "operation_seq": coordinates["operation_seq"],
        },
        "operation": operation,
    }


def expect_result(response: dict[str, Any]) -> dict[str, Any]:
    require("error" not in response, f"unexpected RPC error: {response.get('error')}")
    result = response.get("result")
    require(isinstance(result, dict), "RPC result is missing")
    return result


def expect_error(response: dict[str, Any], code: str) -> None:
    error = response.get("error")
    require(isinstance(error, dict) and error.get("code") == code, f"expected {code}, got {error}")


def validate_preview(result: dict[str, Any]) -> None:
    transaction_id = result.get("coordinates", {}).get("transaction_id")
    require(isinstance(transaction_id, str) and TRANSACTION_ID.fullmatch(transaction_id), "transaction_id is not opaque random hex")
    payload = result.get("preview_payload_json")
    digest = result.get("preview_digest")
    require(isinstance(payload, str) and len(payload.encode()) <= 65_536, "preview payload is not bounded")
    require(digest == "sha256:" + hashlib.sha256(payload.encode()).hexdigest(), "preview digest does not bind exact bytes")
    forbidden_keys = {"object_id", "native_history_id", "pid", "pointer", "rid", "handle", "approval", "nonce", "mac"}

    def walk(value: Any) -> None:
        if isinstance(value, dict):
            require(not (set(value) & forbidden_keys), "preview exposes a forbidden native or approval field")
            for child in value.values():
                walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)

    walk(result)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--project-root", type=Path)
    parser.add_argument("--timeout", type=float, default=60.0)
    arguments = parser.parse_args()
    godot = arguments.godot.resolve(strict=True)
    temporary_project: tempfile.TemporaryDirectory[str] | None = None
    if arguments.project_root is None:
        temporary_project = tempfile.TemporaryDirectory(prefix="s9p-", dir="/tmp")
        project_root = Path(temporary_project.name) / "p"
        shutil.copytree(FIXTURE_ROOT, project_root, ignore=shutil.ignore_patterns(".godot"))
    else:
        project_root = arguments.project_root.resolve(strict=True)
    phase_path = project_root / ".godot/codex-s9-prepare-phase.json"
    markers = {
        "verify": project_root / ".godot/codex-s9-prepare-verify",
        "change": project_root / ".godot/codex-s9-prepare-change",
        "bounded": project_root / ".godot/codex-s9-prepare-bounded",
        "oversized": project_root / ".godot/codex-s9-prepare-oversized",
        "done": project_root / ".godot/codex-s9-prepare-done",
    }
    before_files = file_hashes(project_root)
    for path in [phase_path, *markers.values()]:
        path.unlink(missing_ok=True)
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
            phase_one = wait_for(phase_path, lambda value: value.get("phase") == 1, arguments.timeout)
            discovery = project_root / ".godot/codex/bridge.json"
            wait_for(discovery, lambda value: value.get("editor_session_id"), arguments.timeout)
            client = BridgeClient(project_root)
            initialized = client.initialize()
            capability = next(value for value in initialized["capabilities"] if value.get("name") == "transaction.scene_v1")
            require(capability["transaction_readiness"]["reason"] == "approval_unavailable", "capability readiness still reports coordinator unavailable")

            snapshot_result, entities = client.snapshot()
            coordinates, before_prepare = snapshot_evidence(snapshot_result, entities)
            first_params = prepare_params(1, coordinates, coordinates["root_node_id"])
            first_response = client.request("transaction.prepare", first_params)[0]
            require("error" not in first_response, f"first prepare rejected params={first_params!r}; error={first_response.get('error')!r}")
            first = expect_result(first_response)
            validate_preview(first)
            replay = expect_result(client.request("transaction.prepare", first_params)[0])
            require(canonical(first) == canonical(replay), "same-key replay did not return byte-stable result content")

            changed = json.loads(json.dumps(first_params))
            changed["operation"]["name"] = "DifferentChild"
            expect_error(client.request("transaction.prepare", changed)[0], "idempotency_conflict")
            node_ids = coordinates["node_ids"]
            required_paths = {
                "Deletable",
                "ReparentTarget",
                "NewParent",
                "Scriptless",
                "Scripted",
                "Emitter",
                "Receiver",
                "Instance/LockedChild",
            }
            require(required_paths <= node_ids.keys(), f"fixture operation targets are missing: {sorted(required_paths - node_ids.keys())}")
            pulse = next(
                (
                    connection
                    for connection in phase_one.get("connections", [])
                    if connection.get("signal") == "pulse"
                    and connection.get("receiver") == "Receiver"
                    and connection.get("method") == "_on_pulse"
                ),
                None,
            )
            require(isinstance(pulse, dict) and isinstance(pulse.get("flags"), int), "saved signal connection evidence is missing")
            script_ref = {"uid_missing": True, "path": "res://scripts/fixture_endpoint.gd"}
            operations = [
                {"kind": "delete_node", "node_id": node_ids["Deletable"]},
                {
                    "kind": "reparent_node",
                    "node_id": node_ids["ReparentTarget"],
                    "new_parent_node_id": node_ids["NewParent"],
                    "insertion_index": 0,
                    "keep_global_transform": False,
                },
                {
                    "kind": "set_property",
                    "node_id": coordinates["player_node_id"],
                    "property": "position",
                    "value": {"type": "vector2", "value": [20.0, 30.0]},
                },
                {"kind": "attach_script", "node_id": node_ids["Scriptless"], "script_ref": script_ref},
                {"kind": "attach_script", "node_id": node_ids["Scripted"], "script_ref": script_ref},
                {"kind": "detach_script", "node_id": node_ids["Scripted"]},
                {
                    "kind": "connect_signal",
                    "emitter_node_id": node_ids["Emitter"],
                    "signal": "pulse",
                    "receiver_node_id": node_ids["Receiver"],
                    "method": "_on_tree_entered",
                    "flags": 0,
                    "unbinds": 0,
                    "binds": [],
                },
                {
                    "kind": "disconnect_signal",
                    "emitter_node_id": node_ids["Emitter"],
                    "signal": "pulse",
                    "receiver_node_id": node_ids["Receiver"],
                    "method": "_on_pulse",
                    "flags": pulse["flags"],
                    "unbinds": 0,
                    "binds": [],
                },
            ]
            second: dict[str, Any] | None = None
            for key_number, operation in enumerate(operations, start=2):
                result = expect_result(client.request("transaction.prepare", operation_params(key_number, coordinates, operation))[0])
                validate_preview(result)
                require(result.get("preview", {}).get("operation_kind") == operation["kind"], "operation-specific preview kind differs")
                second = second or result
            require(second is not None, "operation preview set is empty")
            require(second["coordinates"]["transaction_id"] != first["coordinates"]["transaction_id"], "new idempotency key reused transaction_id")

            forbidden_property = operation_params(
                200,
                coordinates,
                {
                    "kind": "set_property",
                    "node_id": node_ids["Scripted"],
                    "property": "custom_value",
                    "value": {"type": "int", "value": 9},
                },
            )
            expect_error(client.request("transaction.prepare", forbidden_property)[0], "property_not_writable")
            locked_instance = operation_params(
                201,
                coordinates,
                {"kind": "delete_node", "node_id": node_ids["Instance/LockedChild"]},
            )
            expect_error(client.request("transaction.prepare", locked_instance)[0], "node_not_editable")
            incompatible_script = operation_params(
                202,
                coordinates,
                {
                    "kind": "attach_script",
                    "node_id": node_ids["Scriptless"],
                    "script_ref": {"uid_missing": True, "path": "res://scripts/incompatible.gd"},
                },
            )
            expect_error(client.request("transaction.prepare", incompatible_script)[0], "script_incompatible")

            for key_number in range(10, 65):
                result = expect_result(client.request("transaction.prepare", prepare_params(key_number, coordinates, coordinates["root_node_id"]))[0])
                validate_preview(result)
            expect_error(client.request("transaction.prepare", prepare_params(65, coordinates, coordinates["root_node_id"]))[0], "transaction_busy")

            after_result, after_entities = client.snapshot()
            _, after_prepare = snapshot_evidence(after_result, after_entities)
            require(canonical(before_prepare) == canonical(after_prepare), "prepare changed scene, history, selection, dirty state, or revisions")
            require(file_hashes(project_root) == before_files, "prepare changed a fixture source file")
            markers["verify"].write_text("verify\n", encoding="utf-8")
            verified = wait_for(phase_path, lambda value: value.get("phase") == 1 and value.get("probe_seq") == 1, arguments.timeout)
            baseline_shape = dict(phase_one)
            verified_shape = dict(verified)
            baseline_shape.pop("probe_seq", None)
            verified_shape.pop("probe_seq", None)
            require(canonical(baseline_shape) == canonical(verified_shape), "prepare changed native tree, scripts, properties, or connections")

            markers["change"].write_text("change\n", encoding="utf-8")
            phase_two = wait_for(phase_path, lambda value: value.get("phase") == 2, arguments.timeout)
            revision_deadline = time.monotonic() + arguments.timeout
            while True:
                phase_two_result, phase_two_entities = client.snapshot()
                phase_two_coordinates, _ = snapshot_evidence(phase_two_result, phase_two_entities)
                if phase_two_coordinates["operation_seq"] > coordinates["operation_seq"]:
                    break
                require(time.monotonic() < revision_deadline, "native fixture edit did not advance operation_seq")
                time.sleep(0.05)
            expect_error(client.request("transaction.prepare", first_params)[0], "transaction_conflicted")

            markers["bounded"].write_text("bounded\n", encoding="utf-8")
            phase_three = wait_for(phase_path, lambda value: value.get("phase") == 3, arguments.timeout)
            duplicate_params = prepare_params(99, phase_two_coordinates, phase_two_coordinates["root_node_id"], name="ConcurrentPreview")
            duplicate_one_id, duplicate_one = client.request_message("transaction.prepare", duplicate_params, request_id="req:s9-duplicate-one")
            duplicate_two_id, duplicate_two = client.request_message("transaction.prepare", duplicate_params, request_id="req:s9-duplicate-two")
            client.send(duplicate_one)
            client.send(duplicate_two)
            duplicate_responses = client.wait_responses({duplicate_one_id, duplicate_two_id})
            duplicate_one_result = expect_result(duplicate_responses[duplicate_one_id][0])
            duplicate_two_result = expect_result(duplicate_responses[duplicate_two_id][0])
            require(canonical(duplicate_one_result) == canonical(duplicate_two_result), "concurrent duplicate admission started a second preflight")

            cancel_params = prepare_params(100, phase_two_coordinates, phase_two_coordinates["root_node_id"], name="CancelledPreview")
            cancel_id, cancel_request = client.request_message("transaction.prepare", cancel_params, request_id="req:s9-cancelled")
            client.send(cancel_request)
            time.sleep(0.03)
            client.send(
                {
                    "protocol_version": "1.7",
                    "kind": "cancel",
                    "request_id": cancel_id,
                    "reason": "acceptance_cancel",
                    "context": client.context,
                }
            )
            expect_error(client.wait_response(cancel_id)[0], "cancelled")
            time.sleep(1.0)
            completed_after_cancel = expect_result(client.request("transaction.prepare", cancel_params)[0])
            validate_preview(completed_after_cancel)
            require(
                completed_after_cancel["coordinates"]["transaction_id"] != first["coordinates"]["transaction_id"],
                "new key at current revisions reused a stale transaction_id",
            )

            markers["oversized"].write_text("oversized\n", encoding="utf-8")
            phase_four = wait_for(phase_path, lambda value: value.get("phase") == 4, arguments.timeout)
            oversized = prepare_params(101, phase_two_coordinates, phase_two_coordinates["root_node_id"], name="TooLarge")
            expect_error(client.request("transaction.prepare", oversized)[0], "transaction_too_large")
            ping = expect_result(client.request("bridge.ping", {"echo": "responsive"})[0])
            require(ping.get("echo") == "responsive", "oversized preflight blocked the Bridge")

            evidence = {
                "schema_version": "s9-03-acceptance/1.0",
                "status": "passed",
                "protocol_version": "1.7",
                "capability_reason": "approval_unavailable",
                "initial_node_count": phase_one["node_count"],
                "manual_change_node_count": phase_two["node_count"],
                "bounded_node_count": phase_three["node_count"],
                "oversized_node_count": phase_four["node_count"],
                "prepared_capacity": 64,
                "concurrent_duplicate_joined": True,
                "operation_previews": 9,
                "operation_kinds": sorted({operation["kind"] for operation in operations} | {"create_node"}),
                "forbidden_property_error": "property_not_writable",
                "locked_instance_error": "node_not_editable",
                "incompatible_script_error": "script_incompatible",
                "replay_identical": True,
                "idempotency_conflict": True,
                "manual_invalidation": True,
                "cancelled_waiter_completed_in_background": True,
                "oversized_error": "transaction_too_large",
                "bridge_responsive_after_oversized": True,
                "read_only_evidence_equal": True,
                "native_tree_evidence_equal": True,
                "source_hashes": before_files,
            }
            arguments.evidence.parent.mkdir(parents=True, exist_ok=True)
            arguments.evidence.write_text(json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8")
            print(json.dumps(evidence, indent=2, sort_keys=True))
        except Exception:
            log.seek(0)
            output = log.read()[-12_000:]
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
            for path in [phase_path, *markers.values()]:
                path.unlink(missing_ok=True)
    require(file_hashes(project_root) == before_files, "editor fixture changed a source file")
    if temporary_project is not None:
        temporary_project.cleanup()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
