#!/usr/bin/env python3
"""Unit tests for the Sprint 3 evidence validator."""

from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path

from sprint3_acceptance import AcceptanceError, PHASES, TOOLS, merge_acceptance, validate_live, validate_storage


def metric(samples: list[int | float]) -> dict[str, object]:
    ordered = sorted(samples)
    return {"samples": samples, "p50": ordered[0], "p95": ordered[-1]}


def live_evidence(platform: str, graph_suffix: str = "") -> dict[str, object]:
    phases: list[dict[str, object]] = []
    for name in PHASES:
        phase: dict[str, object] = {
            "phase": name,
            "passed": True,
            "direct_reverse_oracle_match": True,
            "normalized_graph_sha256": f"sha256:{name}{graph_suffix}",
            "bridge_main_thread": {
                "schema_version": 1,
                "budget_usec": 2000,
                "sample_capacity": 16384,
                "busy_frame_count": 1,
                "samples_usec": [100],
                "max_elapsed_usec": 100,
                "over_budget_count": 0,
                "overflow": False,
            },
        }
        if name == "base":
            phase["same_generation_after_reopen"] = True
            phase["immutable_segment_set_reused"] = True
        if name == "journal_gap":
            phase["full_rebuild_after_gap"] = True
        phases.append(phase)
    return {
        "schema_version": 2,
        "stage": "Sprint 3 Stage 5 / S3-09-S3-10",
        "status": "passed",
        "execution": "local_model_free",
        "profile": "acceptance",
        "platform": platform,
        "host": "fixture-host",
        "git_commit": "fixture-commit",
        "git_dirty": False,
        "source_tree_sha256": "sha256:live-source",
        "artifacts": {
            "godot_version": "fixture-godot",
            "godot_sha256": "sha256:godot",
            "sidecar_version": "fixture-sidecar",
            "sidecar_sha256": "sha256:sidecar",
        },
        "mcp_protocol": "2025-11-25",
        "tools": sorted(TOOLS),
        "oracle_sha256": "sha256:oracle",
        "fixture_digest_before": "fixture",
        "fixture_digest_after": "fixture",
        "canonical_fixture_unchanged": True,
        "phases": phases,
        "metrics_ms": {
            "cached_resource_query": metric([1]),
            "ordinary_incremental_visibility": metric([2]),
            "bulk_status_ping": metric([3]),
            "startup_visibility": metric([4]),
            "compatible_reopen": metric([5]),
            "journal_gap_full_rebuild": metric([6]),
        },
        "bridge_main_thread": {
            **metric([100] * len(PHASES)),
            "unit": "microseconds",
            "budget_usec": 2000,
            "busy_frame_count": len(PHASES),
            "max_elapsed_usec": 100,
            "over_budget_count": 0,
            "overflow": False,
        },
        "slo": {
            "cached_resource_query_p95_lte_300ms": True,
            "ordinary_incremental_visibility_p95_lte_2000ms": True,
            "bulk_status_ping_p95_lte_200ms": True,
            "bridge_main_thread_over_2000us_zero": True,
            "all_passed": True,
        },
        "platform_evidence": {"remote_ci": "not_run"},
    }


def storage_evidence() -> dict[str, object]:
    runs = []
    for os_name in ("linux", "macos", "windows"):
        runs.append(
            {
                "schema_version": 2,
                "decision": "D-05",
                "profile": "decision",
                "git_commit": "fixture-commit",
                "git_dirty": False,
                "source_tree_sha256": "sha256:storage-source",
                "oracle_sha256": "sha256:oracle",
                "os": os_name,
                "backends": [
                    {
                        "backend": "segment",
                        "qualified": True,
                        "gates": {"correctness": True},
                        "fault_matrix": {"hard_kill.pre_commit": True},
                    }
                ],
            }
        )
    return {
        "schema_version": 2,
        "decision": "D-05",
        "platform_runs": runs,
        "backend_summaries": [],
        "cross_platform_complete": True,
        "chosen_backend": "segment",
        "decision_reason": "fixture",
    }


class Sprint3AcceptanceTests(unittest.TestCase):
    def write(self, root: Path, name: str, value: object) -> Path:
        path = root / name
        path.write_text(json.dumps(value, sort_keys=True), encoding="utf-8")
        return path

    def test_validates_and_merges_complete_local_platform_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            macos = self.write(root, "macos.json", live_evidence("macos-arm64"))
            windows = self.write(root, "windows.json", live_evidence("windows-x86_64"))
            storage = self.write(root, "storage.json", storage_evidence())
            output = root / "acceptance.json"
            self.assertEqual(validate_live(macos)["evidence"]["platform"], "macos-arm64")
            self.assertEqual(validate_storage(storage)["evidence"]["chosen_backend"], "segment")
            merged = merge_acceptance(macos, windows, storage, output)
            self.assertEqual(merged["status"], "passed")
            self.assertEqual(len(merged["acceptance_criteria"]), 12)
            self.assertTrue(output.is_file())

    def test_rejects_summary_not_derived_from_raw_samples(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            evidence = live_evidence("macos-arm64")
            evidence["metrics_ms"]["cached_resource_query"]["p95"] = 999  # type: ignore[index]
            path = self.write(root, "macos.json", evidence)
            with self.assertRaisesRegex(AcceptanceError, "p95"):
                validate_live(path)

    def test_rejects_cross_platform_graph_difference_and_dirty_storage(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            macos = self.write(root, "macos.json", live_evidence("macos-arm64"))
            windows = self.write(root, "windows.json", live_evidence("windows-x86_64", "-different"))
            storage_value = storage_evidence()
            storage = self.write(root, "storage.json", storage_value)
            with self.assertRaisesRegex(AcceptanceError, "normalized graphs"):
                merge_acceptance(macos, windows, storage, root / "acceptance.json")

            dirty = copy.deepcopy(storage_value)
            dirty["platform_runs"][0]["git_dirty"] = True  # type: ignore[index]
            dirty_path = self.write(root, "dirty-storage.json", dirty)
            with self.assertRaisesRegex(AcceptanceError, "quick or dirty"):
                validate_storage(dirty_path)


if __name__ == "__main__":
    unittest.main()
