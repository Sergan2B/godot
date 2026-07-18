#!/usr/bin/env python3
"""Regression tests for the fail-closed Sprint 5 S5-02 evidence policy."""

from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import sprint5_stage2_evidence as evidence  # noqa: E402


class Sprint5Stage2EvidenceTests(unittest.TestCase):
    report: dict[str, object]

    @classmethod
    def setUpClass(cls) -> None:
        cls.report = evidence.strict_json_load(evidence.EVIDENCE_PATH)

    def test_committed_evidence_matches_current_or_archived_source(self) -> None:
        self.assertEqual(evidence.validate_evidence(copy.deepcopy(self.report)), self.report)

    def test_source_manifest_is_sorted_self_bound_and_non_circular(self) -> None:
        scopes = evidence._scope_lines(evidence.SOURCE_SCOPE_PATH.read_bytes())
        self.assertEqual(tuple(sorted(scopes)), scopes)
        self.assertIn("tests/codex/sprint5_stage2_source_scopes.txt", scopes)
        self.assertNotIn("tests/codex/evidence/sprint-5-stage-2-oracle.json", scopes)
        self.assertGreater(len(evidence.current_source_files()), len(scopes))

    def test_source_coordinate_tampering_fails_closed(self) -> None:
        tampered = copy.deepcopy(self.report)
        tampered["source"]["source_scope_sha256"] = "sha256:" + "0" * 64  # type: ignore[index]
        with self.assertRaisesRegex(evidence.EvidenceError, "source differ"):
            evidence.validate_evidence(tampered)

    def test_parser_metric_tampering_fails_closed(self) -> None:
        tampered = copy.deepcopy(self.report)
        tampered["godot_parser"]["metrics"]["csharp_discovery_documents"] = 0  # type: ignore[index]
        with self.assertRaisesRegex(evidence.EvidenceError, "C# discovery"):
            evidence.validate_evidence(tampered, check_source=False)

    def test_duplicate_evidence_members_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-s5-oracle-evidence-") as directory:
            path = Path(directory) / "duplicate.json"
            path.write_text('{"schema_version":1,"schema_version":1}', encoding="utf-8")
            with self.assertRaisesRegex(evidence.EvidenceError, "duplicate JSON member"):
                evidence.strict_json_load(path)

    def test_evidence_contains_no_absolute_or_cache_paths(self) -> None:
        serialized = json.dumps(self.report, ensure_ascii=False)
        for forbidden in evidence.FORBIDDEN_MATERIAL:
            self.assertNotIn(forbidden, serialized)


if __name__ == "__main__":
    unittest.main()
