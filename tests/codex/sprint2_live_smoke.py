#!/usr/bin/env python3
"""Model-free Sprint 2 live editor → bridge → sidecar → MCP smoke test."""

from __future__ import annotations

import argparse
import json
import os
import platform
import queue
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any


MCP_PROTOCOL = "2025-11-25"
TOOL_NAMES = {
    "godot_get_current_scene",
    "godot_get_editor_state",
    "godot_get_selected_nodes",
}
DISK_VALUE = 240.0
FIRST_VALUE = 275.0
SECOND_VALUE = 310.0


class SmokeFailure(RuntimeError):
    pass


class LineProcess:
    def __init__(self, arguments: list[str], *, cwd: Path, env: dict[str, str]) -> None:
        self.lines: queue.Queue[str] = queue.Queue()
        self.tail: list[str] = []
        self.process = subprocess.Popen(
            arguments,
            cwd=cwd,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        threading.Thread(target=self._read_stdout, daemon=True).start()
        threading.Thread(target=self._read_stderr, daemon=True).start()

    def _remember(self, line: str) -> None:
        self.tail.append(line.rstrip())
        del self.tail[:-200]

    def _read_stdout(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            self.lines.put(line)

    def _read_stderr(self) -> None:
        assert self.process.stderr is not None
        for line in self.process.stderr:
            self._remember(line)

    def stop(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)


class McpClient:
    def __init__(self, process: LineProcess, timeout: float) -> None:
        self.process = process
        self.timeout = timeout
        self.next_id = 1
        self.pending: dict[int, dict[str, Any]] = {}

    def _send(self, message: dict[str, Any]) -> None:
        stdin = self.process.process.stdin
        if stdin is None:
            raise SmokeFailure("sidecar stdin is unavailable")
        stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        stdin.flush()

    def notify(self, method: str, params: dict[str, Any]) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        request_id = self.next_id
        self.next_id += 1
        self._send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params,
            }
        )
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            if request_id in self.pending:
                return self.pending.pop(request_id)
            if self.process.process.poll() is not None:
                raise SmokeFailure(
                    f"sidecar exited with {self.process.process.returncode}: "
                    + " | ".join(self.process.tail[-10:])
                )
            try:
                line = self.process.lines.get(timeout=0.1)
            except queue.Empty:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError as error:
                raise SmokeFailure("sidecar stdout contained non-JSON data") from error
            if isinstance(message.get("id"), int):
                self.pending[message["id"]] = message
        raise SmokeFailure(f"MCP request timed out: {method}")


