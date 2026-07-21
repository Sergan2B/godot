#!/usr/bin/env python3
"""Model-free Godot editor -> Bridge 1.6 -> MCP Runtime MVP gate."""

from __future__ import annotations

import argparse
import base64
import difflib
import hashlib
import json
import os
import signal
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any, cast

from sprint2_live_smoke import MCP_PROTOCOL, LineProcess, McpClient, atomic_json
from sprint5_script_semantics_live import close_sidecar

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
PROJECT_SOURCE = SCRIPT_DIR / "fixtures" / "runtime_mvp_project"
GOLDEN_PATH = SCRIPT_DIR / "fixtures" / "runtime_mvp_oracle" / "golden-runtime.json"
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
    "godot_get_runtime_tree",
    "godot_get_scene_graph",
    "godot_get_selected_nodes",
    "godot_get_stack_trace",
    "godot_get_viewport_state",
    "godot_inspect_node",
    "godot_inspect_runtime_object",
    "godot_inspect_symbol",
    "godot_search_symbols",
    "godot_capture_viewport",
    "godot_continue_project",
    "godot_pause_project",
    "godot_run_current_scene",
    "godot_run_project",
    "godot_stop_project",
}
FORBIDDEN_KEYS = {
    "object_id",
    "raw_object_id",
    "pid",
    "rid",
    "window_handle",
    "texture_handle",
    "native_handle",
    "path_absolute",
}


class RuntimeGateError(RuntimeError):
    """Raised when the live Runtime MVP path violates its frozen contract."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeGateError(message)


def recursive_keys(value: Any) -> set[str]:
    if isinstance(value, dict):
        return set(value) | {key for child in value.values() for key in recursive_keys(child)}
    if isinstance(value, list):
        return {key for child in value for key in recursive_keys(child)}
    return set()


def wait_file(process: subprocess.Popen[str], path: Path, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.is_file():
            return
        if process.poll() is not None:
            raise RuntimeGateError(f"Godot exited before {path.name}")
        time.sleep(0.05)
    raise RuntimeGateError(f"timed out waiting for {path.name}")


def terminate_process_group(process: subprocess.Popen[str]) -> None:
    """Retire the editor and every game process spawned for this isolated gate."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=5)


def retire_fixture_processes(project: Path) -> None:
    """Stop detached Godot game children that still target this temporary fixture."""
    completed = subprocess.run(
        ["ps", "-axo", "pid=,command="],
        check=True,
        capture_output=True,
        text=True,
    )
    markers = {f"--path {project} ", f"--path {project.resolve()} "}
    pids = []
    for row in completed.stdout.splitlines():
        columns = row.strip().split(maxsplit=1)
        if len(columns) != 2 or not any(marker in columns[1] for marker in markers):
            continue
        pids.append(int(columns[0]))
    for pid in pids:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + 5
    remaining = set(pids)
    while remaining and time.monotonic() < deadline:
        remaining = {pid for pid in remaining if Path(f"/proc/{pid}").exists()} if Path("/proc").exists() else {
            pid for pid in remaining if subprocess.run(["kill", "-0", str(pid)], capture_output=True).returncode == 0
        }
        if remaining:
            time.sleep(0.05)
    for pid in remaining:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
def initialize_sidecar(sidecar: Path, project: Path, timeout: float) -> tuple[LineProcess, McpClient]:
    environment = os.environ.copy()
    environment["GODOT_CODEX_DEBUG_ERRORS"] = "1"
    process = LineProcess(
        [str(sidecar), "--project-root", str(project)], cwd=project, env=environment
    )
    client = McpClient(process, timeout=min(timeout, 20.0))
    initialized = client.request(
        "initialize",
        {
            "protocolVersion": MCP_PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "sprint8-runtime-gate", "version": "1"},
        },
    )
    require(
        initialized.get("result", {}).get("protocolVersion") == MCP_PROTOCOL,
        "MCP protocol differs",
    )
    client.notify("notifications/initialized", {})
    listed = client.request("tools/list", {})
    tools = listed.get("result", {}).get("tools")
    require(isinstance(tools, list), "MCP tool list is absent")
    require({tool.get("name") for tool in tools} == TOOL_NAMES, "25-tool registry differs")
    require(
        all(
            tool.get("inputSchema", {}).get("additionalProperties") is False
            and tool.get("annotations", {}).get("openWorldHint") is False
            for tool in tools
        ),
        "runtime tool schemas or annotations differ",
    )
    return process, client


