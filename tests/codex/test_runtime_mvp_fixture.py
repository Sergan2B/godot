#!/usr/bin/env python3
"""Regression tests for the independent Sprint 8 runtime fixture oracle."""

from __future__ import annotations

import copy
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from runtime_mvp_fixture import (  # noqa: E402
    MANIFEST_PATH,
    FixtureError,
    strict_json,
    validate,
    validate_fixture_sources,
    validate_golden,
    validate_manifest,
    validate_oracle_independence,
)


class RuntimeMvpFixtureTests(unittest.TestCase):
    def test_committed_fixture_and_oracle_close_exactly(self) -> None:
        result = validate()
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["fixture_files"], 11)

    def test_manifest_rejects_unsafe_or_unbound_files(self) -> None:
        manifest = strict_json(MANIFEST_PATH)
        unsafe = copy.deepcopy(manifest)
        unsafe["files"][0]["path"] = "../outside"
        with self.assertRaises(FixtureError):
            validate_manifest(unsafe)

        missing = copy.deepcopy(manifest)
        missing["files"].pop()
        with self.assertRaises(FixtureError):
            validate_manifest(missing)

        altered = copy.deepcopy(manifest)
        altered["files"][0]["sha256"] = "0" * 64
        with self.assertRaises(FixtureError):
            validate_manifest(altered)

    def test_golden_rejects_weaker_limits_and_false_mapping(self) -> None:
        golden = strict_json(SCRIPT_DIR / "fixtures/runtime_mvp_oracle/golden-runtime.json")
        weakened = copy.deepcopy(golden)
        weakened["limits"]["tree_nodes"] = 10001
        with self.assertRaises(FixtureError):
            validate_golden(weakened)

        false_mapping = copy.deepcopy(golden)
        false_mapping["tree"]["runtime_only"] = "StaticNode"
        with self.assertRaises(FixtureError):
            validate_golden(false_mapping)

        wrong_marker = copy.deepcopy(golden)
        wrong_marker["viewport"]["marker_rgb"] = [0, 0, 0]
        with self.assertRaises(FixtureError):
            validate_golden(wrong_marker)

    def test_fixture_requires_crash_hang_and_normal_quit_commands(self) -> None:
        golden = strict_json(SCRIPT_DIR / "fixtures/runtime_mvp_oracle/golden-runtime.json")
        script_path = SCRIPT_DIR / "fixtures/runtime_mvp_project/scripts/runtime_fixture.gd"
        script = script_path.read_text(encoding="utf-8")
        for command in ("crash", "hang", "quit"):
            weakened = script.replace(f'"{command}"', f'"missing_{command}"')
            with self.subTest(command=command), self.assertRaises(FixtureError):
                validate_fixture_sources(golden, {"script": weakened})

    def test_oracle_has_no_production_mapping_or_query_import(self) -> None:
        validate_oracle_independence()

    def test_strict_loader_rejects_duplicate_and_non_finite_json(self) -> None:
        with tempfile.TemporaryDirectory(prefix="s8-fixture-test.") as directory:
            path = Path(directory) / "bad.json"
            path.write_text('{"status":"passed","status":"passed"}', encoding="utf-8")
            with self.assertRaises(FixtureError):
                strict_json(path)
            path.write_text('{"value":NaN}', encoding="utf-8")
            with self.assertRaises(FixtureError):
                strict_json(path)


if __name__ == "__main__":
    unittest.main()
