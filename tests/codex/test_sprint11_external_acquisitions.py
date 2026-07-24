from __future__ import annotations

import contextlib
import copy
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from collections.abc import Iterator
from pathlib import Path
from unittest import mock

from tests.codex import sprint11_external_acquisitions as acquisitions
from tests.codex import sprint11_host_provenance as host_provenance


@contextlib.contextmanager
def current_source_snapshot(
    source_commit: str,
) -> Iterator[acquisitions.SourceSnapshot]:
    yield acquisitions.SourceSnapshot(
        root=acquisitions.REPOSITORY_ROOT,
        source_commit=source_commit,
    )


def current_runner_digest(
    _snapshot: acquisitions.SourceSnapshot,
    runner_path: str,
) -> str:
    return acquisitions.sha256_file(
        acquisitions.REPOSITORY_ROOT / runner_path
    )


def write_package_output(
    root: Path,
    *,
    source_commit: str,
    marker: bytes,
) -> None:
    root.mkdir()
    binary = root / "bin/tool"
    binary.parent.mkdir()
    binary.write_bytes(b"binary:" + marker)
    binary.chmod(0o755)
    archive = root / "fixture.tar.gz"
    archive.write_bytes(b"archive:" + marker)
    provenance = {
        "cargo_lock_sha256": acquisitions.sha256_bytes(b"cargo-lock"),
        "cargo_version": "cargo 1.99.0",
        "fresh_target": True,
        "rust_toolchain_sha256": acquisitions.sha256_bytes(
            b"rust-toolchain"
        ),
        "rustc_commit": "b" * 40,
        "rustc_release": "1.99.0",
        "target_triple": "aarch64-apple-darwin",
    }
    manifest = {
        "schema_version": acquisitions.package_contract.PACKAGE_MANIFEST_SCHEMA,
        "package_version": "1.2.3",
        "source_commit": source_commit,
        "build_provenance": provenance,
        "archive": {
            "path": archive.name,
            "sha256": acquisitions.sha256_file(archive),
            "bytes": archive.stat().st_size,
        },
        "compatibility_matrix_sha256": acquisitions.sha256_bytes(b"matrix"),
        "registry_sha256": acquisitions.sha256_bytes(b"registry"),
        "third_party_licenses_sha256": acquisitions.sha256_bytes(b"licenses"),
        "godot_prerequisite": {"fixture": True},
        "contents": [
            {
                "bytes": binary.stat().st_size,
                "mode": "0755",
                "path": "bin/tool",
                "sha256": acquisitions.sha256_file(binary),
            }
        ],
    }
    (root / acquisitions.PACKAGE_MANIFEST_NAME).write_bytes(
        acquisitions.canonical_json(manifest) + b"\n"
    )


def host_measurement() -> dict[str, object]:
    profile_path = acquisitions.REPOSITORY_ROOT / acquisitions.HOST_PROFILE_PATH
    profile = json.loads(profile_path.read_text())
    measurements: list[dict[str, object]] = []
    for coordinate in profile["surfaces"]:
        surface = coordinate["surface"]

        def observed_signature(field: str) -> dict[str, object] | None:
            value = coordinate[field]
            if value is None:
                return None
            return {**value, "verified": True}

        item: dict[str, object] = {
            "surface": surface,
            "host_artifact_sha256": coordinate["host_artifact_sha256"],
            "host_metadata_sha256": coordinate["host_metadata_sha256"],
            "client_artifact_sha256": coordinate["client_artifact_sha256"],
            "ide_shell_artifact_sha256": coordinate[
                "ide_shell_artifact_sha256"
            ],
            "host_code_signature": observed_signature("host_code_signature"),
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
                "bytes": 10,
                "files": 2 if surface == "ide" else 1,
                "directories": 1 if surface == "ide" else 0,
            },
            "client_bytes": 10,
        }
        if surface == "ide":
            item["metadata_bytes"] = 10
            item["ide_shell_bytes"] = 10
        measurements.append(item)
    payload = profile_path.read_bytes()
    return {
        "schema_version": host_provenance.SCHEMA_VERSION,
        "capture_kind": host_provenance.CAPTURE_KIND,
        "status": "passed",
        "architecture": "arm64",
        "host_coordinate_profile_sha256": host_provenance.sha256_bytes(
            payload
        ),
        "surfaces": measurements,
        "redaction": {
            "absolute_paths_absent": True,
            "account_identity_absent": True,
            "artifact_content_absent": True,
            "environment_secrets_absent": True,
        },
    }


