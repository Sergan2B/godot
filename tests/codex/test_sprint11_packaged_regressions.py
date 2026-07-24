from __future__ import annotations

import copy
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

from tests.codex import sprint11_acceptance as acceptance
from tests.codex import sprint11_packaged_regressions as packaged


def synthetic_report(
    command_id: str,
    *,
    godot_sha256: str,
    sidecar_sha256: str,
) -> dict[str, Any]:
    artifacts = {
        "godot_sha256": godot_sha256,
        "sidecar_sha256": sidecar_sha256,
    }
    if command_id == "s6_semantic":
        return {
            "schema_version": 1,
            "sprint": 6,
            "profile": "model_free_live_smoke",
            "status": "passed",
            "artifacts": artifacts,
            "mcp_contract": {
                "exact_ten_tool_registry": True,
                "closed_input_schemas": True,
                "read_only_annotations": True,
            },
            "cleanup": {"processes_stopped": True},
            "redaction": {"secrets_absent": True},
        }
    if command_id == "s7_editor":
        return {
            "schema_version": 1,
            "sprint": 7,
            "profile": "model_free_live_editor",
            "status": "passed",
            "artifacts": artifacts,
            "checks": {"live_overlay": True, "restart": True},
            "cleanup": {"processes_stopped": True},
        }
    if command_id == "s8_runtime":
        return {
            "schema_version": 1,
            "sprint": 8,
            "status": "passed",
            "headless": True,
            "artifacts": artifacts,
            "checks": {
                "runtime_tree_and_opaque_ids": True,
                "bounded_properties": True,
                "diagnostic_stack": True,
                "fresh_session_per_run": True,
                "hang_timeout_and_recovery": True,
            },
            "cleanup": {"processes_stopped": True},
            "redaction": {"secrets_absent": True},
        }
    if command_id.startswith("s9_"):
        operations: list[dict[str, Any]] = []
        negatives: dict[str, Any] = {}
        faults: dict[str, Any] = {}
        if command_id == "s9_operations_a":
            names = {
                "attach_script",
                "connect_signal",
                "create_node",
                "delete_node",
            }
            operations = [
                {
                    "operation": name,
                    "oracle_cycle": True,
                    "source_unchanged": True,
                    "transaction_native_actions": 1,
                }
                for name in sorted(names)
            ]
        elif command_id == "s9_operations_b":
            names = {
                "detach_script",
                "disconnect_signal",
                "reparent_node",
                "set_property",
            }
            operations = [
                {
                    "operation": name,
                    "oracle_cycle": True,
                    "source_unchanged": True,
                    "transaction_native_actions": 1,
                }
                for name in sorted(names)
            ]
        elif command_id == "s9_negatives":
            negatives = {
                "idempotency": {},
                "committed_replay": {},
                "expired_preview": {},
                "approval": [
                    {"decision": decision}
                    for decision in (
                        "confirm_false",
                        "decline",
                        "cancel",
                        "unsupported",
                        "timeout",
                    )
                ],
            }
        elif command_id == "s9_faults_a":
            faults = {
                name: {}
                for name in (
                    "sidecar_disconnect_prepared",
                    "bridge_response_loss_after_commit",
                    "disconnect_before_commit",
                )
            }
        elif command_id == "s9_faults_b":
            faults = {
                name: {}
                for name in (
                    "editor_crash_before_commit",
                    "editor_restart_after_commit",
                    "corrupt_journal",
                    "restart_after_bridge_response_before_journal_ack",
                )
            }
        return {
            "schema_version": "s9-model-free-workflow/1.0",
            "status": "passed",
            "platform": "macos-arm64",
            "protocol": "2025-11-25",
            "tool_registry": 41,
            "artifacts": artifacts,
            "operations": operations,
            "negatives": negatives,
            "faults": faults,
            "source_unchanged": True,
            "cleanup": True,
            "redaction": True,
        }
    if command_id == "s10_compound":
        return {
            "schema_version": "s10-model-free-live/1.0",
            "status": "passed",
            "platform": "macos-arm64",
            "protocol": "2025-11-25",
            "bridge_rpc": "1.8",
            "tool_registry": 41,
            "artifacts": artifacts,
            "operation_count": 2,
            "one_native_action": True,
            "prepare_read_only": True,
            "exact_undo": True,
            "validation": {
                "outcome": "passed",
                "checks": {"intrinsic": "passed"},
            },
            "cleanup": {"processes_stopped": True},
        }
    raise AssertionError(command_id)


class FakeRepository:
    def __init__(self, blobs: dict[tuple[str, str], bytes]) -> None:
        self.blobs = blobs

    def blob_at(self, commit: str, relative: str) -> bytes:
        return self.blobs[(commit, relative)]


