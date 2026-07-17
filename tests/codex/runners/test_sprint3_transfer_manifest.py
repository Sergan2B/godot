"""Tests for Sprint 3 host-transfer receipts."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

RUNNERS_DIRECTORY = Path(__file__).resolve().parent
sys.path.insert(0, str(RUNNERS_DIRECTORY))

import sprint3_transfer_manifest as manifest  # noqa: E402


class TransferManifestTests(unittest.TestCase):
    def create_file(self, directory: Path, name: str, content: bytes) -> Path:
        path = directory / name
        path.write_bytes(content)
        return path

    def linux_fixture(self, directory: Path) -> tuple[Path, Path, Path]:
        runner = self.create_file(directory, manifest.PLATFORM_RUNNERS["linux-x86_64"], b"runner\n")
        report = self.create_file(directory, manifest.PLATFORM_FILES["linux-x86_64"][0], b"report\n")
        receipt = directory / manifest.PLATFORM_RECEIPTS["linux-x86_64"]
        return runner, report, receipt

    def test_linux_receipt_round_trip(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            directory = Path(temporary_directory)
            runner, report, receipt = self.linux_fixture(directory)
            created = manifest.create_receipt("linux-x86_64", runner, receipt, [report])
            verified = manifest.verify_receipt("linux-x86_64", runner, receipt, [report])
            self.assertEqual(created, verified)
            self.assertEqual(created["files"][0]["bytes"], len(b"report\n"))

    def test_windows_receipt_has_canonical_file_order(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            directory = Path(temporary_directory)
            runner = self.create_file(directory, manifest.PLATFORM_RUNNERS["windows-x86_64"], b"runner\r\n")
            live = self.create_file(directory, manifest.PLATFORM_FILES["windows-x86_64"][0], b"live\n")
            storage = self.create_file(directory, manifest.PLATFORM_FILES["windows-x86_64"][1], b"storage\n")
            receipt = directory / manifest.PLATFORM_RECEIPTS["windows-x86_64"]
            created = manifest.create_receipt("windows-x86_64", runner, receipt, [storage, live])
            self.assertEqual(
                [record["name"] for record in created["files"]],
                list(manifest.PLATFORM_FILES["windows-x86_64"]),
            )
            manifest.verify_receipt("windows-x86_64", runner, receipt, [live, storage])

    def test_mutated_report_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            directory = Path(temporary_directory)
            runner, report, receipt = self.linux_fixture(directory)
            manifest.create_receipt("linux-x86_64", runner, receipt, [report])
            report.write_bytes(b"tamper\n")
            with self.assertRaisesRegex(manifest.ManifestError, "SHA-256 differs"):
                manifest.verify_receipt("linux-x86_64", runner, receipt, [report])

    def test_mutated_runner_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            directory = Path(temporary_directory)
            runner, report, receipt = self.linux_fixture(directory)
            manifest.create_receipt("linux-x86_64", runner, receipt, [report])
            runner.write_bytes(b"changed runner\n")
            with self.assertRaisesRegex(manifest.ManifestError, "runner identity differs"):
                manifest.verify_receipt("linux-x86_64", runner, receipt, [report])

    def test_unknown_receipt_field_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            directory = Path(temporary_directory)
            runner, report, receipt = self.linux_fixture(directory)
            created = manifest.create_receipt("linux-x86_64", runner, receipt, [report])
            created["unexpected"] = True
            receipt.write_text(json.dumps(created), encoding="utf-8")
            with self.assertRaisesRegex(manifest.ManifestError, "fields differ"):
                manifest.verify_receipt("linux-x86_64", runner, receipt, [report])


if __name__ == "__main__":
    unittest.main()
