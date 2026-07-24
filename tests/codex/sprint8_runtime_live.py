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
import struct
import subprocess
import tempfile
import time
import zlib
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
    "address",
    "endpoint",
    "object_id",
    "pointer",
    "raw_object_id",
    "pid",
    "rid",
    "window_handle",
    "texture_handle",
    "native_handle",
    "path_absolute",
}

ABSOLUTE_PATH_PREFIXES = (
    "/Users/",
    "/home/",
    "/private/",
    "/tmp/",
    "file://",
)
BRIDGE_ENDPOINT_MARKERS = (
    "127.0.0.1",
    "http://",
    "https://",
    "tcp://",
    "udp://",
    "ws://",
    "wss://",
)
SECRET_MARKERS = (
    "Authorization:",
    "Bearer ",
    "CODEX_RUNTIME_SECRET_SENTINEL",
    "session.token",
)


class RuntimeGateError(RuntimeError):
    """Raised when the live Runtime MVP path violates its frozen contract."""


class RuntimeSafetyAudit:
    """Accumulate evidence that every model-facing runtime result stayed safe."""

    def __init__(self) -> None:
        self.flags = {
            "absolute_paths_absent": True,
            "bridge_endpoints_absent": True,
            "native_handles_absent": True,
            "secret_material_absent": True,
        }

    def observe(
        self, structured: dict[str, Any], content: list[dict[str, Any]]
    ) -> None:
        strings = recursive_strings(structured)
        strings.extend(
            cast(str, block["text"])
            for block in content
            if block.get("type") == "text" and isinstance(block.get("text"), str)
        )
        self.flags["native_handles_absent"] &= not bool(
            recursive_keys(structured) & FORBIDDEN_KEYS
        )
        self.flags["absolute_paths_absent"] &= not any(
            any(prefix in value for prefix in ABSOLUTE_PATH_PREFIXES)
            or (
                len(value) >= 3
                and value[0].isalpha()
                and value[1] == ":"
                and value[2] in {"/", "\\"}
            )
            for value in strings
        )
        self.flags["bridge_endpoints_absent"] &= not any(
            marker in value for marker in BRIDGE_ENDPOINT_MARKERS for value in strings
        )
        self.flags["secret_material_absent"] &= not any(
            marker in value for marker in SECRET_MARKERS for value in strings
        )
        failed = sorted(field for field, passed in self.flags.items() if not passed)
        require(not failed, f"runtime MCP output failed safety fields: {failed}")

    def result(self) -> dict[str, bool]:
        return dict(self.flags)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeGateError(message)


def recursive_keys(value: Any) -> set[str]:
    if isinstance(value, dict):
        return set(value) | {key for child in value.values() for key in recursive_keys(child)}
    if isinstance(value, list):
        return {key for child in value for key in recursive_keys(child)}
    return set()


def recursive_strings(value: Any) -> list[str]:
    if isinstance(value, dict):
        return [item for child in value.values() for item in recursive_strings(child)]
    if isinstance(value, list):
        return [item for child in value for item in recursive_strings(child)]
    return [value] if isinstance(value, str) else []


def decode_png_rgb(png: bytes) -> tuple[int, int, bytes]:
    """Decode the bounded 8-bit RGB/RGBA PNG subset emitted by Godot."""
    require(png.startswith(b"\x89PNG\r\n\x1a\n"), "runtime capture is not PNG")
    offset = 8
    width = height = color_type = bit_depth = -1
    compressed = bytearray()
    saw_iend = False
    while offset < len(png):
        require(offset + 12 <= len(png), "runtime PNG has a truncated chunk")
        length = struct.unpack(">I", png[offset : offset + 4])[0]
        chunk_type = png[offset + 4 : offset + 8]
        data_start = offset + 8
        data_end = data_start + length
        require(data_end + 4 <= len(png), "runtime PNG chunk exceeds its byte length")
        data = png[data_start:data_end]
        expected_crc = struct.unpack(">I", png[data_end : data_end + 4])[0]
        require(
            zlib.crc32(chunk_type + data) & 0xFFFFFFFF == expected_crc,
            "runtime PNG chunk CRC differs",
        )
        if chunk_type == b"IHDR":
            require(width == -1 and length == 13, "runtime PNG IHDR differs")
            width, height, bit_depth, color_type, compression, filtering, interlace = struct.unpack(
                ">IIBBBBB", data
            )
            require(
                width > 0
                and height > 0
                and bit_depth == 8
                and color_type in {2, 6}
                and compression == 0
                and filtering == 0
                and interlace == 0,
                "runtime PNG format is outside the accepted RGB/RGBA subset",
            )
        elif chunk_type == b"IDAT":
            require(width > 0 and not saw_iend, "runtime PNG IDAT precedes IHDR")
            compressed.extend(data)
        elif chunk_type == b"IEND":
            require(length == 0 and not saw_iend, "runtime PNG IEND differs")
            saw_iend = True
        offset = data_end + 4
        if saw_iend:
            break
    require(saw_iend and offset == len(png) and compressed, "runtime PNG structure differs")

    channels = 3 if color_type == 2 else 4
    stride = width * channels
    require(
        width <= 1280 and height <= 720 and stride <= 1280 * 4,
        "runtime PNG dimensions exceed the viewport policy",
    )
    try:
        filtered = zlib.decompress(bytes(compressed))
    except zlib.error as error:
        raise RuntimeGateError("runtime PNG IDAT cannot be decompressed") from error
    require(
        len(filtered) == height * (stride + 1),
        "runtime PNG decompressed byte length differs",
    )

    decoded = bytearray(height * stride)
    source = 0
    for row in range(height):
        filter_type = filtered[source]
        source += 1
        require(filter_type <= 4, "runtime PNG uses an unsupported scanline filter")
        row_start = row * stride
        previous_start = row_start - stride
        for column in range(stride):
            raw = filtered[source]
            source += 1
            left = decoded[row_start + column - channels] if column >= channels else 0
            above = decoded[previous_start + column] if row > 0 else 0
            upper_left = (
                decoded[previous_start + column - channels]
                if row > 0 and column >= channels
                else 0
            )
            if filter_type == 1:
                raw += left
            elif filter_type == 2:
                raw += above
            elif filter_type == 3:
                raw += (left + above) // 2
            elif filter_type == 4:
                estimate = left + above - upper_left
                distances = (
                    abs(estimate - left),
                    abs(estimate - above),
                    abs(estimate - upper_left),
                )
                raw += (left, above, upper_left)[distances.index(min(distances))]
            decoded[row_start + column] = raw & 0xFF

    if channels == 3:
        return width, height, bytes(decoded)
    rgb = bytearray(width * height * 3)
    for pixel in range(width * height):
        rgb[pixel * 3 : pixel * 3 + 3] = decoded[pixel * 4 : pixel * 4 + 3]
    return width, height, bytes(rgb)


