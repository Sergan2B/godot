#!/usr/bin/env python3
"""Run the local model-free Sprint 4 scene-index and MCP acceptance gate."""

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
from typing import Any

from scene_graph_fixture import GOLDEN_PATH, MANIFEST_PATH, PHASES, PROJECT_SOURCE, strict_json_load
from sprint2_live_smoke import MCP_PROTOCOL, LineProcess, McpClient

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
DRIVER = SCRIPT_DIR / "resource_graph_live_driver.gd"
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint4_source_scopes.txt"
SOURCE_SCOPE_MANIFEST = SOURCE_SCOPE_PATH.relative_to(REPOSITORY_ROOT).as_posix()
SOURCE_SCOPES = tuple(
    line
    for line in SOURCE_SCOPE_PATH.read_text(encoding="utf-8").splitlines()
    if line and not line.startswith("#")
)
TOOL_NAMES = {
    "godot_find_resource_owners",
    "godot_get_current_scene",
    "godot_get_editor_state",
    "godot_get_resource_dependencies",
    "godot_get_scene_graph",
    "godot_get_selected_nodes",
    "godot_inspect_node",
}
MAIN_SCENE = "res://scenes/main.tscn"
TELEMETRY_PREFIX = "[codex_bridge_evidence] "
CI_ENVIRONMENT_MARKERS = (
    "APPVEYOR",
    "BUILDKITE",
    "CI",
    "CIRCLECI",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "JENKINS_URL",
    "TEAMCITY_VERSION",
    "TF_BUILD",
)


class SceneGateError(RuntimeError):
    """Raised when live scene semantics differ from the frozen contract."""


def canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode()


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return "sha256:" + digest.hexdigest()


def percentile(samples: list[float] | list[int], value: int) -> float | None:
    if not samples:
        return None
    ordered = sorted(samples)
    index = max(0, min(len(ordered) - 1, math.ceil(value * len(ordered) / 100) - 1))
    return round(float(ordered[index]), 3)


def metric(samples: list[float] | list[int]) -> dict[str, Any]:
    return {"samples": samples, "p50": percentile(samples, 50), "p95": percentile(samples, 95)}


def command_output(command: list[str], cwd: Path = REPOSITORY_ROOT) -> str:
    result = subprocess.run(command, cwd=cwd, check=False, capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        raise SceneGateError(f"command failed while binding evidence: {command[0]}")
    return result.stdout.strip()


def run_local_test_gates() -> None:
    workspace = REPOSITORY_ROOT / "godot-codex-mcp"
    commands = (
        (["cargo", "test", "--locked", "--workspace", "--all-targets"], workspace),
        (["cargo", "clippy", "--locked", "--workspace", "--all-targets", "--", "-D", "warnings"], workspace),
        ([sys.executable, str(SCRIPT_DIR / "scene_graph_fixture.py"), "validate"], REPOSITORY_ROOT),
    )
    for command, cwd in commands:
        result = subprocess.run(command, cwd=cwd, check=False, capture_output=True, text=True, timeout=300)
        if result.returncode != 0:
            raise SceneGateError(f"local gate failed: {command[0]} {command[1]}")


def target_platform() -> str:
    machine = platform.machine().lower()
    if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
        return "macos-arm64"
    if sys.platform == "win32" and machine in {"amd64", "x86_64"}:
        return "windows-x86_64"
    raise SceneGateError("Sprint 4 qualifying evidence requires macOS arm64 or Windows x86_64")


def default_godot(platform_tag: str) -> Path:
    name = (
        "godot.macos.editor.dev.arm64"
        if platform_tag == "macos-arm64"
        else "godot.windows.editor.dev.x86_64.console.exe"
    )
    return REPOSITORY_ROOT / "bin" / name


def default_sidecar() -> Path:
    name = "godot-codex-mcp.exe" if sys.platform == "win32" else "godot-codex-mcp"
    return REPOSITORY_ROOT / "godot-codex-mcp" / "target" / "release" / name


def default_evidence(platform_tag: str) -> Path:
    suffix = "macos" if platform_tag == "macos-arm64" else "windows"
    return SCRIPT_DIR / "evidence" / f"sprint-4-scene-graph-{suffix}.json"


def source_coordinates() -> dict[str, Any]:
    if not SOURCE_SCOPES or tuple(sorted(set(SOURCE_SCOPES))) != SOURCE_SCOPES:
        raise SceneGateError("Sprint 4 source scopes must be nonempty, unique, and sorted")
    if SOURCE_SCOPE_MANIFEST not in SOURCE_SCOPES:
        raise SceneGateError("Sprint 4 source scopes do not bind their own manifest")
    status = command_output(["git", "--literal-pathspecs", "status", "--porcelain", "--", *SOURCE_SCOPES])
    if status:
        raise SceneGateError("qualifying evidence requires a clean Sprint 4 source scope")
    listed = subprocess.run(
        ["git", "--literal-pathspecs", "ls-files", "-z", "--", *SOURCE_SCOPES],
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
    ).stdout.split(b"\0")
    paths = sorted(path.decode() for path in listed if path)
    if not paths:
        raise SceneGateError("Sprint 4 source scope is empty")
    digest = hashlib.sha256()
    for relative in paths:
        digest.update(relative.encode())
        digest.update(b"\0")
        digest.update((REPOSITORY_ROOT / relative).read_bytes())
        digest.update(b"\0")
    return {
        "git_commit": command_output(["git", "rev-parse", "HEAD"]),
        "relevant_source_sha256": "sha256:" + digest.hexdigest(),
        "relevant_source_clean": True,
        "tracked_file_count": len(paths),
        "scope_manifest": SOURCE_SCOPE_MANIFEST,
    }


def fixture_digest() -> str:
    digest = hashlib.sha256()
    for path in sorted(PROJECT_SOURCE.rglob("*")):
        if path.is_file() and ".godot" not in path.parts:
            digest.update(path.relative_to(PROJECT_SOURCE).as_posix().encode())
            digest.update(b"\0")
            digest.update(path.read_bytes())
            digest.update(b"\0")
    return "sha256:" + digest.hexdigest()


def require_local_host() -> None:
    detected = [name for name in CI_ENVIRONMENT_MARKERS if name in os.environ]
    if detected:
        raise SceneGateError("qualifying Sprint 4 evidence is local-only: " + ", ".join(detected))


def require_safe_mcp(value: Any, context: str) -> None:
    serialized = json.dumps(value, ensure_ascii=False)
    forbidden = ("/Users/", "/home/", "/tmp/", "\\\\.\\pipe\\", ".godot/imported", "session.token")
    if any(item in serialized for item in forbidden):
        raise SceneGateError(f"{context} leaked host or private runtime material")


def wait_for(path: Path, timeout: float, description: str) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.05)
    raise SceneGateError(f"timed out waiting for {description}")


