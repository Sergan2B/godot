from __future__ import annotations

import io
import json
import os
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from typing import Any

from tests.codex import sprint11_surface_recorder as recorder


def digest(character: str) -> str:
    return "sha256:" + character * 64


def metadata() -> dict[str, Any]:
    return {
        "surface": "cli",
        "host": {
            "name": "codex-cli",
            "identifier": "codex-cli",
            "artifact_kind": "standalone_executable",
            "version": "0.145.0-alpha.30",
            "build": "fixture",
            "commit": None,
            "client_version": "0.145.0-alpha.30",
            "host_metadata_sha256": None,
            "host_code_signature": None,
            "client_code_signature": None,
            "ide_host_version": None,
            "ide_shell_identifier": None,
            "ide_shell_artifact_sha256": None,
            "ide_shell_team_id": None,
            "ide_shell_code_signature": None,
            "architecture": "arm64",
        },
        "bindings": {
            "package_source_commit": "a" * 40,
            "package_manifest_sha256": digest("1"),
            "compatibility_matrix_sha256": digest("2"),
            "host_coordinate_profile_sha256": digest("3"),
            "project_fixture_sha256": digest("4"),
            "registry_sha256": digest("5"),
            "prompt_pack_sha256": digest("6"),
            "host_artifact_sha256": digest("7"),
            "client_artifact_sha256": digest("8"),
            "host_provenance_sha256": digest("b"),
            "mcp_binary_sha256": digest("9"),
            "godot_artifact_sha256": digest("a"),
        },
        "host_controls": {
            "sandbox_approval_observed": True,
            "config_reload_observed": True,
            "root_binding_observed": True,
            "package_launcher_observed": True,
            "package_launcher_path": "bin/godot-codex-mcp",
            "package_launcher_sha256": digest("9"),
        },
    }


def host_provenance_receipt() -> dict[str, Any]:
    profile, profile_digest = recorder.host_provenance._profile(
        recorder.HOST_PROFILE_PATH
    )
    surfaces = []
    for coordinate in profile["surfaces"]:
        surface = coordinate["surface"]

        def observed_signature(field: str) -> dict[str, Any] | None:
            value = coordinate[field]
            if value is None:
                return None
            return {**value, "verified": True}

        measurement: dict[str, Any] = {
            "surface": surface,
            "host_artifact_sha256": coordinate["host_artifact_sha256"],
            "host_metadata_sha256": coordinate["host_metadata_sha256"],
            "client_artifact_sha256": coordinate[
                "client_artifact_sha256"
            ],
            "ide_shell_artifact_sha256": coordinate[
                "ide_shell_artifact_sha256"
            ],
            "host_code_signature": observed_signature(
                "host_code_signature"
            ),
            "client_code_signature": observed_signature(
                "client_code_signature"
            ),
            "ide_shell_code_signature": observed_signature(
                "ide_shell_code_signature"
            ),
            "artifact_measurement": {
                "kind": (
                    "directory_tree" if surface == "ide" else "regular_file"
                ),
                "bytes": 1,
                "files": 1,
                "directories": 1 if surface == "ide" else 0,
            },
            "client_bytes": 1,
        }
        if surface == "ide":
            measurement["metadata_bytes"] = 1
            measurement["ide_shell_bytes"] = 1
        surfaces.append(measurement)
    return {
        "schema_version": recorder.host_provenance.SCHEMA_VERSION,
        "capture_kind": recorder.host_provenance.CAPTURE_KIND,
        "status": "passed",
        "architecture": "arm64",
        "host_coordinate_profile_sha256": profile_digest,
        "surfaces": surfaces,
        "redaction": {
            "absolute_paths_absent": True,
            "account_identity_absent": True,
            "artifact_content_absent": True,
            "environment_secrets_absent": True,
        },
    }


def raw_metadata(provenance_sha256: str) -> dict[str, Any]:
    value = metadata()
    bindings = {
        key: item
        for key, item in value["bindings"].items()
        if key
        not in {
            "host_artifact_sha256",
            "client_artifact_sha256",
            "host_provenance_sha256",
        }
    }
    _profile, profile_digest = recorder.host_provenance._profile(
        recorder.HOST_PROFILE_PATH
    )
    bindings["host_coordinate_profile_sha256"] = profile_digest
    return {
        "surface": "cli",
        "host_provenance": {
            "path": recorder.HOST_PROVENANCE_NAME,
            "sha256": provenance_sha256,
        },
        "bindings": bindings,
        "host_controls": value["host_controls"],
    }


