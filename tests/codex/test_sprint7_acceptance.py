#!/usr/bin/env python3
"""Regression tests for the fail-closed Sprint 7 local evidence policy."""

from __future__ import annotations

import copy
import hashlib
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from sprint7_acceptance import (  # noqa: E402
    CHECK_FIELDS,
    LOCAL_GATES,
    MCP_FIELDS,
    PHASES,
    AcceptanceError,
    require_new_output,
    source_scopes,
    strict_json_load,
    validate_report,
)


def digest(value: str) -> str:
    return "sha256:" + hashlib.sha256(value.encode()).hexdigest()


def report() -> dict[str, Any]:
    samples = [1.0, 2.0, 3.0, 4.0, 5.0] * 4
    transition = {
        "operation_seq": 1,
        "dirty_scene_revision": 3,
        "clean_scene_revision": 1,
        "history_transition": "commit",
        "history_operation_kind": "opaque",
    }
    value: dict[str, Any] = {
        "schema_version": 1,
        "sprint": 7,
        "profile": "qualifying_local_macos",
        "platform": "macos-arm64",
        "source": {
            "git_commit": "a" * 40,
            "relevant_source_sha256": digest("source"),
            "relevant_source_clean": True,
            "tracked_file_count": 10,
            "scope_manifest": "tests/codex/sprint7_source_scopes.txt",
            "fixture_manifest_sha256": digest("manifest"),
            "golden_live_editor_sha256": digest("golden"),
        },
        "versions": {
            "godot": "4.8.dev",
            "sidecar": "0.1.0",
            "python": "3.14.0",
            "rustc": "rustc 1.94.1",
            "cargo": "cargo 1.94.1",
            "rust_target": "aarch64-apple-darwin",
            "bridge_rpc": "1.5",
            "mcp_protocol": "2025-11-25",
        },
        "artifacts": {
            "godot_sha256": digest("godot"),
            "sidecar_sha256": digest("sidecar"),
            "fixture_manifest_sha256": digest("manifest"),
            "golden_live_editor_sha256": digest("golden"),
            "live_runner_sha256": digest("runner"),
        },
        "coordinates": {
            "editor_session_id": "editor:" + "1" * 32,
            "initial_snapshot_id": "snapshot:" + "2" * 32,
            "initial_event_seq": 8,
            "final_event_seq": 19,
            "initial_operation_seq": 1,
            "final_operation_seq": 4,
            "dirty_scene_revision_initial": 3,
            "dirty_scene_revision_final": 5,
        },
        "mcp_contract": {field: True for field in MCP_FIELDS},
        "checks": {field: True for field in CHECK_FIELDS},
        "observations": {
            "open_scene_count": 2,
            "selected_node_count": 3,
            "open_script_count": 2,
            "diagnostic_count": 10,
            "redacted_diagnostic_count": 1,
            "editor_value": 21.5,
            "disk_value": 8.0,
            "editor_summary_bytes": 2048,
            "viewport_kind": "2d",
        },
        "performance": {
            "selection_samples_ms": samples,
            "selection_p95_ms": 5.0,
            "change_visibility_samples_ms": [100.0, 200.0, 300.0],
            "change_visibility_p95_ms": 300.0,
            "control_ping_samples_ms": samples,
            "control_ping_p95_ms": 5.0,
            "editor_summary_samples_ms": samples,
            "editor_summary_p95_ms": 5.0,
            "dispatcher_samples_usec": [100, 200, 300, 400, 500],
            "dispatcher_p95_usec": 500.0,
            "dispatcher_max_usec": 500,
            "dispatcher_budget_usec": 2000,
            "dispatcher_over_budget_count": 0,
        },
        "transitions": {
            "commit": copy.deepcopy(transition),
            "undo": {**transition, "operation_seq": 2, "dirty_scene_revision": 4, "history_transition": "undo"},
            "redo": {**transition, "operation_seq": 3, "dirty_scene_revision": 5, "history_transition": "redo"},
            "opaque": {**transition, "operation_seq": 4, "dirty_scene_revision": 5, "history_transition": "redo"},
        },
        "phase_coverage": {
            phase: {"status": "passed", "mode": "live_oracle"}
            for phase in PHASES
        },
        "local_gates": {
            gate: {"status": "passed", "duration_ms": 1.0}
            for gate in LOCAL_GATES
        },
        "codex_cli_smoke": {
            "status": "not_run",
            "blocking": False,
            "cli_available": True,
            "version": "codex-cli 1.0",
            "reason": "isolated profile unavailable",
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
        "external_gates": {
            "windows": {"status": "not_run", "reason": "local coordinate"},
            "linux": {"status": "not_run", "reason": "unavailable"},
            "remote_ci": {"status": "not_run", "reason": "not configured"},
            "model_facing": {"status": "not_run", "reason": "advisory"},
        },
        "status": "passed",
    }
    return value


class Sprint7AcceptanceTests(unittest.TestCase):
    def test_valid_report_passes_without_checkout_binding(self) -> None:
        value = report()
        self.assertIs(validate_report(value, check_checkout=False), value)

    def test_revision_and_latency_fail_closed(self) -> None:
        stale = report()
        stale["coordinates"]["final_event_seq"] = stale["coordinates"]["initial_event_seq"]
        with self.assertRaises(AcceptanceError):
            validate_report(stale, check_checkout=False)

        forged = report()
        forged["performance"]["selection_p95_ms"] = 0.1
        with self.assertRaises(AcceptanceError):
            validate_report(forged, check_checkout=False)

        stalled = report()
        stalled["performance"]["dispatcher_samples_usec"][-1] = 2001
        stalled["performance"]["dispatcher_p95_usec"] = 2001.0
        stalled["performance"]["dispatcher_max_usec"] = 2001
        with self.assertRaises(AcceptanceError):
            validate_report(stalled, check_checkout=False)

    def test_external_claim_private_path_and_extra_field_are_rejected(self) -> None:
        claimed = report()
        claimed["external_gates"]["windows"]["status"] = "passed"
        with self.assertRaises(AcceptanceError):
            validate_report(claimed, check_checkout=False)

        leaked = report()
        leaked["codex_cli_smoke"]["reason"] = "/Users/example/private"
        with self.assertRaises(AcceptanceError):
            validate_report(leaked, check_checkout=False)

        extra = report()
        extra["checks"]["unbound"] = True
        with self.assertRaises(AcceptanceError):
            validate_report(extra, check_checkout=False)

    def test_strict_loader_and_overwrite_policy_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="s7-evidence-test.") as directory:
            path = Path(directory) / "evidence.json"
            path.write_text('{"status":"passed","status":"passed"}', encoding="utf-8")
            with self.assertRaises(AcceptanceError):
                strict_json_load(path)
            path.write_text('{"sample":NaN}', encoding="utf-8")
            with self.assertRaises(AcceptanceError):
                strict_json_load(path)
            with self.assertRaises(AcceptanceError):
                require_new_output(path)

    def test_source_scope_is_sorted_and_self_bound(self) -> None:
        scopes = source_scopes()
        self.assertEqual(tuple(sorted(set(scopes))), scopes)
        self.assertIn("tests/codex/sprint7_source_scopes.txt", scopes)


if __name__ == "__main__":
    unittest.main()
