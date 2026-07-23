#!/usr/bin/env python3
"""Run the model-free Sprint 9 MCP editor transaction workflow."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import math
import os
import platform
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Mapping, cast

import transaction_fixture as oracle

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
FIXTURE_ROOT = SCRIPT_DIR / "fixtures" / "transaction_prepare_project"
GOLDEN_PATH = SCRIPT_DIR / "fixtures" / "transaction_oracle" / "golden-transactions.json"
MCP_PROTOCOL = "2025-11-25"
PRIVATE_SNAPSHOT = Path(".godot/codex-s9-oracle-private.json")
PHASE_PATH = Path(".godot/codex-s9-prepare-phase.json")
VERIFY_MARKER = Path(".godot/codex-s9-prepare-verify")
INTERVENING_MARKER = Path(".godot/codex-s9-prepare-change")
DONE_MARKER = Path(".godot/codex-s9-prepare-done")
NATIVE_UNDO_MARKER = Path(".godot/codex-s9-model-free-native-undo")
NATIVE_REDO_MARKER = Path(".godot/codex-s9-model-free-native-redo")
FAULT_ROOT = Path(".godot/codex/test-faults")
JOURNAL_PATH = Path(".godot/codex/transactions/journal-v1.json")
TRANSACTION_RE = re.compile(r"transaction:[0-9a-f]{32}\Z")
DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
BIND_CANARY = "S9_BIND_SECRET_SENTINEL"

TOOL_NAMES = {
    "godot_capture_viewport",
    "godot_continue_project",
    "godot_find_resource_owners",
    "godot_find_usages",
    "godot_get_current_scene",
    "godot_get_diagnostics",
    "godot_get_editor_history",
    "godot_get_editor_state",
    "godot_get_inspector_state",
    "godot_get_open_scenes",
    "godot_get_open_scripts",
    "godot_get_resource_dependencies",
    "godot_get_runtime_tree",
    "godot_get_selected_nodes",
    "godot_get_stack_trace",
    "godot_get_transaction_status",
    "godot_get_viewport_state",
    "godot_inspect_node",
    "godot_inspect_runtime_object",
    "godot_inspect_symbol",
    "godot_pause_project",
    "godot_prepare_attach_script",
    "godot_prepare_connect_signal",
    "godot_prepare_create_node",
    "godot_prepare_delete_node",
    "godot_prepare_detach_script",
    "godot_prepare_disconnect_signal",
    "godot_prepare_reparent_node",
    "godot_prepare_set_property",
    "godot_run_current_scene",
    "godot_run_project",
    "godot_search_symbols",
    "godot_stop_project",
    "godot_apply_transaction",
    "godot_undo_transaction",
    "godot_get_scene_graph",
}
MUTATING_TOOLS = {
    "godot_apply_transaction",
    "godot_continue_project",
    "godot_pause_project",
    "godot_prepare_attach_script",
    "godot_prepare_connect_signal",
    "godot_prepare_create_node",
    "godot_prepare_delete_node",
    "godot_prepare_detach_script",
    "godot_prepare_disconnect_signal",
    "godot_prepare_reparent_node",
    "godot_prepare_set_property",
    "godot_run_current_scene",
    "godot_run_project",
    "godot_stop_project",
    "godot_undo_transaction",
}
PREPARE_TOOLS = {
    "attach_script": "godot_prepare_attach_script",
    "connect_signal": "godot_prepare_connect_signal",
    "create_node": "godot_prepare_create_node",
    "delete_node": "godot_prepare_delete_node",
    "detach_script": "godot_prepare_detach_script",
    "disconnect_signal": "godot_prepare_disconnect_signal",
    "reparent_node": "godot_prepare_reparent_node",
    "set_property": "godot_prepare_set_property",
}
NATIVE_INTERVENING_FAMILIES = {
    "create_node",
    "set_property",
    "attach_script",
    "connect_signal",
}
FAULT_SCENARIOS = (
    "sidecar_disconnect_prepared",
    "bridge_response_loss_after_commit",
    "disconnect_before_commit",
    "editor_crash_before_commit",
    "editor_restart_after_commit",
    "corrupt_journal",
    "restart_after_bridge_response_before_journal_ack",
)


class WorkflowError(RuntimeError):
    """Raised when the live workflow violates an acceptance invariant."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise WorkflowError(message)


def strict_json_text(value: str) -> dict[str, Any]:
    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        output: dict[str, Any] = {}
        for key, child in items:
            require(key not in output, "MCP stdout contains a duplicate JSON member")
            output[key] = child
        return output

    def constant(name: str) -> None:
        raise WorkflowError(f"MCP stdout contains non-finite JSON: {name}")

    try:
        result = json.loads(value, object_pairs_hook=pairs, parse_constant=constant)
    except json.JSONDecodeError as error:
        raise WorkflowError("MCP stdout contains malformed JSON") from error
    require(isinstance(result, dict), "MCP stdout message is not an object")
    return cast(dict[str, Any], result)


def canonical(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")


def safe_digest(value: str | bytes) -> str:
    encoded = value.encode("utf-8") if isinstance(value, str) else value
    return "sha256:" + hashlib.sha256(encoded).hexdigest()


def percentile(values: list[float], percentile_value: int) -> float:
    require(bool(values), "latency population is empty")
    ordered = sorted(values)
    index = max(
        0,
        min(
            len(ordered) - 1,
            math.ceil(percentile_value * len(ordered) / 100) - 1,
        ),
    )
    return round(ordered[index], 3)


class LineProcess:
    def __init__(
        self,
        arguments: list[str],
        *,
        cwd: Path,
        environment: Mapping[str, str],
    ) -> None:
        self.stdout: queue.Queue[str] = queue.Queue()
        self.stderr_tail: list[str] = []
        self.process = subprocess.Popen(
            arguments,
            cwd=cwd,
            env=dict(environment),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        threading.Thread(target=self._read_stdout, daemon=True).start()
        threading.Thread(target=self._read_stderr, daemon=True).start()

    def _read_stdout(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            self.stdout.put(line)

    def _read_stderr(self) -> None:
        assert self.process.stderr is not None
        for line in self.process.stderr:
            self.stderr_tail.append(line.rstrip())
            del self.stderr_tail[:-200]

    def send(self, message: Mapping[str, Any]) -> None:
        stdin = self.process.stdin
        require(stdin is not None, "sidecar stdin is unavailable")
        stdin.write(canonical(message).decode("utf-8") + "\n")
        stdin.flush()

    def stop(self) -> None:
        if self.process.poll() is not None:
            return
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)


@dataclass
class ApprovalExpectation:
    transaction_id: str
    preview_digest: str
    scope: str
    risk: str
    operation_kind: str
    decision: str = "accept"
    calls: int = 0


class ModelFreeMcpClient:
    def __init__(
        self,
        process: LineProcess,
        *,
        project_root: Path,
        timeout: float,
        supports_form: bool = True,
    ) -> None:
        self.process = process
        self.project_root = project_root
        self.timeout = timeout
        self.supports_form = supports_form
        self.next_id = 1
        self.pending: dict[int, dict[str, Any]] = {}
        self.approval: ApprovalExpectation | None = None
        self.elicitation_total = 0
        self.raw_surfaces: list[dict[str, Any]] = []
        self.tool_surfaces: list[dict[str, Any]] = []
        self.transaction_surfaces: list[dict[str, Any]] = []

    def initialize(self) -> list[dict[str, Any]]:
        capabilities: dict[str, Any] = {}
        if self.supports_form:
            capabilities["elicitation"] = {
                "form": {
                    "schemaValidation": True,
                }
            }
        initialized = self.request(
            "initialize",
            {
                "protocolVersion": MCP_PROTOCOL,
                "capabilities": capabilities,
                "clientInfo": {
                    "name": "sprint9-model-free-gate",
                    "version": "1.0",
                },
            },
        )
        require(
            initialized.get("result", {}).get("protocolVersion") == MCP_PROTOCOL,
            "MCP protocol version differs",
        )
        self.notify("notifications/initialized", {})
        listed = self.request("tools/list", {})
        tools = listed.get("result", {}).get("tools")
        require(isinstance(tools, list), "MCP registry is missing")
        self._validate_tools(cast(list[dict[str, Any]], tools))
        return cast(list[dict[str, Any]], tools)

    def _validate_tools(self, tools: list[dict[str, Any]]) -> None:
        names = {tool.get("name") for tool in tools}
        require(names == TOOL_NAMES, f"MCP registry differs: {sorted(names ^ TOOL_NAMES)}")
        for tool in tools:
            name = str(tool.get("name"))
            schema = tool.get("inputSchema")
            annotations = tool.get("annotations")
            require(
                isinstance(schema, dict)
                and schema.get("type") == "object"
                and schema.get("additionalProperties") is False,
                f"{name} input schema is not closed",
            )
            require(isinstance(annotations, dict), f"{name} annotations are missing")
            read_only = annotations.get("readOnlyHint") is True
            require(
                read_only == (name not in MUTATING_TOOLS),
                f"{name} read-only annotation differs",
            )
            require(
                annotations.get("openWorldHint") is False,
                f"{name} open-world annotation differs",
            )

    def expect_approval(
        self,
        prepared: Mapping[str, Any],
        *,
        decision: str = "accept",
    ) -> ApprovalExpectation:
        require(self.approval is None, "another approval expectation is active")
        expectation = ApprovalExpectation(
            transaction_id=str(prepared["transaction_id"]),
            preview_digest=str(prepared["preview_digest"]),
            scope=str(prepared["scope"]),
            risk=str(prepared["risk"]),
            operation_kind=str(prepared["operation_kind"]),
            decision=decision,
        )
        self.approval = expectation
        return expectation

    def clear_approval(self) -> ApprovalExpectation | None:
        result = self.approval
        self.approval = None
        return result

    def notify(self, method: str, params: Mapping[str, Any]) -> None:
        self.process.send(
            {
                "jsonrpc": "2.0",
                "method": method,
                "params": dict(params),
            }
        )

    def request(
        self,
        method: str,
        params: Mapping[str, Any],
        *,
        timeout: float | None = None,
    ) -> dict[str, Any]:
        request_id = self.next_id
        self.next_id += 1
        self.process.send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": dict(params),
            }
        )
        deadline = time.monotonic() + (timeout or self.timeout)
        while time.monotonic() < deadline:
            response = self.pending.pop(request_id, None)
            if response is not None:
                self.raw_surfaces.append(response)
                return response
            if self.process.process.poll() is not None:
                raise WorkflowError(
                    "sidecar exited during MCP request: "
                    + " | ".join(self.process.stderr_tail[-10:])
                )
            try:
                line = self.process.stdout.get(timeout=0.1)
            except queue.Empty:
                continue
            message = strict_json_text(line)
            if message.get("method") == "elicitation/create":
                self._handle_elicitation(message)
            elif isinstance(message.get("id"), int):
                self.pending[int(message["id"])] = message
        raise WorkflowError(f"MCP request timed out: {method}")

    def _handle_elicitation(self, message: Mapping[str, Any]) -> None:
        request_id = message.get("id")
        params = message.get("params")
        require(isinstance(request_id, int), "elicitation request ID is malformed")
        require(isinstance(params, dict), "elicitation params are malformed")
        expectation = self.approval
        if expectation is None:
            self.process.send(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {
                        "code": -32600,
                        "message": "unsolicited elicitation",
                    },
                }
            )
            raise WorkflowError("sidecar sent unsolicited elicitation")
        require(expectation.calls == 0, "approval elicitation was repeated")
        require(params.get("mode") == "form", "approval is not form elicitation")
        schema = params.get("requestedSchema")
        require(isinstance(schema, dict), "approval schema is missing")
        properties = schema.get("properties")
        require(
            schema.get("type") == "object"
            and schema.get("required") == ["confirm"]
            and isinstance(properties, dict)
            and set(properties) == {"confirm"}
            and properties["confirm"].get("type") == "boolean"
            and "default" not in properties["confirm"],
            "approval schema is not exact",
        )
        message_text = params.get("message")
        require(
            isinstance(message_text, str)
            and len(message_text.encode("utf-8")) <= 8_192
            and f"Transaction: {expectation.transaction_id}" in message_text
            and f"Digest: {expectation.preview_digest}" in message_text
            and f"Scope: {expectation.scope}" in message_text
            and f"Risk: {expectation.risk}" in message_text
            and f'"operation_kind":"{expectation.operation_kind}"' in message_text,
            "approval message binding differs",
        )
        expectation.calls += 1
        self.elicitation_total += 1
        decision = expectation.decision
        if decision == "timeout":
            return
        if decision == "accept":
            result: dict[str, Any] = {
                "action": "accept",
                "content": {"confirm": True},
            }
        elif decision == "confirm_false":
            result = {
                "action": "accept",
                "content": {"confirm": False},
            }
        elif decision in {"decline", "cancel"}:
            result = {"action": decision}
        else:
            raise WorkflowError(f"unsupported test approval decision: {decision}")
        self.process.send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "result": result,
            }
        )

    def tool(
        self,
        name: str,
        arguments: Mapping[str, Any],
        *,
        timeout: float | None = None,
    ) -> tuple[dict[str, Any], bool, float]:
        started = time.monotonic()
        response = self.request(
            "tools/call",
            {
                "name": name,
                "arguments": dict(arguments),
            },
            timeout=timeout,
        )
        elapsed_ms = (time.monotonic() - started) * 1_000
        result = response.get("result")
        require(isinstance(result, dict), f"{name} omitted result")
        content = result.get("structuredContent")
        require(isinstance(content, dict), f"{name} omitted structured content")
        require(len(canonical(content)) <= 65_536, f"{name} exceeded 64 KiB")
        self.tool_surfaces.append(cast(dict[str, Any], content))
        if (
            name.startswith("godot_prepare_")
            or name
            in {
                "godot_apply_transaction",
                "godot_get_transaction_status",
                "godot_undo_transaction",
            }
        ):
            self.transaction_surfaces.append(cast(dict[str, Any], content))
        return cast(dict[str, Any], content), result.get("isError") is True, elapsed_ms


