#!/usr/bin/env python3
"""Regression tests for the fail-closed Sprint 5 S5-01 evidence policy."""

from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import sprint5_stage1_evidence as evidence  # noqa: E402


class Sprint5Stage1EvidenceTests(unittest.TestCase):
    report: dict[str, object]

    @classmethod
    def setUpClass(cls) -> None:
        cls.report = evidence.strict_json_load(evidence.EVIDENCE_PATH)

    def test_committed_evidence_matches_current_stage_source(self) -> None:
        self.assertEqual(evidence.validate_evidence(copy.deepcopy(self.report)), self.report)

    def test_source_manifest_is_exact_sorted_and_self_bound(self) -> None:
        scopes = evidence.source_scopes()
        self.assertEqual(tuple(sorted(scopes)), scopes)
        self.assertEqual(len(scopes), len(set(scopes)))
        self.assertIn("tests/codex/sprint5_source_scopes.txt", scopes)
        self.assertNotIn("tests/codex/evidence/sprint-5-stage-1-contracts.json", scopes)

    def test_runtime_spike_tampering_fails_closed(self) -> None:
        tampered = copy.deepcopy(self.report)
        tampered["decision"]["runtime_spike"]["selected_path_requires_lsp_client"] = True  # type: ignore[index]
        with self.assertRaisesRegex(evidence.EvidenceError, "requires LSP"):
            evidence.validate_evidence(tampered, check_checkout=False)

    def test_source_coordinate_tampering_fails_closed(self) -> None:
        tampered = copy.deepcopy(self.report)
        tampered["source"]["source_scope_sha256"] = "sha256:" + "0" * 64  # type: ignore[index]
        with self.assertRaisesRegex(evidence.EvidenceError, "current S5-01 source differs"):
            evidence.validate_evidence(tampered)

    def test_duplicate_evidence_members_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-s5-evidence-") as directory:
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