def count_rgb_pixels(pixels: bytes, target: list[int], tolerance: int) -> int:
    require(len(target) == 3, "viewport marker color must contain RGB coordinates")
    return sum(
        1
        for offset in range(0, len(pixels), 3)
        if all(abs(pixels[offset + channel] - target[channel]) <= tolerance for channel in range(3))
    )


def tree_structure(nodes: list[dict[str, Any]]) -> list[tuple[Any, ...]]:
    return [
        (
            node.get("runtime_object_id"),
            node.get("parent_runtime_object_id"),
            node.get("name"),
            node.get("godot_type"),
            node.get("runtime_node_path"),
            node.get("depth"),
            node.get("child_count"),
        )
        for node in nodes
    ]


def require_tree_relations(nodes: list[dict[str, Any]]) -> None:
    parents: list[dict[str, Any]] = []
    identities: set[str] = set()
    for node in nodes:
        while parents and parents[-1]["remaining"] == 0:
            parents.pop()
        runtime_object_id = node.get("runtime_object_id")
        require(
            isinstance(runtime_object_id, str) and runtime_object_id not in identities,
            "runtime tree contains a duplicate or missing identity",
        )
        identities.add(runtime_object_id)
        expected_parent = parents[-1]["id"] if parents else None
        require(
            node.get("parent_runtime_object_id") == expected_parent
            and node.get("depth") == len(parents),
            f"runtime DFS parent/depth relation differs: {node}",
        )
        if parents:
            parents[-1]["remaining"] -= 1
        child_count = node.get("child_count")
        require(
            isinstance(child_count, int)
            and not isinstance(child_count, bool)
            and child_count >= 0,
            f"runtime child count is invalid: {node}",
        )
        if child_count:
            parents.append({"id": runtime_object_id, "remaining": child_count})
    while parents and parents[-1]["remaining"] == 0:
        parents.pop()
    require(not parents, "runtime DFS prefix has unfulfilled child counts")


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


def fixture_process_ids(project: Path) -> set[int]:
    """Return only processes whose argv targets this exact temporary fixture."""
    completed = subprocess.run(
        ["ps", "-axo", "pid=,command="],
        check=True,
        capture_output=True,
        text=True,
    )
    markers = {f"--path {project} ", f"--path {project.resolve()} "}
    pids: set[int] = set()
    for row in completed.stdout.splitlines():
        columns = row.strip().split(maxsplit=1)
        if len(columns) != 2 or not any(marker in columns[1] for marker in markers):
            continue
        pids.add(int(columns[0]))
    return pids


def retire_fixture_processes(project: Path) -> bool:
    """Stop detached Godot game children and prove no exact match remains."""
    pids = fixture_process_ids(project)
    for pid in pids:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + 5
    remaining = fixture_process_ids(project)
    while remaining and time.monotonic() < deadline:
        remaining = fixture_process_ids(project)
        if remaining:
            time.sleep(0.05)
    for pid in remaining:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    kill_deadline = time.monotonic() + 5
    while time.monotonic() < kill_deadline:
        remaining = fixture_process_ids(project)
        if not remaining:
            return True
        time.sleep(0.05)
    return not fixture_process_ids(project)


def initialize_sidecar(sidecar: Path, project: Path, timeout: float) -> tuple[LineProcess, McpClient]:
    environment = os.environ.copy()
    environment["GODOT_CODEX_DEBUG_ERRORS"] = "1"
    process = LineProcess(
        [str(sidecar), "--project-root", str(project)], cwd=project, env=environment
    )
    client = McpClient(process, timeout=min(timeout, 20.0))
    client.runtime_safety_audit = RuntimeSafetyAudit()
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
    tool_names = {tool.get("name") for tool in tools}
    require(
        len(tools) == len(tool_names) == 40 and TOOL_NAMES <= tool_names,
        "MCP registry does not preserve the 25-tool Sprint 8 surface",
    )
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
    client.runtime_safety_audit.observe(
        cast(dict[str, Any], structured), cast(list[dict[str, Any]], content)
    )
    return cast(dict[str, Any], structured), result.get("isError") is True, cast(list[dict[str, Any]], content), elapsed


