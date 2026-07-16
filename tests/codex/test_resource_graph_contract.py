"""Contract tests for the Sprint 3 resource graph oracle."""

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

import resource_graph_fixture as fixture  # noqa: E402


class ResourceGraphContractTests(unittest.TestCase):
    manifest: dict[str, Any]
    golden: dict[str, Any]

    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = fixture.load_manifest()
        cls.golden = fixture.strict_json_load(fixture.GOLDEN_PATH)
        if not isinstance(cls.golden, dict):
            raise AssertionError("golden resource graph must be an object")

    def test_draft_2020_12_schemas_and_named_negatives(self) -> None:
        fixture.run_draft_schema_tests()

    def test_identity_and_path_vectors_reproduce_exactly(self) -> None:
        fixture.validate_identity_vectors()

    def test_manifest_rejects_duplicate_uid_and_path(self) -> None:
        duplicate_uid = copy.deepcopy(self.manifest)
        duplicate_uid["resources"][1]["uid"] = duplicate_uid["resources"][0]["uid"]
        with self.assertRaisesRegex(fixture.FixtureError, "duplicate_resource_uid"):
            fixture.validate_manifest_semantics(duplicate_uid)

        duplicate_path = copy.deepcopy(self.manifest)
        duplicate_path["resources"][1]["path"] = duplicate_path["resources"][0]["path"]
        with self.assertRaisesRegex(fixture.FixtureError, "path_normalization_collision"):
            fixture.validate_manifest_semantics(duplicate_path)

    def test_golden_phases_have_graph_and_transition_invariants(self) -> None:
        self.assertEqual(
            [
                "base",
                "rename_uid",
                "rename_uidless",
                "delete",
                "re_add",
                "reimport",
                "content_edit",
                "journal_gap",
            ],
            [phase["name"] for phase in self.golden["phases"]],
        )
        for phase in self.golden["phases"]:
            fixture.validate_phase_invariants(phase)
        fixture.validate_transition_relations(self.golden)

    def test_base_topology_and_format_matrix_are_exact(self) -> None:
        base = self.golden["phases"][0]
        formats = {resource["format"] for resource in base["resources"]}
        self.assertEqual({"tres", "res", "tscn", "scn", "imported"}, formats)
        edges = {(edge["source"], edge["resolved_target"], edge["resolution"]) for edge in base["dependencies"]}
        required = {
            ("chain_root", "chain_mid", "resolved"),
            ("chain_mid", "shared_leaf", "resolved"),
            ("fan_out", "shared_leaf", "resolved"),
            ("fan_out", "unique_leaf", "resolved"),
            ("fan_in_a", "shared_leaf", "resolved"),
            ("fan_in_b", "shared_leaf", "resolved"),
            ("cycle_a", "cycle_b", "resolved"),
            ("cycle_b", "cycle_a", "resolved"),
            ("missing_owner", None, "missing"),
            ("stale_uid_owner", None, "stale_uid"),
        }
        self.assertTrue(required.issubset(edges))
        self.assertTrue(all(edge["declared_type"] == "Resource" for edge in base["dependencies"]))
        direct_sources = {edge["source"] for edge in base["dependencies"]}
        self.assertNotIn("orphan", direct_sources)
        reverse_targets = {
            edge["resolved_target"] for edge in base["dependencies"] if edge["resolved_target"] is not None
        }
        self.assertNotIn("orphan", reverse_targets)

    def test_missing_and_stale_diagnostics_are_distinct(self) -> None:
        base = self.golden["phases"][0]
        diagnostics = {
            (diagnostic["source"], diagnostic["code"], diagnostic["target_reference"])
            for diagnostic in base["diagnostics"]
        }
        self.assertIn(
            ("missing_owner", "missing_dependency", "res://missing/not_created.tres"),
            diagnostics,
        )
        self.assertIn(
            ("stale_uid_owner", "stale_resource_uid", "uid://0"),
            diagnostics,
        )
        self.assertTrue(all(not item["silent_rebind"] for item in base["diagnostics"]))

    def test_equal_bytes_do_not_create_equal_identity(self) -> None:
        twin_a = fixture.PROJECT_SOURCE / "resources" / "twin_a.tres"
        twin_b = fixture.PROJECT_SOURCE / "resources" / "twin_b.tres"
        self.assertEqual(twin_a.read_bytes(), twin_b.read_bytes())
        base = {resource["oracle_id"]: resource for resource in self.golden["phases"][0]["resources"]}
        self.assertEqual(base["twin_a"]["content_generation"], base["twin_b"]["content_generation"])
        self.assertNotEqual(base["twin_a"]["entity_id"], base["twin_b"]["entity_id"])

    def test_restore_is_idempotent_and_cleans_generated_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-rg-contract-") as temporary:
            work_root = Path(temporary) / "project"
            fixture.restore_project(work_root)
            first = fixture.tree_digest(work_root)
            fixture.restore_project(work_root)
            second = fixture.tree_digest(work_root)
            self.assertEqual(first, second)
            self.assertFalse((work_root / ".godot").exists())

    def test_oracle_is_independent_of_production_index(self) -> None:
        driver_source = Path(fixture.__file__).read_text(encoding="utf-8")
        probe_source = fixture.PROBE_SCRIPT.read_text(encoding="utf-8")
        forbidden = ("godot_codex_mcp", "IndexStore", "modules/codex_bridge", "sqlite")
        self.assertFalse(any(name in driver_source for name in forbidden))
        self.assertFalse(any(name in probe_source for name in forbidden))

        parsed = ast.parse(driver_source)
        imported_modules = {
            node.names[0].name.split(".")[0] for node in ast.walk(parsed) if isinstance(node, ast.Import)
        }
        self.assertFalse({"godot_codex_mcp", "codex_bridge"} & imported_modules)

    def test_oracle_semantic_results_have_no_absolute_or_import_payload_paths(self) -> None:
        for phase in self.golden["phases"]:
            semantic = {
                "resources": phase["resources"],
                "dependencies": phase["dependencies"],
                "diagnostics": phase["diagnostics"],
            }
            serialized = json.dumps(semantic, ensure_ascii=False)
            self.assertNotIn("/Users/", serialized)
            self.assertNotIn("C:\\", serialized)
            self.assertNotIn(".godot/imported", serialized)

    def test_binary_and_imported_fixture_inputs_are_present(self) -> None:
        for relative in (
            "resources/unique_leaf.res",
            "resources/fan_in_b.res",
            "scenes/binary_scene.scn",
            "assets/imported_icon.svg",
            "assets/imported_icon.svg.import",
        ):
            path = fixture.PROJECT_SOURCE / relative
            self.assertTrue(path.is_file(), relative)
            self.assertGreater(path.stat().st_size, 0, relative)
        import_sidecar = (fixture.PROJECT_SOURCE / "assets" / "imported_icon.svg.import").read_text(encoding="utf-8")
        self.assertIn('uid="uid://dge6ajgl8jg04"', import_sidecar)
        self.assertIn('source_file="res://assets/imported_icon.svg"', import_sidecar)


if __name__ == "__main__":
    unittest.main()
