"""Contract tests for the independent Sprint 5 script-semantics vectors."""

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

import script_semantics_contract as contract  # noqa: E402


class ScriptSemanticsContractTests(unittest.TestCase):
    document: dict[str, Any]

    @classmethod
    def setUpClass(cls) -> None:
        value = contract.strict_json_load(contract.VECTORS_PATH)
        if not isinstance(value, dict):
            raise TypeError("contract root is not an object")
        cls.document = value

    def test_draft_2020_12_schema_and_named_negatives(self) -> None:
        contract.run_draft_schema_tests()

    def test_identity_vectors_reproduce_and_scope_exactly(self) -> None:
        identities = contract.validate_identity_vectors(self.document)
        self.assertEqual(9, len(identities))
        self.assertEqual(identities["player_class"], identities["player_class_after_body_edit"])
        self.assertEqual(identities["take_damage_method"], identities["take_damage_after_line_shift"])
        self.assertNotEqual(identities["take_damage_method"], identities["renamed_receive_damage"])
        self.assertNotEqual(identities["local_cafe"], identities["local_cafe_after_line_shift"])

    def test_ranges_cover_unicode_tabs_and_crlf(self) -> None:
        ranges = contract.validate_range_vectors(self.document)
        self.assertEqual(5, len(ranges))
        self.assertEqual((168, 172), ranges["astral_literal"])
        self.assertEqual((24, 35), ranges["crlf_second_line"])

    def test_confidence_is_independent_from_freshness(self) -> None:
        values = contract.validate_confidence_vectors(self.document)
        self.assertEqual("exact", values["resolved_method_call"])
        self.assertEqual("dynamic", values["dynamic_call"])
        self.assertEqual("exact", values["stale_exact_reference"])
        stale = next(value for value in self.document["confidence_vectors"] if value["name"] == "stale_exact_reference")
        self.assertFalse(stale["servable_as_current"])

    def test_duplicate_json_members_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-s5-contract-") as directory:
            path = Path(directory) / "duplicate.json"
            path.write_text('{"schema_version":1,"schema_version":1}', encoding="utf-8")
            with self.assertRaisesRegex(contract.ContractError, "duplicate JSON member"):
                contract.strict_json_load(path)

    def test_unsafe_source_and_drifted_hash_fail_closed(self) -> None:
        unsafe = copy.deepcopy(self.document)
        unsafe["sources"][0]["path"] = "res://../secret.gd"
        with self.assertRaisesRegex(contract.ContractError, "unsafe source vector"):
            contract.validate_identity_vectors(unsafe)
        drifted = copy.deepcopy(self.document)
        drifted["sources"][0]["content"] += "# drift\n"
        with self.assertRaisesRegex(contract.ContractError, "source hash mismatch"):
            contract.validate_range_vectors(drifted)

    def test_d07_source_invariants_select_saved_content_cache(self) -> None:
        audit = contract.d07_source_audit()
        self.assertEqual("bridge_owned_exact_content_cache", audit["selected"])
        self.assertTrue(audit["lsp_requires_client"])
        self.assertTrue(audit["lsp_may_contain_dirty_text"])
        self.assertTrue(audit["direct_projection_uses_godot_parser_analyzer"])

    def test_validator_is_independent_of_production_index(self) -> None:
        source = Path(contract.__file__).read_text(encoding="utf-8")
        forbidden = ("godot_codex_mcp", "ScriptSemanticAdapter", "IndexStore", "segment-v3")
        self.assertFalse(any(name in source for name in forbidden))
        imported = {
            alias.name.split(".")[0]
            for node in ast.walk(ast.parse(source))
            if isinstance(node, ast.Import)
            for alias in node.names
        }
        self.assertFalse({"godot_codex_mcp", "codex_bridge"} & imported)

    def test_vectors_have_no_absolute_or_cache_paths(self) -> None:
        serialized = json.dumps(self.document, ensure_ascii=False)
        for forbidden in ("/Users/", "C:\\", ".godot/imported", ".godot/codex"):
            self.assertNotIn(forbidden, serialized)


if __name__ == "__main__":
    unittest.main()
