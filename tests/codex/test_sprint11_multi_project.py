from __future__ import annotations

import copy
import hashlib
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

SCRIPT_ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_ROOT))

import sprint11_multi_project as multi


def digest(label: str) -> str:
    return "sha256:" + hashlib.sha256(label.encode("utf-8")).hexdigest()


def valid_report() -> dict[str, Any]:
    project_fields = {
        "operation_seq": 0,
        "action_count": 0,
        "approval_provider": "mcp_action_only_form_v1",
        "validation_outcome": "passed",
        "readback": True,
        "targeted_undo": True,
    }
    foreign = [
        {
            "kind": kind,
            "error_code": code,
            "is_error": True,
            "mutation": False,
        }
        for kind, code in sorted(multi.FOREIGN_EXPECTATIONS.items())
    ]
    return {
        "schema_version": multi.REPORT_SCHEMA,
        "status": "passed",
        "platform": "macos-arm64",
        "protocol": "2025-11-25",
        "bridge_rpc": "1.8",
        "artifacts": {
            "godot_sha256": digest("g"),
            "sidecar_sha256": digest("s"),
        },
        "registry": {
            "tools": 41,
            "digest": digest("registry"),
        },
        "projects": {
            "a": {
                **project_fields,
                "project_identity_sha256": digest("a-project"),
                "editor_identity_sha256": digest("a-editor"),
                "runtime_identity_sha256": digest("a-runtime"),
                "transaction_identity_sha256": digest("a-transaction"),
                "validation_report_identity_sha256": digest("a-report"),
            },
            "b": {
                **project_fields,
                "project_identity_sha256": digest("b-project"),
                "editor_identity_sha256": digest("b-editor"),
                "runtime_identity_sha256": digest("b-runtime"),
                "transaction_identity_sha256": digest("b-transaction"),
                "validation_report_identity_sha256": digest("b-report"),
            },
        },
        "assertions": {
            "parallel_reads": True,
            "parallel_runtime_start": True,
            "parallel_prepare": True,
            "parallel_apply": True,
            "separate_approvals": True,
            "validation_readback": True,
            "targeted_undo": True,
            "identity_distinct": {
                "project": True,
                "editor": True,
                "runtime": True,
                "transaction": True,
                "validation_report": True,
            },
            "foreign_bindings": foreign,
            "target_restart": {
                "target": "a",
                "component": "sidecar",
                "process_instance_changed": True,
                "target_reconnected": True,
                "target_identity_preserved": True,
                "sibling_process_preserved": True,
                "sibling_editor_preserved": True,
                "sibling_runtime_preserved": True,
                "sibling_transaction_preserved": True,
                "sibling_validation_report_preserved": True,
                "sibling_readback_preserved": True,
                "sibling_revisions_preserved": True,
            },
            "fault_matrix": [
                multi._isolation_case(
                    case_id,
                    sorted(multi.ISOLATION_CASE_EXPECTATIONS[case_id])[0],
                    no_fallback=True,
                    no_cross_project_data=True,
                    no_cross_project_mutation=True,
                    no_cross_project_approval=True,
                    no_cross_project_transaction=True,
                    cleanup=True,
                )
                for case_id in multi.ISOLATION_CASE_ORDER
            ],
        },
        "source_integrity": {"a": True, "b": True},
        "cleanup": {
            "runtime_a_stopped": True,
            "runtime_b_stopped": True,
            "editor_a_stopped": True,
            "editor_b_stopped": True,
            "sidecar_a_stopped": True,
            "sidecar_b_stopped": True,
        },
    }