class FixtureSession:
    def __init__(
        self,
        *,
        godot: Path,
        sidecar: Path,
        timeout: float,
        supports_form: bool = True,
    ) -> None:
        self.godot = godot
        self.sidecar = sidecar
        self.timeout = timeout
        self.supports_form = supports_form
        self.temporary: tempfile.TemporaryDirectory[str] | None = None
        self.project_root: Path | None = None
        self.editor: subprocess.Popen[str] | None = None
        self.editor_log: Any = None
        self.sidecar_process: LineProcess | None = None
        self.client: ModelFreeMcpClient | None = None
        self.cleanup_ok = False
        self.frame_telemetry: dict[str, Any] | None = None

    def __enter__(self) -> FixtureSession:
        self.temporary = tempfile.TemporaryDirectory(prefix="s9-mcp-", dir="/tmp")
        self.project_root = Path(self.temporary.name) / "project"
        shutil.copytree(
            FIXTURE_ROOT,
            self.project_root,
            ignore=shutil.ignore_patterns(".godot"),
        )
        environment = os.environ.copy()
        environment["CODEX_S9_PREPARE_AUTOMATION"] = "1"
        environment["GODOT_CODEX_S9_MODEL_FREE_AUTOMATION"] = "1"
        log_path = Path(self.temporary.name) / "editor.log"
        self.editor_log = log_path.open("w+", encoding="utf-8")
        self.start_editor(environment)
        self.start_sidecar()
        return self

    def start_editor(
        self,
        environment: Mapping[str, str] | None = None,
    ) -> str:
        require(self.project_root is not None, "fixture project is unavailable")
        require(
            self.editor is None or self.editor.poll() is not None,
            "fixture editor is already running",
        )
        previous_session = ""
        discovery_path = self.project_root / ".godot/codex/bridge.json"
        if discovery_path.is_file():
            previous_session = str(
                oracle.strict_json(discovery_path).get("editor_session_id", "")
            )
        editor_environment = dict(environment or os.environ)
        editor_environment["CODEX_S9_PREPARE_AUTOMATION"] = "1"
        editor_environment["GODOT_CODEX_S9_MODEL_FREE_AUTOMATION"] = "1"
        editor_environment["GODOT_CODEX_EVIDENCE_TELEMETRY"] = "1"
        self.editor = subprocess.Popen(
            [
                str(self.godot),
                "--editor",
                "--headless",
                "--path",
                str(self.project_root),
            ],
            cwd=self.project_root,
            env=editor_environment,
            stdout=self.editor_log,
            stderr=subprocess.STDOUT,
            text=True,
        )
        wait_for_json(
            self.project_root / PHASE_PATH,
            lambda value: value.get("phase") == 1,
            self.timeout,
            "fixture did not reach phase one",
        )
        wait_for_json(
            discovery_path,
            lambda value: (
                isinstance(value.get("editor_session_id"), str)
                and value.get("editor_session_id") != previous_session
            ),
            self.timeout,
            "Bridge discovery did not appear",
        )
        return str(oracle.strict_json(discovery_path)["editor_session_id"])

    def stop_editor(self, *, graceful: bool) -> int:
        if self.editor is None:
            return 0
        if self.editor.poll() is None and graceful:
            require(self.project_root is not None, "fixture project is unavailable")
            done = self.project_root / DONE_MARKER
            done.parent.mkdir(parents=True, exist_ok=True)
            done.write_text("done\n", encoding="utf-8")
        if self.editor.poll() is None:
            try:
                if graceful:
                    self.editor.wait(timeout=10)
                else:
                    self.editor.terminate()
                    self.editor.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.editor.kill()
                self.editor.wait(timeout=5)
        return int(self.editor.returncode or 0)

    def start_sidecar(self) -> ModelFreeMcpClient:
        require(self.project_root is not None, "fixture project is unavailable")
        if self.sidecar_process is not None:
            self.sidecar_process.stop()
        environment = os.environ.copy()
        self.sidecar_process = LineProcess(
            [
                str(self.sidecar),
                "--project-root",
                str(self.project_root),
            ],
            cwd=self.project_root,
            environment=environment,
        )
        self.client = ModelFreeMcpClient(
            self.sidecar_process,
            project_root=self.project_root,
            timeout=self.timeout,
            supports_form=self.supports_form,
        )
        self.client.initialize()
        wait_for_live(self.client, self.timeout)
        return self.client

    def private_snapshot(
        self,
        golden: Mapping[str, Any],
        expected: Mapping[str, Any],
        *,
        trigger: bool,
    ) -> tuple[dict[str, Any], dict[str, Any]]:
        require(self.project_root is not None, "fixture project is unavailable")
        snapshot_path = self.project_root / PRIVATE_SNAPSHOT
        before_seq = -1
        if snapshot_path.is_file():
            before_seq = int(oracle.strict_json(snapshot_path).get("probe_seq", -1))
        if trigger:
            (self.project_root / VERIFY_MARKER).write_text("verify\n", encoding="utf-8")
        snapshot = wait_for_json(
            snapshot_path,
            lambda value: int(value.get("probe_seq", -1)) > before_seq
            if trigger
            else value.get("schema_version")
            == "s9-transaction-private-snapshot/1.0",
            self.timeout,
            "private oracle snapshot did not advance",
        )
        return oracle.normalize_private_snapshot(snapshot, golden, expected)

    def marker(self, marker: Path, previous_probe: int) -> dict[str, Any]:
        require(self.project_root is not None, "fixture project is unavailable")
        (self.project_root / marker).write_text("go\n", encoding="utf-8")
        return wait_for_json(
            self.project_root / PHASE_PATH,
            lambda value: int(value.get("probe_seq", -1)) > previous_probe,
            self.timeout,
            f"fixture marker {marker.name} was not observed",
        )

    def editor_tail(self) -> str:
        if self.editor_log is None:
            return ""
        self.editor_log.flush()
        self.editor_log.seek(0)
        return self.editor_log.read()[-16_000:]

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> None:
        if self.sidecar_process is not None:
            self.sidecar_process.stop()
        self.stop_editor(graceful=True)
        if self.editor_log is not None:
            self.editor_log.flush()
            self.editor_log.seek(0)
            for line in self.editor_log:
                if line.startswith("[codex_bridge_evidence] "):
                    self.frame_telemetry = strict_json_text(
                        line.removeprefix("[codex_bridge_evidence] ").strip()
                    )
        self.cleanup_ok = (
            self.editor is None
            or self.editor.poll() is not None
        ) and (
            self.sidecar_process is None
            or self.sidecar_process.process.poll() is not None
        )
        if exc is not None:
            tail = self.editor_tail()
            if tail:
                print(tail, file=sys.stderr)
            if self.sidecar_process is not None and self.sidecar_process.stderr_tail:
                print(
                    "\n".join(self.sidecar_process.stderr_tail[-80:]),
                    file=sys.stderr,
                )
        if self.editor_log is not None:
            self.editor_log.close()
        if self.temporary is not None:
            self.temporary.cleanup()


