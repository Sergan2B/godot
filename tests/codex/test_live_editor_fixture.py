"""Regression tests for the independent Sprint 7 live-editor oracle."""

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

import live_editor_fixture as fixture  # noqa: E402


class LiveEditorFixtureTests(unittest.TestCase):
    def test_oracle_is_complete_and_self_consistent(self) -> None:
        result = fixture.validate_all(run_schemas=False)
        self.assertEqual(9, result["files"])
        self.assertEqual(11, result["phases"])
        self.assertEqual(17, result["coverage"])
        self.assertEqual(4, result["revision_transitions"])
        self.assertEqual("passed", result["status"])

    def test_manifest_hash_drift_fails_closed(self) -> None:
        manifest = fixture.strict_json_load(fixture.MANIFEST_PATH)
        drifted = copy.deepcopy(manifest)
        drifted["files"][0]["sha256"] = "0" * 64
        with self.assertRaisesRegex(fixture.FixtureError, "hash differs"):
            fixture.validate_manifest(drifted)

    def test_revision_and_partial_tombstone_policies_fail_closed(self) -> None:
        golden = fixture.strict_json_load(fixture.GOLDEN_PATH)
        stale_revision = copy.deepcopy(golden)
        stale_revision["revision_transitions"][2]["operation_seq"] = 42
        with self.assertRaisesRegex(fixture.FixtureError, "strictly monotonic"):
            fixture.validate_golden(stale_revision)
        tombstone = copy.deepcopy(golden)
        tombstone["composition_vectors"][1]["expected_effective"] = {}
        with self.assertRaisesRegex(fixture.FixtureError, "composition differs"):
            fixture.validate_golden(tombstone)

    def test_duplicate_json_members_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-s7-oracle-") as directory:
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
        self.assertNotIn("live_overlay.rs", source)

    def test_oracle_contains_no_local_transport_or_secret_material(self) -> None:
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