def start_editor(godot: Path, project: Path, log_path: Path) -> tuple[subprocess.Popen[str], Any]:
    log = log_path.open("w", encoding="utf-8")
    environment = os.environ.copy()
    environment["GODOT_CODEX_EVIDENCE_TELEMETRY"] = "1"
    process = subprocess.Popen(
        [
            str(godot),
            "--editor",
            "--headless",
            "--path",
            str(project),
            "--script",
            str(DRIVER),
            "--",
            "--external-mutation",
            "--mutation-count=1",
        ],
        cwd=REPOSITORY_ROOT,
        env=environment,
        stdout=log,
        stderr=subprocess.STDOUT,
        text=True,
    )
    return process, log


def stop_editor(process: subprocess.Popen[str], project: Path) -> None:
    marker = project / ".godot" / "codex-resource-live-done"
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.touch()
    try:
        process.wait(timeout=20)
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    if process.returncode != 0:
        raise SceneGateError(f"Godot editor exited with status {process.returncode}")


def parse_telemetry(path: Path) -> dict[str, Any]:
    text = path.read_text(encoding="utf-8", errors="replace")
    if "SCRIPT ERROR" in text:
        raise SceneGateError("Godot live driver reported a script error")
    records = [
        json.loads(line.removeprefix(TELEMETRY_PREFIX))
        for line in text.splitlines()
        if line.startswith(TELEMETRY_PREFIX)
    ]
    if len(records) != 1:
        raise SceneGateError(f"Godot emitted {len(records)} Bridge telemetry records")
    record = records[0]
    samples = record.get("samples_usec")
    if (
        record.get("schema_version") != 1
        or record.get("budget_usec") != 2000
        or record.get("overflow") is not False
        or not isinstance(samples, list)
        or not samples
        or record.get("busy_frame_count") != len(samples)
        or record.get("over_budget_count") != sum(value > 2000 for value in samples)
        or record.get("max_elapsed_usec") != max(samples)
    ):
        raise SceneGateError("Bridge telemetry is incomplete or contradictory")
    return record


def initialize_sidecar(sidecar: Path, project: Path, timeout: float) -> tuple[LineProcess, McpClient]:
    process = LineProcess([str(sidecar), "--project-root", str(project)], cwd=project, env=os.environ.copy())
    client = McpClient(process, timeout=min(timeout, 15.0))
    response = client.request(
        "initialize",
        {
            "protocolVersion": MCP_PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "sprint4-scene-gate", "version": "1"},
        },
    )
    require_safe_mcp(response, "MCP initialize")
    if response.get("result", {}).get("protocolVersion") != MCP_PROTOCOL:
        process.stop()
        raise SceneGateError("MCP protocol version differs")
    client.notify("notifications/initialized", {})
    listed = client.request("tools/list", {})
    require_safe_mcp(listed, "MCP registry")
    tools = listed.get("result", {}).get("tools")
    if (
        not isinstance(tools, list)
        or {tool.get("name") for tool in tools} != TOOL_NAMES
        or any(
            tool.get("inputSchema", {}).get("type") != "object"
            or tool.get("inputSchema", {}).get("additionalProperties") is not False
            or tool.get("annotations", {}).get("readOnlyHint") is not True
            or tool.get("annotations", {}).get("destructiveHint") is not False
            or tool.get("annotations", {}).get("openWorldHint") is not False
            for tool in tools
        )
    ):
        process.stop()
        raise SceneGateError("MCP registry differs from the frozen seven-tool contract")
    return process, client