def wait_for_json(
    path: Path,
    predicate: Callable[[dict[str, Any]], bool],
    timeout: float,
    message: str,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        try:
            value = oracle.strict_json(path)
            last = value
            if predicate(value):
                return value
        except (OSError, oracle.FixtureError):
            pass
        time.sleep(0.05)
    raise WorkflowError(f"{message}: {last}")


def wait_for_live(client: ModelFreeMcpClient, timeout: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        content, error, _ = client.tool("godot_get_editor_state", {})
        last = content
        if not error and content.get("status") == "ready":
            return content
        time.sleep(0.05)
    raise WorkflowError(f"MCP live editor snapshot did not become ready: {last}")


def stable_read(
    client: ModelFreeMcpClient,
    timeout: float,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any], list[float]]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] = {}
    latencies: list[float] = []
    while time.monotonic() < deadline:
        current, current_error, elapsed = client.tool("godot_get_current_scene", {})
        latencies.append(elapsed)
        history, history_error, elapsed = client.tool(
            "godot_get_editor_history",
            {"limit": 200},
        )
        latencies.append(elapsed)
        state, state_error, elapsed = client.tool("godot_get_editor_state", {})
        latencies.append(elapsed)
        last = {"current": current, "history": history, "state": state}
        if current_error or history_error or state_error:
            time.sleep(0.05)
            continue
        snapshot_ids = {
            current.get("snapshot_id"),
            history.get("snapshot_id"),
            state.get("snapshot_id"),
        }
        if len(snapshot_ids) == 1 and None not in snapshot_ids:
            return current, history, state, latencies
        time.sleep(0.05)
    raise WorkflowError(f"MCP read snapshot did not stabilize: {last}")


def coordinates(
    current: Mapping[str, Any],
    history: Mapping[str, Any],
) -> dict[str, Any]:
    scene = current.get("scene")
    revisions = current.get("revision_vector")
    nodes = current.get("nodes")
    require(
        isinstance(scene, dict)
        and isinstance(revisions, dict)
        and isinstance(nodes, list),
        "current scene projection is incomplete",
    )
    scene_id = scene.get("entity_id")
    require(isinstance(scene_id, str), "current scene ID is missing")
    scene_revisions = revisions.get("scene_revisions")
    require(isinstance(scene_revisions, dict), "scene revision map is missing")
    node_ids = {
        item["node_path"]: item["entity_id"]
        for item in nodes
        if isinstance(item, dict)
        and isinstance(item.get("node_path"), str)
        and isinstance(item.get("entity_id"), str)
    }
    histories = history.get("histories")
    require(isinstance(histories, list), "editor history list is missing")
    scene_history = next(
        (
            item
            for item in histories
            if isinstance(item, dict) and item.get("scene_id") == scene_id
        ),
        None,
    )
    require(isinstance(scene_history, dict), "scene history is missing")
    return {
        "project_id": current["project_id"],
        "editor_session_id": current["editor_session_id"],
        "scene_id": scene_id,
        "history_id": scene_history["entity_id"],
        "scene_revision": scene_revisions[scene_id],
        "operation_seq": revisions["operation_seq"],
        "action_count": scene_history["action_count"],
        "node_ids": node_ids,
        "node_paths": set(node_ids),
        "nodes": {
            item["node_path"]: item
            for item in nodes
            if isinstance(item, dict) and isinstance(item.get("node_path"), str)
        },
    }


def common_prepare(
    coordinates_value: Mapping[str, Any],
    operation_kind: str,
) -> dict[str, Any]:
    key = hashlib.sha256(f"s9-model-free:{operation_kind}".encode()).hexdigest()[:32]
    return {
        "project_id": coordinates_value["project_id"],
        "editor_session_id": coordinates_value["editor_session_id"],
        "scene_id": coordinates_value["scene_id"],
        "history_id": coordinates_value["history_id"],
        "scene_revision": coordinates_value["scene_revision"],
        "operation_seq": coordinates_value["operation_seq"],
        "idempotency_key": f"idempotency:{key}",
    }


def operation_arguments(
    operation_kind: str,
    coordinates_value: Mapping[str, Any],
) -> dict[str, Any]:
    node_ids = cast(Mapping[str, str], coordinates_value["node_ids"])
    common = common_prepare(coordinates_value, operation_kind)
    details: dict[str, dict[str, Any]] = {
        "create_node": {
            "parent_node_id": node_ids["."],
            "godot_type": "Node",
            "name": "OracleCreated",
        },
        "delete_node": {
            "node_id": node_ids["Deletable"],
        },
        "reparent_node": {
            "node_id": node_ids["ReparentTarget"],
            "new_parent_node_id": node_ids["NewParent"],
            "insertion_index": 0,
            "keep_global_transform": False,
        },
        "set_property": {
            "node_id": node_ids["Player"],
            "property": "position",
            "value": {
                "type": "vector2",
                "value": [20.0, 30.0],
            },
        },
        "attach_script": {
            "node_id": node_ids["Scriptless"],
            "script_ref": {
                "uid_missing": True,
                "path": "res://scripts/fixture_endpoint.gd",
            },
        },
        "detach_script": {
            "node_id": node_ids["Scripted"],
        },
        "connect_signal": {
            "emitter_node_id": node_ids["Emitter"],
            "signal": "pulse",
            "receiver_node_id": node_ids["Receiver"],
            "method": "_on_bound",
            "flags": 3,
            "unbinds": 0,
            "binds": [
                {
                    "type": "string",
                    "value": BIND_CANARY,
                }
            ],
        },
        "disconnect_signal": {
            "emitter_node_id": node_ids["Emitter"],
            "signal": "pulse",
            "receiver_node_id": node_ids["Receiver"],
            "method": "_on_pulse",
            "flags": 2,
            "unbinds": 0,
            "binds": [],
        },
    }
    return {**common, **details[operation_kind]}


def validate_prepared(
    prepared: Mapping[str, Any],
    operation_truth: Mapping[str, Any],
    coordinates_value: Mapping[str, Any],
) -> None:
    require(
        set(operation_truth["states"])
        == {"pre", "prepared", "applied", "undone", "redone"},
        "golden lifecycle states differ",
    )
    require(
        prepared.get("state") == "previewed"
        and prepared.get("operation_kind") == operation_truth["kind"]
        and prepared.get("scope") == operation_truth["scope"]
        and prepared.get("risk") == operation_truth["risk"],
        f"{operation_truth['kind']} prepared projection differs",
    )
    require(
        isinstance(prepared.get("transaction_id"), str)
        and TRANSACTION_RE.fullmatch(str(prepared["transaction_id"])) is not None,
        "prepared transaction ID is malformed",
    )
    require(
        isinstance(prepared.get("preview_digest"), str)
        and DIGEST_RE.fullmatch(str(prepared["preview_digest"])) is not None,
        "preview digest is malformed",
    )
    require(
        prepared.get("scene_id") == coordinates_value["scene_id"]
        and prepared.get("scene_revision") == coordinates_value["scene_revision"]
        and prepared.get("operation_seq") == coordinates_value["operation_seq"],
        "prepared coordinates differ",
    )
    preview = prepared.get("preview")
    require(
        isinstance(preview, dict)
        and preview.get("operation_kind") == operation_truth["kind"]
        and preview.get("truncated") is False,
        "prepared preview differs",
    )


def wait_status(
    client: ModelFreeMcpClient,
    transaction_id: str,
    state: str,
    timeout: float,
) -> tuple[dict[str, Any], list[float]]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    latencies: list[float] = []
    while time.monotonic() < deadline:
        content, error, elapsed = client.tool(
            "godot_get_transaction_status",
            {"transaction_id": transaction_id},
        )
        latencies.append(elapsed)
        last = content
        if not error and content.get("state") == state:
            return content, latencies
        time.sleep(0.05)
    raise WorkflowError(f"transaction did not reach {state}: {last}")


def wait_status_one_of(
    client: ModelFreeMcpClient,
    transaction_id: str,
    states: set[str],
    timeout: float,
) -> tuple[dict[str, Any], list[float]]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    latencies: list[float] = []
    while time.monotonic() < deadline:
        content, error, elapsed = client.tool(
            "godot_get_transaction_status",
            {"transaction_id": transaction_id},
        )
        latencies.append(elapsed)
        last = content
        if not error and content.get("state") in states:
            return content, latencies
        if error and "error" in states:
            return content, latencies
        time.sleep(0.05)
    raise WorkflowError(f"transaction did not reach {sorted(states)}: {last}")


def trigger_private(
    session: FixtureSession,
    golden: Mapping[str, Any],
    expected: Mapping[str, Any],
) -> tuple[dict[str, Any], dict[str, Any]]:
    return session.private_snapshot(golden, expected, trigger=True)


def read_coordinates(
    client: ModelFreeMcpClient,
    timeout: float,
) -> tuple[dict[str, Any], dict[str, Any], list[float]]:
    current, history, state, latencies = stable_read(client, timeout)
    return coordinates(current, history), state, latencies


def wait_operation_seq(
    client: ModelFreeMcpClient,
    minimum: int,
    timeout: float,
) -> tuple[dict[str, Any], list[float]]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    samples: list[float] = []
    while time.monotonic() < deadline:
        observed, _, latency = read_coordinates(client, min(timeout, 5.0))
        samples.extend(latency)
        last = observed
        if int(observed["operation_seq"]) >= minimum:
            return observed, samples
        time.sleep(0.05)
    raise WorkflowError(f"operation sequence did not reach {minimum}: {last}")


def expected_operation(
    golden: Mapping[str, Any],
    kind: str,
) -> Mapping[str, Any]:
    operation = next(
        item
        for item in cast(list[Mapping[str, Any]], golden["operations"])
        if item["kind"] == kind
    )
    return operation


def with_player_position(
    state: Mapping[str, Any],
    position: tuple[int, int],
) -> dict[str, Any]:
    updated = cast(dict[str, Any], copy.deepcopy(state))
    for node in cast(list[dict[str, Any]], updated["tree"]):
        if node.get("path") != "Player":
            continue
        transform = node.get("transform")
        require(isinstance(transform, dict), "Player transform is missing")
        for key in ("local", "global"):
            values = transform.get(key)
            require(
                isinstance(values, list) and len(values) == 6,
                "Player transform projection differs",
            )
            values[-2:] = list(position)
    for property_value in cast(list[dict[str, Any]], updated["properties"]):
        if (
            property_value.get("path") == "Player"
            and property_value.get("name") == "position"
        ):
            property_value["value"] = list(position)
            break
    return updated