def tool_call(
    client: McpClient, name: str, arguments: dict[str, Any]
) -> tuple[dict[str, Any], bool, list[dict[str, Any]], float]:
    started = time.perf_counter_ns()
    response = client.request("tools/call", {"name": name, "arguments": arguments})
    elapsed = round((time.perf_counter_ns() - started) / 1_000_000, 3)
    require("error" not in response, f"{name} returned an MCP protocol error")
    result = response.get("result")
    require(isinstance(result, dict), f"{name} omitted result")
    structured = result.get("structuredContent")
    require(isinstance(structured, dict), f"{name} omitted structured content")
    require(not (recursive_keys(structured) & FORBIDDEN_KEYS), f"{name} leaked a raw/native key")
    content = result.get("content")
    require(isinstance(content, list), f"{name} omitted content")
    return cast(dict[str, Any], structured), result.get("isError") is True, cast(list[dict[str, Any]], content), elapsed


def wait_editor_ready(client: McpClient, timeout: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] = {}
    while time.monotonic() < deadline:
        value, is_error, _, _ = tool_call(client, "godot_get_editor_state", {})
        last = value
        if not is_error and value.get("status") == "ready":
            return value
        time.sleep(0.1)
    raise RuntimeGateError(f"editor snapshot did not become ready: {last}")


def runtime_tree(
    client: McpClient, runtime_session_id: str, expected: int | None = None
) -> tuple[dict[str, Any], list[dict[str, Any]], list[float]]:
    arguments: dict[str, Any] = {"runtime_session_id": runtime_session_id, "limit": 200}
    if expected is not None:
        arguments["expected_runtime_event_seq"] = expected
    first, is_error, _, elapsed = tool_call(client, "godot_get_runtime_tree", arguments)
    require(not is_error, f"runtime tree failed: {first}")
    nodes = list(first.get("nodes", []))
    timings = [elapsed]
    cursor = first.get("next_cursor")
    while isinstance(cursor, str):
        page_args = {
            "runtime_session_id": runtime_session_id,
            "limit": 200,
            "cursor": cursor,
        }
        page, page_error, _, page_elapsed = tool_call(
            client, "godot_get_runtime_tree", page_args
        )
        require(not page_error, f"runtime tree page failed: {page}")
        nodes.extend(page.get("nodes", []))
        timings.append(page_elapsed)
        cursor = page.get("next_cursor")
    require(len(nodes) == first.get("total"), "runtime tree pagination lost nodes")
    return first, cast(list[dict[str, Any]], nodes), timings


def write_game_command(project: Path, action: str) -> None:
    (project / ".godot/codex-sprint8-runtime-ack.json").unlink(missing_ok=True)
    atomic_json(project / ".godot/codex-sprint8-runtime-command.json", {"action": action})


def wait_game_ack(project: Path, kind: str, timeout: float) -> dict[str, Any]:
    path = project / ".godot/codex-sprint8-runtime-ack.json"
    deadline = time.monotonic() + timeout
    last: Any = None
    while time.monotonic() < deadline:
        try:
            last = json.loads(path.read_text(encoding="utf-8"))
        except (FileNotFoundError, json.JSONDecodeError):
            time.sleep(0.05)
            continue
        if isinstance(last, dict) and last.get("kind") == kind:
            return cast(dict[str, Any], last)
        time.sleep(0.05)
    raise RuntimeGateError(f"runtime fixture did not acknowledge {kind}: {last}")


def read_runtime_summary(client: McpClient) -> dict[str, Any]:
    response = client.request("resources/read", {"uri": "godot://runtime/summary"})
    text = response.get("result", {}).get("contents", [{}])[0].get("text")
    require(isinstance(text, str) and len(text.encode()) <= 4096, "runtime summary bound differs")
    value = json.loads(text)
    require(isinstance(value, dict), "runtime summary is not an object")
    return cast(dict[str, Any], value)


def percentile_95(values: list[float]) -> float:
    require(bool(values), "cannot calculate p95 of an empty sample")
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, max(0, int(len(ordered) * 0.95) - 1))]


