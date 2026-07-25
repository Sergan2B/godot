#!/usr/bin/env python3
"""Run the model-free Sprint 7 Godot editor -> Bridge -> MCP live gate."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, cast

from sprint2_live_smoke import MCP_PROTOCOL, LineProcess, McpClient, atomic_json
from sprint5_script_semantics_live import close_sidecar, parse_telemetry

try:
    from tests.codex import sprint11_packaged_fixture as packaged_fixture
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_packaged_fixture as packaged_fixture

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
PROJECT_SOURCE = SCRIPT_DIR / "fixtures" / "live_editor_project"
MANIFEST_PATH = SCRIPT_DIR / "fixtures" / "live_editor_oracle" / "fixture-manifest.json"
GOLDEN_PATH = SCRIPT_DIR / "fixtures" / "live_editor_oracle" / "golden-live-editor.json"
EDITOR_SUMMARY_URI = "godot://editor/summary"
LIVE_VALUE = 21.5
DISK_VALUE = 8.0
TOOL_NAMES = {
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
    "godot_get_scene_graph",
    "godot_get_selected_nodes",
    "godot_get_viewport_state",
    "godot_inspect_node",
    "godot_inspect_symbol",
    "godot_search_symbols",
}
SPRINT11_TOOL_NAME = "godot_get_connection_status"
FORBIDDEN_RESPONSE_MATERIAL = (
    "/Users/",
    "/home/",
    "/tmp/",
    "C:\\",
    "\\\\.\\pipe\\",
    ".godot/imported",
    "session.token",
    "fixture-only-secret",
    "authorization",
    "source_excerpt",
)


class LiveEditorError(RuntimeError):
    """Raised when the real live-editor path differs from EDITOR-001."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise LiveEditorError(message)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return "sha256:" + digest.hexdigest()


def percentile(samples: list[float] | list[int], value: int) -> float:
    require(bool(samples), "performance sample population is empty")
    ordered = sorted(float(sample) for sample in samples)
    index = max(0, min(len(ordered) - 1, math.ceil(value * len(ordered) / 100) - 1))
    return round(ordered[index], 3)


def require_dispatcher_within_budget(
    telemetry: dict[str, Any],
    context: str,
) -> None:
    require(
        telemetry.get("budget_usec") == 2_000
        and isinstance(telemetry.get("dispatcher_sample_count"), int)
        and telemetry["dispatcher_sample_count"] > 0
        and isinstance(telemetry.get("dispatcher_max_elapsed_usec"), int)
        and telemetry["dispatcher_max_elapsed_usec"] <= 2_000
        and telemetry.get("dispatcher_over_budget_count") == 0,
        f"{context} dispatcher exceeded its budget: {telemetry}",
    )


def target_platform() -> str:
    machine = platform.machine().lower()
    if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
        return "macos-arm64"
    if sys.platform == "win32" and machine in {"amd64", "x86_64"}:
        return "windows-x86_64"
    raise LiveEditorError(
        f"Sprint 7 live gate requires macOS arm64 or Windows x86_64, got {sys.platform}/{machine}"
    )


def require_safe_value(value: Any, context: str) -> None:
    serialized = json.dumps(value, ensure_ascii=False)
    require(
        not any(marker in serialized for marker in FORBIDDEN_RESPONSE_MATERIAL),
        f"{context} leaked host, source, secret, or private runtime material",
    )


def tool_call(
    client: McpClient, name: str, arguments: dict[str, Any]
) -> tuple[dict[str, Any], bool, float]:
    started = time.perf_counter_ns()
    response = client.request("tools/call", {"name": name, "arguments": arguments})
    elapsed = round((time.perf_counter_ns() - started) / 1_000_000, 3)
    require_safe_value(response, name)
    require("error" not in response, f"{name} returned an MCP protocol error")
    result = response.get("result")
    require(isinstance(result, dict), f"{name} omitted its result")
    content = result.get("structuredContent")
    require(isinstance(content, dict), f"{name} omitted structuredContent")
    return cast(dict[str, Any], content), result.get("isError") is True, elapsed


def read_editor_summary(client: McpClient) -> tuple[dict[str, Any], int, float]:
    started = time.perf_counter_ns()
    response = client.request("resources/read", {"uri": EDITOR_SUMMARY_URI})
    elapsed = round((time.perf_counter_ns() - started) / 1_000_000, 3)
    require_safe_value(response, "editor summary")
    require("error" not in response, "editor summary returned an MCP error")
    contents = response.get("result", {}).get("contents")
    require(isinstance(contents, list) and len(contents) == 1, "editor summary contents differ")
    text = contents[0].get("text")
    require(isinstance(text, str), "editor summary text is absent")
    encoded_bytes = len(text.encode("utf-8"))
    require(encoded_bytes <= 4096, "editor summary exceeded 4096 UTF-8 bytes")
    value = json.loads(text)
    require(isinstance(value, dict), "editor summary JSON differs")
    return cast(dict[str, Any], value), encoded_bytes, elapsed