def validate_mcp_readback(
    kind: str,
    applied: Mapping[str, Any],
    undone: Mapping[str, Any],
) -> None:
    applied_paths = cast(set[str], applied["node_paths"])
    undone_paths = cast(set[str], undone["node_paths"])
    if kind == "create_node":
        require(
            "OracleCreated" in applied_paths and "OracleCreated" not in undone_paths,
            "create readback differs",
        )
    elif kind == "delete_node":
        require(
            "Deletable" not in applied_paths and "Deletable" in undone_paths,
            "delete readback differs",
        )
    elif kind == "reparent_node":
        require(
            "NewParent/ReparentTarget" in applied_paths
            and "ReparentTarget" in undone_paths,
            "reparent readback differs",
        )
    elif kind == "attach_script":
        require(
            applied["nodes"]["Scriptless"].get("script_path")
            == "res://scripts/fixture_endpoint.gd"
            and undone["nodes"]["Scriptless"].get("script_path") == "",
            "attach-script readback differs",
        )
    elif kind == "detach_script":
        require(
            applied["nodes"]["Scripted"].get("script_path") == ""
            and undone["nodes"]["Scripted"].get("script_path")
            == "res://scripts/fixture_endpoint.gd",
            "detach-script readback differs",
        )
    else:
        require(
            int(applied["scene_revision"]) > int(undone["scene_revision"]) - 2,
            f"{kind} revision readback differs",
        )


def run_operation(
    kind: str,
    *,
    godot: Path,
    sidecar: Path,
    timeout: float,
    golden: Mapping[str, Any],
) -> dict[str, Any]:
    truth = expected_operation(golden, kind)
    baseline = cast(Mapping[str, Any], golden["baseline"])
    expected_applied = oracle.materialize_operation(baseline, truth)
    all_latencies: dict[str, list[float]] = {
        "read": [],
        "prepare": [],
        "apply": [],
        "status": [],
        "undo": [],
    }
    session = FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    with session:
        require(session.project_root is not None, "fixture project is unavailable")
        require(session.client is not None, "MCP client is unavailable")
        client = session.client
        source_before = oracle.source_fingerprint(session.project_root)
        initial_phase = oracle.strict_json(session.project_root / PHASE_PATH)
        pre, pre_native = session.private_snapshot(golden, baseline, trigger=False)
        before, _, latencies = read_coordinates(client, timeout)
        all_latencies["read"].extend(latencies)
        require(
            int(before["operation_seq"]) == 0 and int(before["action_count"]) == 0,
            f"{kind} fixture history is not empty",
        )

        arguments = operation_arguments(kind, before)
        prepared, error, elapsed = client.tool(PREPARE_TOOLS[kind], arguments)
        all_latencies["prepare"].append(elapsed)
        require(not error, f"{kind} prepare failed: {prepared}")
        validate_prepared(prepared, truth, before)
        prepared_state, prepared_native = trigger_private(session, golden, baseline)
        prepared_read, _, latencies = read_coordinates(client, timeout)
        all_latencies["read"].extend(latencies)
        require(
            oracle.canonical(pre) == oracle.canonical(prepared_state),
            f"{kind} prepare mutated the private scene oracle",
        )
        require(
            pre_native == prepared_native,
            f"{kind} prepare mutated native history or selection",
        )
        require(
            prepared_read["scene_revision"] == before["scene_revision"]
            and prepared_read["operation_seq"] == before["operation_seq"]
            and prepared_read["action_count"] == before["action_count"],
            f"{kind} prepare changed live revisions/history",
        )
        oracle.assert_source_unchanged(
            source_before,
            oracle.source_fingerprint(session.project_root),
        )

        client.expect_approval(prepared)
        applied_status, error, elapsed = client.tool(
            "godot_apply_transaction",
            {
                "transaction_id": prepared["transaction_id"],
                "preview_digest": prepared["preview_digest"],
                "expected_scene_revision": prepared["scene_revision"],
                "expected_operation_seq": prepared["operation_seq"],
            },
            timeout=max(timeout, 10.0),
        )
        all_latencies["apply"].append(elapsed)
        approval = client.clear_approval()
        require(
            not error
            and applied_status.get("state") == "committed"
            and approval is not None
            and approval.calls == 1,
            f"{kind} apply/approval failed: {applied_status}",
        )
        applied_read, latencies = wait_operation_seq(
            client,
            int(before["operation_seq"]) + 1,
            timeout,
        )
        all_latencies["read"].extend(latencies)
        applied_state, _ = trigger_private(session, golden, expected_applied)

        status, latencies = wait_status(
            client,
            str(prepared["transaction_id"]),
            "committed",
            timeout,
        )
        all_latencies["status"].extend(latencies)
        require(
            status["current_scene_revision"] == applied_read["scene_revision"]
            and status["current_operation_seq"] == applied_read["operation_seq"],
            f"{kind} status/readback coordinates differ",
        )

        undone_status, error, elapsed = client.tool(
            "godot_undo_transaction",
            {
                "transaction_id": status["transaction_id"],
                "expected_transaction_seq": status["transaction_seq"],
                "expected_scene_revision": status["current_scene_revision"],
                "expected_operation_seq": status["current_operation_seq"],
            },
        )
        all_latencies["undo"].append(elapsed)
        require(
            not error and undone_status.get("state") == "undone",
            f"{kind} targeted Undo failed: {undone_status}",
        )
        undone_read, latencies = wait_operation_seq(
            client,
            int(applied_read["operation_seq"]) + 1,
            timeout,
        )
        all_latencies["read"].extend(latencies)
        undone_state, _ = trigger_private(session, golden, baseline)

        (session.project_root / NATIVE_REDO_MARKER).write_text(
            "redo\n",
            encoding="utf-8",
        )
        redone_status, latencies = wait_status(
            client,
            str(prepared["transaction_id"]),
            "committed",
            timeout,
        )
        all_latencies["status"].extend(latencies)
        redone_read, latencies = wait_operation_seq(
            client,
            int(undone_read["operation_seq"]) + 1,
            timeout,
        )
        all_latencies["read"].extend(latencies)
        redone_state, _ = trigger_private(session, golden, expected_applied)
        require(
            redone_status["current_operation_seq"] == redone_read["operation_seq"],
            f"{kind} native Redo status/readback differ",
        )

        oracle.validate_cycle(
            kind,
            {
                "pre": pre,
                "prepared": prepared_state,
                "applied": applied_state,
                "undone": undone_state,
                "redone": redone_state,
            },
            [
                {"operation_seq": int(before["operation_seq"])},
                {"operation_seq": int(prepared_read["operation_seq"])},
                {"operation_seq": int(applied_read["operation_seq"])},
                {"operation_seq": int(undone_read["operation_seq"])},
                {"operation_seq": int(redone_read["operation_seq"])},
            ],
            golden,
        )
        native_intervening = kind in NATIVE_INTERVENING_FAMILIES
        intervening_actions = 0
        if native_intervening:
            (session.project_root / NATIVE_UNDO_MARKER).write_text(
                "undo\n",
                encoding="utf-8",
            )
            native_undone_read, latencies = wait_operation_seq(
                client,
                int(redone_read["operation_seq"]) + 1,
                timeout,
            )
            all_latencies["read"].extend(latencies)
            native_undone_status, latencies = wait_status(
                client,
                str(prepared["transaction_id"]),
                "undone",
                timeout,
            )
            all_latencies["status"].extend(latencies)
            native_undone_state, _ = trigger_private(session, golden, baseline)
            require(
                oracle.canonical(native_undone_state) == oracle.canonical(baseline)
                and native_undone_status["current_operation_seq"]
                == native_undone_read["operation_seq"],
                f"{kind} native Undo did not restore pre-state",
            )

            (session.project_root / NATIVE_REDO_MARKER).write_text(
                "redo\n",
                encoding="utf-8",
            )
            native_redone_read, latencies = wait_operation_seq(
                client,
                int(native_undone_read["operation_seq"]) + 1,
                timeout,
            )
            all_latencies["read"].extend(latencies)
            native_redone_status, latencies = wait_status(
                client,
                str(prepared["transaction_id"]),
                "committed",
                timeout,
            )
            all_latencies["status"].extend(latencies)
            native_redone_state, _ = trigger_private(
                session,
                golden,
                expected_applied,
            )
            require(
                oracle.canonical(native_redone_state)
                == oracle.canonical(expected_applied)
                and native_redone_status["current_operation_seq"]
                == native_redone_read["operation_seq"],
                f"{kind} native Redo did not restore applied state",
            )

            previous_probe = int(
                oracle.strict_json(session.project_root / PHASE_PATH)["probe_seq"]
            )
            intervening_phase = session.marker(INTERVENING_MARKER, previous_probe)
            intervening_read, latencies = wait_operation_seq(
                client,
                int(native_redone_read["operation_seq"]) + 1,
                timeout,
            )
            all_latencies["read"].extend(latencies)
            intervening_status, latencies = wait_status(
                client,
                str(prepared["transaction_id"]),
                "committed",
                timeout,
            )
            all_latencies["status"].extend(latencies)
            require(
                intervening_phase.get("position") == [9.0, 13.0]
                and intervening_status.get("undo_eligibility", {}).get("reason")
                == "not_newest_action",
                f"{kind} intervening action did not block targeted Undo",
            )
            blocked, error, elapsed = client.tool(
                "godot_undo_transaction",
                {
                    "transaction_id": intervening_status["transaction_id"],
                    "expected_transaction_seq": intervening_status[
                        "transaction_seq"
                    ],
                    "expected_scene_revision": intervening_status[
                        "current_scene_revision"
                    ],
                    "expected_operation_seq": intervening_status[
                        "current_operation_seq"
                    ],
                },
            )
            all_latencies["undo"].append(elapsed)
            require(
                error
                and blocked.get("error", {}).get("code")
                == "transaction_not_undoable",
                f"{kind} targeted Undo crossed an intervening action",
            )
            blocked_state, _ = trigger_private(
                session,
                golden,
                with_player_position(expected_applied, (9, 13)),
            )
            require(
                oracle.canonical(blocked_state)
                == oracle.canonical(with_player_position(expected_applied, (9, 13))),
                f"{kind} blocked targeted Undo mutated editor state",
            )
            intervening_actions = 1

            (session.project_root / NATIVE_UNDO_MARKER).write_text(
                "undo\n",
                encoding="utf-8",
            )
            after_intervening_undo, latencies = wait_operation_seq(
                client,
                int(intervening_read["operation_seq"]) + 1,
                timeout,
            )
            all_latencies["read"].extend(latencies)
            current_status, latencies = wait_status(
                client,
                str(prepared["transaction_id"]),
                "committed",
                timeout,
            )
            all_latencies["status"].extend(latencies)
            restored_applied, _ = trigger_private(
                session,
                golden,
                expected_applied,
            )
            require(
                oracle.canonical(restored_applied)
                == oracle.canonical(expected_applied),
                f"{kind} native Undo of the intervening action changed transaction",
            )

            targeted, error, elapsed = client.tool(
                "godot_undo_transaction",
                {
                    "transaction_id": current_status["transaction_id"],
                    "expected_transaction_seq": current_status["transaction_seq"],
                    "expected_scene_revision": current_status[
                        "current_scene_revision"
                    ],
                    "expected_operation_seq": current_status[
                        "current_operation_seq"
                    ],
                },
            )
            all_latencies["undo"].append(elapsed)
            require(
                not error and targeted.get("state") == "undone",
                f"{kind} targeted Undo failed after removing intervening action",
            )
            final_undone_read, latencies = wait_operation_seq(
                client,
                int(after_intervening_undo["operation_seq"]) + 1,
                timeout,
            )
            all_latencies["read"].extend(latencies)
            final_undone, _ = trigger_private(session, golden, baseline)
            require(
                oracle.canonical(final_undone) == oracle.canonical(baseline),
                f"{kind} final targeted Undo is not exact",
            )

            (session.project_root / NATIVE_REDO_MARKER).write_text(
                "redo\n",
                encoding="utf-8",
            )
            final_redone_read, latencies = wait_operation_seq(
                client,
                int(final_undone_read["operation_seq"]) + 1,
                timeout,
            )
            all_latencies["read"].extend(latencies)
            final_status, latencies = wait_status(
                client,
                str(prepared["transaction_id"]),
                "committed",
                timeout,
            )
            all_latencies["status"].extend(latencies)
            final_redone, _ = trigger_private(
                session,
                golden,
                expected_applied,
            )
            require(
                oracle.canonical(final_redone)
                == oracle.canonical(expected_applied)
                and final_status["current_operation_seq"]
                == final_redone_read["operation_seq"],
                f"{kind} final native Redo lost transaction identity",
            )
        validate_mcp_readback(kind, applied_read, undone_read)
        oracle.assert_source_unchanged(
            source_before,
            oracle.source_fingerprint(session.project_root),
        )
        native_ids = [
            *cast(list[Any], pre_native.get("object_ids", [])),
            *cast(list[Any], pre_native.get("selected_object_ids", [])),
        ]
        for surface in client.tool_surfaces:
            oracle.scan_safe_surface(
                "mcp_results",
                surface,
                native_ids=native_ids,
                absolute_paths=[session.project_root],
                check_forbidden_keys=False,
            )
        for surface in client.transaction_surfaces:
            oracle.scan_safe_surface(
                "mcp_transaction_results",
                surface,
                native_ids=native_ids,
                absolute_paths=[session.project_root],
            )
        require(
            BIND_CANARY not in session.editor_tail()
            and BIND_CANARY
            not in "\n".join(
                session.sidecar_process.stderr_tail
                if session.sidecar_process is not None
                else []
            ),
            f"{kind} leaked the bind canary to logs",
        )
        require(
            int(
                oracle.strict_json(session.project_root / PHASE_PATH)["probe_seq"]
            )
            > int(initial_phase["probe_seq"]),
            "fixture private probe sequence did not advance",
        )

    require(session.cleanup_ok, f"{kind} left a live child process")
    require(
        isinstance(session.frame_telemetry, dict),
        f"{kind} frame telemetry is missing",
    )
    frame_telemetry = cast(Mapping[str, Any], session.frame_telemetry)
    return {
        "operation": kind,
        "transaction_sha256": safe_digest(str(prepared["transaction_id"])),
        "elicitation_count": 1,
        "native_actions": 1 + intervening_actions,
        "transaction_native_actions": 1,
        "native_undo_redo": native_intervening,
        "intervening_action": native_intervening,
        "frame_telemetry": {
            "budget_usec": frame_telemetry.get("budget_usec"),
            "max_elapsed_usec": frame_telemetry.get(
                "dispatcher_max_elapsed_usec"
            ),
            "over_budget_count": frame_telemetry.get(
                "dispatcher_over_budget_count"
            ),
            "frame_max_elapsed_usec": frame_telemetry.get("max_elapsed_usec"),
        },
        "source_unchanged": True,
        "oracle_cycle": True,
        "cleanup": True,
        "latency_ms": {
            name: {
                "count": len(values),
                "p50": percentile(values, 50),
                "p95": percentile(values, 95),
                "max": round(max(values), 3),
            }
            for name, values in all_latencies.items()
            if values
        },
    }


