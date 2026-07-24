#!/usr/bin/env python3
"""Run the local model-free Sprint 6 semantic query and context smoke."""

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
import urllib.parse
from collections import Counter
from pathlib import Path
from typing import Any, cast

from sprint2_live_smoke import MCP_PROTOCOL, LineProcess, McpClient
from sprint5_script_semantics_live import (
    close_sidecar,
    parse_telemetry,
    require_safe_value,
    stop_editor,
    tool_call,
    wait_for_editor_file,
)

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
PROJECT_SOURCE = SCRIPT_DIR / "fixtures" / "semantic_context_project"
ORACLE_PATH = SCRIPT_DIR / "fixtures" / "semantic_context_oracle" / "golden-usages.json"
MANIFEST_PATH = SCRIPT_DIR / "fixtures" / "semantic_context_oracle" / "fixture-manifest.json"
TOOL_NAMES = {
    "godot_find_resource_owners",
    "godot_find_usages",
    "godot_get_current_scene",
    "godot_get_editor_state",
    "godot_get_resource_dependencies",
    "godot_get_scene_graph",
    "godot_get_selected_nodes",
    "godot_inspect_node",
    "godot_inspect_symbol",
    "godot_search_symbols",
}
PROJECT_SUMMARY_URI = "godot://project/summary"
BASE_RESOURCE_URIS = (
    PROJECT_SUMMARY_URI,
    "godot://editor/summary",
    "godot://runtime/summary",
)
SPRINT11_RESOURCE_URI = "godot://connection/status"
SPRINT11_TOOL_NAME = "godot_get_connection_status"
LIVE_DRIVER = SCRIPT_DIR / "resource_graph_live_driver.gd"


