#!/usr/bin/env python3
"""Run the local model-free Sprint 3 persistent-index and resource MCP gate."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

from resource_graph_fixture import (
    GOLDEN_PATH,
    PROJECT_SOURCE,
    apply_phase,
    restore_project,
    strict_json_load,
)
from sprint2_live_smoke import LineProcess, MCP_PROTOCOL, McpClient
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
DEFAULT_SIDECAR = REPOSITORY_ROOT / "godot-codex-mcp" / "target" / "release" / "godot-codex-mcp"
DEFAULT_EVIDENCE = SCRIPT_DIR / "evidence" / "sprint-3-stage-4-index-mcp-macos.json"
TOOL_NAMES = {
    "godot_get_current_scene",
    "godot_get_editor_state",
    "godot_get_selected_nodes",
    "godot_get_resource_dependencies",
    "godot_find_resource_owners",
}


class IndexMcpGateError(RuntimeError):
    """Raised when persistent index or MCP output differs from the oracle."""


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def percentile(samples: list[float], percentile_value: float) -> float:
    if not samples:
        raise IndexMcpGateError("cannot calculate a percentile without samples")
    ordered = sorted(samples)
    index = max(0, min(len(ordered) - 1, int((len(ordered) - 1) * percentile_value + 0.5)))
    return round(ordered[index], 3)


def build_sidecar(sidecar: Path) -> None:
    result = subprocess.run(
        [
            "cargo",
            "build",
            "--locked",
            "--release",
            "--manifest-path",
            str(REPOSITORY_ROOT / "godot-codex-mcp" / "Cargo.toml"),
            "-p",
            "godot-codex-mcp",
        ],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=300,
    )
    if result.returncode != 0 or not sidecar.is_file():
        raise IndexMcpGateError("could not build the release sidecar")


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
    if response.get("result", {}).get("protocolVersion") != MCP_PROTOCOL:
        process.stop()
        raise IndexMcpGateError("MCP protocol version differs")
    client.notify("notifications/initialized", {})
    listed = client.request("tools/list", {})
    tools = listed.get("result", {}).get("tools", [])
    if {tool.get("name") for tool in tools} != TOOL_NAMES:
        process.stop()
        raise IndexMcpGateError("MCP tool set differs from the frozen five-tool contract")
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
    if "error" in response:
        raise IndexMcpGateError(f"MCP protocol error from {name}")
    result = response.get("result", {})
    content = result.get("structuredContent")
    if not isinstance(content, dict):
        raise IndexMcpGateError(f"{name} omitted structuredContent")
    return content, result.get("isError") is True, elapsed_ms


def selector(resource: dict[str, Any]) -> str:
    uid = resource.get("uid")
    return uid if isinstance(uid, str) else resource["path"]


def wait_for_current(
    client: McpClient,
    resource: str,
    timeout: float,
    previous_generation: str | None = None,
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
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    cursor: str | None = None
    metadata: dict[str, Any] | None = None
    records: list[dict[str, Any]] = []
    while True:
        arguments: dict[str, Any] = {"resource": resource, "limit": 1}
        if cursor is not None:
            arguments["cursor"] = cursor
        content, is_error, elapsed_ms = tool_call(client, tool, arguments)
        query_samples.append(round(elapsed_ms, 3))
        if is_error:
            raise IndexMcpGateError(f"{tool} returned {content.get('error', {}).get('code')}")
        page_metadata = {
            "project_id": content.get("project_id"),
            "generation_id": content.get("generation_id"),
            "index_revision": content.get("index_revision"),
            "checkpoint": content.get("validated_checkpoint"),
        }
        if metadata is None:
            metadata = page_metadata
        elif page_metadata != metadata:
            raise IndexMcpGateError(f"{tool} pagination mixed index generations")
        page = content.get(result_key)
        if not isinstance(page, list):
            raise IndexMcpGateError(f"{tool} omitted {result_key}")
        records.extend(page)
        cursor = content.get("next_cursor")
        if cursor is None:
            if content.get("truncated") is not False:
                raise IndexMcpGateError(f"{tool} final page remained truncated")
            break
        if not isinstance(cursor, str) or content.get("truncated") is not True:
            raise IndexMcpGateError(f"{tool} pagination cursor is inconsistent")
    assert metadata is not None
    return metadata, records


def verify_oracle(
    client: McpClient,
    oracle: dict[str, Any],
    query_samples: list[float],
) -> dict[str, Any]:
    resources = {resource["oracle_id"]: resource for resource in oracle["resources"]}
    direct_expected = oracle["expected_direct_queries"]
    all_dependencies: dict[str, dict[str, Any]] = {}
    generation_id: str | None = None
    index_revision: int | None = None
    for oracle_id, resource in sorted(resources.items()):
        metadata, dependencies = paged_query(
            client,
            "godot_get_resource_dependencies",
            "dependencies",
            selector(resource),
            query_samples,
        )
        if generation_id is None:
            generation_id = str(metadata["generation_id"])
            index_revision = int(metadata["index_revision"])
        elif generation_id != metadata["generation_id"] or index_revision != metadata["index_revision"]:
            raise IndexMcpGateError("oracle verification mixed active index generations")
        observed_ids = [dependency.get("edge_id") for dependency in dependencies]
        if observed_ids != direct_expected[oracle_id]:
            raise IndexMcpGateError(f"direct dependencies differ for {oracle_id}")
        for dependency in dependencies:
            edge_id = dependency.get("edge_id")
            if edge_id in all_dependencies:
                raise IndexMcpGateError("dependency appeared in more than one direct query")
            all_dependencies[str(edge_id)] = dependency

    expected_by_id = {dependency["edge_id"]: dependency for dependency in oracle["dependencies"]}
    if set(all_dependencies) != set(expected_by_id):
        raise IndexMcpGateError("stored direct graph differs from the oracle")
    for edge_id, expected in expected_by_id.items():
        observed = all_dependencies[edge_id]
        if (
            observed.get("target_uid") != expected.get("target_uid")
            or observed.get("target_path") != expected["fallback_path"]
            or observed.get("resolution") != expected["resolution"]
            or observed.get("authority") != "godot_resource_loader"
        ):
            raise IndexMcpGateError(f"dependency fact differs for {edge_id}")

    expected_reverse: dict[str, list[str]] = {resource["entity_id"]: [] for resource in resources.values()}
    for dependency in oracle["dependencies"]:
        target = dependency.get("resolved_target_entity_id")
        if target is not None:
            expected_reverse[target].append(dependency["edge_id"])
    for resource in resources.values():
        _, owners = paged_query(
            client,
            "godot_find_resource_owners",
            "owners",
            selector(resource),
            query_samples,
        )
        observed = [owner.get("edge_id") for owner in owners]
        expected = sorted(expected_reverse[resource["entity_id"]], key=lambda edge_id: next(
            dependency["source_entity_id"]
            for dependency in oracle["dependencies"]
            if dependency["edge_id"] == edge_id
        ))
        if set(observed) != set(expected) or len(observed) != len(expected):
            raise IndexMcpGateError(f"reverse owners differ for {resource['oracle_id']}")

    serialized = json.dumps(all_dependencies, sort_keys=True)
    if str(PROJECT_SOURCE.resolve()) in serialized or "/tmp/" in serialized:
        raise IndexMcpGateError("MCP resource output exposed an absolute project path")
    return {
        "generation_id": generation_id,
        "index_revision": index_revision,
        "resource_count": len(resources),
        "dependency_count": len(all_dependencies),
        "diagnostic_count": len(oracle["diagnostics"]),
        "direct_reverse_oracle_match": True,
    }


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


def run_phase(
    phase: str,
    godot: Path,
    sidecar: Path,
    run_root: Path,
    oracles: dict[str, dict[str, Any]],
    timeout: float,
    query_samples: list[float],
    status_samples: list[float],
    visibility_samples: list[float],
) -> dict[str, Any]:
    phase_root = run_root / f"p{PHASES.index(phase)}"
    project = phase_root / "p"
    prepared = phase_root / "m"
    phase_root.mkdir(parents=True)
    shutil.copytree(PROJECT_SOURCE, project, ignore=shutil.ignore_patterns(".godot"))
    mutation_count = 2 if phase == "re_add" else 1
    editor, editor_log = start_editor(godot, project, phase_root / "godot.log", mutation_count)
    mcp: LineProcess | None = None
    try:
        wait_for(project / ".godot" / "codex" / "bridge.json", timeout, f"{phase} discovery")
        mcp, client = initialize_sidecar(sidecar, project, timeout)
        base_resource = selector(next(resource for resource in oracles["base"]["resources"] if resource["oracle_id"] == "fan_out"))
        current, startup_ms = wait_for_current(client, base_resource, timeout)
        visibility_samples.append(startup_ms)
        base_generation = str(current["generation_id"])
        base_revision = int(current["index_revision"])

        if phase == "base":
            base_result = verify_oracle(client, oracles["base"], query_samples)
            segments_before = sorted(
                path.name
                for path in (project / ".godot" / "codex" / "index" / "segments").glob("*.json")
            )
            close_sidecar(mcp)
            mcp, client = initialize_sidecar(sidecar, project, timeout)
            reopened, reopen_ms = wait_for_current(client, base_resource, timeout)
            visibility_samples.append(reopen_ms)
            segments_after = sorted(
                path.name
                for path in (project / ".godot" / "codex" / "index" / "segments").glob("*.json")
            )
            if (
                reopened.get("generation_id") != base_generation
                or reopened.get("index_revision") != base_revision
                or segments_after != segments_before
            ):
                raise IndexMcpGateError("same-session reopen rebuilt or changed the committed index")
            status_content, status_error, status_ms = tool_call(client, "godot_get_editor_state", {})
            status_samples.append(round(status_ms, 3))
            if status_error or status_content.get("freshness") != "current":
                raise IndexMcpGateError("status/ping did not remain current after sidecar reopen")
            (project / ".godot" / "codex-resource-live-mutate").touch()
            return {
                "phase": phase,
                **base_result,
                "startup_visibility_ms": startup_ms,
                "reopen_visibility_ms": reopen_ms,
                "same_generation_after_reopen": True,
                "immutable_segment_set_reused": True,
                "passed": True,
            }

        if phase == "re_add":
            mutate_phase(phase, godot, project, prepared, 1)
            removed, removal_ms = wait_for_current(client, base_resource, timeout, base_generation)
            visibility_samples.append(removal_ms)
            mutate_phase(phase, godot, project, prepared, 2)
            final, visibility_ms = wait_for_current(
                client,
                base_resource,
                timeout,
                str(removed["generation_id"]),
            )
            visibility_samples.append(visibility_ms)
        else:
            mutate_phase(phase, godot, project, prepared)
            final, visibility_ms = wait_for_current(client, base_resource, timeout, base_generation)
            visibility_samples.append(visibility_ms)
        if phase == "journal_gap" and final.get("validated_checkpoint", {}).get("last_batch_id") is not None:
            raise IndexMcpGateError("journal gap did not activate a full-snapshot checkpoint")
        if int(final["index_revision"]) <= base_revision:
            raise IndexMcpGateError(f"{phase} did not advance the durable index revision")
        result = verify_oracle(client, oracles[phase], query_samples)
        status_content, status_error, status_ms = tool_call(client, "godot_get_editor_state", {})
        status_samples.append(round(status_ms, 3))
        if status_error or status_content.get("freshness") != "current":
            raise IndexMcpGateError(f"{phase} editor status was not current")
        phase_result = {
            "phase": phase,
            **result,
            "startup_generation": base_generation,
            "startup_index_revision": base_revision,
            "change_visibility_ms": visibility_ms,
            "generation_advanced": result["generation_id"] != base_generation,
            "passed": True,
        }
        if phase == "journal_gap":
            phase_result["full_rebuild_after_gap"] = True
        return phase_result
    finally:
        if mcp is not None and mcp.process.poll() is None:
            mcp.stop()
        stop_editor(editor, project)
        editor_log.close()
        log_text = (phase_root / "godot.log").read_text(encoding="utf-8", errors="replace")
        if "SCRIPT ERROR" in log_text:
            raise IndexMcpGateError(f"{phase} editor reported a script error")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", required=True, type=Path)
    parser.add_argument("--sidecar", default=DEFAULT_SIDECAR, type=Path)
    parser.add_argument("--evidence", default=DEFAULT_EVIDENCE, type=Path)
    parser.add_argument("--timeout", default=120.0, type=float)
    parser.add_argument(
        "--phases",
        default=",".join(PHASES),
        help="comma-separated local phase subset; canonical evidence uses all phases",
    )
    arguments = parser.parse_args()
    if sys.platform != "darwin" or platform.machine().lower() not in {"arm64", "aarch64"}:
        raise IndexMcpGateError("this local evidence profile requires macOS arm64")
    godot = arguments.godot.resolve(strict=True)
    sidecar = arguments.sidecar.resolve()
    build_sidecar(sidecar)
    golden = strict_json_load(GOLDEN_PATH)
    oracles = {phase["name"]: phase for phase in golden["phases"]}
    selected_phases = tuple(phase for phase in arguments.phases.split(",") if phase)
    if not selected_phases or any(phase not in PHASES for phase in selected_phases):
        raise IndexMcpGateError("--phases contains an unknown oracle phase")
    before = fixture_digest()
    query_samples: list[float] = []
    status_samples: list[float] = []
    visibility_samples: list[float] = []
    with tempfile.TemporaryDirectory(prefix="cs4-", dir="/tmp") as temporary:
        phase_results = [
            run_phase(
                phase,
                godot,
                sidecar,
                Path(temporary),
                oracles,
                arguments.timeout,
                query_samples,
                status_samples,
                visibility_samples,
            )
            for phase in selected_phases
        ]
    after = fixture_digest()
    if before != after:
        raise IndexMcpGateError("canonical resource fixture changed during the live gate")
    evidence = {
        "schema_version": 1,
        "stage": "Sprint 3 Stage 4 / S3-06-S3-08",
        "status": "passed",
        "execution": "local_model_free",
        "platform": "macos-arm64",
        "host": platform.platform(),
        "mcp_protocol": MCP_PROTOCOL,
        "tools": sorted(TOOL_NAMES),
        "fixture_digest_before": before,
        "fixture_digest_after": after,
        "canonical_fixture_unchanged": True,
        "phases": phase_results,
        "metrics_ms": {
            "query": {
                "samples": query_samples,
                "p50": percentile(query_samples, 0.50),
                "p95": percentile(query_samples, 0.95),
            },
            "change_visibility": {
                "samples": visibility_samples,
                "p50": percentile(visibility_samples, 0.50),
                "p95": percentile(visibility_samples, 0.95),
            },
            "status_ping": {
                "samples": status_samples,
                "p50": percentile(status_samples, 0.50),
                "p95": percentile(status_samples, 0.95),
            },
        },
        "platform_evidence": {
            "macos_arm64": "passed",
            "windows_x86_64": "not_run",
            "linux_x86_64": "not_run",
            "remote_ci": "not_run",
        },
    }
    arguments.evidence.parent.mkdir(parents=True, exist_ok=True)
    temporary = arguments.evidence.with_suffix(arguments.evidence.suffix + ".tmp")
    temporary.write_text(canonical_json(evidence), encoding="utf-8")
    os.replace(temporary, arguments.evidence)
    print(canonical_json(evidence), end="")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except IndexMcpGateError as error:
        print(f"FAIL Sprint 3 index/MCP gate: {error}", file=sys.stderr)
        raise SystemExit(1) from error
