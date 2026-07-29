from __future__ import annotations

import argparse
import contextlib
import copy
import hashlib
import importlib
import io
import json
import os
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

from tests.codex import sprint11_acceptance as acceptance
from tests.codex import sprint11_packaged_fixture as packaged_fixture
from tests.codex import sprint11_packaged_regressions as packaged


def installable_package_fixture(root: Path) -> tuple[Path, Path]:
    artifact = root / "artifact"
    artifact.mkdir()
    package_name = "godot-codex-0.1.0-macos-arm64"
    source_commit = "a" * 40
    provenance = {
        "cargo_lock_sha256": "sha256:" + "1" * 64,
        "cargo_version": "cargo fixture",
        "fresh_target": True,
        "rust_toolchain_sha256": "sha256:" + "2" * 64,
        "rustc_commit": "b" * 40,
        "rustc_release": "fixture",
        "target_triple": "aarch64-apple-darwin",
    }
    prerequisite = {
        "version": "4.8.dev.codex.fixture",
        "commit": "c" * 40,
        "sha256": "sha256:" + "3" * 64,
        "architecture": "arm64",
        "expected_install_path": "/Applications/Godot.app",
        "verification": "fixture",
    }
    files: dict[str, tuple[bytes, int]] = {
        "SOURCE_COMMIT": ((source_commit + "\n").encode(), 0o644),
        "VERSION": (b"0.1.0\n", 0o644),
        "bin/godot-codex": (b"#!/bin/sh\nexit 0\n", 0o755),
        "bin/godot-codex-mcp": (b"#!/bin/sh\nexit 0\n", 0o755),
        "install.sh": (
            (
                packaged.REPOSITORY_ROOT
                / "godot-codex-mcp/packaging/install.sh"
            ).read_bytes(),
            0o755,
        ),
        "share/godot-codex/licenses/Godot-LICENSE.txt": (
            b"Godot license fixture\n",
            0o644,
        ),
        "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt": (
            b"Third-party license fixture\n",
            0o644,
        ),
    }
    internal_contents = [
        {
            "bytes": len(payload),
            "mode": f"{mode:04o}",
            "path": relative,
            "sha256": packaged.sha256_bytes(payload),
        }
        for relative, (payload, mode) in sorted(files.items())
    ]
    internal_manifest = {
        "schema_version": packaged.INTERNAL_PACKAGE_MANIFEST_SCHEMA,
        "package_version": "0.1.0",
        "source_commit": source_commit,
        "target": {"architecture": "arm64", "os": "macos"},
        "build_provenance": provenance,
        "godot_prerequisite": prerequisite,
        "compatibility_matrix_sha256": "sha256:" + "4" * 64,
        "registry_sha256": "sha256:" + "5" * 64,
        "third_party_licenses_sha256": packaged.sha256_bytes(
            files[
                "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt"
            ][0]
        ),
        "contents": internal_contents,
        "checksums_path": "checksums.sha256",
    }
    files["package-manifest.json"] = (
        packaged.canonical_json(internal_manifest) + b"\n",
        0o644,
    )
    checksum_lines = [
        f"{hashlib.sha256(payload).hexdigest()}  {relative}\n"
        for relative, (payload, _mode) in sorted(files.items())
    ]
    files["checksums.sha256"] = (
        "".join(checksum_lines).encode("utf-8"),
        0o644,
    )
    archive = artifact / f"{package_name}.tar.gz"
    directories = {
        "/".join(Path(relative).parts[:index])
        for relative in files
        for index in range(1, len(Path(relative).parts))
    }
    with tarfile.open(archive, mode="w:gz", format=tarfile.PAX_FORMAT) as output:
        for relative in ("", *sorted(directories, key=lambda item: (item.count("/"), item))):
            name = package_name + (f"/{relative}" if relative else "") + "/"
            info = tarfile.TarInfo(name)
            info.type = tarfile.DIRTYPE
            info.mode = 0o755
            info.uid = 0
            info.gid = 0
            info.uname = "root"
            info.gname = "root"
            info.mtime = 0
            output.addfile(info)
        for relative, (payload, mode) in sorted(files.items()):
            info = tarfile.TarInfo(f"{package_name}/{relative}")
            info.size = len(payload)
            info.mode = mode
            info.uid = 0
            info.gid = 0
            info.uname = "root"
            info.gname = "root"
            info.mtime = 0
            output.addfile(info, io.BytesIO(payload))
    contents = [
        {
            "bytes": len(payload),
            "mode": f"{mode:04o}",
            "path": relative,
            "sha256": packaged.sha256_bytes(payload),
        }
        for relative, (payload, mode) in sorted(files.items())
    ]
    detached = {
        "schema_version": packaged.PACKAGE_MANIFEST_SCHEMA,
        "package_version": "0.1.0",
        "source_commit": source_commit,
        "build_provenance": provenance,
        "archive": {
            "path": archive.name,
            "sha256": packaged.sha256_file(archive),
            "bytes": archive.stat().st_size,
        },
        "compatibility_matrix_sha256": internal_manifest[
            "compatibility_matrix_sha256"
        ],
        "registry_sha256": internal_manifest["registry_sha256"],
        "third_party_licenses_sha256": internal_manifest[
            "third_party_licenses_sha256"
        ],
        "godot_prerequisite": prerequisite,
        "contents": contents,
    }
    manifest = artifact / "sprint11-package-manifest.json"
    manifest.write_bytes(packaged.canonical_json(detached) + b"\n")
    return artifact, manifest