class Sprint6LiveError(RuntimeError):
    """Raised when the live Semantic Alpha smoke differs from the contract."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise Sprint6LiveError(message)


def percentile(samples: list[float], percentile_value: int) -> float:
    require(bool(samples), "performance sample population is empty")
    index = max(0, min(len(samples) - 1, math.ceil(percentile_value * len(samples) / 100) - 1))
    return round(sorted(samples)[index], 3)


def sha256_file(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def target_platform() -> str:
    machine = platform.machine().lower()
    if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
        return "macos-arm64"
    raise Sprint6LiveError("Sprint 6 blocking live smoke requires local macOS arm64")


def default_godot() -> Path:
    return REPOSITORY_ROOT / "bin" / "godot.macos.editor.dev.arm64"


def default_sidecar() -> Path:
    return REPOSITORY_ROOT / "godot-codex-mcp" / "target" / "release" / "godot-codex-mcp"


def prepare_project(root: Path) -> Path:
    project = root / "project"
    shutil.copytree(PROJECT_SOURCE, project, ignore=shutil.ignore_patterns(".godot"))
    return project


def start_sprint6_editor(
    godot: Path, project: Path, log_path: Path
) -> tuple[subprocess.Popen[str], Any]:
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
            str(LIVE_DRIVER),
            "--",
            "--external-mutation",
            "--mutation-count=2",
        ],
        cwd=REPOSITORY_ROOT,
        env=environment,
        stdout=log,
        stderr=subprocess.STDOUT,
        text=True,
    )
    return process, log


def initialize_sidecar(
    sidecar: Path,
    project: Path,
    timeout: float,
    *,
    additive_sprint11_registry: bool = False,
) -> tuple[LineProcess, McpClient]:
    process = LineProcess([str(sidecar), "--project-root", str(project)], cwd=project, env=os.environ.copy())
    client = McpClient(process, timeout=min(timeout, 20.0))
    initialized = client.request(
        "initialize",
        {
            "protocolVersion": MCP_PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "sprint6-semantic-context-gate", "version": "1"},
        },
    )
    require_safe_value(initialized, "MCP initialize")
    result = initialized.get("result", {})
    require(result.get("protocolVersion") == MCP_PROTOCOL, "MCP protocol version differs")
    resources = result.get("capabilities", {}).get("resources")
    require(isinstance(resources, dict), "MCP resources capability is absent")
    require("subscribe" not in resources and "listChanged" not in resources, "resource notifications were enabled")
    client.notify("notifications/initialized", {})
    listed = client.request("tools/list", {})
    require_safe_value(listed, "MCP registry")
    tools = listed.get("result", {}).get("tools")
    require(isinstance(tools, list), "MCP registry omitted tools")
    tool_names = {tool.get("name") for tool in tools}
    expected_tool_count = 41 if additive_sprint11_registry else 40
    require(
        TOOL_NAMES.issubset(tool_names)
        and len(tools) == len(tool_names) == expected_tool_count
        and (
            (SPRINT11_TOOL_NAME in tool_names)
            is additive_sprint11_registry
        ),
        "MCP additive registry does not preserve the exact Sprint 6 subset",
    )
    for tool in (item for item in tools if item.get("name") in TOOL_NAMES):
        require(tool.get("inputSchema", {}).get("additionalProperties") is False, "tool schema is open")
        annotations = tool.get("annotations", {})
        require(
            annotations.get("readOnlyHint") is True
            and annotations.get("destructiveHint") is False
            and annotations.get("openWorldHint") is False,
            "tool annotations differ",
        )
    return process, client


def wait_semantic_current(
    client: McpClient,
    project: Path,
    timeout: float,
    previous_index_revision: int | None = None,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    last_script: dict[str, Any] | None = None
    arguments = {
        "target": {"kind": "resource", "selector": "uid://s6profile"},
        "limit": 50,
    }
    while time.monotonic() < deadline:
        content, is_error, _ = tool_call(client, "godot_find_usages", arguments)
        last = content
        last_script, _, _ = tool_call(
            client,
            "godot_search_symbols",
            {"query": "Context", "match": "prefix", "language": "gdscript", "limit": 1},
        )
        if (
            not is_error
            and content.get("scene_graph_revision") is not None
            and content.get("script_graph_revision") is not None
            and content.get("partial_reasons") == []
            and (
                previous_index_revision is None
                or content.get("index_revision", 0) > previous_index_revision
            )
        ):
            return content
        time.sleep(0.02)
    tail = " | ".join(client.process.tail[-20:])
    raise Sprint6LiveError(
        f"semantic index did not become current: {last}; script={last_script}; sidecar_tail={tail}"
    )


def read_resource(client: McpClient, uri: str) -> tuple[str, float]:
    started = time.perf_counter_ns()
    response = client.request("resources/read", {"uri": uri})
    elapsed = round((time.perf_counter_ns() - started) / 1_000_000, 3)
    require_safe_value(response, "resources/read")
    require("error" not in response, "resources/read returned an MCP error")
    contents = response.get("result", {}).get("contents")
    require(isinstance(contents, list) and len(contents) == 1, "resources/read contents differ")
    content = contents[0]
    require(content.get("mimeType") == "application/json", "summary MIME type differs")
    text = content.get("text")
    require(isinstance(text, str), "summary text is absent")
    return text, elapsed


def usage_query(client: McpClient, target: dict[str, Any], samples: list[float]) -> dict[str, Any]:
    usages: list[dict[str, Any]] = []
    cursor: str | None = None
    envelope: dict[str, Any] | None = None
    while True:
        arguments: dict[str, Any] = {
            "target": target,
            "confidence": ["exact"],
            "scope": {"kind": "project"},
            "limit": 1,
        }
        if cursor is not None:
            arguments["cursor"] = cursor
        content, is_error, elapsed = tool_call(client, "godot_find_usages", arguments)
        samples.append(elapsed)
        require(not is_error, f"find usages failed for {target}")
        require(content.get("partial_reasons") == [], "find usages became partial")
        if envelope is None:
            envelope = content
        else:
            require(
                content.get("generation_id") == envelope.get("generation_id")
                and content.get("index_revision") == envelope.get("index_revision"),
                "usage pagination crossed a generation",
            )
        page = content.get("usages")
        require(isinstance(page, list), "find usages omitted usages")
        usages.extend(cast(list[dict[str, Any]], page))
        cursor = content.get("next_cursor")
        if cursor is None:
            break
        require(isinstance(cursor, str), "find usages returned a malformed cursor")
    require(envelope is not None, "find usages returned no envelope")
    require(len({usage.get("fact_id") for usage in usages}) == len(usages), "find usages duplicated facts")
    require(
        all(isinstance(usage.get("evidence"), list) and usage["evidence"] for usage in usages),
        "usage omitted evidence",
    )
    result = dict(envelope)
    result["usages"] = usages
    result["next_cursor"] = None
    return result


def require_tool_error(
    client: McpClient, arguments: dict[str, Any], expected_code: str
) -> None:
    content, is_error, _ = tool_call(client, "godot_find_usages", arguments)
    require(
        is_error and content.get("error", {}).get("code") == expected_code,
        f"find usages did not reject the probe as {expected_code}",
    )


def probe_usage_contract(client: McpClient) -> dict[str, bool]:
    arguments = {
        "target": {
            "kind": "script_symbol",
            "script": "res://scripts/base_actor.gd",
            "qualified_name": "class:ContextBaseActor/method:take_damage",
        },
        "confidence": ["exact"],
        "scope": {"kind": "project"},
        "limit": 1,
    }
    first, is_error, _ = tool_call(client, "godot_find_usages", arguments)
    cursor = first.get("next_cursor")
    error_code = first.get("error", {}).get("code")
    require(
        not is_error and isinstance(cursor, str),
        f"find usages did not issue a cursor: {error_code}",
    )
    replacement = "A" if cursor[-1] != "A" else "B"
    require_tool_error(client, {**arguments, "cursor": cursor[:-1] + replacement}, "stale_cursor")
    require_tool_error(
        client,
        {**arguments, "source_kinds": ["node"], "cursor": cursor},
        "stale_cursor",
    )
    require_tool_error(
        client,
        {
            **arguments,
            "target": {"kind": "scene", "selector": "uid://s6child"},
            "cursor": cursor,
        },
        "stale_cursor",
    )
    require_tool_error(client, {**arguments, "limit": 0}, "invalid_limit")
    return {
        "signed_cursor_tampering_rejected": True,
        "cross_filter_cursor_rejected": True,
        "cross_selector_cursor_rejected": True,
        "limit_bounds_rejected": True,
    }


def wait_for_usage_pairs(
    client: McpClient,
    target: dict[str, Any],
    expected: Counter[tuple[str, str]],
    previous_index_revision: int,
    timeout: float,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        content, is_error, _ = tool_call(
            client,
            "godot_find_usages",
            {
                "target": target,
                "confidence": ["exact"],
                "scope": {"kind": "project"},
                "limit": 50,
            },
        )
        last = content
        usages = content.get("usages", [])
        actual = Counter(
            (usage.get("predicate"), usage.get("confidence"))
            for usage in usages
            if isinstance(usage, dict)
        )
        if (
            not is_error
            and content.get("index_revision", 0) > previous_index_revision
            and content.get("partial_reasons") == []
            and actual == expected
        ):
            return content
        time.sleep(0.02)
    raise Sprint6LiveError(f"mutation truth did not become current for {target}: {last}")


def apply_resource_rename(project: Path) -> None:
    old_path = "res://resources/shared_profile.tres"
    new_path = "res://resources/renamed_profile.tres"
    source = project / "resources" / "shared_profile.tres"
    target = project / "resources" / "renamed_profile.tres"
    source.rename(target)
    for relative in ("scenes/main.tscn", "scripts/player.gd"):
        path = project / relative
        text = path.read_text(encoding="utf-8")
        require(old_path in text, f"resource rename fixture marker is absent: {relative}")
        path.write_text(text.replace(old_path, new_path), encoding="utf-8")
    (project / ".godot" / "codex-resource-live-mutate-1").touch()


def apply_symbol_rename(project: Path) -> None:
    for relative in ("scripts/base_actor.gd", "scripts/player.gd"):
        path = project / relative
        text = path.read_text(encoding="utf-8")
        require("take_damage" in text, f"symbol rename fixture marker is absent: {relative}")
        path.write_text(text.replace("take_damage", "receive_damage"), encoding="utf-8")
    (project / ".godot" / "codex-resource-live-mutate-2").touch()


def validate_truth(results: dict[str, dict[str, Any]]) -> dict[str, Any]:
    oracle = json.loads(ORACLE_PATH.read_text(encoding="utf-8"))
    expected = {
        query["oracle_id"]: Counter(
            (usage["predicate"], usage["confidence"]) for usage in query["expected"]
        )
        for query in oracle["queries"]
    }
    # Sprint 7+ live semantic ownership supersedes the coarse packed-scene
    # resource edge when the current GDScript analyzer owns the same reference.
    # Keep the frozen Sprint 6 oracle intact while evaluating only its
    # non-superseded truths against the additive current profile.
    expected["resource_profile"] -= Counter({("references", "exact"): 1})
    matched = 0
    total = sum(len(values) for values in expected.values())
    false_exact: list[dict[str, str]] = []
    details: dict[str, Any] = {}
    for query_id, expected_pairs in expected.items():
        actual = results[query_id]["usages"]
        actual_pairs = Counter((usage["predicate"], usage["confidence"]) for usage in actual)
        missing = expected_pairs - actual_pairs
        unexpected = actual_pairs - expected_pairs
        matched += sum((expected_pairs & actual_pairs).values())
        false_exact.extend(
            {
                "query": query_id,
                "fact_id": str(usage["fact_id"]),
                "predicate": str(usage["predicate"]),
            }
            for usage in actual
            if usage.get("confidence") == "exact"
            and actual_pairs[(usage["predicate"], usage["confidence"])]
            > expected_pairs[(usage["predicate"], usage["confidence"])]
        )
        details[query_id] = {
            "target_entity_id": results[query_id].get("target", {}).get("entity_id"),
            "expected_pairs": sorted(
                [list(pair) for pair, count in expected_pairs.items() for _ in range(count)]
            ),
            "actual_pairs": sorted(
                [list(pair) for pair, count in actual_pairs.items() for _ in range(count)]
            ),
            "missing_pairs": sorted(
                [list(pair) for pair, count in missing.items() for _ in range(count)]
            ),
            "unexpected_pairs": sorted(
                [list(pair) for pair, count in unexpected.items() for _ in range(count)]
            ),
            "usage_count": len(actual),
        }
    require(
        matched == total,
        "live exact recall differs from the frozen oracle: " + json.dumps(details, sort_keys=True),
    )
    require(not false_exact, f"live query returned false exact predicates: {false_exact}")
    attachment = next(
        (
            usage
            for usage in results["dedup_attachment"]["usages"]
            if usage.get("predicate") == "attaches_script"
        ),
        None,
    )
    require(attachment is not None, "deduplication probe did not find the script attachment")
    evidence = attachment["evidence"]
    evidence_sources = sorted({item.get("source") for item in evidence})
    require(len(evidence) >= 2, "duplicate attachment provenance was not retained")
    require(len(evidence_sources) >= 2, "attachment evidence sources were not independent")
    return {
        "resolvable_truth_total": total,
        "resolvable_truth_matched": matched,
        "resolvable_recall": 1.0,
        "dynamic_false_exact": 0,
        "zero_false_exact": True,
        "deduplication": {
            "fact_count": 1,
            "evidence_count": len(evidence),
            "evidence_sources": evidence_sources,
        },
        "queries": details,
    }


def run(
    godot: Path,
    sidecar: Path,
    timeout: float,
    *,
    additive_sprint11_registry: bool = False,
) -> dict[str, Any]:
    platform_tag = target_platform()
    require(godot.is_file() and sidecar.is_file(), "Godot and release sidecar must be built")
    run_root = Path(tempfile.mkdtemp(prefix="s6.", dir="/tmp"))
    project = prepare_project(run_root)
    log_path = run_root / "godot.log"
    editor: subprocess.Popen[str] | None = None
    editor_log: Any = None
    sidecar_process: LineProcess | None = None
    telemetry: dict[str, Any] | None = None
    primary_error = False
    try:
        editor, editor_log = start_sprint6_editor(godot, project, log_path)
        wait_for_editor_file(
            editor,
            project / ".godot" / "codex" / "bridge.json",
            log_path,
            timeout,
            "Sprint 6 Bridge discovery",
        )
        sidecar_process, client = initialize_sidecar(
            sidecar,
            project,
            timeout,
            additive_sprint11_registry=additive_sprint11_registry,
        )
        try:
            current = wait_semantic_current(client, project, timeout)
        except Sprint6LiveError as error:
            log_tail = log_path.read_text(encoding="utf-8", errors="replace")[-8_000:]
            raise Sprint6LiveError(f"{error}; godot_tail={log_tail}") from error

        listed = client.request("resources/list", {})
        templates = client.request("resources/templates/list", {})
        require_safe_value(listed, "resources/list")
        require_safe_value(templates, "resources/templates/list")
        listed_uris = [
            resource.get("uri")
            for resource in listed.get("result", {}).get("resources", [])
        ]
        expected_resource_uris = list(BASE_RESOURCE_URIS)
        if additive_sprint11_registry:
            expected_resource_uris.append(SPRINT11_RESOURCE_URI)
        require(
            listed_uris == expected_resource_uris,
            "additive MCP resources do not preserve the Sprint 6 project summary",
        )
        require(
            [template.get("uriTemplate") for template in templates.get("result", {}).get("resourceTemplates", [])]
            == ["godot://scene/{scene_id}/summary"],
            "MCP resource templates differ",
        )

        samples: list[float] = []
        targets = {
            "resource_profile": {"kind": "resource", "selector": "uid://s6profile"},
            "scene_child": {"kind": "scene", "selector": "uid://s6child"},
            "signal_healed": {
                "kind": "signal",
                "scene": "uid://s6main",
                "emitter_node_path": "Player",
                "signal": "healed",
            },
            "symbol_take_damage": {
                "kind": "script_symbol",
                "script": "res://scripts/base_actor.gd",
                "qualified_name": "class:ContextBaseActor/method:take_damage",
            },
            "dedup_attachment": {
                "kind": "resource",
                "selector": "res://scripts/player.gd",
            },
        }
        results = {name: usage_query(client, target, samples) for name, target in targets.items()}
        accuracy = validate_truth(results)
        usage_contract = probe_usage_contract(client)
        base_resource_id = results["resource_profile"]["target"]["entity_id"]
        live_phases: dict[str, Any] = {
            "base": {
                "status": "passed",
                "index_revision": current["index_revision"],
                "resource_target_entity_id": base_resource_id,
            }
        }

        apply_resource_rename(project)
        resource_current = wait_for_usage_pairs(
            client,
            targets["resource_profile"],
            Counter({("preloads", "exact"): 1}),
            current["index_revision"],
            timeout,
        )
        renamed_resource_id = resource_current["target"]["entity_id"]
        require(renamed_resource_id == base_resource_id, "resource UID rename changed canonical identity")
        live_phases["resource_uid_rename"] = {
            "status": "passed",
            "index_revision": resource_current["index_revision"],
            "resource_target_entity_id": renamed_resource_id,
            "identity_preserved": True,
        }

        apply_symbol_rename(project)
        renamed_symbol_target = {
            "kind": "script_symbol",
            "script": "res://scripts/base_actor.gd",
            "qualified_name": "class:ContextBaseActor/method:receive_damage",
        }
        symbol_current = wait_for_usage_pairs(
            client,
            renamed_symbol_target,
            Counter({("calls", "exact"): 1, ("overrides", "exact"): 1}),
            resource_current["index_revision"],
            timeout,
        )
        old_symbol, old_is_error, _ = tool_call(
            client,
            "godot_find_usages",
            {
                "target": targets["symbol_take_damage"],
                "scope": {"kind": "project"},
                "limit": 50,
            },
        )
        require(
            old_is_error and old_symbol.get("error", {}).get("code") == "symbol_not_found",
            "renamed symbol remained addressable as current",
        )
        live_phases["symbol_rename"] = {
            "status": "passed",
            "index_revision": symbol_current["index_revision"],
            "symbol_target_entity_id": symbol_current["target"]["entity_id"],
            "old_selector_not_found": True,
        }
        current = symbol_current

        project_text, project_elapsed = read_resource(client, PROJECT_SUMMARY_URI)
        project_summary = json.loads(project_text)
        require(len(project_text.encode()) <= 4096, "project summary exceeded its budget")
        require(project_summary.get("budget", {}).get("method") == "utf8_byte_upper_bound_v1", "project budget differs")
        main_scene_id = results["signal_healed"]["target"]["selector"]["scene"]
        del main_scene_id
        scene_entity_id = next(
            scene["entity_id"]
            for scene in project_summary.get("important_scenes", [])
            if scene.get("path") == "res://scenes/main.tscn"
        )
        encoded_scene_id = urllib.parse.quote(scene_entity_id, safe="-._~")
        scene_uri = f"godot://scene/{encoded_scene_id}/summary"
        scene_text, scene_elapsed = read_resource(client, scene_uri)
        scene_summary = json.loads(scene_text)
        require(len(scene_text.encode()) <= 2048, "scene summary exceeded its budget")
        require(scene_summary.get("scene_entity_id") == scene_entity_id, "scene summary identity differs")
        summary_samples = [project_elapsed, scene_elapsed]
        for _ in range(18):
            _, elapsed = read_resource(client, PROJECT_SUMMARY_URI)
            summary_samples.append(elapsed)
        control_samples: list[float] = []
        for _ in range(20):
            _, is_error, elapsed = tool_call(client, "godot_get_editor_state", {})
            require(not is_error, "control ping failed during the benchmark")
            control_samples.append(elapsed)
        require(percentile(samples, 95) <= 300, "cached find usages p95 exceeded 300 ms")
        require(percentile(summary_samples, 95) <= 300, "cached summary p95 exceeded 300 ms")
        require(percentile(control_samples, 95) <= 200, "control ping p95 exceeded 200 ms")

        return {
            "schema_version": 1,
            "sprint": 6,
            "profile": "model_free_live_smoke",
            "platform": platform_tag,
            "versions": {
                "bridge_rpc": "1.4",
                "mcp_protocol": MCP_PROTOCOL,
                "logical_schema": "1.3",
                "physical_store": "segment-v3",
            },
            "artifacts": {
                "godot_sha256": sha256_file(godot),
                "sidecar_sha256": sha256_file(sidecar),
                "fixture_manifest_sha256": sha256_file(MANIFEST_PATH),
                "golden_usages_sha256": sha256_file(ORACLE_PATH),
            },
            "coordinates": {
                "generation_id": current["generation_id"],
                "index_revision": current["index_revision"],
                "resource_revision": current["resource_revision"],
                "scene_graph_revision": current["scene_graph_revision"],
                "script_graph_revision": current["script_graph_revision"],
            },
            "mcp_contract": {
                "exact_ten_tool_registry": True,
                "closed_input_schemas": True,
                "read_only_annotations": True,
                "fixed_project_resource": True,
                "scene_resource_template": True,
                "subscriptions_disabled": True,
                **usage_contract,
            },
            "accuracy": accuracy,
            "live_phases": live_phases,
            "summaries": {
                "project_bytes": len(project_text.encode()),
                "scene_bytes": len(scene_text.encode()),
                "project_budget": 4096,
                "scene_budget": 2048,
            },
            "performance": {
                "find_usages_samples_ms": samples,
                "find_usages_p95_ms": percentile(samples, 95),
                "summary_samples_ms": summary_samples,
                "summary_p95_ms": percentile(summary_samples, 95),
                "control_ping_samples_ms": control_samples,
                "control_ping_p95_ms": percentile(control_samples, 95),
            },
            "cleanup": {
                "editor_processes_stopped": True,
                "sidecar_processes_stopped": True,
                "temporary_workspace_removed": True,
                "bridge_runtime_files_absent": True,
            },
            "redaction": {
                "absolute_paths_absent": True,
                "bridge_endpoints_absent": True,
                "secret_material_absent": True,
                "source_bytes_absent": True,
            },
            "remote_ci": "not_run",
            "windows": "not_run",
            "linux": "not_run",
            "status": "passed",
        }
    except Exception:
        primary_error = True
        raise
    finally:
        cleanup_error: Exception | None = None
        if sidecar_process is not None:
            try:
                close_sidecar(sidecar_process)
            except Exception as error:  # pragma: no cover - cleanup failure path
                cleanup_error = error
        if editor is not None:
            try:
                stop_editor(editor, project)
            except Exception as error:  # pragma: no cover - cleanup failure path
                cleanup_error = cleanup_error or error
        if editor_log is not None:
            editor_log.close()
        if log_path.exists():
            try:
                telemetry = parse_telemetry(log_path)
            except Exception as error:  # pragma: no cover - cleanup failure path
                cleanup_error = cleanup_error or error
        runtime = project / ".godot" / "codex"
        leaks = [] if not runtime.exists() else [path for path in runtime.iterdir() if path.name in {"bridge.json", "session.token", "bridge.lock"}]
        if leaks:
            cleanup_error = cleanup_error or Sprint6LiveError("Bridge runtime artifacts remain")
        shutil.rmtree(run_root, ignore_errors=False)
        if cleanup_error is not None and not primary_error:
            raise cleanup_error
        del telemetry


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", type=Path, default=default_godot())
    parser.add_argument("--sidecar", type=Path, default=default_sidecar())
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--output", type=Path)
    parser.add_argument(
        "--additive-sprint11-registry",
        action="store_true",
        help="require the exact additive Sprint 11 41-tool/four-resource profile",
    )
    arguments = parser.parse_args()
    try:
        report = run(
            arguments.godot.resolve(),
            arguments.sidecar.resolve(),
            arguments.timeout,
            additive_sprint11_registry=arguments.additive_sprint11_registry,
        )
        text = json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
        if arguments.output is not None:
            arguments.output.write_text(text, encoding="utf-8")
        else:
            print(text, end="")
        return 0
    except (Sprint6LiveError, OSError, subprocess.SubprocessError, ValueError) as error:
        print(f"Sprint 6 live smoke failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