class Sprint11MultiProjectContractTests(unittest.TestCase):
    def test_connection_status_waits_for_bounded_cache_convergence(self) -> None:
        project_id = "project:sha256:" + "a" * 64
        project_scope = "a" * 64
        rebuilding = {
            "status": "syncing",
            "project_scope": project_scope,
            "package_version": "0.1.0",
            "bridge": {
                "condition": "ready",
                "negotiated_protocol": "1.8",
            },
            "static_cache": {
                "condition": "rebuilding",
                "schema": "1.3",
                "generation": None,
                "revisions": None,
            },
            "components": {
                "editor": "ready",
                "transactions": "ready",
            },
        }
        ready = copy.deepcopy(rebuilding)
        ready["status"] = "ready"
        ready["static_cache"] = {
            "condition": "online_current",
            "schema": "1.3",
            "generation": "generation:fixture",
            "revisions": {"resource_revision": 1},
        }

        class StatusClient:
            def __init__(self, responses: list[dict[str, Any]]) -> None:
                self.responses = responses
                self.calls = 0

            def tool(
                self,
                _name: str,
                _arguments: dict[str, Any],
            ) -> tuple[dict[str, Any], bool, float]:
                response = self.responses[min(self.calls, len(self.responses) - 1)]
                self.calls += 1
                return copy.deepcopy(response), False, 0.0

        client = StatusClient([rebuilding, ready])
        self.assertEqual(
            multi._connection_status(client, project_id, 1.0),
            ready,
        )
        self.assertEqual(client.calls, 2)

        never_ready = StatusClient([rebuilding])
        with self.assertRaisesRegex(
            multi.s9.WorkflowError,
            "project-scoped connection/cache/version status differs",
        ):
            multi._connection_status(never_ready, project_id, 0.01)

    def test_transaction_status_waits_for_exact_terminal_projection(self) -> None:
        transaction_id = "change-set:" + "b" * 32
        committed = {
            "change_set_id": transaction_id,
            "state": "committed",
        }
        undone = {
            "change_set_id": transaction_id,
            "state": "undone",
        }

        class StatusClient:
            def __init__(self, responses: list[dict[str, Any]]) -> None:
                self.responses = responses
                self.calls = 0

            def tool(
                self,
                _name: str,
                _arguments: dict[str, Any],
            ) -> tuple[dict[str, Any], bool, float]:
                response = self.responses[min(self.calls, len(self.responses) - 1)]
                self.calls += 1
                return copy.deepcopy(response), False, 0.0

        client = StatusClient([committed, undone])
        self.assertEqual(
            multi._transaction_status(
                client,
                transaction_id,
                expected_state="undone",
                timeout=1.0,
                context="unit",
            ),
            undone,
        )
        self.assertEqual(client.calls, 2)

        never_undone = StatusClient([committed])
        with self.assertRaisesRegex(
            multi.s9.WorkflowError,
            "transaction status differs",
        ):
            multi._transaction_status(
                never_undone,
                transaction_id,
                expected_state="undone",
                timeout=0.01,
                context="unit",
            )

    def test_foreign_error_waits_only_through_retryable_transport_state(
        self,
    ) -> None:
        class StatusClient:
            def __init__(self, responses: list[dict[str, Any]]) -> None:
                self.responses = responses
                self.calls = 0

            def tool(
                self,
                _name: str,
                _arguments: dict[str, Any],
            ) -> tuple[dict[str, Any], bool, float]:
                response = self.responses[min(self.calls, len(self.responses) - 1)]
                self.calls += 1
                return copy.deepcopy(response), True, 0.0

        retryable = {
            "error": {
                "code": "runtime_unavailable",
                "retryable": True,
            }
        }
        expected = {
            "error": {
                "code": "transaction_not_found",
                "retryable": False,
            }
        }
        client = StatusClient([retryable, expected])
        self.assertEqual(
            multi._foreign_error(
                client,
                "godot_get_transaction_status",
                {"transaction_id": "change-set:" + "f" * 32},
                kind="transaction_id",
                timeout=1.0,
            )["error_code"],
            "transaction_not_found",
        )
        self.assertEqual(client.calls, 2)

    def test_source_and_artifact_hashing_reject_symlinks(self) -> None:
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            root = Path(directory)
            source = root / "project"
            source.mkdir()
            (source / "project.godot").write_bytes(b"[application]\n")
            hashes = multi._project_source_hashes(source)
            self.assertEqual(
                hashes["project.godot"],
                hashlib.sha256(b"[application]\n").hexdigest(),
            )
            (source / "linked.gd").symlink_to(source / "project.godot")
            with self.assertRaisesRegex(
                multi.s9.WorkflowError,
                "symlink",
            ):
                multi._project_source_hashes(source)
            artifact = root / "artifact"
            artifact.write_bytes(b"binary")
            linked_artifact = root / "linked-artifact"
            linked_artifact.symlink_to(artifact)
            with self.assertRaisesRegex(
                multi.s9.WorkflowError,
                "safely",
            ):
                multi._artifact_digest(linked_artifact)

    def test_private_binding_replacement_is_exact_and_restorable(self) -> None:
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            path = Path(directory) / "session.token"
            path.write_bytes(b"a" * 32)
            path.chmod(0o600)
            snapshot = multi._PrivateFile.capture(path, maximum=32)
            multi._replace_private_file(path, b"b" * 32)
            self.assertEqual(path.read_bytes(), b"b" * 32)
            self.assertEqual(os.stat(path).st_mode & 0o777, 0o600)
            snapshot.restore()
            self.assertEqual(path.read_bytes(), b"a" * 32)
            self.assertEqual(os.stat(path).st_mode & 0o777, 0o600)

    def test_valid_report_passes(self) -> None:
        report = valid_report()
        self.assertIs(multi.validate_report(report), report)

    def test_every_identity_must_be_distinct(self) -> None:
        for kind in (
            "project",
            "editor",
            "runtime",
            "transaction",
            "validation_report",
        ):
            with self.subTest(kind=kind):
                report = valid_report()
                field = f"{kind}_identity_sha256"
                report["projects"]["b"][field] = report["projects"]["a"][field]
                with self.assertRaisesRegex(
                    multi.s9.WorkflowError,
                    f"{kind} identity is not distinct",
                ):
                    multi.validate_report(report)

    def test_foreign_binding_matrix_is_exact_and_fail_closed(self) -> None:
        mutations = (
            lambda report: report["assertions"]["foreign_bindings"].pop(),
            lambda report: report["assertions"]["foreign_bindings"][0].__setitem__(
                "mutation", True
            ),
            lambda report: report["assertions"]["foreign_bindings"][0].__setitem__(
                "error_code", "not_found"
            ),
            lambda report: report["assertions"]["foreign_bindings"][1].__setitem__(
                "kind",
                report["assertions"]["foreign_bindings"][0]["kind"],
            ),
        )
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                report = valid_report()
                mutate(report)
                with self.assertRaises(multi.s9.WorkflowError):
                    multi.validate_report(report)

    def test_restart_and_cleanup_proofs_cannot_be_omitted(self) -> None:
        report = valid_report()
        report["assertions"]["target_restart"]["sibling_runtime_preserved"] = False
        with self.assertRaisesRegex(
            multi.s9.WorkflowError,
            "target restart isolation differs",
        ):
            multi.validate_report(report)

    def test_fault_matrix_is_exact_complete_and_every_case_is_fail_closed(
        self,
    ) -> None:
        for mutate in (
            lambda matrix: matrix.pop(),
            lambda matrix: matrix.reverse(),
            lambda matrix: matrix[0].__setitem__("no_fallback", False),
            lambda matrix: matrix[0].__setitem__(
                "observed_code",
                "unexpected",
            ),
            lambda matrix: matrix[0].__setitem__(
                "proof_source",
                "public_runner_prelaunch_authority_plus_external_surface_attestation",
            ),
            lambda matrix: matrix[0].__setitem__("unexpected", True),
        ):
            with self.subTest(mutate=mutate):
                report = valid_report()
                mutate(report["assertions"]["fault_matrix"])
                with self.assertRaises(multi.s9.WorkflowError):
                    multi.validate_report(report)

    def test_launch_authority_rejects_each_swapped_coordinate(self) -> None:
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            root = Path(directory)
            a = root / "a"
            b = root / "b"
            for project in (a, b):
                project.mkdir()
                (project / "project.godot").write_text(
                    "[application]\n",
                    encoding="utf-8",
                )
            self.assertEqual(
                multi._require_launch_authority(
                    expected_root=a,
                    config_root=a,
                    argument_root=a,
                    cwd=a,
                ),
                a.resolve(),
            )
            for field in ("config_root", "argument_root", "cwd"):
                arguments = {
                    "expected_root": a,
                    "config_root": a,
                    "argument_root": a,
                    "cwd": a,
                }
                arguments[field] = b
                with self.subTest(field=field), self.assertRaisesRegex(
                    multi.s9.WorkflowError,
                    "project_binding_mismatch",
                ):
                    multi._require_launch_authority(**arguments)

    def test_package_authority_rejects_hash_and_version_before_launch(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            sidecar = Path(directory) / "sidecar"
            sidecar.write_bytes(b"package")
            observed = multi._artifact_digest(sidecar)
            multi._require_package_authority(
                sidecar,
                expected_sha256=observed,
                expected_version="1.0.0",
                observed_sha256=observed,
                observed_version="1.0.0",
            )
            with self.assertRaisesRegex(
                multi.s9.WorkflowError,
                "package_binding_mismatch",
            ):
                multi._require_package_authority(
                    sidecar,
                    expected_sha256=digest("foreign-package"),
                    expected_version="1.0.0",
                    observed_sha256=observed,
                    observed_version="1.0.0",
                )
            with self.assertRaisesRegex(
                multi.s9.WorkflowError,
                "package_version_mismatch",
            ):
                multi._require_package_authority(
                    sidecar,
                    expected_sha256=observed,
                    expected_version="2.0.0",
                    observed_sha256=observed,
                    observed_version="1.0.0",
                )

        report = valid_report()
        report["cleanup"]["sidecar_b_stopped"] = False
        with self.assertRaisesRegex(
            multi.s9.WorkflowError,
            "source integrity or cleanup differs",
        ):
            multi.validate_report(report)

    def test_report_is_bounded_and_closed(self) -> None:
        report = valid_report()
        report["unexpected"] = "x"
        with self.assertRaisesRegex(
            multi.s9.WorkflowError,
            "fields differ",
        ):
            multi.validate_report(report)

        report = valid_report()
        report["padding"] = "x" * multi.REPORT_LIMIT
        with self.assertRaisesRegex(
            multi.s9.WorkflowError,
            "unbounded",
        ):
            multi.validate_report(report)

    def test_identity_digest_is_domain_separated(self) -> None:
        value = "runtime:" + "1" * 32
        self.assertNotEqual(
            multi._identity_digest("runtime", value),
            multi._identity_digest("transaction", value),
        )
        self.assertRegex(multi._identity_digest("runtime", value), multi.DIGEST_RE)

    def test_registry_profile_is_exact(self) -> None:
        profile = multi._registry_profile()
        self.assertEqual(41, profile["tool_count"])
        self.assertIn("godot_get_connection_status", profile["tools"])

    def test_tampering_does_not_modify_valid_fixture(self) -> None:
        report = valid_report()
        tampered = copy.deepcopy(report)
        tampered["projects"]["a"]["operation_seq"] = -1
        with self.assertRaises(multi.s9.WorkflowError):
            multi.validate_report(tampered)
        self.assertEqual(0, report["projects"]["a"]["operation_seq"])


if __name__ == "__main__":
    unittest.main()
