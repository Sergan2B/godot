from __future__ import annotations

import shutil
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tests.codex import sprint11_acquisition_paths as paths


class Sprint11AcquisitionPathTests(unittest.TestCase):
    def test_repository_directory_is_private_and_atomically_published(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory).resolve()
            destination = paths.prepare_repository_directory(
                repository / "evidence",
                repository=repository,
                prefix=".stage.",
            )
            self.assertEqual(destination.staging.stat().st_mode & 0o777, 0o700)
            (destination.staging / "receipt.json").write_bytes(b"{}\n")
            destination.publish()
            self.assertEqual(
                (destination.target / "receipt.json").read_bytes(),
                b"{}\n",
            )

    def test_repository_symlink_component_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory).resolve()
            real_parent = repository / "real"
            real_parent.mkdir()
            (repository / "alias").symlink_to(real_parent, target_is_directory=True)
            with self.assertRaises(paths.AcquisitionPathError):
                paths.prepare_repository_directory(
                    repository / "alias" / "evidence",
                    repository=repository,
                    prefix=".stage.",
                )

    def test_directory_publication_never_overwrites_racing_target(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory).resolve()
            destination = paths.prepare_repository_directory(
                repository / "evidence",
                repository=repository,
                prefix=".stage.",
            )
            destination.target.mkdir()
            foreign = destination.target / "foreign"
            foreign.write_bytes(b"foreign")
            try:
                with self.assertRaises(paths.AcquisitionPathError):
                    destination.publish()
                self.assertEqual(foreign.read_bytes(), b"foreign")
            finally:
                shutil.rmtree(destination.staging, ignore_errors=True)

    def test_file_publication_never_overwrites_racing_target(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory).resolve() / "trace.json"
            staged = paths.prepare_staged_file(target, prefix=".stage.")
            staged.staged_path.write_bytes(b"new")
            target.write_bytes(b"foreign")
            try:
                with self.assertRaises(paths.AcquisitionPathError):
                    staged.publish()
                self.assertEqual(target.read_bytes(), b"foreign")
            finally:
                shutil.rmtree(staged.staging_directory, ignore_errors=True)

    def test_staged_file_rechecks_no_symlink_before_publication(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            staged = paths.prepare_staged_file(
                root / "trace.json",
                prefix=".stage.",
            )
            foreign = root / "foreign.json"
            foreign.write_bytes(b"foreign")
            staged.staged_path.symlink_to(foreign)
            try:
                with self.assertRaises(paths.AcquisitionPathError):
                    staged.publish()
                self.assertFalse(staged.target.exists())
            finally:
                shutil.rmtree(staged.staging_directory, ignore_errors=True)

    def test_regular_file_swap_during_read_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root / "artifact"
            displaced = root / "displaced"
            source.write_bytes(b"trusted")
            original_read = paths.os.read
            swapped = False

            def swap_after_read(descriptor: int, size: int) -> bytes:
                nonlocal swapped
                value = original_read(descriptor, size)
                if value and not swapped:
                    swapped = True
                    source.rename(displaced)
                    source.write_bytes(b"foreign")
                return value

            with mock.patch.object(paths.os, "read", side_effect=swap_after_read):
                with self.assertRaises(paths.AcquisitionPathError):
                    paths.read_regular_file(source, maximum=64)

    def test_regular_file_change_during_hash_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory).resolve() / "artifact"
            source.write_bytes(b"trusted")
            original_read = paths.os.read
            changed = False

            def change_after_read(descriptor: int, size: int) -> bytes:
                nonlocal changed
                value = original_read(descriptor, size)
                if value and not changed:
                    changed = True
                    with source.open("r+b") as stream:
                        stream.write(b"T")
                        stream.flush()
                return value

            with mock.patch.object(paths.os, "read", side_effect=change_after_read):
                with self.assertRaises(paths.AcquisitionPathError):
                    paths.sha256_regular_file(source, maximum=64)


if __name__ == "__main__":
    unittest.main()