def run_idempotency_negative(
    *,
    godot: Path,
    sidecar: Path,
    timeout: float,
) -> dict[str, Any]:
    session = FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    with session:
        require(session.client is not None, "MCP client is unavailable")
        client = session.client
        before, _, _ = read_coordinates(client, timeout)
        arguments = operation_arguments("create_node", before)
        first, error, _ = client.tool("godot_prepare_create_node", arguments)
        require(not error, f"idempotency prepare failed: {first}")
        replay, error, _ = client.tool("godot_prepare_create_node", arguments)
        require(
            not error
            and replay["transaction_id"] == first["transaction_id"]
            and replay["preview_digest"] == first["preview_digest"],
            "idempotent prepare did not replay the immutable preview",
        )
        conflict_arguments = dict(arguments)
        conflict_arguments["name"] = "DifferentName"
        conflict, error, _ = client.tool(
            "godot_prepare_create_node",
            conflict_arguments,
        )
        require(
            error and conflict.get("error", {}).get("code") == "idempotency_conflict",
            f"idempotency conflict was not rejected: {conflict}",
        )
        for field, replacement in (
            ("project_id", "project:sha256:" + "f" * 64),
            ("editor_session_id", "editor:" + "f" * 32),
            ("scene_revision", int(before["scene_revision"]) + 1),
        ):
            stale_arguments = dict(arguments)
            stale_arguments["idempotency_key"] = (
                "idempotency:"
                + hashlib.sha256(f"negative:{field}".encode()).hexdigest()[:32]
            )
            stale_arguments[field] = replacement
            rejected, stale_error, _ = client.tool(
                "godot_prepare_create_node",
                stale_arguments,
            )
            require(
                stale_error
                and isinstance(rejected.get("error", {}).get("code"), str)
                and client.elicitation_total == 0,
                f"{field} mismatch reached approval or executor: {rejected}",
            )
        tampered, error, _ = client.tool(
            "godot_apply_transaction",
            {
                "transaction_id": first["transaction_id"],
                "preview_digest": "sha256:" + "f" * 64,
                "expected_scene_revision": first["scene_revision"],
                "expected_operation_seq": first["operation_seq"],
            },
        )
        require(
            error
            and tampered.get("error", {}).get("code") == "preview_mismatch"
            and client.elicitation_total == 0,
            f"tampered digest reached approval/apply: {tampered}",
        )
        for field in ("expected_scene_revision", "expected_operation_seq"):
            stale_apply = {
                "transaction_id": first["transaction_id"],
                "preview_digest": first["preview_digest"],
                "expected_scene_revision": first["scene_revision"],
                "expected_operation_seq": first["operation_seq"],
            }
            stale_apply[field] = int(stale_apply[field]) + 1
            rejected, stale_error, _ = client.tool(
                "godot_apply_transaction",
                stale_apply,
            )
            require(
                stale_error
                and rejected.get("error", {}).get("code")
                in {"stale_scene_revision", "stale_editor_state"}
                and client.elicitation_total == 0,
                f"{field} mismatch reached approval: {rejected}",
            )
        after, _, _ = read_coordinates(client, timeout)
        require(
            after["operation_seq"] == before["operation_seq"]
            and after["action_count"] == before["action_count"],
            "pre-approval negatives created a native action",
        )
    require(session.cleanup_ok, "idempotency scenario left a child process")
    return {
        "idempotent_reprepare": True,
        "idempotency_conflict": True,
        "tampered_digest_preapproval_rejection": True,
        "wrong_binding_preapproval_rejection": True,
        "wrong_revision_preapproval_rejection": True,
    }


def fault_marker(
    project_root: Path,
    *,
    case_id: str,
    transaction_id: str,
    point: str,
    mode: str,
    state: str,
) -> Path:
    root = project_root / FAULT_ROOT
    root.mkdir(parents=True, exist_ok=True)
    path = root / ("release.json" if state == "released" else "armed.json")
    temporary = path.with_suffix(".json.tmp")
    temporary.write_text(
        json.dumps(
            {
                "schema_version": "s9-transaction-fault/1.0",
                "case_id": case_id,
                "transaction_id": transaction_id,
                "point": point,
                "mode": mode,
                "state": state,
            },
            ensure_ascii=False,
            separators=(",", ":"),
        ),
        encoding="utf-8",
    )
    temporary.replace(path)
    return path


def wait_fault_reached(
    project_root: Path,
    *,
    case_id: str,
    transaction_id: str,
    point: str,
    mode: str,
    timeout: float,
) -> dict[str, Any]:
    return wait_for_json(
        project_root / FAULT_ROOT / "reached.json",
        lambda value: (
            value.get("schema_version") == "s9-transaction-fault/1.0"
            and value.get("case_id") == case_id
            and value.get("transaction_id") == transaction_id
            and value.get("point") == point
            and value.get("mode") == mode
            and value.get("state") == "reached"
        ),
        timeout,
        f"fault marker {case_id} did not reach {point}",
    )


def apply_in_background(
    client: ModelFreeMcpClient,
    prepared: Mapping[str, Any],
    *,
    timeout: float,
) -> tuple[threading.Thread, dict[str, Any]]:
    outcome: dict[str, Any] = {}
    client.expect_approval(prepared)

    def invoke() -> None:
        try:
            result, error, elapsed = client.tool(
                "godot_apply_transaction",
                {
                    "transaction_id": prepared["transaction_id"],
                    "preview_digest": prepared["preview_digest"],
                    "expected_scene_revision": prepared["scene_revision"],
                    "expected_operation_seq": prepared["operation_seq"],
                },
                timeout=timeout,
            )
            outcome.update(result=result, error=error, elapsed_ms=elapsed)
        except Exception as error:  # The fault can intentionally sever stdio.
            outcome["exception"] = type(error).__name__
        finally:
            outcome["approval"] = client.clear_approval()

    worker = threading.Thread(target=invoke, daemon=True)
    worker.start()
    return worker, outcome


