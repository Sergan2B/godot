#!/usr/bin/env python3
"""Run the Sprint 3 storage spike without coupling validation to package bumps."""

from __future__ import annotations

import contextlib
import copy
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from collections.abc import Iterator, Sequence
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SPIKE_ROOT = REPOSITORY_ROOT / "tests/codex/storage_spike"
SPIKE_MANIFEST = SPIKE_ROOT / "Cargo.toml"
SPIKE_LOCK = SPIKE_ROOT / "Cargo.lock"
SPIKE_README = SPIKE_ROOT / "README.md"
SOURCE_SCOPES = REPOSITORY_ROOT / "tests/codex/sprint3_source_scopes.txt"
WORKSPACE_MANIFEST = REPOSITORY_ROOT / "godot-codex-mcp/Cargo.toml"
INDEX_STORE_ROOT = REPOSITORY_ROOT / "godot-codex-mcp/crates/index-store"
CANONICAL_STORAGE_EVIDENCE = (
    REPOSITORY_ROOT
    / "tests/codex/evidence/sprint-3-storage-spike-cross-platform.json"
)
AGGREGATE_RUNNER = REPOSITORY_ROOT / "tests/codex/runners/sprint3_aggregate.sh"
LINUX_RUNNER = REPOSITORY_ROOT / "tests/codex/runners/sprint3_linux_x86_64.sh"
INDEX_STORE_PACKAGE = "godot-codex-index-store"
INDEX_STORE_DEPENDENCY = re.compile(
    r'(?m)^godot-codex-index-store = \{ path = "[^"\n]+" \}$'
)
SEMVER = re.compile(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
)


class StorageSpikeLauncherError(RuntimeError):
    """The isolated Sprint 3 storage-spike launcher could not be prepared."""


def rewrite_path_package_version(
    lock_bytes: bytes,
    package_name: str,
    version: str,
) -> bytes:
    """Refresh one exact local path-package version without changing other lock data."""

    if SEMVER.fullmatch(version) is None:
        raise StorageSpikeLauncherError(
            "workspace package version is not bounded SemVer"
        )
    try:
        text = lock_bytes.decode("utf-8")
        document = tomllib.loads(text)
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise StorageSpikeLauncherError("storage-spike lock is invalid") from error

    packages = document.get("package")
    if not isinstance(packages, list):
        raise StorageSpikeLauncherError("storage-spike lock has no package table")
    matches = [
        item
        for item in packages
        if isinstance(item, dict) and item.get("name") == package_name
    ]
    if len(matches) != 1:
        raise StorageSpikeLauncherError(
            "local path package must appear exactly once"
        )
    if "source" in matches[0]:
        raise StorageSpikeLauncherError(
            "eligible package must be a local path package"
        )

    package_blocks = list(
        re.finditer(
            r"(?ms)^\[\[package\]\]\n.*?(?=^\[\[package\]\]\n|\Z)",
            text,
        )
    )
    selected = [
        block
        for block in package_blocks
        if re.search(
            rf'(?m)^name = "{re.escape(package_name)}"$',
            block.group(0),
        )
    ]
    if len(selected) != 1:
        raise StorageSpikeLauncherError(
            "local path package block must appear exactly once"
        )
    block = selected[0]
    replacement, count = re.subn(
        r'(?m)^version = "[^"]+"$',
        f'version = "{version}"',
        block.group(0),
        count=1,
    )
    if count != 1:
        raise StorageSpikeLauncherError(
            "local path package must have one version"
        )

    refreshed = text[: block.start()] + replacement + text[block.end() :]
    try:
        refreshed_document = tomllib.loads(refreshed)
    except tomllib.TOMLDecodeError as error:
        raise StorageSpikeLauncherError(
            "refreshed storage-spike lock is invalid"
        ) from error
    expected_document = copy.deepcopy(document)
    expected_match = next(
        item
        for item in expected_document["package"]
        if item["name"] == package_name
    )
    expected_match["version"] = version
    if refreshed_document != expected_document:
        raise StorageSpikeLauncherError(
            "lock refresh changed unrelated material"
        )
    return refreshed.encode("utf-8")


def workspace_version(repository_root: Path = REPOSITORY_ROOT) -> str:
    """Read the current exact Godot Codex workspace package version."""

    manifest = repository_root / "godot-codex-mcp/Cargo.toml"
    try:
        document = tomllib.loads(manifest.read_text(encoding="utf-8"))
        version = document["workspace"]["package"]["version"]
    except (OSError, UnicodeError, tomllib.TOMLDecodeError, KeyError, TypeError) as error:
        raise StorageSpikeLauncherError(
            "Godot Codex workspace version is unavailable"
        ) from error
    if not isinstance(version, str) or SEMVER.fullmatch(version) is None:
        raise StorageSpikeLauncherError(
            "workspace package version is not bounded SemVer"
        )
    return version


