from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tests.codex import sprint11_host_delta as host_delta
from tests.codex import sprint11_technical_acceptance as technical
from tests.codex.test_sprint11_host_delta import (
    digest,
    matrix,
    passing_smoke,
    profile,
)


SOURCE_COMMIT = "7" * 40


def write_json(path: Path, value: object) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(host_delta.canonical_json(value))
    return host_delta.sha256_file(path)


class TechnicalAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.host = self.root / "host-delta"
        self.host.mkdir()

        self.manifest = {
            "schema_version": "s11-package-manifest/1.0",
            "package_version": "0.1.19",
            "source_commit": SOURCE_COMMIT,
        }
        self.manifest_path = self.root / "package/sprint11-package-manifest.json"
        self.manifest_digest = write_json(self.manifest_path, self.manifest)

        self.same_project = {
            "schema_version": "s11-same-project-live/1.1",
            "status": "passed",
            "package_version": "0.1.19",
            "assertions": {
                "busy_not_syncing": True,
                "busy_projection_fail_closed": True,
                "crash_takeover": True,
                "diagnostic_only_standby": True,
                "exactly_one_owner": True,
                "graceful_takeover": True,
                "takeover_within_90_seconds": True,
                "transaction_coordinator_follows_index_lease": True,
            },
        }
        self.same_path = self.root / "package-live/sprint11-same-project.json"
        same_digest = write_json(self.same_path, self.same_project)
        self.package_live = {
            "schema_version": "s11-packaged-regression-receipt/1.0",
            "status": "passed",
            "bindings": {
                "package_manifest_sha256": self.manifest_digest,
                "package_source_commit": SOURCE_COMMIT,
            },
            "coverage": {"sprints": [6, 7, 8, 9, 10], "sprint10_validation": True},
            "commands": [{
                "id": "s11_same_project",
                "exit_code": 0,
                "report_path": self.same_path.relative_to(self.root).as_posix(),
                "report_sha256": same_digest,
            }],
        }
        self.package_live_path = self.root / "package-live/receipt.json"
        write_json(self.package_live_path, self.package_live)

        self.multi = {
            "schema_version": "s11-multi-project-receipt/1.0",
            "status": "passed",
            "bindings": {
                "package_manifest_sha256": self.manifest_digest,
                "package_source_commit": SOURCE_COMMIT,
                "package_version": "0.1.19",
            },
            "assertions": {
                "isolation_matrix_complete": True,
                "no_cross_project_leakage": True,
                "foreign_ids_rejected": True,
                "target_fault_isolated": True,
                "source_unchanged": True,
            },
        }
        self.multi_path = self.root / "multi/receipt.json"
        write_json(self.multi_path, self.multi)

        self.repro = {
            "schema_version": "s11-reproducibility-receipt/1.0",
            "status": "passed",
            "bindings": {
                "qualified_manifest_sha256": self.manifest_digest,
                "source_commit": SOURCE_COMMIT,
            },
            "reproducibility": {
                "archive_bytes_equal": True,
                "manifest_bytes_equal": True,
                "package_tree_equal": True,
                "source_checkout_unchanged": True,
            },
        }
        self.repro_path = self.root / "repro/receipt.json"
        write_json(self.repro_path, self.repro)

        self.matrix = matrix()
        self.matrix_digest = host_delta.sha256_bytes(host_delta.ordered_compact_json(self.matrix))
        self.supported_profile = profile()
        self.supported_profile["compatibility_matrix"]["sha256"] = self.matrix_digest
        for surface in self.supported_profile["surfaces"]:
            surface["qualification"] = "supported"
        self.profile_path = self.host / "host-coordinate-profile.json"
        profile_digest = write_json(self.profile_path, self.supported_profile)

        assessment = host_delta.DeltaAssessment(
            delta_class=host_delta.DeltaClass.COORDINATE_ONLY,
            affected_surfaces=("app", "cli", "ide"),
            required_probes=(),
            reasons=("fixture",),
        )
        self.bundle = host_delta.build_surface_bundle(
            assessment=assessment,
            matrix=self.matrix,
            profile=self.supported_profile,
            profile_sha256=profile_digest,
            sequence=4,
        )
        self.bundle_path = self.host / "surface-compatibility-bundle.json"
        bundle_digest = write_json(self.bundle_path, self.bundle)

        app_smoke = passing_smoke(digest("a"))
        ide_smoke = passing_smoke(digest("b"))
        ide_smoke["surfaces"] = ["ide"]
        self.smoke_paths = [self.host / "smoke-1.json", self.host / "smoke-2.json"]
        smoke_digests = [
            write_json(self.smoke_paths[0], app_smoke),
            write_json(self.smoke_paths[1], ide_smoke),
        ]

        self.measurement = {
            "schema_version": "s11-host-delta-measurement/1.0",
            "package": {"version": "0.1.19", "baseline_matrix_sha256": self.matrix_digest},
            "surfaces": copy.deepcopy(self.supported_profile["surfaces"]),
            "client_groups": [
                {"client_artifact_sha256": digest("a"), "surfaces": ["app", "cli"]},
                {"client_artifact_sha256": digest("b"), "surfaces": ["ide"]},
            ],
            "contract_projection_sha256": digest("c"),
            "assertions": {
                "package_unchanged": True,
                "all_required_surfaces_measured": True,
                "signatures_verified": True,
                "manual_interaction_absent": True,
            },
        }
        self.measurement_path = self.host / "measurement.json"
        measurement_digest = write_json(self.measurement_path, self.measurement)
        self.receipt = host_delta.build_host_delta_receipt(
            assessment=assessment,
            outcome="compatible_bundle_issued",
            bindings={
                "package_manifest_sha256": self.manifest_digest,
                "baseline_matrix_sha256": self.matrix_digest,
                "previous_profile_sha256": digest("d"),
                "measurement_sha256": measurement_digest,
                "host_profile_sha256": profile_digest,
                "bundle_sha256": bundle_digest,
                "contract_projection_sha256": digest("c"),
                "smoke_report_sha256": smoke_digests,
            },
            bundle={
                "bundle_id": self.bundle["bundle_id"],
                "sequence": 4,
                "qualification": "supported",
            },
        )
        self.receipt_path = self.host / "host-delta-receipt.json"
        write_json(self.receipt_path, self.receipt)
        self.active = {
            "schema_version": "godot-codex-surface-compatibility-status/1.0",
            "status": "ready",
            "package_version": "0.1.19",
            "source": "installed_bundle",
            "effective_matrix_sha256": digest("e"),
            "bundle_id": self.bundle["bundle_id"],
            "sequence": 4,
            "bundle_sha256": bundle_digest,
            "host_coordinate_profile_sha256": profile_digest,
        }
        self.active_path = self.host / "compatibility-active-status.json"
        write_json(self.active_path, self.active)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def inputs(self) -> technical.TechnicalInputs:
        return technical.TechnicalInputs(
            package_manifest=self.manifest_path,
            package_live_receipt=self.package_live_path,
            multi_project_receipt=self.multi_path,
            reproducibility_receipt=self.repro_path,
            host_delta_directory=self.host,
            active_compatibility_status=self.active_path,
        )

    def compose(self) -> dict[str, object]:
        with mock.patch.object(technical, "REPOSITORY_ROOT", self.root):
            return technical.compose(self.inputs())

    def test_host_delta_reuses_exact_package_bound_receipts(self) -> None:
        report = self.compose()
        self.assertEqual(report["status"], "passed")
        self.assertEqual(report["track"], "technical_private_alpha")
        self.assertEqual(report["usability"], "deferred_unacquired")
        self.assertTrue(report["assertions"]["package_evidence_reused"])
        self.assertFalse(report["claims"]["external_codex_beta"])
        self.assertFalse(report["claims"]["commercial_ready"])
        self.assertNotIn("assertions", report["package_evidence"]["same_project"])

    def test_changed_package_digest_rejects_reused_receipts(self) -> None:
        self.package_live["bindings"]["package_manifest_sha256"] = digest("f")
        write_json(self.package_live_path, self.package_live)
        with self.assertRaisesRegex(technical.TechnicalAcceptanceError, "package binding"):
            self.compose()

    def test_candidate_surface_is_rejected(self) -> None:
        self.supported_profile["surfaces"][0]["qualification"] = "candidate"
        write_json(self.profile_path, self.supported_profile)
        with self.assertRaisesRegex(technical.TechnicalAcceptanceError, "supported"):
            self.compose()

    def test_absent_host_surface_is_rejected(self) -> None:
        self.measurement["surfaces"].pop()
        write_json(self.measurement_path, self.measurement)
        with self.assertRaisesRegex(technical.TechnicalAcceptanceError, "surfaces"):
            self.compose()

    def test_active_sequence_mismatch_is_rejected(self) -> None:
        self.active["sequence"] = 5
        write_json(self.active_path, self.active)
        with self.assertRaisesRegex(technical.TechnicalAcceptanceError, "active compatibility"):
            self.compose()

    def test_manual_surface_interaction_is_rejected(self) -> None:
        self.measurement["assertions"]["manual_interaction_absent"] = False
        write_json(self.measurement_path, self.measurement)
        with self.assertRaisesRegex(technical.TechnicalAcceptanceError, "manual"):
            self.compose()

    def test_failed_or_stale_smoke_is_rejected(self) -> None:
        smoke = json.loads(self.smoke_paths[0].read_text(encoding="utf-8"))
        smoke["status"] = "failed"
        write_json(self.smoke_paths[0], smoke)
        with self.assertRaisesRegex(technical.TechnicalAcceptanceError, "smoke"):
            self.compose()

    def test_missing_same_project_takeover_is_rejected(self) -> None:
        self.same_project["assertions"]["crash_takeover"] = False
        same_digest = write_json(self.same_path, self.same_project)
        self.package_live["commands"][0]["report_sha256"] = same_digest
        write_json(self.package_live_path, self.package_live)
        with self.assertRaisesRegex(technical.TechnicalAcceptanceError, "takeover"):
            self.compose()

    def test_claim_escalation_is_rejected(self) -> None:
        report = self.compose()
        report["claims"]["commercial_ready"] = True
        with self.assertRaisesRegex(technical.TechnicalAcceptanceError, "claims"):
            technical.validate_document(report)

    def test_validate_replays_all_file_and_digest_bindings(self) -> None:
        report = self.compose()
        with mock.patch.object(technical, "REPOSITORY_ROOT", self.root):
            self.assertEqual(technical.validate(report, artifact_root=self.root), report)


if __name__ == "__main__":
    unittest.main()