def run_sidecar_disconnect_prepared(
    *,
    godot: Path,
    sidecar: Path,
    timeout: float,
) -> dict[str, Any]:
    session = FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    with session:
        require(session.client is not None, "MCP client is unavailable")
        before, _, _ = read_coordinates(session.client, timeout)
        arguments = operation_arguments("create_node", before)
        prepared, error, _ = session.client.tool(
            "godot_prepare_create_node",
            arguments,
        )
        require(not error, f"prepared disconnect setup failed: {prepared}")
        require(session.sidecar_process is not None, "sidecar process is unavailable")
        session.sidecar_process.stop()
        client = session.start_sidecar()
        replay, replay_error, _ = client.tool(
            "godot_prepare_create_node",
            arguments,
        )
        require(
            not replay_error
            and replay["transaction_id"] == prepared["transaction_id"]
            and replay["preview_digest"] == prepared["preview_digest"],
            f"prepared preview was not recovered immutably: {replay}",
        )
        after, _, _ = read_coordinates(client, timeout)
        require(
            after["operation_seq"] == before["operation_seq"]
            and after["action_count"] == before["action_count"],
            "sidecar prepared reconnect mutated the editor",
        )
    require(session.cleanup_ok, "prepared disconnect left a child process")
    return {
        "same_transaction": True,
        "same_preview_digest": True,
        "native_actions": 0,
        "source_unchanged": True,
    }


def run_committed_replay_negative(
    *,
    godot: Path,
    sidecar: Path,
    timeout: float,
) -> dict[str, Any]:
    session = FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    with session:
        require(session.client is not None, "MCP client is unavailable")
        client = session.client
        before, _, _ = read_coordinates(client, timeout)
        prepared, error, _ = client.tool(
            "godot_prepare_create_node",
            operation_arguments("create_node", before),
        )
        require(not error, f"replay prepare failed: {prepared}")
        apply_arguments = {
            "transaction_id": prepared["transaction_id"],
            "preview_digest": prepared["preview_digest"],
            "expected_scene_revision": prepared["scene_revision"],
            "expected_operation_seq": prepared["operation_seq"],
        }
        client.expect_approval(prepared)
        committed, error, _ = client.tool(
            "godot_apply_transaction",
            apply_arguments,
        )
        expectation = client.clear_approval()
        require(
            not error
            and committed.get("state") == "committed"
            and expectation is not None
            and expectation.calls == 1,
            f"replay baseline apply failed: {committed}",
        )
        applied, _ = wait_operation_seq(
            client,
            int(before["operation_seq"]) + 1,
            timeout,
        )
        replay_arguments = dict(apply_arguments)
        replay_arguments["expected_scene_revision"] = applied["scene_revision"]
        replay_arguments["expected_operation_seq"] = applied["operation_seq"]
        replayed, replay_error, _ = client.tool(
            "godot_apply_transaction",
            replay_arguments,
        )
        require(
            (
                not replay_error
                and replayed.get("state") == "committed"
            )
            or (
                replay_error
                and replayed.get("error", {}).get("code")
                in {
                    "transaction_apply_replay_forbidden",
                    "transaction_in_doubt",
                }
            ),
            f"committed apply replay was not latched: {replayed}",
        )
        after, _, _ = read_coordinates(client, timeout)
        require(
            after["operation_seq"] == applied["operation_seq"]
            and after["action_count"] == applied["action_count"]
            and client.elicitation_total == 1,
            "committed apply replay created approval or a second native action",
        )
    require(session.cleanup_ok, "replay scenario left a child process")
    return {
        "latched_status_or_replay_forbidden": True,
        "elicitation_count": 1,
        "transaction_native_actions": 1,
    }


def run_expired_preview_negative(
    *,
    godot: Path,
    sidecar: Path,
    timeout: float,
) -> dict[str, Any]:
    session = FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    with session:
        require(session.project_root is not None, "fixture project is unavailable")
        require(session.client is not None, "MCP client is unavailable")
        client = session.client
        before, _, _ = read_coordinates(client, timeout)
        prepared, error, _ = client.tool(
            "godot_prepare_create_node",
            operation_arguments("create_node", before),
        )
        require(not error, f"expiry prepare failed: {prepared}")
        require(session.sidecar_process is not None, "sidecar process is unavailable")
        session.sidecar_process.stop()
        journal_path = (
            session.project_root
            / ".godot/codex/transactions/journal-v1.json"
        )
        document = oracle.strict_json(journal_path)
        records = document.get("records")
        require(isinstance(records, list), "transaction journal records are missing")
        record = next(
            (
                item
                for item in records
                if isinstance(item, dict)
                and item.get("transaction_id") == prepared["transaction_id"]
            ),
            None,
        )
        require(isinstance(record, dict), "prepared journal record is missing")
        record["expires_at_ms"] = record["created_at_ms"]
        temporary = journal_path.with_suffix(".json.expiry.tmp")
        temporary.write_text(
            json.dumps(document, ensure_ascii=False, separators=(",", ":")),
            encoding="utf-8",
        )
        temporary.chmod(0o600)
        temporary.replace(journal_path)
        client = session.start_sidecar()
        expired, expired_error, _ = client.tool(
            "godot_apply_transaction",
            {
                "transaction_id": prepared["transaction_id"],
                "preview_digest": prepared["preview_digest"],
                "expected_scene_revision": prepared["scene_revision"],
                "expected_operation_seq": prepared["operation_seq"],
            },
        )
        require(
            expired_error
            and expired.get("error", {}).get("code") == "transaction_expired"
            and client.elicitation_total == 0,
            f"expired preview reached approval or executor: {expired}",
        )
        after, _, _ = read_coordinates(client, timeout)
        require(
            after["operation_seq"] == before["operation_seq"]
            and after["action_count"] == before["action_count"],
            "expired preview created a native action",
        )
    require(session.cleanup_ok, "expiry scenario left a child process")
    return {
        "error": "transaction_expired",
        "elicitation_count": 0,
        "native_actions": 0,
    }


def run_response_loss_after_commit(
    *,
    godot: Path,
    sidecar: Path,
    timeout: float,
    golden: Mapping[str, Any],
) -> dict[str, Any]:
    baseline = cast(Mapping[str, Any], golden["baseline"])
    truth = expected_operation(golden, "create_node")
    expected_applied = oracle.materialize_operation(baseline, truth)
    session = FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    with session:
        require(session.project_root is not None, "fixture project is unavailable")
        require(session.client is not None, "MCP client is unavailable")
        before_source = oracle.source_fingerprint(session.project_root)
        before, _, _ = read_coordinates(session.client, timeout)
        prepared, error, _ = session.client.tool(
            "godot_prepare_create_node",
            operation_arguments("create_node", before),
        )
        require(not error, f"response-loss prepare failed: {prepared}")
        case_id = "response-loss-after-commit"
        fault_marker(
            session.project_root,
            case_id=case_id,
            transaction_id=str(prepared["transaction_id"]),
            point="after_commit_before_response",
            mode="drop_response",
            state="armed",
        )
        worker, outcome = apply_in_background(
            session.client,
            prepared,
            timeout=max(timeout, 30.0),
        )
        wait_fault_reached(
            session.project_root,
            case_id=case_id,
            transaction_id=str(prepared["transaction_id"]),
            point="after_commit_before_response",
            mode="drop_response",
            timeout=timeout,
        )
        committed_state, _ = trigger_private(
            session,
            golden,
            expected_applied,
        )
        require(
            oracle.canonical(committed_state)
            == oracle.canonical(expected_applied),
            "response-loss fault did not reach the committed editor state",
        )
        require(session.sidecar_process is not None, "sidecar process is unavailable")
        session.sidecar_process.stop()
        worker.join(timeout=5)
        require(not worker.is_alive(), "response-loss apply worker did not exit")
        journal = oracle.strict_json(session.project_root / JOURNAL_PATH)
        durable = next(
            (
                item
                for item in cast(list[Any], journal.get("records", []))
                if isinstance(item, dict)
                and item.get("transaction_id") == prepared["transaction_id"]
            ),
            None,
        )
        require(
            isinstance(durable, dict) and durable.get("apply_dispatched") is True,
            "apply boundary was not durable before sidecar restart",
        )
        client = session.start_sidecar()
        recovered, _ = wait_status(
            client,
            str(prepared["transaction_id"]),
            "committed",
            timeout,
        )
        after, _, _ = read_coordinates(client, timeout)
        require(
            int(after["operation_seq"]) == int(before["operation_seq"]) + 1
            and int(after["action_count"]) == int(before["action_count"]) + 1,
            "response-loss recovery did not prove exactly one native action",
        )
        replayed, replay_error, _ = client.tool(
            "godot_apply_transaction",
            {
                "transaction_id": prepared["transaction_id"],
                "preview_digest": prepared["preview_digest"],
                "expected_scene_revision": recovered["current_scene_revision"],
                "expected_operation_seq": recovered["current_operation_seq"],
            },
        )
        require(
            replay_error
            and replayed.get("error", {}).get("code")
            in {
                "transaction_apply_replay_forbidden",
                "transaction_in_doubt",
            }
            and client.elicitation_total == 0,
            f"response-loss recovery allowed apply replay: {replayed}",
        )
        oracle.assert_source_unchanged(
            before_source,
            oracle.source_fingerprint(session.project_root),
        )
    require(session.cleanup_ok, "response-loss scenario left a child process")
    return {
        "state": "committed",
        "apply_dispatched_durable": True,
        "reconciled_without_replay": True,
        "transaction_native_actions": 1,
        "approval_count": 1,
        "source_unchanged": True,
    }