def close_sidecar(process: LineProcess) -> None:
    if process.process.stdin is not None and not process.process.stdin.closed:
        process.process.stdin.close()
    try:
        process.process.wait(timeout=20)
    except subprocess.TimeoutExpired:
        process.stop()
    if process.process.returncode not in (0, -15):
        raise SceneGateError(f"sidecar exited with status {process.process.returncode}")


def tool_call(client: McpClient, name: str, arguments: dict[str, Any]) -> tuple[dict[str, Any], bool, float]:
    started = time.perf_counter_ns()
    response = client.request("tools/call", {"name": name, "arguments": arguments})
    elapsed = round((time.perf_counter_ns() - started) / 1_000_000, 3)
    require_safe_mcp(response, name)
    if "error" in response:
        raise SceneGateError(f"{name} returned an MCP protocol error")
    result = response.get("result", {})
    content = result.get("structuredContent")
    if not isinstance(content, dict):
        raise SceneGateError(f"{name} omitted structuredContent")
    return content, result.get("isError") is True, elapsed


def require_tool_error(client: McpClient, name: str, arguments: dict[str, Any], code: str) -> None:
    content, is_error, _ = tool_call(client, name, arguments)
    if not is_error or content.get("error", {}).get("code") != code:
        raise SceneGateError(f"{name} did not reject a negative query as {code}")


def require_schema_rejection(client: McpClient, name: str, arguments: dict[str, Any]) -> None:
    response = client.request("tools/call", {"name": name, "arguments": arguments})
    require_safe_mcp(response, f"{name} schema rejection")
    result = response.get("result", {})
    text = result.get("content", [{}])[0].get("text", "") if isinstance(result, dict) else ""
    if "error" in response or result.get("isError") is not True or "unknown field `unknown_member`" not in text:
        raise SceneGateError(f"{name} did not enforce its closed input schema")


def wait_scene_current(
    client: McpClient,
    timeout: float,
    previous_revision: int | None = None,
    control_samples: list[float] | None = None,
) -> tuple[dict[str, Any], float]:
    started = time.monotonic()
    deadline = started + timeout
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        content, is_error, _ = tool_call(
            client,
            "godot_get_scene_graph",
            {"scene": MAIN_SCENE, "limit": 1},
        )
        last = content
        if (
            not is_error
            and content.get("freshness") == "current"
            and isinstance(content.get("scene_graph_revision"), int)
            and (previous_revision is None or content["scene_graph_revision"] > previous_revision)
        ):
            return content, round((time.monotonic() - started) * 1000, 3)
        if control_samples is not None:
            _, _, elapsed = tool_call(client, "godot_get_editor_state", {})
            control_samples.append(elapsed)
        time.sleep(0.05)
    raise SceneGateError(f"scene index did not become current: {last}")


def query_graph(client: McpClient, samples: list[float], scene: str = MAIN_SCENE) -> dict[str, Any]:
    nodes: list[dict[str, Any]] = []
    cursor: str | None = None
    envelope: dict[str, Any] | None = None
    while True:
        arguments: dict[str, Any] = {"scene": scene, "limit": 3}
        if cursor is not None:
            arguments["cursor"] = cursor
        content, is_error, elapsed = tool_call(client, "godot_get_scene_graph", arguments)
        samples.append(elapsed)
        if is_error:
            raise SceneGateError("scene graph query failed after the index became current")
        if envelope is None:
            envelope = content
        elif (
            content.get("generation_id") != envelope.get("generation_id")
            or content.get("scene_graph_revision") != envelope.get("scene_graph_revision")
            or content.get("project_context") != envelope.get("project_context")
        ):
            raise SceneGateError("scene pagination crossed an immutable generation")
        page = content.get("nodes")
        if not isinstance(page, list):
            raise SceneGateError("scene graph page omitted nodes")
        nodes.extend(page)
        cursor = content.get("next_cursor")
        if cursor is None:
            break
        if not isinstance(cursor, str):
            raise SceneGateError("scene graph returned a malformed cursor")
    if len({node.get("node_id") for node in nodes}) != len(nodes):
        raise SceneGateError("scene pagination duplicated node occurrences")
    assert envelope is not None
    envelope = dict(envelope)
    envelope["nodes"] = nodes
    envelope["next_cursor"] = None
    envelope["truncated"] = False
    return envelope


def inspect_node(
    client: McpClient,
    samples: list[float],
    node_path: str,
    scene: str = MAIN_SCENE,
) -> dict[str, Any]:
    properties: list[dict[str, Any]] = []
    cursor: str | None = None
    envelope: dict[str, Any] | None = None
    while True:
        arguments: dict[str, Any] = {"scene": scene, "node_path": node_path, "limit": 3}
        if cursor is not None:
            arguments["cursor"] = cursor
        content, is_error, elapsed = tool_call(client, "godot_inspect_node", arguments)
        samples.append(elapsed)
        if is_error:
            raise SceneGateError(f"node inspection failed: {scene}#{node_path}")
        if envelope is None:
            envelope = content
        elif content.get("generation_id") != envelope.get("generation_id"):
            raise SceneGateError("node inspection crossed an immutable generation")
        page = content.get("properties")
        if not isinstance(page, list):
            raise SceneGateError("node inspection omitted properties")
        properties.extend(page)
        cursor = content.get("next_cursor")
        if cursor is None:
            break
    assert envelope is not None
    envelope = dict(envelope)
    envelope["properties"] = properties
    envelope["next_cursor"] = None
    envelope["truncated"] = False
    return envelope


