from __future__ import annotations

import contextlib
import copy
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tests.codex import sprint11_host_provenance as provenance


def digest(character: str) -> str:
    return "sha256:" + character * 64


def signature(
    *,
    identifier: str = "com.example.host",
    team_id: str = "TEAM123456",
    cdhash: str = "a" * 40,
    mode: str = "strict",
    verified: bool = False,
) -> dict[str, object]:
    value: dict[str, object] = {
        "mode": mode,
        "identifier": identifier,
        "team_id": team_id,
        "cdhash": cdhash,
    }
    if verified:
        value["verified"] = True
    return value


def profile() -> dict[str, object]:
    app_host = signature(
        identifier="com.example.app",
        mode="deep_strict",
    )
    client = signature(identifier="client")
    shell = signature(
        identifier="com.example.ide",
        team_id="IDE1234567",
        cdhash="b" * 40,
        mode="deep_strict",
    )
    return {
        "schema_version": "godot-codex-host-coordinate-profile/1.0",
        "profile_id": "fixture",
        "surfaces": [
            {
                "surface": "app",
                "host_artifact_sha256": digest("1"),
                "host_metadata_sha256": None,
                "client_artifact_sha256": digest("2"),
                "ide_shell_artifact_sha256": None,
                "host_code_signature": app_host,
                "client_code_signature": client,
                "ide_shell_code_signature": None,
            },
            {
                "surface": "cli",
                "host_artifact_sha256": digest("2"),
                "host_metadata_sha256": None,
                "client_artifact_sha256": digest("2"),
                "ide_shell_artifact_sha256": None,
                "host_code_signature": client,
                "client_code_signature": client,
                "ide_shell_code_signature": None,
            },
            {
                "surface": "ide",
                "host_artifact_sha256": digest("3"),
                "host_metadata_sha256": digest("4"),
                "client_artifact_sha256": digest("5"),
                "ide_shell_artifact_sha256": digest("6"),
                "host_code_signature": None,
                "client_code_signature": client,
                "ide_shell_code_signature": shell,
            },
        ],
    }


def measurements() -> list[dict[str, object]]:
    app_host = signature(
        identifier="com.example.app",
        mode="deep_strict",
        verified=True,
    )
    client = signature(identifier="client", verified=True)
    shell = signature(
        identifier="com.example.ide",
        team_id="IDE1234567",
        cdhash="b" * 40,
        mode="deep_strict",
        verified=True,
    )
    return [
        {
            "surface": "app",
            "host_artifact_sha256": digest("1"),
            "host_metadata_sha256": None,
            "client_artifact_sha256": digest("2"),
            "ide_shell_artifact_sha256": None,
            "host_code_signature": app_host,
            "client_code_signature": client,
            "ide_shell_code_signature": None,
        },
        {
            "surface": "cli",
            "host_artifact_sha256": digest("2"),
            "host_metadata_sha256": None,
            "client_artifact_sha256": digest("2"),
            "ide_shell_artifact_sha256": None,
            "host_code_signature": client,
            "client_code_signature": client,
            "ide_shell_code_signature": None,
        },
        {
            "surface": "ide",
            "host_artifact_sha256": digest("3"),
            "host_metadata_sha256": digest("4"),
            "client_artifact_sha256": digest("5"),
            "ide_shell_artifact_sha256": digest("6"),
            "host_code_signature": None,
            "client_code_signature": client,
            "ide_shell_code_signature": shell,
        },
    ]


