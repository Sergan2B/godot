from __future__ import annotations

import importlib.util
import io
import json
import os
import shutil
import signal
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from argparse import Namespace
from pathlib import Path, PurePosixPath
from unittest import mock

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
BUILDER_PATH = (
    REPOSITORY_ROOT / "godot-codex-mcp" / "packaging" / "build_macos.py"
)
INSTALLER_PATH = (
    REPOSITORY_ROOT / "godot-codex-mcp" / "packaging" / "install.sh"
)
LICENSE_GENERATOR_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "packaging"
    / "generate_third_party_licenses.py"
)
THIRD_PARTY_LICENSES_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "product"
    / "THIRD_PARTY_LICENSES.txt"
)
PRODUCT_ROOT = REPOSITORY_ROOT / "godot-codex-mcp" / "product"
COMPATIBILITY_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "compatibility-matrix.schema.json"
)
HOST_PROFILE_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "host-coordinate-profile.schema.json"
)
DOCTOR_REPORT_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "doctor-report.schema.json"
)
MULTI_PROJECT_REPORT_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-multi-project-report.schema.json"
)
MULTI_PROJECT_RECEIPT_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-multi-project-receipt.schema.json"
)
SURFACE_AUTHORITY_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-surface-acquisition-authority.schema.json"
)

SPEC = importlib.util.spec_from_file_location("sprint11_package_builder", BUILDER_PATH)
assert SPEC is not None and SPEC.loader is not None
package_builder = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = package_builder
SPEC.loader.exec_module(package_builder)

LICENSE_SPEC = importlib.util.spec_from_file_location(
    "sprint11_license_generator",
    LICENSE_GENERATOR_PATH,
)
assert LICENSE_SPEC is not None and LICENSE_SPEC.loader is not None
license_generator = importlib.util.module_from_spec(LICENSE_SPEC)
sys.modules[LICENSE_SPEC.name] = license_generator
LICENSE_SPEC.loader.exec_module(license_generator)

ACCEPTANCE_PATH = REPOSITORY_ROOT / "tests" / "codex" / "sprint11_acceptance.py"
ACCEPTANCE_SPEC = importlib.util.spec_from_file_location(
    "sprint11_acceptance_for_package",
    ACCEPTANCE_PATH,
)
assert ACCEPTANCE_SPEC is not None and ACCEPTANCE_SPEC.loader is not None
sprint11_acceptance = importlib.util.module_from_spec(ACCEPTANCE_SPEC)
sys.modules[ACCEPTANCE_SPEC.name] = sprint11_acceptance
ACCEPTANCE_SPEC.loader.exec_module(sprint11_acceptance)

SOURCE_COMMIT = "a" * 40
GODOT_HASH = "907001ec5c88b11173795859f9cb3c859b6ac4b5bc862da9ace92b6dd016f401"


def thin_arm64_macho(payload: bytes) -> bytes:
    return b"\xcf\xfa\xed\xfe" + struct.pack("<I", 0x0100000C) + payload