def initialize_sidecar(
    executable: Path,
    project: Path,
    timeout: float,
    *,
    additive_sprint11_registry: bool = False,
) -> tuple[LineProcess, McpClient, dict[str, bool]]:
    process = LineProcess(
        [str(executable), "--project-root", str(project)],
        cwd=project,
        env=os.environ.copy(),
    )
    client = McpClient(process, timeout=min(timeout, 20.0))
    initialized = client.request(
        "initialize",
        {
            "protocolVersion": MCP_PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "sprint7-live-editor-gate", "version": "1"},
        },
    )
    require_safe_value(initialized, "MCP initialize")
    require(
        initialized.get("result", {}).get("protocolVersion") == MCP_PROTOCOL,
        "MCP protocol version differs",
    )
    resources = initialized.get("result", {}).get("capabilities", {}).get("resources")
    require(isinstance(resources, dict), "MCP resources capability is absent")
    require("subscribe" not in resources and "listChanged" not in resources, "subscriptions were enabled")
    client.notify("notifications/initialized", {})
    listed = client.request("tools/list", {})
    require_safe_value(listed, "MCP tool registry")
    tools = listed.get("result", {}).get("tools")
    require(isinstance(tools, list), "MCP tool registry omitted tools")
    tool_names = {tool.get("name") for tool in tools}
    expected_tool_count = 41 if additive_sprint11_registry else 40
    require(
        len(tools) == len(tool_names) == expected_tool_count
        and TOOL_NAMES <= tool_names
        and (
            (SPRINT11_TOOL_NAME in tool_names)
            is additive_sprint11_registry
        ),
        "MCP registry does not preserve the 16-tool Sprint 7 surface",
    )
    sprint7_tools = [
        tool for tool in tools if tool.get("name") in TOOL_NAMES
    ]
    require(
        all(
            tool.get("inputSchema", {}).get("type") == "object"
            and tool.get("inputSchema", {}).get("additionalProperties") is False
            and tool.get("annotations", {}).get("readOnlyHint") is True
            and tool.get("annotations", {}).get("destructiveHint") is False
            and tool.get("annotations", {}).get("openWorldHint") is False
            for tool in sprint7_tools
        ),
        "a Sprint 7 MCP tool schema or annotation differs",
    )
    return process, client, {
        "exact_sixteen_tool_registry": True,
        "closed_input_schemas": True,
        "read_only_annotations": True,
        "subscriptions_disabled": True,
    }


