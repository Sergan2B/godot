#!/usr/bin/env python3
"""Regression tests for the closed Sprint 5 evidence policy."""

from __future__ import annotations

import copy
import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from script_semantics_fixture import PHASES  # noqa: E402
from sprint5_acceptance import (  # noqa: E402
    MCP_CONTRACT_FIELDS,
    AcceptanceError,
    expected_performance,
    merge,
    strict_json_load,
    validate_platform_report,
)
from sprint5_script_semantics_live import apply_mutation, prepare_project  # noqa: E402


def digest(value: str) -> str:
    return "sha256:" + hashlib.sha256(value.encode()).hexdigest()


def telemetry() -> dict[str, Any]:
    return {
        "schema_version": 1,
        "budget_usec": 2000,
        "sample_capacity": 16384,
        "busy_frame_count": 1,
        "samples_usec": [100],
        "max_elapsed_usec": 100,
        "over_budget_count": 0,
        "overflow": False,
    }


def accuracy() -> dict[str, Any]:
    return {
        "zero_false_exact": True,
        "resolvable_truth_total": 9,
        "resolvable_truth_matched": 9,
        "resolvable_recall": 1.0,
        "dynamic_truth_total": 5,
        "dynamic_false_exact": 0,
        "symbol_truth_total": 48,
        "symbol_truth_matched": 48,
    }


def report(platform_tag: str) -> dict[str, Any]:
    phases = [
        {
            "phase": phase,
            "initial_script_graph_revision": 2,
            "script_graph_revision": 3,
            "index_revision": 3,
            "document_count": 8,
            "symbol_count": 48,
            "relation_count": 20,
            "diagnostic_count": 4,
            "normalized_script_sha256": digest(phase),
            "assertions": {"phase_semantics_verified": True},
            "cached_symbol_query_samples_ms": [1.0],
            "bulk_status_ping_samples_ms": [1.0] if phase == "journal_gap" else [],
            "change_visibility_ms": None if phase in {"base", "journal_gap"} else 100.0,
            "bridge_main_thread_sessions": [telemetry()],
            "passed": True,
        }
        for phase in PHASES
    ]
    return {
        "schema_version": 1,
        "sprint": 5,
        "profile": "qualifying",
        "platform": platform_tag,
        "source": {
            "git_commit": "a" * 40,
            "relevant_source_sha256": digest("source"),
            "relevant_source_clean": True,
            "tracked_file_count": 10,
            "scope_manifest": "tests/codex/sprint5_source_scopes.txt",
            "fixture_sha256": digest("fixture"),
            "fixture_manifest_sha256": digest("manifest"),
            "golden_script_graph_sha256": digest("golden"),
            "canonical_symbol_ids_sha256": digest("symbol-ids"),
        },
        "artifacts": {
            "godot_sha256": digest("godot"),
            "sidecar_sha256": digest("sidecar"),
            "script_probe_sha256": digest("probe"),
        },
        "versions": {
            "godot": "4.8",
            "sidecar": "0.1.0",
            "python": "CPython",
            "rustc": "rustc 1.94.1",
            "cargo": "cargo 1.94.1",
            "rust_target": (
                "aarch64-apple-darwin" if platform_tag == "macos-arm64" else "x86_64-pc-windows-msvc"
            ),
            "bridge_rpc": "1.4",
            "mcp_protocol": "2025-11-25",
            "logical_schema": "1.3",
            "physical_store": "segment-v3",
        },
        "adapter_profile": {
            "gdscript": "gdscript_parser_analyzer_v1",
            "csharp": "csharp_discovery_only_v1",
            "csharp_runtime_required": False,
        },
        "mcp_contract": {field: True for field in MCP_CONTRACT_FIELDS},
        "accuracy": accuracy(),
        "phases": phases,
        "performance": expected_performance(phases),
        "completion": {
            "requested_phases": list(PHASES),
            "completed_phases": list(PHASES),
            "all_phases_completed": True,
            "script_mcp_contract_verified": True,
            "fixture_integrity_preserved": True,
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


class Sprint5AcceptanceTests(unittest.TestCase):
    def test_platform_report_recomputes_raw_slos(self) -> None:
        value = report("macos-arm64")
        self.assertIs(validate_platform_report(value, "macos-arm64", check_checkout=False), value)
        value["performance"]["cached_symbol_query_ms"]["p95"] = 999
        with self.assertRaises(AcceptanceError):
            validate_platform_report(value, "macos-arm64", check_checkout=False)

    def test_accuracy_is_fail_closed(self) -> None:
        value = report("macos-arm64")
        value["accuracy"]["resolvable_truth_matched"] = 8
        with self.assertRaises(AcceptanceError):
            validate_platform_report(value, "macos-arm64", check_checkout=False)

    def test_linux_is_not_an_acceptance_coordinate(self) -> None:
        value = report("macos-arm64")
        value["platform"] = "linux-x86_64"
        with self.assertRaises(AcceptanceError):
            validate_platform_report(value, check_checkout=False)

    def test_nested_fields_and_private_paths_fail_closed(self) -> None:
        extra = report("macos-arm64")
        extra["artifacts"]["unbound_artifact"] = digest("extra")
        with self.assertRaises(AcceptanceError):
            validate_platform_report(extra, "macos-arm64", check_checkout=False)

        leaked = report("macos-arm64")
        leaked["versions"]["python"] = "/Users/example/private/python"
        with self.assertRaises(AcceptanceError):
            validate_platform_report(leaked, "macos-arm64", check_checkout=False)

    def test_strict_loader_rejects_nonfinite_numbers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory) / "evidence.json"
            evidence.write_text('{"sample": NaN}', encoding="utf-8")
            with self.assertRaises(AcceptanceError):
                strict_json_load(evidence)

    def test_all_mutation_phases_preserve_lf(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for phase in PHASES:
                if phase == "base":
                    continue
                project, payload = prepare_project(root, phase)
                apply_mutation(project, phase, payload)
                candidates = [
                    path
                    for path in project.rglob("*")
                    if path.is_file() and path.suffix in {".gd", ".cs", ".tscn", ".tres"}
                ]
                self.assertTrue(candidates)
                self.assertTrue(all(b"\r" not in path.read_bytes() for path in candidates), phase)

    def test_merge_requires_cross_platform_semantic_parity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            macos = root / "macos.json"
            windows = root / "windows.json"
            output = root / "aggregate.json"
            macos.write_text(json.dumps(report("macos-arm64")), encoding="utf-8")
            windows_report = report("windows-x86_64")
            windows.write_text(json.dumps(windows_report), encoding="utf-8")
            result = merge(macos, windows, output, check_checkout=False)
            self.assertEqual(result["status"], "passed")
            self.assertEqual(list(result["phase_semantic_digests"]), list(PHASES))

            broken = copy.deepcopy(windows_report)
            broken["phases"][1]["normalized_script_sha256"] = digest("different")
            windows.write_text(json.dumps(broken), encoding="utf-8")
            with self.assertRaises(AcceptanceError):
                merge(macos, windows, output, check_checkout=False)


if __name__ == "__main__":
    unittest.main()