def project_source_snapshot(project: Path) -> dict[str, str]:
    snapshot: dict[str, str] = {}
    files = sorted(
        path
        for path in project.rglob("*")
        if path.is_file() and ".godot" not in path.parts
    )
    for path in files:
        relative = path.relative_to(project).as_posix()
        snapshot[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
    return snapshot


def source_change_summary(
    before: dict[str, str], after: dict[str, str]
) -> dict[str, list[str]]:
    return {
        "added": sorted(after.keys() - before.keys()),
        "removed": sorted(before.keys() - after.keys()),
        "changed": sorted(
            path for path in before.keys() & after.keys() if before[path] != after[path]
        ),
    }


def wait_runtime_state(
    client: McpClient, runtime_session_id: str, state: str, timeout: float
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] = {}
    while time.monotonic() < deadline:
        value, is_error, _, _ = tool_call(
            client,
            "godot_get_diagnostics",
            {"scope": "runtime", "runtime_session_id": runtime_session_id, "limit": 200},
        )
        last = value
        if not is_error and value.get("state") == state:
            return value
        time.sleep(0.1)
    raise RuntimeGateError(f"runtime did not reach {state}: {last}")


def run_live(godot: Path, sidecar: Path, timeout: float, headless: bool) -> dict[str, Any]:
    golden = json.loads(GOLDEN_PATH.read_text(encoding="utf-8"))
    with tempfile.TemporaryDirectory(prefix="s8-", dir="/tmp") as temporary:
        project = Path(temporary) / "p"
        shutil.copytree(PROJECT_SOURCE, project, ignore=shutil.ignore_patterns(".godot"))
        initial_source_snapshot = project_source_snapshot(project)
        log_path = Path(temporary) / "editor.log"
        log = log_path.open("w", encoding="utf-8")
        environment = os.environ.copy()
        environment["CODEX_SPRINT8_AUTOMATION"] = "1"
        environment["GODOT_CODEX_EVIDENCE_TELEMETRY"] = "1"
        command = [str(godot), "--editor", "--path", str(project), "--no-header"]
        if headless:
            command.insert(2, "--headless")
        editor = subprocess.Popen(
            command,
            cwd=REPOSITORY_ROOT,
            env=environment,
            stdout=log,
            stderr=subprocess.STDOUT,
            text=True,
            start_new_session=True,
        )
        sidecar_process: LineProcess | None = None
        try:
            wait_file(editor, project / ".godot/codex-sprint8-editor-ready.json", timeout)
            wait_file(editor, project / ".godot/codex/bridge.json", timeout)
            sidecar_process, client = initialize_sidecar(sidecar, project, timeout)
            wait_editor_ready(client, timeout)

            run, run_error, _, run_ms = tool_call(client, "godot_run_project", {})
            require(not run_error and run.get("state") == "running", f"run failed: {run}")
            first_session = cast(str, run["runtime_session_id"])
            first_seq = cast(int, run["runtime_event_seq"])

            active_run, active_run_error, _, _ = tool_call(
                client, "godot_run_project", {}
            )
            require(
                active_run_error
                and active_run.get("error", {}).get("code")
                == "runtime_already_active"
                and active_run.get("error", {}).get("current", {}).get(
                    "runtime_session_id"
                )
                == first_session,
                f"an active runtime did not reject a second run: {active_run}",
            )

            tree_deadline = time.monotonic() + timeout
            tree_timings: list[float] = []
            tree_meta, nodes, attempt_timings = runtime_tree(client, first_session, first_seq)
            tree_timings.extend(attempt_timings)
            names = {node.get("name") for node in nodes}
            while not set(golden["tree"]["required_names"]) <= names and time.monotonic() < tree_deadline:
                time.sleep(0.1)
                tree_meta, nodes, attempt_timings = runtime_tree(client, first_session)
                tree_timings.extend(attempt_timings)
                names = {node.get("name") for node in nodes}
            require(
                set(golden["tree"]["required_names"]) <= names,
                f"runtime tree misses fixture nodes: {sorted(str(name) for name in names)}",
            )
            require(
                all(str(node.get("runtime_object_id", "")).startswith("runtime-object:") for node in nodes),
                "runtime tree exposes a non-opaque object identity",
            )
            static = next(node for node in nodes if node.get("name") == "StaticNode")
            while (
                static.get("source_mapping", {}).get("confidence") != "runtime_confirmed"
                and time.monotonic() < tree_deadline
            ):
                time.sleep(0.1)
                tree_meta, nodes, attempt_timings = runtime_tree(client, first_session)
                tree_timings.extend(attempt_timings)
                static = next(node for node in nodes if node.get("name") == "StaticNode")
            dynamic = next(node for node in nodes if node.get("name") == "RuntimeOnlyNode")
            require(
                static.get("source_mapping", {}).get("confidence") == "runtime_confirmed"
                and isinstance(static.get("source_mapping", {}).get("editor_node_id"), str)
                and {
                    evidence.get("source")
                    for evidence in static.get("source_mapping", {}).get("evidence", [])
                }
                == {
                    "runtime_debugger_tree",
                    "persistent_scene_state",
                    "live_editor_scene_state",
                },
                f"static runtime node lacks runtime-confirmed editor evidence: {static.get('source_mapping')}",
            )
            require(
                dynamic.get("source_mapping", {}).get("confidence") != "runtime_confirmed",
                "runtime-only node was incorrectly mapped",
            )

            root = next(node for node in nodes if node.get("name") == "RuntimeFixture")
            inspected, inspect_error, _, inspect_ms = tool_call(
                client,
                "godot_inspect_runtime_object",
                {
                    "runtime_session_id": first_session,
                    "runtime_object_id": root["runtime_object_id"],
                    "expected_runtime_event_seq": tree_meta["runtime_event_seq"],
                },
            )
            require(not inspect_error, f"runtime object inspection failed: {inspected}")
            properties = {item.get("name"): item for item in inspected.get("properties", [])}
            require("fixture_number" in properties, "fixture_number property is absent")
            require(
                properties["fixture_number"].get("value", {}).get("value")
                == golden["properties"]["fixture_number"],
                "fixture_number value differs",
            )
            for property_name in golden["properties"]["truncated"]:
                projected = properties.get(property_name, {}).get("value", {})
                require(
                    projected.get("truncated") is True
                    or isinstance(projected.get("omitted_reason"), str)
                    or "reference_id" in recursive_keys(projected),
                    f"{property_name} was not safely bounded: {projected}",
                )
            require(
                inspected.get("limits_applied", {}).get("properties") == 512,
                "runtime property limit differs",
            )

            cursor_page, cursor_page_error, _, _ = tool_call(
                client,
                "godot_get_runtime_tree",
                {"runtime_session_id": first_session, "limit": 1},
            )
            require(not cursor_page_error, f"runtime cursor setup failed: {cursor_page}")
            stale_runtime_cursor = cursor_page.get("next_cursor")
            require(isinstance(stale_runtime_cursor, str), "runtime cursor setup omitted cursor")
            write_game_command(project, "diagnostic")
            wait_game_ack(project, "diagnostic", timeout)
            cursor_deadline = time.monotonic() + timeout
            while True:
                stale_page, stale_page_error, _, _ = tool_call(
                    client,
                    "godot_get_runtime_tree",
                    {
                        "runtime_session_id": first_session,
                        "limit": 1,
                        "cursor": stale_runtime_cursor,
                    },
                )
                if stale_page_error and stale_page.get("error", {}).get("code") == "stale_cursor":
                    break
                if time.monotonic() >= cursor_deadline:
                    raise RuntimeGateError(f"runtime event did not invalidate cursor: {stale_page}")
                time.sleep(0.05)
            diagnostics: dict[str, Any] = {}
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                diagnostics, diag_error, _, _ = tool_call(
                    client,
                    "godot_get_diagnostics",
                    {"scope": "runtime", "runtime_session_id": first_session, "limit": 200},
                )
                messages = [item.get("message", "") for item in diagnostics.get("diagnostics", [])]
                if not diag_error and any(golden["diagnostics"]["error"] in message for message in messages):
                    break
                time.sleep(0.1)
            else:
                raise RuntimeGateError(f"deliberate runtime diagnostic did not arrive: {diagnostics}")
            error_record = next(
                item
                for item in diagnostics["diagnostics"]
                if golden["diagnostics"]["error"] in item.get("message", "")
            )
            stack_id = error_record.get("runtime_stack_id")
            require(isinstance(stack_id, str), "deliberate runtime error omitted stack ID")
            stack, stack_error, _, stack_ms = tool_call(
                client,
                "godot_get_stack_trace",
                {"runtime_session_id": first_session, "runtime_stack_id": stack_id},
            )
            require(not stack_error, f"runtime stack failed: {stack}")
            frames = stack.get("stack", {}).get("frames", [])
            functions = [frame.get("function") for frame in frames]
            require(
                all(function in functions for function in golden["diagnostics"]["stack_functions"]),
                f"runtime stack differs: {functions}",
            )
            require(
                all(
                    frame.get("script_path") in (None, golden["diagnostics"]["script_path"])
                    and isinstance(frame.get("line"), int)
                    and frame["line"] >= 1
                    for frame in frames
                ),
                "runtime stack leaked a path or zero-based line",
            )

            capture_ok = False
            capture_ms = 0.0
            capture, capture_error, capture_content, capture_ms = tool_call(
                client,
                "godot_capture_viewport",
                {"runtime_session_id": first_session, "max_width": 1280, "max_height": 720},
            )
            if headless:
                require(capture_error, "headless capture unexpectedly bypassed viewport availability")
            else:
                require(not capture_error, f"runtime viewport capture failed: {capture}")
                image = next((item for item in capture_content if item.get("type") == "image"), None)
                require(isinstance(image, dict), "runtime capture omitted MCP image content")
                png = base64.b64decode(image["data"], validate=True)
                require(png.startswith(b"\x89PNG\r\n\x1a\n"), "runtime capture is not PNG")
                require(len(png) == capture["byte_length"] <= 524288, "runtime PNG byte bound differs")
                require(hashlib.sha256(png).hexdigest() == capture["sha256"], "runtime PNG digest differs")
                require("data_base64url" not in capture, "structured capture duplicated image bytes")
                capture_ok = True

            # A successful viewport capture is itself an observable runtime event.
            # Continue from its returned coordinate instead of racing the strict
            # expected-sequence guard with the preceding diagnostics snapshot.
            current_seq = cast(
                int, capture.get("runtime_event_seq", diagnostics["runtime_event_seq"])
            )
            stale_pause, stale_pause_error, _, _ = tool_call(
                client,
                "godot_pause_project",
                {
                    "runtime_session_id": first_session,
                    "expected_runtime_event_seq": max(1, current_seq - 1),
                },
            )
            require(
                stale_pause_error
                and stale_pause.get("error", {}).get("code")
                == "stale_runtime_state"
                and stale_pause.get("error", {}).get("current", {}).get(
                    "runtime_event_seq"
                )
                == current_seq,
                f"stale runtime control did not fail closed: {stale_pause}",
            )
            paused, pause_error, _, pause_ms = tool_call(
                client,
                "godot_pause_project",
                {"runtime_session_id": first_session, "expected_runtime_event_seq": current_seq},
            )
            require(not pause_error and paused.get("state") == "paused", f"pause failed: {paused}")
            continued, continue_error, _, continue_ms = tool_call(
                client,
                "godot_continue_project",
                {"runtime_session_id": first_session},
            )
            require(not continue_error and continued.get("state") == "running", f"continue failed: {continued}")
            stopped, stop_error, _, stop_ms = tool_call(
                client,
                "godot_stop_project",
                {"runtime_session_id": first_session},
            )
            require(not stop_error and stopped.get("state") == "stopped", f"stop failed: {stopped}")
            terminal, terminal_error, _, _ = tool_call(
                client,
                "godot_get_diagnostics",
                {
                    "scope": "runtime",
                    "runtime_session_id": first_session,
                    "limit": 200,
                },
            )
            require(
                not terminal_error
                and terminal.get("runtime_session_id") == first_session
                and terminal.get("state") == "stopped"
                and terminal.get("runtime_event_seq")
                == stopped.get("runtime_event_seq"),
                f"terminal runtime coordinates were not retained: {terminal}",
            )

            second, second_error, _, _ = tool_call(client, "godot_run_current_scene", {})
            require(not second_error and second.get("state") == "running", f"second run failed: {second}")
            second_session = cast(str, second["runtime_session_id"])
            require(second_session != first_session, "runtime_session_id was reused")
            stale, stale_error, _, _ = tool_call(
                client,
                "godot_get_runtime_tree",
                {"runtime_session_id": first_session, "limit": 1},
            )
            require(
                stale_error and stale.get("error", {}).get("code") == "stale_runtime_session",
                "old runtime session did not fail closed",
            )
            second_stop, second_stop_error, _, _ = tool_call(
                client,
                "godot_stop_project",
                {"runtime_session_id": second_session},
            )
            require(not second_stop_error and second_stop.get("state") == "stopped", "second stop failed")

            large_run, large_run_error, _, _ = tool_call(client, "godot_run_project", {})
            require(not large_run_error, f"large-tree run failed: {large_run}")
            large_session = cast(str, large_run["runtime_session_id"])
            large_ready_deadline = time.monotonic() + timeout
            while True:
                large_meta, large_nodes, _ = runtime_tree(client, large_session)
                if any(node.get("name") == "RuntimeFixture" for node in large_nodes):
                    break
                if time.monotonic() >= large_ready_deadline:
                    raise RuntimeGateError("large-tree runtime did not become ready")
                time.sleep(0.1)
            write_game_command(project, "large_tree")
            wait_game_ack(project, "large_tree", timeout)
            large_meta, large_nodes, large_tree_timings = runtime_tree(client, large_session)
            require(
                large_meta.get("total") == golden["limits"]["tree_nodes"]
                and len(large_nodes) == golden["limits"]["tree_nodes"]
                and large_meta.get("limits_applied", {}).get("truncated") is True,
                f"large runtime tree was not deterministically bounded: {large_meta.get('total')}",
            )
            require(
                percentile_95(large_tree_timings) <= 500,
                f"cached runtime page p95 exceeded 500 ms: {large_tree_timings}",
            )
            large_stop, large_stop_error, _, _ = tool_call(
                client,
                "godot_stop_project",
                {
                    "runtime_session_id": large_session,
                    "expected_runtime_event_seq": large_meta["runtime_event_seq"],
                },
            )
            require(not large_stop_error and large_stop.get("state") == "stopped", "large-tree stop failed")

            crash_run, crash_run_error, _, _ = tool_call(client, "godot_run_project", {})
            require(not crash_run_error, f"crash run failed: {crash_run}")
            crash_session = cast(str, crash_run["runtime_session_id"])
            crash_ready_deadline = time.monotonic() + timeout
            while True:
                _, crash_nodes, _ = runtime_tree(client, crash_session)
                if any(node.get("name") == "RuntimeFixture" for node in crash_nodes):
                    break
                if time.monotonic() >= crash_ready_deadline:
                    raise RuntimeGateError("crash runtime did not become ready")
                time.sleep(0.1)
            crash_root = next(node for node in crash_nodes if node.get("name") == "RuntimeFixture")
            write_game_command(project, "diagnostic")
            wait_game_ack(project, "diagnostic", timeout)
            crash_diagnostics: dict[str, Any] = {}
            crash_diagnostic_deadline = time.monotonic() + timeout
            while time.monotonic() < crash_diagnostic_deadline:
                crash_diagnostics, crash_diag_error, _, _ = tool_call(
                    client,
                    "godot_get_diagnostics",
                    {"scope": "runtime", "runtime_session_id": crash_session, "limit": 200},
                )
                crash_errors = [
                    item
                    for item in crash_diagnostics.get("diagnostics", [])
                    if golden["diagnostics"]["error"] in item.get("message", "")
                ]
                if not crash_diag_error and crash_errors:
                    break
                time.sleep(0.1)
            else:
                raise RuntimeGateError("pre-crash diagnostic did not arrive")
            retained_stack_id = cast(str, crash_errors[0]["runtime_stack_id"])
            write_game_command(project, "crash")
            crashed = wait_runtime_state(client, crash_session, "crashed", timeout)
            require(
                any(
                    golden["diagnostics"]["error"] in item.get("message", "")
                    for item in crashed.get("diagnostics", [])
                ),
                "crash boundary did not retain bounded diagnostics",
            )
            retired_tree, retired_nodes, _ = runtime_tree(client, crash_session)
            require(
                retired_tree.get("state") == "crashed" and not retired_nodes,
                "crash boundary did not retire runtime tree",
            )
            retired_object, retired_object_error, _, _ = tool_call(
                client,
                "godot_inspect_runtime_object",
                {
                    "runtime_session_id": crash_session,
                    "runtime_object_id": crash_root["runtime_object_id"],
                },
            )
            require(
                retired_object_error
                and retired_object.get("error", {}).get("code")
                in {"runtime_crashed", "runtime_data_retired", "runtime_object_stale"},
                f"crashed runtime object remained readable: {retired_object}",
            )
            retained_stack, retained_stack_error, _, _ = tool_call(
                client,
                "godot_get_stack_trace",
                {"runtime_session_id": crash_session, "runtime_stack_id": retained_stack_id},
            )
            require(not retained_stack_error, f"terminal stack was not retained: {retained_stack}")

            hang_run, hang_run_error, _, _ = tool_call(client, "godot_run_project", {})
            require(not hang_run_error, f"hang run failed: {hang_run}")
            hang_session = cast(str, hang_run["runtime_session_id"])
            hang_ready_deadline = time.monotonic() + timeout
            while True:
                hang_meta, hang_nodes, _ = runtime_tree(client, hang_session)
                if any(node.get("name") == "RuntimeFixture" for node in hang_nodes):
                    break
                if time.monotonic() >= hang_ready_deadline:
                    raise RuntimeGateError("hang runtime did not become ready")
                time.sleep(0.1)
            wait_editor_ready(client, timeout)
            write_game_command(project, "hang")
            wait_game_ack(project, "hang", timeout)
            bridge_ping_timings = []
            for _ in range(10):
                editor_state, editor_error, _, editor_ms = tool_call(client, "godot_get_editor_state", {})
                require(not editor_error, f"hung game blocked editor state: {editor_state}")
                bridge_ping_timings.append(editor_ms)
            require(
                percentile_95(bridge_ping_timings) <= 200,
                f"hung-game bridge p95 exceeded 200 ms: {bridge_ping_timings}",
            )
            hung_tree, hung_tree_error, _, hung_tree_ms = tool_call(
                client,
                "godot_get_runtime_tree",
                {
                    "runtime_session_id": hang_session,
                    "expected_runtime_event_seq": hang_meta["runtime_event_seq"],
                    "limit": 200,
                },
            )
            require(
                hung_tree_error
                and hung_tree.get("error", {}).get("code") == "runtime_request_timeout",
                f"hung runtime tree did not time out safely: {hung_tree}",
            )
            hang_stop, hang_stop_error, _, hang_stop_ms = tool_call(
                client,
                "godot_stop_project",
                {"runtime_session_id": hang_session},
            )
            require(
                not hang_stop_error and hang_stop.get("state") == "stopped" and hang_stop_ms <= 5000,
                f"hung runtime did not stop within its bound: {hang_stop}",
            )

            atomic_json(project / ".godot/codex-sprint8-editor-command.json", {"action": "manual_run"})
            manual_deadline = time.monotonic() + timeout
            manual_summary: dict[str, Any] = {}
            while time.monotonic() < manual_deadline:
                manual_summary = read_runtime_summary(client)
                if (
                    manual_summary.get("state") == "running"
                    and manual_summary.get("runtime_session_id") != hang_session
                ):
                    break
                time.sleep(0.1)
            else:
                raise RuntimeGateError(f"manual editor run was not observed: {manual_summary}")
            manual_session = cast(str, manual_summary["runtime_session_id"])
            manual_tree_deadline = time.monotonic() + timeout
            while True:
                _, manual_nodes, _ = runtime_tree(client, manual_session)
                if any(node.get("name") == "RuntimeFixture" for node in manual_nodes):
                    break
                if time.monotonic() >= manual_tree_deadline:
                    break
                time.sleep(0.1)
            require(
                any(node.get("name") == "RuntimeFixture" for node in manual_nodes),
                "manual editor run did not expose its runtime tree",
            )
            atomic_json(project / ".godot/codex-sprint8-editor-command.json", {"action": "manual_stop"})
            manual_stop_deadline = time.monotonic() + timeout
            while time.monotonic() < manual_stop_deadline:
                manual_summary = read_runtime_summary(client)
                if (
                    manual_summary.get("runtime_session_id") == manual_session
                    and manual_summary.get("state") == "stopped"
                ):
                    break
                time.sleep(0.1)
            else:
                raise RuntimeGateError(f"manual editor stop was not observed: {manual_summary}")

            read_runtime_summary(client)

            final_source_snapshot = project_source_snapshot(project)
            source_changes = source_change_summary(
                initial_source_snapshot, final_source_snapshot
            )
            source_change_detail = ""
            if "project.godot" in source_changes["changed"]:
                before_lines = (PROJECT_SOURCE / "project.godot").read_text(
                    encoding="utf-8"
                ).splitlines()
                after_lines = (project / "project.godot").read_text(
                    encoding="utf-8"
                ).splitlines()
                source_change_detail = "\n".join(
                    difflib.unified_diff(
                        before_lines,
                        after_lines,
                        fromfile="project.godot.before",
                        tofile="project.godot.after",
                        lineterm="",
                    )
                )
            require(
                not any(source_changes.values()),
                "read-only runtime workflow changed fixture source content: "
                f"{source_changes}; diff={source_change_detail}",
            )

            return {
                "schema_version": 1,
                "sprint": 8,
                "status": "passed",
                "headless": headless,
                "sessions": [
                    first_session,
                    second_session,
                    large_session,
                    crash_session,
                    hang_session,
                    manual_session,
                ],
                "checks": {
                    "exact_25_tool_registry": True,
                    "active_run_rejected": True,
                    "runtime_tree_and_opaque_ids": True,
                    "runtime_editor_mapping_evidence": True,
                    "runtime_only_unmapped": True,
                    "bounded_properties": True,
                    "diagnostic_stack": True,
                    "pause_continue_stop_confirmed": True,
                    "fresh_session_per_run": True,
                    "stale_session_rejected": True,
                    "stale_runtime_state_rejected": True,
                    "terminal_coordinates_retained": True,
                    "source_content_unchanged": True,
                    "runtime_cursor_invalidation": True,
                    "large_tree_bounded": True,
                    "crash_retention_and_retirement": True,
                    "hang_timeout_and_recovery": True,
                    "manual_editor_lifecycle_observed": True,
                    "runtime_summary_bounded": True,
                    "viewport_capture": capture_ok if not headless else "unavailable_headless",
                },
                "observations": {
                    "runtime_nodes": len(nodes),
                    "diagnostics": len(diagnostics.get("diagnostics", [])),
                    "stack_frames": len(frames),
                    "large_runtime_nodes": len(large_nodes),
                },
                "timings_ms": {
                    "run": run_ms,
                    "tree_pages": tree_timings,
                    "inspect": inspect_ms,
                    "stack": stack_ms,
                    "capture": capture_ms,
                    "pause": pause_ms,
                    "continue": continue_ms,
                    "stop": stop_ms,
                    "large_tree_pages": large_tree_timings,
                    "large_tree_page_p95": percentile_95(large_tree_timings),
                    "hung_tree_timeout": hung_tree_ms,
                    "hung_game_bridge_pings": bridge_ping_timings,
                    "hung_game_bridge_ping_p95": percentile_95(bridge_ping_timings),
                    "hung_game_stop": hang_stop_ms,
                },
            }
        except Exception as error:
            tail = list(sidecar_process.tail[-20:]) if sidecar_process is not None else []
            raise RuntimeGateError(f"{error}; sidecar_tail={tail}") from error
        finally:
            if sidecar_process is not None:
                close_sidecar(sidecar_process)
            if editor.poll() is None:
                atomic_json(project / ".godot/codex-sprint8-editor-command.json", {"action": "done"})
                try:
                    editor.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    terminate_process_group(editor)
            # A normally closed or crashed editor can leave its game child alive;
            # the dedicated process group keeps this exact and unrelated-process safe.
            try:
                os.killpg(editor.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            retire_fixture_processes(project)
            log.close()
            if editor.returncode != 0:
                tail = log_path.read_text(encoding="utf-8", errors="replace")[-8000:]
                raise RuntimeGateError(f"Godot editor exited with {editor.returncode}: {tail}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--godot", type=Path, required=True)
    parser.add_argument("--sidecar", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--headless", action="store_true")
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    report = run_live(
        arguments.godot.resolve(),
        arguments.sidecar.resolve(),
        arguments.timeout,
        arguments.headless,
    )
    encoded = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if arguments.output:
        if arguments.output.exists():
            try:
                existing = json.loads(arguments.output.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                existing = None
            if (
                isinstance(existing, dict)
                and existing.get("sprint") == 8
                and existing.get("status") == "passed"
            ):
                raise RuntimeGateError(
                    f"refusing to overwrite qualifying evidence: {arguments.output}"
                )
        atomic_json(arguments.output, report)
    print(encoded, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
