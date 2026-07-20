#!/usr/bin/env python3
"""Regression tests for the fail-closed Sprint 6 local evidence policy."""

from __future__ import annotations

import copy
import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from sprint6_acceptance import (  # noqa: E402
    LOCAL_GATES,
    MCP_FIELDS,
    PHASES,
    AcceptanceError,
    source_scopes,
    strict_json_load,
    validate_report,
)


def digest(value: str) -> str:
    return "sha256:" + hashlib.sha256(value.encode()).hexdigest()


def report() -> dict[str, Any]:
    samples = [0.1, 0.2, 0.3, 0.4, 0.5]
    query = {
        "target_entity_id": "godot:test",
        "expected_pairs": [["references", "exact"]],
        "actual_pairs": [["references", "exact"]],
        "missing_pairs": [],
        "unexpected_pairs": [],
        "usage_count": 1,
    }
    return {
        "schema_version": 1,
        "sprint": 6,
        "profile": "qualifying_local_macos",
        "platform": "macos-arm64",
        "source": {
            "git_commit": "a" * 40,
            "relevant_source_sha256": digest("source"),
            "relevant_source_clean": True,
            "tracked_file_count": 10,
            "scope_manifest": "tests/codex/sprint6_source_scopes.txt",
            "fixture_manifest_sha256": digest("manifest"),
            "golden_usages_sha256": digest("golden"),
        },
        "versions": {
            "godot": "4.8.dev",
            "sidecar": "0.1.0",
            "python": "3.14.0",
            "rustc": "rustc 1.94.1",
            "cargo": "cargo 1.94.1",
            "rust_target": "aarch64-apple-darwin",
            "bridge_rpc": "1.4",
            "mcp_protocol": "2025-11-25",
            "logical_schema": "1.3",
            "physical_store": "segment-v3",
        },
        "artifacts": {
            "godot_sha256": digest("godot"),
            "sidecar_sha256": digest("sidecar"),
            "fixture_manifest_sha256": digest("manifest"),
            "golden_usages_sha256": digest("golden"),
        },
        "coordinates": {
            "generation_id": "generation-test",
            "index_revision": 1,
            "resource_revision": 1,
            "scene_graph_revision": 1,
            "script_graph_revision": 1,
        },
        "mcp_contract": {field: True for field in MCP_FIELDS},
        "accuracy": {
            "deduplication": {
                "fact_count": 1,
                "evidence_count": 2,
                "evidence_sources": ["scene_node_attachment", "scene_relation"],
            },
            "dynamic_false_exact": 0,
            "queries": {
                "resource_profile": copy.deepcopy(query),
                "scene_child": copy.deepcopy(query),
                "signal_healed": copy.deepcopy(query),
                "symbol_take_damage": copy.deepcopy(query),
            },
            "resolvable_recall": 1.0,
            "resolvable_truth_matched": 7,
            "resolvable_truth_total": 7,
            "zero_false_exact": True,
        },
        "summaries": {
            "project_budget": 4096,
            "project_bytes": 4000,
            "scene_budget": 2048,
            "scene_bytes": 2000,
        },
        "performance": {
            "find_usages_samples_ms": samples,
            "find_usages_p95_ms": 0.5,
            "summary_samples_ms": samples,
            "summary_p95_ms": 0.5,
            "control_ping_samples_ms": samples,
            "control_ping_p95_ms": 0.5,
        },
        "live_phases": {
            "base": {
                "status": "passed",
                "index_revision": 1,
                "resource_target_entity_id": "godot:resource:test",
            },
            "resource_uid_rename": {
                "status": "passed",
                "index_revision": 2,
                "resource_target_entity_id": "godot:resource:test",
                "identity_preserved": True,
            },
            "symbol_rename": {
                "status": "passed",
                "index_revision": 3,
                "symbol_target_entity_id": "godot:symbol:test",
                "old_selector_not_found": True,
            },
        },
        "phase_coverage": {
            phase: {
                "status": "passed",
                "mode": (
                    "live_oracle"
                    if phase
                    in {"base", "resource_uid_rename", "symbol_rename", "duplicate_provenance"}
                    else "deterministic_regression"
                ),
            }
            for phase in PHASES
        },
        "local_gates": {
            gate: {"status": "passed", "duration_ms": 1.0} for gate in LOCAL_GATES
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
            "windows": {"status": "not_run", "reason": "deferred"},
            "linux": {"status": "not_run", "reason": "not in scope"},
            "remote_ci": {"status": "not_run", "reason": "local gate"},
        },
        "status": "passed",
    }


class Sprint6AcceptanceTests(unittest.TestCase):
    def test_valid_report_passes_without_checkout_binding(self) -> None:
        value = report()
        self.assertIs(validate_report(value, check_checkout=False), value)

    def test_accuracy_and_latency_fail_closed(self) -> None:
        false_exact = report()
        false_exact["accuracy"]["zero_false_exact"] = False
        with self.assertRaises(AcceptanceError):
            validate_report(false_exact, check_checkout=False)

        forged_p95 = report()
        forged_p95["performance"]["find_usages_p95_ms"] = 0.1
        with self.assertRaises(AcceptanceError):
            validate_report(forged_p95, check_checkout=False)

    def test_external_gate_cannot_be_claimed(self) -> None:
        value = report()
        value["external_gates"]["windows"]["status"] = "passed"
        with self.assertRaises(AcceptanceError):
            validate_report(value, check_checkout=False)

    def test_private_paths_and_extra_fields_are_rejected(self) -> None:
        leaked = report()
        leaked["codex_cli_smoke"]["reason"] = "/Users/example/private"
        with self.assertRaises(AcceptanceError):
            validate_report(leaked, check_checkout=False)

        extra = report()
        extra["accuracy"]["unbound"] = True
        with self.assertRaises(AcceptanceError):
            validate_report(extra, check_checkout=False)

    def test_strict_loader_rejects_duplicate_and_nonfinite_json(self) -> None:
        with tempfile.TemporaryDirectory(prefix="s6-evidence-test.") as directory:
            path = Path(directory) / "evidence.json"
            path.write_text('{"status":"passed","status":"passed"}', encoding="utf-8")
            with self.assertRaises(AcceptanceError):
                strict_json_load(path)
            path.write_text('{"sample":NaN}', encoding="utf-8")
            with self.assertRaises(AcceptanceError):
                strict_json_load(path)

    def test_source_scope_is_sorted_and_self_bound(self) -> None:
        scopes = source_scopes()
        self.assertEqual(tuple(sorted(set(scopes))), scopes)
        self.assertIn("tests/codex/sprint6_source_scopes.txt", scopes)


if __name__ == "__main__":
    unittest.main()