def synthetic_report(
    command_id: str,
    *,
    godot_sha256: str,
    sidecar_sha256: str,
) -> dict[str, Any]:
    artifacts = {
        "godot_sha256": godot_sha256,
        "sidecar_sha256": sidecar_sha256,
    }
    if command_id == "s6_semantic":
        return {
            "schema_version": 1,
            "sprint": 6,
            "profile": "model_free_live_smoke",
            "status": "passed",
            "artifacts": artifacts,
            "mcp_contract": {
                "exact_ten_tool_registry": True,
                "closed_input_schemas": True,
                "read_only_annotations": True,
            },
            "cleanup": {"processes_stopped": True},
            "redaction": {"secrets_absent": True},
        }
    if command_id == "s7_editor":
        return {
            "schema_version": 1,
            "sprint": 7,
            "profile": "model_free_live_editor",
            "status": "passed",
            "artifacts": artifacts,
            "checks": {"live_overlay": True, "restart": True},
            "cleanup": {"processes_stopped": True},
        }
    if command_id == "s8_runtime":
        return {
            "schema_version": 1,
            "sprint": 8,
            "status": "passed",
            "headless": True,
            "artifacts": artifacts,
            "checks": {
                "runtime_tree_and_opaque_ids": True,
                "bounded_properties": True,
                "diagnostic_stack": True,
                "fresh_session_per_run": True,
                "hang_timeout_and_recovery": True,
            },
            "cleanup": {"processes_stopped": True},
            "redaction": {"secrets_absent": True},
        }
    if command_id.startswith("s9_"):
        operations: list[dict[str, Any]] = []
        negatives: dict[str, Any] = {}
        faults: dict[str, Any] = {}
        if command_id == "s9_operations_a":
            names = {
                "attach_script",
                "connect_signal",
                "create_node",
                "delete_node",
            }
            operations = [
                {
                    "operation": name,
                    "oracle_cycle": True,
                    "source_unchanged": True,
                    "transaction_native_actions": 1,
                }
                for name in sorted(names)
            ]
        elif command_id == "s9_operations_b":
            names = {
                "detach_script",
                "disconnect_signal",
                "reparent_node",
                "set_property",
            }
            operations = [
                {
                    "operation": name,
                    "oracle_cycle": True,
                    "source_unchanged": True,
                    "transaction_native_actions": 1,
                }
                for name in sorted(names)
            ]
        elif command_id == "s9_negatives":
            negatives = {
                "idempotency": {},
                "committed_replay": {},
                "expired_preview": {},
                "approval": [
                    (
                        {
                            "decision": "unsupported",
                            "elicitation_count": 0,
                            "error": "approval_host_unsupported",
                            "native_actions": 0,
                            "source_unchanged": True,
                        }
                        if decision == "unsupported"
                        else {"decision": decision}
                    )
                    for decision in (
                        "confirm_false",
                        "decline",
                        "cancel",
                        "unsupported",
                    )
                ],
            }
        elif command_id == "s9_approval_timeout":
            negatives = {
                "approval": [{"decision": "timeout"}],
            }
        elif command_id == "s9_faults_a":
            faults = {
                name: {}
                for name in (
                    "sidecar_disconnect_prepared",
                    "bridge_response_loss_after_commit",
                    "disconnect_before_commit",
                )
            }
        elif command_id == "s9_faults_b":
            faults = {
                name: {}
                for name in (
                    "editor_crash_before_commit",
                    "editor_restart_after_commit",
                    "corrupt_journal",
                    "restart_after_bridge_response_before_journal_ack",
                )
            }
        return {
            "schema_version": "s9-model-free-workflow/1.0",
            "status": "passed",
            "platform": "macos-arm64",
            "protocol": "2025-11-25",
            "tool_registry": 41,
            "artifacts": artifacts,
            "operations": operations,
            "negatives": negatives,
            "faults": faults,
            "source_unchanged": True,
            "cleanup": True,
            "redaction": True,
        }
    if command_id == "s10_compound":
        return {
            "schema_version": "s10-model-free-live/1.0",
            "status": "passed",
            "platform": "macos-arm64",
            "protocol": "2025-11-25",
            "bridge_rpc": "1.8",
            "tool_registry": 41,
            "artifacts": artifacts,
            "operation_count": 2,
            "one_native_action": True,
            "prepare_read_only": True,
            "exact_undo": True,
            "validation": {
                "outcome": "passed",
                "checks": {"intrinsic": "passed"},
            },
            "cleanup": {"processes_stopped": True},
        }
    if command_id == "s11_same_project":
        return {
            "schema_version": "s11-same-project-live/1.1",
            "status": "passed",
            "platform": "macos-arm64",
            "protocol": "2025-11-25",
            "bridge_rpc": "1.8",
            "package_version": "0.1.1",
            "artifacts": artifacts,
            "registry": {
                "tools": 41,
                "digest": "sha256:" + "6" * 64,
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
    raise AssertionError(command_id)


class FakeRepository:
    def __init__(self, blobs: dict[tuple[str, str], bytes]) -> None:
        self.blobs = blobs

    def blob_at(self, commit: str, relative: str) -> bytes:
        return self.blobs[(commit, relative)]


class Sprint11PackagedRegressionTests(unittest.TestCase):
    def test_command_matrix_is_exact_bounded_and_package_scoped(self) -> None:
        specs = packaged.canonical_command_specs()
        self.assertEqual(len(specs), 11)
        self.assertEqual(len({item.command_id for item in specs}), 11)
        for spec in specs:
            template = spec.argv_template()
            self.assertEqual(
                template[1:4],
                packaged.ISOLATED_PYTHON_FLAGS,
            )
            self.assertIn("{package_sidecar}", template)
            self.assertIn("{package_operations}", template)
            self.assertIn("{package_data_root}", template)
            self.assertIn("{godot}", template)
            self.assertIn("{timeout}", template)
            self.assertIn("--additive-sprint11-registry", template)
            self.assertNotIn("target/release/godot-codex-mcp", " ".join(template))
        s9 = [item for item in specs if item.command_id.startswith("s9_")]
        self.assertEqual(len(s9), 6)
        self.assertTrue(
            all("--additive-sprint10-registry" in item.arguments for item in s9)
        )
        negatives = next(
            item for item in specs if item.command_id == "s9_negatives"
        )
        approval_timeout = next(
            item for item in specs if item.command_id == "s9_approval_timeout"
        )
        self.assertIn("--skip-approval-timeout", negatives.arguments)
        self.assertIn("--only-approval-timeout", approval_timeout.arguments)

    def test_python_runner_flags_ignore_ambient_site_customization(self) -> None:
        with tempfile.TemporaryDirectory(prefix="s11-python-isolation.") as temporary:
            root = Path(temporary)
            attacker = root / "attacker"
            attacker.mkdir()
            marker = root / "sitecustomize-ran"
            (attacker / "sitecustomize.py").write_text(
                f"from pathlib import Path\nPath({str(marker)!r}).write_text('ran')\n",
                encoding="utf-8",
            )
            runner = root / "runner.py"
            runner.write_text("print('isolated')\n", encoding="utf-8")
            environment = dict(os.environ)
            environment["PYTHONPATH"] = str(attacker)
            result = subprocess.run(
                [
                    sys.executable,
                    *packaged.ISOLATED_PYTHON_FLAGS,
                    str(runner),
                ],
                check=False,
                capture_output=True,
                env=environment,
                timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual(result.stdout, b"isolated\n")
            self.assertFalse(marker.exists())

    def test_read_only_source_fixture_becomes_private_and_writable(self) -> None:
        with tempfile.TemporaryDirectory(prefix="s11-fixture-copy.") as temporary:
            root = Path(temporary)
            source = root / "source"
            nested = source / "nested"
            nested.mkdir(parents=True)
            regular = nested / "project.godot"
            executable = source / "tool.sh"
            regular.write_text("[application]\n", encoding="utf-8")
            executable.write_text("#!/bin/sh\n", encoding="utf-8")
            executable.chmod(0o500)
            regular.chmod(0o400)
            nested.chmod(0o500)
            source.chmod(0o500)
            try:
                destination = root / "destination"
                packaged_fixture.copy_project_fixture(source, destination)
                self.assertEqual(
                    stat.S_IMODE(destination.stat().st_mode),
                    0o700,
                )
                self.assertEqual(
                    stat.S_IMODE((destination / "nested").stat().st_mode),
                    0o700,
                )
                self.assertEqual(
                    stat.S_IMODE(
                        (destination / "nested/project.godot").stat().st_mode
                    ),
                    0o600,
                )
                self.assertEqual(
                    stat.S_IMODE((destination / "tool.sh").stat().st_mode),
                    0o700,
                )
                (destination / "nested/project.godot").write_text(
                    "changed\n",
                    encoding="utf-8",
                )
                (destination / ".codex").mkdir()
                self.assertEqual(
                    stat.S_IMODE(source.stat().st_mode),
                    0o500,
                )
                self.assertEqual(
                    stat.S_IMODE(regular.stat().st_mode),
                    0o400,
                )
            finally:
                source.chmod(0o700)
                nested.chmod(0o700)
                regular.chmod(0o600)

    def test_fixture_copy_bounds_empty_directories_and_detects_source_races(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-fixture-topology."
        ) as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            for index in range(3):
                (source / f"empty-{index}").mkdir()
            destination = root / "bounded-copy"
            with (
                mock.patch.object(
                    packaged_fixture,
                    "MAX_PROJECT_FILES",
                    2,
                ),
                self.assertRaisesRegex(
                    packaged_fixture.PackagedFixtureError,
                    "path bound",
                ),
            ):
                packaged_fixture.copy_project_fixture(
                    source,
                    destination,
                )
            self.assertFalse(destination.exists())

            for path in source.iterdir():
                path.rmdir()
            nested = source / "nested"
            nested.mkdir()
            tracked = nested / "project.godot"
            tracked.write_text("[application]\n", encoding="utf-8")
            real_copytree = packaged_fixture.shutil.copytree

            def copy_then_mutate(*args: Any, **kwargs: Any) -> Any:
                result = real_copytree(*args, **kwargs)
                source.chmod(0o700)
                nested.chmod(0o700)
                tracked.chmod(0o600)
                tracked.write_text("[application]\n# changed\n", encoding="utf-8")
                return result

            destination = root / "racing-copy"
            tracked.chmod(0o400)
            nested.chmod(0o500)
            source.chmod(0o500)
            try:
                with (
                    mock.patch.object(
                        packaged_fixture.shutil,
                        "copytree",
                        side_effect=copy_then_mutate,
                    ),
                    self.assertRaisesRegex(
                        packaged_fixture.PackagedFixtureError,
                        "source changed",
                    ),
                ):
                    packaged_fixture.copy_project_fixture(
                        source,
                        destination,
                    )
                self.assertFalse(destination.exists())
                self.assertFalse(destination.is_symlink())
            finally:
                source.chmod(0o700)
                nested.chmod(0o700)
                tracked.chmod(0o600)

    def test_detached_archive_is_installed_and_removed_through_managed_root(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="s11-install-context.") as temporary:
            artifact, manifest = installable_package_fixture(Path(temporary))
            installed_data: Path | None = None
            with packaged.installed_package(artifact, manifest, 15) as installed:
                installed_data = installed.data_root
                self.assertEqual(installed.package_version, "0.1.0")
                self.assertTrue((installed.data_root / "current").is_symlink())
                self.assertEqual(
                    packaged.sha256_file(installed.sidecar),
                    packaged.sha256_bytes(b"#!/bin/sh\nexit 0\n"),
                )
                self.assertTrue(installed.operations.is_file())
            self.assertIsNotNone(installed_data)
            self.assertFalse(installed_data.exists())

    def test_fixture_provisioner_uses_exact_preview_and_apply(self) -> None:
        with tempfile.TemporaryDirectory(prefix="s11-fixture-setup.") as temporary:
            root = Path(temporary)
            data_root = root / "data"
            project_path = root / "project"
            version_root = data_root / "versions/0.1.0"
            binary_root = version_root / "bin"
            binary_root.mkdir(parents=True)
            tools = list(packaged_fixture._canonical_full_beta_tools())
            enabled = ",\n".join(f'  "{name}"' for name in tools)
            ownership_marker = "sha256:" + "2" * 64
            config = (
                "[mcp_servers.godot_editor]\n"
                f"# godot-codex-setup-owner: {ownership_marker}\n"
                f'command = "{data_root}/current/bin/godot-codex-mcp"\n'
                'args = ["--project-root", "."]\n'
                f'cwd = "{project_path.resolve()}"\n'
                "required = true\n"
                "startup_timeout_sec = 10\n"
                "tool_timeout_sec = 60\n"
                f'env = {{ GODOT_CODEX_DATA_ROOT = "{data_root}" }}\n'
                'default_tools_approval_mode = "writes"\n'
                "enabled_tools = [\n"
                f"{enabled}\n"
                "]\n"
            )
            operations = binary_root / "godot-codex"
            operations.write_text(
                "#!/usr/bin/python3\n"
                "import hashlib, json, os, pathlib, stat, sys\n"
                "home = pathlib.Path(os.environ['HOME'])\n"
                "plan = home / 'fixture-plan.json'\n"
                "digest = 'sha256:' + '1' * 64\n"
                f"config_text = {config!r}\n"
                "def digest_bytes(value):\n"
                "    return 'sha256:' + hashlib.sha256(value).hexdigest()\n"
                "def file_state(path):\n"
                "    return {'exists': True,\n"
                "            'digest': digest_bytes(path.read_bytes()),\n"
                "            'mode': stat.S_IMODE(path.stat().st_mode)}\n"
                "def receipt_for(root):\n"
                "    package_root = pathlib.Path(sys.argv[0]).resolve().parent.parent\n"
                "    identity = {\n"
                "      'operations': file_state(package_root / 'bin/godot-codex'),\n"
                "      'sidecar': file_state(package_root / 'bin/godot-codex-mcp'),\n"
                "      'manifest': file_state(package_root / 'package-manifest.json'),\n"
                "    }\n"
                "    identity_bytes = json.dumps(identity, separators=(',', ':')).encode()\n"
                "    lexical_launcher = pathlib.Path(os.environ['GODOT_CODEX_DATA_ROOT']) / 'current/bin/godot-codex-mcp'\n"
                "    project_identity = str(root.resolve()).rstrip('/').encode()\n"
                "    return {\n"
                "      'schema_version': 'godot-codex-setup-receipt/1.1',\n"
                "      'project_id': 'project:sha256:' + hashlib.sha256(b'godot-codex-project-id/v1\\0' + project_identity).hexdigest(),\n"
                "      'package_version': '0.1.0',\n"
                "      'profile': 'full-beta', 'guidance': 'none',\n"
                "      'package_identity_digest': digest_bytes(identity_bytes),\n"
                "      'launcher_path_sha256': digest_bytes(str(lexical_launcher).encode()),\n"
                "      'launcher_file_sha256': identity['sidecar']['digest'],\n"
                f"      'config_ownership_marker': {ownership_marker!r},\n"
                "      'config_table_digest': digest_bytes(config_text.rstrip(' \\t\\r\\n').encode()),\n"
                "      'config_created': True, 'config_file_mode': 0o644,\n"
                "      'agents_block_digest': None, 'agents_separator': None,\n"
                "      'agents_file_mode': None, 'skill_file_digest': None,\n"
                "      'skill_file_mode': None, 'plan_digest': digest,\n"
                "    }\n"
                "def safe_diff(path, before, after):\n"
                "    value = f'--- a/{path}\\n+++ b/{path}\\n@@ owned content @@\\n'\n"
                "    value += ''.join('-' + line + '\\n' for line in before.splitlines())\n"
                "    value += ''.join('+' + line + '\\n' for line in after.splitlines())\n"
                "    return value\n"
                "def redact(line):\n"
                "    if line.startswith('command = '):\n"
                "        return 'command = \"<package-launcher>\"'\n"
                "    if line.startswith('cwd = '):\n"
                "        return 'cwd = \"<project-root>\"'\n"
                "    if line.startswith('env = '):\n"
                "        return 'env = { GODOT_CODEX_DATA_ROOT = \"<package-data-root>\" }'\n"
                "    return line\n"
                "redacted_config = '\\n'.join(\n"
                "    redact(line) for line in config_text.splitlines()\n"
                ") + '\\n'\n"
                "if sys.argv[1:2] == ['setup'] and sys.argv[2:3] == ['--project-root'] and sys.argv[4:] == ['--profile', 'full-beta', '--guidance', 'none', '--dry-run', '--json']:\n"
                "    root = pathlib.Path(sys.argv[sys.argv.index('--project-root') + 1])\n"
                "    plan.write_text(json.dumps({'root': str(root)}))\n"
                "    receipt_text = json.dumps(receipt_for(root))\n"
                "    value = {\n"
                "      'schema_version': 'godot-codex-setup-plan/1.3',\n"
                "      'mode': 'configure', 'plan_digest': digest,\n"
                "      'expires_in_seconds': 600, 'package_version': '0.1.0',\n"
                "      'profile': 'full-beta', 'guidance': 'none',\n"
                "      'restart_required': True, 'project_trust_unchanged': True,\n"
                "      'changes': [\n"
                "        {'path': '.codex/config.toml', 'action': 'create',\n"
                "         'before_digest': 'sha256:missing',\n"
                "         'after_digest': digest_bytes(config_text.encode()),\n"
                "         'diff': safe_diff('.codex/config.toml', '<absent>\\n', redacted_config)},\n"
                "        {'path': '.godot/codex/setup-receipt-v1.json', 'action': 'create',\n"
                "         'before_digest': 'sha256:missing',\n"
                "         'after_digest': digest_bytes(receipt_text.encode()),\n"
                "         'diff': safe_diff('.godot/codex/setup-receipt-v1.json', '<absent>\\n', receipt_text)},\n"
                "      ],\n"
                "    }\n"
                "elif sys.argv[1:] == ['setup', '--apply-plan', digest, '--json']:\n"
                "    root = pathlib.Path(json.loads(plan.read_text())['root'])\n"
                "    (root / '.codex').mkdir(parents=True)\n"
                "    (root / '.godot/codex').mkdir(parents=True)\n"
                "    (root / '.codex').chmod(0o755)\n"
                "    (root / '.godot').chmod(0o700)\n"
                "    (root / '.godot/codex').chmod(0o700)\n"
                "    (root / '.codex/config.toml').write_text(config_text)\n"
                "    (root / '.codex/config.toml').chmod(0o644)\n"
                "    lock = root / '.godot/codex/setup-operation-v1.lock'\n"
                "    lock.write_bytes(b'')\n"
                "    lock.chmod(0o600)\n"
                "    receipt_path = root / '.godot/codex/setup-receipt-v1.json'\n"
                "    receipt_path.write_text(json.dumps(receipt_for(root)))\n"
                "    receipt_path.chmod(0o600)\n"
                "    value = {\n"
                "      'schema_version': 'godot-codex-setup-result/1.1',\n"
                "      'mode': 'configure', 'plan_digest': digest, 'status': 'applied',\n"
                "      'profile': 'full-beta', 'guidance': 'none',\n"
                "      'changed_paths': ['.codex/config.toml',\n"
                "                        '.godot/codex/setup-receipt-v1.json'],\n"
                "      'restart_required': True,\n"
                "    }\n"
                "else:\n"
                "    raise SystemExit(2)\n"
                "print(json.dumps(value, sort_keys=True))\n",
                encoding="utf-8",
            )
            sidecar = binary_root / "godot-codex-mcp"
            sidecar.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            (version_root / "package-manifest.json").write_text(
                '{"package_version":"0.1.0"}\n',
                encoding="utf-8",
            )
            os.chmod(operations, 0o755)
            os.chmod(sidecar, 0o755)
            data_root.mkdir(exist_ok=True)
            (data_root / "current").symlink_to(version_root)
            project = project_path
            project.mkdir()
            (project / "project.godot").write_text(
                "[application]\nconfig/name=\"fixture\"\n",
                encoding="utf-8",
            )
            old_data_root = os.environ.get("GODOT_CODEX_DATA_ROOT")
            old_home = os.environ.get("HOME")
            private_home = root / "home"
            private_home.mkdir()
            project_before = packaged_fixture._project_state(project)
            try:
                os.environ["HOME"] = str(private_home)
                packaged_fixture.activate_from_arguments(
                    argparse.Namespace(
                        package_operations=data_root
                        / "current/bin/godot-codex",
                        package_data_root=data_root,
                    )
                )
                packaged_fixture.configure_project_if_requested(project)
                self.assertTrue((project / ".codex/config.toml").is_file())
                self.assertTrue(
                    (project / ".godot/codex/setup-receipt-v1.json").is_file()
                )
                config_path = project / ".codex/config.toml"
                original_config = config_path.read_bytes()
                config_path.write_bytes(
                    original_config.replace(
                        b"godot_apply_transaction",
                        b"godot_fixture_transaction",
                    )
                )
                with self.assertRaisesRegex(
                    packaged_fixture.PackagedFixtureError,
                    "full-beta configuration differs",
                ):
                    packaged_fixture._validate_applied_project(
                        project_root=project,
                        before=project_before,
                        data_root=data_root,
                        plan_digest="sha256:" + "1" * 64,
                        package_version="0.1.0",
                    )
                config_path.write_bytes(original_config)
                for label, suffix in (
                    (
                        "extra_top_level",
                        b"\n[fixture_extra]\nvalue = true\n",
                    ),
                    (
                        "extra_mcp_server",
                        b"\n[mcp_servers.fixture_extra]\n"
                        b'command = "fixture"\n',
                    ),
                    (
                        "headers",
                        b"\nhttp_headers = "
                        b'{ Authorization = "redacted" }\n',
                    ),
                ):
                    with self.subTest(config_shape=label):
                        config_path.write_bytes(original_config + suffix)
                        with self.assertRaisesRegex(
                            packaged_fixture.PackagedFixtureError,
                            "full-beta configuration differs",
                        ):
                            packaged_fixture._validate_applied_project(
                                project_root=project,
                                before=project_before,
                                data_root=data_root,
                                plan_digest="sha256:" + "1" * 64,
                                package_version="0.1.0",
                            )
                config_path.write_bytes(original_config)
                changed_environment = original_config.replace(
                    (
                        "env = { GODOT_CODEX_DATA_ROOT = "
                        f'"{data_root}" }}'
                    ).encode(),
                    b'env = { GODOT_CODEX_DATA_ROOT = "/wrong" }',
                )
                self.assertNotEqual(changed_environment, original_config)
                config_path.write_bytes(changed_environment)
                with self.assertRaisesRegex(
                    packaged_fixture.PackagedFixtureError,
                    "full-beta configuration differs",
                ):
                    packaged_fixture._validate_applied_project(
                        project_root=project,
                        before=project_before,
                        data_root=data_root,
                        plan_digest="sha256:" + "1" * 64,
                        package_version="0.1.0",
                    )
                config_path.write_bytes(original_config)
                receipt_path = (
                    project / ".godot/codex/setup-receipt-v1.json"
                )
                receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
                receipt["plan_digest"] = "sha256:" + "f" * 64
                receipt_path.write_text(json.dumps(receipt), encoding="utf-8")
                receipt_path.chmod(0o600)
                with self.assertRaisesRegex(
                    packaged_fixture.PackagedFixtureError,
                    "setup receipt differs",
                ):
                    packaged_fixture._validate_applied_project(
                        project_root=project,
                        before=project_before,
                        data_root=data_root,
                        plan_digest="sha256:" + "1" * 64,
                        package_version="0.1.0",
                    )
            finally:
                packaged_fixture.reset_for_tests()
                if old_home is None:
                    os.environ.pop("HOME", None)
                else:
                    os.environ["HOME"] = old_home
                if old_data_root is not None:
                    os.environ["GODOT_CODEX_DATA_ROOT"] = old_data_root

    def test_fixture_scope_rejects_deletions_and_type_replacements(self) -> None:
        file_state = ("file", 0o644, 1, "sha256:" + "1" * 64)
        directory_state = ("directory", 0o755, 0, "")
        with self.assertRaisesRegex(
            packaged_fixture.PackagedFixtureError,
            "deleted or replaced",
        ):
            packaged_fixture._validate_project_scope(
                before={"fixture.txt": file_state},
                after={},
            )
        with self.assertRaisesRegex(
            packaged_fixture.PackagedFixtureError,
            "deleted or replaced",
        ):
            packaged_fixture._validate_project_scope(
                before={".codex": file_state},
                after={".codex": directory_state},
            )
        changed_file = ("file", 0o600, 1, "sha256:" + "2" * 64)
        before = {
            ".codex": ("directory", 0o700, 0, ""),
        }
        after = {
            ".codex": ("directory", 0o777, 0, ""),
            ".godot": ("directory", 0o700, 0, ""),
            ".godot/codex": ("directory", 0o700, 0, ""),
            ".codex/config.toml": changed_file,
            ".godot/codex/setup-receipt-v1.json": changed_file,
            ".godot/codex/setup-operation-v1.lock": changed_file,
        }
        with self.assertRaisesRegex(
            packaged_fixture.PackagedFixtureError,
            "directory mode|unsafe directory",
        ):
            packaged_fixture._validate_project_scope(
                before=before,
                after=after,
            )

    def test_fixture_preview_changes_are_exact_bound_and_redacted(self) -> None:
        def valid_changes() -> list[dict[str, Any]]:
            changes: list[dict[str, Any]] = []
            for path in sorted(packaged_fixture.EXPECTED_CHANGED_PATHS):
                added = (
                    'command = "<package-launcher>"'
                    if path == ".codex/config.toml"
                    else "{}"
                )
                changes.append(
                    {
                        "path": path,
                        "action": "create",
                        "before_digest": packaged_fixture.MISSING_DIGEST,
                        "after_digest": "sha256:" + "1" * 64,
                        "diff": (
                            f"--- a/{path}\n"
                            f"+++ b/{path}\n"
                            "@@ owned content @@\n"
                            "-<absent>\n"
                            f"+{added}\n"
                        ),
                    }
                )
            return changes

        project_root = Path("/qualification/project")
        data_root = Path("/qualification/package")
        records = packaged_fixture._validate_setup_changes(
            changes=valid_changes(),
            before={},
            project_root=project_root,
            data_root=data_root,
        )
        self.assertEqual(set(records), packaged_fixture.EXPECTED_CHANGED_PATHS)
        with tempfile.TemporaryDirectory(
            prefix="s11-exact-setup-diff."
        ) as temporary:
            applied_root = Path(temporary)
            (applied_root / ".codex").mkdir()
            (applied_root / ".godot/codex").mkdir(parents=True)
            config_text = (
                "[mcp_servers.godot_editor]\n"
                'command = "/private/package/godot-codex-mcp"\n'
                'args = ["--project-root", "."]\n'
                'cwd = "/private/project"\n'
                'env = { GODOT_CODEX_DATA_ROOT = "/private/data" }\n'
            )
            receipt_text = "{}"
            (applied_root / ".codex/config.toml").write_text(
                config_text,
                encoding="utf-8",
            )
            (
                applied_root
                / ".godot/codex/setup-receipt-v1.json"
            ).write_text(receipt_text, encoding="utf-8")
            matching_after = packaged_fixture._project_state(applied_root)
            exact_records = copy.deepcopy(records)
            for path, after_display in (
                (
                    ".codex/config.toml",
                    packaged_fixture._redact_config_launcher(
                        packaged_fixture._config_table_stanza(config_text)
                    ),
                ),
                (
                    ".godot/codex/setup-receipt-v1.json",
                    receipt_text,
                ),
            ):
                self.assertNotIn("/private/", after_display)
                exact_records[path]["after_digest"] = matching_after[path][3]
                exact_records[path]["diff"] = (
                    packaged_fixture._canonical_setup_diff(
                        path,
                        "<absent>\n",
                        after_display,
                    )
                )
            packaged_fixture._validate_setup_change_outputs(
                changes=exact_records,
                after=matching_after,
                before_displays={
                    path: "<absent>\n"
                    for path in packaged_fixture.EXPECTED_CHANGED_PATHS
                },
                project_root=applied_root,
            )
            misleading = copy.deepcopy(exact_records)
            misleading[
                ".godot/codex/setup-receipt-v1.json"
            ]["diff"] = (
                "--- a/.godot/codex/setup-receipt-v1.json\n"
                "+++ b/.godot/codex/setup-receipt-v1.json\n"
                "@@ owned content @@\n"
                "-<absent>\n"
                "+{}\n"
                "+misleading\n"
            )
            with self.assertRaisesRegex(
                packaged_fixture.PackagedFixtureError,
                "diff differs from applied output",
            ):
                packaged_fixture._validate_setup_change_outputs(
                    changes=misleading,
                    after=matching_after,
                    before_displays={
                        path: "<absent>\n"
                        for path in packaged_fixture.EXPECTED_CHANGED_PATHS
                    },
                    project_root=applied_root,
                )
            mismatched_after = copy.deepcopy(matching_after)
            mismatched_after[".codex/config.toml"] = (
                "file",
                0o644,
                1,
                "sha256:" + "f" * 64,
            )
            with self.assertRaisesRegex(
                packaged_fixture.PackagedFixtureError,
                "output differs from its preview",
            ):
                packaged_fixture._validate_setup_change_outputs(
                    changes=exact_records,
                    after=mismatched_after,
                    before_displays={
                        path: "<absent>\n"
                        for path in packaged_fixture.EXPECTED_CHANGED_PATHS
                    },
                    project_root=applied_root,
                )

        for label, mutate, expected in (
            (
                "extra_field",
                lambda changes: changes[0].__setitem__("extra", True),
                "change contract differs",
            ),
            (
                "missing_field",
                lambda changes: changes[0].pop("action"),
                "change contract differs",
            ),
            (
                "wrong_action",
                lambda changes: changes[0].__setitem__("action", "update"),
                "change binding differs",
            ),
            (
                "wrong_before_digest",
                lambda changes: changes[0].__setitem__(
                    "before_digest",
                    "sha256:" + "2" * 64,
                ),
                "change binding differs",
            ),
            (
                "unsafe_diff",
                lambda changes: changes[0].__setitem__(
                    "diff",
                    changes[0]["diff"] + "+/Users/fixture/private\n",
                ),
                "exposes unsafe material",
            ),
            (
                "unbounded_diff",
                lambda changes: changes[0].__setitem__(
                    "diff",
                    changes[0]["diff"]
                    + "+"
                    + "x" * packaged_fixture.MAX_SETUP_DIFF_BYTES
                    + "\n",
                ),
                "preview diff differs",
            ),
        ):
            with self.subTest(change_contract=label):
                changed = valid_changes()
                mutate(changed)
                with self.assertRaisesRegex(
                    packaged_fixture.PackagedFixtureError,
                    expected,
                ):
                    packaged_fixture._validate_setup_changes(
                        changes=changed,
                        before={},
                        project_root=project_root,
                        data_root=data_root,
                    )

    def test_fixture_json_rejects_duplicates_and_non_finite_values(self) -> None:
        for payload in (
            b'{"value":1,"value":2}',
            b'{"value":NaN}',
            b'{"value":Infinity}',
        ):
            with self.subTest(payload=payload), self.assertRaises(
                packaged_fixture.PackagedFixtureError
            ):
                packaged_fixture._strict_json(payload, label="fixture")

    def test_fixture_apply_result_requires_an_exact_changed_path_list(self) -> None:
        preview = {
            "schema_version": packaged_fixture.SETUP_PLAN_SCHEMA,
            "mode": "configure",
            "plan_digest": "sha256:" + "1" * 64,
            "expires_in_seconds": 60,
            "package_version": "0.1.0",
            "profile": "full-beta",
            "guidance": "none",
            "restart_required": True,
            "project_trust_unchanged": True,
            "changes": [
                {
                    "path": path,
                    "action": "create",
                    "before_digest": packaged_fixture.MISSING_DIGEST,
                    "after_digest": "sha256:" + "2" * 64,
                    "diff": (
                        f"--- a/{path}\n"
                        f"+++ b/{path}\n"
                        "@@ owned content @@\n"
                        "-<absent>\n"
                        + (
                            '+command = "<package-launcher>"\n'
                            if path == ".codex/config.toml"
                            else "+{}\n"
                        )
                    ),
                }
                for path in sorted(packaged_fixture.EXPECTED_CHANGED_PATHS)
            ],
        }
        malformed_apply = {
            "schema_version": packaged_fixture.SETUP_RESULT_SCHEMA,
            "mode": "configure",
            "plan_digest": preview["plan_digest"],
            "status": "applied",
            "profile": "full-beta",
            "guidance": "none",
            "changed_paths": {
                path: True
                for path in packaged_fixture.EXPECTED_CHANGED_PATHS
            },
            "restart_required": True,
        }
        with tempfile.TemporaryDirectory(
            prefix="s11-malformed-setup-result."
        ) as temporary:
            project = Path(temporary)
            (project / "project.godot").write_text(
                "[application]\nconfig/name=\"fixture\"\n",
                encoding="utf-8",
            )
            provisioner = packaged_fixture.PackagedFixtureProvisioner(
                operations=project / "operations",
                data_root=project / "data",
            )
            with (
                mock.patch.object(
                    packaged_fixture,
                    "_run",
                    side_effect=[preview, malformed_apply],
                ),
                self.assertRaisesRegex(
                    packaged_fixture.PackagedFixtureError,
                    "apply result differs",
                ),
            ):
                provisioner.configure_project(project)

    @unittest.skipUnless(os.name == "posix", "POSIX process-group contract")
    def test_fixture_setup_process_is_stream_bounded_and_kills_descendants(
        self,
    ) -> None:
        for failure, parent_body, expected in (
            ("timeout", "time.sleep(30)", "timed out"),
            (
                "output",
                "sys.stdout.buffer.write(b'x' * 131072); "
                "sys.stdout.buffer.flush(); time.sleep(30)",
                "output exceeds its bound",
            ),
        ):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory(
                prefix="s11-setup-process-group."
            ) as temporary:
                marker = Path(temporary) / "descendant-survived"
                child = (
                    "import pathlib,time; time.sleep(0.8); "
                    f"pathlib.Path({str(marker)!r}).write_text('leak')"
                )
                parent = (
                    "import subprocess,sys,time; "
                    f"subprocess.Popen([sys.executable,'-c',{child!r}]); "
                    f"{parent_body}"
                )
                with (
                    mock.patch.object(
                        packaged_fixture,
                        "MAX_SETUP_SECONDS",
                        0.2 if failure == "timeout" else 5.0,
                    ),
                    mock.patch.object(
                        packaged_fixture,
                        "MAX_SETUP_OUTPUT_BYTES",
                        1024,
                    ),
                    self.assertRaisesRegex(
                        packaged_fixture.PackagedFixtureError,
                        expected,
                    ),
                ):
                    packaged_fixture._bounded_process(
                        [sys.executable, "-c", parent],
                        environment=os.environ.copy(),
                    )
                time.sleep(1.0)
                self.assertFalse(marker.exists())

    @unittest.skipUnless(os.name == "posix", "POSIX process-group contract")
    def test_packaged_sprint8_editor_inherits_the_outer_acquisition_group(
        self,
    ) -> None:
        script_root = str(packaged.SCRIPT_DIR)
        if script_root not in sys.path:
            sys.path.insert(0, script_root)
        sprint8 = importlib.import_module("sprint8_runtime_live")
        with mock.patch.object(
            packaged_fixture,
            "provisioning_is_active",
            return_value=True,
        ):
            options, isolated = sprint8.editor_process_group_options()
        self.assertEqual(options, {})
        self.assertFalse(isolated)
        with mock.patch.object(
            packaged_fixture,
            "provisioning_is_active",
            return_value=False,
        ):
            options, isolated = sprint8.editor_process_group_options()
        self.assertEqual(options, {"start_new_session": True})
        self.assertTrue(isolated)

    def test_live_runners_expose_opt_in_sprint11_profile(self) -> None:
        for name in (
            "sprint6_semantic_context_live.py",
            "sprint7_live_editor.py",
            "sprint8_runtime_live.py",
            "sprint10_model_free_live.py",
        ):
            result = subprocess.run(
                [sys.executable, str(packaged.SCRIPT_DIR / name), "--help"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("--additive-sprint11-registry", result.stdout)

    def test_detached_version_and_source_coordinates_are_cross_bound(self) -> None:
        for field, value, expected in (
            ("package_version", "9.9.9", "archive/version binding differs"),
            ("source_commit", "d" * 40, "package SOURCE_COMMIT binding differs"),
        ):
            with self.subTest(field=field), tempfile.TemporaryDirectory(
                prefix="s11-package-coordinate."
            ) as temporary:
                root = Path(temporary)
                artifact, manifest_path = installable_package_fixture(root)
                manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
                manifest[field] = value
                manifest_path.write_bytes(packaged.canonical_json(manifest) + b"\n")
                with self.assertRaisesRegex(
                    packaged.PackagedRegressionError,
                    expected,
                ):
                    packaged._extract_verified_package(
                        artifact_root=artifact,
                        package_manifest=manifest_path,
                        destination=root / "extracted",
                    )

    @unittest.skipUnless(os.name == "posix", "POSIX process-group contract")
    def test_bounded_process_timeout_and_output_overflow_kill_descendants(
        self,
    ) -> None:
        for failure, parent_body, expected in (
            (
                "timeout",
                "time.sleep(30)",
                "timed out",
            ),
            (
                "output",
                "sys.stdout.buffer.write(b'x' * 131072); "
                "sys.stdout.buffer.flush(); time.sleep(30)",
                "output exceeds byte bound",
            ),
        ):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory(
                prefix="s11-process-group."
            ) as temporary:
                marker = Path(temporary) / "descendant-survived"
                child = (
                    "import pathlib,time; time.sleep(0.8); "
                    f"pathlib.Path({str(marker)!r}).write_text('leak')"
                )
                parent = (
                    "import subprocess,sys,time; "
                    f"subprocess.Popen([sys.executable,'-c',{child!r}]); "
                    f"{parent_body}"
                )
                with self.assertRaisesRegex(
                    packaged.PackagedRegressionError,
                    expected,
                ):
                    packaged._run_bounded_process(
                        [sys.executable, "-c", parent],
                        timeout=0.2 if failure == "timeout" else 5.0,
                        output_limit=1024,
                        label=f"{failure} probe",
                    )
                time.sleep(1.0)
                self.assertFalse(marker.exists())

    @unittest.skipUnless(os.name == "posix", "POSIX process scope contract")
    def test_bounded_runners_preserve_primary_scope_cleanup_failure(
        self,
    ) -> None:
        scope_module = packaged_fixture.process_scope
        real_close_scope = scope_module.close_scope

        def close_then_fail(
            scope: scope_module.ProcessScope,
        ) -> bool:
            real_close_scope(scope)
            raise scope_module.ProcessScopeError(
                "fixture cleanup failure"
            )

        cases = (
            (
                packaged.PackagedRegressionError,
                lambda: packaged._run_bounded_process(
                    [sys.executable, "-c", "import time; time.sleep(30)"],
                    timeout=0.05,
                    output_limit=1024,
                    label="cleanup probe",
                ),
            ),
            (
                packaged_fixture.PackagedFixtureError,
                lambda: packaged_fixture._bounded_process(
                    [sys.executable, "-c", "import time; time.sleep(30)"],
                    environment=os.environ.copy(),
                ),
            ),
        )
        for expected_error, operation in cases:
            with (
                self.subTest(error=expected_error.__name__),
                mock.patch.object(
                    scope_module,
                    "close_scope",
                    side_effect=close_then_fail,
                ),
                mock.patch.object(
                    packaged_fixture,
                    "MAX_SETUP_SECONDS",
                    0.05,
                ),
                self.assertRaisesRegex(expected_error, "timed out") as raised,
            ):
                operation()
            self.assertTrue(
                any(
                    "detached process scope could not be closed" in note
                    for note in getattr(raised.exception, "__notes__", ())
                )
            )

    @unittest.skipUnless(os.name == "posix", "POSIX process scope contract")
    def test_bounded_runners_close_partially_activated_scope(self) -> None:
        scope_module = packaged_fixture.process_scope
        real_activate_scope = scope_module.activate_scope
        real_close_scope = scope_module.close_scope
        activated_processes: list[subprocess.Popen[bytes]] = []

        def activate_then_fail(
            scope: scope_module.ProcessScope,
            process: subprocess.Popen[bytes],
        ) -> None:
            real_activate_scope(scope, process)
            activated_processes.append(process)
            raise scope_module.ProcessScopeError(
                "post-activation failure"
            )

        cases = (
            (
                packaged.PackagedRegressionError,
                "regression activation probe process scope could not be activated",
                lambda: packaged._run_bounded_process(
                    [sys.executable, "-c", "import time;time.sleep(30)"],
                    timeout=10,
                    output_limit=1024,
                    label="regression activation probe",
                ),
            ),
            (
                packaged_fixture.PackagedFixtureError,
                "setup process scope could not be activated",
                lambda: packaged_fixture._bounded_process(
                    [sys.executable, "-c", "import time;time.sleep(30)"],
                    environment=os.environ.copy(),
                ),
            ),
        )
        for expected_error, message, operation in cases:
            with (
                self.subTest(error=expected_error.__name__),
                mock.patch.object(
                    scope_module,
                    "activate_scope",
                    side_effect=activate_then_fail,
                ),
                mock.patch.object(
                    scope_module,
                    "close_scope",
                    wraps=real_close_scope,
                ) as close_scope,
                self.assertRaisesRegex(expected_error, message),
            ):
                operation()
            close_scope.assert_called_once()
            process = activated_processes.pop()
            self.assertIsNotNone(process.poll())
            self.assertTrue(process.stdout is not None and process.stdout.closed)
            self.assertTrue(process.stderr is not None and process.stderr.closed)

    def test_bounded_runners_reject_empty_commands_without_unbound_cleanup(
        self,
    ) -> None:
        cases = (
            (
                packaged.PackagedRegressionError,
                "command scope differs",
                lambda: packaged._run_bounded_process(
                    [],
                    timeout=1,
                    output_limit=1,
                    label="empty regression probe",
                ),
            ),
            (
                packaged_fixture.PackagedFixtureError,
                "command scope differs",
                lambda: packaged_fixture._bounded_process(
                    [],
                    environment=os.environ.copy(),
                ),
            ),
        )
        for expected_error, message, operation in cases:
            with (
                self.subTest(error=expected_error.__name__),
                self.assertRaisesRegex(expected_error, message),
            ):
                operation()

    @unittest.skipUnless(
        sys.platform == "darwin",
        "Darwin original-parent process identities are required",
    )
    def test_scope_terminates_setsid_child_with_clean_environment(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-clean-environment-escape."
        ) as temporary:
            root = Path(temporary)
            child_pid_path = root / "escaped-child.pid"
            marker = root / "escaped-child-marker"
            child = (
                "import os,pathlib,sys,time;"
                "pathlib.Path(sys.argv[1]).write_text(str(os.getpid()));"
                "time.sleep(0.8);"
                "pathlib.Path(sys.argv[2]).write_text('escaped');"
                "time.sleep(30)"
            )
            parent = (
                "import subprocess,sys;"
                "subprocess.Popen("
                f"[sys.executable,'-c',{child!r},"
                f"{str(child_pid_path)!r},{str(marker)!r}],"
                "start_new_session=True,"
                "env={'PATH':'/usr/bin:/bin'},"
                "stdin=subprocess.DEVNULL,"
                "stdout=subprocess.DEVNULL,"
                "stderr=subprocess.DEVNULL)"
            )
            child_pid: int | None = None
            try:
                with self.assertRaisesRegex(
                    packaged.PackagedRegressionError,
                    "detached descendants escaped",
                ):
                    packaged._run_bounded_process(
                        [sys.executable, "-c", parent],
                        timeout=10,
                        output_limit=1024,
                        label="clean environment escape probe",
                    )
                if child_pid_path.exists():
                    child_pid = int(
                        child_pid_path.read_text(encoding="utf-8")
                    )
                    status = subprocess.run(
                        ["ps", "-o", "stat=", "-p", str(child_pid)],
                        check=False,
                        capture_output=True,
                        text=True,
                    )
                    self.assertTrue(
                        status.returncode != 0
                        or status.stdout.lstrip().startswith("Z")
                    )
                time.sleep(1)
                self.assertFalse(marker.exists())
            finally:
                if child_pid is None and child_pid_path.exists():
                    child_pid = int(
                        child_pid_path.read_text(encoding="utf-8")
                    )
                if child_pid is not None:
                    try:
                        os.kill(child_pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    @unittest.skipUnless(
        sys.platform == "darwin",
        "Darwin exec-version process identities are required",
    )
    def test_scope_tracks_exec_of_known_descendant(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-exec-descendant-escape."
        ) as temporary:
            root = Path(temporary)
            child_pid_path = root / "exec-descendant.pid"
            marker = root / "exec-descendant-marker"
            leaf = (
                "import os,pathlib,sys,time;"
                "pathlib.Path(sys.argv[1]).write_text(str(os.getpid()));"
                "time.sleep(0.8);"
                "pathlib.Path(sys.argv[2]).write_text('escaped');"
                "time.sleep(30)"
            )
            after_exec = (
                "import subprocess,sys;"
                "subprocess.Popen("
                f"[sys.executable,'-c',{leaf!r},"
                f"{str(child_pid_path)!r},{str(marker)!r}],"
                "start_new_session=True,"
                "env={'PATH':'/usr/bin:/bin'},"
                "stdin=subprocess.DEVNULL,"
                "stdout=subprocess.DEVNULL,"
                "stderr=subprocess.DEVNULL)"
            )
            tracked_child = (
                "import os,sys,time;"
                "time.sleep(0.25);"
                "os.execve("
                "sys.executable,"
                f"[sys.executable,'-c',{after_exec!r}],"
                "os.environ.copy())"
            )
            parent = (
                "import subprocess,sys;"
                "child=subprocess.Popen("
                f"[sys.executable,'-c',{tracked_child!r}],"
                "stdin=subprocess.DEVNULL,"
                "stdout=subprocess.DEVNULL,"
                "stderr=subprocess.DEVNULL);"
                "child.wait()"
            )
            child_pid: int | None = None
            try:
                with self.assertRaisesRegex(
                    packaged.PackagedRegressionError,
                    "detached descendants escaped",
                ):
                    packaged._run_bounded_process(
                        [sys.executable, "-c", parent],
                        timeout=10,
                        output_limit=1024,
                        label="exec descendant escape probe",
                    )
                time.sleep(1)
                self.assertFalse(marker.exists())
            finally:
                if child_pid_path.exists():
                    child_pid = int(
                        child_pid_path.read_text(encoding="utf-8")
                    )
                if child_pid is not None:
                    try:
                        os.kill(child_pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    @unittest.skipUnless(os.name == "posix", "POSIX process-group contract")
    def test_successful_leader_cannot_leave_a_background_process(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-process-group-success."
        ) as temporary:
            marker = Path(temporary) / "descendant-survived"
            child = (
                "import pathlib,time; time.sleep(0.8); "
                f"pathlib.Path({str(marker)!r}).write_text('leak')"
            )
            parent = (
                "import subprocess,sys; "
                "subprocess.Popen("
                f"[sys.executable,'-c',{child!r}], "
                "stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, "
                "stderr=subprocess.DEVNULL, start_new_session=True)"
            )
            with self.assertRaisesRegex(
                packaged.PackagedRegressionError,
                "detached descendants escaped",
            ):
                packaged._run_bounded_process(
                    [sys.executable, "-c", parent],
                    timeout=5.0,
                    output_limit=1024,
                    label="surviving descendant probe",
                )
            time.sleep(1.0)
            self.assertFalse(marker.exists())

    @unittest.skipUnless(os.name == "posix", "POSIX process scope contract")
    def test_nested_bounded_runner_retains_the_outer_scope(self) -> None:
        nested = (
            "import os,sys;"
            "from tests.codex import sprint11_packaged_fixture as fixture;"
            "result=fixture._bounded_process("
            "[sys.executable,'-c','import sys;sys.exit(0)'],"
            "environment=os.environ.copy());"
            "sys.exit(result.returncode)"
        )
        result = packaged._run_bounded_process(
            [sys.executable, "-c", nested],
            cwd=packaged.REPOSITORY_ROOT,
            environment=os.environ.copy(),
            timeout=10,
            output_limit=4096,
            label="nested bounded scope probe",
        )
        self.assertEqual(result.returncode, 0)

    @unittest.skipUnless(os.name == "posix", "POSIX process scope contract")
    def test_process_scope_coalesces_event_storms(self) -> None:
        class EventStorm:
            def __init__(self) -> None:
                self.calls = 0

            def control(
                self,
                changelist: Any,
                max_events: int,
                timeout: float,
            ) -> list[Any]:
                self.assert_wait(changelist, max_events, timeout)
                self.calls += 1
                return []

            @staticmethod
            def assert_wait(
                changelist: Any,
                max_events: int,
                timeout: float,
            ) -> None:
                if (
                    changelist is not None
                    or max_events != 256
                    or timeout
                    != packaged_fixture.process_scope.TRACK_INTERVAL_SECONDS
                ):
                    raise AssertionError("event wait contract differs")

        scope_module = packaged_fixture.process_scope
        event_storm = EventStorm()
        scope = scope_module.ProcessScope(
            marker_key=(
                scope_module.SCOPE_ENVIRONMENT_PREFIX + "a" * 48
            ),
            event_queue=event_storm,
        )
        table_calls: list[bool] = []
        table = {
            os.getpid(): scope_module._ProcessRecord(
                process_id=os.getpid(),
                parent_id=os.getppid(),
                started="fixture",
                command=b"fixture",
            )
        }

        def process_table(
            *,
            include_environment: bool = True,
        ) -> dict[int, Any]:
            table_calls.append(include_environment)
            return table

        with mock.patch.object(
            scope_module,
            "_process_table",
            side_effect=process_table,
        ):
            tracker = threading.Thread(
                target=scope_module._track_scope,
                args=(scope,),
            )
            tracker.start()
            time.sleep(scope_module.TRACK_INTERVAL_SECONDS * 2.6)
            scope.stop.set()
            tracker.join(timeout=2)

        self.assertFalse(tracker.is_alive())
        self.assertFalse(scope.failures)
        self.assertGreaterEqual(len(table_calls), 1)
        self.assertLessEqual(len(table_calls), 3)
        self.assertTrue(all(value is False for value in table_calls))
        self.assertLessEqual(event_storm.calls, 4)

    def test_process_scope_revalidates_darwin_identity_before_signal(
        self,
    ) -> None:
        scope_module = packaged_fixture.process_scope
        process_id = 12345
        started = "Sat Jul 25 00:00:00 2026"
        record = scope_module._ProcessRecord(
            process_id=process_id,
            parent_id=1,
            started=started,
            command=b"fixture",
            unique_id=101,
            parent_unique_id=1,
            id_version=201,
            original_parent_id_version=1,
        )
        scope = scope_module.ProcessScope(
            marker_key=(
                scope_module.SCOPE_ENVIRONMENT_PREFIX + "b" * 48
            ),
            identities={process_id: started},
            unique_ids={101},
            id_versions={201},
        )
        with (
            mock.patch.object(
                scope_module.sys,
                "platform",
                "darwin",
            ),
            mock.patch.object(
                scope_module,
                "_process_table",
                side_effect=({process_id: record}, {}),
            ),
            mock.patch.object(
                scope_module,
                "_darwin_process_unique_ids",
                return_value=(102, 1, 202, 1),
            ),
            mock.patch.object(scope_module.os, "kill") as kill,
        ):
            observed = scope_module._terminate_scope_identities(scope)
        self.assertTrue(observed)
        kill.assert_not_called()

    def _acquire_fixture(
        self,
    ) -> tuple[dict[str, Any], Path, dict[str, Any], tempfile.TemporaryDirectory[str]]:
        temporary = tempfile.TemporaryDirectory(
            prefix=".s11-package-live-test.",
            dir=packaged.REPOSITORY_ROOT,
        )
        root = Path(temporary.name)
        artifact_root = root / "artifact"
        artifact_root.mkdir()
        sidecar = artifact_root / packaged.PACKAGE_SIDECAR_PATH
        sidecar.parent.mkdir()
        sidecar.write_bytes(b"package-sidecar")
        godot = root / "Godot"
        godot.write_bytes(b"godot-prerequisite")
        manifest = root / "sprint11-package-manifest.json"
        manifest.write_text(
            '{"package_version":"0.1.0"}\n',
            encoding="utf-8",
        )
        bindings = {
            "package_source_commit": "a" * 40,
            "package_manifest_sha256": packaged.sha256_file(manifest),
            "package_build_provenance_sha256": packaged.sha256_bytes(
                packaged.canonical_json(
                    {
                        "cargo_lock_sha256": packaged.sha256_bytes(b"lock"),
                        "cargo_version": "cargo fixture",
                        "fresh_target": True,
                        "rust_toolchain_sha256": packaged.sha256_bytes(b"toolchain"),
                        "rustc_commit": "c" * 40,
                        "rustc_release": "fixture",
                        "target_triple": "aarch64-apple-darwin",
                    }
                )
            ),
            "package_archive_sha256": "sha256:" + "1" * 64,
            "package_sidecar_path": packaged.PACKAGE_SIDECAR_PATH,
            "package_sidecar_sha256": packaged.sha256_file(sidecar),
            "godot_version": "4.8.dev.codex.fixture",
            "godot_commit": "b" * 40,
            "godot_artifact_sha256": packaged.sha256_file(godot),
            "registry_sha256": "sha256:" + "2" * 64,
        }

        def execute(
            spec: packaged.CommandSpec,
            argv: tuple[str, ...],
            _cwd: Path,
            _timeout: float,
        ) -> packaged.Execution:
            self.assertEqual(_cwd, packaged.REPOSITORY_ROOT)
            report_path = Path(argv[-1])
            report = synthetic_report(
                spec.command_id,
                godot_sha256=bindings["godot_artifact_sha256"],
                sidecar_sha256=bindings["package_sidecar_sha256"],
            )
            report_path.write_text(
                json.dumps(report, ensure_ascii=False, sort_keys=True) + "\n",
                encoding="utf-8",
            )
            return packaged.Execution(0, b"ok", b"", 1)

        output_root = root / "output"

        @contextlib.contextmanager
        def package_session(
            _artifact_root: Path,
            _manifest: Path,
            _timeout: float,
        ) -> Any:
            yield packaged.InstalledPackage(
                operations=sidecar,
                sidecar=sidecar,
                data_root=artifact_root,
                package_version="0.1.0",
            )

        @contextlib.contextmanager
        def source_session(source_commit: str) -> Any:
            yield packaged.SourceSnapshot(
                root=packaged.REPOSITORY_ROOT,
                source_commit=source_commit,
            )

        with mock.patch.object(
            packaged,
            "_manifest_bindings",
            return_value=(bindings, sidecar),
        ):
            receipt = packaged.acquire(
                artifact_root=artifact_root,
                package_manifest=manifest,
                godot=godot,
                output_root=output_root,
                timeout=17,
                executor=execute,
                check_repository=False,
                package_session_factory=package_session,
                source_session_factory=source_session,
            )
        return receipt, output_root, bindings, temporary

    def test_acquisition_is_atomic_bounded_and_covers_every_shard(self) -> None:
        receipt, output_root, _bindings, temporary = self._acquire_fixture()
        try:
            self.assertEqual(receipt["capture_kind"], "real_package_live")
            self.assertEqual(receipt["coverage"]["sprints"], [6, 7, 8, 9, 10])
            self.assertEqual(len(receipt["commands"]), 11)
            self.assertTrue((output_root / "receipt.json").is_file())
            self.assertTrue(
                all(
                    not Path(item["report_path"]).is_absolute()
                    for item in receipt["commands"]
                )
            )
        finally:
            temporary.cleanup()

    def test_receipt_validator_cross_binds_package_runners_and_reports(self) -> None:
        receipt, output_root, bindings, temporary = self._acquire_fixture()
        try:
            package_source = bindings["package_source_commit"]
            source_commit = "c" * 40
            blobs: dict[tuple[str, str], bytes] = {}
            for relative in packaged.FIXTURE_PATHS:
                blobs[(package_source, relative)] = (
                    packaged.REPOSITORY_ROOT / relative
                ).read_bytes()
            specs = packaged.canonical_command_specs()
            rebound = copy.deepcopy(receipt)
            for spec, command in zip(specs, rebound["commands"]):
                blobs[(package_source, spec.runner_path)] = (
                    packaged.REPOSITORY_ROOT / spec.runner_path
                ).read_bytes()
                canonical_path = (
                    "tests/codex/acquisition/sprint11/package-live/"
                    + spec.report_name
                )
                command["report_path"] = canonical_path
                blobs[(source_commit, canonical_path)] = (
                    output_root / spec.report_name
                ).read_bytes()
            package = {
                "build_provenance": {
                    "cargo_lock_sha256": packaged.sha256_bytes(b"lock"),
                    "cargo_version": "cargo fixture",
                    "fresh_target": True,
                    "rust_toolchain_sha256": packaged.sha256_bytes(b"toolchain"),
                    "rustc_commit": "c" * 40,
                    "rustc_release": "fixture",
                    "target_triple": "aarch64-apple-darwin",
                },
                "contents": [
                    {
                        "path": packaged.PACKAGE_SIDECAR_PATH,
                        "sha256": bindings["package_sidecar_sha256"],
                    }
                ],
                "godot_prerequisite": {
                    "version": bindings["godot_version"],
                    "commit": bindings["godot_commit"],
                },
            }
            repository = FakeRepository(blobs)
            paths = acceptance.validate_packaged_regression_receipt(
                rebound,
                repository=repository,  # type: ignore[arg-type]
                source_commit=source_commit,
                package_source_commit=package_source,
                package_manifest_sha256=bindings["package_manifest_sha256"],
                package_archive_sha256=bindings["package_archive_sha256"],
                package=package,
                registry_sha256=bindings["registry_sha256"],
                godot_artifact_sha256=bindings["godot_artifact_sha256"],
            )
            self.assertEqual(len(paths), 11)

            changed = copy.deepcopy(rebound)
            changed["bindings"]["package_sidecar_sha256"] = (
                "sha256:" + "f" * 64
            )
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_packaged_regression_receipt(
                    changed,
                    repository=repository,  # type: ignore[arg-type]
                    source_commit=source_commit,
                    package_source_commit=package_source,
                    package_manifest_sha256=bindings["package_manifest_sha256"],
                    package_archive_sha256=bindings["package_archive_sha256"],
                    package=package,
                    registry_sha256=bindings["registry_sha256"],
                    godot_artifact_sha256=bindings["godot_artifact_sha256"],
                )

            changed = copy.deepcopy(rebound)
            changed["commands"][0]["argv_template"].remove(
                "--additive-sprint11-registry"
            )
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_packaged_regression_receipt(
                    changed,
                    repository=repository,  # type: ignore[arg-type]
                    source_commit=source_commit,
                    package_source_commit=package_source,
                    package_manifest_sha256=bindings["package_manifest_sha256"],
                    package_archive_sha256=bindings["package_archive_sha256"],
                    package=package,
                    registry_sha256=bindings["registry_sha256"],
                    godot_artifact_sha256=bindings["godot_artifact_sha256"],
                )
        finally:
            temporary.cleanup()

    def test_status_only_and_incomplete_shards_cannot_qualify(self) -> None:
        with self.assertRaises(packaged.PackagedRegressionError):
            packaged.validate_live_report(
                "s8_runtime",
                {
                    "status": "passed",
                    "artifacts": {
                        "godot_sha256": "sha256:" + "1" * 64,
                        "sidecar_sha256": "sha256:" + "2" * 64,
                    },
                },
                godot_sha256="sha256:" + "1" * 64,
                sidecar_sha256="sha256:" + "2" * 64,
            )
        reports = {
            spec.command_id: synthetic_report(
                spec.command_id,
                godot_sha256="sha256:" + "1" * 64,
                sidecar_sha256="sha256:" + "2" * 64,
            )
            for spec in packaged.canonical_command_specs()
        }
        reports["s9_operations_a"]["operations"].pop()
        with self.assertRaises(packaged.PackagedRegressionError):
            packaged.validate_aggregate_reports(reports)


if __name__ == "__main__":
    unittest.main()