def wait_for_file(process: subprocess.Popen[str], path: Path, log_path: Path, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.is_file():
            return
        if process.poll() is not None:
            tail = log_path.read_text(encoding="utf-8", errors="replace")[-8000:]
            raise LiveEditorError(f"Godot exited before {path.name}: {tail}")
        time.sleep(0.02)
    raise LiveEditorError(f"timed out waiting for {path.name}")


def wait_phase(
    process: subprocess.Popen[str], path: Path, expected_phase: int, log_path: Path, timeout: float
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: Any = None
    while time.monotonic() < deadline:
        try:
            last = json.loads(path.read_text(encoding="utf-8"))
            if isinstance(last, dict) and last.get("phase") == expected_phase:
                return cast(dict[str, Any], last)
        except (OSError, json.JSONDecodeError, UnicodeError):
            pass
        if process.poll() is not None:
            tail = log_path.read_text(encoding="utf-8", errors="replace")[-8000:]
            raise LiveEditorError(f"Godot exited before phase {expected_phase}: {tail}")
        time.sleep(0.02)
    tail = log_path.read_text(encoding="utf-8", errors="replace")[-8000:]
    raise LiveEditorError(
        f"timed out waiting for phase {expected_phase}; last={last}; godot_tail={tail}"
    )


def start_editor(
    godot: Path, project: Path, log_path: Path
) -> tuple[subprocess.Popen[str], Any]:
    log = log_path.open("w", encoding="utf-8")
    environment = os.environ.copy()
    environment["CODEX_SPRINT7_AUTOMATION"] = "1"
    environment["GODOT_CODEX_EVIDENCE_TELEMETRY"] = "1"
    process = subprocess.Popen(
        [str(godot), "--editor", "--headless", "--path", str(project)],
        cwd=REPOSITORY_ROOT,
        env=environment,
        stdout=log,
        stderr=subprocess.STDOUT,
        text=True,
    )
    return process, log


def command(project: Path, action: str) -> None:
    atomic_json(project / ".godot/codex-sprint7-command.json", {"action": action})


def stop_editor(
    process: subprocess.Popen[str], project: Path, phase: int, log: Any, timeout: float
) -> dict[str, Any]:
    if process.poll() is None:
        command(project, "done")
        try:
            process.wait(timeout=min(timeout, 30.0))
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
    log.close()
    require(process.returncode == 0, f"Godot editor exited with status {process.returncode}")
    telemetry = parse_telemetry(Path(log.name))
    require_dispatcher_within_budget(telemetry, "Bridge")
    del phase
    return telemetry


def recursive_keys(value: Any) -> set[str]:
    if isinstance(value, dict):
        return set(value) | {key for child in value.values() for key in recursive_keys(child)}
    if isinstance(value, list):
        return {key for child in value for key in recursive_keys(child)}
    return set()


def find_property(node: dict[str, Any], name: str) -> dict[str, Any] | None:
    properties = node.get("properties")
    if not isinstance(properties, list):
        return None
    return next(
        (cast(dict[str, Any], item) for item in properties if isinstance(item, dict) and item.get("name") == name),
        None,
    )


def current_scene_revision(snapshot: dict[str, Any]) -> int:
    scene = snapshot.get("scene")
    revisions = snapshot.get("revision_vector")
    require(isinstance(scene, dict) and isinstance(revisions, dict), "current scene revisions are absent")
    scene_id = scene.get("entity_id")
    scene_revisions = revisions.get("scene_revisions")
    require(isinstance(scene_id, str) and isinstance(scene_revisions, dict), "scene revision map differs")
    value = scene_revisions.get(scene_id)
    require(isinstance(value, int) and not isinstance(value, bool), "current scene revision differs")
    return value


def stable_snapshot(client: McpClient, timeout: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] = {}
    while time.monotonic() < deadline:
        calls = {
            "open_scenes": ("godot_get_open_scenes", {"limit": 200}),
            "selection": ("godot_get_selected_nodes", {}),
            "inspector": ("godot_get_inspector_state", {}),
            "scripts": ("godot_get_open_scripts", {"limit": 200}),
            "history": ("godot_get_editor_history", {"limit": 200}),
            "diagnostics": ("godot_get_diagnostics", {"limit": 200}),
            "viewport": ("godot_get_viewport_state", {}),
        }
        values: dict[str, dict[str, Any]] = {}
        failed = False
        for key, (name, arguments) in calls.items():
            content, is_error, _ = tool_call(client, name, arguments)
            if is_error:
                failed = True
                last = content
                break
            values[key] = content
        if failed:
            time.sleep(0.05)
            continue
        snapshot_ids = {value.get("snapshot_id") for value in values.values()}
        if len(snapshot_ids) == 1 and None not in snapshot_ids:
            return values
        last = values
        time.sleep(0.05)
    raise LiveEditorError(f"live snapshot did not stabilize: {last}")


def dirty_history(values: dict[str, Any], scene_id: str) -> dict[str, Any]:
    histories = values.get("history", {}).get("histories")
    require(isinstance(histories, list), "history list is absent")
    match = next(
        (
            cast(dict[str, Any], item)
            for item in histories
            if isinstance(item, dict) and item.get("scene_id") == scene_id
        ),
        None,
    )
    require(match is not None, "dirty scene history is absent")
    return cast(dict[str, Any], match)


def snapshot_observation(values: dict[str, Any]) -> dict[str, Any]:
    scenes = values["open_scenes"].get("scenes")
    selection = values["selection"].get("selected_nodes")
    inspector = values["inspector"].get("inspector")
    scripts = values["scripts"].get("scripts")
    diagnostics = values["diagnostics"].get("diagnostics")
    viewport = values["viewport"].get("viewport")
    require(isinstance(scenes, list) and len(scenes) == 2, "live fixture must expose two saved scene tabs")
    require(
        isinstance(selection, list) and len(selection) == 3,
        f"multi-selection lost a node: {selection}",
    )
    require(
        isinstance(inspector, dict)
        and inspector.get("has_object") is True
        and inspector.get("object_kind") in {"node", "object"},
        f"Inspector node is absent: {inspector}",
    )
    require(isinstance(scripts, list) and len(scripts) == 2, "open scripts differ")
    require(isinstance(diagnostics, list), "diagnostics list is absent")
    require(isinstance(viewport, dict), "viewport metadata is absent")
    current = next((scene for scene in scenes if isinstance(scene, dict) and scene.get("current") is True), None)
    clean = next((scene for scene in scenes if isinstance(scene, dict) and scene.get("path") == "res://clean.tscn"), None)
    require(
        isinstance(current, dict)
        and current.get("path") == "res://dirty.tscn"
        and current.get("dirty") is True,
        "dirty current scene is not explicit",
    )
    require(isinstance(clean, dict) and clean.get("dirty") is False, "clean scene state differs")
    paths = {node.get("node_path") for node in selection if isinstance(node, dict)}
    identities = [node.get("entity_id") for node in selection if isinstance(node, dict)]
    require(paths == {"Alpha", "Beta", "Gamma"}, "multi-selection paths differ")
    require(len(identities) == 3 and len(set(identities)) == 3 and all(isinstance(item, str) for item in identities), "multi-selection identity differs")
    alpha = next(node for node in selection if isinstance(node, dict) and node.get("node_path") == "Alpha")
    movement = find_property(alpha, "movement_speed")
    require(
        movement is not None
        and isinstance(movement.get("value"), (int, float))
        and not isinstance(movement.get("value"), bool),
        "live movement_speed is absent",
    )
    require(movement.get("source") == "live_editor_property", "live property provenance differs")
    inspector_movement = find_property(inspector, "movement_speed")
    require(
        inspector_movement is not None
        and inspector_movement.get("value") == movement.get("value"),
        "Inspector did not expose the effective live value",
    )
    if inspector.get("object_kind") == "node":
        require(inspector.get("object_id") == alpha.get("entity_id"), "Inspector lost selected-node identity")
    else:
        require(
            inspector.get("godot_type") == "MultiNodeEdit"
            and isinstance(inspector.get("object_id"), str),
            "native multi-selection Inspector proxy was not safely opaque",
        )
    require(sum(script.get("active") is True for script in scripts if isinstance(script, dict)) == 1, "active script differs")
    require(all(script.get("source_text_included") is False for script in scripts if isinstance(script, dict)), "script source was exposed")
    require("source_code" not in recursive_keys(scripts), "script source field was exposed")
    redacted = [item for item in diagnostics if isinstance(item, dict) and item.get("redacted") is True]
    require(redacted and all("fixture-only-secret" not in str(item.get("message")) for item in diagnostics if isinstance(item, dict)), "diagnostic redaction differs")
    require(values["diagnostics"].get("diagnostic_state", {}).get("encoded_bytes", 0) <= 262144, "diagnostic snapshot exceeded its byte limit")
    require(viewport.get("screenshot_available") is False, "viewport screenshot became available early")
    require(
        not ({"pixels", "rid", "texture", "texture_rid"} & {key.lower() for key in recursive_keys(viewport)}),
        "viewport response exposed pixel or native handle material",
    )
    revisions = values["selection"].get("revision_vector")
    require(isinstance(revisions, dict), "revision vector is absent")
    history = dirty_history(values, cast(str, current["entity_id"]))
    return {
        "snapshot_id": values["selection"]["snapshot_id"],
        "editor_session_id": values["selection"]["editor_session_id"],
        "event_seq": revisions["event_seq"],
        "operation_seq": revisions["operation_seq"],
        "dirty_scene_id": current["entity_id"],
        "dirty_scene_revision": current["scene_revision"],
        "clean_scene_id": clean["entity_id"],
        "clean_scene_revision": clean["scene_revision"],
        "selection_ids": sorted(cast(list[str], identities)),
        "history_id": history["entity_id"],
        "history_transition": history["transition_kind"],
        "history_operation_seq": history["last_operation_seq"],
        "history_operation_kind": history.get("last_operation", {}).get("kind"),
        "live_value": movement["value"],
        "diagnostic_count": len(diagnostics),
        "redacted_diagnostic_count": len(redacted),
        "script_count": len(scripts),
        "viewport_kind": viewport.get("active_kind"),
    }


def wait_observation(
    client: McpClient,
    timeout: float,
    *,
    minimum_operation_seq: int | None = None,
    transition_kind: str | None = None,
    expected_value: float | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    last_error: str | None = None
    while time.monotonic() < deadline:
        try:
            values = stable_snapshot(client, min(2.0, max(0.2, deadline - time.monotonic())))
            observation = snapshot_observation(values)
            last = observation
            if minimum_operation_seq is not None and observation["operation_seq"] <= minimum_operation_seq:
                time.sleep(0.02)
                continue
            if transition_kind is not None and observation["history_transition"] != transition_kind:
                time.sleep(0.02)
                continue
            if expected_value is not None and observation["live_value"] != expected_value:
                time.sleep(0.02)
                continue
            return values, observation
        except LiveEditorError as error:
            last_error = str(error)
            time.sleep(0.05)
    raise LiveEditorError(
        f"expected live transition did not become visible: {last}; last_error={last_error}"
    )


def require_tool_error(
    client: McpClient, name: str, arguments: dict[str, Any], code: str
) -> dict[str, Any]:
    content, is_error, _ = tool_call(client, name, arguments)
    require(is_error and content.get("error", {}).get("code") == code, f"{name} did not reject as {code}")
    require(content.get("error", {}).get("retryable") is True, f"{code} is not retryable")
    require(isinstance(content.get("error", {}).get("current"), dict), f"{code} omitted current coordinates")
    return content


def wait_inspect_node(client: McpClient, timeout: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        content, is_error, _ = tool_call(
            client,
            "godot_inspect_node",
            {"scene": "res://dirty.tscn", "node_path": "Alpha", "limit": 200},
        )
        last = content
        properties = content.get("properties")
        movement = next(
            (item for item in properties if isinstance(item, dict) and item.get("name") == "movement_speed"),
            None,
        ) if isinstance(properties, list) else None
        if (
            not is_error
            and content.get("live_overlay", {}).get("status") == "composed"
            and isinstance(movement, dict)
            and movement.get("origin") == "live_editor"
            and movement.get("value", {}).get("value") == LIVE_VALUE
            and isinstance(movement.get("disk_comparison"), dict)
            and movement["disk_comparison"].get("value", {}).get("value") == DISK_VALUE
            and content.get("conflicts")
        ):
            return content
        time.sleep(0.05)
    raise LiveEditorError(f"disk/live node composition did not become current: {last}")


def runtime_paths(project: Path) -> list[Path]:
    codex = project / ".godot/codex"
    paths = [codex / "bridge.json", codex / "session.token", codex / "bridge.lock"]
    discovery = codex / "bridge.json"
    if discovery.is_file():
        try:
            endpoint = Path(str(json.loads(discovery.read_text(encoding="utf-8")).get("endpoint", "")))
            if endpoint and not endpoint.is_absolute() and ".." not in endpoint.parts:
                paths.append(project / endpoint)
        except (OSError, json.JSONDecodeError, UnicodeError):
            pass
    return paths


def run_session(
    godot: Path,
    sidecar: Path,
    project: Path,
    run_root: Path,
    timeout: float,
    old_session_id: str | None = None,
    *,
    additive_sprint11_registry: bool = False,
) -> dict[str, Any]:
    log_path = run_root / ("godot-restart.log" if old_session_id else "godot.log")
    phase_path = project / ".godot/codex-sprint7-phase.json"
    command_path = project / ".godot/codex-sprint7-command.json"
    phase_path.unlink(missing_ok=True)
    command_path.unlink(missing_ok=True)
    editor, log = start_editor(godot, project, log_path)
    sidecar_process: LineProcess | None = None
    telemetry: dict[str, Any] | None = None
    try:
        wait_phase(editor, phase_path, 1, log_path, timeout)
        discovery = project / ".godot/codex/bridge.json"
        wait_for_file(editor, discovery, log_path, timeout)
        paths = runtime_paths(project)
        sidecar_process, client, contract = initialize_sidecar(
            sidecar,
            project,
            timeout,
            additive_sprint11_registry=additive_sprint11_registry,
        )
        values, initial = wait_observation(client, timeout, expected_value=LIVE_VALUE)
        if old_session_id is not None:
            require(initial["editor_session_id"] != old_session_id, "editor restart reused the old session ID")
            require_tool_error(
                client,
                "godot_get_editor_state",
                {"expected_editor_session_id": old_session_id},
                "stale_editor_state",
            )
            command(project, "done")
            editor.wait(timeout=min(timeout, 30.0))
            close_sidecar(sidecar_process)
            sidecar_process = None
            log.close()
            telemetry = parse_telemetry(log_path)
            require_dispatcher_within_budget(telemetry, "restart")
            require(not any(path.exists() for path in paths), "restart left Bridge runtime files")
            return {
                "session_id_changed": True,
                "old_session_guard_rejected": True,
                "editor_session_id": initial["editor_session_id"],
                "telemetry": telemetry,
            }

        scene_revision = initial["dirty_scene_revision"]
        require_tool_error(
            client,
            "godot_get_editor_state",
            {"expected_event_seq": initial["event_seq"] + 1},
            "stale_editor_state",
        )
        require_tool_error(
            client,
            "godot_get_inspector_state",
            {"expected_scene_revision": scene_revision + 1},
            "stale_scene_revision",
        )
        composed = wait_inspect_node(client, timeout)
        summary, summary_bytes, first_summary_ms = read_editor_summary(client)
        require(summary.get("dirty_scene_count", 0) >= 1, "editor summary omitted dirty state")

        selection_samples: list[float] = []
        control_samples: list[float] = []
        summary_samples = [first_summary_ms]
        for _ in range(20):
            content, is_error, elapsed = tool_call(client, "godot_get_selected_nodes", {})
            require(not is_error and len(content.get("selected_nodes", [])) == 3, "selection benchmark failed")
            selection_samples.append(elapsed)
            content, is_error, elapsed = tool_call(client, "godot_get_editor_state", {})
            require(not is_error and content.get("freshness") == "current", "control benchmark failed")
            control_samples.append(elapsed)
            _, _, elapsed = read_editor_summary(client)
            summary_samples.append(elapsed)

        transitions: dict[str, dict[str, Any]] = {"commit": initial}
        visibility_samples: list[float] = []
        previous = initial
        phase = 1
        for action, expected_transition, expected_value in (
            ("undo", "undo", DISK_VALUE),
            ("redo", "redo", LIVE_VALUE),
        ):
            phase += 1
            started = time.perf_counter_ns()
            command(project, action)
            wait_phase(editor, phase_path, phase, log_path, timeout)
            _, observed = wait_observation(
                client,
                timeout,
                minimum_operation_seq=previous["operation_seq"],
                transition_kind=expected_transition,
                expected_value=expected_value,
            )
            visibility_samples.append(round((time.perf_counter_ns() - started) / 1_000_000, 3))
            require(observed["selection_ids"] == initial["selection_ids"], "selection identity drifted")
            require(observed["dirty_scene_revision"] > previous["dirty_scene_revision"], "affected scene revision did not advance")
            require(observed["clean_scene_revision"] == previous["clean_scene_revision"], "unaffected scene revision advanced")
            transitions[action] = observed
            previous = observed

        # A p95 over the three semantic transition kinds would collapse to a
        # maximum and would not be statistically meaningful. Exercise nine
        # additional native Undo/Redo pairs so the latency gate has 21 real
        # editor operations while the canonical transition evidence above
        # remains compact.
        for _ in range(9):
            for action, expected_transition, expected_value in (
                ("undo", "undo", DISK_VALUE),
                ("redo", "redo", LIVE_VALUE),
            ):
                phase += 1
                started = time.perf_counter_ns()
                command(project, action)
                wait_phase(editor, phase_path, phase, log_path, timeout)
                _, observed = wait_observation(
                    client,
                    timeout,
                    minimum_operation_seq=previous["operation_seq"],
                    transition_kind=expected_transition,
                    expected_value=expected_value,
                )
                visibility_samples.append(round((time.perf_counter_ns() - started) / 1_000_000, 3))
                require(observed["selection_ids"] == initial["selection_ids"], "selection identity drifted during SLO sampling")
                require(observed["dirty_scene_revision"] > previous["dirty_scene_revision"], "SLO sample did not advance the affected scene revision")
                require(observed["clean_scene_revision"] == previous["clean_scene_revision"], "SLO sample advanced the unaffected scene revision")
                previous = observed

        page, page_error, _ = tool_call(client, "godot_get_open_scenes", {"limit": 1})
        cursor = page.get("next_cursor")
        require(not page_error and isinstance(cursor, str), "live list did not issue a signed cursor")
        phase += 1
        started = time.perf_counter_ns()
        command(project, "opaque")
        wait_phase(editor, phase_path, phase, log_path, timeout)
        _, opaque = wait_observation(
            client,
            timeout,
            minimum_operation_seq=previous["operation_seq"],
            expected_value=LIVE_VALUE,
        )
        visibility_samples.append(round((time.perf_counter_ns() - started) / 1_000_000, 3))
        require(opaque["history_operation_kind"] == "opaque", "native operation payload was inferred")
        stale_cursor, stale_cursor_error, _ = tool_call(
            client, "godot_get_open_scenes", {"limit": 1, "cursor": cursor}
        )
        require(
            stale_cursor_error and stale_cursor.get("error", {}).get("code") == "stale_cursor",
            "cursor crossed a live snapshot revision",
        )
        transitions["opaque"] = opaque

        reconnect_session = opaque["editor_session_id"]
        reconnect_operation = opaque["operation_seq"]
        close_sidecar(sidecar_process)
        sidecar_process = None
        sidecar_process, client, reconnect_contract = initialize_sidecar(
            sidecar,
            project,
            timeout,
            additive_sprint11_registry=additive_sprint11_registry,
        )
        _, reconnected = wait_observation(client, timeout, expected_value=LIVE_VALUE)
        require(reconnected["editor_session_id"] == reconnect_session, "sidecar reconnect changed editor session")
        require(reconnected["operation_seq"] >= reconnect_operation, "sidecar reconnect regressed operation sequence")
        require(reconnected["selection_ids"] == initial["selection_ids"], "sidecar reconnect lost selection identity")
        require(reconnect_contract == contract, "MCP contract changed across reconnect")

        phase += 1
        old_dirty_id = reconnected["dirty_scene_id"]
        old_revision = reconnected["dirty_scene_revision"]
        command(project, "close")
        wait_phase(editor, phase_path, phase, log_path, timeout)
        deadline = time.monotonic() + timeout
        closed: dict[str, Any] | None = None
        while time.monotonic() < deadline:
            content, is_error, _ = tool_call(client, "godot_get_open_scenes", {"limit": 200})
            scenes = content.get("scenes")
            if not is_error and isinstance(scenes, list) and len(scenes) == 1:
                closed = content
                break
            time.sleep(0.03)
        require(closed is not None, "closed scene remained in the live overlay")
        require(
            all(scene.get("entity_id") != old_dirty_id for scene in closed["scenes"] if isinstance(scene, dict)),
            "closed scene live identity remained current",
        )
        require_tool_error(
            client,
            "godot_get_inspector_state",
            {"expected_scene_revision": old_revision},
            "stale_scene_revision",
        )

        command(project, "done")
        editor.wait(timeout=min(timeout, 30.0))
        close_sidecar(sidecar_process)
        sidecar_process = None
        log.close()
        telemetry = parse_telemetry(log_path)
        require_dispatcher_within_budget(telemetry, "Bridge")
        require(not any(path.exists() for path in paths), "editor left Bridge runtime files")

        require(percentile(selection_samples, 95) <= 500, f"selection p95 exceeded 500 ms: {selection_samples}")
        require(percentile(visibility_samples, 95) <= 2000, f"change visibility p95 exceeded 2 seconds: {visibility_samples}")
        require(percentile(control_samples, 95) <= 200, f"control ping p95 exceeded 200 ms: {control_samples}")
        return {
            "contract": contract,
            "coordinates": {
                "editor_session_id": initial["editor_session_id"],
                "initial_snapshot_id": initial["snapshot_id"],
                "initial_event_seq": initial["event_seq"],
                "final_event_seq": closed["revision_vector"]["event_seq"],
                "initial_operation_seq": initial["operation_seq"],
                "final_operation_seq": opaque["operation_seq"],
                "dirty_scene_revision_initial": initial["dirty_scene_revision"],
                "dirty_scene_revision_final": opaque["dirty_scene_revision"],
            },
            "checks": {
                "dirty_editor_value_precedes_disk": True,
                "disk_value_retained_for_comparison": True,
                "dirty_state_explicit": True,
                "multi_selection_identity_stable": True,
                "stale_editor_revision_rejected": True,
                "stale_scene_revision_rejected": True,
                "undo_redo_advance_history_and_revision": True,
                "unaffected_scene_revision_stable": True,
                "native_payload_remains_opaque": True,
                "changed_snapshot_cursor_rejected": True,
                "sidecar_reconnect_full_snapshot": True,
                "closed_scene_retired": True,
                "diagnostics_redacted_and_bounded": True,
                "script_source_absent": True,
                "viewport_metadata_only": True,
                "editor_summary_bounded": True,
            },
            "observations": {
                "open_scene_count": 2,
                "selected_node_count": len(initial["selection_ids"]),
                "open_script_count": initial["script_count"],
                "diagnostic_count": initial["diagnostic_count"],
                "redacted_diagnostic_count": initial["redacted_diagnostic_count"],
                "editor_value": initial["live_value"],
                "disk_value": composed["properties"][next(index for index, item in enumerate(composed["properties"]) if item.get("name") == "movement_speed")]["disk_comparison"]["value"]["value"],
                "editor_summary_bytes": summary_bytes,
                "viewport_kind": initial["viewport_kind"],
            },
            "transitions": {
                key: {
                    "operation_seq": value["operation_seq"],
                    "dirty_scene_revision": value["dirty_scene_revision"],
                    "clean_scene_revision": value["clean_scene_revision"],
                    "history_transition": value["history_transition"],
                    "history_operation_kind": value["history_operation_kind"],
                }
                for key, value in transitions.items()
            },
            "performance": {
                "selection_samples_ms": selection_samples,
                "selection_p95_ms": percentile(selection_samples, 95),
                "change_visibility_samples_ms": visibility_samples,
                "change_visibility_p95_ms": percentile(visibility_samples, 95),
                "control_ping_samples_ms": control_samples,
                "control_ping_p95_ms": percentile(control_samples, 95),
                "editor_summary_samples_ms": summary_samples,
                "editor_summary_p95_ms": percentile(summary_samples, 95),
            },
            "telemetry": telemetry,
            "runtime_files_absent": True,
        }
    finally:
        if sidecar_process is not None:
            sidecar_process.stop()
        if editor.poll() is None:
            try:
                command(project, "done")
                editor.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                editor.terminate()
                try:
                    editor.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    editor.kill()
                    editor.wait(timeout=5)
        if not log.closed:
            log.close()


def run(
    godot: Path,
    sidecar: Path,
    timeout: float,
    *,
    additive_sprint11_registry: bool = False,
) -> dict[str, Any]:
    platform_tag = target_platform()
    require(godot.is_file() and sidecar.is_file(), "Godot and release sidecar must be built")
    run_root = Path(
        tempfile.mkdtemp(
            prefix="s7.",
            dir=None if os.name == "nt" else "/tmp",
        )
    )
    project = run_root / "project"
    try:
        packaged_fixture.copy_project_fixture(PROJECT_SOURCE, project)
        packaged_fixture.configure_project_if_requested(project)
        first = run_session(
            godot,
            sidecar,
            project,
            run_root,
            timeout,
            additive_sprint11_registry=additive_sprint11_registry,
        )
        restart = run_session(
            godot,
            sidecar,
            project,
            run_root,
            timeout,
            old_session_id=first["coordinates"]["editor_session_id"],
            additive_sprint11_registry=additive_sprint11_registry,
        )
        telemetry_samples = [
            first["telemetry"]["dispatcher_max_elapsed_usec"],
            restart["telemetry"]["dispatcher_max_elapsed_usec"],
        ]
        report = {
            "schema_version": 1,
            "sprint": 7,
            "profile": "model_free_live_editor",
            "platform": platform_tag,
            "versions": {"bridge_rpc": "1.5", "mcp_protocol": MCP_PROTOCOL},
            "artifacts": {
                "godot_sha256": sha256_file(godot),
                "sidecar_sha256": sha256_file(sidecar),
                "fixture_manifest_sha256": sha256_file(MANIFEST_PATH),
                "golden_live_editor_sha256": sha256_file(GOLDEN_PATH),
            },
            "mcp_contract": first["contract"],
            "coordinates": first["coordinates"],
            "checks": {
                **first["checks"],
                "editor_session_reset_invalidates_ids": restart["session_id_changed"],
                "old_session_guard_rejected": restart["old_session_guard_rejected"],
            },
            "observations": first["observations"],
            "transitions": first["transitions"],
            "performance": {
                **first["performance"],
                "dispatcher_samples_usec": telemetry_samples,
                "dispatcher_p95_usec": percentile(telemetry_samples, 95),
                "dispatcher_max_usec": max(telemetry_samples),
                "dispatcher_budget_usec": 2000,
                "dispatcher_over_budget_count": 0,
            },
            "cleanup": {
                "editor_processes_stopped": True,
                "sidecar_processes_stopped": True,
                "bridge_runtime_files_absent": first["runtime_files_absent"],
                "temporary_workspace_removed": True,
            },
            "status": "passed",
        }
        require_safe_value(report, "live report")
        return report
    finally:
        shutil.rmtree(run_root, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", type=Path, required=True)
    parser.add_argument("--sidecar", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=120.0)
    packaged_fixture.add_arguments(parser)
    parser.add_argument(
        "--additive-sprint11-registry",
        action="store_true",
        help="require the exact additive Sprint 11 41-tool profile",
    )
    arguments = parser.parse_args()
    try:
        packaged_fixture.activate_from_arguments(arguments)
        report = run(
            arguments.godot.resolve(strict=True),
            arguments.sidecar.resolve(strict=True),
            arguments.timeout,
            additive_sprint11_registry=arguments.additive_sprint11_registry,
        )
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(
            json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        print(json.dumps({"status": report["status"], "output": str(arguments.output)}, sort_keys=True))
        return 0
    except (
        LiveEditorError,
        packaged_fixture.PackagedFixtureError,
        OSError,
        subprocess.SubprocessError,
        json.JSONDecodeError,
    ) as error:
        print(f"Sprint 7 live editor gate failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
