from __future__ import annotations

import copy
import hashlib
import sys
import unittest
from pathlib import Path
from typing import Any

SCRIPT_ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_ROOT))

import sprint11_same_project as same_project


def digest(label: str) -> str:
    return "sha256:" + hashlib.sha256(label.encode("utf-8")).hexdigest()


def busy_status() -> dict[str, Any]:
    return {
        "schema_version": same_project.CONNECTION_SCHEMA,
        "status": "project_session_busy",
        "bridge": {
            "condition": "ready",
            "negotiated_protocol": "1.8",
        },
        "static_cache": {
            "condition": "unavailable",
            "schema": None,
            "generation": None,
            "revisions": None,
            "source_hashes_verified": False,
            "age_seconds": None,
        },
        "components": {
            "editor": "ready",
            "runtime": "unavailable",
            "transactions": "unavailable",
        },
        "diagnostic": {"code": "project_session_busy"},
        "remediation_id": "wait_for_project_session",
        "next_action": same_project.BUSY_ACTION,
    }


def valid_report() -> dict[str, Any]:
    return {
        "schema_version": same_project.REPORT_SCHEMA,
        "status": "passed",
        "platform": "macos-arm64",
        "protocol": "2025-11-25",
        "bridge_rpc": "1.8",
        "package_version": "0.1.1",
        "artifacts": {
            "godot_sha256": digest("godot"),
            "sidecar_sha256": digest("sidecar"),
        },
        "registry": {
            "tools": 41,
            "digest": digest("registry"),
        },
        "assertions": {
            "exactly_one_owner": True,
            "busy_not_syncing": True,
            "busy_projection_fail_closed": True,
            "diagnostic_only_standby": True,
            "graceful_takeover": True,
            "crash_takeover": True,
            "takeover_within_90_seconds": True,
            "transaction_coordinator_follows_index_lease": True,
        },
        "source_unchanged": True,
        "cleanup": {
            "editor_stopped": True,
            "owner_stopped": True,
            "standby_stopped": True,
            "successor_stopped": True,
        },
        "redaction": True,
    }


class Sprint11SameProjectContractTests(unittest.TestCase):
    def test_takeover_limit_is_exactly_ninety_seconds(self) -> None:
        self.assertEqual(same_project.TAKEOVER_LIMIT_SECONDS, 90.0)

    def test_busy_projection_is_explicit_and_fail_closed(self) -> None:
        self.assertTrue(
            same_project._busy_projection_is_exact(busy_status())
        )
        for field, value in (
            ("status", "syncing"),
            ("next_action", "Wait for the project sync to complete."),
        ):
            changed = busy_status()
            changed[field] = value
            self.assertFalse(
                same_project._busy_projection_is_exact(changed),
                field,
            )

        changed = busy_status()
        changed["static_cache"]["condition"] = "rebuilding"
        self.assertFalse(same_project._busy_projection_is_exact(changed))

        changed = busy_status()
        changed["components"]["transactions"] = "ready"
        self.assertFalse(same_project._busy_projection_is_exact(changed))

    def test_report_contract_requires_every_takeover_proof(self) -> None:
        self.assertEqual(
            same_project.validate_report(valid_report()),
            valid_report(),
        )
        for assertion in valid_report()["assertions"]:
            changed = valid_report()
            changed["assertions"][assertion] = False
            with self.subTest(assertion=assertion):
                with self.assertRaisesRegex(
                    same_project.s9.WorkflowError,
                    "same-project proof differs",
                ):
                    same_project.validate_report(changed)

    def test_report_rejects_private_binding_material(self) -> None:
        changed = valid_report()
        changed["package_version"] = "/tmp/private"
        with self.assertRaisesRegex(
            same_project.s9.WorkflowError,
            "private binding",
        ):
            same_project.validate_report(changed)

    def test_wait_does_not_accept_syncing_as_contention(self) -> None:
        syncing = busy_status()
        syncing["status"] = "syncing"
        syncing["diagnostic"]["code"] = "static_cache_rebuilding"

        class StatusClient:
            def __init__(self) -> None:
                self.responses = [syncing, busy_status()]
                self.calls = 0

            def tool(
                self,
                _name: str,
                _arguments: dict[str, Any],
            ) -> tuple[dict[str, Any], bool, float]:
                response = self.responses[min(self.calls, 1)]
                self.calls += 1
                return copy.deepcopy(response), False, 0.0

        client = StatusClient()
        observed = same_project._wait_for_projection(
            client,  # type: ignore[arg-type]
            expected="project_session_busy",
            timeout=1.0,
        )
        self.assertEqual(observed["status"], "project_session_busy")
        self.assertEqual(client.calls, 2)


if __name__ == "__main__":
    unittest.main()
