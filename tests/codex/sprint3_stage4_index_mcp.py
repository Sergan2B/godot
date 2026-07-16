#!/usr/bin/env python3
"""Run the local model-free Sprint 3 persistent-index and resource MCP gate."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from resource_graph_fixture import (
    GOLDEN_PATH,
    PROJECT_SOURCE,
    apply_phase,
    restore_project,
    strict_json_load,
)
from sprint2_live_smoke import MCP_PROTOCOL, LineProcess, McpClient
from sprint3_acceptance import (
    BRIDGE_TELEMETRY_SAMPLE_CAPACITY,
    REDACTION_FIELDS,
    redaction_status,
)
from sprint3_stage3_live import (
    PHASES,
    apply_prepared_diff,
    create_gap_resources,
    fixture_digest,
    start_editor,
    stop_editor,
    wait_for,
)

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint3_source_scopes.txt"
SOURCE_SCOPE_MANIFEST = SOURCE_SCOPE_PATH.relative_to(REPOSITORY_ROOT).as_posix()
SOURCE_SCOPES = tuple(
    line for line in SOURCE_SCOPE_PATH.read_text(encoding="utf-8").splitlines() if line and not line.startswith("#")
)
if (
    not SOURCE_SCOPES
    or tuple(sorted(set(SOURCE_SCOPES))) != SOURCE_SCOPES
    or SOURCE_SCOPE_MANIFEST not in SOURCE_SCOPES
):
    raise RuntimeError("Sprint 3 source scopes must be nonempty, unique, and sorted")
TELEMETRY_PREFIX = "[codex_bridge_evidence] "
TOOL_NAMES = {
    "godot_get_current_scene",
    "godot_get_editor_state",
    "godot_get_selected_nodes",
    "godot_get_resource_dependencies",
    "godot_find_resource_owners",
}
RESOURCE_TOOLS = (
    "godot_get_resource_dependencies",
    "godot_find_resource_owners",
)
RMCP_UNKNOWN_MEMBER_REJECTION_TEXT = (
    "failed to deserialize parameters: unknown field `unknown_member`, expected one of `resource`, `limit`, `cursor`"
)
MCP_CONTRACT_FIELDS = {
    "closed_input_schemas",
    "cross_project_cursor_rejected",
    "cross_tool_cursor_rejected",
    "exact_ordering",
    "exact_tool_registry",
    "limit_bounds_rejected",
    "schema_negatives_rejected",
    "stable_project_scope",
    "stale_generation_cursor_rejected",
    "tampered_cursor_rejected",
    "traversal_rejected",
}
CI_ENVIRONMENT_MARKERS = (
    "APPVEYOR",
    "BITBUCKET_BUILD_NUMBER",
    "BUILDKITE",
    "CI",
    "CIRCLECI",
    "CODEBUILD_BUILD_ID",
    "CONTINUOUS_INTEGRATION",
    "DRONE",
    "GITEA_ACTIONS",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "JENKINS_URL",
    "TEAMCITY_VERSION",
    "TF_BUILD",
    "TRAVIS",
    "WOODPECKER",
)


class IndexMcpGateError(RuntimeError):
    """Raised when persistent index or MCP output differs from the oracle."""


def require_local_host_for_qualifying_evidence(
    qualifying: bool,
    environment: Mapping[str, str] | None = None,
) -> None:
    if not qualifying:
        return
    variables = os.environ if environment is None else environment
    detected = [name for name in CI_ENVIRONMENT_MARKERS if name in variables]
    if detected:
        raise IndexMcpGateError(
            "qualifying evidence requires a local host; detected CI environment markers: " + ", ".join(detected)
        )


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def percentile(samples: list[float] | list[int], percentile_value: int) -> float:
    if not samples:
        raise IndexMcpGateError("cannot calculate a percentile without samples")
    ordered = sorted(samples)
    index = max(0, min(len(ordered) - 1, math.ceil(percentile_value * len(ordered) / 100) - 1))
    return round(ordered[index], 3)


def target_platform() -> str:
    machine = platform.machine().lower()
    if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
        return "macos-arm64"
    if sys.platform == "win32" and machine in {"amd64", "x86_64"}:
        return "windows-x86_64"
    raise IndexMcpGateError("the Sprint 3 live gate requires macOS arm64 or Windows x86_64")


def default_sidecar() -> Path:
    name = "godot-codex-mcp.exe" if sys.platform == "win32" else "godot-codex-mcp"
    return REPOSITORY_ROOT / "godot-codex-mcp" / "target" / "release" / name


def default_godot(platform_tag: str) -> Path:
    name = (
        "godot.macos.editor.dev.arm64"
        if platform_tag == "macos-arm64"
        else "godot.windows.editor.dev.x86_64.console.exe"
    )
    return REPOSITORY_ROOT / "bin" / name


def default_evidence(platform_tag: str) -> Path:
    suffix = "macos" if platform_tag == "macos-arm64" else "windows"
    return SCRIPT_DIR / "evidence" / f"sprint-3-resource-graph-{suffix}.json"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return f"sha256:{digest.hexdigest()}"


def require_redacted(value: Any, context: str, *, allow_session_identity: bool = False) -> dict[str, bool]:
    status: dict[str, bool] = redaction_status(value)
    checked = {
        name: passed
        for name, passed in status.items()
        if not allow_session_identity or name != "session_identifiers_absent"
    }
    if not all(checked.values()):
        failed = ", ".join(sorted(name for name, passed in checked.items() if not passed))
        raise IndexMcpGateError(f"{context} failed redaction checks: {failed}")
    return status


def command_output(command: list[str], *, cwd: Path = REPOSITORY_ROOT) -> str:
    result = subprocess.run(
        command,
        cwd=cwd,
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
    )
    if result.returncode != 0:
        raise IndexMcpGateError(f"command failed while binding evidence: {command[0]}")
    return result.stdout.strip()


def scons_version() -> str:
    commands = [
        [str(REPOSITORY_ROOT / ".venv" / "bin" / "scons"), "--version"],
        [str(REPOSITORY_ROOT / ".venv" / "Scripts" / "scons.exe"), "--version"],
    ]
    located = shutil.which("scons")
    if located:
        commands.append([located, "--version"])
    commands.append([sys.executable, "-m", "SCons", "--version"])
    for command in commands:
        if command[0] != sys.executable and not Path(command[0]).is_file():
            continue
        result = subprocess.run(
            command,
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )
        match = re.search(r"^\s*SCons:\s+v([^,\s]+)", result.stdout, re.MULTILINE)
        if result.returncode == 0 and match:
            return f"SCons {match.group(1)}"
    raise IndexMcpGateError("cannot determine the SCons version used by the local build")


def toolchain_coordinates() -> dict[str, str]:
    rust_workspace = REPOSITORY_ROOT / "godot-codex-mcp"
    rust_verbose = command_output(["rustc", "--version", "--verbose"], cwd=rust_workspace)
    rust_target = next(
        (line.removeprefix("host: ") for line in rust_verbose.splitlines() if line.startswith("host: ")),
        "",
    )
    if not rust_target:
        raise IndexMcpGateError("rustc host triple is unavailable")
    return {
        "python": f"{platform.python_implementation()} {platform.python_version()}",
        "scons": scons_version(),
        "rustc": rust_verbose.splitlines()[0],
        "cargo": command_output(["cargo", "--version"], cwd=rust_workspace).splitlines()[0],
        "rust_target": rust_target,
    }


def source_coordinates() -> dict[str, Any]:
    status = command_output(["git", "--literal-pathspecs", "status", "--porcelain", "--", *SOURCE_SCOPES])
    listed = subprocess.run(
        [
            "git",
            "--literal-pathspecs",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            *SOURCE_SCOPES,
        ],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        timeout=30,
    )
    if listed.returncode != 0:
        raise IndexMcpGateError("git ls-files failed while binding live evidence")
    paths = sorted(path for path in listed.stdout.split(b"\0") if path)
    digest = hashlib.sha256()
    for encoded_path in paths:
        relative = encoded_path.decode("utf-8")
        content = (REPOSITORY_ROOT / relative).read_bytes()
        digest.update(len(encoded_path).to_bytes(8, "big"))
        digest.update(encoded_path)
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    return {
        "git_commit": command_output(["git", "rev-parse", "HEAD"]),
        "git_dirty": bool(status),
        "source_tree_sha256": f"sha256:{digest.hexdigest()}",
    }


def version_output(executable: Path) -> str:
    result = subprocess.run(
        [str(executable), "--version"],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=15,
    )
    if result.returncode != 0 or not result.stdout.strip():
        raise IndexMcpGateError(f"cannot read version from {executable.name}")
    return result.stdout.strip().splitlines()[0]


def build_sidecar(sidecar: Path) -> None:
    workspace = REPOSITORY_ROOT / "godot-codex-mcp"
    result = subprocess.run(
        [
            "cargo",
            "build",
            "--locked",
            "--release",
            "-p",
            "godot-codex-mcp",
        ],
        cwd=workspace,
        check=False,
        capture_output=True,
        text=True,
        timeout=300,
    )
    if result.returncode != 0 or not sidecar.is_file():
        raise IndexMcpGateError("could not build the release sidecar")


def validate_tool_registry(tools: Any) -> list[dict[str, Any]]:
    if (
        not isinstance(tools, list)
        or len(tools) != len(TOOL_NAMES)
        or not all(isinstance(tool, dict) for tool in tools)
        or {tool.get("name") for tool in tools} != TOOL_NAMES
    ):
        raise IndexMcpGateError("MCP tool set differs from the frozen five-tool contract")
    for tool in tools:
        schema = tool.get("inputSchema")
        if (
            not isinstance(schema, dict)
            or schema.get("type") != "object"
            or schema.get("additionalProperties") is not False
        ):
            raise IndexMcpGateError(f"{tool.get('name')} input schema is not a closed object")
    return tools


def initialize_sidecar(sidecar: Path, project: Path, timeout: float) -> tuple[LineProcess, McpClient]:
    process = LineProcess(
        [str(sidecar), "--project-root", str(project)],
        cwd=project,
        env=os.environ.copy(),
    )
    client = McpClient(process, timeout=min(timeout, 10.0))
    response = client.request(
        "initialize",
        {
            "protocolVersion": MCP_PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "sprint3-index-mcp-gate", "version": "1"},
        },
    )
    try:
        require_redacted(response, "MCP initialize output", allow_session_identity=True)
    except IndexMcpGateError:
        process.stop()
        raise
    if response.get("result", {}).get("protocolVersion") != MCP_PROTOCOL:
        process.stop()
        raise IndexMcpGateError("MCP protocol version differs")
    client.notify("notifications/initialized", {})
    listed = client.request("tools/list", {})
    tools = listed.get("result", {}).get("tools", [])
    try:
        require_redacted(listed, "MCP tool registry", allow_session_identity=True)
    except IndexMcpGateError:
        process.stop()
        raise
    try:
        tools = validate_tool_registry(tools)
    except IndexMcpGateError:
        process.stop()
        raise
    if not all(
        tool.get("annotations", {}).get("readOnlyHint") is True
        and tool.get("annotations", {}).get("destructiveHint") is False
        and tool.get("annotations", {}).get("openWorldHint") is False
        for tool in tools
    ):
        process.stop()
        raise IndexMcpGateError("an MCP tool is not closed-world read-only")
    return process, client


def close_sidecar(process: LineProcess) -> None:
    stdin = process.process.stdin
    if stdin is not None and not stdin.closed:
        stdin.close()
    try:
        process.process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        process.stop()
    if process.process.returncode not in (0, -15):
        raise IndexMcpGateError(f"sidecar exited with status {process.process.returncode}")


def tool_call(
    client: McpClient,
    name: str,
    arguments: dict[str, Any],
) -> tuple[dict[str, Any], bool, float]:
    started = time.perf_counter_ns()
    response = client.request("tools/call", {"name": name, "arguments": arguments})
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    require_redacted(response, f"{name} MCP output", allow_session_identity=True)
    if "error" in response:
        raise IndexMcpGateError(f"MCP protocol error from {name}")
    result = response.get("result", {})
    content = result.get("structuredContent")
    if not isinstance(content, dict):
        raise IndexMcpGateError(f"{name} omitted structuredContent")
    return content, result.get("isError") is True, elapsed_ms


def require_tool_error(
    client: McpClient,
    name: str,
    arguments: dict[str, Any],
    expected_code: str,
) -> None:
    response = client.request("tools/call", {"name": name, "arguments": arguments})
    require_redacted(response, f"{name} negative MCP output", allow_session_identity=True)
    if "error" in response:
        raise IndexMcpGateError(f"{name} returned a protocol error instead of {expected_code}")
    result = response.get("result")
    content = result.get("structuredContent") if isinstance(result, dict) else None
    code = content.get("error", {}).get("code") if isinstance(content, dict) else None
    if not isinstance(result, dict) or result.get("isError") is not True or code != expected_code:
        raise IndexMcpGateError(f"{name} did not reject the negative query as {expected_code}")


def require_schema_rejection(
    client: McpClient,
    name: str,
    arguments: dict[str, Any],
) -> None:
    response = client.request("tools/call", {"name": name, "arguments": arguments})
    require_redacted(response, f"{name} schema-negative MCP output", allow_session_identity=True)
    expected_result = {
        "content": [
            {
                "type": "text",
                "text": RMCP_UNKNOWN_MEMBER_REJECTION_TEXT,
            }
        ],
        "isError": True,
    }
    if "error" in response or response.get("result") != expected_result:
        raise IndexMcpGateError(f"{name} did not return the pinned rmcp schema-rejection envelope")


def issue_probe_cursor(
    client: McpClient,
    tool: str,
    resource: str,
) -> tuple[str, str]:
    content, is_error, _ = tool_call(client, tool, {"resource": resource, "limit": 1})
    cursor = content.get("next_cursor")
    project_id = content.get("project_id")
    if (
        is_error
        or not isinstance(cursor, str)
        or content.get("truncated") is not True
        or not isinstance(project_id, str)
        or not project_id.startswith("project:sha256:")
    ):
        raise IndexMcpGateError(f"{tool} did not produce a project-scoped probe cursor")
    return cursor, project_id


def probe_base_mcp_contract(
    client: McpClient,
    resource: str,
) -> tuple[dict[str, bool], str, str]:
    for tool in RESOURCE_TOOLS:
        require_tool_error(client, tool, {"resource": resource, "limit": 0}, "invalid_limit")
        require_tool_error(client, tool, {"resource": resource, "limit": 201}, "invalid_limit")
        require_tool_error(client, tool, {"resource": "res://../project.godot", "limit": 50}, "invalid_path")
        require_schema_rejection(
            client,
            tool,
            {"resource": resource, "limit": 1, "unknown_member": True},
        )

    cursor, project_id = issue_probe_cursor(
        client,
        "godot_get_resource_dependencies",
        resource,
    )
    replacement = "A" if cursor[-1] != "A" else "B"
    require_tool_error(
        client,
        "godot_get_resource_dependencies",
        {"resource": resource, "limit": 1, "cursor": cursor[:-1] + replacement},
        "stale_cursor",
    )
    require_tool_error(
        client,
        "godot_find_resource_owners",
        {"resource": resource, "limit": 1, "cursor": cursor},
        "stale_cursor",
    )
    return (
        {
            "closed_input_schemas": True,
            "cross_project_cursor_rejected": False,
            "cross_tool_cursor_rejected": True,
            "exact_ordering": False,
            "exact_tool_registry": True,
            "limit_bounds_rejected": True,
            "schema_negatives_rejected": True,
            "stable_project_scope": False,
            "stale_generation_cursor_rejected": False,
            "tampered_cursor_rejected": True,
            "traversal_rejected": True,
        },
        cursor,
        project_id,
    )


def selector(resource: dict[str, Any]) -> str:
    uid = resource.get("uid")
    return uid if isinstance(uid, str) else resource["path"]


def wait_for_current(
    client: McpClient,
    resource: str,
    timeout: float,
    previous_generation: str | None = None,
    bulk_status_samples: list[float] | None = None,
) -> tuple[dict[str, Any], float]:
    started = time.monotonic()
    deadline = started + timeout
    last_code = "none"
    last_tail_line = ""
    while time.monotonic() < deadline:
        content, is_error, _ = tool_call(
            client,
            "godot_get_resource_dependencies",
            {"resource": resource, "limit": 50},
        )
        if not is_error and content.get("freshness") == "current":
            if previous_generation is None or content.get("generation_id") != previous_generation:
                return content, round((time.monotonic() - started) * 1000, 3)
        elif is_error:
            last_code = str(content.get("error", {}).get("code"))
            if last_code not in {
                "project_not_bound",
                "index_not_ready",
                "index_not_current",
                "resource_not_found",
            }:
                raise IndexMcpGateError(f"unexpected resource tool error while polling: {last_code}")
        if bulk_status_samples is not None:
            status, status_error, status_ms = tool_call(client, "godot_get_editor_state", {})
            bulk_status_samples.append(round(status_ms, 3))
            if status_error or status.get("freshness") != "current":
                raise IndexMcpGateError("editor status was unavailable during bulk rebuild")
        if client.process.tail and client.process.tail[-1] != last_tail_line:
            last_tail_line = client.process.tail[-1]
            if last_tail_line.startswith("[godot-codex-index]"):
                print(last_tail_line, file=sys.stderr, flush=True)
        time.sleep(0.05)
    raise IndexMcpGateError(
        f"resource index did not become current; last_error={last_code}; "
        f"sidecar_tail={' | '.join(client.process.tail[-10:])}"
    )


def paged_query(
    client: McpClient,
    tool: str,
    result_key: str,
    resource: str,
    query_samples: list[float],
) -> tuple[dict[str, Any], list[dict[str, Any]], list[dict[str, Any]]]:
    cursor: str | None = None
    metadata: dict[str, Any] | None = None
    records: list[dict[str, Any]] = []
    diagnostics: dict[tuple[str, str, str | None], dict[str, Any]] = {}
    while True:
        arguments: dict[str, Any] = {"resource": resource, "limit": 1}
        if cursor is not None:
            arguments["cursor"] = cursor
        content, is_error, elapsed_ms = tool_call(client, tool, arguments)
        query_samples.append(round(elapsed_ms, 3))
        if is_error:
            raise IndexMcpGateError(f"{tool} returned {content.get('error', {}).get('code')}")
        page_metadata: dict[str, Any] = {
            "project_id": content.get("project_id"),
            "generation_id": content.get("generation_id"),
            "index_revision": content.get("index_revision"),
            "checkpoint": content.get("validated_checkpoint"),
            "resource": content.get("resource"),
        }
        if metadata is None:
            metadata = page_metadata
        elif page_metadata != metadata:
            raise IndexMcpGateError(f"{tool} pagination mixed index generations")
        page = content.get(result_key)
        if not isinstance(page, list):
            raise IndexMcpGateError(f"{tool} omitted {result_key}")
        records.extend(page)
        page_diagnostics = content.get("diagnostics")
        if not isinstance(page_diagnostics, list):
            raise IndexMcpGateError(f"{tool} omitted diagnostics")
        for diagnostic in page_diagnostics:
            if not isinstance(diagnostic, dict):
                raise IndexMcpGateError(f"{tool} returned a malformed diagnostic")
            code = diagnostic.get("code")
            subject = diagnostic.get("subject")
            detail = diagnostic.get("detail")
            first_revision = diagnostic.get("first_index_revision")
            last_revision = diagnostic.get("last_index_revision")
            if (
                not isinstance(code, str)
                or not isinstance(subject, str)
                or (detail is not None and not isinstance(detail, str))
                or not isinstance(first_revision, int)
                or isinstance(first_revision, bool)
                or not isinstance(last_revision, int)
                or isinstance(last_revision, bool)
                or first_revision <= 0
                or last_revision < first_revision
                or last_revision > int(page_metadata["index_revision"])
            ):
                raise IndexMcpGateError(f"{tool} returned an invalid diagnostic")
            key = (code, subject, detail)
            previous = diagnostics.setdefault(key, diagnostic)
            if previous != diagnostic:
                raise IndexMcpGateError(f"{tool} changed a diagnostic across pages")
        cursor = content.get("next_cursor")
        if cursor is None:
            if content.get("truncated") is not False:
                raise IndexMcpGateError(f"{tool} final page remained truncated")
            break
        if not isinstance(cursor, str) or content.get("truncated") is not True:
            raise IndexMcpGateError(f"{tool} pagination cursor is inconsistent")
    assert metadata is not None
    return metadata, records, [diagnostics[key] for key in sorted(diagnostics)]


def verify_oracle(
    client: McpClient,
    oracle: dict[str, Any],
    query_samples: list[float],
) -> dict[str, Any]:
    resources = {resource["oracle_id"]: resource for resource in oracle["resources"]}
    direct_expected = oracle["expected_direct_queries"]
    all_dependencies: dict[str, dict[str, Any]] = {}
    all_diagnostics: dict[tuple[str, str, str | None], dict[str, Any]] = {}
    generation_id: str | None = None
    index_revision: int | None = None
    project_id: str | None = None
    for oracle_id, resource in sorted(resources.items()):
        metadata, dependencies, diagnostics = paged_query(
            client,
            "godot_get_resource_dependencies",
            "dependencies",
            selector(resource),
            query_samples,
        )
        if generation_id is None:
            generation_id = str(metadata["generation_id"])
            index_revision = int(metadata["index_revision"])
            project_id = str(metadata["project_id"])
        elif generation_id != metadata["generation_id"] or index_revision != metadata["index_revision"]:
            raise IndexMcpGateError("oracle verification mixed active index generations")
        elif project_id != metadata["project_id"]:
            raise IndexMcpGateError("oracle verification mixed project bindings")
        observed_resource = metadata["resource"]
        if (
            not isinstance(observed_resource, dict)
            or observed_resource.get("entity_id") != resource["entity_id"]
            or observed_resource.get("uid") != resource.get("uid")
            or observed_resource.get("path") != resource["path"]
            or observed_resource.get("type") != resource["type"]
            or observed_resource.get("import_state") != resource["import_state"]
        ):
            raise IndexMcpGateError(f"resource identity or metadata differs for {oracle_id}")
        observed_ids = [dependency.get("edge_id") for dependency in dependencies]
        if observed_ids != direct_expected[oracle_id]:
            raise IndexMcpGateError(f"direct dependencies differ for {oracle_id}")
        for dependency in dependencies:
            edge_id = dependency.get("edge_id")
            if edge_id in all_dependencies:
                raise IndexMcpGateError("dependency appeared in more than one direct query")
            all_dependencies[str(edge_id)] = dependency
        for diagnostic in diagnostics:
            key = (
                str(diagnostic["code"]),
                str(diagnostic["subject"]),
                diagnostic.get("detail"),
            )
            previous = all_diagnostics.setdefault(key, diagnostic)
            if previous != diagnostic:
                raise IndexMcpGateError("diagnostic changed across resource queries")

    expected_by_id = {dependency["edge_id"]: dependency for dependency in oracle["dependencies"]}
    if set(all_dependencies) != set(expected_by_id):
        raise IndexMcpGateError("stored direct graph differs from the oracle")
    for edge_id, expected in expected_by_id.items():
        observed = all_dependencies[edge_id]
        if (
            observed.get("declared_type") != expected.get("declared_type")
            or observed.get("target_uid") != expected.get("target_uid")
            or observed.get("target_path") != expected["fallback_path"]
            or observed.get("resolution") != expected["resolution"]
            or observed.get("authority") != "godot_resource_loader"
        ):
            raise IndexMcpGateError(f"dependency fact differs for {edge_id}")

    expected_reverse: dict[str, list[str]] = {resource["entity_id"]: [] for resource in resources.values()}
    resources_by_entity = {resource["entity_id"]: resource for resource in resources.values()}
    for dependency in oracle["dependencies"]:
        target = dependency.get("resolved_target_entity_id")
        if target is not None:
            expected_reverse[target].append(dependency["edge_id"])
    for resource in resources.values():
        metadata, owners, diagnostics = paged_query(
            client,
            "godot_find_resource_owners",
            "owners",
            selector(resource),
            query_samples,
        )
        if (
            metadata["project_id"] != project_id
            or metadata["generation_id"] != generation_id
            or metadata["index_revision"] != index_revision
        ):
            raise IndexMcpGateError("reverse query escaped the pinned project generation")
        observed_owner_ids = [owner.get("edge_id") for owner in owners]
        expected = sorted(
            expected_reverse[resource["entity_id"]],
            key=lambda edge_id: (
                resources_by_entity[expected_by_id[edge_id]["source_entity_id"]]["path"],
                resources_by_entity[expected_by_id[edge_id]["source_entity_id"]].get("uid") is None,
                resources_by_entity[expected_by_id[edge_id]["source_entity_id"]].get("uid") or "",
                expected_by_id[edge_id]["source_entity_id"],
                edge_id,
            ),
        )
        if observed_owner_ids != expected:
            raise IndexMcpGateError(f"reverse owners differ for {resource['oracle_id']}")
        for diagnostic in diagnostics:
            key = (
                str(diagnostic["code"]),
                str(diagnostic["subject"]),
                diagnostic.get("detail"),
            )
            previous = all_diagnostics.setdefault(key, diagnostic)
            if previous != diagnostic:
                raise IndexMcpGateError("diagnostic changed across direct/reverse queries")

    expected_diagnostics = {
        (
            diagnostic["code"],
            resources[diagnostic["source"]]["entity_id"],
            diagnostic["target_reference"],
        )
        for diagnostic in oracle["diagnostics"]
    }
    if set(all_diagnostics) != expected_diagnostics:
        raise IndexMcpGateError("stored diagnostics differ from the oracle")

    serialized = json.dumps(all_dependencies, sort_keys=True)
    if str(PROJECT_SOURCE.resolve()) in serialized or "/tmp/" in serialized:
        raise IndexMcpGateError("MCP resource output exposed an absolute project path")
    normalized_graph = [
        {
            "edge_id": edge_id,
            "source_entity_id": expected_by_id[edge_id]["source_entity_id"],
            "declared_type": dependency.get("declared_type"),
            "target_uid": dependency.get("target_uid"),
            "target_path": dependency.get("target_path"),
            "target_entity_id": (
                dependency.get("target", {}).get("entity_id") if isinstance(dependency.get("target"), dict) else None
            ),
            "resolution": dependency.get("resolution"),
            "authority": dependency.get("authority"),
        }
        for edge_id, dependency in sorted(all_dependencies.items())
    ]
    graph_digest = hashlib.sha256(
        json.dumps(normalized_graph, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode("utf-8")
    ).hexdigest()
    return {
        "generation_id": generation_id,
        "index_revision": index_revision,
        "resource_count": len(resources),
        "dependency_count": len(all_dependencies),
        "diagnostic_count": len(all_diagnostics),
        "diagnostic_codes": sorted({key[0] for key in all_diagnostics}),
        "diagnostics_oracle_match": True,
        "normalized_graph_sha256": f"sha256:{graph_digest}",
        "direct_reverse_oracle_match": True,
        "stable_project_scope": isinstance(project_id, str) and project_id.startswith("project:sha256:"),
    }


def verify_current_oracle(
    client: McpClient,
    current: dict[str, Any],
    oracle: dict[str, Any],
    query_samples: list[float],
    context: str,
) -> dict[str, Any]:
    """Verify and bind an oracle result to the exact generation that ended a wait."""
    result = verify_oracle(client, oracle, query_samples)
    if result["generation_id"] != current.get("generation_id") or result["index_revision"] != current.get(
        "index_revision"
    ):
        raise IndexMcpGateError(f"{context} oracle verification crossed index generations")
    return result


def mutate_phase(
    phase: str,
    godot: Path,
    project: Path,
    prepared: Path,
    marker_index: int | None = None,
) -> None:
    if phase == "journal_gap":
        create_gap_resources(project)
    elif phase == "re_add":
        resource = project / "resources" / "unique_leaf.res"
        if marker_index == 1:
            resource.unlink()
        elif marker_index == 2:
            shutil.copy2(PROJECT_SOURCE / "resources" / "unique_leaf.res", resource)
        else:
            raise IndexMcpGateError("re_add requires an explicit mutation index")
    else:
        restore_project(prepared)
        apply_phase(prepared, phase, godot if phase == "rename_uid" else None)
        apply_prepared_diff(project, prepared)
    marker = project / ".godot" / "codex-resource-live-mutate"
    if marker_index is not None:
        marker = marker.with_name(marker.name + f"-{marker_index}")
    marker.touch()


def parse_bridge_telemetry(log_text: str) -> dict[str, Any]:
    try:
        records = [
            json.loads(line.removeprefix(TELEMETRY_PREFIX))
            for line in log_text.splitlines()
            if line.startswith(TELEMETRY_PREFIX)
        ]
    except json.JSONDecodeError as error:
        raise IndexMcpGateError("editor emitted malformed Bridge telemetry") from error
    if len(records) != 1 or not isinstance(records[0], dict):
        raise IndexMcpGateError("editor did not emit exactly one Bridge telemetry record")
    record = records[0]
    required = {
        "schema_version",
        "budget_usec",
        "sample_capacity",
        "busy_frame_count",
        "samples_usec",
        "max_elapsed_usec",
        "over_budget_count",
        "overflow",
    }
    if (
        set(record) != required
        or record.get("schema_version") != 1
        or record.get("budget_usec") != 2000
        or not isinstance(record.get("sample_capacity"), int)
        or isinstance(record.get("sample_capacity"), bool)
        or record.get("sample_capacity") != BRIDGE_TELEMETRY_SAMPLE_CAPACITY
    ):
        raise IndexMcpGateError("Bridge telemetry contract differs")
    samples = record.get("samples_usec")
    if (
        not isinstance(samples, list)
        or not samples
        or len(samples) > BRIDGE_TELEMETRY_SAMPLE_CAPACITY
        or not all(isinstance(value, int) and not isinstance(value, bool) and value >= 0 for value in samples)
    ):
        raise IndexMcpGateError("Bridge telemetry samples are empty or invalid")
    busy_count = record.get("busy_frame_count")
    over_budget = record.get("over_budget_count")
    maximum = record.get("max_elapsed_usec")
    if not all(
        isinstance(value, int) and not isinstance(value, bool) and value >= 0
        for value in (busy_count, over_budget, maximum)
    ) or not isinstance(record.get("overflow"), bool):
        raise IndexMcpGateError("Bridge telemetry counters are invalid")
    if record.get("overflow") is not False:
        raise IndexMcpGateError("Bridge telemetry overflowed")
    if busy_count != len(samples):
        raise IndexMcpGateError("Bridge telemetry omitted a busy frame")
    if over_budget != sum(value > 2000 for value in samples) or maximum != max(samples):
        raise IndexMcpGateError("Bridge telemetry summary contradicts raw samples")
    return record


def run_phase(
    phase: str,
    godot: Path,
    sidecar: Path,
    run_root: Path,
    oracles: dict[str, dict[str, Any]],
    timeout: float,
    mcp_contract_state: dict[str, Any],
) -> dict[str, Any]:
    phase_root = run_root / f"p{PHASES.index(phase)}"
    project = phase_root / "p"
    prepared = phase_root / "m"
    phase_root.mkdir(parents=True)
    shutil.copytree(PROJECT_SOURCE, project, ignore=shutil.ignore_patterns(".godot"))
    mutation_count = 2 if phase == "re_add" else 1
    telemetry_log_path = phase_root / "godot.log"
    log_paths = [telemetry_log_path]
    editor, editor_log = start_editor(
        godot,
        project,
        telemetry_log_path,
        mutation_count,
        evidence_telemetry=True,
    )
    mcp: LineProcess | None = None
    phase_result: dict[str, Any] = {}
    runtime_paths: list[Path] = []
    query_samples: list[float] = []
    bulk_status_samples: list[float] = []
    try:
        discovery = project / ".godot" / "codex" / "bridge.json"
        wait_for(discovery, timeout, f"{phase} discovery")
        discovery_record = strict_json_load(discovery)
        runtime_paths = [
            discovery,
            project / ".godot" / "codex" / "session.token",
            project / ".godot" / "codex" / "bridge.lock",
        ]
        if discovery_record.get("transport") == "uds":
            endpoint = Path(str(discovery_record.get("endpoint", "")))
            if endpoint.is_absolute() or ".." in endpoint.parts:
                raise IndexMcpGateError("bridge discovery exposed an unsafe UDS endpoint")
            runtime_paths.append(project / endpoint)
        mcp, client = initialize_sidecar(sidecar, project, timeout)
        base_resource = selector(
            next(resource for resource in oracles["base"]["resources"] if resource["oracle_id"] == "fan_out")
        )
        current, startup_ms = wait_for_current(client, base_resource, timeout)
        base_generation = str(current["generation_id"])
        base_revision = int(current["index_revision"])
        generation_probe_cursor: str | None = None

        previous_cursor = mcp_contract_state.get("_cross_project_cursor")
        if phase != "base" and isinstance(previous_cursor, str):
            previous_project = mcp_contract_state.get("_cross_project_id")
            if current.get("project_id") == previous_project:
                raise IndexMcpGateError("phase isolation reused the previous project binding")
            require_tool_error(
                client,
                "godot_get_resource_dependencies",
                {"resource": base_resource, "limit": 1, "cursor": previous_cursor},
                "stale_cursor",
            )
            mcp_contract_state["cross_project_cursor_rejected"] = True
            mcp_contract_state.pop("_cross_project_cursor", None)
            mcp_contract_state.pop("_cross_project_id", None)

        if phase == "rename_uid":
            generation_probe_cursor, cursor_project = issue_probe_cursor(
                client,
                "godot_get_resource_dependencies",
                base_resource,
            )
            if cursor_project != current.get("project_id"):
                raise IndexMcpGateError("generation probe cursor escaped the current project")

        if phase == "base":
            contract, cross_project_cursor, cross_project_id = probe_base_mcp_contract(client, base_resource)
            mcp_contract_state.update(contract)
            mcp_contract_state["_cross_project_cursor"] = cross_project_cursor
            mcp_contract_state["_cross_project_id"] = cross_project_id
            base_result = verify_oracle(client, oracles["base"], query_samples)
            mcp_contract_state["exact_ordering"] = base_result["direct_reverse_oracle_match"] is True
            mcp_contract_state["stable_project_scope"] = base_result["stable_project_scope"] is True
            segment_root = project / ".godot" / "codex" / "index" / "segments"
            segments_before = sorted(
                (path.relative_to(segment_root).as_posix(), sha256_file(path))
                for path in segment_root.rglob("*")
                if path.is_file()
            )
            if not segments_before:
                raise IndexMcpGateError("compatible reopen had no committed segment artifacts")
            initial_editor_session = discovery_record.get("editor_session_id")
            close_sidecar(mcp)
            mcp = None
            stop_editor(editor, project)
            editor_log.close()
            if editor.poll() is None:
                raise IndexMcpGateError("base editor did not stop before reopen")
            remaining_before_reopen = [path.name for path in runtime_paths if path.exists()]
            if remaining_before_reopen:
                raise IndexMcpGateError(
                    "base editor left runtime artifacts before reopen: " + ", ".join(remaining_before_reopen)
                )
            (project / ".godot" / "codex-resource-live-done").unlink(missing_ok=True)

            reopen_started = time.monotonic()
            telemetry_log_path = phase_root / "godot-reopen.log"
            log_paths.append(telemetry_log_path)
            editor, editor_log = start_editor(
                godot,
                project,
                telemetry_log_path,
                mutation_count,
                evidence_telemetry=True,
            )
            wait_for(discovery, timeout, "base reopened discovery")
            reopened_discovery = strict_json_load(discovery)
            if reopened_discovery.get("editor_session_id") == initial_editor_session:
                raise IndexMcpGateError("editor reopen did not create a new session")
            if reopened_discovery.get("transport") == "uds":
                endpoint = Path(str(reopened_discovery.get("endpoint", "")))
                if endpoint.is_absolute() or ".." in endpoint.parts:
                    raise IndexMcpGateError("reopened bridge exposed an unsafe UDS endpoint")
                reopened_endpoint = project / endpoint
                if reopened_endpoint not in runtime_paths:
                    runtime_paths.append(reopened_endpoint)
            mcp, client = initialize_sidecar(sidecar, project, timeout)
            reopened, _ = wait_for_current(client, base_resource, timeout)
            reopen_ms = round((time.monotonic() - reopen_started) * 1000, 3)
            reopened_result = verify_oracle(client, oracles["base"], query_samples)
            segments_after = sorted(
                (path.relative_to(segment_root).as_posix(), sha256_file(path))
                for path in segment_root.rglob("*")
                if path.is_file()
            )
            if (
                reopened.get("generation_id") != base_generation
                or reopened.get("index_revision") != base_revision
                or segments_after != segments_before
                or reopened_result["normalized_graph_sha256"] != base_result["normalized_graph_sha256"]
            ):
                raise IndexMcpGateError("editor/sidecar reopen rebuilt or changed the committed index")
            status_content, status_error, _ = tool_call(client, "godot_get_editor_state", {})
            if status_error or status_content.get("freshness") != "current":
                raise IndexMcpGateError("status/ping did not remain current after editor/sidecar reopen")
            (project / ".godot" / "codex-resource-live-mutate").touch()
            phase_result.update({
                "phase": phase,
                **base_result,
                "startup_visibility_ms": startup_ms,
                "reopen_visibility_ms": reopen_ms,
                "editor_reopened": True,
                "sidecar_reopened": True,
                "same_generation_after_reopen": True,
                "immutable_segment_set_reused": True,
                "passed": True,
            })
            return phase_result

        if phase == "re_add":
            mutate_phase(phase, godot, project, prepared, 1)
            removal_started = time.monotonic()
            removed, _ = wait_for_current(client, base_resource, timeout, base_generation)
            verify_current_oracle(
                client,
                removed,
                oracles["delete"],
                query_samples,
                "re_add removal",
            )
            removal_ms = round((time.monotonic() - removal_started) * 1000, 3)
            mutate_phase(phase, godot, project, prepared, 2)
            visibility_started = time.monotonic()
            final, _ = wait_for_current(
                client,
                base_resource,
                timeout,
                str(removed["generation_id"]),
            )
        else:
            mutate_phase(phase, godot, project, prepared)
            visibility_started = time.monotonic()
            final, _ = wait_for_current(
                client,
                base_resource,
                timeout,
                base_generation,
                bulk_status_samples if phase == "journal_gap" else None,
            )
        if phase == "journal_gap" and final.get("validated_checkpoint", {}).get("last_batch_id") is not None:
            raise IndexMcpGateError("journal gap did not activate a full-snapshot checkpoint")
        if int(final["index_revision"]) <= base_revision:
            raise IndexMcpGateError(f"{phase} did not advance the durable index revision")
        result = verify_current_oracle(client, final, oracles[phase], query_samples, phase)
        visibility_ms = round((time.monotonic() - visibility_started) * 1000, 3)
        if generation_probe_cursor is not None:
            require_tool_error(
                client,
                "godot_get_resource_dependencies",
                {"resource": base_resource, "limit": 1, "cursor": generation_probe_cursor},
                "stale_cursor",
            )
            mcp_contract_state["stale_generation_cursor_rejected"] = True
        status_content, status_error, _ = tool_call(client, "godot_get_editor_state", {})
        if status_error or status_content.get("freshness") != "current":
            raise IndexMcpGateError(f"{phase} editor status was not current")
        phase_result.update({
            "phase": phase,
            **result,
            "startup_generation": base_generation,
            "startup_index_revision": base_revision,
            "startup_visibility_ms": startup_ms,
            "change_visibility_ms": visibility_ms,
            "generation_advanced": result["generation_id"] != base_generation,
            "passed": True,
        })
        if phase == "re_add":
            phase_result["removal_visibility_ms"] = removal_ms
        if phase == "rename_uid":
            base_by_uid = {
                resource["uid"]: resource
                for resource in oracles["base"]["resources"]
                if resource.get("uid") is not None
            }
            renamed = [
                resource
                for resource in oracles[phase]["resources"]
                if resource.get("uid") in base_by_uid and resource["path"] != base_by_uid[resource["uid"]]["path"]
            ]
            incremental_commit_count = int(final["index_revision"]) - base_revision
            identity_preserved = (
                len(renamed) == 1 and renamed[0]["entity_id"] == base_by_uid[renamed[0]["uid"]]["entity_id"]
            )
            incremental_checkpoint = final.get("validated_checkpoint", {}).get("last_batch_id") is not None
            if not identity_preserved or not incremental_checkpoint or incremental_commit_count != 1:
                raise IndexMcpGateError("UID rename did not preserve identity in one incremental commit")
            phase_result["entity_identity_preserved"] = True
            phase_result["full_rebuild_count_delta"] = 0
            phase_result["incremental_commit_count"] = incremental_commit_count
        if phase == "journal_gap":
            phase_result["full_rebuild_after_gap"] = True
        return phase_result
    finally:
        if mcp is not None and mcp.process.poll() is None:
            mcp.stop()
        stop_editor(editor, project)
        editor_log.close()
        if editor.poll() is None or (mcp is not None and mcp.process.poll() is None):
            raise IndexMcpGateError(f"{phase} left a live editor or sidecar process")
        remaining_runtime_paths = [path.name for path in runtime_paths if path.exists()]
        if remaining_runtime_paths:
            raise IndexMcpGateError(f"{phase} editor left runtime artifacts: {', '.join(remaining_runtime_paths)}")
        log_texts = [path.read_text(encoding="utf-8", errors="replace") for path in log_paths]
        if any("SCRIPT ERROR" in log_text for log_text in log_texts):
            raise IndexMcpGateError(f"{phase} editor reported a script error")
        phase_result["bridge_main_thread_sessions"] = [parse_bridge_telemetry(log_text) for log_text in log_texts]
        phase_result["cached_resource_query_samples_ms"] = query_samples
        phase_result["bulk_status_ping_samples_ms"] = bulk_status_samples
        phase_result["cleanup_verified"] = True


def metric_summary(samples: list[float] | list[int]) -> dict[str, Any]:
    return {
        "samples": samples,
        "p50": percentile(samples, 50) if samples else None,
        "p95": percentile(samples, 95) if samples else None,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", required=True, type=Path)
    parser.add_argument("--sidecar", type=Path)
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--timeout", default=120.0, type=float)
    parser.add_argument(
        "--phases",
        default=",".join(PHASES),
        help="comma-separated local phase subset; canonical evidence uses all phases",
    )
    arguments = parser.parse_args()
    selected_phases = tuple(phase for phase in arguments.phases.split(",") if phase)
    if (
        not selected_phases
        or any(phase not in PHASES for phase in selected_phases)
        or len(set(selected_phases)) != len(selected_phases)
    ):
        raise IndexMcpGateError("--phases contains an unknown oracle phase")
    complete_profile = selected_phases == PHASES
    require_local_host_for_qualifying_evidence(complete_profile)
    platform_tag = target_platform()
    godot = arguments.godot.resolve(strict=True)
    sidecar = (arguments.sidecar or default_sidecar()).resolve()
    if godot != default_godot(platform_tag).resolve():
        raise IndexMcpGateError("qualifying evidence requires the canonical local Godot build target")
    if sidecar != default_sidecar().resolve():
        raise IndexMcpGateError("qualifying evidence requires the canonical release sidecar target")
    evidence_path = arguments.evidence or default_evidence(platform_tag)
    build_sidecar(sidecar)
    sidecar = sidecar.resolve(strict=True)
    source = source_coordinates()
    toolchain = toolchain_coordinates()
    godot_version = version_output(godot)
    if source["git_commit"][:9] not in godot_version:
        raise IndexMcpGateError("the Godot artifact was not built from the evidence commit")
    sidecar_version = version_output(sidecar)
    golden = strict_json_load(GOLDEN_PATH)
    oracles = {phase["name"]: phase for phase in golden["phases"]}
    base_resources = oracles["base"]["resources"]
    observed_formats = {Path(resource["path"]).suffix for resource in base_resources}
    format_import_matrix_verified = {".tres", ".res", ".tscn", ".scn", ".svg"}.issubset(observed_formats) and any(
        resource.get("imported") is True for resource in base_resources
    )
    if not format_import_matrix_verified:
        raise IndexMcpGateError("the canonical format/import matrix is incomplete")
    before = fixture_digest()
    mcp_contract_state: dict[str, Any] = {}
    temporary_parent = "/tmp" if platform_tag == "macos-arm64" else None
    temporary_path: Path | None = None
    with tempfile.TemporaryDirectory(prefix="cs5-", dir=temporary_parent) as temporary:
        temporary_path = Path(temporary)
        phase_results = [
            run_phase(
                phase,
                godot,
                sidecar,
                Path(temporary),
                oracles,
                arguments.timeout,
                mcp_contract_state,
            )
            for phase in selected_phases
        ]
    temporary_workspace_removed = temporary_path is not None and not temporary_path.exists()
    if not temporary_workspace_removed:
        raise IndexMcpGateError("the temporary live-gate workspace was not removed")
    after = fixture_digest()
    if before != after:
        raise IndexMcpGateError("canonical resource fixture changed during the live gate")
    telemetry_records = [record for phase in phase_results for record in phase["bridge_main_thread_sessions"]]
    bridge_samples = [sample for record in telemetry_records for sample in record["samples_usec"]]
    bridge_over_budget = sum(int(record["over_budget_count"]) for record in telemetry_records)
    bridge_overflow = any(bool(record["overflow"]) for record in telemetry_records)
    query_samples = [sample for phase in phase_results for sample in phase["cached_resource_query_samples_ms"]]
    bulk_status_samples = [sample for phase in phase_results for sample in phase["bulk_status_ping_samples_ms"]]
    incremental_visibility_samples = [
        sample
        for phase in phase_results
        if phase["phase"] not in {"base", "journal_gap"}
        for sample in (
            [phase["removal_visibility_ms"], phase["change_visibility_ms"]]
            if phase["phase"] == "re_add"
            else [phase["change_visibility_ms"]]
        )
    ]
    startup_samples = [phase["startup_visibility_ms"] for phase in phase_results]
    reopen_samples = [phase["reopen_visibility_ms"] for phase in phase_results if phase["phase"] == "base"]
    rebuild_samples = [phase["change_visibility_ms"] for phase in phase_results if phase["phase"] == "journal_gap"]
    metrics_ms = {
        "cached_resource_query": metric_summary(query_samples),
        "ordinary_incremental_visibility": metric_summary(incremental_visibility_samples),
        "bulk_status_ping": metric_summary(bulk_status_samples),
        "startup_visibility": metric_summary(startup_samples),
        "compatible_reopen": metric_summary(reopen_samples),
        "journal_gap_full_rebuild": metric_summary(rebuild_samples),
    }
    main_thread_metric = {
        **metric_summary(bridge_samples),
        "unit": "microseconds",
        "budget_usec": 2000,
        "busy_frame_count": sum(int(record["busy_frame_count"]) for record in telemetry_records),
        "max_elapsed_usec": max((int(record["max_elapsed_usec"]) for record in telemetry_records), default=0),
        "over_budget_count": bridge_over_budget,
        "overflow": bridge_overflow,
    }
    slo = {
        "cached_resource_query_p95_lte_300ms": bool(query_samples)
        and metrics_ms["cached_resource_query"]["p95"] <= 300,
        "ordinary_incremental_visibility_p95_lte_2000ms": bool(incremental_visibility_samples)
        and metrics_ms["ordinary_incremental_visibility"]["p95"] <= 2000,
        "bulk_status_ping_p95_lte_200ms": bool(bulk_status_samples) and metrics_ms["bulk_status_ping"]["p95"] <= 200,
        "bridge_main_thread_over_2000us_zero": bool(bridge_samples) and bridge_over_budget == 0 and not bridge_overflow,
    }
    slo["all_passed"] = all(slo.values())
    completed_phases = [phase["phase"] for phase in phase_results]
    mcp_contract = {field: mcp_contract_state.get(field) is True for field in sorted(MCP_CONTRACT_FIELDS)}
    resource_mcp_contract_verified = all(mcp_contract.values()) and all(
        phase.get("passed") is True
        and phase.get("direct_reverse_oracle_match") is True
        and phase.get("diagnostics_oracle_match") is True
        for phase in phase_results
    )
    qualifying_source = complete_profile and not source["git_dirty"]
    status = (
        "passed"
        if qualifying_source and slo["all_passed"] and resource_mcp_contract_verified
        else "failed"
        if complete_profile
        else "development"
    )
    cleanup_verified = all(phase.get("cleanup_verified") is True for phase in phase_results)
    evidence = {
        "schema_version": 3,
        "stage": "Sprint 3 Stage 5 / S3-09-S3-10",
        "status": status,
        "execution": "local_model_free",
        "profile": "acceptance" if complete_profile else "development",
        "platform": platform_tag,
        "host": platform.platform(),
        **source,
        "toolchain": toolchain,
        "artifacts": {
            "godot_version": godot_version,
            "godot_sha256": sha256_file(godot),
            "sidecar_version": sidecar_version,
            "sidecar_sha256": sha256_file(sidecar),
        },
        "mcp_protocol": MCP_PROTOCOL,
        "tools": sorted(TOOL_NAMES),
        "oracle_sha256": sha256_file(GOLDEN_PATH),
        "fixture_digest_before": before,
        "fixture_digest_after": after,
        "canonical_fixture_unchanged": True,
        "phases": phase_results,
        "mcp_contract": mcp_contract,
        "metrics_ms": metrics_ms,
        "bridge_main_thread": main_thread_metric,
        "slo": slo,
        "completion": {
            "requested_phases": list(selected_phases),
            "completed_phases": completed_phases,
            "all_phases_completed": complete_profile and completed_phases == list(PHASES),
            "format_import_matrix_verified": format_import_matrix_verified,
            "resource_mcp_contract_verified": resource_mcp_contract_verified,
        },
        "cleanup": {
            "editor_processes_stopped": cleanup_verified,
            "sidecar_processes_stopped": cleanup_verified,
            "bridge_runtime_files_absent": cleanup_verified,
            "temporary_workspace_removed": temporary_workspace_removed,
        },
        "redaction": {field: False for field in REDACTION_FIELDS},
        "platform_evidence": {
            "remote_ci": "not_run",
        },
    }
    evidence["redaction"] = redaction_status(evidence)
    require_redacted(evidence, "live evidence")
    serialized = canonical_json(evidence)
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    temporary_evidence_path = evidence_path.with_suffix(evidence_path.suffix + ".tmp")
    temporary_evidence_path.write_text(serialized, encoding="utf-8")
    os.replace(temporary_evidence_path, evidence_path)
    print(serialized, end="")
    if complete_profile and status != "passed":
        reason = (
            "source tree is dirty"
            if source["git_dirty"]
            else "the live MCP contract failed"
            if not resource_mcp_contract_verified
            else "one or more live SLOs failed"
        )
        raise IndexMcpGateError(f"Sprint 3 live acceptance failed ({reason}); evidence={evidence_path}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except IndexMcpGateError as error:
        print(f"FAIL Sprint 3 index/MCP gate: {error}", file=sys.stderr)
        raise SystemExit(1) from error
