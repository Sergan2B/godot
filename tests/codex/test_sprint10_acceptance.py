from __future__ import annotations

import copy
import unittest

import sprint10_acceptance as acceptance


class Sprint10AcceptanceTests(unittest.TestCase):
    def model_report(self) -> dict:
        checks = {
            "diagnostics": "passed",
            "index_convergence": "passed",
            "intrinsic": "passed",
            "persistence": "passed",
            "reload_reparse": "passed",
            "runtime": "skipped",
            "semantic_graph": "passed",
        }
        return {
            "schema_version": "s10-model-free-live/1.0",
            "status": "passed",
            "platform": "macos-arm64",
            "protocol": "2025-11-25",
            "bridge_rpc": "1.8",
            "tool_registry": 40,
            "change_set_id": "change-set:" + "a" * 32,
            "validation_report_id": "validation-report:" + "b" * 32,
            "operation_count": 2,
            "one_native_action": True,
            "prepare_read_only": True,
            "exact_undo": True,
            "validation": {
                "outcome": "passed",
                "checks": checks,
                "page_count": 1,
                "retained_bytes": 4096,
            },
            "latency_ms": {
                "read": [1.0],
                "prepare": [2.0],
                "apply": [3.0],
                "status": [4.0],
                "report": [5.0],
                "undo": [6.0],
            },
            "cleanup": {"processes_stopped": True},
        }

    def test_model_free_projection_is_strict(self) -> None:
        report = self.model_report()
        acceptance.validate_model_free(report)
        self.assertEqual(acceptance.performance_projection(report)["prepare"]["p95_ms"], 2.0)

    def test_registry_and_required_check_downgrades_fail(self) -> None:
        report = self.model_report()
        report["tool_registry"] = 39
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_model_free(report)

        report = self.model_report()
        report["validation"]["checks"]["semantic_graph"] = "inconclusive"
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_model_free(report)

    def test_evidence_redaction_rejects_opaque_and_host_values(self) -> None:
        safe = {"status": "passed", "digest": "sha256:" + "c" * 64}
        acceptance.safe_evidence_scan(safe)
        for leak in ("/Users/private/project", "change-set:" + "d" * 32):
            value = copy.deepcopy(safe)
            value["leak"] = leak
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.safe_evidence_scan(value)


if __name__ == "__main__":
    unittest.main()