class Sprint11PackagedRegressionTests(unittest.TestCase):
    def test_command_matrix_is_exact_bounded_and_package_scoped(self) -> None:
        specs = packaged.canonical_command_specs()
        self.assertEqual(len(specs), 9)
        self.assertEqual(len({item.command_id for item in specs}), 9)
        for spec in specs:
            template = spec.argv_template()
            self.assertIn("{package_sidecar}", template)
            self.assertIn("{godot}", template)
            self.assertIn("{timeout}", template)
            self.assertIn("--additive-sprint11-registry", template)
            self.assertNotIn("target/release/godot-codex-mcp", " ".join(template))
        s9 = [item for item in specs if item.command_id.startswith("s9_")]
        self.assertEqual(len(s9), 5)
        self.assertTrue(
            all("--additive-sprint10-registry" in item.arguments for item in s9)
        )

    def test_live_runners_expose_opt_in_sprint11_profile(self) -> None:
        for name in (
            "sprint6_semantic_context_live.py",
            "sprint7_live_editor.py",
            "sprint8_runtime_live.py",
            "sprint10_model_free_live.py",
        ):
            result = subprocess.run(
                [sys.executable, str(packaged.SCRIPT_DIR / name), "--help"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("--additive-sprint11-registry", result.stdout)

    def test_dirty_source_bound_input_is_rejected_before_live_work(self) -> None:
        dirty = subprocess.CompletedProcess(
            args=["git", "status"],
            returncode=0,
            stdout=b" M tests/codex/sprint8_runtime_live.py\n",
            stderr=b"",
        )
        with mock.patch.object(packaged.subprocess, "run", return_value=dirty):
            with self.assertRaises(packaged.PackagedRegressionError):
                packaged.require_source_bound_inputs_clean(
                    packaged.canonical_command_specs()
                )

    def _acquire_fixture(
        self,
    ) -> tuple[dict[str, Any], Path, dict[str, Any], tempfile.TemporaryDirectory[str]]:
        temporary = tempfile.TemporaryDirectory(
            prefix=".s11-package-live-test.",
            dir=packaged.REPOSITORY_ROOT,
        )
        root = Path(temporary.name)
        artifact_root = root / "artifact"
        artifact_root.mkdir()
        sidecar = artifact_root / packaged.PACKAGE_SIDECAR_PATH
        sidecar.parent.mkdir()
        sidecar.write_bytes(b"package-sidecar")
        godot = root / "Godot"
        godot.write_bytes(b"godot-prerequisite")
        manifest = root / "sprint11-package-manifest.json"
        manifest.write_text("{}\n", encoding="utf-8")
        bindings = {
            "package_source_commit": "a" * 40,
            "package_manifest_sha256": packaged.sha256_file(manifest),
            "package_build_provenance_sha256": packaged.sha256_bytes(
                packaged.canonical_json(
                    {
                        "cargo_lock_sha256": packaged.sha256_bytes(b"lock"),
                        "cargo_version": "cargo fixture",
                        "fresh_target": True,
                        "rust_toolchain_sha256": packaged.sha256_bytes(b"toolchain"),
                        "rustc_commit": "c" * 40,
                        "rustc_release": "fixture",
                        "target_triple": "aarch64-apple-darwin",
                    }
                )
            ),
            "package_archive_sha256": "sha256:" + "1" * 64,
            "package_sidecar_path": packaged.PACKAGE_SIDECAR_PATH,
            "package_sidecar_sha256": packaged.sha256_file(sidecar),
            "godot_version": "4.8.dev.codex.fixture",
            "godot_commit": "b" * 40,
            "godot_artifact_sha256": packaged.sha256_file(godot),
            "registry_sha256": "sha256:" + "2" * 64,
        }

        def execute(
            spec: packaged.CommandSpec,
            argv: tuple[str, ...],
            _cwd: Path,
            _timeout: float,
        ) -> packaged.Execution:
            report_path = Path(argv[-1])
            report = synthetic_report(
                spec.command_id,
                godot_sha256=bindings["godot_artifact_sha256"],
                sidecar_sha256=bindings["package_sidecar_sha256"],
            )
            report_path.write_text(
                json.dumps(report, ensure_ascii=False, sort_keys=True) + "\n",
                encoding="utf-8",
            )
            return packaged.Execution(0, b"ok", b"", 1)

        output_root = root / "output"
        with mock.patch.object(
            packaged,
            "_manifest_bindings",
            return_value=(bindings, sidecar),
        ):
            receipt = packaged.acquire(
                artifact_root=artifact_root,
                package_manifest=manifest,
                godot=godot,
                output_root=output_root,
                timeout=17,
                executor=execute,
                check_repository=False,
            )
        return receipt, output_root, bindings, temporary

    def test_acquisition_is_atomic_bounded_and_covers_every_shard(self) -> None:
        receipt, output_root, _bindings, temporary = self._acquire_fixture()
        try:
            self.assertEqual(receipt["capture_kind"], "real_package_live")
            self.assertEqual(receipt["coverage"]["sprints"], [6, 7, 8, 9, 10])
            self.assertEqual(len(receipt["commands"]), 9)
            self.assertTrue((output_root / "receipt.json").is_file())
            self.assertTrue(
                all(
                    not Path(item["report_path"]).is_absolute()
                    for item in receipt["commands"]
                )
            )
        finally:
            temporary.cleanup()

    def test_receipt_validator_cross_binds_package_runners_and_reports(self) -> None:
        receipt, output_root, bindings, temporary = self._acquire_fixture()
        try:
            package_source = bindings["package_source_commit"]
            source_commit = "c" * 40
            blobs: dict[tuple[str, str], bytes] = {}
            for relative in packaged.FIXTURE_PATHS:
                blobs[(package_source, relative)] = (
                    packaged.REPOSITORY_ROOT / relative
                ).read_bytes()
            specs = packaged.canonical_command_specs()
            rebound = copy.deepcopy(receipt)
            for spec, command in zip(specs, rebound["commands"]):
                blobs[(package_source, spec.runner_path)] = (
                    packaged.REPOSITORY_ROOT / spec.runner_path
                ).read_bytes()
                canonical_path = (
                    "tests/codex/acquisition/sprint11/package-live/"
                    + spec.report_name
                )
                command["report_path"] = canonical_path
                blobs[(source_commit, canonical_path)] = (
                    output_root / spec.report_name
                ).read_bytes()
            package = {
                "build_provenance": {
                    "cargo_lock_sha256": packaged.sha256_bytes(b"lock"),
                    "cargo_version": "cargo fixture",
                    "fresh_target": True,
                    "rust_toolchain_sha256": packaged.sha256_bytes(b"toolchain"),
                    "rustc_commit": "c" * 40,
                    "rustc_release": "fixture",
                    "target_triple": "aarch64-apple-darwin",
                },
                "contents": [
                    {
                        "path": packaged.PACKAGE_SIDECAR_PATH,
                        "sha256": bindings["package_sidecar_sha256"],
                    }
                ],
                "godot_prerequisite": {
                    "version": bindings["godot_version"],
                    "commit": bindings["godot_commit"],
                },
            }
            repository = FakeRepository(blobs)
            paths = acceptance.validate_packaged_regression_receipt(
                rebound,
                repository=repository,  # type: ignore[arg-type]
                source_commit=source_commit,
                package_source_commit=package_source,
                package_manifest_sha256=bindings["package_manifest_sha256"],
                package_archive_sha256=bindings["package_archive_sha256"],
                package=package,
                registry_sha256=bindings["registry_sha256"],
                godot_artifact_sha256=bindings["godot_artifact_sha256"],
            )
            self.assertEqual(len(paths), 9)

            changed = copy.deepcopy(rebound)
            changed["bindings"]["package_sidecar_sha256"] = (
                "sha256:" + "f" * 64
            )
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_packaged_regression_receipt(
                    changed,
                    repository=repository,  # type: ignore[arg-type]
                    source_commit=source_commit,
                    package_source_commit=package_source,
                    package_manifest_sha256=bindings["package_manifest_sha256"],
                    package_archive_sha256=bindings["package_archive_sha256"],
                    package=package,
                    registry_sha256=bindings["registry_sha256"],
                    godot_artifact_sha256=bindings["godot_artifact_sha256"],
                )

            changed = copy.deepcopy(rebound)
            changed["commands"][0]["argv_template"].remove(
                "--additive-sprint11-registry"
            )
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_packaged_regression_receipt(
                    changed,
                    repository=repository,  # type: ignore[arg-type]
                    source_commit=source_commit,
                    package_source_commit=package_source,
                    package_manifest_sha256=bindings["package_manifest_sha256"],
                    package_archive_sha256=bindings["package_archive_sha256"],
                    package=package,
                    registry_sha256=bindings["registry_sha256"],
                    godot_artifact_sha256=bindings["godot_artifact_sha256"],
                )
        finally:
            temporary.cleanup()

    def test_status_only_and_incomplete_shards_cannot_qualify(self) -> None:
        with self.assertRaises(packaged.PackagedRegressionError):
            packaged.validate_live_report(
                "s8_runtime",
                {
                    "status": "passed",
                    "artifacts": {
                        "godot_sha256": "sha256:" + "1" * 64,
                        "sidecar_sha256": "sha256:" + "2" * 64,
                    },
                },
                godot_sha256="sha256:" + "1" * 64,
                sidecar_sha256="sha256:" + "2" * 64,
            )
        reports = {
            spec.command_id: synthetic_report(
                spec.command_id,
                godot_sha256="sha256:" + "1" * 64,
                sidecar_sha256="sha256:" + "2" * 64,
            )
            for spec in packaged.canonical_command_specs()
        }
        reports["s9_operations_a"]["operations"].pop()
        with self.assertRaises(packaged.PackagedRegressionError):
            packaged.validate_aggregate_reports(reports)


if __name__ == "__main__":
    unittest.main()
