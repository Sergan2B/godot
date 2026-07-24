#!/usr/bin/env python3
"""Run the model-free Sprint 10 compound MCP workflow."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import time
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Mapping, cast

import sprint9_model_free_live as s9

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
CHANGE_SET_RE = re.compile(r"change-set:[0-9a-f]{32}\Z")
DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
REPORT_RE = re.compile(r"validation-report:[0-9a-f]{32}\Z")

SPRINT10_TOOLS = {
    "godot_prepare_change_set",
    "godot_get_validation_report",
    "godot_get_confirmation_policy",
    "godot_reset_confirmation_policy",
}
BASE_SPRINT9_TOOLS = frozenset(s9.TOOL_NAMES)
BASE_SPRINT9_MUTATING_TOOLS = frozenset(s9.MUTATING_TOOLS)


class Sprint10McpClient(s9.ModelFreeMcpClient):
    """Validate the real host-owned compound confirmation form."""

    def expect_change_set(self, prepared: Mapping[str, Any]) -> None:
        s9.require(self.approval is None, "another approval expectation is active")
        self.approval = cast(
            Any,
            SimpleNamespace(
                transaction_id=str(prepared["change_set_id"]),
                preview_digest=str(prepared["preview_digest"]),
                scope="change_set.atomic",
                risk=str(prepared["risk"]),
                operation_kind="change_set",
                decision="accept",
                calls=0,
            ),
        )

    def _handle_elicitation(self, message: Mapping[str, Any]) -> None:
        expectation = self.approval
        if expectation is None or not str(expectation.transaction_id).startswith(
            "change-set:"
        ):
            super()._handle_elicitation(message)
            return
        request_id = message.get("id")
        params = message.get("params")
        s9.require(isinstance(request_id, int), "elicitation request ID is malformed")
        s9.require(isinstance(params, dict), "elicitation params are malformed")
        s9.require(expectation.calls == 0, "compound approval elicitation was repeated")
        s9.require(params.get("mode") == "form", "compound approval is not a form")
        schema = params.get("requestedSchema")
        properties = schema.get("properties") if isinstance(schema, dict) else None
        legacy_confirm = (
            isinstance(schema, dict)
            and schema.get("required") == ["confirm"]
            and isinstance(properties, dict)
            and set(properties).issubset(
                {"confirm", "allow_low_risk_for_session"}
            )
            and set(properties).issuperset({"confirm"})
            and properties["confirm"].get("type") == "boolean"
        )
        action_only = (
            isinstance(schema, dict)
            and schema.get("required") in (None, [])
            and isinstance(properties, dict)
            and not properties
        )
        s9.require(
            isinstance(schema, dict)
            and schema.get("type") == "object"
            and (legacy_confirm or action_only),
            "compound approval schema is not exact",
        )
        message_text = params.get("message")
        s9.require(
            isinstance(message_text, str)
            and len(message_text.encode("utf-8")) <= 8_192
            and f"Change set: {expectation.transaction_id}" in message_text
            and f"Digest: {expectation.preview_digest}" in message_text
            and "Scope: change_set.atomic" in message_text
            and f"Risk: {expectation.risk}" in message_text,
            "compound approval message binding differs",
        )
        expectation.calls += 1
        self.elicitation_total += 1
        self.process.send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "result": (
                    {
                        "action": "accept",
                        "content": {"confirm": True},
                    }
                    if legacy_confirm
                    else {"action": "accept"}
                ),
            }
        )


def _install_sprint10_client(*, additive_sprint11_registry: bool = False) -> None:
    s9.TOOL_NAMES = set(BASE_SPRINT9_TOOLS) | SPRINT10_TOOLS
    if additive_sprint11_registry:
        s9.TOOL_NAMES |= s9.SPRINT11_TOOL_NAMES
    s9.MUTATING_TOOLS = set(BASE_SPRINT9_MUTATING_TOOLS) | {
        "godot_prepare_change_set",
        "godot_reset_confirmation_policy",
    }
    s9.ModelFreeMcpClient = Sprint10McpClient


def _coordinates(
    current: Mapping[str, Any],
    history: Mapping[str, Any],
    state: Mapping[str, Any],
) -> dict[str, Any]:
    base = s9.coordinates(current, history)
    revisions = state.get("revision_vector")
    s9.require(isinstance(revisions, dict), "editor revision vector is missing")
    for field in ("resource_revision", "script_graph_revision"):
        s9.require(isinstance(revisions.get(field), int), f"{field} is missing")
    return {
        **base,
        "resource_revision": revisions["resource_revision"],
        "script_graph_revision": revisions["script_graph_revision"],
    }


def _wait_terminal(
    client: Sprint10McpClient,
    change_set_id: str,
    timeout: float,
) -> tuple[dict[str, Any], list[float]]:
    deadline = time.monotonic() + timeout
    samples: list[float] = []
    last: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        status, error, elapsed = client.tool(
            "godot_get_transaction_status",
            {"transaction_id": change_set_id},
        )
        samples.append(elapsed)
        last = status
        if not error and status.get("state") in {
            "committed",
            "rolled_back",
            "rollback_blocked",
            "failed",
            "in_doubt",
        }:
            return status, samples
        time.sleep(0.05)
    raise s9.WorkflowError(f"change set did not become terminal: {last}")


def _all_project_hashes(root: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for path in sorted(root.rglob("*")):
        if not path.is_file() or ".godot" in path.parts:
            continue
        relative = path.relative_to(root).as_posix()
        result[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def _wait_scene_node(
    client: Sprint10McpClient,
    name: str,
    present: bool,
    timeout: float,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    deadline = time.monotonic() + timeout
    last: tuple[dict[str, Any], dict[str, Any], dict[str, Any]] | None = None
    while time.monotonic() < deadline:
        current, history, state, _ = s9.stable_read(client, timeout)
        last = (current, history, state)
        projected = _coordinates(current, history, state)
        if (name in cast(set[str], projected["node_paths"])) == present:
            return current, history, state
        time.sleep(0.05)
    raise s9.WorkflowError(f"scene node readback did not converge: {last}")


def run_workflow(
    godot: Path,
    sidecar: Path,
    timeout: float,
    *,
    additive_sprint11_registry: bool = False,
) -> dict[str, Any]:
    _install_sprint10_client(
        additive_sprint11_registry=additive_sprint11_registry
    )
    session = s9.FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    with session:
        s9.require(session.project_root is not None, "fixture project is unavailable")
        s9.require(
            isinstance(session.client, Sprint10McpClient),
            "Sprint 10 MCP client is unavailable",
        )
        client = session.client
        current, history, state, read_latencies = s9.stable_read(client, timeout)
        before = _coordinates(current, history, state)
        root_id = cast(Mapping[str, str], before["node_ids"])["."]
        source_before = _all_project_hashes(session.project_root)
        key = hashlib.sha256(b"s10:model-free:memory-alias").hexdigest()[:32]
        prepare_arguments = {
            "project_id": before["project_id"],
            "idempotency_key": f"idempotency:{key}",
            "coordinates": {
                "editor_session_id": before["editor_session_id"],
                "scene_id": before["scene_id"],
                "scene_revision": before["scene_revision"],
                "operation_seq": before["operation_seq"],
                "resource_revision": before["resource_revision"],
                "script_graph_revision": before["script_graph_revision"],
            },
            "operations": [
                {
                    "kind": "create_node",
                    "alias": "alias:created",
                    "parent_node_id": root_id,
                    "godot_type": "Node",
                    "name": "CompoundCreated",
                },
                {
                    "kind": "set_property",
                    "node_id": "alias:created",
                    "property": "process_priority",
                    "value": {"type": "int", "value": 7},
                },
            ],
            "save_scope": {"paths": []},
            "validation_policy": {
                "rollback": "on_required_failure",
                "warnings": "allow",
                "runtime": "skip",
            },
        }
        prepared, error, prepare_ms = client.tool(
            "godot_prepare_change_set", prepare_arguments
        )
        s9.require(
            not error,
            f"compound prepare failed: {prepared}; arguments={prepare_arguments}",
        )
        s9.require(
            CHANGE_SET_RE.fullmatch(str(prepared.get("change_set_id"))) is not None
            and DIGEST_RE.fullmatch(str(prepared.get("preview_digest"))) is not None
            and prepared.get("state") == "previewed"
            and prepared.get("operation_count") == 2,
            "compound prepare projection differs",
        )
        s9.require(
            source_before == _all_project_hashes(session.project_root),
            "compound prepare changed project-content bytes",
        )
        replay, replay_error, _ = client.tool(
            "godot_prepare_change_set", prepare_arguments
        )
        s9.require(
            not replay_error
            and replay.get("change_set_id") == prepared["change_set_id"]
            and replay.get("preview_digest") == prepared["preview_digest"],
            "compound idempotent prepare did not replay exactly",
        )

        client.expect_change_set(prepared)
        applied, apply_error, apply_ms = client.tool(
            "godot_apply_transaction",
            {
                "transaction_id": prepared["change_set_id"],
                "preview_digest": prepared["preview_digest"],
                "expected_scene_revision": before["scene_revision"],
                "expected_operation_seq": before["operation_seq"],
            },
            timeout=max(timeout, 15.0),
        )
        client.clear_approval()
        s9.require(not apply_error, f"compound apply failed: {applied}")
        terminal, status_samples = _wait_terminal(
            client, str(prepared["change_set_id"]), max(timeout, 15.0)
        )
        report_id = terminal.get("validation_report_id")
        s9.require(
            isinstance(report_id, str) and REPORT_RE.fullmatch(report_id) is not None,
            "terminal change set omitted its validation report",
        )
        report, report_error, report_ms = client.tool(
            "godot_get_validation_report",
            {"report_id": report_id, "page": 0},
        )
        s9.require(not report_error, f"validation report read failed: {report}")
        page_count = report.get("page_count")
        s9.require(
            report.get("report_id") == report_id
            and report.get("page") == 0
            and page_count in range(1, 5)
            and isinstance(report.get("content_bytes"), int)
            and report["content_bytes"] <= 65_536,
            f"validation report page binding differs: {report}",
        )
        pages = [report]
        for page in range(1, cast(int, page_count)):
            next_page, next_error, elapsed = client.tool(
                "godot_get_validation_report",
                {"report_id": report_id, "page": page},
            )
            report_ms += elapsed
            s9.require(not next_error, f"validation report page read failed: {next_page}")
            pages.append(next_page)
        report_content = ""
        report_digest = report.get("report_digest")
        for page, item in enumerate(pages):
            content = item.get("content")
            s9.require(
                item.get("report_id") == report_id
                and item.get("report_digest") == report_digest
                and item.get("page") == page
                and item.get("page_count") == page_count
                and isinstance(content, str)
                and item.get("content_bytes") == len(content.encode("utf-8"))
                and item["content_bytes"] <= 65_536,
                "validation report pagination binding differs",
            )
            report_content += content
        s9.require(
            len(report_content.encode("utf-8")) <= 262_144,
            "validation report retained bytes exceed the bound",
        )
        parsed_report = s9.strict_json_text(report_content)
        checks = parsed_report.get("checks")
        check_outcomes = {
            item.get("check"): item.get("outcome")
            for item in checks
            if isinstance(item, dict)
        } if isinstance(checks, list) else {}
        s9.require(
            parsed_report.get("report_id") == report_id
            and parsed_report.get("change_set_id") == prepared["change_set_id"]
            and parsed_report.get("preview_digest") == prepared["preview_digest"]
            and parsed_report.get("report_digest") == report_digest
            and parsed_report.get("outcome") == "passed"
            and parsed_report.get("affected_closure_only") is True
            and set(check_outcomes)
            == {
                "diagnostics",
                "index_convergence",
                "intrinsic",
                "persistence",
                "reload_reparse",
                "runtime",
                "semantic_graph",
            }
            and all(
                check_outcomes[name] == "passed"
                for name in set(check_outcomes) - {"runtime"}
            )
            and check_outcomes["runtime"] == "skipped",
            f"validation report proof differs: outcome={parsed_report.get('outcome')}; checks={check_outcomes}",
        )
        s9.require(
            terminal.get("state") == "committed",
            f"automatic validation did not commit: {terminal}; report={report}",
        )

        after_current, after_history, after_state = _wait_scene_node(
            client, "CompoundCreated", True, timeout
        )
        after = _coordinates(after_current, after_history, after_state)
        s9.require(
            "CompoundCreated" in cast(set[str], after["node_paths"])
            and int(after["action_count"]) == int(before["action_count"]) + 1,
            "compound post-state is not one native action",
        )
        undo, undo_error, undo_ms = client.tool(
            "godot_undo_transaction",
            {
                "transaction_id": prepared["change_set_id"],
                "expected_transaction_seq": terminal["transaction_seq"],
                "expected_scene_revision": after["scene_revision"],
                "expected_operation_seq": after["operation_seq"],
            },
        )
        s9.require(not undo_error and undo.get("state") == "undone", f"Undo failed: {undo}")
        restored_current, restored_history, restored_state = _wait_scene_node(
            client, "CompoundCreated", False, timeout
        )
        restored = _coordinates(restored_current, restored_history, restored_state)
        s9.require(
            "CompoundCreated" not in cast(set[str], restored["node_paths"]),
            "compound Undo did not restore the scene tree",
        )
        s9.require(
            source_before == _all_project_hashes(session.project_root),
            "memory-only workflow changed project-content bytes",
        )
        return {
            "schema_version": "s10-model-free-live/1.0",
            "status": "passed",
            "platform": "macos-arm64",
            "protocol": "2025-11-25",
            "bridge_rpc": "1.8",
            "tool_registry": 41 if additive_sprint11_registry else 40,
            "artifacts": {
                "godot_sha256": s9.sha256_file(godot),
                "sidecar_sha256": s9.sha256_file(sidecar),
            },
            "change_set_id": prepared["change_set_id"],
            "validation_report_id": report_id,
            "operation_count": 2,
            "one_native_action": True,
            "prepare_read_only": True,
            "exact_undo": True,
            "validation": {
                "outcome": parsed_report["outcome"],
                "checks": check_outcomes,
                "page_count": page_count,
                "retained_bytes": len(report_content.encode("utf-8")),
            },
            "latency_ms": {
                "read": read_latencies,
                "prepare": [prepare_ms],
                "apply": [apply_ms],
                "status": status_samples,
                "report": [report_ms],
                "undo": [undo_ms],
            },
            "cleanup": {"processes_stopped": True},
        }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--godot", type=Path, required=True)
    parser.add_argument("--sidecar", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--report", type=Path)
    parser.add_argument(
        "--additive-sprint11-registry",
        action="store_true",
        help="require the exact additive Sprint 11 41-tool profile",
    )
    args = parser.parse_args()
    try:
        result = run_workflow(
            args.godot.resolve(),
            args.sidecar.resolve(),
            args.timeout,
            additive_sprint11_registry=args.additive_sprint11_registry,
        )
    except (OSError, s9.WorkflowError) as error:
        print(f"Sprint 10 model-free workflow failed: {error}", file=s9.sys.stderr)
        return 1
    encoded = json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    if args.report is not None:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(encoded, encoding="utf-8")
    print(encoded, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