def normalized_context(values: list[dict[str, Any]]) -> list[dict[str, Any]]:
    output = []
    for fact in values:
        key = fact["key"]
        value = fact["value"]["value"]
        if key.startswith("autoload/") and isinstance(value, str):
            value = {"path": value.removeprefix("*"), "singleton": value.startswith("*")}
        elif key.startswith("input/") and isinstance(value, dict):
            value = {"deadzone": value.get("deadzone"), "event_count": len(value.get("events", []))}
        output.append({"key": key, "value": value, "type": fact["value"]["type"]})
    return sorted(output, key=lambda item: item["key"])


def without_runtime_coordinates(value: Any) -> Any:
    """Keep semantic payloads comparable across isolated host runs."""
    if isinstance(value, list):
        return [without_runtime_coordinates(item) for item in value]
    if isinstance(value, dict):
        return {
            key: without_runtime_coordinates(item)
            for key, item in value.items()
            if not key.endswith("_revision")
            and key
            not in {
                "editor_session_id",
                "generation_id",
                "project_id",
                "snapshot_checksum",
                "validated_checkpoint",
            }
        }
    return value


def normalized_inspection(value: dict[str, Any]) -> dict[str, Any]:
    return without_runtime_coordinates({
        "node": value["node"],
        "properties": value["properties"],
        "attached_script_resource_id": value["attached_script_resource_id"],
        "resources": value["resources"],
        "groups": value["groups"],
        "connections": value["connections"],
        "animation_references": value["animation_references"],
        "diagnostic_codes": sorted(
            diagnostic.get("code") for diagnostic in value["diagnostics"] if diagnostic.get("code")
        ),
    })


def normalized_semantics(graph: dict[str, Any], actor: dict[str, Any], animation: dict[str, Any]) -> dict[str, Any]:
    return without_runtime_coordinates({
        "scene": graph["scene"],
        "nodes": graph["nodes"],
        "project_context": normalized_context(graph["project_context"]),
        "diagnostic_codes": sorted(
            diagnostic.get("code") for diagnostic in graph["diagnostics"] if diagnostic.get("code")
        ),
        "actor_player": normalized_inspection(actor),
        "animation_player": normalized_inspection(animation),
    })


def replace_once(path: Path, before: str, after: str) -> None:
    text = path.read_text(encoding="utf-8")
    if text.count(before) != 1:
        raise SceneGateError(f"mutation anchor is not unique: {path.name}")
    path.write_text(text.replace(before, after), encoding="utf-8")


def apply_mutation(project: Path, phase: str) -> None:
    base = project / "scenes" / "base_actor.tscn"
    child = project / "scenes" / "child_scene.tscn"
    main = project / "scenes" / "main.tscn"
    if phase == "node_rename":
        replace_once(
            base,
            '[node name="Marker" type="Marker2D" parent="." unique_id=1003]\nposition = Vector2(32, 0)',
            '[node name="MarkerRenamed" type="Marker2D" parent="." unique_id=1003]\n'
            'position = Vector2(32, 0)\n\n'
            '[node name="MarkerCopy" type="Marker2D" parent="." unique_id=1004]\n'
            'position = Vector2(64, 0)',
        )
        replace_once(base, 'target = NodePath("../Marker")', 'target = NodePath("../MarkerRenamed")')
    elif phase == "node_reparent":
        replace_once(base, 'target = NodePath("../Marker")', 'target = NodePath("Marker")')
        replace_once(
            base,
            '[node name="Marker" type="Marker2D" parent="." unique_id=1003]',
            '[node name="Marker" type="Marker2D" parent="Player" unique_id=1003]',
        )
    elif phase == "property_override":
        replace_once(main, "speed = 30", "speed = 45")
    elif phase == "instance_mutation":
        with child.open("a", encoding="utf-8") as target:
            target.write('\n[node name="NestedActor2" parent="." unique_id=4004 instance=ExtResource("1_base")]\n')
    elif phase == "signal_group":
        replace_once(base, 'groups=["actors"]', 'groups=["actors_mutated"]')
        replace_once(base, 'method="_on_player_ready"', 'method="_on_player_entered"')
    elif phase == "animation_fix":
        replace_once(main, 'NodePath("MissingNode:position")', 'NodePath("Actor/Player:position")')
    elif phase == "journal_gap":
        (project / "scenes" / "gap_scene.tscn").write_text(
            '[gd_scene format=3]\n\n[node name="GapScene" type="Node" unique_id=9001]\n',
            encoding="utf-8",
        )
    else:
        raise SceneGateError(f"unsupported mutation phase: {phase}")
    marker = project / ".godot" / "codex-resource-live-mutate"
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.touch()


