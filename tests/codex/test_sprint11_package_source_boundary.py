from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tests.codex import sprint11_acceptance as acceptance
from tests.codex import sprint11_external_acquisitions as acquisitions
from tests.codex import sprint11_packaged_regressions as packaged

SYSTEM_PATH = "/usr/bin:/bin:/usr/sbin:/sbin:/Library/Apple/usr/bin"


def _git(repository: Path, *arguments: str) -> str:
    environment = {
        "GIT_AUTHOR_EMAIL": "s11@example.invalid",
        "GIT_AUTHOR_NAME": "Sprint 11",
        "GIT_COMMITTER_EMAIL": "s11@example.invalid",
        "GIT_COMMITTER_NAME": "Sprint 11",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "HOME": "/var/empty",
        "LANG": "C",
        "LC_ALL": "C",
        "PATH": "/usr/bin:/bin",
    }
    result = subprocess.run(
        [packaged.GIT_EXECUTABLE, "-C", repository, *arguments],
        check=True,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return result.stdout.strip()


@unittest.skipUnless(
    os.name == "posix" and Path("/usr/bin/git").is_file(),
    "fixed system Git and POSIX worktrees are required",
)
class PackageSourceBoundaryTests(unittest.TestCase):
    def test_git_environment_preserves_only_valid_scope_markers(self) -> None:
        marker = packaged.process_scope.SCOPE_ENVIRONMENT_PREFIX + "a" * 48
        with mock.patch.dict(
            os.environ,
            {
                marker: "1",
                "SPRINT11_SENTINEL_SECRET": "secret",
            },
            clear=False,
        ):
            environment = packaged._git_environment()
        self.assertEqual(environment[marker], "1")
        self.assertEqual(environment["GIT_NO_LAZY_FETCH"], "1")
        self.assertNotIn("SPRINT11_SENTINEL_SECRET", environment)

        with (
            mock.patch.dict(
                os.environ,
                {
                    packaged.process_scope.SCOPE_ENVIRONMENT_PREFIX
                    + "invalid": "1"
                },
                clear=False,
            ),
            self.assertRaisesRegex(
                packaged.PackagedRegressionError,
                "process scope environment differs",
            ),
        ):
            packaged._git_environment()

    def test_snapshot_uses_commit_bytes_for_dirty_imports_and_fixture_scripts(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-source-boundary."
        ) as temporary:
            repository = Path(temporary) / "repository"
            repository.mkdir()
            _git(repository, "init", "--quiet")
            helper = repository / "tests/codex/imported_helper.py"
            fixture = repository / "tests/codex/fixtures/oracle/main.gd"
            helper.parent.mkdir(parents=True)
            fixture.parent.mkdir(parents=True)
            helper.write_text("VALUE = 'committed'\n", encoding="utf-8")
            fixture.write_text("extends Node\n# committed\n", encoding="utf-8")
            _git(repository, "add", "--all")
            _git(repository, "commit", "--quiet", "-m", "fixture")
            source_commit = _git(repository, "rev-parse", "HEAD")

            helper.write_text("VALUE = 'dirty'\n", encoding="utf-8")
            fixture.write_text("extends Node\n# dirty\n", encoding="utf-8")

            with packaged.source_snapshot(
                source_commit,
                repository=repository,
            ) as snapshot:
                self.assertNotEqual(snapshot.root, repository)
                self.assertEqual(
                    (
                        snapshot.root / "tests/codex/imported_helper.py"
                    ).read_text(encoding="utf-8"),
                    "VALUE = 'committed'\n",
                )
                self.assertEqual(
                    (
                        snapshot.root / "tests/codex/fixtures/oracle/main.gd"
                    ).read_text(encoding="utf-8"),
                    "extends Node\n# committed\n",
                )
                self.assertFalse((snapshot.root / ".git").exists())
                packaged._verify_source_snapshot(snapshot)

    def test_fixed_git_ignores_hostile_path_and_repository_redirection(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-hostile-git."
        ) as temporary:
            root = Path(temporary)
            repository = root / "repository"
            repository.mkdir()
            _git(repository, "init", "--quiet")
            (repository / "tracked").write_text("committed\n", encoding="utf-8")
            _git(repository, "add", "--all")
            _git(repository, "commit", "--quiet", "-m", "fixture")
            expected = _git(repository, "rev-parse", "HEAD")

            fake_bin = root / "bin"
            fake_bin.mkdir()
            marker = root / "fake-git-ran"
            fake_git = fake_bin / "git"
            fake_git.write_text(
                "#!/bin/sh\n"
                f"touch {marker}\n"
                "printf '%040d\\n' 0\n",
                encoding="utf-8",
            )
            fake_git.chmod(0o755)
            hostile = {
                "GIT_CONFIG_GLOBAL": str(root / "hostile.gitconfig"),
                "GIT_DIR": str(root / "redirected.git"),
                "GIT_WORK_TREE": str(root / "redirected-worktree"),
                "PATH": str(fake_bin),
            }
            with (
                mock.patch.dict(os.environ, hostile, clear=False),
                mock.patch.object(packaged, "REPOSITORY_ROOT", repository),
            ):
                actual = packaged.repository_head()
            self.assertEqual(actual, expected)
            self.assertFalse(marker.exists())

    def test_final_validator_uses_the_same_fixed_git_boundary(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-validator-git."
        ) as temporary:
            root = Path(temporary)
            repository = root / "repository"
            repository.mkdir()
            _git(repository, "init", "--quiet")
            (repository / "tracked").write_text("committed\n", encoding="utf-8")
            _git(repository, "add", "--all")
            _git(repository, "commit", "--quiet", "-m", "fixture")
            expected = _git(repository, "rev-parse", "HEAD")
            fake_bin = root / "bin"
            fake_bin.mkdir()
            marker = root / "fake-git-ran"
            fake_git = fake_bin / "git"
            fake_git.write_text(
                "#!/bin/sh\n"
                f"touch {marker}\n"
                "printf '%040d\\n' 0\n",
                encoding="utf-8",
            )
            fake_git.chmod(0o755)
            with mock.patch.dict(
                os.environ,
                {
                    "GIT_DIR": str(root / "redirected.git"),
                    "GIT_WORK_TREE": str(root / "redirected-worktree"),
                    "PATH": str(fake_bin),
                },
                clear=False,
            ):
                actual = acceptance.GitRepository(repository).head()
            self.assertEqual(actual, expected)
            self.assertFalse(marker.exists())

    def test_final_validator_ignores_legacy_git_grafts(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-validator-graft."
        ) as temporary:
            repository = Path(temporary) / "repository"
            repository.mkdir()
            _git(repository, "init", "--quiet")
            tracked = repository / "tracked"
            tracked.write_text("line-a\n", encoding="utf-8")
            _git(repository, "add", "tracked")
            _git(repository, "commit", "--quiet", "-m", "lineage a")
            commit_a = _git(repository, "rev-parse", "HEAD")

            _git(repository, "switch", "--quiet", "--orphan", "lineage-b")
            tracked.write_text("line-b1\n", encoding="utf-8")
            _git(repository, "add", "tracked")
            _git(repository, "commit", "--quiet", "-m", "lineage b1")
            commit_b1 = _git(repository, "rev-parse", "HEAD")
            tracked.write_text("line-b2\n", encoding="utf-8")
            _git(repository, "add", "tracked")
            _git(repository, "commit", "--quiet", "-m", "lineage b2")
            commit_b2 = _git(repository, "rev-parse", "HEAD")

            git_directory = Path(
                _git(repository, "rev-parse", "--absolute-git-dir")
            )
            grafts = git_directory / "info/grafts"
            grafts.parent.mkdir(parents=True, exist_ok=True)
            grafts.write_text(
                f"{commit_b2} {commit_a}\n",
                encoding="ascii",
            )
            self.assertEqual(
                subprocess.run(
                    [
                        packaged.GIT_EXECUTABLE,
                        "-C",
                        repository,
                        "merge-base",
                        "--is-ancestor",
                        commit_a,
                        commit_b2,
                    ],
                    check=False,
                    env=packaged._git_environment(),
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                ).returncode,
                0,
            )

            validator = acceptance.GitRepository(repository)
            self.assertEqual(validator.parent(commit_b2), commit_b1)
            self.assertFalse(validator.is_ancestor(commit_a, commit_b2))

    def test_snapshot_disables_git_replace_object_substitution(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-replace-object."
        ) as temporary:
            repository = Path(temporary) / "repository"
            repository.mkdir()
            _git(repository, "init", "--quiet")
            runner = repository / "runner.py"
            runner.write_text("SOURCE = 'commit-a'\n", encoding="utf-8")
            _git(repository, "add", "runner.py")
            _git(repository, "commit", "--quiet", "-m", "commit a")
            commit_a = _git(repository, "rev-parse", "HEAD")

            runner.write_text("SOURCE = 'commit-b'\n", encoding="utf-8")
            _git(repository, "add", "runner.py")
            _git(repository, "commit", "--quiet", "-m", "commit b")
            commit_b = _git(repository, "rev-parse", "HEAD")
            _git(repository, "replace", commit_a, commit_b)
            self.assertEqual(
                _git(repository, "show", f"{commit_a}:runner.py"),
                "SOURCE = 'commit-b'",
            )

            with packaged.source_snapshot(
                commit_a,
                repository=repository,
            ) as snapshot:
                self.assertEqual(
                    (snapshot.root / "runner.py").read_text(
                        encoding="utf-8"
                    ),
                    "SOURCE = 'commit-a'\n",
                )

    def test_snapshot_does_not_execute_repository_smudge_filters(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-smudge-filter."
        ) as temporary:
            root = Path(temporary)
            repository = root / "repository"
            repository.mkdir()
            _git(repository, "init", "--quiet")
            (repository / ".gitattributes").write_text(
                "runner.py filter=s11evil\n",
                encoding="utf-8",
            )
            (repository / "runner.py").write_text(
                "SOURCE = 'declared'\n",
                encoding="utf-8",
            )
            _git(repository, "add", "--all")
            _git(repository, "commit", "--quiet", "-m", "fixture")
            source_commit = _git(repository, "rev-parse", "HEAD")
            marker = root / "smudge-executed"
            smudge = root / "smudge.sh"
            smudge.write_text(
                "#!/bin/sh\n"
                f"touch {marker}\n"
                "sed \"s/declared/replacement/g\"\n",
                encoding="utf-8",
            )
            smudge.chmod(0o755)
            _git(
                repository,
                "config",
                "filter.s11evil.smudge",
                str(smudge),
            )
            _git(repository, "config", "filter.s11evil.clean", "cat")

            with packaged.source_snapshot(
                source_commit,
                repository=repository,
            ) as snapshot:
                self.assertEqual(
                    (snapshot.root / "runner.py").read_text(
                        encoding="utf-8"
                    ),
                    "SOURCE = 'declared'\n",
                )
            self.assertFalse(marker.exists())

    def test_snapshot_rejects_tracked_absolute_and_parent_symlinks(self) -> None:
        for target in ("/tmp/s11-outside.py", "../../s11-outside.py"):
            with self.subTest(target=target), tempfile.TemporaryDirectory(
                prefix="s11-source-symlink."
            ) as temporary:
                repository = Path(temporary) / "repository"
                repository.mkdir()
                _git(repository, "init", "--quiet")
                os.symlink(target, repository / "helper.py")
                (repository / "runner.py").write_text(
                    "import helper\n",
                    encoding="utf-8",
                )
                _git(repository, "add", "--all")
                _git(repository, "commit", "--quiet", "-m", "fixture")
                source_commit = _git(repository, "rev-parse", "HEAD")
                with self.assertRaisesRegex(
                    packaged.PackagedRegressionError,
                    "link, submodule, or unsupported entry",
                ):
                    with packaged.source_snapshot(
                        source_commit,
                        repository=repository,
                    ):
                        self.fail("unsafe source snapshot was accepted")

    def test_package_live_environment_excludes_account_toolchain_state(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-live-environment."
        ) as temporary:
            root = Path(temporary)
            account = root / "account"
            cargo = account / ".cargo"
            rustup = account / ".rustup"
            (cargo / "bin").mkdir(parents=True)
            rustup.mkdir(parents=True)
            (cargo / "credentials.toml").write_text(
                "[registry]\ntoken='secret-canary'\n",
                encoding="utf-8",
            )
            (cargo / "bin/ps").write_text(
                "#!/bin/sh\nexit 99\n",
                encoding="utf-8",
            )
            inherited = {
                "CARGO_HOME": str(cargo),
                "DEVELOPER_DIR": str(account / "developer"),
                "MACOSX_DEPLOYMENT_TARGET": "99.0",
                "RUSTUP_HOME": str(rustup),
                "SDKROOT": str(account / "sdk"),
                "PATH": str(cargo / "bin"),
            }
            (root / "run").mkdir()
            with mock.patch.dict(os.environ, inherited, clear=True):
                environment = packaged._package_live_environment(root / "run")
            self.assertEqual(environment["PATH"], SYSTEM_PATH)
            self.assertNotIn("CARGO_HOME", environment)
            self.assertNotIn("DEVELOPER_DIR", environment)
            self.assertNotIn("MACOSX_DEPLOYMENT_TARGET", environment)
            self.assertNotIn("RUSTUP_HOME", environment)
            self.assertNotIn("SDKROOT", environment)
            self.assertNotIn("secret-canary", repr(environment))

    def test_external_multi_executor_uses_no_toolchain_environment(self) -> None:
        captured: dict[str, str] = {}

        def execute(
            _argv: tuple[str, ...],
            _cwd: Path,
            _timeout: float,
            *,
            environment: dict[str, str],
        ) -> acquisitions.Execution:
            captured.update(environment)
            return acquisitions.Execution(0, b"", b"", 1)

        with (
            mock.patch.dict(
                os.environ,
                {
                    "DEVELOPER_DIR": "/tmp/hostile-developer",
                    "MACOSX_DEPLOYMENT_TARGET": "99.0",
                    "SDKROOT": "/tmp/hostile-sdk",
                },
                clear=False,
            ),
            mock.patch.object(acquisitions, "_execute", side_effect=execute),
            mock.patch.object(
                acquisitions,
                "_tool_home",
                side_effect=AssertionError("toolchain lookup is forbidden"),
            ),
        ):
            acquisitions.execute_package_live(
                ("/usr/bin/true",),
                packaged.REPOSITORY_ROOT,
                5.0,
            )
        self.assertEqual(captured["PATH"], SYSTEM_PATH)
        self.assertNotIn("CARGO_HOME", captured)
        self.assertNotIn("DEVELOPER_DIR", captured)
        self.assertNotIn("MACOSX_DEPLOYMENT_TARGET", captured)
        self.assertNotIn("RUSTUP_HOME", captured)
        self.assertNotIn("SDKROOT", captured)


if __name__ == "__main__":
    unittest.main()
