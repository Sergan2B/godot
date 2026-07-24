from __future__ import annotations

import copy
import unittest

import sprint10_fixture as fixture


class Sprint10FixtureTests(unittest.TestCase):
    def test_frozen_fixture_and_oracle_validate(self) -> None:
        report = fixture.validate()
        self.assertEqual(report["status"], "passed")
        self.assertEqual(report["operation_kinds"], 11)
        self.assertEqual(report["fault_scenarios"], 11)

    def test_operation_limit_and_alias_negatives_are_mandatory(self) -> None:
        golden = fixture.strict_json(fixture.GOLDEN_PATH)
        invalid = copy.deepcopy(golden["invalid_change_sets"])
        golden["invalid_change_sets"] = [
            item for item in invalid if item["id"] != "dependency_cycle"
        ]
        with self.assertRaises(fixture.FixtureError):
            fixture.validate_golden(golden)

    def test_report_and_confirmation_limits_fail_closed(self) -> None:
        golden = fixture.strict_json(fixture.GOLDEN_PATH)
        golden["report_limits"]["page_bytes"] += 1
        with self.assertRaises(fixture.FixtureError):
            fixture.validate_golden(golden)

        golden = fixture.strict_json(fixture.GOLDEN_PATH)
        golden["confirmation_matrix"]["session_grant"].append("destructive")
        with self.assertRaises(fixture.FixtureError):
            fixture.validate_golden(golden)


if __name__ == "__main__":
    unittest.main()