def expected_ids() -> dict[str, str]:
    golden = strict_json_load(GOLDEN_PATH)
    result = {record["oracle_id"]: record["entity_id"] for record in golden["nodes"]}
    result.update({record["oracle_id"]: record["entity_id"] for record in golden["occurrences"]})
    return result


def property_by_name(inspection: dict[str, Any], name: str) -> dict[str, Any]:
    matches = [property_value for property_value in inspection["properties"] if property_value["name"] == name]
    if len(matches) != 1:
        raise SceneGateError(f"expected one effective property: {name}")
    return matches[0]


def resource_types(inspection: dict[str, Any]) -> set[str]:
    return {
        relation.get("attributes", {}).get("resource_type")
        for relation in inspection["resources"]
        if relation.get("kind") == "subresource"
    }


def verify_order_equivalence(client: McpClient, samples: list[float]) -> bool:
    observed = []
    for scene in ("res://scenes/order_a.tscn", "res://scenes/order_b.tscn"):
        value = inspect_node(client, samples, ".", scene)
        observed.append(
            {
                "properties": sorted(
                    (
                        item["name"],
                        item["value"]["type"],
                        item["value"]["value"].get("godot_type"),
                        item["value"]["value"].get("scene_unique_id"),
                    )
                    for item in value["properties"]
                ),
                "subresources": sorted(
                    (
                        item["attributes"]["resource_type"],
                        item["attributes"]["scene_unique_id"],
                        item["attributes"]["ownership_paths"],
                    )
                    for item in value["resources"]
                    if item["kind"] == "subresource"
                ),
            }
        )
    if observed[0] != observed[1]:
        raise SceneGateError("order-equivalent scenes produced different semantics")
    return True


def verify_phase(
    phase: str,
    graph: dict[str, Any],
    actor: dict[str, Any],
    animation: dict[str, Any],
    ids: dict[str, str],
) -> dict[str, bool]:
    by_path = {node["node_path"]: node for node in graph["nodes"]}
    required_base = {
        ".": ids["main_root_occ"],
        "Actor": ids["actor_root_occ"],
        "Actor/Player": ids["actor_player_occ"],
        "Child": ids["child_root_occ"],
        "Child/NestedActor": ids["nested_actor_root_occ"],
        "Child/NestedActor/Player": ids["nested_actor_player_occ"],
        "AnimationPlayer": ids["animation_occ"],
    }
    if any(by_path.get(path, {}).get("node_id") != entity_id for path, entity_id in required_base.items()):
        raise SceneGateError(f"{phase} changed a canonical unaffected occurrence identity")
    assertions = {
        "canonical_occurrence_ids": True,
        "project_context": {item["key"] for item in graph["project_context"]}
        == {
            "application/run/main_scene",
            "autoload/SceneService",
            "input/jump",
            "layer_names/2d_physics/1",
            "layer_names/2d_render/1",
            "layer_names/3d_physics/1",
            "layer_names/3d_render/1",
            "layer_names/navigation/1",
        },
    }
    speed = property_by_name(actor, "speed")
    if phase == "property_override":
        assertions["property_override"] = speed["value"]["value"] == 45 and speed["origin"] == "instance_override"
    else:
        assertions["property_override"] = speed["value"]["value"] == 30 and speed["origin"] == "instance_override"
    if phase == "node_rename":
        renamed = by_path.get("Actor/MarkerRenamed", {})
        duplicate = by_path.get("Actor/MarkerCopy", {})
        assertions["rename_identity_preserved"] = renamed.get("definition", {}).get("node_id") == ids["base_marker"]
        assertions["duplicate_identity_distinct"] = (
            isinstance(duplicate.get("definition", {}).get("node_id"), str)
            and duplicate["definition"]["node_id"] != ids["base_marker"]
        )
    elif phase == "node_reparent":
        marker = by_path.get("Actor/Player/Marker", {})
        assertions["reparent_identity_preserved"] = (
            marker.get("definition", {}).get("node_id") == ids["base_marker"]
            and marker.get("parent_node_id") == ids["actor_player_occ"]
        )
    elif phase == "instance_mutation":
        assertions["instance_closure_advanced"] = all(
            path in by_path
            for path in ("Child/NestedActor2", "Child/NestedActor2/Player", "Child/NestedActor2/Marker")
        )
    elif phase == "signal_group":
        assertions["signal_group_atomic"] = (
            actor["groups"][0]["group"] == "actors_mutated"
            and actor["connections"][0]["method"] == "_on_player_entered"
            and actor["connections"][0]["flags"] == 2
        )
    elif phase == "animation_fix":
        assertions["animation_paths_resolved"] = (
            len(animation["animation_references"]) == 2
            and {item["resolution"] for item in animation["animation_references"]} == {"resolved"}
            and "broken_animation_node_path" not in graph["partial_reasons"]
        )
    elif phase == "journal_gap":
        assertions["gap_scene_recovered"] = graph["freshness"] == "current" and graph["validated_checkpoint"]["source_complete"] is True
    if phase == "base":
        assertions.update(
            {
                "inheritance_and_instances": by_path["Actor/Player"]["origin_scene_id"]
                != graph["scene"]["scene_id"],
                "attached_script": isinstance(actor["attached_script_resource_id"], str),
                "external_and_nested_subresources": {"GradientTexture1D", "Gradient"}.issubset(
                    resource_types(actor)
                )
                and {"AnimationLibrary", "Animation"}.issubset(resource_types(animation)),
                "persistent_subresource_identity": all(
                    relation["source"].startswith("godot:subresource:scene-id:v1:")
                    for inspection in (actor, animation)
                    for relation in inspection["resources"]
                    if relation["kind"] == "subresource"
                ),
                "signal_and_group": actor["groups"][0]["group"] == "actors"
                and actor["connections"][0]["method"] == "_on_player_ready"
                and actor["connections"][0]["flags"] == 2,
                "animation_resolution": {item["resolution"] for item in animation["animation_references"]}
                == {"resolved", "broken"},
            }
        )
    if not all(assertions.values()):
        failed = ", ".join(sorted(name for name, passed in assertions.items() if not passed))
        raise SceneGateError(f"{phase} semantic assertions failed: {failed}")
    return assertions


