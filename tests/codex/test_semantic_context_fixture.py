"""Regression tests for the independent Sprint 6 semantic-context oracle."""

from __future__ import annotations

import ast
import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import semantic_context_fixture as fixture  # noqa: E402


class SemanticContextFixtureTests(unittest.TestCase):
    def test_oracle_is_complete_and_self_consistent(self) -> None:
        result = fixture.validate_all(run_schemas=False)
        self.assertEqual(11, result["files"])
        self.assertEqual(8, result["phases"])
        self.assertEqual(4, result["queries"])
        self.assertEqual(7, result["expected_usages"])
        self.assertEqual("passed", result["status"])

    def test_manifest_hash_drift_fails_closed(self) -> None:
        manifest = fixture.strict_json_load(fixture.MANIFEST_PATH)
        drifted = copy.deepcopy(manifest)
        drifted["files"][0]["sha256"] = "0" * 64
        with self.assertRaisesRegex(fixture.FixtureError, "hash differs"):
            fixture.validate_manifest(drifted)

    def test_false_exact_and_conflict_policies_fail_closed(self) -> None:
        golden = fixture.strict_json_load(fixture.GOLDEN_PATH)
        false_exact = copy.deepcopy(golden)
        false_exact["false_exact_allowed"] = 1
        with self.assertRaisesRegex(fixture.FixtureError, "false exact"):
            fixture.validate_golden(false_exact)
        no_conflict = copy.deepcopy(golden)
        no_conflict["conflict_vectors"][0]["facts"][1]["value"] = "Node2D"
        with self.assertRaisesRegex(fixture.FixtureError, "conflict values"):
            fixture.validate_golden(no_conflict)

    def test_duplicate_json_members_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-s6-oracle-") as directory:
            path = Path(directory) / "duplicate.json"
            path.write_text('{"schema_version":1,"schema_version":1}', encoding="utf-8")
            with self.assertRaisesRegex(fixture.FixtureError, "duplicate JSON member"):
                fixture.strict_json_load(path)

    def test_validator_has_no_production_dependency(self) -> None:
        source = Path(fixture.__file__).read_text(encoding="utf-8")
        imported = {
            alias.name.split(".")[0]
            for node in ast.walk(ast.parse(source))
            if isinstance(node, ast.Import)
            for alias in node.names
        }
        self.assertFalse({"godot_codex_mcp", "codex_bridge"} & imported)
        self.assertNotIn("segment-v3", source)

    def test_oracle_contains_no_private_paths(self) -> None:
        serialized = json.dumps(
            {
                "manifest": fixture.strict_json_load(fixture.MANIFEST_PATH),
                "golden": fixture.strict_json_load(fixture.GOLDEN_PATH),
            },
            ensure_ascii=False,
        )
        for forbidden in fixture.FORBIDDEN_MATERIAL:
            self.assertNotIn(forbidden, serialized)


if __name__ == "__main__":
    unittest.main()