def run_disconnect_before_commit(
    *,
    godot: Path,
    sidecar: Path,
    timeout: float,
) -> dict[str, Any]:
    session = FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    with session:
        require(session.project_root is not None, "fixture project is unavailable")
        require(session.client is not None, "MCP client is unavailable")
        before_source = oracle.source_fingerprint(session.project_root)
        before, _, _ = read_coordinates(session.client, timeout)
        prepared, error, _ = session.client.tool(
            "godot_prepare_create_node",
            operation_arguments("create_node", before),
        )
        require(not error, f"precommit disconnect prepare failed: {prepared}")
        case_id = "disconnect-before-commit"
        fault_marker(
            session.project_root,
            case_id=case_id,
            transaction_id=str(prepared["transaction_id"]),
            point="after_approval_before_preflight",
            mode="pause",
            state="armed",
        )
        worker, outcome = apply_in_background(
            session.client,
            prepared,
            timeout=max(timeout, 30.0),
        )
        wait_fault_reached(
            session.project_root,
            case_id=case_id,
            transaction_id=str(prepared["transaction_id"]),
            point="after_approval_before_preflight",
            mode="pause",
            timeout=timeout,
        )
        require(session.sidecar_process is not None, "sidecar process is unavailable")
        session.sidecar_process.stop()
        fault_marker(
            session.project_root,
            case_id=case_id,
            transaction_id=str(prepared["transaction_id"]),
            point="after_approval_before_preflight",
            mode="pause",
            state="released",
        )
        worker.join(timeout=5)
        require(not worker.is_alive(), "precommit disconnect worker did not exit")
        client = session.start_sidecar()
        terminal, _ = wait_status_one_of(
            client,
            str(prepared["transaction_id"]),
            {"failed", "in_doubt", "error"},
            timeout,
        )
        after, _, _ = read_coordinates(client, timeout)
        require(
            after["operation_seq"] == before["operation_seq"]
            and after["action_count"] == before["action_count"],
            "disconnect before proven commit created a native action",
        )
        require(
            terminal.get("state") in {"failed", "in_doubt"}
            or terminal.get("error", {}).get("code")
            in {
                "approval_required",
                "transaction_in_doubt",
                "transaction_not_found",
            },
            f"precommit disconnect classification differs: {terminal}",
        )
        oracle.assert_source_unchanged(
            before_source,
            oracle.source_fingerprint(session.project_root),
        )
    require(session.cleanup_ok, "precommit disconnect left a child process")
    return {
        "classification": terminal.get(
            "state",
            terminal.get("error", {}).get("code", "error"),
        ),
        "automatic_replay": False,
        "transaction_native_actions": 0,
        "source_unchanged": True,
    }


