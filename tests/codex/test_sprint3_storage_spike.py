#!/usr/bin/env python3
"""Regression tests for the version-resilient Sprint 3 storage launcher."""

from __future__ import annotations

import sys
import tomllib
import unittest
from pathlib import Path

SCRIPT_DIRECTORY = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIRECTORY))

import sprint3_storage_spike as launcher  # noqa: E402


class Sprint3StorageSpikeTests(unittest.TestCase):
    def test_refreshes_only_exact_local_index_store_version(self) -> None:
        stale = b'''version = 4

[[package]]
name = "godot-codex-index-store"
version = "0.1.0"
dependencies = ["serde"]
'''
        refreshed = launcher.rewrite_path_package_version(
            stale,
            "godot-codex-index-store",
            "0.1.19",
        )
        self.assertIn(
            b'name = "godot-codex-index-store"\nversion = "0.1.19"',
            refreshed,
        )
        self.assertEqual(refreshed.count(b"0.1.19"), 1)

    def test_rejects_registry_backed_index_store_entry(self) -> None:
        registry = b'''version = 4

[[package]]
name = "godot-codex-index-store"
version = "0.1.0"
source = "registry+https://example.invalid"
'''
        with self.assertRaisesRegex(
            launcher.StorageSpikeLauncherError,
            "local path package",
        ):
            launcher.rewrite_path_package_version(
                registry,
                "godot-codex-index-store",
                "0.1.19",
            )

    def test_rejects_duplicate_index_store_entries(self) -> None:
        duplicate = b'''version = 4

[[package]]
name = "godot-codex-index-store"
version = "0.1.0"

[[package]]
name = "godot-codex-index-store"
version = "0.1.0"
'''
        with self.assertRaisesRegex(
            launcher.StorageSpikeLauncherError,
            "exactly once",
        ):
            launcher.rewrite_path_package_version(
                duplicate,
                "godot-codex-index-store",
                "0.1.19",
            )

    def test_rejects_malformed_version_and_lock(self) -> None:
        local = b'''version = 4

[[package]]
name = "godot-codex-index-store"
version = "0.1.0"
'''
        with self.assertRaisesRegex(
            launcher.StorageSpikeLauncherError,
            "SemVer",
        ):
            launcher.rewrite_path_package_version(
                local,
                "godot-codex-index-store",
                "next",
            )
        with self.assertRaisesRegex(
            launcher.StorageSpikeLauncherError,
            "lock is invalid",
        ):
            launcher.rewrite_path_package_version(
                b"not valid TOML = [",
                "godot-codex-index-store",
                "0.1.19",
            )

    def test_preserves_unrelated_package_material(self) -> None:
        local = b'''version = 4

[[package]]
name = "godot-codex-index-store"
version = "0.1.0"

[[package]]
name = "serde"
version = "1.0.228"
source = "registry+https://github.com/rust-lang/crates.io-index"
'''
        refreshed = tomllib.loads(
            launcher.rewrite_path_package_version(
                local,
                "godot-codex-index-store",
                "0.1.19",
            ).decode("utf-8")
        )
        serde = next(
            item for item in refreshed["package"] if item["name"] == "serde"
        )
        self.assertEqual(serde["version"], "1.0.228")
        self.assertIn("source", serde)


if __name__ == "__main__":
    unittest.main()