def probe_contract(client: McpClient) -> tuple[dict[str, bool], str]:
    require_tool_error(client, "godot_get_scene_graph", {"scene": MAIN_SCENE, "limit": 0}, "invalid_limit")
    require_tool_error(client, "godot_get_scene_graph", {"scene": "res://../main.tscn"}, "invalid_path")
    require_tool_error(
        client,
        "godot_inspect_node",
        {"scene": MAIN_SCENE, "node_path": "../Actor", "limit": 50},
        "invalid_query",
    )
    require_schema_rejection(
        client,
        "godot_get_scene_graph",
        {"scene": MAIN_SCENE, "limit": 1, "unknown_member": True},
    )
    require_schema_rejection(
        client,
        "godot_inspect_node",
        {"scene": MAIN_SCENE, "node_path": ".", "unknown_member": True},
    )
    first, is_error, _ = tool_call(
        client,
        "godot_get_scene_graph",
        {"scene": MAIN_SCENE, "limit": 2},
    )
    cursor = first.get("next_cursor")
    if is_error or not isinstance(cursor, str):
        raise SceneGateError("scene tool did not issue a pagination cursor")
    replacement = "A" if cursor[-1] != "A" else "B"
    require_tool_error(
        client,
        "godot_get_scene_graph",
        {"scene": MAIN_SCENE, "limit": 2, "cursor": cursor[:-1] + replacement},
        "stale_cursor",
    )
    require_tool_error(
        client,
        "godot_get_scene_graph",
        {"scene": "uid://s4main", "limit": 2, "cursor": cursor},
        "stale_cursor",
    )
    require_tool_error(
        client,
        "godot_inspect_node",
        {"node_id": ids_for_contract()["actor_player_occ"], "limit": 2, "cursor": cursor},
        "stale_cursor",
    )
    return (
        {
            "closed_input_schemas": True,
            "cursor_tampering_rejected": True,
            "cross_selector_cursor_rejected": True,
            "cross_tool_cursor_rejected": True,
            "exact_tool_registry": True,
            "limit_bounds_rejected": True,
            "strict_selector_forms": True,
            "traversal_rejected": True,
        },
        cursor,
    )


def ids_for_contract() -> dict[str, str]:
    return expected_ids()


