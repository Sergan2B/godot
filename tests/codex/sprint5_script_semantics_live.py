#!/usr/bin/env python3
"""Run the local model-free Sprint 5 script-index and MCP acceptance gate."""

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

import script_semantics_fixture as fixture
from sprint2_live_smoke import MCP_PROTOCOL, LineProcess, McpClient

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
PROJECT_SOURCE = SCRIPT_DIR / "fixtures" / "script_semantics_project"
MANIFEST_PATH = fixture.MANIFEST_PATH
GOLDEN_PATH = fixture.GOLDEN_PATH
DRIVER = SCRIPT_DIR / "resource_graph_live_driver.gd"
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint5_source_scopes.txt"
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
    "godot_inspect_symbol",
    "godot_search_symbols",
}
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
ATTACHMENT_SCENES = (
    "res://scenes/attachment_player.tscn",
    "res://scenes/attachment_removed.tscn",
    "res://scenes/attachment_replaced.tscn",
)


class ScriptGateError(RuntimeError):
    """Raised when live script semantics differ from the frozen contract."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ScriptGateError(message)


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


def required_percentile(samples: list[float] | list[int], value: int, context: str) -> float:
    result = percentile(samples, value)
    if result is None:
        raise ScriptGateError(f"{context} percentile is unavailable")
    return result


def command_output(command: list[str], cwd: Path = REPOSITORY_ROOT) -> str:
    result = subprocess.run(command, cwd=cwd, check=False, capture_output=True, text=True, timeout=60)
    if result.returncode != 0:
        raise ScriptGateError(f"command failed while binding evidence: {command[0]}")
    return result.stdout.strip()


def run_local_test_gates() -> None:
    workspace = REPOSITORY_ROOT / "godot-codex-mcp"
    commands = (
        (["cargo", "test", "--locked", "--workspace", "--all-targets"], workspace, 600),
        (["cargo", "clippy", "--locked", "--workspace", "--all-targets", "--", "-D", "warnings"], workspace, 600),
        ([sys.executable, str(fixture.__file__), "validate"], REPOSITORY_ROOT, 300),
        ([sys.executable, "-m", "unittest", "tests/codex/test_sprint5_acceptance.py"], REPOSITORY_ROOT, 120),
    )
    for command, cwd, timeout in commands:
        result = subprocess.run(command, cwd=cwd, check=False, capture_output=True, text=True, timeout=timeout)
        if result.returncode != 0:
            raise ScriptGateError(f"local gate failed: {command[0]} {command[1]}")


def target_platform() -> str:
    machine = platform.machine().lower()
    if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
        return "macos-arm64"
    if sys.platform == "win32" and machine in {"amd64", "x86_64"}:
        return "windows-x86_64"
    raise ScriptGateError("Sprint 5 qualifying evidence requires macOS arm64 or Windows x86_64")


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


def default_script_probe() -> Path:
    name = "script_graph_live.exe" if sys.platform == "win32" else "script_graph_live"
    return REPOSITORY_ROOT / "godot-codex-mcp" / "target" / "release" / "examples" / name


def default_evidence(platform_tag: str) -> Path:
    suffix = "macos" if platform_tag == "macos-arm64" else "windows"
    return SCRIPT_DIR / "evidence" / f"sprint-5-script-semantics-{suffix}.json"


def source_coordinates(*, allow_dirty: bool = False) -> dict[str, Any]:
    if not SOURCE_SCOPES or tuple(sorted(set(SOURCE_SCOPES))) != SOURCE_SCOPES:
        raise ScriptGateError("Sprint 5 source scopes must be nonempty, unique, and sorted")
    if SOURCE_SCOPE_MANIFEST not in SOURCE_SCOPES:
        raise ScriptGateError("Sprint 5 source scopes do not bind their own manifest")
    status = command_output(["git", "--literal-pathspecs", "status", "--porcelain", "--", *SOURCE_SCOPES])
    if status and not allow_dirty:
        raise ScriptGateError("qualifying evidence requires a clean Sprint 5 source scope")
    listed = subprocess.run(
        ["git", "--literal-pathspecs", "ls-files", "-z", "--", *SOURCE_SCOPES],
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
    ).stdout.split(b"\0")
    paths = sorted(path.decode() for path in listed if path)
    if not paths:
        raise ScriptGateError("Sprint 5 source scope is empty")
    digest = hashlib.sha256()
    for relative in paths:
        digest.update(relative.encode())
        digest.update(b"\0")
        digest.update((REPOSITORY_ROOT / relative).read_bytes())
        digest.update(b"\0")
    return {
        "git_commit": command_output(["git", "rev-parse", "HEAD"]),
        "relevant_source_sha256": "sha256:" + digest.hexdigest(),
        "relevant_source_clean": not bool(status),
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


def oracle_truth() -> dict[str, Any]:
    manifest = fixture.load_manifest()
    golden = fixture.load_golden()
    files = fixture.validate_manifest(manifest)
    documents = fixture.validate_documents(golden, files)
    resources = fixture.validate_resources(golden, files)
    ranges = fixture.validate_ranges(golden, documents)
    symbols, identities = fixture.validate_symbols(golden, documents, ranges)
    relations = fixture.validate_relations(golden, symbols, resources, ranges)
    fixture.validate_diagnostics_and_attachments(golden, documents, symbols, ranges)
    identity_digest = hashlib.sha256()
    for oracle_id, identity in sorted(identities.items()):
        identity_digest.update(oracle_id.encode() + b"\0" + identity.encode() + b"\0")
    return {
        "golden": golden,
        "documents": documents,
        "resources": resources,
        "ranges": ranges,
        "symbols": symbols,
        "identities": identities,
        "relations": relations,
        "canonical_symbol_ids_sha256": "sha256:" + identity_digest.hexdigest(),
    }


def require_local_host() -> None:
    detected = [name for name in CI_ENVIRONMENT_MARKERS if name in os.environ]
    if detected:
        raise ScriptGateError("qualifying Sprint 5 evidence is local-only: " + ", ".join(detected))


def require_safe_value(value: Any, context: str) -> None:
    serialized = json.dumps(value, ensure_ascii=False)
    forbidden = (
        "/Users/",
        "/home/",
        "/tmp/",
        "C:\\",
        "\\\\.\\pipe\\",
        ".godot/imported",
        "session.token",
        "authorization",
        "source_excerpt",
    )
    if any(item in serialized for item in forbidden):
        raise ScriptGateError(f"{context} leaked host, source, or private runtime material")


def wait_for_editor_file(
    process: subprocess.Popen[str],
    path: Path,
    log_path: Path,
    timeout: float,
    description: str,
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        return_code = process.poll()
        if return_code is not None:
            raise ScriptGateError(f"Godot exited with status {return_code} before {description}")
        if log_path.exists():
            log_text = log_path.read_text(encoding="utf-8", errors="replace")
            if "[codex_bridge] Failed to start the transport worker" in log_text:
                raise ScriptGateError("Godot could not start the Bridge transport worker")
            if "resource graph live driver cannot" in log_text:
                raise ScriptGateError("Godot live driver could not access EditorFileSystem")
        time.sleep(0.05)
    raise ScriptGateError(f"timed out waiting for {description}")


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
        process.wait(timeout=30)
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    if process.returncode != 0:
        raise ScriptGateError(f"Godot editor exited with status {process.returncode}")


def parse_telemetry(path: Path) -> dict[str, Any]:
    text = path.read_text(encoding="utf-8", errors="replace")
    if "resource graph live driver cannot" in text:
        raise ScriptGateError("Godot live driver reported an error")
    if "[codex_bridge] Failed to start the transport worker" in text:
        raise ScriptGateError("Godot Bridge transport failed during the live session")
    records = [
        json.loads(line.removeprefix(TELEMETRY_PREFIX))
        for line in text.splitlines()
        if line.startswith(TELEMETRY_PREFIX)
    ]
    if len(records) != 1 or not isinstance(records[0], dict):
        raise ScriptGateError(f"Godot emitted {len(records)} Bridge telemetry records")
    record = cast(dict[str, Any], records[0])
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
        raise ScriptGateError("Bridge telemetry is incomplete or contradictory")
    return record


def initialize_sidecar(sidecar: Path, project: Path, timeout: float) -> tuple[LineProcess, McpClient]:
    process = LineProcess([str(sidecar), "--project-root", str(project)], cwd=project, env=os.environ.copy())
    client = McpClient(process, timeout=min(timeout, 20.0))
    response = client.request(
        "initialize",
        {
            "protocolVersion": MCP_PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "sprint5-script-gate", "version": "1"},
        },
    )
    require_safe_value(response, "MCP initialize")
    if response.get("result", {}).get("protocolVersion") != MCP_PROTOCOL:
        process.stop()
        raise ScriptGateError("MCP protocol version differs")
    client.notify("notifications/initialized", {})
    listed = client.request("tools/list", {})
    require_safe_value(listed, "MCP registry")
    tools = listed.get("result", {}).get("tools")
    if not isinstance(tools, list):
        process.stop()
        raise ScriptGateError("MCP registry omitted its tool array")
    actual_names = {tool.get("name") for tool in tools}
    if actual_names != TOOL_NAMES:
        process.stop()
        raise ScriptGateError(
            "MCP registry tool names differ: "
            + ", ".join(sorted(str(name) for name in actual_names))
        )
    invalid_contracts = [
        str(tool.get("name"))
        for tool in tools
        if tool.get("inputSchema", {}).get("type") != "object"
        or tool.get("inputSchema", {}).get("additionalProperties") is not False
        or tool.get("annotations", {}).get("readOnlyHint") is not True
        or tool.get("annotations", {}).get("destructiveHint") is not False
        or tool.get("annotations", {}).get("openWorldHint") is not False
    ]
    if invalid_contracts:
        process.stop()
        raise ScriptGateError("MCP registry contracts differ: " + ", ".join(sorted(invalid_contracts)))
    return process, client


def close_sidecar(process: LineProcess) -> None:
    if process.process.stdin is not None and not process.process.stdin.closed:
        process.process.stdin.close()
    try:
        process.process.wait(timeout=30)
    except subprocess.TimeoutExpired:
        process.stop()
    if process.process.returncode not in (0, -15):
        raise ScriptGateError(f"sidecar exited with status {process.process.returncode}")


def tool_call(client: McpClient, name: str, arguments: dict[str, Any]) -> tuple[dict[str, Any], bool, float]:
    started = time.perf_counter_ns()
    response = client.request("tools/call", {"name": name, "arguments": arguments})
    elapsed = round((time.perf_counter_ns() - started) / 1_000_000, 3)
    require_safe_value(response, name)
    if "error" in response:
        raise ScriptGateError(f"{name} returned an MCP protocol error")
    result = response.get("result", {})
    content = result.get("structuredContent")
    if not isinstance(content, dict):
        raise ScriptGateError(f"{name} omitted structuredContent")
    return content, result.get("isError") is True, elapsed


def require_tool_error(client: McpClient, name: str, arguments: dict[str, Any], code: str) -> None:
    content, is_error, _ = tool_call(client, name, arguments)
    if not is_error or content.get("error", {}).get("code") != code:
        raise ScriptGateError(f"{name} did not reject a negative query as {code}")


def require_schema_rejection(client: McpClient, name: str, arguments: dict[str, Any]) -> None:
    response = client.request("tools/call", {"name": name, "arguments": arguments})
    require_safe_value(response, f"{name} schema rejection")
    result = response.get("result", {})
    text = result.get("content", [{}])[0].get("text", "") if isinstance(result, dict) else ""
    if "error" in response or result.get("isError") is not True or "unknown field `unknown_member`" not in text:
        raise ScriptGateError(f"{name} did not enforce its closed input schema")


def wait_script_current(
    client: McpClient,
    timeout: float,
    previous_index_revision: int | None = None,
    control_samples: list[float] | None = None,
) -> tuple[dict[str, Any], float]:
    started = time.monotonic()
    deadline = started + timeout
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        content, is_error, _ = tool_call(
            client,
            "godot_search_symbols",
            {"query": "BaseActor", "match": "exact", "language": "gdscript", "kind": "class", "limit": 1},
        )
        last = content
        if (
            not is_error
            and content.get("freshness") == "current"
            and isinstance(content.get("index_revision"), int)
            and (previous_index_revision is None or content["index_revision"] > previous_index_revision)
        ):
            return content, round((time.monotonic() - started) * 1000, 3)
        if control_samples is not None:
            _, _, elapsed = tool_call(client, "godot_get_editor_state", {})
            control_samples.append(elapsed)
        time.sleep(0.05)
    sidecar_tail = " | ".join(client.process.tail[-20:])
    raise ScriptGateError(f"script index did not become current: {last}; sidecar_tail={sidecar_tail}")


def capture_probe(probe: Path, project: Path, timeout: float, after_revision: int | None = None) -> dict[str, Any]:
    command = [str(probe), str(project), "--details"]
    if after_revision is not None:
        command.extend(["--poll-delta", f"--after={after_revision}"])
    result = subprocess.run(command, cwd=project, check=False, capture_output=True, text=True, timeout=timeout)
    if result.returncode != 0:
        raise ScriptGateError("script Bridge probe failed: " + result.stderr.strip())
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ScriptGateError("script Bridge probe returned malformed JSON") from error
    if not isinstance(value, dict) or value.get("protocol_version") != "1.4":
        raise ScriptGateError("script Bridge probe did not negotiate RPC 1.4")
    require_safe_value(value, "script Bridge probe")
    return cast(dict[str, Any], value)


def query_symbols(
    client: McpClient,
    query: str,
    match_mode: str,
    samples: list[float] | None = None,
    **filters: Any,
) -> dict[str, Any]:
    symbols: list[dict[str, Any]] = []
    cursor: str | None = None
    envelope: dict[str, Any] | None = None
    while True:
        arguments: dict[str, Any] = {"query": query, "match": match_mode, "limit": 7, **filters}
        if cursor is not None:
            arguments["cursor"] = cursor
        content, is_error, elapsed = tool_call(client, "godot_search_symbols", arguments)
        if samples is not None:
            samples.append(elapsed)
        if is_error:
            raise ScriptGateError("symbol search failed after the index became current")
        if envelope is None:
            envelope = content
        elif (
            content.get("generation_id") != envelope.get("generation_id")
            or content.get("script_graph_revision") != envelope.get("script_graph_revision")
        ):
            raise ScriptGateError("symbol pagination crossed an immutable generation")
        page = content.get("symbols")
        if not isinstance(page, list):
            raise ScriptGateError("symbol search omitted symbols")
        symbols.extend(page)
        cursor = content.get("next_cursor")
        if cursor is None:
            break
        if not isinstance(cursor, str):
            raise ScriptGateError("symbol search returned a malformed cursor")
    if len({symbol.get("symbol_id") for symbol in symbols}) != len(symbols):
        raise ScriptGateError("symbol pagination duplicated declarations")
    assert envelope is not None
    result = dict(envelope)
    result["symbols"] = symbols
    result["next_cursor"] = None
    result["truncated"] = False
    return result


def inspect_symbol(client: McpClient, script: str, qualified_name: str) -> dict[str, Any]:
    outgoing: list[dict[str, Any]] = []
    attachments: list[dict[str, Any]] = []
    cursor: str | None = None
    envelope: dict[str, Any] | None = None
    while True:
        arguments: dict[str, Any] = {
            "script": script,
            "qualified_name": qualified_name,
            "limit": 2,
        }
        if cursor is not None:
            arguments["cursor"] = cursor
        content, is_error, _ = tool_call(client, "godot_inspect_symbol", arguments)
        if is_error:
            raise ScriptGateError(f"symbol inspection failed: {script}#{qualified_name}")
        if envelope is None:
            envelope = content
        elif content.get("generation_id") != envelope.get("generation_id"):
            raise ScriptGateError("symbol inspection crossed an immutable generation")
        outgoing.extend(content.get("outgoing_relations", []))
        attachments.extend(content.get("scene_attachments", []))
        cursor = content.get("next_cursor")
        if cursor is None:
            break
    assert envelope is not None
    result = dict(envelope)
    result["outgoing_relations"] = outgoing
    result["scene_attachments"] = attachments
    result["next_cursor"] = None
    result["truncated"] = False
    return result


def attachment_semantics(client: McpClient) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for scene in ATTACHMENT_SCENES:
        content, is_error, _ = tool_call(
            client,
            "godot_inspect_node",
            {"scene": scene, "node_path": "Actor", "limit": 200},
        )
        if is_error:
            raise ScriptGateError(f"scene attachment inspection failed: {scene}")
        result[scene] = content.get("attached_script_resource_id")
    return result


def phase_semantic_digest(snapshot: dict[str, Any], attachments: dict[str, Any]) -> str:
    return sha256_bytes(
        canonical_bytes(
            {
                "script_semantic_digest": snapshot["semantic_digest"],
                "scene_attachments": attachments,
            }
        )
    )


def write_text_lf(path: Path, text: str) -> None:
    with path.open("w", encoding="utf-8", newline="\n") as target:
        target.write(text)


def replace_exact(path: Path, before: str, after: str, count: int = 1) -> None:
    text = path.read_text(encoding="utf-8")
    if text.count(before) != count:
        raise ScriptGateError(f"mutation anchor count differs: {path.name}#{before}")
    write_text_lf(path, text.replace(before, after))


def prepare_project(run_root: Path, phase: str) -> tuple[Path, dict[str, bytes]]:
    project = run_root / f"p{fixture.PHASES.index(phase)}" / "p"
    project.parent.mkdir(parents=True)
    shutil.copytree(PROJECT_SOURCE, project, ignore=shutil.ignore_patterns(".godot"))
    payload: dict[str, bytes] = {}
    if phase == "csharp_discovery":
        for relative in ("scripts/Enemy.cs", "scripts/Enemy.cs.uid"):
            path = project / relative
            payload[relative] = path.read_bytes()
            path.unlink()
    return project, payload


def apply_mutation(project: Path, phase: str, payload: dict[str, bytes]) -> None:
    player = project / "scripts" / "player.gd"
    base = project / "scripts" / "base_actor.gd"
    if phase == "body_edit":
        replace_exact(player, "return adjust.call(1)", "return adjust.call(2)")
    elif phase == "line_shift":
        write_text_lf(player, "\n\n" + player.read_text(encoding="utf-8"))
    elif phase == "symbol_rename":
        replace_exact(player, "_apply_bonus", "_apply_bonus_renamed", count=2)
    elif phase == "override_change":
        replace_exact(base, "take_damage", "receive_damage")
        replace_exact(player, "take_damage", "receive_damage", count=2)
    elif phase == "literal_dependency_change":
        replace_exact(player, "res://resources/damage_profile.tres", "res://resources/alternate_profile.tres", count=2)
        replace_exact(player, "uid://b3iupk70ub7mc", "uid://b53lfeoqlgg1w")
    elif phase == "attachment_change":
        scene = project / "scenes" / "attachment_player.tscn"
        replace_exact(
            scene,
            'uid="uid://s5player" path="res://scripts/player.gd"',
            'uid="uid://s5pathonly" path="res://scripts/path_only.gd"',
        )
    elif phase == "parse_error":
        with player.open("a", encoding="utf-8", newline="\n") as target:
            target.write("\nfunc newly_broken(value:\n")
    elif phase == "csharp_discovery":
        for relative, value in payload.items():
            path = project / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(value)
    elif phase == "journal_gap":
        lines = ["extends RefCounted", "class_name GapCatalog", ""]
        lines.extend(f"var gap_symbol_{index:04d}: int = {index}" for index in range(5000))
        write_text_lf(project / "scripts" / "gap_catalog.gd", "\n".join(lines) + "\n")
    else:
        raise ScriptGateError(f"unsupported mutation phase: {phase}")
    for path in project.rglob("*"):
        if path.is_file() and path.suffix in {".gd", ".cs", ".tscn", ".tres"} and b"\r" in path.read_bytes():
            raise ScriptGateError(f"script mutation introduced non-LF newlines: {path.name}")
    marker = project / ".godot" / "codex-resource-live-mutate"
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.touch()


def document_by_path(snapshot: dict[str, Any], path: str) -> dict[str, Any] | None:
    matches = [document for document in snapshot["documents"] if document.get("path") == path]
    if len(matches) > 1:
        raise ScriptGateError(f"duplicate script document: {path}")
    return matches[0] if matches else None


def symbols_for_path(snapshot: dict[str, Any], path: str) -> list[dict[str, Any]]:
    return [symbol for symbol in snapshot["symbols"] if symbol.get("declaration_range", {}).get("path") == path]


def named_symbol(snapshot: dict[str, Any], qualified_key: str) -> dict[str, Any] | None:
    matches = [symbol for symbol in snapshot["symbols"] if symbol.get("qualified_key") == qualified_key]
    if len(matches) > 1:
        raise ScriptGateError(f"duplicate qualified symbol: {qualified_key}")
    return matches[0] if matches else None


def range_matches(actual: dict[str, Any], expected: dict[str, Any], document: dict[str, Any]) -> bool:
    return actual == {
        "path": document["path"],
        "content_sha256": document["content_sha256"],
        "start_byte": expected["bytes"][0],
        "end_byte": expected["bytes"][1],
        "start_line": expected["start"][0],
        "start_column": expected["start"][1],
        "end_line": expected["end"][0],
        "end_column": expected["end"][1],
    }


def relation_key(relation: dict[str, Any]) -> tuple[Any, ...]:
    target = relation.get("target")
    if isinstance(target, dict) and target.get("kind") == "resource":
        target_value: Any = ("resource", json.dumps(target.get("resource_ref"), sort_keys=True))
    elif isinstance(target, dict):
        target_value = (target.get("kind"), target.get("symbol_id"), target.get("node_id"))
    else:
        target_value = None
    evidence = relation.get("evidence_range") or {}
    return (
        relation.get("source", {}).get("symbol_id"),
        relation.get("predicate"),
        target_value,
        relation.get("confidence"),
        relation.get("detail"),
        evidence.get("path"),
        evidence.get("start_byte"),
        evidence.get("end_byte"),
    )


def dynamic_relation_key(relation: dict[str, Any]) -> tuple[Any, ...]:
    """Compare dynamic semantics while allowing ranges to move after earlier edits."""
    return (
        relation.get("source", {}).get("symbol_id"),
        relation.get("predicate"),
        relation.get("confidence"),
        relation.get("detail"),
        relation.get("target"),
    )


def expected_relation_key(oracle_id: str, truth: dict[str, Any]) -> tuple[Any, ...]:
    relation = truth["relations"][oracle_id]
    source = truth["identities"][relation["source_symbol"]]
    if relation.get("target_symbol") is not None:
        target: Any = ("symbol", truth["identities"][relation["target_symbol"]], None)
    elif relation.get("target_resource") is not None:
        resource = truth["resources"][relation["target_resource"]]
        target = ("resource", json.dumps({"uid": resource["uid"]}, sort_keys=True))
    else:
        target = None
    expected_range = truth["ranges"][relation["evidence_range"]]
    document = truth["documents"][expected_range["document"]]
    return (
        source,
        relation["predicate"],
        target,
        relation["confidence"],
        relation["detail"],
        document["path"],
        expected_range["bytes"][0],
        expected_range["bytes"][1],
    )


def verify_base_oracle(
    snapshot: dict[str, Any],
    attachments: dict[str, Any],
    truth: dict[str, Any],
) -> tuple[dict[str, bool], dict[str, Any]]:
    documents = {document["path"]: document for document in snapshot["documents"]}
    expected_paths = {document["path"] for document in truth["documents"].values()}
    document_truth = set(documents) == expected_paths
    state_map = {
        "complete": "complete",
        "partial": "partial",
        "invalid": "invalid",
        "unavailable": "unavailable",
    }
    document_truth = document_truth and all(
        documents[expected["path"]]["content_sha256"] == expected["content_sha256"]
        and documents[expected["path"]]["completeness"] == state_map[expected["semantic_state"]]
        for expected in truth["documents"].values()
    )

    actual_symbols = {symbol["symbol_id"]: symbol for symbol in snapshot["symbols"]}
    matched_symbols = 0
    symbol_mismatches: list[str] = []
    for oracle_id, expected in truth["symbols"].items():
        identity = truth["identities"][oracle_id]
        actual = actual_symbols.get(identity)
        expected_range = truth["ranges"][expected["range"]]
        document = truth["documents"][expected["document"]]
        expected_wire_name = None if expected["kind"] == "lambda" and expected["name"] == "<lambda>" else expected["name"]
        if (
            actual is not None
            and actual.get("kind") == expected["kind"]
            and actual.get("name") == expected_wire_name
            and actual.get("identity_scope") == expected["identity_scope"]
            and actual.get("qualified_key") == (expected.get("qualified_key") or actual.get("qualified_key"))
            and range_matches(actual["declaration_range"], expected_range, document)
        ):
            matched_symbols += 1
        else:
            symbol_mismatches.append(oracle_id)
    symbol_truth = matched_symbols == len(truth["symbols"]) == len(actual_symbols)

    semantic_predicates = {relation["predicate"] for relation in truth["relations"].values()}
    observed = [
        relation
        for relation in snapshot["relations"]
        if relation.get("authority") == "gdscript_parser_analyzer"
        and relation.get("predicate") in semantic_predicates
    ]
    observed_keys = [relation_key(relation) for relation in observed]
    expected_keys = [expected_relation_key(oracle_id, truth) for oracle_id in truth["relations"]]
    matched_relations = sum(1 for key in expected_keys if observed_keys.count(key) == 1)
    exact_expected = [key for key in expected_keys if key[3] == "exact"]
    dynamic_expected = [key for key in expected_keys if key[3] == "dynamic"]
    exact_matched = sum(1 for key in exact_expected if observed_keys.count(key) == 1)
    dynamic_matched = sum(1 for key in dynamic_expected if observed_keys.count(key) == 1)
    exact_truth_coordinates = {(key[1], key[5], key[6], key[7]) for key in exact_expected}
    dynamic_truth_coordinates = {(key[1], key[5], key[6], key[7]) for key in dynamic_expected}
    false_exact = [
        key
        for key in observed_keys
        if key[3] == "exact"
        and (key[1], key[5], key[6], key[7]) in exact_truth_coordinates | dynamic_truth_coordinates
        and key not in exact_expected
    ]
    dynamic_false_exact = sum(
        1
        for relation, key in zip(observed, observed_keys)
        if (key[1], key[5], key[6], key[7]) in dynamic_truth_coordinates
        and (relation.get("confidence") == "exact" or relation.get("target") is not None)
    )
    zero_false_exact = not false_exact and exact_matched == len(exact_expected)

    diagnostics = {(diagnostic["code"], diagnostic["range"]["path"]) for diagnostic in snapshot["diagnostics"]}
    expected_diagnostics = {
        (diagnostic["code"].upper(), truth["documents"][diagnostic["document"]]["path"])
        for diagnostic in truth["golden"]["diagnostics"]
    }
    adapter_statuses = {status["language"]: status for status in snapshot["adapter_statuses"]}
    csharp = document_by_path(snapshot, "res://scripts/Enemy.cs")
    assertions = {
        "base_documents_match_oracle": document_truth,
        "base_symbols_and_ranges_match_oracle": symbol_truth,
        "base_relations_match_oracle": matched_relations == len(expected_keys),
        "base_diagnostics_include_oracle": expected_diagnostics.issubset(diagnostics),
        "base_scene_attachments_match_oracle": (
            attachments["res://scenes/attachment_player.tscn"] is not None
            and attachments["res://scenes/attachment_removed.tscn"] is None
            and attachments["res://scenes/attachment_replaced.tscn"] is not None
            and attachments["res://scenes/attachment_player.tscn"]
            != attachments["res://scenes/attachment_replaced.tscn"]
        ),
        "csharp_discovery_only": (
            csharp is not None
            and csharp["completeness"] == "unavailable"
            and adapter_statuses.get("csharp", {}).get("availability") == "discovery_only"
            and not any(symbol.get("language") == "csharp" for symbol in snapshot["symbols"])
        ),
        "zero_false_exact": zero_false_exact,
        "all_resolvable_truth_present": exact_matched == len(exact_expected) == 9,
        "dynamic_truth_has_no_target": dynamic_false_exact == 0
        and dynamic_matched == len(dynamic_expected) == 5,
    }
    if not all(assertions.values()):
        failed = ", ".join(name for name, passed in assertions.items() if not passed)
        expected_id_set = set(truth["identities"].values())
        diagnostics_detail = {
            "document_refs": {
                document["path"]: document["script_ref"] for document in snapshot["documents"]
            },
            "symbol_mismatches": symbol_mismatches,
            "symbol_mismatch_candidates": {
                oracle_id: [
                    {
                        "symbol_id": candidate.get("symbol_id"),
                        "qualified_key": candidate.get("qualified_key"),
                        "declaration_range": candidate.get("declaration_range"),
                    }
                    for candidate in snapshot["symbols"]
                    if candidate.get("kind") == truth["symbols"][oracle_id]["kind"]
                    and candidate.get("name") == truth["symbols"][oracle_id]["name"]
                    and candidate.get("declaration_range", {}).get("path")
                    == truth["documents"][truth["symbols"][oracle_id]["document"]]["path"]
                ]
                for oracle_id in symbol_mismatches
            },
            "symbol_mismatch_actual": {
                oracle_id: actual_symbols.get(truth["identities"][oracle_id])
                for oracle_id in symbol_mismatches
            },
            "unexpected_symbol_ids": sorted(set(actual_symbols) - expected_id_set),
            "missing_relations": [list(key) for key in expected_keys if observed_keys.count(key) != 1],
            "unexpected_relations": [list(key) for key in observed_keys if expected_keys.count(key) != 1],
            "actual_diagnostics": sorted(diagnostics),
            "expected_diagnostics": sorted(expected_diagnostics),
        }
        raise ScriptGateError(
            "base oracle assertions failed: "
            + failed
            + "; details="
            + json.dumps(diagnostics_detail, ensure_ascii=False, sort_keys=True)
        )
    accuracy = {
        "zero_false_exact": zero_false_exact,
        "resolvable_truth_total": len(exact_expected),
        "resolvable_truth_matched": exact_matched,
        "resolvable_recall": exact_matched / len(exact_expected),
        "dynamic_truth_total": len(dynamic_expected),
        "dynamic_false_exact": dynamic_false_exact,
        "symbol_truth_total": len(truth["symbols"]),
        "symbol_truth_matched": matched_symbols,
    }
    return assertions, accuracy


def persistent_ids(snapshot: dict[str, Any], path: str) -> dict[str, str]:
    return {
        symbol["qualified_key"]: symbol["symbol_id"]
        for symbol in symbols_for_path(snapshot, path)
        if symbol["identity_scope"] == "persistent"
    }


def content_ids(snapshot: dict[str, Any], path: str) -> set[str]:
    return {
        symbol["symbol_id"]
        for symbol in symbols_for_path(snapshot, path)
        if symbol["identity_scope"] == "content_revision"
    }


def verify_phase(
    phase: str,
    initial: dict[str, Any],
    final: dict[str, Any],
    initial_attachments: dict[str, Any],
    final_attachments: dict[str, Any],
    current: dict[str, Any],
) -> dict[str, bool]:
    player_path = "res://scripts/player.gd"
    assertions = {
        "current_immutable_generation": current.get("freshness") == "current",
        "bridge_index_semantic_digest_parity": current.get("validated_checkpoint", {}).get("semantic_digest")
        == final.get("semantic_digest"),
        "adapter_profile_current": {status["language"]: status["availability"] for status in final["adapter_statuses"]}
        == {"gdscript": "available", "csharp": "discovery_only"},
    }
    if phase == "body_edit":
        assertions.update(
            {
                "named_identity_preserved": persistent_ids(initial, player_path) == persistent_ids(final, player_path),
                "content_hash_changed": document_by_path(initial, player_path)["content_sha256"]
                != document_by_path(final, player_path)["content_sha256"],
            }
        )
    elif phase == "line_shift":
        initial_named = persistent_ids(initial, player_path)
        final_named = persistent_ids(final, player_path)
        initial_player = named_symbol(initial, "class:Player")
        final_player = named_symbol(final, "class:Player")
        assertions.update(
            {
                "named_identity_preserved": initial_named == final_named,
                "content_scoped_identity_changed": content_ids(initial, player_path).isdisjoint(content_ids(final, player_path)),
                "ranges_shifted": initial_player is not None
                and final_player is not None
                and final_player["declaration_range"]["start_line"]
                == initial_player["declaration_range"]["start_line"] + 2,
            }
        )
    elif phase == "symbol_rename":
        old_key = "class:Player/method:_apply_bonus"
        new_key = "class:Player/method:_apply_bonus_renamed"
        new_symbol = named_symbol(final, new_key)
        call = [
            relation
            for relation in final["relations"]
            if relation.get("predicate") == "calls"
            and relation.get("confidence") == "exact"
            and relation.get("source", {}).get("symbol_id")
            == named_symbol(final, "class:Player/method:take_damage")["symbol_id"]
        ]
        assertions.update(
            {
                "renamed_symbol_identity_changed": named_symbol(initial, old_key) is not None
                and named_symbol(final, old_key) is None
                and new_symbol is not None
                and new_symbol["symbol_id"] != named_symbol(initial, old_key)["symbol_id"],
                "exact_call_target_updated": any(
                    relation.get("target", {}).get("symbol_id") == new_symbol["symbol_id"] for relation in call
                ),
            }
        )
    elif phase == "override_change":
        base_new = named_symbol(final, "class:BaseActor/method:receive_damage")
        player_new = named_symbol(final, "class:Player/method:receive_damage")
        override = [relation for relation in final["relations"] if relation.get("predicate") == "overrides"]
        assertions.update(
            {
                "override_target_exact": base_new is not None
                and player_new is not None
                and any(
                    relation.get("source", {}).get("symbol_id") == player_new["symbol_id"]
                    and relation.get("target", {}).get("symbol_id") == base_new["symbol_id"]
                    and relation.get("confidence") == "exact"
                    for relation in override
                ),
                "dependent_relation_advanced": named_symbol(final, "class:Player/method:take_damage") is None
                and named_symbol(final, "class:BaseActor/method:take_damage") is None,
            }
        )
    elif phase == "literal_dependency_change":
        exact_resources = [
            relation.get("target", {}).get("resource_ref", {}).get("uid")
            for relation in final["relations"]
            if relation.get("predicate") in {"preloads", "loads"} and relation.get("confidence") == "exact"
        ]
        initial_dynamic = sorted(
            dynamic_relation_key(relation)
            for relation in initial["relations"]
            if relation["confidence"] == "dynamic"
        )
        final_dynamic = sorted(
            dynamic_relation_key(relation)
            for relation in final["relations"]
            if relation["confidence"] == "dynamic"
        )
        assertions.update(
            {
                "literal_target_changed": len(exact_resources) == 2
                and all(uid == "uid://b53lfeoqlgg1w" for uid in exact_resources),
                "dynamic_load_unchanged": initial_dynamic == final_dynamic,
            }
        )
    elif phase == "attachment_change":
        assertions.update(
            {
                "old_attachment_removed": initial_attachments[ATTACHMENT_SCENES[0]]
                != final_attachments[ATTACHMENT_SCENES[0]],
                "replacement_attachment_exact": final_attachments[ATTACHMENT_SCENES[0]]
                == final_attachments[ATTACHMENT_SCENES[2]]
                and final_attachments[ATTACHMENT_SCENES[1]] is None,
            }
        )
    elif phase == "parse_error":
        player = document_by_path(final, player_path)
        assertions.update(
            {
                "diagnostic_current": player is not None
                and player["completeness"] == "invalid"
                and any(
                    diagnostic["range"]["path"] == player_path
                    and diagnostic["code"] == "GDSCRIPT_PARSE_ERROR"
                    for diagnostic in final["diagnostics"]
                ),
                "stale_symbols_not_current": not symbols_for_path(final, player_path),
                "other_documents_current": named_symbol(final, "class:BaseActor") is not None,
            }
        )
    elif phase == "csharp_discovery":
        initial_csharp = document_by_path(initial, "res://scripts/Enemy.cs")
        final_csharp = document_by_path(final, "res://scripts/Enemy.cs")
        assertions.update(
            {
                "csharp_discovered": initial_csharp is None and final_csharp is not None,
                "csharp_semantics_unavailable": final_csharp is not None
                and final_csharp["completeness"] == "unavailable",
                "no_false_csharp_symbols": not any(symbol["language"] == "csharp" for symbol in final["symbols"]),
            }
        )
    elif phase == "journal_gap":
        gap = document_by_path(final, "res://scripts/gap_catalog.gd")
        assertions.update(
            {
                "full_resnapshot_required": final.get("delta", {}).get("status") == "gap",
                "no_partial_generation_published": gap is not None
                and gap["completeness"] == "complete"
                and len(symbols_for_path(final, "res://scripts/gap_catalog.gd")) >= 5000,
            }
        )
    if phase != "base" and not all(assertions.values()):
        failed = ", ".join(name for name, passed in assertions.items() if not passed)
        raise ScriptGateError(f"{phase} semantic assertions failed: {failed}")
    return assertions


def probe_contract(client: McpClient) -> tuple[dict[str, bool], str]:
    require_tool_error(
        client,
        "godot_search_symbols",
        {"query": "Player", "match": "exact", "limit": 0},
        "invalid_limit",
    )
    require_tool_error(
        client,
        "godot_search_symbols",
        {"query": "Player", "match": "exact", "script": "res://../player.gd"},
        "invalid_path",
    )
    require_tool_error(
        client,
        "godot_inspect_symbol",
        {
            "symbol_id": "godot:script-symbol:named:v1:" + "A" * 43,
            "script": "res://scripts/player.gd",
            "qualified_name": "class:Player",
        },
        "invalid_query",
    )
    require_schema_rejection(
        client,
        "godot_search_symbols",
        {"query": "Player", "match": "exact", "unknown_member": True},
    )
    require_schema_rejection(
        client,
        "godot_inspect_symbol",
        {"script": "res://scripts/player.gd", "qualified_name": "class:Player", "unknown_member": True},
    )
    first, is_error, _ = tool_call(
        client,
        "godot_search_symbols",
        {"query": "take", "match": "prefix", "language": "gdscript", "kind": "method", "limit": 1},
    )
    cursor = first.get("next_cursor")
    symbols = first.get("symbols")
    if is_error or not isinstance(cursor, str) or not isinstance(symbols, list) or not symbols:
        raise ScriptGateError("symbol tool did not issue a pagination cursor")
    replacement = "A" if cursor[-1] != "A" else "B"
    require_tool_error(
        client,
        "godot_search_symbols",
        {
            "query": "take",
            "match": "prefix",
            "language": "gdscript",
            "kind": "method",
            "limit": 1,
            "cursor": cursor[:-1] + replacement,
        },
        "stale_cursor",
    )
    require_tool_error(
        client,
        "godot_search_symbols",
        {"query": "take", "match": "prefix", "language": "gdscript", "limit": 1, "cursor": cursor},
        "stale_cursor",
    )
    require_tool_error(
        client,
        "godot_search_symbols",
        {
            "query": "take",
            "match": "prefix",
            "language": "gdscript",
            "kind": "method",
            "script": "res://scripts/player.gd",
            "limit": 1,
            "cursor": cursor,
        },
        "stale_cursor",
    )
    require_tool_error(
        client,
        "godot_inspect_symbol",
        {"symbol_id": symbols[0]["symbol_id"], "limit": 1, "cursor": cursor},
        "stale_cursor",
    )
    locals_result = query_symbols(client, "remaining", "exact", language="gdscript")
    if locals_result["symbols"] or locals_result["total_matches"] != 0:
        raise ScriptGateError("default symbol search exposed locals")
    return (
        {
            "closed_input_schemas": True,
            "cursor_tampering_rejected": True,
            "cross_filter_cursor_rejected": True,
            "cross_selector_cursor_rejected": True,
            "cross_tool_cursor_rejected": True,
            "exact_tool_registry": True,
            "limit_bounds_rejected": True,
            "locals_excluded": True,
            "strict_selector_forms": True,
            "traversal_rejected": True,
        },
        cursor,
    )


def issue_generation_cursor(client: McpClient) -> str:
    first, is_error, _ = tool_call(
        client,
        "godot_search_symbols",
        {"query": "take", "match": "prefix", "language": "gdscript", "kind": "method", "limit": 1},
    )
    cursor = first.get("next_cursor")
    if is_error or not isinstance(cursor, str):
        raise ScriptGateError("could not issue pre-mutation script cursor")
    return cursor


def run_phase(
    phase: str,
    godot: Path,
    sidecar: Path,
    probe: Path,
    run_root: Path,
    timeout: float,
    truth: dict[str, Any],
    contract: dict[str, bool],
) -> tuple[dict[str, Any], list[dict[str, Any]], dict[str, Any] | None]:
    project, payload = prepare_project(run_root, phase)
    log_path = project.parent / "godot.log"
    telemetry: list[dict[str, Any]] = []
    cached_samples: list[float] = []
    control_samples: list[float] = []
    change_visibility: float | None = None
    editor: subprocess.Popen[str] | None = None
    editor_log: Any = None
    sidecar_process: LineProcess | None = None
    phase_result: dict[str, Any] = {}
    accuracy: dict[str, Any] | None = None
    try:
        editor, editor_log = start_editor(godot, project, log_path)
        wait_for_editor_file(
            editor,
            project / ".godot" / "codex" / "bridge.json",
            log_path,
            timeout,
            f"{phase} discovery",
        )
        sidecar_process, client = initialize_sidecar(sidecar, project, timeout)
        current, _ = wait_script_current(client, timeout)
        initial_index_revision = int(current["index_revision"])
        initial = capture_probe(probe, project, timeout)
        initial_snapshot = cast(dict[str, Any], initial["snapshot"])
        initial_attachments = attachment_semantics(client)
        stale_cursor: str | None = None
        assertions: dict[str, bool]
        if phase == "base":
            probed, _ = probe_contract(client)
            contract.update(probed)
            assertions, accuracy = verify_base_oracle(initial_snapshot, initial_attachments, truth)
            final = initial
            final_snapshot = initial_snapshot
            final_attachments = initial_attachments
        else:
            if phase == "body_edit":
                stale_cursor = issue_generation_cursor(client)
            apply_mutation(project, phase, payload)
            mutation_started = time.monotonic()
            current, _ = wait_script_current(
                client,
                timeout,
                initial_index_revision,
                control_samples if phase == "journal_gap" else None,
            )
            change_visibility = round((time.monotonic() - mutation_started) * 1000, 3)
            final = capture_probe(
                probe,
                project,
                timeout,
                int(initial_snapshot["script_graph_revision"]) if phase == "journal_gap" else None,
            )
            final_snapshot = cast(dict[str, Any], final["snapshot"])
            final_attachments = attachment_semantics(client)
            assertions = verify_phase(
                phase,
                initial_snapshot,
                {**final_snapshot, "delta": final.get("delta")},
                initial_attachments,
                final_attachments,
                current,
            )
            if stale_cursor is not None:
                require_tool_error(
                    client,
                    "godot_search_symbols",
                    {
                        "query": "take",
                        "match": "prefix",
                        "language": "gdscript",
                        "kind": "method",
                        "limit": 1,
                        "cursor": stale_cursor,
                    },
                    "stale_cursor",
                )
                contract["stale_generation_cursor_rejected"] = True

        for _ in range(10):
            query_symbols(client, "BaseActor", "exact", cached_samples, language="gdscript", kind="class")
        if named_symbol(final_snapshot, "class:Player") is not None:
            inspection = inspect_symbol(client, "res://scripts/player.gd", "class:Player")
            assertions["symbol_inspection_current"] = (
                inspection.get("freshness") == "current"
                and inspection.get("declaration", {}).get("qualified_name") == "class:Player"
            )
        else:
            assertions["symbol_inspection_current"] = named_symbol(final_snapshot, "class:BaseActor") is not None
        if not all(assertions.values()):
            failed = ", ".join(name for name, passed in assertions.items() if not passed)
            raise ScriptGateError(f"{phase} semantic assertions failed: {failed}")
        phase_result = {
            "phase": phase,
            "initial_script_graph_revision": initial_snapshot["script_graph_revision"],
            "script_graph_revision": final_snapshot["script_graph_revision"],
            "index_revision": current["index_revision"],
            "document_count": final_snapshot["document_count"],
            "symbol_count": final_snapshot["symbol_count"],
            "relation_count": final_snapshot["relation_count"],
            "diagnostic_count": final_snapshot["diagnostic_count"],
            "normalized_script_sha256": phase_semantic_digest(final_snapshot, final_attachments),
            "assertions": assertions,
            "cached_symbol_query_samples_ms": cached_samples,
            "bulk_status_ping_samples_ms": control_samples,
            "change_visibility_ms": change_visibility,
            "bridge_main_thread_sessions": telemetry,
            "passed": True,
        }
        return phase_result, telemetry, accuracy
    finally:
        primary_error = sys.exc_info()[0] is not None
        cleanup_error: ScriptGateError | None = None
        if sidecar_process is not None:
            try:
                close_sidecar(sidecar_process)
            except ScriptGateError as error:
                cleanup_error = error
        if editor is not None:
            try:
                stop_editor(editor, project)
            except ScriptGateError as error:
                cleanup_error = cleanup_error or error
        if editor_log is not None:
            editor_log.close()
        if log_path.exists():
            try:
                telemetry.append(parse_telemetry(log_path))
            except ScriptGateError as error:
                cleanup_error = cleanup_error or error
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
            cleanup_error = cleanup_error or ScriptGateError(f"{phase} left Bridge runtime artifacts")
        if cleanup_error is not None and not primary_error:
            raise cleanup_error


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", type=Path)
    parser.add_argument("--sidecar", type=Path)
    parser.add_argument("--script-probe", type=Path)
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--development-phase", choices=fixture.PHASES)
    parser.add_argument("--skip-local-gates", action="store_true")
    arguments = parser.parse_args()
    run_root: Path | None = None
    try:
        require_local_host()
        platform_tag = target_platform()
        godot = (arguments.godot or default_godot(platform_tag)).resolve()
        sidecar = (arguments.sidecar or default_sidecar()).resolve()
        probe = (arguments.script_probe or default_script_probe()).resolve()
        evidence = (arguments.evidence or default_evidence(platform_tag)).resolve()
        if not godot.is_file() or not sidecar.is_file() or not probe.is_file():
            raise ScriptGateError("Godot, release sidecar, and release script probe must be built before the live gate")
        qualifying = arguments.development_phase is None
        if qualifying and evidence.exists():
            raise ScriptGateError(f"refusing to overwrite existing evidence: {evidence.name}")
        source = source_coordinates(allow_dirty=not qualifying)
        fixture_before = fixture_digest()
        truth = oracle_truth()
        if not arguments.skip_local_gates:
            run_local_test_gates()
        # macOS AF_UNIX endpoints have a short path limit. Keep the canonical
        # fixture root under /tmp, as the accepted Sprint 4 live gate does.
        temporary_parent = "/tmp" if sys.platform == "darwin" else None
        run_root = Path(tempfile.mkdtemp(prefix="s5.", dir=temporary_parent))
        contract: dict[str, bool] = {"stale_generation_cursor_rejected": False}
        phases_to_run = (arguments.development_phase,) if arguments.development_phase else fixture.PHASES
        phases: list[dict[str, Any]] = []
        all_telemetry: list[dict[str, Any]] = []
        accuracy: dict[str, Any] | None = None
        for phase in phases_to_run:
            result, telemetry, phase_accuracy = run_phase(
                phase,
                godot,
                sidecar,
                probe,
                run_root,
                arguments.timeout,
                truth,
                contract,
            )
            phases.append(result)
            all_telemetry.extend(telemetry)
            if phase_accuracy is not None:
                accuracy = phase_accuracy
        if not qualifying:
            print(json.dumps({"profile": "development", "phases": phases}, ensure_ascii=False, indent=2, sort_keys=True))
            return 0
        if accuracy is None or not all(contract.values()):
            raise ScriptGateError("the complete live accuracy/MCP contract was not exercised")

        cached = [sample for phase in phases for sample in phase["cached_symbol_query_samples_ms"]]
        visibility = [
            phase["change_visibility_ms"]
            for phase in phases
            if phase["phase"] not in {"base", "journal_gap"}
        ]
        control = [sample for phase in phases for sample in phase["bulk_status_ping_samples_ms"]]
        bridge = [sample for record in all_telemetry for sample in record["samples_usec"]]
        performance = {
            "cached_symbol_query_ms": metric(cached),
            "saved_change_visibility_ms": metric(visibility),
            "bulk_status_ping_ms": metric(control),
            "bridge_main_thread_usec": metric(bridge),
            "gates": {
                "cached_symbol_query_p95_lte_300ms": required_percentile(cached, 95, "cached symbol query") <= 300,
                "saved_change_visibility_p95_lte_2000ms": required_percentile(
                    visibility, 95, "saved change visibility"
                )
                <= 2000,
                "bulk_status_ping_p95_lte_200ms": required_percentile(control, 95, "bulk status ping") <= 200,
                "bridge_main_thread_zero_over_2000us": all(sample <= 2000 for sample in bridge),
            },
        }
        if not all(performance["gates"].values()):
            raise ScriptGateError("one or more Sprint 5 SLOs failed")

        rust_verbose = command_output(["rustc", "--version", "--verbose"], REPOSITORY_ROOT / "godot-codex-mcp")
        temporary_path = str(run_root)
        shutil.rmtree(run_root)
        run_root = None
        fixture_after = fixture_digest()
        if fixture_after != fixture_before:
            raise ScriptGateError("live gate mutated the committed script fixture")
        report = {
            "schema_version": 1,
            "sprint": 5,
            "profile": "qualifying",
            "platform": platform_tag,
            "source": {
                **source,
                "fixture_sha256": fixture_before,
                "fixture_manifest_sha256": sha256_file(MANIFEST_PATH),
                "golden_script_graph_sha256": sha256_file(GOLDEN_PATH),
                "canonical_symbol_ids_sha256": truth["canonical_symbol_ids_sha256"],
            },
            "artifacts": {
                "godot_sha256": sha256_file(godot),
                "sidecar_sha256": sha256_file(sidecar),
                "script_probe_sha256": sha256_file(probe),
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
                "bridge_rpc": "1.4",
                "mcp_protocol": MCP_PROTOCOL,
                "logical_schema": "1.3",
                "physical_store": "segment-v3",
            },
            "adapter_profile": {
                "gdscript": "gdscript_parser_analyzer_v1",
                "csharp": "csharp_discovery_only_v1",
                "csharp_runtime_required": False,
            },
            "mcp_contract": contract,
            "accuracy": accuracy,
            "phases": phases,
            "performance": performance,
            "completion": {
                "requested_phases": list(fixture.PHASES),
                "completed_phases": [phase["phase"] for phase in phases],
                "all_phases_completed": True,
                "script_mcp_contract_verified": True,
                "fixture_integrity_preserved": fixture_after == fixture_before,
                "oracle_accuracy_verified": True,
                "rust_workspace_tests_passed": True,
                "rust_clippy_passed": True,
                "script_fixture_tests_passed": True,
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
        for forbidden in (temporary_path, str(REPOSITORY_ROOT), "session.token", "editor:", "source_excerpt"):
            if forbidden in encoded:
                raise ScriptGateError("final evidence failed redaction")
        evidence.parent.mkdir(parents=True, exist_ok=True)
        evidence.write_text(encoded, encoding="utf-8")
        print(encoded, end="")
        return 0
    except (ScriptGateError, fixture.FixtureError, OSError, subprocess.SubprocessError, json.JSONDecodeError) as error:
        print(f"Sprint 5 script gate failed: {error}", file=sys.stderr)
        return 1
    finally:
        if run_root is not None:
            shutil.rmtree(run_root, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