class HostProvenanceTests(unittest.TestCase):
    def test_command_environment_preserves_only_valid_scope_markers(
        self,
    ) -> None:
        marker = "GODOT_CODEX_PROCESS_SCOPE_" + "a" * 48
        with mock.patch.dict(
            os.environ,
            {
                marker: "1",
                "SPRINT11_SENTINEL_SECRET": "secret",
            },
            clear=False,
        ):
            environment = provenance.command_environment()
        self.assertEqual(environment[marker], "1")
        self.assertNotIn("SPRINT11_SENTINEL_SECRET", environment)

        with (
            mock.patch.dict(
                os.environ,
                {"GODOT_CODEX_PROCESS_SCOPE_invalid": "1"},
                clear=False,
            ),
            self.assertRaisesRegex(
                provenance.HostProvenanceError,
                "process scope environment differs",
            ),
        ):
            provenance.command_environment()

    def test_tree_digest_is_stable_and_content_mode_and_empty_directory_bound(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / "bin").mkdir()
            (root / "empty").mkdir()
            executable = root / "bin/client"
            executable.write_bytes(b"one")
            executable.chmod(0o755)
            first = provenance.stable_tree_digest(root)
            self.assertEqual(first.file_count, 1)
            self.assertEqual(first.directory_count, 2)
            self.assertEqual(first.total_bytes, 3)

            executable.write_bytes(b"two")
            content = provenance.stable_tree_digest(root)
            self.assertNotEqual(content.sha256, first.sha256)

            executable.chmod(0o700)
            mode = provenance.stable_tree_digest(root)
            self.assertNotEqual(mode.sha256, content.sha256)

            (root / "empty").rmdir()
            structure = provenance.stable_tree_digest(root)
            self.assertNotEqual(structure.sha256, mode.sha256)

    def test_tree_rejects_symlink_special_file_and_unsafe_permissions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            target = root / "target"
            target.write_bytes(b"value")
            link = root / "link"
            link.symlink_to(target)
            with self.assertRaisesRegex(
                provenance.HostProvenanceError,
                "symlink or special file",
            ):
                provenance.stable_tree_digest(root)
            link.unlink()

            fifo = root / "pipe"
            os.mkfifo(fifo)
            with self.assertRaisesRegex(
                provenance.HostProvenanceError,
                "symlink or special file",
            ):
                provenance.stable_tree_digest(root)
            fifo.unlink()

            target.chmod(0o666)
            with self.assertRaisesRegex(
                provenance.HostProvenanceError,
                "ownership or permissions",
            ):
                provenance.stable_tree_digest(root)

    def test_codesign_coordinate_is_closed_and_ambiguous_values_fail(self) -> None:
        output = (
            b"Executable=/private/path/ignored\n"
            b"Identifier=com.example.host\n"
            b"CDHash=0123456789abcdef0123456789abcdef01234567\n"
            b"TeamIdentifier=TEAM123456\n"
        )
        self.assertEqual(
            provenance.parse_codesign_display(output, mode="deep_strict"),
            {
                "verified": True,
                "mode": "deep_strict",
                "identifier": "com.example.host",
                "team_id": "TEAM123456",
                "cdhash": "0123456789abcdef0123456789abcdef01234567",
            },
        )
        with self.assertRaisesRegex(
            provenance.HostProvenanceError,
            "incomplete or ambiguous",
        ):
            provenance.parse_codesign_display(
                output + b"Identifier=other\n",
                mode="strict",
            )
        with self.assertRaisesRegex(
            provenance.HostProvenanceError,
            "unsafe value",
        ):
            provenance.parse_codesign_display(
                output.replace(b"TEAM123456", b"team identity with spaces"),
                mode="strict",
            )

    def test_measurements_must_match_every_profile_coordinate(self) -> None:
        expected = profile()
        actual = measurements()
        provenance.validate_measurements_against_profile(expected, actual)

        for surface, field in [
            ("app", "host_artifact_sha256"),
            ("cli", "client_code_signature"),
            ("ide", "host_metadata_sha256"),
            ("ide", "ide_shell_code_signature"),
        ]:
            changed = copy.deepcopy(actual)
            record = next(item for item in changed if item["surface"] == surface)
            if "signature" in field:
                assert isinstance(record[field], dict)
                record[field]["cdhash"] = "f" * 40
            else:
                record[field] = digest("f")
            with self.assertRaises(provenance.HostProvenanceError):
                provenance.validate_measurements_against_profile(
                    expected,
                    changed,
                )

    def test_profile_reader_binds_exact_bytes_and_rejects_duplicate_json(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            path = root / "profile.json"
            payload = json.dumps(profile(), sort_keys=True).encode()
            path.write_bytes(payload)
            document, observed_digest = provenance._profile(path)
            self.assertEqual(document, profile())
            self.assertEqual(observed_digest, provenance.sha256_bytes(payload))

            path.write_text('{"schema_version":"a","schema_version":"b"}')
            with self.assertRaisesRegex(
                provenance.HostProvenanceError,
                "duplicate member",
            ):
                provenance._profile(path)

    def test_code_signature_probe_verifies_before_reading_coordinate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            executable = root / "client"
            executable.write_bytes(b"client")
            executable.chmod(0o755)
            calls: list[list[str]] = []

            def runner(
                argv: list[str] | tuple[str, ...],
                _timeout: float,
            ) -> provenance.CommandResult:
                calls.append(list(argv))
                if "--verify" in argv:
                    return provenance.CommandResult(0, b"", b"valid\n")
                return provenance.CommandResult(
                    0,
                    b"",
                    (
                        b"Identifier=client\n"
                        b"TeamIdentifier=TEAM123456\n"
                        b"CDHash=" + b"a" * 40 + b"\n"
                    ),
                )

            observed = provenance.measure_code_signature(
                executable,
                deep=False,
                runner=runner,
            )
            self.assertTrue(observed["verified"])
            self.assertEqual(len(calls), 2)
            self.assertIn("--strict", calls[0])
            self.assertNotIn("--deep", calls[0])

            def invalid_runner(
                _argv: list[str] | tuple[str, ...],
                _timeout: float,
            ) -> provenance.CommandResult:
                return provenance.CommandResult(1, b"", b"invalid")

            with self.assertRaisesRegex(
                provenance.HostProvenanceError,
                "signature is invalid",
            ):
                provenance.measure_code_signature(
                    executable,
                    deep=False,
                    runner=invalid_runner,
                )

    def test_cli_output_uses_exclusive_atomic_publication(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "receipt.json"
            fake_receipt = {
                "schema_version": provenance.SCHEMA_VERSION,
                "status": "passed",
            }
            arguments = [
                "--profile",
                str(root / "profile.json"),
                "--app-bundle",
                str(root / "app"),
                "--app-executable",
                str(root / "app-bin"),
                "--app-client",
                str(root / "client"),
                "--vscode-bundle",
                str(root / "vscode"),
                "--vscode-executable",
                str(root / "vscode-bin"),
                "--extension-root",
                str(root / "extension"),
                "--extension-package-json",
                str(root / "extension/package.json"),
                "--ide-client",
                str(root / "ide-client"),
                "--output",
                str(output),
            ]
            with mock.patch.object(
                provenance,
                "acquire_host_provenance",
                return_value=fake_receipt,
            ):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(provenance.main(arguments), 0)
            self.assertEqual(
                json.loads(output.read_text()),
                fake_receipt,
            )
            with mock.patch.object(
                provenance,
                "acquire_host_provenance",
                return_value=fake_receipt,
            ):
                with self.assertRaises(provenance.HostProvenanceError):
                    provenance.main(arguments)

    @unittest.skipUnless(sys.platform == "darwin", "macOS mode check")
    def test_file_digest_rejects_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "target"
            target.write_bytes(b"x")
            link = root / "link"
            link.symlink_to(target)
            with self.assertRaisesRegex(
                provenance.HostProvenanceError,
                "symlink-free",
            ):
                provenance._safe_file_digest(link, label="fixture")


if __name__ == "__main__":
    unittest.main()
