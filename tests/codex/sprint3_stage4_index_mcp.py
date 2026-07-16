#!/usr/bin/env python3
"""Run the local model-free Sprint 3 persistent-index and resource MCP gate."""

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
SOURCE_SCOPES = (
    "modules/codex_bridge",
    "schemas/codex_bridge/v1",
    "godot-codex-mcp/Cargo.toml",
    "godot-codex-mcp/Cargo.lock",
    "godot-codex-mcp/crates",
    "tests/codex/sprint2_live_smoke.py",
    "tests/codex/sprint3_stage3_live.py",
    "tests/codex/sprint3_stage4_index_mcp.py",
    "tests/codex/resource_graph_fixture.py",
    "tests/codex/resource_graph_live_driver.gd",
    "tests/codex/fixtures/resource_graph_project",
    "tests/codex/fixtures/resource_graph_oracle",
)
TELEMETRY_PREFIX = "[codex_bridge_evidence] "
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


def default_evidence(platform_tag: str) -> Path:
    suffix = "macos" if platform_tag == "macos-arm64" else "windows"
    return SCRIPT_DIR / "evidence" / f"sprint-3-resource-graph-{suffix}.json"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return f"sha256:{digest.hexdigest()}"


def command_output(command: list[str]) -> str:
    result = subprocess.run(
        command,
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
    )
    if result.returncode != 0:
        raise IndexMcpGateError(f"command failed while binding evidence: {command[0]}")
    return result.stdout.strip()