def wait_for_json(path: Path, phase: int, timeout: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            value = json.loads(path.read_text(encoding="utf-8"))
            if "error" in value:
                raise SmokeFailure(f"editor automation failed: {value['error']}")
            if value.get("phase") == phase:
                return value
        except (FileNotFoundError, json.JSONDecodeError) as error:
            last_error = error
        time.sleep(0.05)
    raise SmokeFailure(f"editor phase {phase} timed out ({last_error})")


def result_content(response: dict[str, Any]) -> dict[str, Any] | None:
    if "error" in response:
        raise SmokeFailure(f"MCP protocol error: {response['error']}")
    result = response.get("result", {})
    if result.get("isError"):
        return None
    content = result.get("structuredContent")
    return content if isinstance(content, dict) else None


def selected_observation(content: dict[str, Any], expected: float) -> dict[str, Any] | None:
    if content.get("freshness") != "current" or content.get("scene_dirty") is not True:
        return None
    nodes = content.get("selected_nodes")
    if not isinstance(nodes, list) or len(nodes) != 1:
        return None
    node = nodes[0]
    scene = content.get("scene")
    if not isinstance(scene, dict):
        return None
    if (
        node.get("node_path") != "Player"
        or node.get("godot_type") != "CharacterBody2D"
        or node.get("owner_path") != "."
        or node.get("script_path") != "res://player.gd"
        or not isinstance(node.get("entity_id"), str)
        or not node["entity_id"].startswith("node:")
        or not isinstance(node.get("scene_id"), str)
        or node["scene_id"] != scene.get("entity_id")
        or scene.get("path") != "res://main.tscn"
    ):
        return None
    properties = node.get("properties")
    if not isinstance(properties, list):
        return None
    property_value = next(
        (item for item in properties if item.get("name") == "movement_speed"), None
    )
    if (
        property_value is None
        or property_value.get("value") != expected
        or property_value.get("source") != "live_editor_property"
        or property_value.get("freshness") != "current"
        or not isinstance(property_value.get("scene_revision"), int)
    ):
        return None
    revisions = content.get("revision_vector")
    if not isinstance(revisions, dict) or not isinstance(revisions.get("event_seq"), int):
        return None
    if not isinstance(content.get("project_id"), str) or not content["project_id"].startswith(
        "project:sha256:"
    ):
        return None
    if not isinstance(content.get("editor_session_id"), str) or not content[
        "editor_session_id"
    ].startswith("editor:"):
        return None
    return {
        "snapshot_id": content.get("snapshot_id"),
        "project_id": content["project_id"],
        "editor_session_id": content["editor_session_id"],
        "scene_id": scene["entity_id"],
        "node_id": node["entity_id"],
        "event_seq": revisions["event_seq"],
        "scene_revision": property_value["scene_revision"],
        "node_path": node["node_path"],
        "godot_type": node["godot_type"],
        "owner_path": node["owner_path"],
        "script_path": node["script_path"],
        "scene_dirty": True,
        "property": {
            "name": "movement_speed",
            "value": property_value["value"],
            "source": property_value["source"],
            "freshness": property_value["freshness"],
        },
    }


def poll_observation(client: McpClient, expected: float, timeout: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last_result: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        response = client.request(
            "tools/call",
            {"name": "godot_get_selected_nodes", "arguments": {}},
        )
        last_result = response.get("result")
        content = result_content(response)
        if content is not None:
            observation = selected_observation(content, expected)
            if observation is not None:
                return observation
        time.sleep(0.1)
    raise SmokeFailure(
        f"MCP never returned the live movement_speed={expected}; "
        f"last_result={json.dumps(last_result, sort_keys=True)}; "
        f"sidecar_tail={' | '.join(client.process.tail[-10:])}"
    )


def atomic_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    temporary.replace(path)


def version_output(executable: Path, cwd: Path) -> str:
    result = subprocess.run(
        [str(executable), "--version"],
        cwd=cwd,
        text=True,
        encoding="utf-8",
        capture_output=True,
        timeout=10,
        check=False,
    )
    if result.returncode != 0 or not result.stdout.strip():
        raise SmokeFailure(f"cannot read version from {executable.name}")
    return result.stdout.strip().splitlines()[0]


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", required=True, type=Path)
    parser.add_argument("--sidecar", required=True, type=Path)
    parser.add_argument("--project-root", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--timeout", type=float, default=45.0)
    return parser.parse_args()


def main() -> int:
    arguments = parse_arguments()
    machine = platform.machine().lower()
    if sys.platform == "win32" and machine in {"amd64", "x86_64"}:
        platform_tag = "windows-x86_64"
    elif sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
        platform_tag = "macos-arm64"
    else:
        raise SmokeFailure(
            "the Sprint 2 live gate requires Windows x86_64 or macOS arm64"
        )
    project_root = arguments.project_root.resolve(strict=True)
    godot = arguments.godot.resolve(strict=True)
    sidecar = arguments.sidecar.resolve(strict=True)
    godot_version = version_output(godot, project_root)
    sidecar_version = version_output(sidecar, project_root)
    scene_path = project_root / "main.tscn"
    disk_before = scene_path.read_text(encoding="utf-8")
    if "movement_speed = 240.0" not in disk_before:
        raise SmokeFailure("fixture disk value is not movement_speed=240.0")

    phase_path = project_root / ".godot/codex-sprint2-phase.json"
    next_path = project_root / ".godot/codex-sprint2-next"
    done_path = project_root / ".godot/codex-sprint2-done"
    for marker in (phase_path, next_path, done_path):
        marker.unlink(missing_ok=True)

    environment = os.environ.copy()
    environment["CODEX_SPRINT2_AUTOMATION"] = "1"
    editor = LineProcess(
        [str(godot), "--editor", "--headless", "--path", str(project_root)],
        cwd=project_root,
        env=environment,
    )
    mcp: LineProcess | None = None
    try:
        wait_for_json(phase_path, 1, arguments.timeout)
        discovery = project_root / ".godot/codex/bridge.json"
        deadline = time.monotonic() + arguments.timeout
        while not discovery.is_file() and time.monotonic() < deadline:
            time.sleep(0.05)
        if not discovery.is_file():
            raise SmokeFailure("bridge discovery did not appear")

        mcp = LineProcess(
            [str(sidecar), "--project-root", str(project_root)],
            cwd=project_root,
            env=os.environ.copy(),
        )
        client = McpClient(mcp, timeout=min(arguments.timeout, 10.0))
        initialized = client.request(
            "initialize",
            {
                "protocolVersion": MCP_PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": "sprint2-live-smoke", "version": "1"},
            },
        )
        if initialized.get("result", {}).get("protocolVersion") != MCP_PROTOCOL:
            raise SmokeFailure("MCP protocol version differs")
        client.notify("notifications/initialized", {})
        listed = client.request("tools/list", {})
        tools = listed.get("result", {}).get("tools", [])
        if {tool.get("name") for tool in tools} != TOOL_NAMES:
            raise SmokeFailure("MCP tool set differs")
        if not all(tool.get("annotations", {}).get("readOnlyHint") is True for tool in tools):
            raise SmokeFailure("an MCP tool is not read-only")

        first = poll_observation(client, FIRST_VALUE, arguments.timeout)
        next_path.write_text("next\n", encoding="utf-8")
        wait_for_json(phase_path, 2, arguments.timeout)
        second = poll_observation(client, SECOND_VALUE, arguments.timeout)
        if second["event_seq"] <= first["event_seq"]:
            raise SmokeFailure("event_seq did not increase after the second edit")
        if second["scene_revision"] <= first["scene_revision"]:
            raise SmokeFailure("scene_revision did not increase after the second edit")
        if second["snapshot_id"] == first["snapshot_id"]:
            raise SmokeFailure("the second edit reused the previous snapshot generation")

        done_path.write_text("done\n", encoding="utf-8")
        try:
            editor.process.wait(timeout=10)
        except subprocess.TimeoutExpired as error:
            raise SmokeFailure("editor automation did not exit cleanly") from error
        if editor.process.returncode != 0:
            raise SmokeFailure("editor exited unsuccessfully")
        disk_after = scene_path.read_text(encoding="utf-8")
        if disk_after != disk_before:
            raise SmokeFailure("the live smoke unexpectedly changed main.tscn on disk")

        atomic_json(
            arguments.evidence,
            {
                "schema_version": "1.0",
                "status": "pass",
                "platform": platform_tag,
                "host": platform.platform(),
                "godot_version": godot_version,
                "sidecar_version": sidecar_version,
                "mcp_protocol": MCP_PROTOCOL,
                "tools": sorted(TOOL_NAMES),
                "disk_value": DISK_VALUE,
                "first": first,
                "second": second,
            },
        )
        print(f"PASS Sprint 2 live MCP smoke; evidence={arguments.evidence}")
        return 0
    finally:
        done_path.parent.mkdir(parents=True, exist_ok=True)
        done_path.write_text("done\n", encoding="utf-8")
        if mcp is not None:
            mcp.stop()
        editor.stop()
        for marker in (phase_path, next_path, done_path):
            marker.unlink(missing_ok=True)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SmokeFailure as error:
        print(f"FAIL Sprint 2 live MCP smoke: {error}", file=sys.stderr)
        raise SystemExit(1) from error