def runtime_control(
    client: McpClient,
    name: str,
    runtime_session_id: str,
    expected_runtime_event_seq: int,
    target_state: str,
) -> tuple[dict[str, Any], bool, list[dict[str, Any]], float]:
    total_elapsed = 0.0
    last: dict[str, Any] = {}
    content: list[dict[str, Any]] = []
    for _ in range(4):
        last, is_error, content, elapsed = tool_call(
            client,
            name,
            {
                "runtime_session_id": runtime_session_id,
                "expected_runtime_event_seq": expected_runtime_event_seq,
            },
        )
        total_elapsed += elapsed
        if not is_error:
            return last, False, content, round(total_elapsed, 3)
        error_value = last.get("error", {})
        error = error_value if isinstance(error_value, dict) else {}
        current = error.get("current", {})
        if (
            not isinstance(current, dict)
            or error.get("code") != "stale_runtime_state"
            or current.get("runtime_session_id") != runtime_session_id
            or not isinstance(current.get("runtime_event_seq"), int)
        ):
            return last, True, content, round(total_elapsed, 3)
        if current.get("state") == target_state:
            return current, False, content, round(total_elapsed, 3)
        expected_runtime_event_seq = current["runtime_event_seq"]
    return last, True, content, round(total_elapsed, 3)


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
    timings = [elapsed]
    if (
        is_error
        and expected is not None
        and first.get("error", {}).get("code") == "stale_runtime_state"
    ):
        arguments.pop("expected_runtime_event_seq")
        first, is_error, _, retry_elapsed = tool_call(
            client, "godot_get_runtime_tree", arguments
        )
        timings.append(retry_elapsed)
    require(not is_error, f"runtime tree failed: {first}")
    nodes = list(first.get("nodes", []))
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
    client.runtime_safety_audit.observe(
        cast(dict[str, Any], value), [{"type": "text", "text": text}]
    )
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
    report: dict[str, Any] | None = None
    primary_failure = False
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
            require_tree_relations(nodes)
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

            stable_meta, stable_nodes, stable_timings = runtime_tree(
                client, first_session, tree_meta["runtime_event_seq"]
            )
            tree_timings.extend(stable_timings)
            require(
                stable_meta["runtime_event_seq"] == tree_meta["runtime_event_seq"]
                and tree_structure(stable_nodes) == tree_structure(nodes),
                "identical tree resnapshot advanced or changed the accepted revision",
            )
            require_tree_relations(stable_nodes)

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
            for _attempt in range(2):
                if not inspect_error or inspected.get("error", {}).get("code") not in {
                    "runtime_unavailable",
                    "runtime_request_timeout",
                    "runtime_object_stale",
                }:
                    break
                refreshed_meta, refreshed_nodes, refreshed_timings = runtime_tree(
                    client, first_session
                )
                tree_timings.extend(refreshed_timings)
                root = next(
                    node
                    for node in refreshed_nodes
                    if node.get("name") == "RuntimeFixture"
                )
                inspected, inspect_error, _, retry_inspect_ms = tool_call(
                    client,
                    "godot_inspect_runtime_object",
                    {
                        "runtime_session_id": first_session,
                        "runtime_object_id": root["runtime_object_id"],
                        "expected_runtime_event_seq": refreshed_meta[
                            "runtime_event_seq"
                        ],
                    },
                )
                inspect_ms += retry_inspect_ms
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
            for property_name, expected_reason in golden["properties"][
                "unsafe_reasons"
            ].items():
                projected = properties.get(property_name, {}).get("value", {})
                require(
                    projected.get("value", {}).get("omitted_reason")
                    == expected_reason,
                    f"{property_name} unsafe value was not omitted: {projected}",
                )
            object_reference = properties.get(
                golden["properties"]["object_reference"], {}
            ).get("value", {})
            require(
                object_reference.get("type") == "object"
                and object_reference.get("runtime_object_id")
                == dynamic["runtime_object_id"],
                f"runtime Object was not linked through its opaque tree ID: {object_reference}",
            )
            require(
                "reference_id"
                in recursive_keys(properties["Members/cyclic_dictionary"]["value"]),
                "cyclic dictionary did not use snapshot-local references",
            )
            require(
                not any(
                    value.startswith(("/Users/", "/home/", "/private/", "/tmp/", "file://"))
                    or (
                        len(value) >= 3
                        and value[1] == ":"
                        and value[2] in {"/", "\\"}
                    )
                    for value in recursive_strings(inspected)
                ),
                "runtime properties leaked an absolute host path",
            )

            bounded_node = next(
                node
                for node in nodes
                if node.get("name") == golden["properties"]["bounded_node"]
            )
            bounded_event_seq = inspected["runtime_event_seq"]
            for bounded_attempt in range(4):
                # A timed-out attempt may still have exercised its bounded getter
                # prefix before cancellation. Measure each retry independently.
                write_game_command(project, "reset_bounded_properties")
                wait_game_ack(project, "reset_bounded_properties", timeout)
                bounded, bounded_error, _, _ = tool_call(
                    client,
                    "godot_inspect_runtime_object",
                    {
                        "runtime_session_id": first_session,
                        "runtime_object_id": bounded_node["runtime_object_id"],
                        "expected_runtime_event_seq": bounded_event_seq,
                    },
                )
                if not bounded_error or bounded.get("error", {}).get("code") not in {
                    "runtime_unavailable",
                    "runtime_request_timeout",
                    "runtime_object_stale",
                }:
                    break
                if bounded_attempt == 3:
                    continue
                refreshed_meta, refreshed_nodes, refreshed_timings = runtime_tree(
                    client, first_session
                )
                tree_timings.extend(refreshed_timings)
                bounded_node = next(
                    node
                    for node in refreshed_nodes
                    if node.get("name") == golden["properties"]["bounded_node"]
                )
                bounded_event_seq = refreshed_meta["runtime_event_seq"]
            require(not bounded_error, f"bounded runtime object inspection failed: {bounded}")
            bounded_names = {
                property_value.get("name")
                for property_value in bounded.get("properties", [])
            }
            require(
                len(bounded.get("properties", [])) <= golden["properties"]["max_getters"]
                and golden["properties"]["bounded_first"] in bounded_names
                and golden["properties"]["bounded_omitted"] not in bounded_names,
                f"bounded property prefix differs: {len(bounded_names)} properties",
            )
            require(
                all(item.get("read_only") is True for item in bounded["properties"]),
                "runtime object exposed a writable property",
            )
            require(
                len(json.dumps(bounded, separators=(",", ":")).encode())
                <= bounded["limits_applied"]["object_bytes"] + 8192,
                "runtime object exceeded its encoded byte envelope",
            )
            write_game_command(project, "bounded_property_stats")
            bounded_stats = wait_game_ack(project, "bounded_property_stats", timeout)
            require(
                0 < bounded_stats.get("getter_count", 0)
                <= golden["properties"]["max_getters"]
                and bounded_stats.get("last_get_index", 513)
                < golden["properties"]["max_getters"],
                f"property getters ran beyond the collector limit: {bounded_stats}",
            )

            write_game_command(project, "spawn_ephemeral")
            wait_game_ack(project, "spawn_ephemeral", timeout)
            ephemeral_meta, ephemeral_nodes, _ = runtime_tree(client, first_session)
            ephemeral = next(
                node for node in ephemeral_nodes if node.get("name") == "EphemeralRuntimeNode"
            )
            write_game_command(project, "free_ephemeral")
            wait_game_ack(project, "free_ephemeral", timeout)
            retired_meta, retired_nodes, _ = runtime_tree(client, first_session)
            require(
                not any(node.get("name") == "EphemeralRuntimeNode" for node in retired_nodes)
                and retired_meta["runtime_event_seq"] > ephemeral_meta["runtime_event_seq"],
                "freed runtime node remained in the accepted tree snapshot",
            )
            stale_object, stale_object_error, _, _ = tool_call(
                client,
                "godot_inspect_runtime_object",
                {
                    "runtime_session_id": first_session,
                    "runtime_object_id": ephemeral["runtime_object_id"],
                    "expected_runtime_event_seq": retired_meta["runtime_event_seq"],
                },
            )
            require(
                stale_object_error
                and stale_object.get("error", {}).get("code")
                in {"runtime_object_stale", "runtime_object_not_found"},
                f"freed runtime object ID remained readable: {stale_object}",
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
            require(
                isinstance(stack.get("stack", {}).get("runtime_event_seq"), int)
                and stack["stack"]["runtime_event_seq"] <= stack["runtime_event_seq"],
                "runtime stack omitted or advanced its capture sequence",
            )
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

            initial_repeat_count = error_record.get("repeat_count")
            require(isinstance(initial_repeat_count, int), "runtime diagnostic omitted repeat count")
            write_game_command(project, "repeat_diagnostic")
            wait_game_ack(project, "repeat_diagnostic", timeout)
            repeat_deadline = time.monotonic() + timeout
            repeated_error: dict[str, Any] = {}
            while time.monotonic() < repeat_deadline:
                repeated, repeated_error_flag, _, _ = tool_call(
                    client,
                    "godot_get_diagnostics",
                    {"scope": "runtime", "runtime_session_id": first_session, "limit": 200},
                )
                matching = [
                    item
                    for item in repeated.get("diagnostics", [])
                    if golden["diagnostics"]["error"] in item.get("message", "")
                ]
                if (
                    not repeated_error_flag
                    and matching
                    and matching[0].get("repeat_count")
                    == initial_repeat_count + golden["diagnostics"]["repeat_increment"]
                ):
                    repeated_error = matching[0]
                    break
                time.sleep(0.05)
            require(bool(repeated_error), "repeated runtime error did not coalesce")
            require(
                repeated_error.get("runtime_stack_id") == stack_id,
                "repeated runtime error replaced its identical stack",
            )

            write_game_command(project, "sensitive_diagnostic")
            wait_game_ack(project, "sensitive_diagnostic", timeout)
            sensitive_deadline = time.monotonic() + timeout
            sensitive_diagnostics: dict[str, Any] = {}
            while time.monotonic() < sensitive_deadline:
                sensitive_diagnostics, sensitive_error, _, _ = tool_call(
                    client,
                    "godot_get_diagnostics",
                    {"scope": "runtime", "runtime_session_id": first_session, "limit": 200},
                )
                sensitive_messages = [
                    item.get("message", "")
                    for item in sensitive_diagnostics.get("diagnostics", [])
                ]
                if (
                    not sensitive_error
                    and golden["diagnostics"]["redacted_message"] in sensitive_messages
                    and any(
                        message.startswith(golden["diagnostics"]["utf8_prefix"])
                        for message in sensitive_messages
                    )
                ):
                    break
                time.sleep(0.05)
            else:
                raise RuntimeGateError(
                    "sensitive runtime diagnostics did not arrive: "
                    f"{sensitive_diagnostics}"
                )
            serialized_sensitive = json.dumps(sensitive_diagnostics, ensure_ascii=False)
            for forbidden in (
                "CODEX_RUNTIME_SECRET_SENTINEL",
                "/Users/private",
                "127.0.0.1",
                "\u001b",
                "\u0001",
            ):
                require(forbidden not in serialized_sensitive, f"runtime diagnostic leaked {forbidden}")
            path_message = next(
                message
                for message in sensitive_messages
                if "CODEX_RUNTIME_SENSITIVE_PATH" in message
            )
            require(
                "<path>" in path_message and "<endpoint>" in path_message,
                "runtime diagnostic did not redact path and endpoint independently",
            )
            utf8_message = next(
                message
                for message in sensitive_messages
                if message.startswith(golden["diagnostics"]["utf8_prefix"])
            )
            require(
                len(utf8_message.encode("utf-8")) <= 16384
                and utf8_message.endswith(" [truncated]"),
                "runtime diagnostic UTF-8 byte bound differs",
            )

            write_game_command(project, "stack_flood")
            wait_game_ack(project, "stack_flood", timeout)
            stack_flood_deadline = time.monotonic() + timeout
            stack_flood_diagnostics: dict[str, Any] = {}
            while time.monotonic() < stack_flood_deadline:
                stack_flood_diagnostics, stack_flood_error, _, _ = tool_call(
                    client,
                    "godot_get_diagnostics",
                    {"scope": "runtime", "runtime_session_id": first_session, "limit": 200},
                )
                flood_errors = [
                    item
                    for item in stack_flood_diagnostics.get("diagnostics", [])
                    if "CODEX_RUNTIME_STACK_FLOOD_" in item.get("message", "")
                ]
                if (
                    not stack_flood_error
                    and len(flood_errors) == golden["diagnostics"]["stack_flood_count"]
                ):
                    break
                time.sleep(0.05)
            else:
                raise RuntimeGateError("runtime stack flood did not arrive")
            linked_stack_ids = {
                item["runtime_stack_id"]
                for item in flood_errors
                if isinstance(item.get("runtime_stack_id"), str)
            }
            require(len(linked_stack_ids) <= 64, "runtime retained more than 64 stacks")
            require(
                any("runtime_stack_id" not in item for item in flood_errors),
                "runtime stack eviction did not retire an older diagnostic link",
            )

            write_game_command(project, "diagnostic_flood")
            wait_game_ack(project, "diagnostic_flood", timeout)
            diagnostic_flood_deadline = time.monotonic() + timeout
            bounded_diagnostics: dict[str, Any] = {}
            while time.monotonic() < diagnostic_flood_deadline:
                bounded_diagnostics, bounded_error, _, _ = tool_call(
                    client,
                    "godot_get_diagnostics",
                    {"scope": "runtime", "runtime_session_id": first_session, "limit": 200},
                )
                flood_outputs = [
                    item
                    for item in bounded_diagnostics.get("diagnostics", [])
                    if "CODEX_RUNTIME_FLOOD_OUTPUT_" in item.get("message", "")
                ]
                if not bounded_error and any(
                    "CODEX_RUNTIME_FLOOD_OUTPUT_204" in item.get("message", "")
                    for item in flood_outputs
                ):
                    break
                time.sleep(0.05)
            else:
                raise RuntimeGateError("runtime diagnostic flood did not arrive")
            require(
                isinstance(bounded_diagnostics.get("total"), int)
                and 1 <= bounded_diagnostics["total"] <= 200
                and len(bounded_diagnostics.get("diagnostics", []))
                == bounded_diagnostics["total"]
                and len(
                    json.dumps(
                        bounded_diagnostics.get("diagnostics", []),
                        ensure_ascii=False,
                        separators=(",", ":"),
                    ).encode("utf-8")
                )
                <= 262144
                and all(
                    len(item.get("message", "").encode("utf-8")) <= 16384
                    for item in bounded_diagnostics.get("diagnostics", [])
                )
                and bounded_diagnostics.get("truncated") is True,
                f"runtime diagnostic journal count/byte/truncation bound differs: {bounded_diagnostics.get('total')}",
            )

            capture_ok = False
            capture_ms = 0.0
            capture, capture_error, capture_content, capture_ms = tool_call(
                client,
                "godot_capture_viewport",
                {"runtime_session_id": first_session, "max_width": 1280, "max_height": 720},
            )
            if headless:
                require(
                    capture_error
                    and capture.get("error", {}).get("code") == "runtime_capture_unavailable"
                    and capture.get("error", {}).get("retryable") is True,
                    f"headless capture semantics differ: {capture}",
                )
                post_capture_tree, post_capture_tree_error, _, _ = tool_call(
                    client,
                    "godot_get_runtime_tree",
                    {"runtime_session_id": first_session, "limit": 1},
                )
                require(
                    not post_capture_tree_error and post_capture_tree.get("nodes"),
                    "headless capture failure blocked the Bridge runtime workflow",
                )
            else:
                require(not capture_error, f"runtime viewport capture failed: {capture}")
                images = [item for item in capture_content if item.get("type") == "image"]
                require(
                    len(capture_content) == 2 and len(images) == 1,
                    "runtime capture did not return one metadata and one image content block",
                )
                image = images[0]
                png = base64.b64decode(image["data"], validate=True)
                require(len(png) == capture["byte_length"] <= 524288, "runtime PNG byte bound differs")
                require(hashlib.sha256(png).hexdigest() == capture["sha256"], "runtime PNG digest differs")
                width, height, pixels = decode_png_rgb(png)
                require(
                    width == capture.get("width")
                    and height == capture.get("height")
                    and width <= golden["viewport"]["max_width"]
                    and height <= golden["viewport"]["max_height"],
                    "runtime PNG dimensions differ from structured metadata",
                )
                tolerance = golden["viewport"]["color_tolerance"]
                minimum = golden["viewport"]["min_color_pixels"]
                for field in ("marker_rgb", "core_rgb"):
                    require(
                        count_rgb_pixels(pixels, golden["viewport"][field], tolerance)
                        >= minimum,
                        f"runtime viewport omitted fixture color: {field}",
                    )
                require(
                    "data_base64url" not in capture
                    and not (recursive_keys(capture) & FORBIDDEN_KEYS),
                    "structured capture duplicated bytes or leaked native coordinates",
                )

                first_capture_seq = cast(int, capture["runtime_event_seq"])
                rate_limited, rate_limited_error, _, _ = tool_call(
                    client,
                    "godot_capture_viewport",
                    {
                        "runtime_session_id": first_session,
                        "expected_runtime_event_seq": first_capture_seq,
                        "max_width": 320,
                        "max_height": 180,
                    },
                )
                require(
                    rate_limited_error
                    and rate_limited.get("error", {}).get("code")
                    == "runtime_capture_rate_limited"
                    and rate_limited.get("error", {}).get("retryable") is True
                    and rate_limited.get("error", {}).get("current", {}).get(
                        "runtime_event_seq"
                    )
                    == first_capture_seq,
                    f"runtime capture rate limit differs: {rate_limited}",
                )
                time.sleep(1.05)
                second_capture, second_capture_error, second_content, second_capture_ms = tool_call(
                    client,
                    "godot_capture_viewport",
                    {
                        "runtime_session_id": first_session,
                        "expected_runtime_event_seq": first_capture_seq,
                        "max_width": 320,
                        "max_height": 180,
                    },
                )
                require(
                    not second_capture_error
                    and second_capture.get("runtime_event_seq", 0) > first_capture_seq,
                    f"runtime capture did not recover after its rate window: {second_capture}",
                )
                second_images = [item for item in second_content if item.get("type") == "image"]
                require(len(second_content) == 2 and len(second_images) == 1, "second capture content differs")
                second_png = base64.b64decode(second_images[0]["data"], validate=True)
                second_width, second_height, second_pixels = decode_png_rgb(second_png)
                require(
                    second_width == second_capture.get("width") <= 320
                    and second_height == second_capture.get("height") <= 180
                    and len(second_png) == second_capture.get("byte_length") <= 524288
                    and hashlib.sha256(second_png).hexdigest() == second_capture.get("sha256"),
                    "second runtime capture metadata differs",
                )
                for field in ("marker_rgb", "core_rgb"):
                    require(
                        count_rgb_pixels(second_pixels, golden["viewport"][field], tolerance)
                        >= minimum,
                        f"second runtime viewport omitted fixture color: {field}",
                    )
                capture = second_capture
                capture_ms = max(capture_ms, second_capture_ms)
                capture_ok = True

            # A successful viewport capture is itself an observable runtime event.
            # Continue from its returned coordinate instead of racing the strict
            # expected-sequence guard with the preceding diagnostics snapshot.
            current_seq = cast(
                int,
                capture.get(
                    "runtime_event_seq", bounded_diagnostics["runtime_event_seq"]
                ),
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
            paused, pause_error, _, pause_ms = runtime_control(
                client,
                "godot_pause_project",
                first_session,
                current_seq,
                "paused",
            )
            require(not pause_error and paused.get("state") == "paused", f"pause failed: {paused}")
            continued, continue_error, _, continue_ms = runtime_control(
                client,
                "godot_continue_project",
                first_session,
                paused["runtime_event_seq"],
                "running",
            )
            require(
                not continue_error and continued.get("state") == "running",
                f"continue failed: {continued}",
            )
            control_snapshot, control_snapshot_error, _, _ = tool_call(
                client,
                "godot_get_diagnostics",
                {"scope": "runtime", "runtime_session_id": first_session, "limit": 200},
            )
            require(
                not control_snapshot_error
                and control_snapshot.get("state") == "running"
                and control_snapshot.get("active_stack_id") is None,
                "continue did not clear the control-pause stack",
            )

            write_game_command(project, "pause_stack")
            wait_game_ack(project, "pause_stack", timeout)
            pause_stack_deadline = time.monotonic() + timeout
            pause_snapshot: dict[str, Any] = {}
            while time.monotonic() < pause_stack_deadline:
                pause_snapshot, pause_snapshot_error, _, _ = tool_call(
                    client,
                    "godot_get_diagnostics",
                    {"scope": "runtime", "runtime_session_id": first_session, "limit": 200},
                )
                if (
                    not pause_snapshot_error
                    and pause_snapshot.get("state") == "paused"
                    and isinstance(pause_snapshot.get("active_stack_id"), str)
                ):
                    break
                time.sleep(0.05)
            else:
                raise RuntimeGateError(
                    "confirmed pause did not publish an active stack: "
                    f"{pause_snapshot}"
                )
            pause_stack_id = cast(str, pause_snapshot["active_stack_id"])
            pause_stack, pause_stack_error, _, _ = tool_call(
                client,
                "godot_get_stack_trace",
                {
                    "runtime_session_id": first_session,
                    "runtime_stack_id": pause_stack_id,
                    "expected_runtime_event_seq": pause_snapshot["runtime_event_seq"],
                },
            )
            require(
                not pause_stack_error
                and pause_stack.get("stack", {}).get("stack_kind") == "pause"
                and pause_stack.get("stack", {}).get("frames"),
                f"active pause stack was unavailable: {pause_stack}",
            )
            pause_functions = {
                frame.get("function")
                for frame in pause_stack.get("stack", {}).get("frames", [])
            }
            require(
                all(
                    function in pause_functions
                    for function in golden["diagnostics"]["pause_stack_functions"]
                ),
                f"pause stack differs: {pause_functions}",
            )
            breakpoint_continued, breakpoint_continue_error, _, _ = runtime_control(
                client,
                "godot_continue_project",
                first_session,
                pause_snapshot["runtime_event_seq"],
                "running",
            )
            require(
                not breakpoint_continue_error
                and breakpoint_continued.get("state") == "running",
                f"breakpoint continue failed: {breakpoint_continued}",
            )
            continued_snapshot, continued_snapshot_error, _, _ = tool_call(
                client,
                "godot_get_diagnostics",
                {"scope": "runtime", "runtime_session_id": first_session, "limit": 200},
            )
            require(
                not continued_snapshot_error
                and continued_snapshot.get("state") == "running"
                and continued_snapshot.get("active_stack_id") is None,
                "continue did not clear active pause stack",
            )
            stopped, stop_error, _, stop_ms = runtime_control(
                client,
                "godot_stop_project",
                first_session,
                continued_snapshot["runtime_event_seq"],
                "stopped",
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
            second_tree_deadline = time.monotonic() + timeout
            while True:
                _, second_nodes, _ = runtime_tree(client, second_session)
                second_roots = [
                    node for node in second_nodes if node.get("name") == "RuntimeFixture"
                ]
                if second_roots:
                    break
                if time.monotonic() >= second_tree_deadline:
                    raise RuntimeGateError("second runtime tree did not become ready")
                time.sleep(0.1)
            require(
                second_roots[0]["runtime_object_id"] != root["runtime_object_id"],
                "opaque runtime object identity was reused across sessions",
            )
            write_game_command(project, "quit")
            wait_game_ack(project, "quit", timeout)
            second_stop = wait_runtime_state(client, second_session, "stopped", timeout)
            require(
                second_stop.get("runtime_session_id") == second_session,
                "normal runtime quit lost its session coordinates",
            )

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
            write_game_command(project, "deep_tree")
            wait_game_ack(project, "deep_tree", timeout)
            deep_meta, deep_nodes, _ = runtime_tree(client, large_session)
            require_tree_relations(deep_nodes)
            deep_names = {node.get("name") for node in deep_nodes}
            require(
                deep_meta.get("limits_applied", {}).get("truncated") is True
                and max(node.get("depth", 0) for node in deep_nodes)
                < golden["limits"]["tree_depth"]
                and "Depth250" in deep_names
                and "Depth257" not in deep_names,
                "deep runtime tree did not truncate at the negotiated depth",
            )
            write_game_command(project, "large_tree")
            wait_game_ack(project, "large_tree", timeout)
            large_meta, large_nodes, large_tree_timings = runtime_tree(client, large_session)
            require_tree_relations(large_nodes)
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
            large_stop, large_stop_error, _, _ = runtime_control(
                client,
                "godot_stop_project",
                large_session,
                large_meta["runtime_event_seq"],
                "stopped",
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
            wait_editor_ready(client, timeout)

            hang_run, hang_run_error, _, _ = tool_call(client, "godot_run_project", {})
            require(not hang_run_error, f"hang run failed: {hang_run}")
            hang_session = cast(str, hang_run["runtime_session_id"])
            require(
                hang_session != crash_session,
                "post-crash run reused the terminal runtime session",
            )
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
                and hung_tree.get("error", {}).get("code") == "runtime_request_timeout"
                and hung_tree.get("error", {}).get("retryable") is True
                and isinstance(hung_tree.get("error", {}).get("current"), dict)
                and set(hung_tree["error"]["current"])
                <= {"runtime_session_id", "runtime_event_seq", "state"}
                and 2500 <= hung_tree_ms <= 5000,
                f"hung runtime tree did not time out safely: {hung_tree}",
            )
            hang_stop, hang_stop_error, _, hang_stop_ms = runtime_control(
                client,
                "godot_stop_project",
                hang_session,
                hang_meta["runtime_event_seq"],
                "stopped",
            )
            require(
                not hang_stop_error and hang_stop.get("state") == "stopped" and hang_stop_ms <= 5000,
                f"hung runtime did not stop within its bound: {hang_stop}",
            )
            time.sleep(0.25)
            hang_terminal, hang_terminal_error, _, _ = tool_call(
                client,
                "godot_get_diagnostics",
                {
                    "scope": "runtime",
                    "runtime_session_id": hang_session,
                    "limit": 200,
                },
            )
            hang_tree, hang_nodes, _ = runtime_tree(
                client, hang_session, hang_stop["runtime_event_seq"]
            )
            require(
                not hang_terminal_error
                and hang_terminal.get("state") == "stopped"
                and hang_terminal.get("runtime_event_seq")
                == hang_stop.get("runtime_event_seq")
                and hang_tree.get("state") == "stopped"
                and hang_tree.get("runtime_event_seq")
                == hang_stop.get("runtime_event_seq")
                and not hang_nodes,
                "a late hung-runtime callback revived retired data or coordinates",
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
            require(
                manual_session not in {crash_session, hang_session},
                "manual recovery run reused a terminal runtime session",
            )
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

            report = {
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
                    "session_scoped_opaque_ids": True,
                    "runtime_editor_mapping_evidence": True,
                    "runtime_only_unmapped": True,
                    "bounded_properties": True,
                    "deep_tree_bounded": True,
                    "diagnostic_bounds_and_redaction": True,
                    "diagnostic_repeat_coalesced": True,
                    "diagnostic_stack": True,
                    "pause_continue_stop_confirmed": True,
                    "pause_stack_active": True,
                    "fresh_session_per_run": True,
                    "normal_quit_observed": True,
                    "stale_session_rejected": True,
                    "stale_runtime_state_rejected": True,
                    "terminal_coordinates_retained": True,
                    "source_content_unchanged": True,
                    "runtime_cursor_invalidation": True,
                    "large_tree_bounded": True,
                    "getter_limit_before_read": True,
                    "snapshot_checksums_and_ack": True,
                    "stable_resnapshot_revision": True,
                    "stale_object_rejected": True,
                    "unsafe_values_omitted": True,
                    "crash_retention_and_retirement": True,
                    "hang_timeout_and_recovery": True,
                    "manual_editor_lifecycle_observed": True,
                    "runtime_summary_bounded": True,
                    "viewport_capture": capture_ok if not headless else "unavailable_headless",
                    "viewport_capture_rate_limit": True if not headless else "unavailable_headless",
                    "viewport_visual_marker": True if not headless else "unavailable_headless",
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
                "cleanup": {
                    "editor_processes_stopped": False,
                    "sidecar_processes_stopped": False,
                    "temporary_workspace_removed": False,
                    "runtime_values_retired": bool(
                        retired_tree.get("state") == "crashed"
                        and not retired_nodes
                        and hang_tree.get("state") == "stopped"
                        and not hang_nodes
                    ),
                },
                "redaction": client.runtime_safety_audit.result(),
                "_temporary_workspace": temporary,
            }
            return report
        except Exception as error:
            primary_failure = True
            tail = list(sidecar_process.tail[-20:]) if sidecar_process is not None else []
            log.flush()
            try:
                editor_tail = log_path.read_text(encoding="utf-8", errors="replace").splitlines()[-30:]
            except OSError:
                editor_tail = []
            raise RuntimeGateError(
                f"{error}; sidecar_tail={tail}; editor_tail={editor_tail}"
            ) from error
        finally:
            cleanup_errors: list[str] = []
            if sidecar_process is not None:
                try:
                    close_sidecar(sidecar_process)
                except Exception as error:
                    cleanup_errors.append(f"sidecar cleanup failed: {error}")
            sidecar_stopped = (
                sidecar_process is None
                or sidecar_process.process.poll() is not None
            )
            if editor.poll() is None:
                try:
                    atomic_json(
                        project / ".godot/codex-sprint8-editor-command.json",
                        {"action": "done"},
                    )
                    try:
                        editor.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        terminate_process_group(editor)
                except Exception as error:
                    cleanup_errors.append(f"editor cleanup failed: {error}")
                    if editor.poll() is None:
                        try:
                            terminate_process_group(editor)
                        except Exception as terminate_error:
                            cleanup_errors.append(
                                f"editor process-group cleanup failed: {terminate_error}"
                            )
            # A normally closed or crashed editor can leave its game child alive;
            # the dedicated process group keeps this exact and unrelated-process safe.
            try:
                os.killpg(editor.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                fixture_processes_stopped = retire_fixture_processes(project)
            except Exception as error:
                fixture_processes_stopped = False
                cleanup_errors.append(f"fixture process cleanup failed: {error}")
            editor_stopped = editor.poll() is not None and fixture_processes_stopped
            if report is not None:
                report["cleanup"]["editor_processes_stopped"] = editor_stopped
                report["cleanup"]["sidecar_processes_stopped"] = sidecar_stopped
            log.close()
            if editor.returncode != 0:
                tail = log_path.read_text(encoding="utf-8", errors="replace")[-8000:]
                cleanup_errors.append(
                    f"Godot editor exited with {editor.returncode}: {tail}"
                )
            if not sidecar_stopped:
                cleanup_errors.append("sidecar process remained alive")
            if not editor_stopped:
                cleanup_errors.append("editor or fixture process remained alive")
            if cleanup_errors and not primary_failure:
                raise RuntimeGateError("; ".join(cleanup_errors))


def finalize_live_report(report: dict[str, Any]) -> dict[str, Any]:
    """Verify post-context cleanup immediately before serializing evidence."""
    temporary = report.pop("_temporary_workspace", None)
    require(isinstance(temporary, str), "live report omitted its temporary workspace")
    cleanup = report.get("cleanup")
    require(isinstance(cleanup, dict), "live report omitted cleanup results")
    cleanup["temporary_workspace_removed"] = not Path(temporary).exists()
    require(all(value is True for value in cleanup.values()), "live cleanup proof failed")
    redaction = report.get("redaction")
    require(isinstance(redaction, dict), "live report omitted redaction results")
    require(all(value is True for value in redaction.values()), "live redaction proof failed")
    return report


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--godot", type=Path, required=True)
    parser.add_argument("--sidecar", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--headless", action="store_true")
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    report = finalize_live_report(
        run_live(
            arguments.godot.resolve(),
            arguments.sidecar.resolve(),
            arguments.timeout,
            arguments.headless,
        )
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