def source_coordinates() -> dict[str, Any]:
    status = command_output(["git", "status", "--porcelain", "--", *SOURCE_SCOPES])
    listed = subprocess.run(
        [
            "git",
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
        if bulk_status_samples is not None:
            status, status_error, status_ms = tool_call(client, "godot_get_editor_state", {})
            bulk_status_samples.append(round(status_ms, 3))
            if status_error or status.get("freshness") != "current":
                raise IndexMcpGateError("editor status was unavailable during bulk rebuild")
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
    normalized_graph = [
        {
            "edge_id": edge_id,
            "source_entity_id": expected_by_id[edge_id]["source_entity_id"],
            "target_uid": dependency.get("target_uid"),
            "target_path": dependency.get("target_path"),
            "target_entity_id": (
                dependency.get("target", {}).get("entity_id")
                if isinstance(dependency.get("target"), dict)
                else None
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
        "diagnostic_count": len(oracle["diagnostics"]),
        "normalized_graph_sha256": f"sha256:{graph_digest}",
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
    if set(record) != required or record.get("schema_version") != 1 or record.get("budget_usec") != 2000:
        raise IndexMcpGateError("Bridge telemetry contract differs")
    samples = record.get("samples_usec")
    if not isinstance(samples, list) or not samples or not all(isinstance(value, int) and value >= 0 for value in samples):
        raise IndexMcpGateError("Bridge telemetry samples are empty or invalid")
    busy_count = record.get("busy_frame_count")
    over_budget = record.get("over_budget_count")
    maximum = record.get("max_elapsed_usec")
    if not all(isinstance(value, int) and value >= 0 for value in (busy_count, over_budget, maximum)):
        raise IndexMcpGateError("Bridge telemetry counters are invalid")
    if record.get("overflow") is False and busy_count != len(samples):
        raise IndexMcpGateError("Bridge telemetry omitted a busy frame without overflow")
    if over_budget < sum(value > 2000 for value in samples) or maximum < max(samples):
        raise IndexMcpGateError("Bridge telemetry summary contradicts raw samples")
    return record


def run_phase(
    phase: str,
    godot: Path,
    sidecar: Path,
    run_root: Path,
    oracles: dict[str, dict[str, Any]],
    timeout: float,
    query_samples: list[float],
    bulk_status_samples: list[float],
    incremental_visibility_samples: list[float],
    startup_samples: list[float],
    reopen_samples: list[float],
    rebuild_samples: list[float],
) -> dict[str, Any]:
    phase_root = run_root / f"p{PHASES.index(phase)}"
    project = phase_root / "p"
    prepared = phase_root / "m"
    phase_root.mkdir(parents=True)
    shutil.copytree(PROJECT_SOURCE, project, ignore=shutil.ignore_patterns(".godot"))
    mutation_count = 2 if phase == "re_add" else 1
    editor, editor_log = start_editor(
        godot,
        project,
        phase_root / "godot.log",
        mutation_count,
        evidence_telemetry=True,
    )
    mcp: LineProcess | None = None
    phase_result: dict[str, Any] = {}
    try:
        wait_for(project / ".godot" / "codex" / "bridge.json", timeout, f"{phase} discovery")
        mcp, client = initialize_sidecar(sidecar, project, timeout)
        base_resource = selector(next(resource for resource in oracles["base"]["resources"] if resource["oracle_id"] == "fan_out"))
        current, startup_ms = wait_for_current(client, base_resource, timeout)
        startup_samples.append(startup_ms)
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
            reopen_samples.append(reopen_ms)
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
            status_content, status_error, _ = tool_call(client, "godot_get_editor_state", {})
            if status_error or status_content.get("freshness") != "current":
                raise IndexMcpGateError("status/ping did not remain current after sidecar reopen")
            (project / ".godot" / "codex-resource-live-mutate").touch()
            phase_result.update({
                "phase": phase,
                **base_result,
                "startup_visibility_ms": startup_ms,
                "reopen_visibility_ms": reopen_ms,
                "same_generation_after_reopen": True,
                "immutable_segment_set_reused": True,
                "passed": True,
            })
            return phase_result

        if phase == "re_add":
            mutate_phase(phase, godot, project, prepared, 1)
            removed, removal_ms = wait_for_current(client, base_resource, timeout, base_generation)
            incremental_visibility_samples.append(removal_ms)
            mutate_phase(phase, godot, project, prepared, 2)
            final, visibility_ms = wait_for_current(
                client,
                base_resource,
                timeout,
                str(removed["generation_id"]),
            )
            incremental_visibility_samples.append(visibility_ms)
        else:
            mutate_phase(phase, godot, project, prepared)
            final, visibility_ms = wait_for_current(
                client,
                base_resource,
                timeout,
                base_generation,
                bulk_status_samples if phase == "journal_gap" else None,
            )
            if phase == "journal_gap":
                rebuild_samples.append(visibility_ms)
            else:
                incremental_visibility_samples.append(visibility_ms)
        if phase == "journal_gap" and final.get("validated_checkpoint", {}).get("last_batch_id") is not None:
            raise IndexMcpGateError("journal gap did not activate a full-snapshot checkpoint")
        if int(final["index_revision"]) <= base_revision:
            raise IndexMcpGateError(f"{phase} did not advance the durable index revision")
        result = verify_oracle(client, oracles[phase], query_samples)
        status_content, status_error, _ = tool_call(client, "godot_get_editor_state", {})
        if status_error or status_content.get("freshness") != "current":
            raise IndexMcpGateError(f"{phase} editor status was not current")
        phase_result.update({
            "phase": phase,
            **result,
            "startup_generation": base_generation,
            "startup_index_revision": base_revision,
            "change_visibility_ms": visibility_ms,
            "generation_advanced": result["generation_id"] != base_generation,
            "passed": True,
        })
        if phase == "re_add":
            phase_result["removal_visibility_ms"] = removal_ms
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
        phase_result["bridge_main_thread"] = parse_bridge_telemetry(log_text)


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
    platform_tag = target_platform()
    godot = arguments.godot.resolve(strict=True)
    sidecar = (arguments.sidecar or default_sidecar()).resolve()
    evidence_path = arguments.evidence or default_evidence(platform_tag)
    build_sidecar(sidecar)
    sidecar = sidecar.resolve(strict=True)
    source = source_coordinates()
    golden = strict_json_load(GOLDEN_PATH)
    oracles = {phase["name"]: phase for phase in golden["phases"]}
    selected_phases = tuple(phase for phase in arguments.phases.split(",") if phase)
    if (
        not selected_phases
        or any(phase not in PHASES for phase in selected_phases)
        or len(set(selected_phases)) != len(selected_phases)
    ):
        raise IndexMcpGateError("--phases contains an unknown oracle phase")
    complete_profile = selected_phases == PHASES
    before = fixture_digest()
    query_samples: list[float] = []
    bulk_status_samples: list[float] = []
    incremental_visibility_samples: list[float] = []
    startup_samples: list[float] = []
    reopen_samples: list[float] = []
    rebuild_samples: list[float] = []
    temporary_parent = "/tmp" if platform_tag == "macos-arm64" else None
    with tempfile.TemporaryDirectory(prefix="cs5-", dir=temporary_parent) as temporary:
        phase_results = [
            run_phase(
                phase,
                godot,
                sidecar,
                Path(temporary),
                oracles,
                arguments.timeout,
                query_samples,
                bulk_status_samples,
                incremental_visibility_samples,
                startup_samples,
                reopen_samples,
                rebuild_samples,
            )
            for phase in selected_phases
        ]
    after = fixture_digest()
    if before != after:
        raise IndexMcpGateError("canonical resource fixture changed during the live gate")
    telemetry_records = [phase["bridge_main_thread"] for phase in phase_results]
    bridge_samples = [
        sample
        for record in telemetry_records
        for sample in record["samples_usec"]
    ]
    bridge_over_budget = sum(int(record["over_budget_count"]) for record in telemetry_records)
    bridge_overflow = any(bool(record["overflow"]) for record in telemetry_records)
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
        "bulk_status_ping_p95_lte_200ms": bool(bulk_status_samples)
        and metrics_ms["bulk_status_ping"]["p95"] <= 200,
        "bridge_main_thread_over_2000us_zero": bool(bridge_samples)
        and bridge_over_budget == 0
        and not bridge_overflow,
    }
    slo["all_passed"] = all(slo.values())
    qualifying_source = complete_profile and not source["git_dirty"]
    status = "passed" if qualifying_source and slo["all_passed"] else "failed" if complete_profile else "development"
    evidence = {
        "schema_version": 2,
        "stage": "Sprint 3 Stage 5 / S3-09-S3-10",
        "status": status,
        "execution": "local_model_free",
        "profile": "acceptance" if complete_profile else "development",
        "platform": platform_tag,
        "host": platform.platform(),
        **source,
        "artifacts": {
            "godot_version": version_output(godot),
            "godot_sha256": sha256_file(godot),
            "sidecar_version": version_output(sidecar),
            "sidecar_sha256": sha256_file(sidecar),
        },
        "mcp_protocol": MCP_PROTOCOL,
        "tools": sorted(TOOL_NAMES),
        "oracle_sha256": sha256_file(GOLDEN_PATH),
        "fixture_digest_before": before,
        "fixture_digest_after": after,
        "canonical_fixture_unchanged": True,
        "phases": phase_results,
        "metrics_ms": metrics_ms,
        "bridge_main_thread": main_thread_metric,
        "slo": slo,
        "platform_evidence": {
            "remote_ci": "not_run",
        },
    }
    serialized = canonical_json(evidence)
    forbidden = (str(REPOSITORY_ROOT), str(PROJECT_SOURCE.resolve()))
    if any(value in serialized for value in forbidden):
        raise IndexMcpGateError("live evidence contains an absolute repository or fixture path")
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    temporary = evidence_path.with_suffix(evidence_path.suffix + ".tmp")
    temporary.write_text(serialized, encoding="utf-8")
    os.replace(temporary, evidence_path)
    print(serialized, end="")
    if complete_profile and status != "passed":
        reason = "source tree is dirty" if source["git_dirty"] else "one or more live SLOs failed"
        raise IndexMcpGateError(f"Sprint 3 live acceptance failed ({reason}); evidence={evidence_path}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except IndexMcpGateError as error:
        print(f"FAIL Sprint 3 index/MCP gate: {error}", file=sys.stderr)
        raise SystemExit(1) from error
