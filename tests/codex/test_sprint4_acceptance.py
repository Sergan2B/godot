#!/usr/bin/env python3
"""Regression tests for the closed Sprint 4 evidence policy."""

from __future__ import annotations

import copy
import hashlib
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

from scene_graph_fixture import PHASES  # noqa: E402
from sprint4_acceptance import (  # noqa: E402
    MCP_CONTRACT_FIELDS,
    AcceptanceError,
    expected_performance,
    merge,
    validate_platform_report,
)
from sprint4_scene_graph_live import PROJECT_SOURCE, apply_mutation  # noqa: E402


def digest(value: str) -> str:
    return "sha256:" + hashlib.sha256(value.encode()).hexdigest()


def assertions(phase: str) -> dict[str, bool]:
    value = {"canonical_occurrence_ids": True, "project_context": True, "property_override": True}
    if phase == "base":
        value.update({
            "animation_resolution": True,
            "attached_script": True,
            "external_and_nested_subresources": True,
            "inheritance_and_instances": True,
            "persistent_subresource_identity": True,
            "save_reopen_identity": True,
            "signal_and_group": True,
            "subresource_order_equivalence": True,
        })
    elif phase == "node_rename":
        value.update({"rename_identity_preserved": True, "duplicate_identity_distinct": True})
    elif phase == "node_reparent":
        value["reparent_identity_preserved"] = True
    elif phase == "instance_mutation":
        value["instance_closure_advanced"] = True
    elif phase == "signal_group":
        value["signal_group_atomic"] = True
    elif phase == "animation_fix":
        value["animation_paths_resolved"] = True
    elif phase == "journal_gap":
        value["gap_scene_recovered"] = True
    return value


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


def report(platform_tag: str) -> dict[str, Any]:
    phases = []
    for phase in PHASES:
        phases.append({
            "phase": phase,
            "startup_visibility_ms": 100.0,
            "reopen_visibility_ms": 100.0 if phase == "base" else None,
            "scene_graph_revision": 2,
            "index_revision": 2,
            "node_count": 10,
            "normalized_scene_sha256": digest(phase),
            "assertions": assertions(phase),
            "cached_scene_query_samples_ms": [1.0],
            "bulk_status_ping_samples_ms": [1.0] if phase == "journal_gap" else [],
            "change_visibility_ms": None if phase in {"base", "journal_gap"} else 100.0,
            "passed": True,
            "bridge_main_thread_sessions": [telemetry(), telemetry()] if phase == "base" else [telemetry()],
        })
    source = {
        "git_commit": "a" * 40,
        "relevant_source_sha256": digest("source"),
        "relevant_source_clean": True,
        "tracked_file_count": 10,
        "scope_manifest": "tests/codex/sprint4_source_scopes.txt",
        "fixture_sha256": digest("fixture"),
        "fixture_manifest_sha256": digest("manifest"),
        "golden_scene_graph_sha256": digest("golden"),
    }
    return {
        "schema_version": 1,
        "sprint": 4,
        "profile": "qualifying",
        "platform": platform_tag,
        "source": source,
        "artifacts": {"godot_sha256": digest("godot"), "sidecar_sha256": digest("sidecar")},
        "versions": {
            "godot": "4.8",
            "sidecar": "0.1.0",
            "python": "CPython",
            "rustc": "rustc 1.94.1",
            "cargo": "cargo 1.94.1",
            "rust_target": ("aarch64-apple-darwin" if platform_tag == "macos-arm64" else "x86_64-pc-windows-msvc"),
            "bridge_rpc": "1.3",
            "mcp_protocol": "2025-11-25",
            "logical_schema": "1.2",
            "physical_store": "segment-v2",
        },
        "mcp_contract": {field: True for field in MCP_CONTRACT_FIELDS},
        "phases": phases,
        "performance": expected_performance(phases),
        "completion": {
            "requested_phases": list(PHASES),
            "completed_phases": list(PHASES),
            "all_phases_completed": True,
            "scene_mcp_contract_verified": True,
            "fixture_integrity_preserved": True,
            "rust_workspace_tests_passed": True,
            "rust_clippy_passed": True,
            "scene_contract_tests_passed": True,
        },
        "cleanup": {"complete": True},
        "redaction": {"complete": True},
        "remote_ci": "not_run",
        "status": "passed",
    }


class Sprint4AcceptanceTests(unittest.TestCase):
    def test_live_mutations_preserve_cross_platform_lf_content_generations(self) -> None:
        for phase in PHASES[1:]:
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as temporary:
                project = Path(temporary) / "project"
                shutil.copytree(PROJECT_SOURCE, project)
                apply_mutation(project, phase)
                for scene in project.rglob("*.tscn"):
                    self.assertNotIn(b"\r", scene.read_bytes())

    def test_platform_report_recomputes_raw_slos(self) -> None:
        value = report("macos-arm64")
        self.assertIs(validate_platform_report(value, "macos-arm64", check_checkout=False), value)
        value["performance"]["cached_scene_query_ms"]["p95"] = 999
        with self.assertRaises(AcceptanceError):
            validate_platform_report(value, "macos-arm64", check_checkout=False)

    def test_linux_is_not_an_acceptance_coordinate(self) -> None:
        value = report("macos-arm64")
        value["platform"] = "linux-x86_64"
        with self.assertRaises(AcceptanceError):
            validate_platform_report(value, check_checkout=False)

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
            self.assertTrue(all(result["acceptance"].values()))

            broken = copy.deepcopy(windows_report)
            broken["phases"][1]["normalized_scene_sha256"] = digest("different")
            windows.write_text(json.dumps(broken), encoding="utf-8")
            with self.assertRaises(AcceptanceError):
                merge(macos, windows, output, check_checkout=False)


if __name__ == "__main__":
    unittest.main()