def write(path: Path, content: bytes, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    path.chmod(mode)


def rebind_fixture_host_profile(workspace: Path) -> None:
    matrix_content = (
        workspace / "product" / "compatibility-matrix.v1.json"
    ).read_bytes()
    instructions_content = (
        workspace / "product" / "server-instructions.v1.txt"
    ).read_bytes()
    host_profile = json.loads(
        (workspace / "product" / "host-coordinate-profile.v1.json").read_bytes()
    )
    host_profile["compatibility_matrix"]["sha256"] = (
        f"sha256:{package_builder.sha256_bytes(matrix_content)}"
    )
    host_profile["server_instructions"]["file_sha256"] = (
        f"sha256:{package_builder.sha256_bytes(instructions_content)}"
    )
    wire_content = (
        instructions_content.decode("utf-8").rstrip("\n").encode("utf-8")
    )
    host_profile["server_instructions"]["wire_sha256"] = (
        f"sha256:{package_builder.sha256_bytes(wire_content)}"
    )
    write(
        workspace / "product" / "host-coordinate-profile.v1.json",
        package_builder.canonical_json(host_profile),
    )


def package_fixture(root: Path, *, version: str = "0.1.0") -> tuple[Path, Path]:
    repository = root / "Source Tree Ω"
    workspace = repository / "godot-codex-mcp"
    matrix = json.loads(
        (PRODUCT_ROOT / "compatibility-matrix.v1.json").read_bytes()
    )
    matrix["package"]["version"] = version
    matrix["godot"]["artifact_sha256"] = GODOT_HASH
    matrix["godot"]["build_id"] = "4.8.dev.codex.11785494e"
    matrix["godot"]["source_commit"] = (
        "11785494ee4ac0a1e53cbefac59d713703b82e17"
    )
    matrix_content = package_builder.canonical_json(matrix)
    write(
        workspace / "product" / "compatibility-matrix.v1.json",
        matrix_content,
    )
    write(
        workspace / "product" / "registry-profile.v1.json",
        (PRODUCT_ROOT / "registry-profile.v1.json").read_bytes(),
    )
    write(
        workspace / "product" / "THIRD_PARTY_LICENSES.txt",
        b"Fixture third-party license bundle\n",
    )
    for product_path in sorted(PRODUCT_ROOT.iterdir()):
        if (
            product_path.is_file()
            and product_path.name
            not in {
                "THIRD_PARTY_LICENSES.txt",
                "compatibility-matrix.v1.json",
                "registry-profile.v1.json",
            }
        ):
            write(
                workspace / "product" / product_path.name,
                product_path.read_bytes(),
            )
    rebind_fixture_host_profile(workspace)
    write(
        workspace
        / "schemas"
        / "godot_codex"
        / "compatibility-matrix.schema.json",
        COMPATIBILITY_SCHEMA_PATH.read_bytes(),
    )
    write(
        workspace
        / "schemas"
        / "godot_codex"
        / "host-coordinate-profile.schema.json",
        HOST_PROFILE_SCHEMA_PATH.read_bytes(),
    )
    write(
        workspace
        / "schemas"
        / "godot_codex"
        / "doctor-report.schema.json",
        DOCTOR_REPORT_SCHEMA_PATH.read_bytes(),
    )
    write(
        workspace
        / "schemas"
        / "godot_codex"
        / "sprint11-multi-project-report.schema.json",
        MULTI_PROJECT_REPORT_SCHEMA_PATH.read_bytes(),
    )
    write(
        workspace
        / "schemas"
        / "godot_codex"
        / "sprint11-multi-project-receipt.schema.json",
        MULTI_PROJECT_RECEIPT_SCHEMA_PATH.read_bytes(),
    )
    write(
        workspace
        / "schemas"
        / "godot_codex"
        / "sprint11-surface-acquisition-authority.schema.json",
        SURFACE_AUTHORITY_SCHEMA_PATH.read_bytes(),
    )
    write(
        workspace / "schemas" / "godot_codex" / "fixture.schema.json",
        b'{"type":"object"}\n',
    )
    write(
        workspace / "Cargo.toml",
        (
            "[workspace]\n"
            "resolver = \"3\"\n"
            "[workspace.package]\n"
            f"version = \"{version}\"\n"
        ).encode(),
    )
    write(workspace / "Cargo.lock", b"# fixture lock\n")
    write(
        workspace / "rust-toolchain.toml",
        (
            REPOSITORY_ROOT
            / "godot-codex-mcp"
            / "rust-toolchain.toml"
        ).read_bytes(),
    )
    write(workspace / ".gitignore", b"/target/\n")
    write(
        workspace / "target" / "release" / "godot-codex",
        thin_arm64_macho(b"ignored operations"),
        0o755,
    )
    write(
        workspace / "target" / "release" / "godot-codex-mcp",
        thin_arm64_macho(b"ignored sidecar"),
        0o755,
    )
    write(workspace / "packaging" / "install.sh", INSTALLER_PATH.read_bytes(), 0o755)
    write(
        workspace / "packaging" / "generate_third_party_licenses.py",
        LICENSE_GENERATOR_PATH.read_bytes(),
        0o755,
    )
    write(repository / ".codex" / "config.toml.example", b"[mcp_servers.godot_editor]\n")
    write(
        repository
        / "docs"
        / "codex-integration"
        / "templates"
        / "AGENTS.godot.md",
        b"# fixture\n",
    )
    write(
        repository
        / "docs"
        / "codex-integration"
        / "EXTERNAL-CODEX-BETA-GUIDE.md",
        b"# fixture guide\n",
    )
    write(
        repository / ".agents" / "skills" / "godot-editor" / "SKILL.md",
        b"---\nname: godot-editor\ndescription: fixture\n---\n",
    )
    write(
        repository
        / ".agents"
        / "skills"
        / "godot-editor"
        / "agents"
        / "openai.yaml",
        b'interface:\n  display_name: "Fixture"\n',
    )
    write(repository / "LICENSE.txt", b"Fixture package license\n")
    prerequisite = root / "Godot Bridge Ω"
    write(prerequisite, thin_arm64_macho(b"godot fixture"), 0o755)
    return repository, prerequisite


def build_fixture(
    repository: Path,
    prerequisite: Path,
    output: Path,
) -> tuple[Path, Path]:
    args = Namespace(
        repository_root=repository,
        workspace=repository / "godot-codex-mcp",
        output_dir=output,
        source_commit=SOURCE_COMMIT,
        godot_prerequisite=prerequisite,
    )
    godot = {
        "architecture": "arm64",
        "commit": "11785494ee4ac0a1e53cbefac59d713703b82e17",
        "expected_install_path": package_builder.GODOT_EXPECTED_INSTALL_PATH,
        "sha256": f"sha256:{GODOT_HASH}",
        "verification": package_builder.GODOT_VERIFICATION,
        "version": "4.8.dev.codex.11785494e",
    }
    provenance = {
        "cargo_lock_sha256": "sha256:" + "1" * 64,
        "cargo_version": "cargo 1.94.1 (fixture)",
        "fresh_target": True,
        "rust_toolchain_sha256": "sha256:" + "2" * 64,
        "rustc_commit": "3" * 40,
        "rustc_release": "1.94.1",
        "target_triple": "aarch64-apple-darwin",
    }
    binaries = {
        "godot-codex": thin_arm64_macho(b"operations"),
        "godot-codex-mcp": thin_arm64_macho(b"sidecar"),
    }

    def copy_snapshot(
        source: Path,
        _commit: str,
        destination: Path,
    ) -> None:
        shutil.copytree(source, destination, symlinks=True)

    with (
        mock.patch.object(package_builder, "verify_source_checkout"),
        mock.patch.object(
            package_builder,
            "snapshot_source_checkout",
            side_effect=copy_snapshot,
        ),
        mock.patch.object(
            package_builder,
            "verify_third_party_license_bundle",
            return_value=(
                repository
                / "godot-codex-mcp"
                / "product"
                / "THIRD_PARTY_LICENSES.txt"
            ).read_bytes(),
        ),
        mock.patch.object(
            package_builder,
            "collect_build_provenance",
            return_value=provenance,
        ),
        mock.patch.object(
            package_builder,
            "build_release_binaries",
            return_value=binaries,
        ),
        mock.patch.object(
            package_builder,
            "verify_godot_prerequisite",
            return_value=godot,
        ),
    ):
        return package_builder.build(args)


def extract_fixture(archive: Path, destination: Path) -> Path:
    destination.mkdir(parents=True)
    with tarfile.open(archive, mode="r:gz") as package:
        members = package.getmembers()
        if not members:
            raise AssertionError("fixture archive is empty")
        root = PurePosixPath(members[0].name).parts[0]
        if any(
            Path(member.name).is_absolute()
            or ".." in Path(member.name).parts
            or PurePosixPath(member.name).parts[0] != root
            for member in members
        ):
            raise AssertionError("fixture archive path is unsafe")
        package.extractall(destination)
    return destination / root


class Sprint11PackageTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="s11-package-test-")
        self.root = Path(self.temporary.name)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_package_requires_the_exact_clean_git_checkout(self) -> None:
        repository = self.root / "source checkout"
        repository.mkdir()
        subprocess.run(["git", "init", "-q", repository], check=True)
        subprocess.run(
            ["git", "-C", repository, "config", "user.name", "Sprint 11 Test"],
            check=True,
        )
        subprocess.run(
            [
                "git",
                "-C",
                repository,
                "config",
                "user.email",
                "sprint11@example.invalid",
            ],
            check=True,
        )
        tracked = repository / "tracked.txt"
        tracked.write_text("clean\n", encoding="utf-8")
        subprocess.run(["git", "-C", repository, "add", "tracked.txt"], check=True)
        subprocess.run(
            ["git", "-C", repository, "commit", "-q", "-m", "fixture"],
            check=True,
        )
        source_commit = (
            subprocess.run(
                ["git", "-C", repository, "rev-parse", "HEAD"],
                check=True,
                capture_output=True,
                text=True,
            )
            .stdout.strip()
        )

        package_builder.verify_source_checkout(repository.resolve(), source_commit)
        with self.assertRaises(package_builder.PackageError):
            package_builder.verify_source_checkout(repository.resolve(), "a" * 40)

        git_directory = Path(
            subprocess.run(
                ["git", "-C", repository, "rev-parse", "--absolute-git-dir"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
        )
        for relative in (
            Path("info/attributes"),
            Path("info/grafts"),
            Path("objects/info/alternates"),
        ):
            with self.subTest(local_git_overlay=relative.as_posix()):
                overlay = git_directory / relative
                overlay.parent.mkdir(parents=True, exist_ok=True)
                overlay.write_text("unsafe local overlay\n", encoding="utf-8")
                with self.assertRaises(package_builder.PackageError):
                    package_builder.verify_source_checkout(
                        repository.resolve(),
                        source_commit,
                    )
                overlay.unlink()

        with mock.patch.dict(
            os.environ,
            {
                "GIT_CONFIG_GLOBAL": str(self.root / "hostile-git-config"),
                "GIT_DIR": str(self.root / "hostile-git-dir"),
                "GIT_WORK_TREE": str(self.root / "hostile-work-tree"),
                "PATH": str(self.root / "hostile-path"),
            },
            clear=False,
        ):
            package_builder.verify_source_checkout(
                repository.resolve(),
                source_commit,
            )

        index_path = git_directory / "index"
        index_before = index_path.read_bytes()
        tracked_metadata = tracked.stat()
        os.utime(
            tracked,
            ns=(
                tracked_metadata.st_atime_ns,
                tracked_metadata.st_mtime_ns + 10_000_000_000,
            ),
        )
        package_builder.verify_source_checkout(
            repository.resolve(),
            source_commit,
        )
        self.assertEqual(index_path.read_bytes(), index_before)

        (repository / "untracked.txt").write_text("dirty\n", encoding="utf-8")
        with self.assertRaises(package_builder.PackageError):
            package_builder.verify_source_checkout(repository.resolve(), source_commit)

    def test_source_snapshot_is_commit_bound_and_excludes_ignored_targets(
        self,
    ) -> None:
        repository, _prerequisite = package_fixture(self.root)
        attributes = repository / ".gitattributes"
        attributes.write_text(
            "godot-codex-mcp/raw-substitution.txt export-subst\n"
            "godot-codex-mcp/raw-ignore.txt export-ignore\n",
            encoding="utf-8",
        )
        substitution_probe = (
            repository / "godot-codex-mcp" / "raw-substitution.txt"
        )
        substitution_probe.write_text(
            "commit=$Format:%H$\n",
            encoding="utf-8",
        )
        ignored_probe = repository / "godot-codex-mcp" / "raw-ignore.txt"
        ignored_probe.write_text("raw committed bytes\n", encoding="utf-8")
        subprocess.run(["git", "init", "-q", repository], check=True)
        subprocess.run(
            ["git", "-C", repository, "config", "user.name", "Sprint 11 Test"],
            check=True,
        )
        subprocess.run(
            [
                "git",
                "-C",
                repository,
                "config",
                "user.email",
                "sprint11@example.invalid",
            ],
            check=True,
        )
        subprocess.run(["git", "-C", repository, "add", "-A"], check=True)
        subprocess.run(
            ["git", "-C", repository, "commit", "-q", "-m", "fixture"],
            check=True,
        )
        source_commit = subprocess.run(
            ["git", "-C", repository, "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        registry = (
            repository
            / "godot-codex-mcp"
            / "product"
            / "registry-profile.v1.json"
        )
        committed_registry = registry.read_bytes()
        registry.write_bytes(b'{"working_tree":"must not ship"}\n')
        ignored_binary = (
            repository
            / "godot-codex-mcp"
            / "target"
            / "release"
            / "godot-codex-mcp"
        )
        ignored_binary.write_bytes(thin_arm64_macho(b"ignored attacker payload"))

        snapshot = self.root / "snapshot"
        package_builder.snapshot_source_checkout(
            repository.resolve(),
            source_commit,
            snapshot,
        )
        self.assertEqual(
            (
                snapshot
                / "godot-codex-mcp"
                / "product"
                / "registry-profile.v1.json"
            ).read_bytes(),
            committed_registry,
        )
        self.assertFalse(
            (snapshot / "godot-codex-mcp" / "target").exists()
        )
        self.assertEqual(
            (
                snapshot / "godot-codex-mcp" / "raw-substitution.txt"
            ).read_text(encoding="utf-8"),
            "commit=$Format:%H$\n",
        )
        self.assertEqual(
            (snapshot / "godot-codex-mcp" / "raw-ignore.txt").read_text(
                encoding="utf-8"
            ),
            "raw committed bytes\n",
        )

    @unittest.skipUnless(
        Path("/usr/bin/git").is_file(),
        "fixed system Git is required",
    )
    def test_source_snapshot_never_lazy_fetches_missing_promisor_blobs(
        self,
    ) -> None:
        origin = self.root / "promisor-origin"
        origin.mkdir()
        tracked = origin / "godot-codex-mcp" / "Cargo.toml"
        tracked.parent.mkdir()
        tracked.write_bytes(b"[workspace]\n" + b"# source\n" * 131072)
        git = "/usr/bin/git"
        subprocess.run([git, "init", "-q", origin], check=True)
        subprocess.run(
            [git, "-C", origin, "config", "user.name", "Sprint 11 Test"],
            check=True,
        )
        subprocess.run(
            [
                git,
                "-C",
                origin,
                "config",
                "user.email",
                "sprint11@example.invalid",
            ],
            check=True,
        )
        subprocess.run([git, "-C", origin, "add", "--all"], check=True)
        subprocess.run(
            [git, "-C", origin, "commit", "-q", "-m", "fixture"],
            check=True,
        )
        source_commit = subprocess.run(
            [git, "-C", origin, "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        object_id = subprocess.run(
            [
                git,
                "-C",
                origin,
                "rev-parse",
                f"{source_commit}:godot-codex-mcp/Cargo.toml",
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

        remote = self.root / "promisor-remote.git"
        subprocess.run(
            [git, "clone", "-q", "--bare", origin, remote],
            check=True,
        )
        subprocess.run(
            [
                git,
                "--git-dir",
                remote,
                "config",
                "uploadpack.allowFilter",
                "true",
            ],
            check=True,
        )
        partial = self.root / "promisor-partial"
        subprocess.run(
            [
                git,
                "clone",
                "-q",
                "--filter=blob:none",
                "--no-checkout",
                remote.as_uri(),
                partial,
            ],
            check=True,
        )

        environment = package_builder.source_git_environment()

        def object_is_missing() -> bool:
            result = subprocess.run(
                [git, "-C", partial, "cat-file", "-e", object_id],
                check=False,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            return result.returncode != 0

        self.assertTrue(
            object_is_missing(),
            "blobless clone unexpectedly contains the required source blob",
        )
        with self.assertRaisesRegex(
            package_builder.PackageError,
            "source snapshot inventory is invalid",
        ):
            package_builder._preflight_source_snapshot(
                partial.resolve(),
                source_commit,
            )
        self.assertTrue(
            object_is_missing(),
            "source preflight lazy-fetched and persisted a promisor blob",
        )

    def test_workspace_must_be_the_exact_checked_out_workspace(self) -> None:
        repository, _prerequisite = package_fixture(self.root)
        workspace = repository / "godot-codex-mcp"
        package_builder.verify_workspace_binding(
            repository.resolve(),
            workspace,
        )
        outside = self.root / "outside-workspace"
        outside.mkdir()
        with self.assertRaises(package_builder.PackageError):
            package_builder.verify_workspace_binding(
                repository.resolve(),
                outside,
            )
        linked = self.root / "linked-workspace"
        linked.symlink_to(workspace, target_is_directory=True)
        with self.assertRaises(package_builder.PackageError):
            package_builder.verify_workspace_binding(
                repository.resolve(),
                linked,
            )

    def test_product_contracts_are_schema_and_registry_bound(self) -> None:
        repository, _prerequisite = package_fixture(self.root)
        workspace = repository / "godot-codex-mcp"
        matrix = package_builder.load_json(
            workspace / "product" / "compatibility-matrix.v1.json"
        )
        compatibility_content = (
            workspace / "product" / "compatibility-matrix.v1.json"
        ).read_bytes()
        registry = package_builder.load_json(
            workspace / "product" / "registry-profile.v1.json"
        )
        additional_product_inputs = (
            package_builder.snapshot_additional_product_inputs(workspace)
        )
        self.assertEqual(
            package_builder.validate_product_contracts(
                workspace,
                matrix,
                registry,
                compatibility_content,
                additional_product_inputs,
            ),
            "0.1.0",
        )

        wrong_binding = json.loads(json.dumps(matrix))
        wrong_binding["registry"]["digest"] = "0" * 64
        with self.assertRaises(package_builder.PackageError):
            package_builder.validate_product_contracts(
                workspace,
                wrong_binding,
                registry,
                compatibility_content,
                additional_product_inputs,
            )

        wrong_target = json.loads(json.dumps(matrix))
        wrong_target["package"]["target"]["architecture"] = "x86_64"
        with self.assertRaises(package_builder.PackageError):
            package_builder.validate_product_contracts(
                workspace,
                wrong_target,
                registry,
                compatibility_content,
                additional_product_inputs,
            )

        incomplete = json.loads(json.dumps(matrix))
        del incomplete["protocols"]
        with self.assertRaises(package_builder.PackageError):
            package_builder.validate_product_contracts(
                workspace,
                incomplete,
                registry,
                compatibility_content,
                additional_product_inputs,
            )

        wrong_host_inputs = dict(additional_product_inputs)
        wrong_host = json.loads(
            wrong_host_inputs["host-coordinate-profile.v1.json"]
        )
        wrong_host["compatibility_matrix"]["sha256"] = "sha256:" + "0" * 64
        wrong_host_inputs["host-coordinate-profile.v1.json"] = (
            package_builder.canonical_json(wrong_host)
        )
        with self.assertRaises(package_builder.PackageError):
            package_builder.validate_product_contracts(
                workspace,
                matrix,
                registry,
                compatibility_content,
                wrong_host_inputs,
            )

        wrong_instructions = dict(additional_product_inputs)
        wrong_instructions["server-instructions.v1.txt"] += b"mutated\n"
        with self.assertRaises(package_builder.PackageError):
            package_builder.validate_product_contracts(
                workspace,
                matrix,
                registry,
                compatibility_content,
                wrong_instructions,
            )

        extra_host_field = dict(additional_product_inputs)
        invalid_host = json.loads(
            extra_host_field["host-coordinate-profile.v1.json"]
        )
        invalid_host["unexpected"] = True
        extra_host_field["host-coordinate-profile.v1.json"] = (
            package_builder.canonical_json(invalid_host)
        )
        with self.assertRaises(package_builder.PackageError):
            package_builder.validate_product_contracts(
                workspace,
                matrix,
                registry,
                compatibility_content,
                extra_host_field,
            )

    def test_release_build_command_is_frozen_targeted_and_root_scoped(
        self,
    ) -> None:
        workspace = self.root / "workspace"
        command = package_builder.cargo_build_command(workspace)
        self.assertEqual(command[:2], ["cargo", "build"])
        self.assertIn("--frozen", command)
        self.assertIn("--locked", command)
        self.assertIn("--release", command)
        self.assertEqual(
            command[command.index("--target") + 1],
            "aarch64-apple-darwin",
        )
        self.assertEqual(
            command[command.index("--manifest-path") + 1],
            str(workspace / "Cargo.toml"),
        )
        packages = [
            command[index + 1]
            for index, value in enumerate(command)
            if value == "--package"
        ]
        self.assertEqual(packages, ["godot-codex", "godot-codex-mcp"])
        binaries = [
            command[index + 1]
            for index, value in enumerate(command)
            if value == "--bin"
        ]
        self.assertEqual(binaries, ["godot-codex", "godot-codex-mcp"])
        self.assertNotIn("--bins", command)

    def test_toolchain_children_receive_only_the_closed_environment(self) -> None:
        sentinel = "s11-super-secret-value"
        process_scope_key = (
            "GODOT_CODEX_PROCESS_SCOPE_" + "a" * 48
        )
        with mock.patch.dict(
            os.environ,
            {
                "SPRINT11_SENTINEL_SECRET": sentinel,
                "HTTPS_PROXY": f"https://token:{sentinel}@example.invalid",
                "RUSTFLAGS": f"--cfg={sentinel}",
                process_scope_key: "1",
            },
            clear=False,
        ):
            environment = package_builder.minimal_toolchain_environment()
            source_git_environment = package_builder.source_git_environment()
            result = package_builder.run_bounded_process(
                [
                    sys.executable,
                    "-c",
                    (
                        "import os;"
                        "print(os.getenv('SPRINT11_SENTINEL_SECRET', ''));"
                        "print(os.getenv('HTTPS_PROXY', ''));"
                        "print(os.getenv('RUSTFLAGS', ''))"
                    ),
                ],
                cwd=self.root,
                env=environment,
                timeout=10,
            )
        self.assertEqual(result.returncode, 0)
        self.assertNotIn(sentinel, environment.values())
        self.assertNotIn(sentinel.encode(), result.stdout + result.stderr)
        self.assertNotIn("HTTPS_PROXY", environment)
        self.assertNotIn("RUSTFLAGS", environment)
        self.assertEqual(environment["SOURCE_DATE_EPOCH"], "0")
        self.assertEqual(environment["LC_ALL"], "C")
        self.assertEqual(environment["CARGO_NET_OFFLINE"], "true")
        self.assertEqual(environment[process_scope_key], "1")
        self.assertEqual(source_git_environment[process_scope_key], "1")
        self.assertEqual(source_git_environment["GIT_NO_LAZY_FETCH"], "1")
        self.assertNotIn("SPRINT11_SENTINEL_SECRET", source_git_environment)
        self.assertNotIn("HTTPS_PROXY", source_git_environment)
        self.assertNotIn("RUSTFLAGS", source_git_environment)

    @unittest.skipUnless(os.name == "posix", "POSIX process-group cleanup")
    def test_bounded_process_enforces_capture_and_file_sink_limits_while_running(
        self,
    ) -> None:
        command = [
            sys.executable,
            "-c",
            (
                "import os,time;"
                "os.write(1,b'x'*65536);"
                "time.sleep(30)"
            ),
        ]
        started = time.monotonic()
        with self.assertRaisesRegex(
            package_builder.PackageError,
            "output exceeds its byte bound",
        ):
            package_builder.run_bounded_process(
                command,
                cwd=self.root,
                timeout=10,
                stdout_limit=1024,
                stderr_limit=1024,
            )
        self.assertLess(time.monotonic() - started, 3)

        with tempfile.TemporaryFile() as output:
            started = time.monotonic()
            with self.assertRaisesRegex(
                package_builder.PackageError,
                "output exceeds its byte bound",
            ):
                package_builder.run_bounded_process(
                    command,
                    cwd=self.root,
                    timeout=10,
                    stdout=output,
                    stdout_limit=1024,
                    stderr_limit=1024,
                )
            self.assertLessEqual(os.fstat(output.fileno()).st_size, 1024)
            self.assertLess(time.monotonic() - started, 3)

    def test_bounded_process_streams_bounded_stdin_and_output(self) -> None:
        payload = b"bounded-input"
        result = package_builder.run_bounded_process(
            [
                sys.executable,
                "-c",
                "import sys;sys.stdout.buffer.write(sys.stdin.buffer.read())",
            ],
            cwd=self.root,
            timeout=10,
            stdin_data=payload,
            stdout_limit=len(payload),
            stderr_limit=0,
        )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, payload)
        self.assertEqual(result.stderr, b"")

    @unittest.skipUnless(os.name == "posix", "POSIX process-group cleanup")
    def test_bounded_process_terminates_descendants(self) -> None:
        child_pid_path = self.root / "child.pid"
        command = [
            "/bin/sh",
            "-c",
            f"sleep 30 & child=$!; printf '%s' \"$child\" > {child_pid_path!s}; wait",
        ]
        with self.assertRaises(package_builder.PackageError):
            package_builder.run_bounded_process(
                command,
                cwd=self.root,
                timeout=1,
            )
        child_pid = int(child_pid_path.read_text(encoding="utf-8"))
        live = True
        for _ in range(20):
            status = subprocess.run(
                ["ps", "-o", "stat=", "-p", str(child_pid)],
                check=False,
                capture_output=True,
                text=True,
            )
            live = status.returncode == 0 and not status.stdout.lstrip().startswith(
                "Z"
            )
            if not live:
                break
            time.sleep(0.05)
        self.assertFalse(live, "bounded subprocess left a live descendant")

    @unittest.skipUnless(
        sys.platform == "darwin",
        "Darwin original-parent process identities are required",
    )
    def test_bounded_process_terminates_setsid_child_with_clean_environment(
        self,
    ) -> None:
        child_pid_path = self.root / "escaped-child.pid"
        marker = self.root / "escaped-child-marker"
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
                package_builder.PackageError,
                "left detached descendants",
            ):
                package_builder.run_bounded_process(
                    [sys.executable, "-c", parent],
                    cwd=self.root,
                    timeout=10,
                    stdout_limit=1024,
                    stderr_limit=1024,
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
    def test_bounded_process_tracks_exec_of_known_descendant(
        self,
    ) -> None:
        child_pid_path = self.root / "exec-descendant.pid"
        marker = self.root / "exec-descendant-marker"
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
                package_builder.PackageError,
                "left detached descendants",
            ):
                package_builder.run_bounded_process(
                    [sys.executable, "-c", parent],
                    cwd=self.root,
                    timeout=10,
                    stdout_limit=1024,
                    stderr_limit=1024,
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

    def test_godot_snapshot_is_immutable_and_rejects_symlinks(self) -> None:
        original = thin_arm64_macho(b"verified Godot")
        source = self.root / "Godot"
        write(source, original, 0o755)
        destination = self.root / "private-Godot"
        with mock.patch.object(
            package_builder,
            "read_regular_input",
            wraps=package_builder.read_regular_input,
        ) as reader:
            observed = package_builder.snapshot_executable(source, destination)
        source.write_bytes(thin_arm64_macho(b"replaced Godot"))
        self.assertEqual(observed, original)
        self.assertEqual(destination.read_bytes(), original)
        self.assertGreaterEqual(
            package_builder.MAX_GODOT_PREREQUISITE_BYTES,
            301_877_808,
        )
        self.assertLessEqual(
            package_builder.MAX_GODOT_PREREQUISITE_BYTES,
            384 * 1024 * 1024,
        )
        self.assertEqual(reader.call_count, 2)
        self.assertTrue(
            all(
                call.kwargs["maximum"]
                == package_builder.MAX_GODOT_PREREQUISITE_BYTES
                for call in reader.call_args_list
            )
        )

        linked = self.root / "linked-Godot"
        linked.symlink_to(source)
        with self.assertRaises(package_builder.PackageError):
            package_builder.snapshot_executable(
                linked,
                self.root / "must-not-exist",
            )

    def test_output_publication_rejects_foreign_destinations_and_cleans_stage(
        self,
    ) -> None:
        output = self.root / "published"
        outside = self.root / "outside"
        outside.mkdir()
        output.symlink_to(outside, target_is_directory=True)
        with self.assertRaises(package_builder.PackageError):
            package_builder.resolve_new_output_destination(output)
        self.assertEqual(list(outside.iterdir()), [])
        output.unlink()

        resolved = package_builder.resolve_new_output_destination(output)
        output.mkdir()
        sentinel = output / "must-survive"
        sentinel.write_text("foreign\n", encoding="utf-8")
        files = {
            "VERSION": package_builder.PackageFile(
                PurePosixPath("VERSION"),
                b"0.1.0\n",
                0o644,
            )
        }
        with self.assertRaises(package_builder.PackageError):
            package_builder.publish_output_tree(
                resolved,
                files,
                "fixture.tar.gz",
                b"archive",
                b"manifest",
            )
        self.assertEqual(sentinel.read_text(encoding="utf-8"), "foreign\n")
        self.assertEqual(
            list(self.root.glob(".godot-codex-stage-*")),
            [],
        )

    def test_committed_third_party_license_bundle_is_current_and_tamper_evident(
        self,
    ) -> None:
        command = [
            sys.executable,
            str(LICENSE_GENERATOR_PATH),
            "--check",
        ]
        current = subprocess.run(
            command,
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=90,
        )
        self.assertEqual(current.returncode, 0, current.stderr)
        report = json.loads(current.stdout)
        self.assertEqual(report["status"], "PASS")
        bundle = THIRD_PARTY_LICENSES_PATH.read_bytes()
        self.assertEqual(report["bytes"], len(bundle))
        self.assertNotIn(str(REPOSITORY_ROOT).encode(), bundle)
        self.assertNotIn(str(Path.home()).encode(), bundle)
        self.assertIn(b"Target: aarch64-apple-darwin\n", bundle)
        self.assertNotIn(b"\nPackage: android_system_properties ", bundle)
        self.assertNotIn(b"\nPackage: windows_", bundle)
        self.assertTrue(
            all(line == line.rstrip(b" \t") for line in bundle.splitlines()),
            "generated license bundle contains trailing horizontal whitespace",
        )
        declared_count = int(
            next(
                line.removeprefix(b"External packages: ")
                for line in bundle.splitlines()
                if line.startswith(b"External packages: ")
            )
        )
        self.assertGreater(declared_count, 0)
        self.assertLessEqual(declared_count, 512)
        self.assertEqual(declared_count, bundle.count(b"\nPackage: "))

        tampered = self.root / "THIRD_PARTY_LICENSES.txt"
        tampered.write_bytes(bundle + b"tampered\n")
        rejected = subprocess.run(
            [*command, "--output", str(tampered)],
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=90,
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("stale", rejected.stderr)

    def test_curated_upstream_licenses_are_identity_and_digest_bound(self) -> None:
        workspace = (self.root / "license-workspace").resolve()
        workspace.mkdir()
        for relative_path, _expected_sha256 in set(
            license_generator.CURATED_LICENSE_DOCUMENTS.values()
        ):
            source = (
                REPOSITORY_ROOT / "godot-codex-mcp" / relative_path
            )
            write(workspace / relative_path, source.read_bytes())

        identity = (
            "jsonschema-regex",
            "0.47.0",
            "MIT",
            "crates.io registry",
        )
        document_name, content = license_generator.curated_license_document(
            workspace,
            identity,
        )
        self.assertTrue(document_name.startswith("curated-upstream/"))
        self.assertIn(b"Copyright (c) 2020-2026 Dmitry Dygalo", content)
        self.assertIsNone(
            license_generator.curated_license_document(
                workspace,
                ("jsonschema-regex", "0.47.1", "MIT", "crates.io registry"),
            )
        )

        relative_path, _expected_sha256 = (
            license_generator.CURATED_LICENSE_DOCUMENTS[identity]
        )
        write(workspace / relative_path, b"tampered\n")
        with self.assertRaisesRegex(
            license_generator.LicenseBundleError,
            "digest differs",
        ):
            license_generator.curated_license_document(workspace, identity)

    def test_package_is_reproducible_safe_and_accepted(self) -> None:
        repository, prerequisite = package_fixture(self.root)
        first_archive, first_manifest = build_fixture(
            repository, prerequisite, self.root / "first output"
        )
        second_archive, second_manifest = build_fixture(
            repository, prerequisite, self.root / "second output"
        )
        self.assertEqual(first_archive.read_bytes(), second_archive.read_bytes())
        self.assertEqual(first_manifest.read_bytes(), second_manifest.read_bytes())

        manifest = json.loads(first_manifest.read_bytes())
        self.assertEqual(
            set(manifest["build_provenance"]),
            {
                "cargo_lock_sha256",
                "cargo_version",
                "fresh_target",
                "rust_toolchain_sha256",
                "rustc_commit",
                "rustc_release",
                "target_triple",
            },
        )
        self.assertTrue(manifest["build_provenance"]["fresh_target"])
        self.assertEqual(
            manifest["build_provenance"]["target_triple"],
            "aarch64-apple-darwin",
        )
        internal_manifest = json.loads(
            (first_manifest.parent / "package-manifest.json").read_bytes()
        )
        self.assertEqual(
            internal_manifest["build_provenance"],
            manifest["build_provenance"],
        )
        sprint11_acceptance.validate_detached_package_manifest(
            manifest,
            manifest_relative_path=first_manifest.name,
            artifact_root=first_manifest.parent,
            expected_source_commit=SOURCE_COMMIT,
        )

        with tarfile.open(
            fileobj=io.BytesIO(first_archive.read_bytes()),
            mode="r:gz",
        ) as archive:
            members = archive.getmembers()
        self.assertTrue(members)
        self.assertTrue(
            all(
                member.uid == 0 and member.gid == 0 and member.mtime == 0
                for member in members
            )
        )
        self.assertTrue(
            all(
                not Path(member.name).is_absolute()
                and ".." not in Path(member.name).parts
                for member in members
            )
        )
        binary = next(
            member for member in members if member.name.endswith("/bin/godot-codex")
        )
        self.assertEqual(stat.S_IMODE(binary.mode), 0o755)
        self.assertTrue(
            any(
                member.name.endswith(
                    "/share/godot-codex/licenses/Godot-LICENSE.txt"
                )
                for member in members
            )
        )
        self.assertTrue(
            any(
                member.name.endswith(
                    "/share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt"
                )
                for member in members
            )
        )
        third_party_record = next(
            record
            for record in manifest["contents"]
            if record["path"]
            == "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt"
        )
        self.assertEqual(
            manifest["third_party_licenses_sha256"],
            third_party_record["sha256"],
        )
        for product_path in PRODUCT_ROOT.iterdir():
            if product_path.name == "THIRD_PARTY_LICENSES.txt":
                continue
            self.assertIn(
                f"share/godot-codex/product/{product_path.name}",
                {
                    record["path"]
                    for record in manifest["contents"]
                },
            )
        packaged_paths = {
            record["path"] for record in manifest["contents"]
        }
        for schema_name in (
            "doctor-report.schema.json",
            "sprint11-multi-project-report.schema.json",
            "sprint11-multi-project-receipt.schema.json",
            "sprint11-surface-acquisition-authority.schema.json",
        ):
            self.assertIn(
                "share/godot-codex/schemas/godot_codex/" + schema_name,
                packaged_paths,
            )

    def test_package_rejects_existing_output_and_detects_tampering(self) -> None:
        repository, prerequisite = package_fixture(self.root)
        output = self.root / "output"
        archive, manifest_path = build_fixture(repository, prerequisite, output)
        with self.assertRaises(package_builder.PackageError):
            build_fixture(repository, prerequisite, output)

        manifest = json.loads(manifest_path.read_bytes())
        missing_license = json.loads(manifest_path.read_bytes())
        missing_license["contents"] = [
            record
            for record in missing_license["contents"]
            if record["path"]
            != "share/godot-codex/licenses/Godot-LICENSE.txt"
        ]
        with self.assertRaises(sprint11_acceptance.AcceptanceError):
            sprint11_acceptance.validate_detached_package_manifest(
                missing_license,
                manifest_relative_path=manifest_path.name,
                artifact_root=output,
                expected_source_commit=SOURCE_COMMIT,
            )

        missing_third_party = json.loads(manifest_path.read_bytes())
        missing_third_party["contents"] = [
            record
            for record in missing_third_party["contents"]
            if record["path"]
            != "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt"
        ]
        with self.assertRaises(sprint11_acceptance.AcceptanceError):
            sprint11_acceptance.validate_detached_package_manifest(
                missing_third_party,
                manifest_relative_path=manifest_path.name,
                artifact_root=output,
                expected_source_commit=SOURCE_COMMIT,
            )

        wrong_third_party_digest = json.loads(manifest_path.read_bytes())
        wrong_third_party_digest["third_party_licenses_sha256"] = (
            "sha256:" + "0" * 64
        )
        with self.assertRaises(sprint11_acceptance.AcceptanceError):
            sprint11_acceptance.validate_detached_package_manifest(
                wrong_third_party_digest,
                manifest_relative_path=manifest_path.name,
                artifact_root=output,
                expected_source_commit=SOURCE_COMMIT,
            )

        wrong_verification = json.loads(manifest_path.read_bytes())
        wrong_verification["godot_prerequisite"]["verification"]["sha256"] = [
            "shasum",
            "-a",
            "256",
            "<godot-binary>",
        ]
        with self.assertRaises(sprint11_acceptance.AcceptanceError):
            sprint11_acceptance.validate_detached_package_manifest(
                wrong_verification,
                manifest_relative_path=manifest_path.name,
                artifact_root=output,
                expected_source_commit=SOURCE_COMMIT,
            )

        archive.write_bytes(archive.read_bytes() + b"tamper")
        with self.assertRaises(sprint11_acceptance.AcceptanceError):
            sprint11_acceptance.validate_detached_package_manifest(
                manifest,
                manifest_relative_path=manifest_path.name,
                artifact_root=output,
                expected_source_commit=SOURCE_COMMIT,
            )

    @unittest.skipUnless(os.name == "posix", "POSIX installer contract")
    def test_installer_owns_only_private_user_local_scope(self) -> None:
        repository, prerequisite = package_fixture(self.root)
        output = self.root / "package output"
        archive, _manifest = build_fixture(repository, prerequisite, output)
        package = extract_fixture(archive, self.root / "extracted package")
        data_root = self.root / "Library Data Ω" / "GodotCodex"
        bin_dir = self.root / "User Bin Ω"
        environment = {
            **os.environ,
            "GODOT_CODEX_DATA_ROOT": str(data_root),
            "GODOT_CODEX_BIN_DIR": str(bin_dir),
        }
        installer = package / "install.sh"
        subprocess.run([installer, "install"], env=environment, check=True)
        subprocess.run([installer, "verify"], env=environment, check=True)
        self.assertTrue((bin_dir / "godot-codex").is_symlink())
        self.assertTrue((bin_dir / "godot-codex-mcp").is_symlink())
        self.assertTrue((data_root / "current").is_symlink())
        self.assertEqual(
            os.readlink(bin_dir / "godot-codex-mcp"),
            str(
                data_root.resolve()
                / "current"
                / "bin"
                / "godot-codex-mcp"
            ),
        )
        stable_sidecar = (
            data_root / "current" / "bin" / "godot-codex-mcp"
        )
        self.assertTrue(stable_sidecar.is_file())
        self.assertTrue(os.access(stable_sidecar, os.X_OK))
        installed = (data_root / "current").resolve()
        self.assertEqual(
            stat.S_IMODE((installed / "bin/godot-codex").stat().st_mode),
            0o755,
        )
        self.assertFalse(
            any(path.suffix == ".gz" for path in installed.iterdir()),
            "detached build artifacts leaked into the installed tree",
        )

        subprocess.run([installer, "uninstall"], env=environment, check=True)
        self.assertFalse((bin_dir / "godot-codex").exists())
        self.assertFalse((data_root / "current").exists())

    @unittest.skipUnless(os.name == "posix", "POSIX installer contract")
    def test_installer_upgrade_and_rollback_are_version_scoped(self) -> None:
        repository, prerequisite = package_fixture(self.root, version="0.1.0")
        first_output = self.root / "first package"
        first_archive, _first_manifest = build_fixture(
            repository, prerequisite, first_output
        )
        first_package = extract_fixture(
            first_archive, self.root / "first extracted package"
        )
        matrix_path = (
            repository
            / "godot-codex-mcp"
            / "product"
            / "compatibility-matrix.v1.json"
        )
        matrix = json.loads(matrix_path.read_bytes())
        matrix["package"]["version"] = "0.1.1"
        matrix_path.write_bytes(package_builder.canonical_json(matrix))
        rebind_fixture_host_profile(repository / "godot-codex-mcp")
        cargo_path = repository / "godot-codex-mcp" / "Cargo.toml"
        cargo_path.write_text(
            cargo_path.read_text(encoding="utf-8").replace(
                'version = "0.1.0"',
                'version = "0.1.1"',
            ),
            encoding="utf-8",
        )
        second_output = self.root / "second package"
        second_archive, _second_manifest = build_fixture(
            repository, prerequisite, second_output
        )
        second_package = extract_fixture(
            second_archive, self.root / "second extracted package"
        )

        data_root = self.root / "versions"
        bin_dir = self.root / "bin"
        environment = {
            **os.environ,
            "GODOT_CODEX_DATA_ROOT": str(data_root),
            "GODOT_CODEX_BIN_DIR": str(bin_dir),
        }
        subprocess.run([first_package / "install.sh", "install"], env=environment, check=True)
        subprocess.run([second_package / "install.sh", "install"], env=environment, check=True)
        self.assertEqual((data_root / "current").resolve().name, "0.1.1")
        self.assertEqual((data_root / "previous").resolve().name, "0.1.0")
        subprocess.run([second_package / "install.sh", "install"], env=environment, check=True)
        self.assertEqual((data_root / "current").resolve().name, "0.1.1")
        self.assertEqual(
            (data_root / "previous").resolve().name,
            "0.1.0",
            "an exact reinstall must preserve the previous rollback generation",
        )
        self.assertTrue(
            (data_root / "current" / "bin" / "godot-codex-mcp").is_file()
        )
        self.assertTrue(
            os.access(
                data_root / "current" / "bin" / "godot-codex-mcp",
                os.X_OK,
            )
        )

        current_binary = data_root / "current" / "bin" / "godot-codex"
        original_binary = current_binary.read_bytes()
        current_binary.write_bytes(b"corrupted current version")
        rejected = subprocess.run(
            [second_package / "install.sh", "rollback"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertEqual((data_root / "current").resolve().name, "0.1.1")
        self.assertEqual((data_root / "previous").resolve().name, "0.1.0")
        self.assertIn("checksum verification failed", rejected.stderr)
        current_binary.write_bytes(original_binary)

        subprocess.run([second_package / "install.sh", "rollback"], env=environment, check=True)
        self.assertEqual((data_root / "current").resolve().name, "0.1.0")
        self.assertEqual((data_root / "previous").resolve().name, "0.1.1")
        self.assertTrue(
            (data_root / "current" / "bin" / "godot-codex-mcp").is_file()
        )
        self.assertTrue(
            os.access(
                data_root / "current" / "bin" / "godot-codex-mcp",
                os.X_OK,
            )
        )
        subprocess.run([second_package / "install.sh", "uninstall"], env=environment, check=True)

    @unittest.skipUnless(os.name == "posix", "POSIX installer contract")
    def test_installer_rejects_different_package_with_the_same_version(self) -> None:
        repository, prerequisite = package_fixture(self.root)
        first_archive, _first_manifest = build_fixture(
            repository,
            prerequisite,
            self.root / "first package",
        )
        first_package = extract_fixture(
            first_archive,
            self.root / "first extracted",
        )
        guide = (
            repository
            / "docs"
            / "codex-integration"
            / "EXTERNAL-CODEX-BETA-GUIDE.md"
        )
        guide.write_text("# different valid fixture guide\n", encoding="utf-8")
        second_archive, _second_manifest = build_fixture(
            repository,
            prerequisite,
            self.root / "second package",
        )
        second_package = extract_fixture(
            second_archive,
            self.root / "second extracted",
        )
        self.assertNotEqual(
            (first_package / "checksums.sha256").read_bytes(),
            (second_package / "checksums.sha256").read_bytes(),
        )

        data_root = self.root / "managed"
        bin_dir = self.root / "bin"
        environment = {
            **os.environ,
            "GODOT_CODEX_DATA_ROOT": str(data_root),
            "GODOT_CODEX_BIN_DIR": str(bin_dir),
        }
        subprocess.run(
            [first_package / "install.sh", "install"],
            env=environment,
            check=True,
            capture_output=True,
            text=True,
        )
        installed_manifest = (
            data_root / "current" / "package-manifest.json"
        ).read_bytes()
        rejected = subprocess.run(
            [second_package / "install.sh", "install"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn(
            "existing version differs from source package",
            rejected.stderr,
        )
        self.assertEqual(
            (data_root / "current" / "package-manifest.json").read_bytes(),
            installed_manifest,
        )
        self.assertFalse((data_root / "previous").exists())
        self.assertTrue((bin_dir / "godot-codex").is_symlink())

    @unittest.skipUnless(os.name == "posix", "POSIX installer contract")
    def test_installer_rejects_traversal_roots_before_mutation(self) -> None:
        repository, prerequisite = package_fixture(self.root)
        archive, _manifest = build_fixture(
            repository,
            prerequisite,
            self.root / "package",
        )
        package = extract_fixture(archive, self.root / "extracted")
        parent = self.root / "managed parent"
        parent.mkdir(mode=0o755)
        sentinel = parent / "sentinel"
        sentinel.write_text("must survive\n", encoding="utf-8")
        original_mode = stat.S_IMODE(parent.stat().st_mode)
        environment = {
            **os.environ,
            "HOME": str(parent),
            "GODOT_CODEX_DATA_ROOT": f"{parent}/child/..",
            "GODOT_CODEX_BIN_DIR": str(self.root / "bin"),
        }

        rejected = subprocess.run(
            [package / "install.sh", "install"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("unsafe managed root", rejected.stderr)
        self.assertEqual(sentinel.read_text(encoding="utf-8"), "must survive\n")
        self.assertEqual(stat.S_IMODE(parent.stat().st_mode), original_mode)
        self.assertFalse((parent / "child").exists())
        self.assertFalse((self.root / "bin").exists())

    @unittest.skipUnless(os.name == "posix", "POSIX installer contract")
    def test_installer_links_must_target_direct_managed_versions(self) -> None:
        repository, prerequisite = package_fixture(self.root)
        archive, _manifest = build_fixture(
            repository,
            prerequisite,
            self.root / "package",
        )
        package = extract_fixture(archive, self.root / "extracted")
        data_root = self.root / "managed"
        bin_dir = self.root / "bin"
        environment = {
            **os.environ,
            "GODOT_CODEX_DATA_ROOT": str(data_root),
            "GODOT_CODEX_BIN_DIR": str(bin_dir),
        }
        installer = package / "install.sh"
        subprocess.run(
            [installer, "install"],
            env=environment,
            check=True,
            capture_output=True,
            text=True,
        )
        installed = (data_root / "current").resolve()
        outside = data_root / "outside"
        shutil.copytree(installed, outside)
        indirect = f"{installed.parent}/../outside"

        (data_root / "current").unlink()
        (data_root / "current").symlink_to(indirect, target_is_directory=True)
        rejected_verify = subprocess.run(
            [installer, "verify"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(rejected_verify.returncode, 0)
        self.assertIn("not a direct managed version", rejected_verify.stderr)

        (data_root / "current").unlink()
        (data_root / "current").symlink_to(installed, target_is_directory=True)
        (data_root / "previous").symlink_to(indirect, target_is_directory=True)
        rejected_rollback = subprocess.run(
            [installer, "rollback"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(rejected_rollback.returncode, 0)
        self.assertIn("not a direct managed version", rejected_rollback.stderr)
        self.assertEqual((data_root / "current").resolve(), installed)

    @unittest.skipUnless(os.name == "posix", "POSIX installer contract")
    def test_installer_verify_requires_both_exact_launcher_links(self) -> None:
        repository, prerequisite = package_fixture(self.root)
        archive, _manifest = build_fixture(
            repository,
            prerequisite,
            self.root / "package",
        )
        package = extract_fixture(archive, self.root / "extracted")
        data_root = self.root / "managed"
        bin_dir = self.root / "bin"
        environment = {
            **os.environ,
            "GODOT_CODEX_DATA_ROOT": str(data_root),
            "GODOT_CODEX_BIN_DIR": str(bin_dir),
        }
        subprocess.run(
            [package / "install.sh", "install"],
            env=environment,
            check=True,
            capture_output=True,
            text=True,
        )
        missing = bin_dir / "godot-codex"
        missing.unlink()
        result = subprocess.run(
            [package / "install.sh", "verify"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("managed executable link is missing", result.stderr)
        self.assertTrue(
            (data_root / "current" / "bin" / "godot-codex-mcp").is_file()
        )

    @unittest.skipUnless(os.name == "posix", "POSIX installer contract")
    def test_installer_rejects_unlisted_files_symlinks_and_unsafe_ancestors(self) -> None:
        repository, prerequisite = package_fixture(self.root)
        archive, _manifest = build_fixture(
            repository, prerequisite, self.root / "package"
        )
        package = extract_fixture(archive, self.root / "extracted")
        data_root = self.root / "managed" / "GodotCodex"
        bin_dir = self.root / "bin"
        environment = {
            **os.environ,
            "GODOT_CODEX_DATA_ROOT": str(data_root),
            "GODOT_CODEX_BIN_DIR": str(bin_dir),
        }

        (package / "unlisted").write_text("foreign", encoding="utf-8")
        result = subprocess.run(
            [package / "install.sh", "install"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        (package / "unlisted").unlink()

        checksums = package / "checksums.sha256"
        original_checksums = checksums.read_bytes()
        third_party_license = (
            package
            / "share"
            / "godot-codex"
            / "licenses"
            / "THIRD_PARTY_LICENSES.txt"
        )
        original_license = third_party_license.read_bytes()
        checksums.write_text(
            "".join(
                line
                for line in original_checksums.decode("utf-8").splitlines(
                    keepends=True
                )
                if not line.endswith(
                    "  share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt\n"
                )
            ),
            encoding="utf-8",
        )
        third_party_license.unlink()
        result = subprocess.run(
            [package / "install.sh", "install"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum manifest is invalid", result.stderr)
        checksums.write_bytes(original_checksums)
        third_party_license.write_bytes(original_license)

        (package / "unsafe-link").symlink_to(package / "VERSION")
        result = subprocess.run(
            [package / "install.sh", "install"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        (package / "unsafe-link").unlink()

        outside = self.root / "outside"
        outside.mkdir()
        symlink_parent = self.root / "linked-parent"
        symlink_parent.symlink_to(outside, target_is_directory=True)
        unsafe_environment = {
            **environment,
            "GODOT_CODEX_DATA_ROOT": str(symlink_parent / "GodotCodex"),
        }
        result = subprocess.run(
            [package / "install.sh", "install"],
            env=unsafe_environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((outside / "GodotCodex").exists())

    @unittest.skipUnless(os.name == "posix", "POSIX installer contract")
    def test_installer_preflights_foreign_links_partial_targets_and_locks(self) -> None:
        repository, prerequisite = package_fixture(self.root)
        archive, _manifest = build_fixture(
            repository, prerequisite, self.root / "package"
        )
        package = extract_fixture(archive, self.root / "extracted")
        data_root = self.root / "managed"
        bin_dir = self.root / "bin"
        environment = {
            **os.environ,
            "GODOT_CODEX_DATA_ROOT": str(data_root),
            "GODOT_CODEX_BIN_DIR": str(bin_dir),
        }
        bin_dir.mkdir()
        foreign = bin_dir / "godot-codex"
        foreign.write_text("foreign", encoding="utf-8")
        result = subprocess.run(
            [package / "install.sh", "install"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(foreign.read_text(encoding="utf-8"), "foreign")
        self.assertFalse((data_root / "current").exists())
        foreign.unlink()

        partial = data_root / "versions" / "0.1.0"
        partial.mkdir(parents=True)
        (partial / "partial").write_text("incomplete", encoding="utf-8")
        result = subprocess.run(
            [package / "install.sh", "install"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((data_root / "current").exists())
        (partial / "partial").unlink()
        partial.rmdir()

        lock = data_root / ".installer-lock"
        lock.mkdir()
        result = subprocess.run(
            [package / "install.sh", "install"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((data_root / "current").exists())
        self.assertTrue(
            lock.is_dir(),
            "a contender must not remove a lock owned by another installer",
        )

    @unittest.skipUnless(os.name == "posix", "POSIX installer contract")
    def test_uninstall_preflights_every_launcher_before_mutation(self) -> None:
        repository, prerequisite = package_fixture(self.root)
        archive, _manifest = build_fixture(
            repository,
            prerequisite,
            self.root / "package",
        )
        package = extract_fixture(archive, self.root / "extracted")
        data_root = self.root / "managed"
        bin_dir = self.root / "bin"
        environment = {
            **os.environ,
            "GODOT_CODEX_DATA_ROOT": str(data_root),
            "GODOT_CODEX_BIN_DIR": str(bin_dir),
        }
        subprocess.run(
            [package / "install.sh", "install"],
            env=environment,
            check=True,
            capture_output=True,
            text=True,
        )
        foreign = bin_dir / "godot-codex-mcp"
        foreign.unlink()
        foreign.write_text("foreign\n", encoding="utf-8")

        result = subprocess.run(
            [package / "install.sh", "uninstall"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(foreign.read_text(encoding="utf-8"), "foreign\n")
        self.assertTrue((bin_dir / "godot-codex").is_symlink())
        self.assertTrue((data_root / "current").is_symlink())
        self.assertIn("refusing to remove a foreign executable", result.stderr)

    @unittest.skipUnless(os.name == "posix", "POSIX installer contract")
    def test_uninstall_rejects_forged_owned_version_before_deleting_it(self) -> None:
        repository, prerequisite = package_fixture(self.root)
        archive, _manifest = build_fixture(
            repository, prerequisite, self.root / "package"
        )
        package = extract_fixture(archive, self.root / "extracted")
        data_root = self.root / "managed"
        bin_dir = self.root / "bin"
        environment = {
            **os.environ,
            "GODOT_CODEX_DATA_ROOT": str(data_root),
            "GODOT_CODEX_BIN_DIR": str(bin_dir),
        }
        subprocess.run(
            [package / "install.sh", "install"],
            env=environment,
            check=True,
            capture_output=True,
            text=True,
        )
        forged = data_root / "versions" / "forged"
        forged.mkdir()
        (forged / ".godot-codex-owned").write_text(
            "0" * 64 + "\n",
            encoding="utf-8",
        )
        sentinel = forged / "must-survive.txt"
        sentinel.write_text("foreign\n", encoding="utf-8")

        result = subprocess.run(
            [package / "install.sh", "uninstall"],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(sentinel.is_file())
        self.assertTrue((data_root / "current").is_symlink())
        self.assertTrue((bin_dir / "godot-codex").is_symlink())
        self.assertIn("package checksum manifest", result.stderr)

    def test_package_path_oracle_rejects_traversal(self) -> None:
        with self.assertRaises(package_builder.PackageError):
            package_builder.safe_archive_path(PurePosixPath("../escape"))


if __name__ == "__main__":
    unittest.main()