def run_phase(
    phase: str,
    godot: Path,
    sidecar: Path,
    run_root: Path,
    timeout: float,
    ids: dict[str, str],
    contract: dict[str, bool],
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    phase_root = run_root / f"p{PHASES.index(phase)}"
    project = phase_root / "p"
    phase_root.mkdir(parents=True)
    shutil.copytree(PROJECT_SOURCE, project, ignore=shutil.ignore_patterns(".godot"))
    logs: list[Path] = []
    telemetry: list[dict[str, Any]] = []
    query_samples: list[float] = []
    control_samples: list[float] = []
    change_visibility: float | None = None
    editor: subprocess.Popen[str] | None = None
    editor_log: Any = None
    sidecar_process: LineProcess | None = None
    phase_result: dict[str, Any] = {}
    try:
        log_path = phase_root / "godot.log"
        logs.append(log_path)
        editor, editor_log = start_editor(godot, project, log_path)
        wait_for(project / ".godot" / "codex" / "bridge.json", timeout, f"{phase} discovery")
        sidecar_process, client = initialize_sidecar(sidecar, project, timeout)
        current, startup_ms = wait_scene_current(client, timeout)
        base_revision = int(current["scene_graph_revision"])
        stale_cursor: str | None = None
        if phase == "base":
            probed, stale_cursor = probe_contract(client)
            contract.update(probed)
        elif phase == "node_rename":
            first, is_error, _ = tool_call(
                client,
                "godot_get_scene_graph",
                {"scene": MAIN_SCENE, "limit": 2},
            )
            stale_cursor = first.get("next_cursor")
            if is_error or not isinstance(stale_cursor, str):
                raise SceneGateError("could not issue a pre-mutation cursor")

        graph = query_graph(client, query_samples)
        actor = inspect_node(client, query_samples, "Actor/Player")
        animation = inspect_node(client, query_samples, "AnimationPlayer")
        initial_semantics = normalized_semantics(graph, actor, animation)

        if phase == "base":
            assertions = verify_phase(phase, graph, actor, animation, ids)
            assertions["subresource_order_equivalence"] = verify_order_equivalence(client, query_samples)
            close_sidecar(sidecar_process)
            sidecar_process = None
            stop_editor(editor, project)
            editor_log.close()
            editor = None
            editor_log = None
            (project / ".godot" / "codex-resource-live-done").unlink(missing_ok=True)

            reopen_log = phase_root / "godot-reopen.log"
            logs.append(reopen_log)
            editor, editor_log = start_editor(godot, project, reopen_log)
            wait_for(project / ".godot" / "codex" / "bridge.json", timeout, "base reopen discovery")
            sidecar_process, client = initialize_sidecar(sidecar, project, timeout)
            _, reopen_ms = wait_scene_current(client, timeout)
            graph = query_graph(client, query_samples)
            actor = inspect_node(client, query_samples, "Actor/Player")
            animation = inspect_node(client, query_samples, "AnimationPlayer")
            reopened_semantics = normalized_semantics(graph, actor, animation)
            if reopened_semantics != initial_semantics:
                raise SceneGateError("save/reopen changed canonical scene semantics")
            assertions["save_reopen_identity"] = True
            phase_result = {
                "phase": phase,
                "startup_visibility_ms": startup_ms,
                "reopen_visibility_ms": reopen_ms,
                "scene_graph_revision": graph["scene_graph_revision"],
                "index_revision": graph["index_revision"],
                "node_count": len(graph["nodes"]),
                "normalized_scene_sha256": sha256_bytes(canonical_bytes(reopened_semantics)),
                "assertions": assertions,
                "cached_scene_query_samples_ms": query_samples,
                "bulk_status_ping_samples_ms": control_samples,
                "change_visibility_ms": None,
                "passed": True,
            }
        else:
            apply_mutation(project, phase)
            mutation_started = time.monotonic()
            if phase == "journal_gap":
                for _ in range(5):
                    _, _, elapsed = tool_call(client, "godot_get_editor_state", {})
                    control_samples.append(elapsed)
                    time.sleep(0.02)
            final, _ = wait_scene_current(
                client,
                timeout,
                base_revision,
                control_samples if phase == "journal_gap" else None,
            )
            change_visibility = round((time.monotonic() - mutation_started) * 1000, 3)
            if phase == "node_rename" and stale_cursor is not None:
                require_tool_error(
                    client,
                    "godot_get_scene_graph",
                    {"scene": MAIN_SCENE, "limit": 2, "cursor": stale_cursor},
                    "stale_cursor",
                )
                contract["stale_generation_cursor_rejected"] = True
            graph = query_graph(client, query_samples)
            actor = inspect_node(client, query_samples, "Actor/Player")
            animation = inspect_node(client, query_samples, "AnimationPlayer")
            semantics = normalized_semantics(graph, actor, animation)
            assertions = verify_phase(phase, graph, actor, animation, ids)
            phase_result = {
                "phase": phase,
                "startup_visibility_ms": startup_ms,
                "reopen_visibility_ms": None,
                "scene_graph_revision": final["scene_graph_revision"],
                "index_revision": final["index_revision"],
                "node_count": len(graph["nodes"]),
                "normalized_scene_sha256": sha256_bytes(canonical_bytes(semantics)),
                "assertions": assertions,
                "cached_scene_query_samples_ms": query_samples,
                "bulk_status_ping_samples_ms": control_samples,
                "change_visibility_ms": change_visibility,
                "passed": True,
            }
        return phase_result, telemetry
    finally:
        if sidecar_process is not None:
            close_sidecar(sidecar_process)
        if editor is not None:
            stop_editor(editor, project)
        if editor_log is not None:
            editor_log.close()
        active_error = sys.exc_info()[0] is not None
        for log_path in logs:
            if not log_path.exists():
                continue
            try:
                telemetry.append(parse_telemetry(log_path))
            except SceneGateError:
                if not active_error:
                    raise
        runtime = project / ".godot" / "codex"
        runtime_leaks = []
        if runtime.exists():
            runtime_leaks = [
                path
                for path in runtime.iterdir()
                if path.name in {"bridge.json", "session.token", "bridge.lock"}
                or path.suffix in {".sock", ".pipe"}
            ]
        if runtime_leaks:
            raise SceneGateError(f"{phase} left Bridge runtime artifacts")
        phase_result["bridge_main_thread_sessions"] = telemetry


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", type=Path)
    parser.add_argument("--sidecar", type=Path)
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--timeout", type=float, default=90.0)
    arguments = parser.parse_args()
    run_root: Path | None = None
    try:
        require_local_host()
        platform_tag = target_platform()
        godot = (arguments.godot or default_godot(platform_tag)).resolve()
        sidecar = (arguments.sidecar or default_sidecar()).resolve()
        evidence = (arguments.evidence or default_evidence(platform_tag)).resolve()
        if not godot.is_file() or not sidecar.is_file():
            raise SceneGateError("Godot editor and release sidecar must be built before the live gate")
        source = source_coordinates()
        fixture_before = fixture_digest()
        run_local_test_gates()
        temporary_parent = "/tmp" if sys.platform == "darwin" else None
        run_root = Path(tempfile.mkdtemp(prefix="s4.", dir=temporary_parent))
        ids = expected_ids()
        contract: dict[str, bool] = {"stale_generation_cursor_rejected": False}
        phases = []
        all_telemetry: list[dict[str, Any]] = []
        for phase in PHASES:
            result, telemetry = run_phase(
                phase,
                godot,
                sidecar,
                run_root,
                arguments.timeout,
                ids,
                contract,
            )
            phases.append(result)
            all_telemetry.extend(telemetry)
        if not all(contract.values()):
            raise SceneGateError("the complete live MCP contract was not exercised")

        cached = [sample for phase in phases for sample in phase["cached_scene_query_samples_ms"]]
        ordinary_visibility = [
            phase["change_visibility_ms"]
            for phase in phases
            if phase["phase"] not in {"base", "journal_gap"}
        ]
        control = [sample for phase in phases for sample in phase["bulk_status_ping_samples_ms"]]
        bridge = [sample for record in all_telemetry for sample in record["samples_usec"]]
        performance = {
            "cached_scene_query_ms": metric(cached),
            "ordinary_change_visibility_ms": metric(ordinary_visibility),
            "bulk_status_ping_ms": metric(control),
            "bridge_main_thread_usec": metric(bridge),
            "gates": {
                "cached_scene_query_p95_lte_300ms": percentile(cached, 95) <= 300,
                "ordinary_change_visibility_p95_lte_2000ms": percentile(ordinary_visibility, 95) <= 2000,
                "bulk_status_ping_p95_lte_200ms": percentile(control, 95) <= 200,
                "bridge_main_thread_zero_over_2000us": all(sample <= 2000 for sample in bridge),
            },
        }
        if not all(performance["gates"].values()):
            raise SceneGateError("one or more Sprint 4 SLOs failed")

        rust_verbose = command_output(["rustc", "--version", "--verbose"], REPOSITORY_ROOT / "godot-codex-mcp")
        temporary_path = str(run_root)
        shutil.rmtree(run_root)
        run_root = None
        fixture_after = fixture_digest()
        if fixture_after != fixture_before:
            raise SceneGateError("live gate mutated the committed scene fixture")
        report = {
            "schema_version": 1,
            "sprint": 4,
            "profile": "qualifying",
            "platform": platform_tag,
            "source": {
                **source,
                "fixture_sha256": fixture_before,
                "fixture_manifest_sha256": sha256_file(MANIFEST_PATH),
                "golden_scene_graph_sha256": sha256_file(GOLDEN_PATH),
            },
            "artifacts": {
                "godot_sha256": sha256_file(godot),
                "sidecar_sha256": sha256_file(sidecar),
            },
            "versions": {
                "godot": command_output([str(godot), "--version"]),
                "sidecar": command_output([str(sidecar), "--version"]),
                "python": f"{platform.python_implementation()} {platform.python_version()}",
                "rustc": rust_verbose.splitlines()[0],
                "cargo": command_output(["cargo", "--version"], REPOSITORY_ROOT / "godot-codex-mcp"),
                "rust_target": next(
                    line.removeprefix("host: ") for line in rust_verbose.splitlines() if line.startswith("host: ")
                ),
                "bridge_rpc": "1.3",
                "mcp_protocol": MCP_PROTOCOL,
                "logical_schema": "1.2",
                "physical_store": "segment-v2",
            },
            "mcp_contract": contract,
            "phases": phases,
            "performance": performance,
            "completion": {
                "requested_phases": list(PHASES),
                "completed_phases": [phase["phase"] for phase in phases],
                "all_phases_completed": True,
                "scene_mcp_contract_verified": True,
                "fixture_integrity_preserved": fixture_after == fixture_before,
                "rust_workspace_tests_passed": True,
                "rust_clippy_passed": True,
                "scene_contract_tests_passed": True,
            },
            "cleanup": {
                "editor_processes_stopped": True,
                "sidecar_processes_stopped": True,
                "bridge_runtime_files_absent": True,
                "temporary_workspace_removed": True,
            },
            "redaction": {
                "absolute_paths_absent": True,
                "bridge_endpoints_absent": True,
                "secret_material_absent": True,
                "source_bytes_absent": True,
                "session_identifiers_absent": True,
            },
            "remote_ci": "not_run",
            "status": "passed",
        }
        encoded = json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
        for forbidden in (temporary_path, str(REPOSITORY_ROOT), "session.token", "editor:"):
            if forbidden in encoded:
                raise SceneGateError("final evidence failed redaction")
        evidence.parent.mkdir(parents=True, exist_ok=True)
        evidence.write_text(encoded, encoding="utf-8")
        print(encoded, end="")
        return 0
    except (SceneGateError, OSError, subprocess.SubprocessError, json.JSONDecodeError) as error:
        print(f"Sprint 4 scene gate failed: {error}", file=sys.stderr)
        return 1
    finally:
        if run_root is not None:
            shutil.rmtree(run_root, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
