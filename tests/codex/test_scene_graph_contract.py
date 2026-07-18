"""Contract tests for the independent Sprint 4 scene graph oracle."""

from __future__ import annotations

import ast
import copy
import json
import sys
import unittest
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import scene_graph_fixture as fixture  # noqa: E402


class SceneGraphContractTests(unittest.TestCase):
    manifest: dict[str, Any]
    golden: dict[str, Any]

    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = fixture.load_manifest()
        cls.golden = fixture.strict_json_load(fixture.GOLDEN_PATH)

    def test_draft_2020_12_schemas_and_named_negatives(self) -> None:
        fixture.run_draft_schema_tests()

    def test_d06_identity_vectors_reproduce_exactly(self) -> None:
        identities = fixture.validate_identity_vectors()
        self.assertEqual(11, len(identities))
        self.assertEqual(identities["base_player"], identities["base_player_after_rename_reparent"])
        self.assertNotEqual(identities["base_player"], identities["duplicated_player"])

    def test_manifest_hashes_and_phases_are_exact(self) -> None:
        fixture.validate_manifest(self.manifest)
        self.assertEqual(fixture.PHASES, tuple(item["name"] for item in self.manifest["phases"]))

    def test_manifest_rejects_duplicate_or_unsafe_paths(self) -> None:
        duplicate = copy.deepcopy(self.manifest)
        duplicate["files"][1]["path"] = duplicate["files"][0]["path"]
        with self.assertRaisesRegex(fixture.FixtureError, "duplicate_or_unsafe_manifest_path"):
            fixture.validate_manifest(duplicate)
        unsafe = copy.deepcopy(self.manifest)
        unsafe["files"][0]["path"] = "../project.godot"
        with self.assertRaisesRegex(fixture.FixtureError, "duplicate_or_unsafe_manifest_path"):
            fixture.validate_manifest(unsafe)

    def test_golden_graph_is_referentially_and_cryptographically_closed(self) -> None:
        records = fixture.validate_golden()
        self.assertEqual(4, len(records["scenes"]))
        self.assertEqual(11, len(records["nodes"]))
        self.assertEqual(9, len(records["occurrences"]))
        self.assertEqual(
            {"local", "inherited", "instance_override"},
            {item["origin"] for item in records["properties"].values()},
        )
        self.assertEqual(6, len(records["resource_references"]))

    def test_nested_instance_and_broken_animation_truth_are_explicit(self) -> None:
        records = fixture.validate_golden()
        nested = records["occurrences"]["nested_actor_player_occ"]
        self.assertEqual(["main_child_instance", "child_nested_actor"], nested["instance_chain"])
        broken = records["animation_tracks"]["broken_move_track"]
        self.assertEqual("broken", broken["resolution"])
        self.assertIsNone(broken["target"])

    def test_project_settings_are_allowlisted_exactly(self) -> None:
        keys = {item["key"] for item in self.golden["project_context"]}
        self.assertEqual(fixture.PROJECT_KEYS, keys)

    def test_subresource_order_equivalence_is_semantic_not_byte_equality(self) -> None:
        fixture.validate_order_equivalence(self.manifest)

    def test_oracle_is_independent_of_production_scene_index(self) -> None:
        source = Path(fixture.__file__).read_text(encoding="utf-8")
        forbidden = ("godot_codex_mcp", "SceneStateAdapter", "modules/codex_bridge", "IndexStore")
        self.assertFalse(any(name in source for name in forbidden))
        imports = {
            node.names[0].name.split(".")[0] for node in ast.walk(ast.parse(source)) if isinstance(node, ast.Import)
        }
        self.assertFalse({"godot_codex_mcp", "codex_bridge"} & imports)

    def test_oracle_has_no_absolute_or_cache_paths(self) -> None:
        serialized = json.dumps(self.golden, ensure_ascii=False)
        for forbidden in ("/Users/", "C:\\", ".godot/imported", ".godot/codex"):
            self.assertNotIn(forbidden, serialized)


if __name__ == "__main__":
    unittest.main()
