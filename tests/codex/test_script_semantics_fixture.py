"""Regression tests for the independent Sprint 5 script-semantics oracle."""

from __future__ import annotations

import ast
import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import script_semantics_fixture as fixture  # noqa: E402


class ScriptSemanticsFixtureTests(unittest.TestCase):
    manifest: dict[str, Any]
    golden: dict[str, Any]

    @classmethod
    def setUpClass(cls) -> None:
        manifest = fixture.load_manifest()
        golden = fixture.load_golden()
        cls.manifest = dict(manifest)
        cls.golden = dict(golden)

    def test_independent_oracle_is_complete_and_self_consistent(self) -> None:
        summary = fixture.validate_all(run_schemas=False)
        self.assertEqual(23, summary["files"])
        self.assertEqual(10, summary["phases"])
        self.assertEqual(8, summary["documents"])
        self.assertEqual(48, summary["symbols"])
        self.assertEqual(14, summary["relations"])
        self.assertEqual(65, summary["ranges"])
        self.assertEqual("passed", summary["status"])

    def test_draft_2020_12_schemas_and_named_negatives(self) -> None:
        fixture.run_draft_schema_tests()

    def test_exact_and_dynamic_truth_are_disjoint(self) -> None:
        relations = self.golden["relations"]
        exact = [value for value in relations if value["confidence"] == "exact"]
        dynamic = [value for value in relations if value["confidence"] == "dynamic"]
        self.assertEqual(9, len(exact))
        self.assertEqual(5, len(dynamic))
        self.assertTrue(all(value["resolvable_truth"] for value in exact))
        self.assertTrue(all(value["target_symbol"] is None and value["target_resource"] is None for value in dynamic))

    def test_broken_missing_cycle_and_csharp_states_are_explicit(self) -> None:
        documents = {value["oracle_id"]: value for value in self.golden["documents"]}
        self.assertEqual("invalid", documents["broken"]["semantic_state"])
        self.assertEqual("partial", documents["missing_base"]["semantic_state"])
        self.assertEqual("partial", documents["cycle_a"]["semantic_state"])
        self.assertEqual("partial", documents["cycle_b"]["semantic_state"])
        self.assertEqual("unavailable", documents["enemy_cs"]["semantic_state"])
        self.assertIsNone(documents["enemy_cs"]["class_symbol"])

    def test_range_or_hash_drift_fails_closed(self) -> None:
        documents = fixture.validate_documents(self.golden, fixture.validate_manifest(self.manifest))
        drifted = copy.deepcopy(self.golden)
        drifted["ranges"][0]["bytes"][0] += 1
        with self.assertRaisesRegex(fixture.FixtureError, "content-bound range differs"):
            fixture.validate_ranges(drifted, documents)

        bad_manifest = copy.deepcopy(self.manifest)
        bad_manifest["files"][0]["sha256"] = "0" * 64
        with self.assertRaisesRegex(fixture.FixtureError, "hash differs"):
            fixture.validate_manifest(bad_manifest)

    def test_duplicate_json_members_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-s5-oracle-") as directory:
            path = Path(directory) / "duplicate.json"
            path.write_text('{"schema_version":1,"schema_version":1}', encoding="utf-8")
            with self.assertRaisesRegex(fixture.FixtureError, "duplicate JSON member"):
                fixture.strict_json_load(path)

    def test_validator_has_no_production_or_bridge_dependency(self) -> None:
        source = Path(fixture.__file__).read_text(encoding="utf-8")
        forbidden = ("godot_codex_mcp", "ScriptSemanticAdapter", "IndexStore", "segment-v3")
        self.assertFalse(any(value in source for value in forbidden))
        imported = {
            alias.name.split(".")[0]
            for node in ast.walk(ast.parse(source))
            if isinstance(node, ast.Import)
            for alias in node.names
        }
        self.assertFalse({"godot_codex_mcp", "codex_bridge"} & imported)

    def test_oracle_contains_no_absolute_or_cache_paths(self) -> None:
        serialized = json.dumps(self.golden, ensure_ascii=False)
        for forbidden in fixture.FORBIDDEN_MATERIAL:
            self.assertNotIn(forbidden, serialized)


if __name__ == "__main__":
    unittest.main()
