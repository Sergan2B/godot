"""Tests for the independent S9-01 approval and WRITE-001 contract evidence."""

from __future__ import annotations

import ast
import copy
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import sprint9_approval_host_probe as host_probe  # noqa: E402
import sprint9_contracts as contract  # noqa: E402


class Sprint9ContractTests(unittest.TestCase):
    document: dict[str, Any]

    @classmethod
    def setUpClass(cls) -> None:
        value = contract.strict_json_load(contract.VECTORS_PATH)
        if not isinstance(value, dict):
            raise TypeError("approval contract root is not an object")
        cls.document = value

    def test_draft_2020_12_schema_and_rust_golden_vector(self) -> None:
        contract.run_draft_schema_tests()

    def test_positive_receipt_reproduces_independently(self) -> None:
        result = contract.validate_positive(self.document["positive"])
        self.assertEqual(423, result["canonical_length"])
        self.assertEqual(
            "sha256:f458905442d7891185a9fb9c8b029fe353f3625393bb4055104f873042716b75",
            result["canonical_sha256"],
        )
        self.assertEqual("UA22qIiXEf_HN0OEg83xEyJjdrSlNZBop2DYeAMaeEM", result["mac"])

    def test_all_ten_named_negatives_fail_with_exact_errors(self) -> None:
        outcomes = contract.validate_negative_cases(self.document)
        self.assertEqual(10, len(outcomes))
        self.assertEqual("preview_mismatch", outcomes["receipt.wrong_digest"])
        self.assertEqual("approval_scope_mismatch", outcomes["receipt.wrong_scope"])
        self.assertEqual("approval_replayed", outcomes["receipt.replayed"])
        self.assertEqual("stale_scene_revision", outcomes["receipt.stale_revision"])

    def test_duplicate_json_members_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-s9-contract-") as directory:
            path = Path(directory) / "duplicate.json"
            path.write_text('{"schema_version":1,"schema_version":1}', encoding="utf-8")
            with self.assertRaisesRegex(contract.ContractError, "duplicate JSON member"):
                contract.strict_json_load(path)

    def test_receipt_mac_and_shape_tampering_fail_closed(self) -> None:
        positive = copy.deepcopy(self.document["positive"])
        positive["receipt"]["approved"] = True
        with self.assertRaisesRegex(contract.ContractError, "approval_invalid"):
            contract.verify_receipt(
                positive,
                positive["coordinates"],
                positive["current_ms"],
                set(),
            )
        positive = copy.deepcopy(self.document["positive"])
        positive["receipt"]["mac"] = "AA22qIiXEf_HN0OEg83xEyJjdrSlNZBop2DYeAMaeEM"
        with self.assertRaisesRegex(contract.ContractError, "approval_invalid"):
            contract.verify_receipt(
                positive,
                positive["coordinates"],
                positive["current_ms"],
                set(),
            )

    def test_docs_and_production_registry_match_the_gate(self) -> None:
        audit = contract.audit_documents_and_registry()
        self.assertEqual(4, audit["documents"])
        self.assertEqual(25, audit["production_tools"])
        self.assertEqual(11, audit["reserved_tools"])

    def test_validator_is_independent_of_production_implementation(self) -> None:
        source = Path(contract.__file__).read_text(encoding="utf-8")
        imported = {
            alias.name.split(".")[0]
            for node in ast.walk(ast.parse(source))
            if isinstance(node, ast.Import)
            for alias in node.names
        }
        self.assertFalse({"rmcp", "godot_codex_mcp", "codex_bridge"} & imported)

    def test_vectors_are_synthetic_and_path_free(self) -> None:
        serialized = json.dumps(self.document, ensure_ascii=False)
        for forbidden in ("/Users/", "C:\\\\", ".godot/codex", "bridge_session_token"):
            self.assertNotIn(forbidden, serialized)
        self.assertEqual(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            self.document["positive"]["session_token_hex"],
        )

    def test_local_host_trace_accepts_only_form_capable_protocols(self) -> None:
        trace = {
            "schema_version": "approval-probe/1.0",
            "status": "ok",
            "code": "approval_accepted",
            "host": {
                "name": "codex-mcp-client",
                "version": "test",
                "protocol_version": "2025-06-18",
                "form_elicitation": True,
            },
            "action": "accept",
            "confirmed": True,
            "receipt_eligible": True,
            "project_mutated": False,
        }
        with tempfile.TemporaryDirectory(prefix="codex-s9-host-") as directory:
            path = Path(directory) / "trace.json"
            path.write_text(json.dumps(trace), encoding="utf-8")
            if os.name == "posix":
                path.chmod(0o600)
            self.assertEqual("2025-06-18", host_probe.validate_trace(path)["host"]["protocol_version"])
            trace["host"]["protocol_version"] = "2025-03-26"
            path.write_text(json.dumps(trace), encoding="utf-8")
            with self.assertRaisesRegex(host_probe.ApprovalProbeError, "pre-elicitation"):
                host_probe.validate_trace(path)


if __name__ == "__main__":
    unittest.main()