def run_editor_crash_before_commit(
    *,
    godot: Path,
    sidecar: Path,
    timeout: float,
    golden: Mapping[str, Any],
) -> dict[str, Any]:
    baseline = cast(Mapping[str, Any], golden["baseline"])
    session = FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    with session:
        require(session.project_root is not None, "fixture project is unavailable")
        require(session.client is not None, "MCP client is unavailable")
        before_source = oracle.source_fingerprint(session.project_root)
        before, _, _ = read_coordinates(session.client, timeout)
        old_session = str(before["editor_session_id"])
        prepared, error, _ = session.client.tool(
            "godot_prepare_create_node",
            operation_arguments("create_node", before),
        )
        require(not error, f"editor crash prepare failed: {prepared}")
        case_id = "editor-crash-action-created"
        fault_marker(
            session.project_root,
            case_id=case_id,
            transaction_id=str(prepared["transaction_id"]),
            point="after_action_created_before_commit",
            mode="terminate",
            state="armed",
        )
        worker, outcome = apply_in_background(
            session.client,
            prepared,
            timeout=max(timeout, 30.0),
        )
        wait_fault_reached(
            session.project_root,
            case_id=case_id,
            transaction_id=str(prepared["transaction_id"]),
            point="after_action_created_before_commit",
            mode="terminate",
            timeout=timeout,
        )
        require(session.editor is not None, "fixture editor is unavailable")
        try:
            crash_code = session.editor.wait(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raise WorkflowError("editor did not terminate at the crash point") from error
        require(crash_code != 0, "editor crash fault exited successfully")
        require(session.sidecar_process is not None, "sidecar process is unavailable")
        session.sidecar_process.stop()
        worker.join(timeout=5)
        require(not worker.is_alive(), "editor crash apply worker did not exit")
        new_session = session.start_editor()
        require(new_session != old_session, "editor restart reused session identity")
        client = session.start_sidecar()
        restarted, _, _ = read_coordinates(client, timeout)
        require(
            restarted["operation_seq"] == 0
            and restarted["action_count"] == 0
            and "OracleCreated" not in restarted["node_paths"],
            "editor crash before commit survived as a native action",
        )
        restarted_state, _ = session.private_snapshot(
            golden,
            baseline,
            trigger=False,
        )
        require(
            oracle.canonical(restarted_state) == oracle.canonical(baseline),
            "editor crash before commit changed the scene",
        )
        old_status, old_error, _ = client.tool(
            "godot_get_transaction_status",
            {"transaction_id": prepared["transaction_id"]},
        )
        require(
            old_error
            or old_status.get("state")
            in {"expired", "failed", "in_doubt"},
            f"old crash-scoped transaction remained usable: {old_status}",
        )
        oracle.assert_source_unchanged(
            before_source,
            oracle.source_fingerprint(session.project_root),
        )
    require(session.cleanup_ok, "editor crash scenario left a child process")
    return {
        "editor_exit_nonzero": True,
        "new_editor_session": True,
        "pre_state_restored": True,
        "old_transaction_unavailable": True,
        "transaction_native_actions": 0,
        "source_unchanged": True,
    }


def run_editor_restart_after_commit(
    *,
    godot: Path,
    sidecar: Path,
    timeout: float,
) -> dict[str, Any]:
    session = FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    with session:
        require(session.project_root is not None, "fixture project is unavailable")
        require(session.client is not None, "MCP client is unavailable")
        before_source = oracle.source_fingerprint(session.project_root)
        before, _, _ = read_coordinates(session.client, timeout)
        old_session = str(before["editor_session_id"])
        prepared, error, _ = session.client.tool(
            "godot_prepare_create_node",
            operation_arguments("create_node", before),
        )
        require(not error, f"editor restart prepare failed: {prepared}")
        session.client.expect_approval(prepared)
        committed, error, _ = session.client.tool(
            "godot_apply_transaction",
            {
                "transaction_id": prepared["transaction_id"],
                "preview_digest": prepared["preview_digest"],
                "expected_scene_revision": prepared["scene_revision"],
                "expected_operation_seq": prepared["operation_seq"],
            },
        )
        session.client.clear_approval()
        require(
            not error and committed.get("state") == "committed",
            f"editor restart baseline apply failed: {committed}",
        )
        require(session.sidecar_process is not None, "sidecar process is unavailable")
        session.sidecar_process.stop()
        session.stop_editor(graceful=True)
        new_session = session.start_editor()
        require(new_session != old_session, "editor restart reused session identity")
        client = session.start_sidecar()
        restarted, _, _ = read_coordinates(client, timeout)
        require(
            restarted["operation_seq"] == 0
            and restarted["action_count"] == 0
            and "OracleCreated" not in restarted["node_paths"],
            "editor restart claimed persistent transaction Undo",
        )
        old_status, old_error, _ = client.tool(
            "godot_get_transaction_status",
            {"transaction_id": prepared["transaction_id"]},
        )
        require(
            old_error
            or old_status.get("state")
            in {"expired", "failed", "in_doubt"},
            f"old editor transaction retained native Undo: {old_status}",
        )
        oracle.assert_source_unchanged(
            before_source,
            oracle.source_fingerprint(session.project_root),
        )
    require(session.cleanup_ok, "editor restart scenario left a child process")
    return {
        "new_editor_session": True,
        "persistent_undo_claimed": False,
        "old_transaction_unavailable": True,
        "source_unchanged": True,
    }


def run_corrupt_journal_recovery(
    *,
    godot: Path,
    sidecar: Path,
    timeout: float,
) -> dict[str, Any]:
    session = FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    with session:
        require(session.project_root is not None, "fixture project is unavailable")
        require(session.client is not None, "MCP client is unavailable")
        before_source = oracle.source_fingerprint(session.project_root)
        before, _, _ = read_coordinates(session.client, timeout)
        prepared, error, _ = session.client.tool(
            "godot_prepare_create_node",
            operation_arguments("create_node", before),
        )
        require(not error, f"corrupt-journal prepare failed: {prepared}")
        require(session.sidecar_process is not None, "sidecar process is unavailable")
        session.sidecar_process.stop()
        journal_path = session.project_root / JOURNAL_PATH
        journal_path.write_text('{"invalid":', encoding="utf-8")
        journal_path.chmod(0o600)
        client = session.start_sidecar()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            stderr = "\n".join(
                session.sidecar_process.stderr_tail
                if session.sidecar_process is not None
                else []
            )
            if "quarantined an invalid transaction journal" in stderr:
                break
            time.sleep(0.05)
        else:
            raise WorkflowError("corrupt journal was not quarantined")
        readable, _, _ = read_coordinates(client, timeout)
        require(
            readable["scene_id"] == before["scene_id"],
            "read-only editor workflow stopped after journal quarantine",
        )
        old_status, old_error, _ = client.tool(
            "godot_get_transaction_status",
            {"transaction_id": prepared["transaction_id"]},
        )
        require(
            old_error
            and old_status.get("error", {}).get("code")
            == "transaction_not_found",
            f"quarantined transaction remained available: {old_status}",
        )
        quarantine = session.project_root / ".godot/codex/quarantine"
        require(
            quarantine.is_dir() and any(quarantine.iterdir()),
            "corrupt journal quarantine artifact is missing",
        )
        oracle.assert_source_unchanged(
            before_source,
            oracle.source_fingerprint(session.project_root),
        )
    require(session.cleanup_ok, "corrupt journal scenario left a child process")
    return {
        "quarantined": True,
        "old_transaction_fail_closed": True,
        "diagnostic_reads_available": True,
        "native_actions": 0,
        "source_unchanged": True,
    }


def run_restart_after_journal_ack_gate(timeout: float) -> dict[str, Any]:
    started = time.monotonic()
    result = subprocess.run(
        [
            "cargo",
            "test",
            "-p",
            "godot-codex-transactions",
            "--locked",
            "--offline",
            "tests::restart_after_bridge_response_before_journal_ack_reconciles_without_apply_replay",
            "--",
            "--exact",
        ],
        cwd=REPOSITORY_ROOT / "godot-codex-mcp",
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=max(timeout, 60.0),
        check=False,
    )
    require(
        result.returncode == 0
        and "1 passed" in result.stdout
        and "0 failed" in result.stdout,
        "after-Bridge-response journal reconciliation gate failed",
    )
    return {
        "coverage": "deterministic_rust_fault_gate",
        "apply_dispatched_durable": True,
        "status_reconciled": "committed",
        "automatic_replay": False,
        "duration_ms": round((time.monotonic() - started) * 1_000, 3),
    }


def run_approval_negative(
    decision: str,
    expected_code: str,
    *,
    godot: Path,
    sidecar: Path,
    timeout: float,
    supports_form: bool = True,
) -> dict[str, Any]:
    session = FixtureSession(
        godot=godot,
        sidecar=sidecar,
        timeout=timeout,
        supports_form=supports_form,
    )
    with session:
        require(session.project_root is not None, "fixture project is unavailable")
        require(session.client is not None, "MCP client is unavailable")
        client = session.client
        before_source = oracle.source_fingerprint(session.project_root)
        before, _, _ = read_coordinates(client, timeout)
        prepared, error, _ = client.tool(
            "godot_prepare_create_node",
            operation_arguments("create_node", before),
        )
        require(not error, f"approval negative prepare failed: {prepared}")
        if supports_form:
            client.expect_approval(prepared, decision=decision)
        result, error, _ = client.tool(
            "godot_apply_transaction",
            {
                "transaction_id": prepared["transaction_id"],
                "preview_digest": prepared["preview_digest"],
                "expected_scene_revision": prepared["scene_revision"],
                "expected_operation_seq": prepared["operation_seq"],
            },
            timeout=max(timeout, 130.0) if decision == "timeout" else timeout,
        )
        expectation = client.clear_approval()
        require(
            error and result.get("error", {}).get("code") == expected_code,
            f"{decision} approval result differs: {result}",
        )
        require(
            (not supports_form and client.elicitation_total == 0)
            or (
                expectation is not None
                and expectation.calls == 1
                and client.elicitation_total == 1
            ),
            f"{decision} elicitation count differs",
        )
        after, _, _ = read_coordinates(client, timeout)
        require(
            after["operation_seq"] == before["operation_seq"]
            and after["action_count"] == before["action_count"],
            f"{decision} approval created a native action",
        )
        oracle.assert_source_unchanged(
            before_source,
            oracle.source_fingerprint(session.project_root),
        )
    require(session.cleanup_ok, f"{decision} scenario left a child process")
    return {
        "decision": decision,
        "error": expected_code,
        "elicitation_count": 0 if not supports_form else 1,
        "native_actions": 0,
        "source_unchanged": True,
    }


def aggregate_latencies(operations: list[Mapping[str, Any]]) -> dict[str, Any]:
    output: dict[str, list[float]] = {}
    for operation in operations:
        latency = operation.get("latency_ms")
        if not isinstance(latency, dict):
            continue
        for name, values in latency.items():
            if isinstance(values, dict) and isinstance(values.get("p95"), (int, float)):
                output.setdefault(name, []).append(float(values["p95"]))
    return {
        name: {
            "scenario_count": len(values),
            "p50": percentile(values, 50),
            "p95": percentile(values, 95),
            "max": round(max(values), 3),
        }
        for name, values in output.items()
    }


def atomic_json(path: Path, value: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--godot",
        type=Path,
        default=REPOSITORY_ROOT / "bin/godot.macos.editor.dev.arm64",
    )
    parser.add_argument(
        "--sidecar",
        type=Path,
        default=REPOSITORY_ROOT
        / "godot-codex-mcp/target/release/godot-codex-mcp",
    )
    parser.add_argument("--report", type=Path)
    parser.add_argument("--timeout", type=float, default=45.0)
    parser.add_argument(
        "--operations",
        nargs="*",
        choices=sorted(PREPARE_TOOLS),
        default=sorted(PREPARE_TOOLS),
    )
    parser.add_argument("--skip-negatives", action="store_true")
    parser.add_argument("--skip-approval-timeout", action="store_true")
    parser.add_argument("--skip-faults", action="store_true")
    parser.add_argument(
        "--fault-scenarios",
        nargs="*",
        choices=FAULT_SCENARIOS,
        default=list(FAULT_SCENARIOS),
    )
    return parser.parse_args()


def main() -> int:
    arguments = parse_arguments()
    require(sys.platform == "darwin", "S9 model-free gate requires macOS")
    require(
        platform.machine().lower() in {"arm64", "aarch64"},
        "S9 model-free gate requires arm64",
    )
    godot = arguments.godot.resolve(strict=True)
    sidecar = arguments.sidecar.resolve(strict=True)
    golden = oracle.strict_json(GOLDEN_PATH)
    oracle.validate_golden(golden)
    operations: list[dict[str, Any]] = []
    for kind in arguments.operations:
        print(f"[S9 model-free] {kind}...", flush=True)
        operations.append(
            run_operation(
                kind,
                godot=godot,
                sidecar=sidecar,
                timeout=arguments.timeout,
                golden=golden,
            )
        )
    negatives: dict[str, Any] = {}
    if not arguments.skip_negatives:
        print("[S9 model-free] idempotency and approval negatives...", flush=True)
        negatives["idempotency"] = run_idempotency_negative(
            godot=godot,
            sidecar=sidecar,
            timeout=arguments.timeout,
        )
        negatives["committed_replay"] = run_committed_replay_negative(
            godot=godot,
            sidecar=sidecar,
            timeout=arguments.timeout,
        )
        negatives["expired_preview"] = run_expired_preview_negative(
            godot=godot,
            sidecar=sidecar,
            timeout=arguments.timeout,
        )
        negatives["approval"] = [
            run_approval_negative(
                "confirm_false",
                "approval_invalid",
                godot=godot,
                sidecar=sidecar,
                timeout=arguments.timeout,
            ),
            run_approval_negative(
                "decline",
                "approval_declined",
                godot=godot,
                sidecar=sidecar,
                timeout=arguments.timeout,
            ),
            run_approval_negative(
                "cancel",
                "approval_cancelled",
                godot=godot,
                sidecar=sidecar,
                timeout=arguments.timeout,
            ),
            run_approval_negative(
                "unsupported",
                "approval_host_unsupported",
                godot=godot,
                sidecar=sidecar,
                timeout=arguments.timeout,
                supports_form=False,
            ),
        ]
        if not arguments.skip_approval_timeout:
            negatives["approval"].append(
                run_approval_negative(
                    "timeout",
                    "approval_timeout",
                    godot=godot,
                    sidecar=sidecar,
                    timeout=arguments.timeout,
                )
            )
    faults: dict[str, Any] = {}
    if not arguments.skip_faults:
        print("[S9 model-free] crash and disconnect matrix...", flush=True)
        selected_faults = set(arguments.fault_scenarios)
        if "sidecar_disconnect_prepared" in selected_faults:
            faults["sidecar_disconnect_prepared"] = run_sidecar_disconnect_prepared(
                godot=godot,
                sidecar=sidecar,
                timeout=arguments.timeout,
            )
        if "bridge_response_loss_after_commit" in selected_faults:
            faults["bridge_response_loss_after_commit"] = run_response_loss_after_commit(
                godot=godot,
                sidecar=sidecar,
                timeout=arguments.timeout,
                golden=golden,
            )
        if "disconnect_before_commit" in selected_faults:
            faults["disconnect_before_commit"] = run_disconnect_before_commit(
                godot=godot,
                sidecar=sidecar,
                timeout=arguments.timeout,
            )
        if "editor_crash_before_commit" in selected_faults:
            faults["editor_crash_before_commit"] = run_editor_crash_before_commit(
                godot=godot,
                sidecar=sidecar,
                timeout=arguments.timeout,
                golden=golden,
            )
        if "editor_restart_after_commit" in selected_faults:
            faults["editor_restart_after_commit"] = run_editor_restart_after_commit(
                godot=godot,
                sidecar=sidecar,
                timeout=arguments.timeout,
            )
        if "corrupt_journal" in selected_faults:
            faults["corrupt_journal"] = run_corrupt_journal_recovery(
                godot=godot,
                sidecar=sidecar,
                timeout=arguments.timeout,
            )
        if (
            "restart_after_bridge_response_before_journal_ack"
            in selected_faults
        ):
            faults[
                "restart_after_bridge_response_before_journal_ack"
            ] = run_restart_after_journal_ack_gate(arguments.timeout)
    transaction_hashes = [item["transaction_sha256"] for item in operations]
    require(
        len(transaction_hashes) == len(set(transaction_hashes)),
        "distinct operations reused a transaction identity",
    )
    frame_max = max(
        (
            int(item["frame_telemetry"]["max_elapsed_usec"])
            for item in operations
        ),
        default=0,
    )
    frame_over_budget = sum(
        int(item["frame_telemetry"]["over_budget_count"])
        for item in operations
    )
    require(
        all(
            int(item["frame_telemetry"]["budget_usec"]) == 2_000
            for item in operations
        )
        and frame_max <= 2_000
        and frame_over_budget == 0,
        (
            "Bridge dispatcher frame slice exceeded 2,000 usec "
            f"(max={frame_max}, over={frame_over_budget})"
        ),
    )
    report = {
        "schema_version": "s9-model-free-workflow/1.0",
        "status": "passed",
        "platform": "macos-arm64",
        "protocol": MCP_PROTOCOL,
        "tool_registry": 36,
        "operations": operations,
        "operation_count": len(operations),
        "transaction_identity_unique": True,
        "elicitation_count": (
            sum(item["elicitation_count"] for item in operations)
            + sum(
                int(item.get("elicitation_count", 0))
                for item in cast(
                    list[Mapping[str, Any]],
                    negatives.get("approval", []),
                )
            )
            + int(
                cast(
                    Mapping[str, Any],
                    negatives.get("committed_replay", {}),
                ).get("elicitation_count", 0)
            )
        ),
        "native_action_count": sum(item["native_actions"] for item in operations),
        "negatives": negatives,
        "faults": faults,
        "fault_count": len(faults),
        "latency_ms": aggregate_latencies(operations),
        "bridge_frame": {
            "budget_usec": 2_000,
            "max_elapsed_usec": frame_max,
            "over_budget_count": frame_over_budget,
        },
        "source_unchanged": all(item["source_unchanged"] for item in operations),
        "cleanup": all(item["cleanup"] for item in operations),
        "redaction": True,
    }
    encoded = canonical(report)
    require(len(encoded) <= 65_536, "model-free report exceeds 64 KiB")
    require(BIND_CANARY.encode() not in encoded, "model-free report leaks a canary")
    if arguments.report is not None:
        atomic_json(arguments.report, report)
    print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (WorkflowError, oracle.FixtureError) as error:
        print(f"S9 model-free workflow failed: {error}", file=sys.stderr)
        raise SystemExit(1)
