from __future__ import annotations

import hashlib
import json
import unittest
from pathlib import Path
from typing import Any


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
WAIVER_PATH = (
    REPOSITORY_ROOT
    / "tests"
    / "codex"
    / "evidence"
    / "sprint-11-usability-deferral-waiver.json"
)
FULL_BETA_EVIDENCE_PATH = (
    REPOSITORY_ROOT
    / "tests"
    / "codex"
    / "evidence"
    / "sprint-11-external-codex-beta-macos.json"
)


def _exact_fields(value: Any, expected: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != expected:
        raise AssertionError(f"{label} fields differ")
    return value


class Sprint11UsabilityDeferralTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.record = json.loads(WAIVER_PATH.read_text(encoding="utf-8"))

    def test_waiver_is_explicit_and_cannot_claim_beta(self) -> None:
        record = _exact_fields(
            self.record,
            {
                "decision",
                "future_exit_gate",
                "human_usability",
                "package",
                "risk",
                "schema_version",
                "sprint",
                "status",
                "technical_qualification",
            },
            "waiver",
        )
        self.assertEqual(record["schema_version"], "s11-usability-deferral/1.0")
        self.assertEqual(record["sprint"], 11)
        self.assertEqual(record["status"], "accepted_scope_change")

        technical = _exact_fields(
            record["technical_qualification"],
            {
                "allowed_claim",
                "app_cli_ide_required",
                "commercial_release_qualified",
                "external_codex_beta_qualified",
                "status",
            },
            "technical qualification",
        )
        self.assertEqual(technical["status"], "in_progress")
        self.assertEqual(
            technical["allowed_claim"], "engineering_candidate_private_alpha"
        )
        self.assertTrue(technical["app_cli_ide_required"])
        self.assertFalse(technical["external_codex_beta_qualified"])
        self.assertFalse(technical["commercial_release_qualified"])
        self.assertFalse(FULL_BETA_EVIDENCE_PATH.exists())

    def test_human_evidence_remains_unacquired_without_substitute(self) -> None:
        human = _exact_fields(
            self.record["human_usability"],
            {
                "independent_participant",
                "participant_count",
                "qualifying_report",
                "status",
                "superseded_by",
                "synthetic_substitute_allowed",
            },
            "human usability",
        )
        self.assertEqual(human["status"], "deferred_unacquired")
        self.assertEqual(human["participant_count"], 0)
        self.assertFalse(human["independent_participant"])
        self.assertFalse(human["synthetic_substitute_allowed"])
        self.assertIsNone(human["qualifying_report"])
        self.assertIsNone(human["superseded_by"])

        exit_gate = _exact_fields(
            self.record["future_exit_gate"],
            {
                "consent_required",
                "independent_participants_minimum",
                "participants_minimum",
                "phase",
                "retest_after_high_or_critical_fix",
                "waiver_can_satisfy",
            },
            "future exit gate",
        )
        self.assertEqual(exit_gate["phase"], "commercial_beta")
        self.assertEqual(exit_gate["participants_minimum"], 3)
        self.assertEqual(exit_gate["independent_participants_minimum"], 1)
        self.assertTrue(exit_gate["consent_required"])
        self.assertTrue(exit_gate["retest_after_high_or_critical_fix"])
        self.assertFalse(exit_gate["waiver_can_satisfy"])

    def test_waiver_binds_the_frozen_package_without_private_paths(self) -> None:
        package = _exact_fields(
            self.record["package"],
            {
                "archive_sha256",
                "manifest_path",
                "manifest_sha256",
                "source_commit",
                "version",
            },
            "package",
        )
        self.assertEqual(package["version"], "0.1.9")
        manifest_path = REPOSITORY_ROOT / package["manifest_path"]
        manifest_bytes = manifest_path.read_bytes()
        self.assertEqual(
            package["manifest_sha256"],
            f"sha256:{hashlib.sha256(manifest_bytes).hexdigest()}",
        )
        manifest = json.loads(manifest_bytes)
        self.assertEqual(manifest["source_commit"], package["source_commit"])
        self.assertEqual(manifest["package_version"], package["version"])
        self.assertEqual(
            manifest["archive"]["sha256"], package["archive_sha256"]
        )
        serialized = json.dumps(self.record, sort_keys=True)
        self.assertNotIn("/Users/", serialized)
        self.assertNotIn("token", serialized.lower())


if __name__ == "__main__":
    unittest.main()