class Sprint11ExternalAcquisitionTests(unittest.TestCase):
    def test_multi_project_template_is_closed_and_uses_public_runner(self) -> None:
        self.assertEqual(
            acquisitions.multi_command_template(),
            (
                "{python}",
                "tests/codex/sprint11_multi_project.py",
                "--godot",
                "{godot}",
                "--sidecar",
                "{package_sidecar}",
                "--expected-sidecar-sha256",
                "{package_sidecar_sha256}",
                "--expected-package-version",
                "{package_version}",
                "--timeout",
                "{timeout}",
                "--report",
                "{report}",
            ),
        )

    def test_reproducibility_templates_use_only_public_build_cli(self) -> None:
        for build_id in ("a", "b"):
            template = acquisitions.reproducibility_command_template(build_id)
            self.assertEqual(template[1], "godot-codex-mcp/packaging/build_macos.py")
            self.assertEqual(template.count("--output-dir"), 1)
            self.assertIn(f"{{build_{build_id}}}", template)
            self.assertNotIn("-c", template)
            self.assertNotIn("-m", template)
        with self.assertRaises(acquisitions.AcquisitionError):
            acquisitions.reproducibility_command_template("c")

    def test_host_provenance_template_is_closed_and_public(self) -> None:
        template = acquisitions.host_provenance_command_template()
        self.assertEqual(template[1], acquisitions.HOST_RUNNER)
        self.assertEqual(
            template[3],
            "{profile}",
        )
        self.assertEqual(template.count("--output"), 1)
        self.assertNotIn("-c", template)
        self.assertNotIn("-m", template)

    def test_execute_is_bounded_and_captures_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = acquisitions.execute(
                (
                    sys.executable,
                    "-c",
                    "import sys; print('ok'); print('safe', file=sys.stderr)",
                ),
                Path(directory),
                5,
            )
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(result.stdout, b"ok\n")
        self.assertEqual(result.stderr, b"safe\n")
        self.assertGreaterEqual(result.duration_ms, 0)

    def test_execute_kills_a_timed_out_process_group(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(
                acquisitions.AcquisitionError,
                "timed out",
            ):
                acquisitions.execute(
                    (
                        sys.executable,
                        "-c",
                        "import time; time.sleep(30)",
                    ),
                    Path(directory),
                    0.05,
                )

    def test_stop_group_treats_post_exit_eperm_as_reaped_on_macos(self) -> None:
        process = mock.Mock()
        process.pid = 4242
        process.poll.side_effect = [None, 0]
        process.wait.return_value = 0
        probe_count = 0

        def killpg(_process_id: int, requested_signal: int) -> None:
            nonlocal probe_count
            if requested_signal != 0:
                return
            probe_count += 1
            if probe_count > 1:
                raise PermissionError("Darwin group probe race")

        with mock.patch.object(os, "killpg", side_effect=killpg):
            acquisitions._stop_process_group(process)
        process.wait.assert_called_once()
        process.kill.assert_not_called()

    def test_execute_rejects_orphan_descendants(self) -> None:
        child = (
            "import subprocess,sys;"
            "subprocess.Popen([sys.executable,'-c',"
            "'import time; time.sleep(30)']);"
        )
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(
                acquisitions.AcquisitionError,
                (
                    "left child processes|process group did not stop|"
                    "process group cannot be terminated"
                ),
            ):
                acquisitions.execute(
                    (sys.executable, "-c", child),
                    Path(directory),
                    5,
                )

    def test_execute_rejects_unbounded_output(self) -> None:
        started = time.monotonic()
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(
                acquisitions.AcquisitionError,
                "output exceeds byte bound",
            ):
                acquisitions.execute(
                    (
                        sys.executable,
                        "-c",
                        (
                            "import os\n"
                            "chunk = b'x' * 65536\n"
                            "while True:\n"
                            "    os.write(1, chunk)\n"
                        ),
                    ),
                    Path(directory),
                    10,
                )
        self.assertLess(time.monotonic() - started, 3)

    def test_host_execute_does_not_forward_parent_credentials(self) -> None:
        secret_name = "SPRINT11_TEST_SECRET"
        with mock.patch.dict(os.environ, {secret_name: "do-not-forward"}):
            with tempfile.TemporaryDirectory() as directory:
                result = acquisitions.execute_host(
                    (
                        sys.executable,
                        "-c",
                        (
                            "import os;"
                            f"print(os.environ.get('{secret_name}', 'absent'))"
                        ),
                    ),
                    Path(directory),
                    5,
                )
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(result.stdout, b"absent\n")

    def test_private_temporary_roots_are_canonical_and_owner_only(
        self,
    ) -> None:
        with acquisitions._safe_temporary_root(
            ".s11-canonical-test.",
        ) as directory:
            root = Path(directory)
            self.assertEqual(root, root.resolve(strict=True))
            self.assertEqual(root.parent, Path("/tmp").resolve(strict=True))
            self.assertEqual(root.stat().st_mode & 0o077, 0)
        self.assertFalse(root.exists())

    def test_execute_scrubs_secret_home_and_path_for_runner_and_child(
        self,
    ) -> None:
        secret_name = "SPRINT11_SENTINEL_SECRET"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sentinel_home = root / "sentinel-home"
            sentinel_path = root / "sentinel-bin"
            (sentinel_home / ".cargo" / "bin").mkdir(parents=True)
            sentinel_path.mkdir()
            script = (
                "import json,os,subprocess,sys;"
                "keys=('HOME','PATH','SPRINT11_SENTINEL_SECRET');"
                "child=subprocess.run("
                "[sys.executable,'-c',"
                "\"import json,os;"
                "print(json.dumps({k:os.environ.get(k) for k in "
                "('HOME','PATH','SPRINT11_SENTINEL_SECRET')}))\"],"
                "check=True,stdout=subprocess.PIPE,text=True);"
                "print(json.dumps({"
                "'runner':{k:os.environ.get(k) for k in keys},"
                "'child':json.loads(child.stdout)}))"
            )
            with mock.patch.dict(
                os.environ,
                {
                    "HOME": str(sentinel_home),
                    "PATH": str(sentinel_path),
                    secret_name: "do-not-forward",
                },
                clear=False,
            ):
                result = acquisitions.execute(
                    (sys.executable, "-c", script),
                    root,
                    5,
                )
        observed = json.loads(result.stdout)
        for process in ("runner", "child"):
            environment = observed[process]
            self.assertIsNone(environment[secret_name])
            self.assertNotEqual(environment["HOME"], str(sentinel_home))
            self.assertNotIn(str(sentinel_home), environment["PATH"])
            self.assertNotIn(str(sentinel_path), environment["PATH"])

    def test_command_record_binds_exact_runner_and_template(self) -> None:
        template = acquisitions.multi_command_template()
        execution = acquisitions.Execution(
            exit_code=0,
            stdout=b"",
            stderr=b"",
            duration_ms=1,
        )
        record = acquisitions._command_record(
            command_id="multi_project_live",
            template=template,
            runner_path=acquisitions.MULTI_RUNNER,
            runner_sha256=acquisitions.sha256_file(
                acquisitions.REPOSITORY_ROOT / acquisitions.MULTI_RUNNER
            ),
            execution=execution,
        )
        self.assertEqual(record["argv_template"], list(template))
        self.assertEqual(record["runner_path"], acquisitions.MULTI_RUNNER)
        self.assertEqual(
            record["runner_sha256"],
            acquisitions.sha256_file(
                acquisitions.REPOSITORY_ROOT / acquisitions.MULTI_RUNNER
            ),
        )

    def test_source_snapshot_uses_commit_bytes_despite_worktree_swap_restore(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory) / "repository"
            repository.mkdir()

            def git(*arguments: str) -> str:
                result = subprocess.run(
                    [
                        acquisitions.GIT_EXECUTABLE,
                        "-C",
                        str(repository),
                        *arguments,
                    ],
                    check=True,
                    stdout=subprocess.PIPE,
                    text=True,
                )
                return result.stdout.strip()

            git("init", "--quiet")
            git("config", "user.email", "fixture@example.invalid")
            git("config", "user.name", "Sprint 11 Fixture")
            runner = repository / "runner.py"
            runner.write_bytes(b"print('committed runner')\n")
            git("add", "runner.py")
            git("commit", "--quiet", "-m", "fixture")
            source_commit = git("rev-parse", "HEAD")
            committed_digest = acquisitions.sha256_file(runner)
            runner.write_bytes(b"print('swapped worktree runner')\n")
            try:
                with acquisitions.source_snapshot(
                    source_commit,
                    repository=repository,
                ) as snapshot:
                    self.assertNotEqual(snapshot.root, repository)
                    self.assertEqual(
                        (snapshot.root / "runner.py").read_bytes(),
                        b"print('committed runner')\n",
                    )
                    self.assertEqual(
                        acquisitions._snapshot_file_digest(
                            snapshot,
                            "runner.py",
                        ),
                        committed_digest,
                    )
                    mode = stat.S_IMODE(
                        (snapshot.root / "runner.py").stat().st_mode
                    )
                    self.assertEqual(mode & 0o222, 0)
                runner.write_bytes(b"print('committed runner')\n")
            finally:
                runner.write_bytes(b"print('committed runner')\n")

    def test_snapshot_runner_post_binding_rejects_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runner = root / "runner.py"
            runner.write_bytes(b"before\n")
            snapshot = acquisitions.SourceSnapshot(
                root=root,
                source_commit="a" * 40,
            )
            expected = acquisitions.sha256_file(runner)
            runner.write_bytes(b"after\n")
            with (
                mock.patch.object(
                    acquisitions,
                    "_snapshot_file_digest",
                    return_value=acquisitions.sha256_file(runner),
                ),
                self.assertRaisesRegex(
                    acquisitions.AcquisitionError,
                    "changed during acquisition",
                ),
            ):
                acquisitions._verify_snapshot_file(
                    snapshot,
                    "runner.py",
                    expected,
                )

    def test_checkout_guard_allows_only_its_private_staging_root(self) -> None:
        staging = (
            acquisitions.REPOSITORY_ROOT
            / "tests/codex/.s11-acquisition-fixture"
        )
        allowed = acquisitions.Execution(
            0,
            b"?? tests/codex/.s11-acquisition-fixture/report.json\0",
            b"",
            1,
        )
        with mock.patch.object(
            acquisitions,
            "_run_git",
            return_value=allowed,
        ):
            acquisitions.require_clean_checkout(
                allowed_untracked_roots=(staging,)
            )
        for status in (
            b" M tracked-source.py\0",
            b"?? unrelated-output.json\0",
        ):
            with (
                mock.patch.object(
                    acquisitions,
                    "_run_git",
                    return_value=acquisitions.Execution(
                        0,
                        status,
                        b"",
                        1,
                    ),
                ),
                self.assertRaisesRegex(
                    acquisitions.AcquisitionError,
                    "completely clean checkout",
                ),
            ):
                acquisitions.require_clean_checkout(
                    allowed_untracked_roots=(staging,)
                )

    def test_multi_project_acquisition_executes_full_fake_runner_and_publishes(
        self,
    ) -> None:
        from tests.codex.test_sprint11_multi_project import valid_report

        report = valid_report()
        bindings = {
            "package_source_commit": "a" * 40,
            "package_manifest_sha256": acquisitions.sha256_bytes(b"manifest"),
            "package_build_provenance_sha256": acquisitions.sha256_bytes(
                b"provenance"
            ),
            "package_archive_sha256": acquisitions.sha256_bytes(b"archive"),
            "package_sidecar_path": "bin/godot-codex-mcp",
            "package_sidecar_sha256": report["artifacts"]["sidecar_sha256"],
            "godot_version": "4.8.dev.fixture",
            "godot_commit": "b" * 40,
            "godot_artifact_sha256": report["artifacts"]["godot_sha256"],
            "registry_sha256": acquisitions.sha256_bytes(b"registry"),
        }
        observed_argv: list[tuple[str, ...]] = []

        def executor(
            argv: tuple[str, ...],
            cwd: Path,
            timeout: float,
        ) -> acquisitions.Execution:
            observed_argv.append(argv)
            self.assertEqual(cwd, acquisitions.REPOSITORY_ROOT)
            self.assertEqual(timeout, 10)
            self.assertNotIn("{profile}", argv)
            self.assertNotIn("{package_version}", argv)
            self.assertEqual(
                argv[argv.index("--expected-package-version") + 1],
                "1.2.3",
            )
            self.assertEqual(
                argv[argv.index("--expected-sidecar-sha256") + 1],
                bindings["package_sidecar_sha256"],
            )
            output = Path(argv[argv.index("--report") + 1])
            output.write_bytes(acquisitions.canonical_json(report) + b"\n")
            return acquisitions.Execution(0, b'{"status":"passed"}\n', b"", 7)

        tests_root = acquisitions.REPOSITORY_ROOT / "tests/codex"
        with tempfile.TemporaryDirectory(
            prefix=".s11-multi-test.",
            dir=tests_root,
        ) as temporary:
            root = Path(temporary)
            artifact_root = root / "package"
            artifact_root.mkdir()
            sidecar = artifact_root / "godot-codex-mcp"
            sidecar.write_bytes(b"sidecar")
            manifest = root / "manifest.json"
            manifest.write_text(
                '{"package_version":"1.2.3"}\n',
                encoding="utf-8",
            )
            godot = root / "godot"
            godot.write_bytes(b"godot")
            output = root / "acquired"
            fixture = {
                "path": "tests/codex/fixtures/transaction_prepare_project/project.godot",
                "sha256": acquisitions.sha256_file(
                    acquisitions.REPOSITORY_ROOT
                    / "tests/codex/fixtures/transaction_prepare_project/project.godot"
                ),
            }
            with (
                mock.patch.object(
                    acquisitions.package_contract,
                    "_manifest_bindings",
                    return_value=(bindings, sidecar),
                ),
                mock.patch.object(
                    acquisitions,
                    "fixture_records",
                    return_value=[fixture],
                ),
                mock.patch.object(
                    acquisitions,
                    "source_snapshot",
                    side_effect=current_source_snapshot,
                ),
                mock.patch.object(
                    acquisitions,
                    "_snapshot_file_digest",
                    side_effect=current_runner_digest,
                ),
            ):
                receipt = acquisitions.acquire_multi_project(
                    artifact_root=artifact_root,
                    package_manifest=manifest,
                    godot=godot,
                    output_root=output,
                    timeout=10,
                    executor=executor,
                    check_repository=False,
                )
            self.assertEqual(len(observed_argv), 1)
            self.assertEqual(receipt["bindings"]["package_version"], "1.2.3")
            self.assertTrue(receipt["assertions"]["isolation_matrix_complete"])
            self.assertEqual(
                json.loads((output / "report.json").read_text()),
                report,
            )
            self.assertEqual(
                json.loads((output / "receipt.json").read_text()),
                receipt,
            )
            tampered = copy.deepcopy(receipt)
            tampered["assertions"].pop("cache_rebuild_isolated")
            with self.assertRaisesRegex(
                acquisitions.AcquisitionError,
                "assertions fields differ",
            ):
                acquisitions.validate_multi_project_receipt(
                    tampered,
                    report=report,
                    expected_bindings=receipt["bindings"],
                )
            tampered_report = copy.deepcopy(report)
            tampered_report["assertions"]["fault_matrix"].pop()
            with self.assertRaisesRegex(
                acquisitions.AcquisitionError,
                "live report differs",
            ):
                acquisitions.validate_multi_project_receipt(
                    receipt,
                    report=tampered_report,
                    expected_bindings=receipt["bindings"],
                )

    def test_host_acquisition_binds_runner_schemas_profile_and_measurement(
        self,
    ) -> None:
        measurement = host_measurement()

        def executor(
            argv: tuple[str, ...],
            cwd: Path,
            _timeout: float,
        ) -> acquisitions.Execution:
            profile = Path(argv[argv.index("--profile") + 1])
            self.assertTrue(profile.is_absolute())
            self.assertEqual(
                profile,
                cwd / acquisitions.HOST_PROFILE_PATH,
            )
            output = Path(argv[argv.index("--output") + 1])
            output.write_bytes(
                host_provenance.canonical_json(measurement) + b"\n"
            )
            return acquisitions.Execution(
                exit_code=0,
                stdout=b'{"status":"passed"}\n',
                stderr=b"",
                duration_ms=4,
            )

        tests_root = acquisitions.REPOSITORY_ROOT / "tests/codex"
        with tempfile.TemporaryDirectory(
            prefix=".s11-host-test.",
            dir=tests_root,
        ) as temporary:
            output = Path(temporary) / "result"
            with (
                mock.patch.object(
                    acquisitions,
                    "repository_head",
                    return_value="a" * 40,
                ),
                mock.patch.object(
                    acquisitions,
                    "require_clean_checkout",
                ) as clean,
                mock.patch.object(
                    acquisitions,
                    "source_snapshot",
                    side_effect=current_source_snapshot,
                ),
                mock.patch.object(
                    acquisitions,
                    "_snapshot_file_digest",
                    side_effect=current_runner_digest,
                ),
            ):
                receipt = acquisitions.acquire_host_provenance(
                    app_bundle=Path("/fixture/App.app"),
                    app_executable=Path("/fixture/App"),
                    app_client=Path("/fixture/codex"),
                    vscode_bundle=Path("/fixture/Code.app"),
                    vscode_executable=Path("/fixture/Code"),
                    extension_root=Path("/fixture/extension"),
                    extension_package_json=Path("/fixture/package.json"),
                    ide_client=Path("/fixture/ide-codex"),
                    output_root=output,
                    timeout=10,
                    executor=executor,
                )
            self.assertEqual(clean.call_count, 2)
            self.assertEqual(
                receipt["schema_version"],
                acquisitions.HOST_ACQUISITION_SCHEMA,
            )
            bindings = receipt["bindings"]
            self.assertEqual(
                bindings["runner_sha256"],
                acquisitions.sha256_file(
                    acquisitions.REPOSITORY_ROOT
                    / acquisitions.HOST_RUNNER
                ),
            )
            self.assertEqual(
                bindings["measurement_schema_sha256"],
                acquisitions.sha256_file(
                    acquisitions.REPOSITORY_ROOT
                    / acquisitions.HOST_MEASUREMENT_SCHEMA_PATH
                ),
            )
            self.assertEqual(
                bindings["host_coordinate_profile_sha256"],
                measurement["host_coordinate_profile_sha256"],
            )
            self.assertEqual(
                json.loads((output / "measurement.json").read_text()),
                measurement,
            )
            self.assertEqual(
                json.loads((output / "receipt.json").read_text()),
                receipt,
            )

    def test_host_acquisition_rejects_a_failed_public_runner(self) -> None:
        def executor(
            _argv: tuple[str, ...],
            _cwd: Path,
            _timeout: float,
        ) -> acquisitions.Execution:
            return acquisitions.Execution(
                exit_code=1,
                stdout=b"",
                stderr=b"safe failure",
                duration_ms=1,
            )

        tests_root = acquisitions.REPOSITORY_ROOT / "tests/codex"
        with tempfile.TemporaryDirectory(
            prefix=".s11-host-test.",
            dir=tests_root,
        ) as temporary:
            output = Path(temporary) / "result"
            with (
                mock.patch.object(
                    acquisitions,
                    "repository_head",
                    return_value="a" * 40,
                ),
                mock.patch.object(
                    acquisitions,
                    "require_clean_checkout",
                ),
                mock.patch.object(
                    acquisitions,
                    "source_snapshot",
                    side_effect=current_source_snapshot,
                ),
                mock.patch.object(
                    acquisitions,
                    "_snapshot_file_digest",
                    side_effect=current_runner_digest,
                ),
            ):
                with self.assertRaisesRegex(
                    acquisitions.AcquisitionError,
                    "runner failed",
                ):
                    acquisitions.acquire_host_provenance(
                        app_bundle=Path("/fixture/App.app"),
                        app_executable=Path("/fixture/App"),
                        app_client=Path("/fixture/codex"),
                        vscode_bundle=Path("/fixture/Code.app"),
                        vscode_executable=Path("/fixture/Code"),
                        extension_root=Path("/fixture/extension"),
                        extension_package_json=Path("/fixture/package.json"),
                        ide_client=Path("/fixture/ide-codex"),
                        output_root=output,
                        timeout=10,
                        executor=executor,
                    )
            self.assertFalse(output.exists())

    def test_host_acquisition_rejects_a_tampered_measurement(self) -> None:
        measurement = host_measurement()
        tampered = copy.deepcopy(measurement)
        tampered["surfaces"][2]["host_artifact_sha256"] = "sha256:" + "f" * 64

        def executor(
            argv: tuple[str, ...],
            _cwd: Path,
            _timeout: float,
        ) -> acquisitions.Execution:
            output = Path(argv[argv.index("--output") + 1])
            output.write_bytes(host_provenance.canonical_json(tampered) + b"\n")
            return acquisitions.Execution(0, b"", b"", 1)

        tests_root = acquisitions.REPOSITORY_ROOT / "tests/codex"
        with tempfile.TemporaryDirectory(
            prefix=".s11-host-test.",
            dir=tests_root,
        ) as temporary:
            output = Path(temporary) / "result"
            with (
                mock.patch.object(
                    acquisitions,
                    "repository_head",
                    return_value="a" * 40,
                ),
                mock.patch.object(
                    acquisitions,
                    "require_clean_checkout",
                ),
                mock.patch.object(
                    acquisitions,
                    "source_snapshot",
                    side_effect=current_source_snapshot,
                ),
                mock.patch.object(
                    acquisitions,
                    "_snapshot_file_digest",
                    side_effect=current_runner_digest,
                ),
            ):
                with self.assertRaisesRegex(
                    acquisitions.AcquisitionError,
                    "measurement differs",
                ):
                    acquisitions.acquire_host_provenance(
                        app_bundle=Path("/fixture/App.app"),
                        app_executable=Path("/fixture/App"),
                        app_client=Path("/fixture/codex"),
                        vscode_bundle=Path("/fixture/Code.app"),
                        vscode_executable=Path("/fixture/Code"),
                        extension_root=Path("/fixture/extension"),
                        extension_package_json=Path("/fixture/package.json"),
                        ide_client=Path("/fixture/ide-codex"),
                        output_root=output,
                        timeout=10,
                        executor=executor,
                    )
            self.assertFalse(output.exists())

    def test_reproducibility_binds_rebuilds_to_qualified_package(self) -> None:
        source_commit = "a" * 40
        tests_root = acquisitions.REPOSITORY_ROOT / "tests/codex"
        with (
            tempfile.TemporaryDirectory() as external,
            tempfile.TemporaryDirectory(
                prefix=".s11-repro-test.",
                dir=tests_root,
            ) as repository_local,
        ):
            external_root = Path(external)
            qualified = external_root / "qualified"
            write_package_output(
                qualified,
                source_commit=source_commit,
                marker=b"qualified",
            )
            godot = external_root / "godot"
            godot.write_bytes(b"godot")
            build_root = external_root / "build"
            output = Path(repository_local) / "result"

            def executor(
                argv: tuple[str, ...],
                _cwd: Path,
                _timeout: float,
            ) -> acquisitions.Execution:
                destination = Path(argv[argv.index("--output-dir") + 1])
                shutil.copytree(qualified, destination)
                return acquisitions.Execution(0, b'{"status":"passed"}\n', b"", 2)

            with (
                mock.patch.object(
                    acquisitions,
                    "repository_head",
                    return_value=source_commit,
                ),
                mock.patch.object(acquisitions, "require_clean_checkout"),
                mock.patch.object(
                    acquisitions,
                    "source_snapshot",
                    side_effect=current_source_snapshot,
                ),
                mock.patch.object(
                    acquisitions,
                    "_snapshot_file_digest",
                    side_effect=current_runner_digest,
                ),
            ):
                receipt = acquisitions.acquire_reproducibility(
                    qualified_artifact_root=qualified,
                    godot_prerequisite=godot,
                    build_root=build_root,
                    output_root=output,
                    timeout=10,
                    executor=executor,
                )
            qualified_record = acquisitions._build_output_record(
                qualified,
                "qualified",
                expected_source_commit=source_commit,
            )
            self.assertEqual(
                receipt["bindings"]["qualified_manifest_sha256"],
                qualified_record["manifest_sha256"],
            )
            self.assertEqual(
                receipt["bindings"]["qualified_archive_sha256"],
                qualified_record["archive_sha256"],
            )
            self.assertEqual(
                receipt["bindings"]["qualified_archive_bytes"],
                qualified_record["archive_bytes"],
            )
            self.assertEqual(
                receipt["bindings"]["qualified_package_tree_sha256"],
                qualified_record["package_tree_sha256"],
            )
            self.assertEqual(
                receipt["bindings"][
                    "qualified_build_provenance_sha256"
                ],
                qualified_record["build_provenance_sha256"],
            )

    def test_reproducibility_rejects_equal_rebuilds_that_differ_from_qualified(
        self,
    ) -> None:
        source_commit = "a" * 40
        tests_root = acquisitions.REPOSITORY_ROOT / "tests/codex"
        with (
            tempfile.TemporaryDirectory() as external,
            tempfile.TemporaryDirectory(
                prefix=".s11-repro-test.",
                dir=tests_root,
            ) as repository_local,
        ):
            external_root = Path(external)
            qualified = external_root / "qualified"
            rebuilt = external_root / "rebuilt"
            write_package_output(
                qualified,
                source_commit=source_commit,
                marker=b"qualified",
            )
            write_package_output(
                rebuilt,
                source_commit=source_commit,
                marker=b"different-but-repeatable",
            )
            godot = external_root / "godot"
            godot.write_bytes(b"godot")

            def executor(
                argv: tuple[str, ...],
                _cwd: Path,
                _timeout: float,
            ) -> acquisitions.Execution:
                destination = Path(argv[argv.index("--output-dir") + 1])
                shutil.copytree(rebuilt, destination)
                return acquisitions.Execution(0, b"", b"", 2)

            with (
                mock.patch.object(
                    acquisitions,
                    "repository_head",
                    return_value=source_commit,
                ),
                mock.patch.object(acquisitions, "require_clean_checkout"),
                mock.patch.object(
                    acquisitions,
                    "source_snapshot",
                    side_effect=current_source_snapshot,
                ),
                mock.patch.object(
                    acquisitions,
                    "_snapshot_file_digest",
                    side_effect=current_runner_digest,
                ),
                self.assertRaisesRegex(
                    acquisitions.AcquisitionError,
                    "differ from qualified release package",
                ),
            ):
                acquisitions.acquire_reproducibility(
                    qualified_artifact_root=qualified,
                    godot_prerequisite=godot,
                    build_root=external_root / "build",
                    output_root=Path(repository_local) / "result",
                    timeout=10,
                    executor=executor,
                )


if __name__ == "__main__":
    unittest.main()
