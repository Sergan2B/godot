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
    validate_golden,
    validate_manifest,
)


class RuntimeMvpFixtureTests(unittest.TestCase):
    def test_committed_fixture_and_oracle_close_exactly(self) -> None:
        result = validate()
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["fixture_files"], 9)

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
