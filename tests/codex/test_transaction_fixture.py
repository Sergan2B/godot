"""Named-negative regressions for the independent Sprint 9 transaction oracle."""

from __future__ import annotations

import copy
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT_ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_ROOT))

import transaction_fixture as fixture  # noqa: E402


class TransactionFixtureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = fixture.strict_json(fixture.MANIFEST_PATH)
        cls.golden = fixture.strict_json(fixture.GOLDEN_PATH)

    def test_committed_fixture_and_oracle_close_exactly(self) -> None:
        result = fixture.validate()
        self.assertEqual("passed", result["status"])
        self.assertEqual(14, result["fixture_files"])
        self.assertEqual(8, result["operations"])
        self.assertEqual(8, result["fault_points"])
        self.assertEqual(18, result["baseline_nodes"])

    def test_manifest_rejects_hash_drift_unsafe_path_and_unbound_file(self) -> None:
        drifted = copy.deepcopy(self.manifest)
        drifted["files"][0]["sha256"] = "0" * 64
        with self.assertRaisesRegex(fixture.FixtureError, "fixture hash differs"):
            fixture.validate_manifest(drifted)

        unsafe = copy.deepcopy(self.manifest)
        unsafe["files"][0]["path"] = "../outside"
        with self.assertRaisesRegex(fixture.FixtureError, "unsafe path"):
            fixture.validate_manifest(unsafe)

        unbound = copy.deepcopy(self.manifest)
        unbound["files"].pop()
        with self.assertRaisesRegex(fixture.FixtureError, "file closure differs"):
            fixture.validate_manifest(unbound)

    def test_strict_json_rejects_duplicate_and_non_finite_members(self) -> None:
        with tempfile.TemporaryDirectory(prefix="s9-oracle-json-") as directory:
            path = Path(directory) / "bad.json"
            path.write_text('{"status":true,"status":false}', encoding="utf-8")
            with self.assertRaisesRegex(fixture.FixtureError, "duplicate JSON member"):
                fixture.strict_json(path)
            path.write_text('{"value":NaN}', encoding="utf-8")
            with self.assertRaisesRegex(fixture.FixtureError, "non-finite"):
                fixture.strict_json(path)

    def test_missing_operation_and_fault_point_fail_closed(self) -> None:
        missing_operation = copy.deepcopy(self.golden)
        missing_operation["operations"].pop()
        with self.assertRaisesRegex(fixture.FixtureError, "coverage differs"):
            fixture.validate_golden(missing_operation)

        missing_fault = copy.deepcopy(self.golden)
        missing_fault["faults"].pop()
        with self.assertRaisesRegex(fixture.FixtureError, "coverage differs"):
            fixture.validate_golden(missing_fault)

    def test_false_no_mutation_claim_is_detected(self) -> None:
        operation = self.golden["operations"][0]
        applied = fixture.materialize_operation(self.golden["baseline"], operation)
        states = {
            "pre": self.golden["baseline"],
            "prepared": applied,
            "applied": applied,
            "undone": self.golden["baseline"],
            "redone": applied,
        }
        transitions = [
            {"operation_seq": 10},
            {"operation_seq": 10},
            {"operation_seq": 11},
            {"operation_seq": 12},
            {"operation_seq": 13},
        ]
        with self.assertRaisesRegex(fixture.FixtureError, "prepare mutated"):
            fixture.validate_cycle("attach_script", states, transitions, self.golden)

    def test_incomplete_undo_and_wrong_redo_are_detected(self) -> None:
        operation = next(
            item for item in self.golden["operations"] if item["kind"] == "set_property"
        )
        applied = fixture.materialize_operation(self.golden["baseline"], operation)
        transitions = [
            {"operation_seq": 20},
            {"operation_seq": 20},
            {"operation_seq": 21},
            {"operation_seq": 22},
            {"operation_seq": 23},
        ]
        states = {
            "pre": self.golden["baseline"],
            "prepared": self.golden["baseline"],
            "applied": applied,
            "undone": applied,
            "redone": applied,
        }
        with self.assertRaisesRegex(fixture.FixtureError, "Undo is not exact"):
            fixture.validate_cycle("set_property", states, transitions, self.golden)

        states["undone"] = self.golden["baseline"]
        states["redone"] = self.golden["baseline"]
        with self.assertRaisesRegex(fixture.FixtureError, "Redo differs"):
            fixture.validate_cycle("set_property", states, transitions, self.golden)

    def test_wrong_owner_index_and_transform_are_detected(self) -> None:
        operation = next(
            item for item in self.golden["operations"] if item["kind"] == "reparent_node"
        )
        applied = fixture.materialize_operation(self.golden["baseline"], operation)
        transitions = [
            {"operation_seq": 1},
            {"operation_seq": 1},
            {"operation_seq": 2},
            {"operation_seq": 3},
            {"operation_seq": 4},
        ]
        for field, value in (("owner", "WrongOwner"), ("index", 9)):
            wrong = copy.deepcopy(applied)
            row = next(
                item
                for item in wrong["tree"]
                if item["path"] == "NewParent/ReparentTarget"
            )
            row[field] = value
            states = {
                "pre": self.golden["baseline"],
                "prepared": self.golden["baseline"],
                "applied": wrong,
                "undone": self.golden["baseline"],
                "redone": wrong,
            }
            with self.subTest(field=field), self.assertRaisesRegex(
                fixture.FixtureError, "applied state differs"
            ):
                fixture.validate_cycle(
                    "reparent_node", states, transitions, self.golden
                )

        property_operation = next(
            item for item in self.golden["operations"] if item["kind"] == "set_property"
        )
        property_applied = fixture.materialize_operation(
            self.golden["baseline"], property_operation
        )
        player = next(
            item for item in property_applied["tree"] if item["path"] == "Player"
        )
        player["transform"]["global"][-1] = 31
        states = {
            "pre": self.golden["baseline"],
            "prepared": self.golden["baseline"],
            "applied": property_applied,
            "undone": self.golden["baseline"],
            "redone": property_applied,
        }
        with self.assertRaisesRegex(fixture.FixtureError, "applied state differs"):
            fixture.validate_cycle("set_property", states, transitions, self.golden)

    def test_stale_success_and_second_response_loss_action_are_detected(self) -> None:
        stale = copy.deepcopy(self.golden)
        stale["stale_cases"][0]["native_actions"] = 1
        stale["stale_cases"][0]["executor_reached"] = True
        with self.assertRaisesRegex(fixture.FixtureError, "reaches mutation"):
            fixture.validate_golden(stale)

        observation = copy.deepcopy(self.golden["faults"][3])
        self.assertEqual("after_commit_before_response", observation["point"])
        observation["native_actions"] = 2
        with self.assertRaisesRegex(fixture.FixtureError, "native_actions differs"):
            fixture.validate_fault_observation(observation, self.golden)

    def test_weakened_limits_and_volatile_golden_ids_fail_closed(self) -> None:
        weakened = copy.deepcopy(self.golden)
        weakened["limits"]["structural_nodes"] = 1001
        with self.assertRaisesRegex(fixture.FixtureError, "limits differ"):
            fixture.validate_golden(weakened)

        volatile = copy.deepcopy(self.golden)
        volatile["baseline"]["tree"][0]["transaction_id"] = (
            "transaction:" + "a" * 32
        )
        with self.assertRaisesRegex(
            fixture.FixtureError, "volatile ID entered golden state"
        ):
            fixture.validate_golden(volatile)

    def test_source_byte_or_metadata_change_is_detected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="s9-source-") as directory:
            project = Path(directory) / "project"
            shutil.copytree(fixture.PROJECT_ROOT, project, ignore=shutil.ignore_patterns(".godot"))
            before = fixture.source_fingerprint(project)
            path = project / "scripts/fixture_endpoint.gd"
            path.write_bytes(path.read_bytes() + b"\n")
            after = fixture.source_fingerprint(project)
            with self.assertRaisesRegex(
                fixture.FixtureError, "bytes or metadata changed"
            ):
                fixture.assert_source_unchanged(before, after)

    def test_canary_native_id_path_and_approval_leaks_fail_closed(self) -> None:
        cases = [
            (
                {"safe": fixture.CANARIES[0]},
                {},
                "raw canary leaked",
            ),
            (
                {"safe": "987654321"},
                {"native_ids": [987654321]},
                "native ID leaked",
            ),
            (
                {"safe": "/tmp/s9-private/project"},
                {"absolute_paths": ["/tmp/s9-private/project"]},
                "absolute path leaked",
            ),
            (
                {"safe": "approval-mac-value"},
                {"approval_material": ["approval-mac-value"]},
                "approval material leaked",
            ),
        ]
        for value, kwargs, message in cases:
            with self.subTest(message=message), self.assertRaisesRegex(
                fixture.FixtureError, message
            ):
                fixture.scan_safe_surface("journal", value, **kwargs)

    def test_preview_payload_is_allowed_only_in_prepare_surface(self) -> None:
        value = {"preview_payload_json": "{\"redacted\":true}"}
        fixture.scan_safe_surface("bridge_prepare", value)
        with self.assertRaisesRegex(fixture.FixtureError, "forbidden output key"):
            fixture.scan_safe_surface("journal", value)

    def test_identity_relations_reject_reuse_and_bad_format(self) -> None:
        first = {"transaction_id": "transaction:" + "1" * 32}
        replay = copy.deepcopy(first)
        distinct = {"transaction_id": "transaction:" + "2" * 32}
        fixture.validate_volatile_identity(first, replay, distinct)

        reused = copy.deepcopy(first)
        with self.assertRaisesRegex(fixture.FixtureError, "reused transaction ID"):
            fixture.validate_volatile_identity(first, replay, reused)
        malformed = {"transaction_id": "transaction:native-42"}
        with self.assertRaisesRegex(fixture.FixtureError, "format differs"):
            fixture.validate_volatile_identity(malformed, malformed, distinct)

    def test_validator_has_no_production_or_existing_runner_import(self) -> None:
        fixture.validate_oracle_independence()

    def test_error_messages_do_not_echo_values(self) -> None:
        canary = fixture.CANARIES[1]
        try:
            fixture.scan_safe_surface("logs", {"message": canary})
        except fixture.FixtureError as error:
            self.assertNotIn(canary, str(error))
            self.assertLess(len(str(error).encode("utf-8")), 256)
        else:
            self.fail("leak scan unexpectedly accepted a canary")

    def test_committed_oracle_contains_no_raw_canaries(self) -> None:
        serialized = json.dumps(self.golden, sort_keys=True)
        for canary in fixture.CANARIES:
            self.assertNotIn(canary, serialized)


if __name__ == "__main__":
    unittest.main()
