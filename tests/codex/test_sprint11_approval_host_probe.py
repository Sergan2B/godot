from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path
from typing import Any

from tests.codex import sprint11_approval_host_probe as probe


def trace(decision: str) -> dict[str, Any]:
    code, status, receipt_eligible, confirmed = probe.OUTCOME[decision]
    return {
        "schema_version": "approval-probe/1.0",
        "status": status,
        "code": code,
        "host": {
            "name": "codex-mcp-client",
            "version": "0.145.0-alpha.30",
            "protocol_version": "2025-06-18",
            "form_elicitation": True,
        },
        "action": decision,
        "confirmed": confirmed,
        "receipt_eligible": receipt_eligible,
        "project_mutated": False,
    }


class Sprint11ApprovalHostProbeTests(unittest.TestCase):
    def write_trace(self, directory: str, value: dict[str, Any]) -> Path:
        path = Path(directory) / "trace.json"
        path.write_text(
            json.dumps(value, separators=(",", ":"), sort_keys=True) + "\n",
            encoding="utf-8",
        )
        if os.name == "posix":
            path.chmod(0o600)
        return path

    def test_all_form_outcomes_are_exact_and_non_mutating(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            for decision in probe.DECISIONS:
                path = self.write_trace(directory, trace(decision))
                self.assertEqual(
                    probe.validate_trace(path, decision)["action"],
                    decision,
                )
                path.unlink()

    def test_wrong_action_receipt_or_confirmation_is_rejected(self) -> None:
        mutations = (
            ("action", "cancel"),
            ("receipt_eligible", False),
            ("confirmed", False),
            ("project_mutated", True),
        )
        with tempfile.TemporaryDirectory() as directory:
            for field, value in mutations:
                changed = trace("accept")
                changed[field] = value
                path = self.write_trace(directory, changed)
                with self.assertRaises(probe.ApprovalProbeError):
                    probe.validate_trace(path, "accept")
                path.unlink()

    def test_private_material_and_extra_fields_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            for field in ("nonce", "receipt", "source_text"):
                changed = trace("accept")
                changed[field] = "private"
                path = self.write_trace(directory, changed)
                with self.assertRaises(probe.ApprovalProbeError):
                    probe.validate_trace(path, "accept")
                path.unlink()

    def test_prompt_instructions_keep_outcomes_distinct(self) -> None:
        instructions = {
            decision: probe._instruction(decision)
            for decision in probe.DECISIONS
        }
        self.assertEqual(len(set(instructions.values())), 4)
        self.assertIn("Allow action", instructions["accept"])
        self.assertIn("decline action", instructions["decline"])
        self.assertIn("cancel action", instructions["cancel"])
        self.assertIn("leave the form unanswered", instructions["timeout"])

    def test_only_safe_unique_configured_server_names_are_returned(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "config.toml"
            config.write_text(
                """
[mcp_servers.node_repl]
command = "node"

[mcp_servers.openaiDeveloperDocs]
url = "https://example.invalid"

[mcp_servers.disabled]
enabled = false
command = "false"

[mcp_servers.godot_s11_approval_probe]
command = "probe"
""",
                encoding="utf-8",
            )
            self.assertEqual(
                probe.configured_mcp_server_names(
                    Path("/bin/codex"),
                    config_path=config,
                ),
                ("node_repl", "openaiDeveloperDocs"),
            )

            config.write_text(
                '[mcp_servers."bad.name"]\ncommand = "false"\n',
                encoding="utf-8",
            )
            with self.assertRaises(probe.ApprovalProbeError):
                probe.configured_mcp_server_names(
                    Path("/bin/codex"),
                    config_path=config,
                )


if __name__ == "__main__":
    unittest.main()