def line(value: Any) -> bytes:
    return recorder.canonical_json(value) + b"\n"


class Sprint11SurfaceRecorderTests(unittest.TestCase):
    def test_qualifying_command_binds_versioned_package_and_rejects_symlink(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            root = Path(directory)
            package = root / "install" / "versions" / "0.1.0"
            executable = package / "bin" / "godot-codex-mcp"
            executable.parent.mkdir(parents=True)
            executable.write_bytes(b"fixture executable")
            executable.chmod(0o700)
            (package / "VERSION").write_bytes(b"0.1.0\n")
            manifest = b'{"schema_version":"fixture"}\n'
            (package / "package-manifest.json").write_bytes(manifest)
            (package / ".godot-codex-owned").write_text(
                recorder.sha256_bytes(manifest).removeprefix("sha256:") + "\n",
                encoding="ascii",
            )
            project = root / "project"
            project.mkdir()
            (project / "project.godot").write_text(
                "[application]\n",
                encoding="utf-8",
            )
            bound = metadata()
            bound["bindings"]["package_manifest_sha256"] = (
                recorder.sha256_bytes(manifest)
            )
            bound["bindings"]["mcp_binary_sha256"] = recorder.sha256_bytes(
                executable.read_bytes()
            )
            command, descriptor = recorder._qualifying_command(
                [str(executable), "--project-root", str(project)],
                bound,
            )
            os.close(descriptor)
            self.assertEqual(
                command,
                [str(executable), "--project-root", str(project)],
            )
            executable.unlink()
            executable.symlink_to(Path(sys.executable))
            with self.assertRaisesRegex(
                recorder.RecorderError,
                "symlink-free",
            ):
                recorder._qualifying_command(
                    [str(executable), "--project-root", str(project)],
                    bound,
                )

    def test_transport_capture_rejects_arbitrary_command_and_commits_failure(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "journal.json"
            with self.assertRaisesRegex(
                recorder.RecorderError,
                "closed profile",
            ):
                recorder.run_proxy(
                    [sys.executable, "-c", "raise SystemExit(0)"],
                    metadata=metadata(),
                    journal_path=journal,
                    stdin=io.BytesIO(),
                    stdout=io.BytesIO(),
                )
            document = json.loads(journal.read_text(encoding="utf-8"))
            self.assertEqual(
                document["capture_kind"],
                "surface_transport_capture",
            )
            self.assertEqual(document["status"], "incomplete")
            self.assertIn(
                "command_validation_failed",
                document["integrity"]["recorder_errors"],
            )

    def test_synthetic_child_timeout_is_bounded_and_nonqualifying(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "journal.json"
            exit_code = recorder.run_proxy(
                [sys.executable, "-c", "import time; time.sleep(30)"],
                metadata=metadata(),
                journal_path=journal,
                stdin=io.BytesIO(),
                stdout=io.BytesIO(),
                child_timeout=1,
                synthetic_command=True,
            )
            self.assertNotEqual(exit_code, 0)
            document = json.loads(journal.read_text(encoding="utf-8"))
            self.assertEqual(
                document["capture_kind"],
                "synthetic_contract_fixture",
            )
            self.assertEqual(document["status"], "incomplete")
            self.assertIn(
                "child_timeout",
                document["integrity"]["recorder_errors"],
            )

    def test_proxy_preserves_server_bytes_and_records_only_safe_metadata(self) -> None:
        secret = "S11_SECRET_DO_NOT_RECORD_123456"
        request = line(
            {
                "jsonrpc": "2.0",
                "id": f"request-{secret}",
                "method": "tools/call",
                "params": {
                    "name": "godot_get_connection_status",
                    "arguments": {
                        "absolute_project_root": "/Users/private/project",
                        "token": secret,
                    },
                },
            }
        )
        response = line(
            {
                "jsonrpc": "2.0",
                "id": f"request-{secret}",
                "result": {
                    "isError": False,
                    "structuredContent": {
                        "status": "ready",
                        "source_text": secret,
                    },
                    "content": [{"type": "text", "text": secret}],
                },
            }
        )
        child = (
            "import sys;"
            f"sys.stdin.buffer.readline();sys.stdout.buffer.write({response!r});"
            "sys.stdout.buffer.flush()"
        )
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "journal.json"
            output = io.BytesIO()
            exit_code = recorder.run_proxy(
                [sys.executable, "-c", child],
                metadata=metadata(),
                journal_path=journal,
                stdin=io.BytesIO(request),
                stdout=output,
                synthetic_command=True,
            )
            self.assertEqual(exit_code, 0)
            self.assertEqual(output.getvalue(), response)
            document = json.loads(journal.read_text(encoding="utf-8"))
            encoded = recorder.canonical_json(document)
            self.assertNotIn(secret.encode(), encoded)
            self.assertNotIn(b"/Users/", encoded)
            self.assertEqual(document["status"], "complete")
            self.assertEqual(
                document["events"][0]["tool"],
                "godot_get_connection_status",
            )
            self.assertEqual(
                document["events"][1]["semantic_status"],
                "ready",
            )
            self.assertEqual(
                document["integrity"]["event_count"],
                len(document["events"]),
            )
            self.assertEqual(
                document["integrity"]["event_chain_sha256"],
                recorder.event_chain_sha256(document["events"]),
            )

    def test_registry_and_form_events_are_content_minimized(self) -> None:
        instance = recorder.ProtocolRecorder(metadata())
        instance.note_transport(
            recorder.CLIENT_TO_SERVER,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/list",
                    "params": {},
                }
            ),
        )
        instance.note_transport(
            recorder.SERVER_TO_CLIENT,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "tools": [
                            {"name": "godot_get_editor_state"},
                            {"name": "godot_get_connection_status"},
                        ]
                    },
                }
            ),
        )
        instance.note_transport(
            recorder.SERVER_TO_CLIENT,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": "approval-secret-id",
                    "method": "elicitation/create",
                    "params": {
                        "mode": "form",
                        "message": "S11_SECRET_FORM_CONTENT",
                    },
                }
            ),
        )
        instance.note_transport(
            recorder.CLIENT_TO_SERVER,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": "approval-secret-id",
                    "result": {
                        "action": "accept",
                        "content": {"approval": "S11_SECRET_FORM_CONTENT"},
                    },
                }
            ),
        )
        document = instance.document(0, [])
        encoded = recorder.canonical_json(document)
        self.assertEqual(document["status"], "complete")
        self.assertNotIn(b"S11_SECRET_FORM_CONTENT", encoded)
        self.assertEqual(
            document["events"][1]["tools"],
            [
                "godot_get_connection_status",
                "godot_get_editor_state",
            ],
        )
        self.assertEqual(document["events"][3]["form_action"], "accept")
        self.assertFalse(document["events"][3]["content_recorded"])
        self.assertEqual(
            document["integrity"]["event_chain_sha256"],
            recorder.event_chain_sha256(document["events"]),
        )

    def test_child_failure_and_unanswered_form_are_nonqualifying(self) -> None:
        instance = recorder.ProtocolRecorder(metadata())
        instance.note_transport(
            recorder.SERVER_TO_CLIENT,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "elicitation/create",
                    "params": {"mode": "form"},
                }
            ),
        )
        document = instance.document(9, [])
        self.assertEqual(document["status"], "incomplete")
        self.assertEqual(document["integrity"]["child_exit_code"], 9)
        self.assertIn(
            "elicitation_response_missing",
            document["integrity"]["recorder_errors"],
        )

    def test_timeout_closes_only_its_correlated_pending_form(self) -> None:
        instance = recorder.ProtocolRecorder(metadata())
        instance.note_transport(
            recorder.CLIENT_TO_SERVER,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": "apply-1",
                    "method": "tools/call",
                    "params": {
                        "name": "godot_apply_transaction",
                        "arguments": {},
                    },
                }
            ),
        )
        instance.note_transport(
            recorder.SERVER_TO_CLIENT,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": "form-1",
                    "method": "elicitation/create",
                    "params": {"mode": "form", "message": "not recorded"},
                }
            ),
        )
        instance.note_transport(
            recorder.SERVER_TO_CLIENT,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": "apply-1",
                    "result": {
                        "isError": True,
                        "structuredContent": {
                            "code": "approval_timeout",
                            "state": "not_applied",
                        },
                    },
                }
            ),
        )
        document = instance.document(0, [])
        self.assertEqual(document["status"], "complete")
        timeout = document["events"][-1]
        self.assertEqual(timeout["method"], "tools/call")
        self.assertEqual(timeout["tool"], "godot_apply_transaction")
        self.assertEqual(timeout["error_code"], "approval_timeout")
        self.assertEqual(timeout["form_action"], "timeout")
        self.assertFalse(timeout["content_recorded"])

    def test_unrelated_timeout_error_cannot_close_a_pending_form(self) -> None:
        instance = recorder.ProtocolRecorder(metadata())
        instance.note_transport(
            recorder.CLIENT_TO_SERVER,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": "apply-1",
                    "method": "tools/call",
                    "params": {
                        "name": "godot_apply_transaction",
                        "arguments": {},
                    },
                }
            ),
        )
        instance.note_transport(
            recorder.SERVER_TO_CLIENT,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": "form-1",
                    "method": "elicitation/create",
                    "params": {"mode": "form"},
                }
            ),
        )
        instance.note_transport(
            recorder.CLIENT_TO_SERVER,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": "read-1",
                    "method": "tools/call",
                    "params": {
                        "name": "godot_get_connection_status",
                        "arguments": {},
                    },
                }
            ),
        )
        instance.note_transport(
            recorder.SERVER_TO_CLIENT,
            line(
                {
                    "jsonrpc": "2.0",
                    "id": "read-1",
                    "error": {
                        "code": -32000,
                        "data": {"code": "approval_timeout"},
                    },
                }
            ),
        )
        document = instance.document(0, [])
        self.assertEqual(document["status"], "incomplete")
        self.assertNotIn("form_action", document["events"][-1])
        self.assertIn(
            "elicitation_response_missing",
            document["integrity"]["recorder_errors"],
        )

    def test_frame_and_event_bounds_fail_closed(self) -> None:
        instance = recorder.ProtocolRecorder(metadata())
        instance.note_transport(
            recorder.CLIENT_TO_SERVER,
            b"{" + b"x" * recorder.MAX_FRAME_BYTES + b"}\n",
        )
        instance.events = [
            {"seq": index, "kind": "fixture"}
            for index in range(recorder.MAX_EVENTS)
        ]
        instance.note_transport(
            recorder.CLIENT_TO_SERVER,
            line({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        document = instance.document(0, [])
        self.assertEqual(document["status"], "incomplete")
        self.assertTrue(document["integrity"]["truncated"])
        self.assertIn(
            "frame_bound_exceeded",
            document["integrity"]["recorder_errors"],
        )
        self.assertIn(
            "event_bound_exceeded",
            document["integrity"]["recorder_errors"],
        )

    def test_metadata_derives_host_from_bound_provenance_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "metadata.json"
            receipt = host_provenance_receipt()
            receipt_bytes = recorder.canonical_json(receipt)
            (root / recorder.HOST_PROVENANCE_NAME).write_bytes(receipt_bytes)
            path.write_bytes(
                recorder.canonical_json(
                    raw_metadata(recorder.sha256_bytes(receipt_bytes))
                )
            )
            loaded = recorder.load_metadata(path)
            self.assertEqual(loaded["surface"], "cli")
            self.assertEqual(
                loaded["host"]["identifier"],
                "codex-cli",
            )
            self.assertEqual(
                loaded["bindings"]["host_provenance_sha256"],
                recorder.sha256_bytes(receipt_bytes),
            )

            changed = raw_metadata(recorder.sha256_bytes(receipt_bytes))
            changed["host"] = {"build": "/Users/private/build"}
            path.write_bytes(recorder.canonical_json(changed))
            with self.assertRaises(recorder.RecorderError):
                recorder.load_metadata(path)

    def test_tampered_host_provenance_receipt_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "metadata.json"
            receipt = host_provenance_receipt()
            receipt_bytes = recorder.canonical_json(receipt)
            expected_digest = recorder.sha256_bytes(receipt_bytes)
            (root / recorder.HOST_PROVENANCE_NAME).write_bytes(
                receipt_bytes + b" "
            )
            path.write_bytes(
                recorder.canonical_json(raw_metadata(expected_digest))
            )
            with self.assertRaisesRegex(
                recorder.RecorderError,
                "digest differs",
            ):
                recorder.load_metadata(path)

    def test_wrapped_server_crash_cannot_wait_forever_for_client_input(self) -> None:
        release = threading.Event()

        class BlockingInput(io.BytesIO):
            def readline(self, _size: int = -1) -> bytes:
                release.wait(10)
                return b""

        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "journal.json"
            try:
                exit_code = recorder.run_proxy(
                    [sys.executable, "-c", "raise SystemExit(17)"],
                    metadata=metadata(),
                    journal_path=journal,
                    stdin=BlockingInput(),
                    stdout=io.BytesIO(),
                    synthetic_command=True,
                )
            finally:
                release.set()
            self.assertEqual(exit_code, 17)
            document = json.loads(journal.read_text(encoding="utf-8"))
            self.assertEqual(document["status"], "incomplete")
            self.assertIn(
                "client_input_not_closed",
                document["integrity"]["recorder_errors"],
            )


if __name__ == "__main__":
    unittest.main()
