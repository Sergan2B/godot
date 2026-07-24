#!/usr/bin/env python3
"""Fail-closed policy tests for the Sprint 9 acceptance evidence."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from tests.codex import sprint9_acceptance as acceptance

SHA = "sha256:" + ("1" * 64)


def valid_report() -> dict[str, object]:
    gates = {
        name: {"duration_ms": 1.0, "status": "passed"}
        for name in acceptance.LOCAL_GATES
    }
    identities = [
        "sha256:" + f"{index:064x}"
        for index in range(1, 9)
    ]
    return {
        "schema_version": "s9-editor-transactions-evidence/1.0",
        "sprint": 9,
        "profile": "qualifying_local_macos_arm64",
        "platform": {"architecture": "arm64", "os": "macos"},
        "source": {
            "commit": "1" * 40,
            "sha256": SHA,
            "file_count": 1,
            "scope_manifest": "tests/codex/sprint9_source_scopes.txt",
            "fixture_manifest_sha256": SHA,
            "golden_sha256": SHA,
        },
        "toolchain": {
            "python": "3.13.5",
            "rustc": "rustc 1.88.0",
            "cargo": "cargo 1.88.0",
            "rust_host": "aarch64-apple-darwin",
            "scons": "SCons 4.9.1",
            "xcode": "Xcode 16.4",
            "macos_sdk": "15.5",
        },
        "protocols": {
            "bridge_rpc": "1.7",
            "mcp": "2025-11-25",
            "tools": 36,
        },
        "artifacts": {
            "godot_sha256": SHA,
            "sidecar_sha256": SHA,
            "model_free_runner_sha256": SHA,
            "validator_sha256": SHA,
            "model_free_report_sha256": SHA,
        },
        "summary": {
            "operations": 8,
            "primary_transactions": 8,
            "fault_scenarios": 7,
            "elicitation_count": 13,
            "transaction_native_actions": 8,
            "intervening_native_actions": 4,
        },
        "transaction_identity_sha256": identities,
        "checks": {field: True for field in acceptance.CHECK_FIELDS},
        "lifecycle": {
            "prepare_read_only": True,
            "apply_exactly_once": True,
            "targeted_undo_exact": True,
            "native_redo_same_transaction": True,
            "revision_relations_exact": True,
        },
        "recovery": {
            "sidecar_disconnect_prepared": "immutable_reprepare",
            "bridge_response_loss": "committed_without_replay",
            "disconnect_before_commit": "failed_without_action",
            "journal_ack_restart": "committed_without_replay",
            "editor_crash_before_commit": "pre_state_new_session",
            "editor_restart_after_commit": "no_persistent_undo",
            "corrupt_journal": "quarantined_reads_available",
        },
        "performance": {
            name: {"p50_ms": 1.0, "p95_ms": 2.0, "max_ms": 3.0}
            for name in ("apply", "prepare", "status", "undo")
        }
        | {
            "bridge_dispatcher": {
                "budget_usec": 2_000,
                "over_budget_count": 0,
                "within_budget": True,
            }
        },
        "local_gates": gates,
        "cleanup": {
            "editor_processes_stopped": True,
            "game_processes_stopped": True,
            "sidecar_processes_stopped": True,
            "temporary_workspaces_removed": True,
        },
        "redaction": {
            "absolute_paths_absent": True,
            "approval_material_absent": True,
            "canaries_absent": True,
            "native_ids_absent": True,
        },
        "external_gates": {
            name: {"status": "not_run", "reason": "outside local coordinate"}
            for name in ("linux", "model_facing", "remote_ci", "windows")
        },
        "status": "passed",
    }


class Sprint9AcceptanceTests(unittest.TestCase):
    def assert_rejected(self, report: dict[str, object]) -> None:
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_report(report, check_checkout=False)

    def test_valid_report_passes_without_checkout_binding(self) -> None:
        report = valid_report()
        self.assertIs(
            acceptance.validate_report(report, check_checkout=False),
            report,
        )

    def test_windows_report_passes_without_checkout_binding(self) -> None:
        report = valid_report()
        report["profile"] = "qualifying_local_windows_x86_64"
        report["platform"] = {"architecture": "x86_64", "os": "windows"}
        report["toolchain"] = {
            "python": "3.14.5",
            "rustc": "rustc 1.94.1",
            "cargo": "cargo 1.94.1",
            "rust_host": "x86_64-pc-windows-msvc",
            "scons": "SCons 4.10.1",
            "msvc": "17.14.37216.2",
            "windows_sdk": "10.0.26100.0",
        }
        external = report["external_gates"]
        assert isinstance(external, dict)
        external["macos"] = external.pop("windows")
        self.assertIs(
            acceptance.validate_report(report, check_checkout=False),
            report,
        )

    def test_strict_json_rejects_duplicate_members(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "duplicate.json"
            path.write_text('{"status":"passed","status":"failed"}')
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.strict_json_load(path)

    def test_strict_json_rejects_non_finite_numbers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "non-finite.json"
            path.write_text('{"duration":NaN}')
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.strict_json_load(path)

    def test_top_level_extension_is_rejected(self) -> None:
        report = valid_report()
        report["unreviewed_claim"] = True
        self.assert_rejected(report)

    def test_performance_threshold_is_fail_closed(self) -> None:
        report = valid_report()
        performance = report["performance"]
        assert isinstance(performance, dict)
        prepare = performance["prepare"]
        assert isinstance(prepare, dict)
        prepare["p95_ms"] = 500.001
        self.assert_rejected(report)

    def test_transaction_identity_must_be_unique(self) -> None:
        report = valid_report()
        identities = report["transaction_identity_sha256"]
        assert isinstance(identities, list)
        identities[-1] = identities[0]
        self.assert_rejected(report)

    def test_external_gate_cannot_claim_success(self) -> None:
        report = valid_report()
        external = report["external_gates"]
        assert isinstance(external, dict)
        windows = external["windows"]
        assert isinstance(windows, dict)
        windows["status"] = "passed"
        self.assert_rejected(report)

    def test_absolute_path_leak_is_rejected(self) -> None:
        report = valid_report()
        external = report["external_gates"]
        assert isinstance(external, dict)
        linux = external["linux"]
        assert isinstance(linux, dict)
        linux["reason"] = "not run under /Users/operator/workspace"
        self.assert_rejected(report)

    def test_source_scope_is_sorted_unique_and_self_bound(self) -> None:
        scopes = acceptance.source_scopes()
        self.assertEqual(scopes, tuple(sorted(set(scopes))))
        self.assertIn("docs/codex-integration/SPRINT-9-WINDOWS.md", scopes)
        self.assertIn("editor/run/game_view_plugin.cpp", scopes)
        self.assertIn("tests/codex/sprint9_acceptance.py", scopes)
        self.assertIn("tests/codex/test_sprint9_acceptance.py", scopes)

    def test_serialized_evidence_is_bounded(self) -> None:
        report = valid_report()
        external = report["external_gates"]
        assert isinstance(external, dict)
        linux = external["linux"]
        assert isinstance(linux, dict)
        linux["reason"] = "x" * 70_000
        self.assert_rejected(report)


if __name__ == "__main__":
    unittest.main()
