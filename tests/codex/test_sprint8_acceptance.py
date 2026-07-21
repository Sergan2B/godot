#!/usr/bin/env python3
"""Regression tests for the fail-closed Sprint 8 evidence policy."""

from __future__ import annotations

import base64
import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from sprint8_acceptance import (  # noqa: E402
    CHECK_FIELDS,
    EVIDENCE_PATH,
    LOCAL_GATES,
    AcceptanceError,
    require_new_output,
    source_scopes,
    strict_json_load,
    validate_report,
)
from sprint8_runtime_live import (  # noqa: E402
    count_rgb_pixels,
    decode_png_rgb,
    finalize_live_report,
)


def digest(value: str) -> str:
    return "sha256:" + hashlib.sha256(value.encode()).hexdigest()


def report() -> dict[str, Any]:
    transition_samples = [10.0, 20.0, 30.0]
    return {
        "schema_version": 1,
        "sprint": 8,
        "profile": "qualifying_local_macos",
        "platform": "macos-arm64",
        "source": {
            "git_commit": "a" * 40,
            "relevant_source_sha256": digest("source"),
            "relevant_source_clean": True,
            "tracked_file_count": 10,
            "scope_manifest": "tests/codex/sprint8_source_scopes.txt",
            "fixture_manifest_sha256": digest("manifest"),
            "golden_runtime_sha256": digest("golden"),
        },
        "versions": {
            "godot": "4.8.dev",
            "sidecar": "0.1.0",
            "python": "3.14.0",
            "rustc": "rustc 1.94.1",
            "cargo": "cargo 1.94.1",
            "rust_target": "aarch64-apple-darwin",
            "bridge_rpc": "1.6",
            "mcp_protocol": "2025-11-25",
        },
        "artifacts": {
            "godot_sha256": digest("godot"),
            "sidecar_sha256": digest("sidecar"),
            "live_runner_sha256": digest("runner"),
            "fixture_validator_sha256": digest("fixture"),
        },
        "sessions": [f"runtime:{index:032x}" for index in range(12)],
        "checks": {field: True for field in CHECK_FIELDS},
        "observations": {
            "runtime_nodes": 9,
            "large_runtime_nodes": 10000,
            "diagnostics": 4,
            "stack_frames": 5,
        },
        "performance": {
            "large_tree_page_p95_ms": 30.0,
            "transition_visibility_samples_ms": transition_samples,
            "transition_visibility_p95_ms": 30.0,
            "viewport_capture_ms": 100.0,
            "hung_game_bridge_ping_p95_ms": 1.0,
            "hung_tree_timeout_ms": 3000.0,
            "hung_game_stop_ms": 50.0,
        },
        "local_gates": {
            field: {"status": "passed", "duration_ms": 1.0}
            for field in LOCAL_GATES
        },
        "cleanup": {
            "editor_processes_stopped": True,
            "sidecar_processes_stopped": True,
            "temporary_workspace_removed": True,
            "runtime_values_retired": True,
        },
        "redaction": {
            "absolute_paths_absent": True,
            "bridge_endpoints_absent": True,
            "native_handles_absent": True,
            "secret_material_absent": True,
        },
        "external_gates": {
            "windows": {"status": "not_run", "reason": "local coordinate"},
            "linux": {"status": "not_run", "reason": "unavailable"},
            "remote_ci": {"status": "not_run", "reason": "not configured"},
            "model_facing": {"status": "not_run", "reason": "deterministic gate"},
        },
        "status": "passed",
    }


class Sprint8AcceptanceTests(unittest.TestCase):
    def test_default_evidence_is_v2_and_historical_v1_remains_distinct(self) -> None:
        self.assertEqual(EVIDENCE_PATH.name, "sprint-8-runtime-macos-v2.json")
        historical = EVIDENCE_PATH.with_name("sprint-8-runtime-macos.json")
        self.assertTrue(historical.is_file())
        self.assertNotEqual(EVIDENCE_PATH, historical)

    def test_runtime_png_decoder_observes_fixture_pixels(self) -> None:
        fixture = json.loads(
            (
                SCRIPT_DIR.parent.parent
                / "schemas/codex_bridge/v1/fixtures/valid/runtime-viewport-capture-response.json"
            ).read_text(encoding="utf-8")
        )
        png = base64.urlsafe_b64decode(fixture["result"]["data_base64url"] + "==")
        width, height, pixels = decode_png_rgb(png)
        self.assertEqual((width, height), (2, 2))
        self.assertEqual(count_rgb_pixels(pixels, [31, 158, 240], 0), 2)
        self.assertEqual(count_rgb_pixels(pixels, [255, 115, 31], 0), 2)

    def test_valid_report_passes_without_checkout_binding(self) -> None:
        value = report()
        self.assertIs(validate_report(value, check_checkout=False), value)

    def test_sessions_slo_and_external_claims_fail_closed(self) -> None:
        duplicate = report()
        duplicate["sessions"][-1] = duplicate["sessions"][0]
        with self.assertRaises(AcceptanceError):
            validate_report(duplicate, check_checkout=False)

        slow = report()
        slow["performance"]["viewport_capture_ms"] = 3001.0
        with self.assertRaises(AcceptanceError):
            validate_report(slow, check_checkout=False)

        early_timeout = report()
        early_timeout["performance"]["hung_tree_timeout_ms"] = 2499.0
        with self.assertRaises(AcceptanceError):
            validate_report(early_timeout, check_checkout=False)

        slow_stop = report()
        slow_stop["performance"]["hung_game_stop_ms"] = 5001.0
        with self.assertRaises(AcceptanceError):
            validate_report(slow_stop, check_checkout=False)

        claimed = report()
        claimed["external_gates"]["windows"]["status"] = "passed"
        with self.assertRaises(AcceptanceError):
            validate_report(claimed, check_checkout=False)

    def test_private_material_and_extra_fields_are_rejected(self) -> None:
        leaked = report()
        leaked["external_gates"]["linux"]["reason"] = "/Users/example/private"
        with self.assertRaises(AcceptanceError):
            validate_report(leaked, check_checkout=False)

        extra = report()
        extra["checks"]["unbound"] = True
        with self.assertRaises(AcceptanceError):
            validate_report(extra, check_checkout=False)

        for section, field in (
            ("cleanup", "editor_processes_stopped"),
            ("redaction", "secret_material_absent"),
        ):
            weakened = report()
            weakened[section][field] = False
            with self.subTest(section=section), self.assertRaises(AcceptanceError):
                validate_report(weakened, check_checkout=False)

    def test_live_report_cleanup_is_checked_after_workspace_removal(self) -> None:
        with tempfile.TemporaryDirectory(prefix="s8-live-report-test.") as directory:
            value = {
                "_temporary_workspace": directory,
                "cleanup": {
                    "editor_processes_stopped": True,
                    "sidecar_processes_stopped": True,
                    "temporary_workspace_removed": False,
                    "runtime_values_retired": True,
                },
                "redaction": {
                    "absolute_paths_absent": True,
                    "bridge_endpoints_absent": True,
                    "native_handles_absent": True,
                    "secret_material_absent": True,
                },
            }
        finalized = finalize_live_report(value)
        self.assertTrue(finalized["cleanup"]["temporary_workspace_removed"])
        self.assertNotIn("_temporary_workspace", finalized)

    def test_strict_loader_and_overwrite_policy_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="s8-evidence-test.") as directory:
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
        self.assertIn("tests/codex/sprint8_source_scopes.txt", scopes)


if __name__ == "__main__":
    unittest.main()