def replace_exact_index_store_path(
    manifest_text: str,
    index_store: Path,
) -> str:
    """Bind the copied spike manifest to one exact current index-store path."""

    if not index_store.is_dir() or not (index_store / "Cargo.toml").is_file():
        raise StorageSpikeLauncherError("index-store source is unavailable")
    replacement = (
        "godot-codex-index-store = { path = "
        f"{json.dumps(str(index_store.resolve(strict=True)))}"
        " }"
    )
    rewritten, count = INDEX_STORE_DEPENDENCY.subn(replacement, manifest_text)
    if count != 1:
        raise StorageSpikeLauncherError(
            "index-store dependency must appear exactly once"
        )
    return rewritten


def _require_regular_tree(root: Path) -> None:
    if root.is_symlink() or not root.is_dir():
        raise StorageSpikeLauncherError("storage-spike source tree is unavailable")
    try:
        unsafe = next((path for path in root.rglob("*") if path.is_symlink()), None)
    except OSError as error:
        raise StorageSpikeLauncherError(
            "storage-spike source tree could not be inspected"
        ) from error
    if unsafe is not None:
        raise StorageSpikeLauncherError(
            "storage-spike source tree contains a symlink"
        )


def _require_regular_file(path: Path, label: str) -> None:
    if path.is_symlink() or not path.is_file():
        raise StorageSpikeLauncherError(f"{label} must be a regular file")


@contextlib.contextmanager
def prepared_storage_spike(
    repository_root: Path = REPOSITORY_ROOT,
) -> Iterator[Path]:
    """Yield an isolated manifest whose local path version matches the workspace."""

    try:
        repository_root = repository_root.resolve(strict=True)
    except OSError as error:
        raise StorageSpikeLauncherError("repository root is unavailable") from error
    spike_root = repository_root / "tests/codex/storage_spike"
    source_root = spike_root / "src"
    source_scopes = repository_root / "tests/codex/sprint3_source_scopes.txt"
    manifest = spike_root / "Cargo.toml"
    lock = spike_root / "Cargo.lock"
    readme = spike_root / "README.md"
    index_store = repository_root / "godot-codex-mcp/crates/index-store"

    _require_regular_tree(source_root)
    for path, label in (
        (source_scopes, "Sprint 3 source scopes"),
        (manifest, "storage-spike manifest"),
        (lock, "storage-spike lock"),
        (readme, "storage-spike README"),
    ):
        _require_regular_file(path, label)

    try:
        manifest_text = replace_exact_index_store_path(
            manifest.read_text(encoding="utf-8"),
            index_store,
        )
        refreshed_lock = rewrite_path_package_version(
            lock.read_bytes(),
            INDEX_STORE_PACKAGE,
            workspace_version(repository_root),
        )
    except (OSError, UnicodeError) as error:
        raise StorageSpikeLauncherError(
            "storage-spike inputs could not be read"
        ) from error

    with tempfile.TemporaryDirectory(
        prefix="sprint3-storage-validator-"
    ) as temporary:
        destination = Path(temporary) / "tests/codex/storage_spike"
        try:
            destination.mkdir(parents=True)
            shutil.copytree(source_root, destination / "src")
            shutil.copy2(readme, destination / "README.md")
            (destination / "Cargo.toml").write_text(
                manifest_text,
                encoding="utf-8",
            )
            (destination / "Cargo.lock").write_bytes(refreshed_lock)
            shutil.copy2(source_scopes, destination.parent / source_scopes.name)
        except OSError as error:
            raise StorageSpikeLauncherError(
                "isolated storage-spike workspace could not be created"
            ) from error
        yield destination / "Cargo.toml"


def storage_spike_command(
    manifest: Path,
    arguments: Sequence[str],
) -> list[str]:
    """Build the exact frozen, offline Cargo command for one spike operation."""

    return [
        "cargo",
        "+1.94.1",
        "run",
        "--quiet",
        "--locked",
        "--offline",
        "--release",
        "--manifest-path",
        str(manifest),
        "--",
        *arguments,
    ]


def run_storage_spike(
    arguments: Sequence[str],
    *,
    capture_output: bool = False,
) -> subprocess.CompletedProcess[str]:
    """Run one storage-spike operation through the isolated current-version lock."""

    with prepared_storage_spike() as manifest:
        environment = os.environ.copy()
        environment["CARGO_TARGET_DIR"] = str(SPIKE_ROOT / "target")
        return subprocess.run(
            storage_spike_command(manifest, arguments),
            cwd=REPOSITORY_ROOT,
            env=environment,
            check=False,
            capture_output=capture_output,
            text=True,
            timeout=600,
        )


def main(arguments: Sequence[str] | None = None) -> int:
    """Forward CLI arguments while keeping launcher failures bounded."""

    try:
        return run_storage_spike(
            tuple(sys.argv[1:] if arguments is None else arguments)
        ).returncode
    except (
        OSError,
        subprocess.TimeoutExpired,
        StorageSpikeLauncherError,
    ) as error:
        print(
            f"Sprint 3 storage-spike launcher failed: {error}",
            file=sys.stderr,
        )
        return 70


if __name__ == "__main__":
    raise SystemExit(main())
