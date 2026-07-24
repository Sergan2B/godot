#!/usr/bin/env python3
"""Build a deterministic, detached-manifest macOS arm64 beta package."""

from __future__ import annotations

import argparse
import ctypes
import errno
import gzip
import hashlib
import io
import json
import os
import re
import secrets
import shutil
import signal
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Final

PACKAGE_SCHEMA: Final = "godot-codex-package/1.0"
DETACHED_SCHEMA: Final = "s11-package-manifest/1.0"
TARGET_OS: Final = "macos"
TARGET_ARCH: Final = "arm64"
GODOT_EXPECTED_INSTALL_PATH: Final = (
    "~/Applications/Godot Codex.app/Contents/MacOS/Godot"
)
GODOT_VERIFICATION: Final = {
    "sha256": ["/usr/bin/shasum", "-a", "256", "<godot-binary>"],
    "version": ["<godot-binary>", "--version"],
}
MACHO_CPU_TYPE_ARM64: Final = 0x0100000C
MAX_FILES: Final = 512
MAX_INPUT_BYTES: Final = 512 * 1024 * 1024
MAX_SINGLE_FILE_BYTES: Final = 256 * 1024 * 1024
MAX_LICENSE_CHECK_OUTPUT_BYTES: Final = 4096
MAX_SOURCE_FILES: Final = 8192
MAX_SOURCE_BYTES: Final = 512 * 1024 * 1024
MAX_SOURCE_ARCHIVE_BYTES: Final = 640 * 1024 * 1024
MAX_CARGO_OUTPUT_BYTES: Final = 1024 * 1024
MAX_CARGO_BUILD_SECONDS: Final = 15 * 60
COMMIT_RE: Final = re.compile(r"[0-9a-f]{40}\Z")
DIGEST_RE: Final = re.compile(r"[0-9a-f]{64}\Z")
VERSION_RE: Final = re.compile(
    r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?\Z"
)
SOURCE_ARCHIVE_DIRECTORIES: Final = (
    PurePosixPath("godot-codex-mcp"),
    PurePosixPath(".agents/skills/godot-editor"),
)
SOURCE_ARCHIVE_FILES: Final = (
    PurePosixPath(".codex/config.toml.example"),
    PurePosixPath("docs/codex-integration/templates/AGENTS.godot.md"),
    PurePosixPath("docs/codex-integration/EXTERNAL-CODEX-BETA-GUIDE.md"),
    PurePosixPath("LICENSE.txt"),
)
SOURCE_ARCHIVE_PATHS: Final = tuple(
    path.as_posix()
    for path in (*SOURCE_ARCHIVE_DIRECTORIES, *SOURCE_ARCHIVE_FILES)
)
REGISTRY_FIELDS: Final = {
    "digest",
    "fixed_resources",
    "fixed_resource_count",
    "profile_id",
    "read_only_tools",
    "resource_templates",
    "resource_template_count",
    "schema_version",
    "tool_count",
    "tools",
}
HOST_PROFILE_NAME: Final = "host-coordinate-profile.v1.json"
SERVER_INSTRUCTIONS_NAME: Final = "server-instructions.v1.txt"
CORE_PRODUCT_INPUTS: Final = {
    "THIRD_PARTY_LICENSES.txt",
    "compatibility-matrix.v1.json",
    "registry-profile.v1.json",
}
# These location-only variables are the complete inherited environment surface
# for Cargo/rustc. Credentials, proxies, compiler wrappers, flags, and arbitrary
# CARGO_/RUST_ variables are intentionally absent.
TOOLCHAIN_ENV_ALLOWLIST: Final = (
    "CARGO_HOME",
    "DEVELOPER_DIR",
    "HOME",
    "MACOSX_DEPLOYMENT_TARGET",
    "PATH",
    "RUSTUP_HOME",
    "SDKROOT",
    "TMPDIR",
)


class PackageError(RuntimeError):
    """Raised when an input cannot produce a bounded release artifact."""


@dataclass(frozen=True)
class PackageFile:
    archive_path: PurePosixPath
    content: bytes
    mode: int

    def read(self) -> bytes:
        return self.content


def canonical_json(value: object) -> bytes:
    return (
        json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
        + b"\n"
    )


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    hasher = hashlib.sha256()
    try:
        with path.open("rb") as stream:
            while chunk := stream.read(1024 * 1024):
                hasher.update(chunk)
    except OSError as error:
        raise PackageError("artifact could not be hashed") from error
    return hasher.hexdigest()


def minimal_toolchain_environment(
    source: dict[str, str] | None = None,
) -> dict[str, str]:
    inherited = os.environ if source is None else source
    environment: dict[str, str] = {}
    for name in TOOLCHAIN_ENV_ALLOWLIST:
        value = inherited.get(name)
        if value is None:
            continue
        if (
            not value
            or "\0" in value
            or len(value.encode("utf-8")) > 4096
        ):
            raise PackageError("toolchain environment is invalid")
        environment[name] = value
    if "PATH" not in environment:
        raise PackageError("toolchain environment lacks PATH")
    if "HOME" not in environment and not {
        "CARGO_HOME",
        "RUSTUP_HOME",
    }.issubset(environment):
        raise PackageError("toolchain environment lacks tool homes")
    environment.update(
        {
            "CARGO_INCREMENTAL": "0",
            "CARGO_NET_OFFLINE": "true",
            "CARGO_TERM_COLOR": "never",
            "LANG": "C",
            "LC_ALL": "C",
            "NO_COLOR": "1",
            "SOURCE_DATE_EPOCH": "0",
            "TZ": "UTC",
        }
    )
    return environment


def _terminate_process_group(process: subprocess.Popen[bytes]) -> None:
    if os.name == "posix":
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            pass
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    else:
        process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.kill()
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired as error:
        raise PackageError("bounded subprocess could not be terminated") from error


def run_bounded_process(
    command: list[str],
    *,
    cwd: Path,
    timeout: int,
    env: dict[str, str] | None = None,
    stdout: Any = subprocess.PIPE,
    stderr: Any = subprocess.PIPE,
) -> subprocess.CompletedProcess[bytes]:
    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=stdout,
            stderr=stderr,
            start_new_session=os.name == "posix",
        )
    except OSError as error:
        raise PackageError("bounded subprocess could not be started") from error
    try:
        process_stdout, process_stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        _terminate_process_group(process)
        process.communicate()
        raise PackageError("bounded subprocess timed out") from error
    if os.name == "posix":
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            pass
        else:
            _terminate_process_group(process)
    return subprocess.CompletedProcess(
        command,
        process.returncode,
        process_stdout,
        process_stderr,
    )


def safe_archive_path(value: PurePosixPath) -> None:
    if (
        value.is_absolute()
        or not value.parts
        or any(part in {"", ".", ".."} for part in value.parts)
        or "\n" in value.as_posix()
        or "\r" in value.as_posix()
        or "\0" in value.as_posix()
    ):
        raise PackageError("unsafe package path")


def read_regular_input(
    path: Path,
    *,
    executable: bool = False,
    maximum: int = MAX_SINGLE_FILE_BYTES,
    nonempty: bool = False,
) -> bytes:
    descriptor = -1
    try:
        flags = os.O_RDONLY
        if hasattr(os, "O_CLOEXEC"):
            flags |= os.O_CLOEXEC
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        descriptor = os.open(path, flags)
    except OSError as error:
        raise PackageError("required package input is missing") from error
    try:
        metadata_before = os.fstat(descriptor)
        if not stat.S_ISREG(metadata_before.st_mode):
            raise PackageError("package inputs must be regular non-symlink files")
        if executable and metadata_before.st_mode & 0o111 == 0:
            raise PackageError("release binary is not executable")
        if metadata_before.st_size < 0 or metadata_before.st_size > maximum:
            raise PackageError("package input exceeds the single-file limit")
        chunks: list[bytes] = []
        remaining = maximum + 1
        while remaining:
            chunk = os.read(descriptor, min(1024 * 1024, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        value = b"".join(chunks)
        metadata_after = os.fstat(descriptor)
    except OSError as error:
        raise PackageError("required package input is unreadable") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    identity_before = (
        metadata_before.st_dev,
        metadata_before.st_ino,
        metadata_before.st_size,
        metadata_before.st_mode,
        metadata_before.st_mtime_ns,
        metadata_before.st_ctime_ns,
    )
    identity_after = (
        metadata_after.st_dev,
        metadata_after.st_ino,
        metadata_after.st_size,
        metadata_after.st_mode,
        metadata_after.st_mtime_ns,
        metadata_after.st_ctime_ns,
    )
    if (
        len(value) != metadata_before.st_size
        or len(value) > maximum
        or identity_before != identity_after
    ):
        raise PackageError("package input changed while it was read")
    if nonempty and not value:
        raise PackageError("required package input is empty")
    return value


def require_regular_input(path: Path, *, executable: bool = False) -> None:
    read_regular_input(path, executable=executable)


def require_nonempty_regular_input(path: Path) -> None:
    read_regular_input(path, nonempty=True)


def require_arm64_macho_bytes(value: bytes) -> None:
    header = value[:8]
    if len(header) != 8:
        raise PackageError("release binary is not a Mach-O executable")
    if header[:4] == b"\xcf\xfa\xed\xfe":
        cpu_type = struct.unpack("<I", header[4:8])[0]
    elif header[:4] == b"\xfe\xed\xfa\xcf":
        cpu_type = struct.unpack(">I", header[4:8])[0]
    else:
        raise PackageError("release binary must be a thin Mach-O artifact")
    if cpu_type != MACHO_CPU_TYPE_ARM64:
        raise PackageError("release binary architecture is not arm64")


def require_arm64_macho(path: Path) -> None:
    require_arm64_macho_bytes(read_regular_input(path, executable=True))


def add_file(
    files: dict[str, PackageFile],
    archive_path: str,
    source_path: Path,
    *,
    mode: int = 0o644,
) -> None:
    add_bytes(
        files,
        archive_path,
        read_regular_input(source_path, executable=bool(mode & 0o111)),
        mode=mode,
    )


def add_tree(
    files: dict[str, PackageFile],
    source_root: Path,
    archive_root: str,
) -> None:
    if not source_root.is_dir() or source_root.is_symlink():
        raise PackageError("required package directory is missing or unsafe")
    for path in sorted(source_root.rglob("*")):
        if path.is_dir() and not path.is_symlink():
            continue
        relative = path.relative_to(source_root)
        try:
            metadata = path.lstat()
        except OSError as error:
            raise PackageError("package input is unavailable") from error
        mode = 0o755 if metadata.st_mode & 0o111 else 0o644
        add_file(
            files,
            (PurePosixPath(archive_root) / PurePosixPath(relative.as_posix())).as_posix(),
            path,
            mode=mode,
        )


def add_bytes(
    files: dict[str, PackageFile],
    archive_path: str,
    content: bytes,
    *,
    mode: int = 0o644,
) -> None:
    path = PurePosixPath(archive_path)
    safe_archive_path(path)
    if len(content) > MAX_SINGLE_FILE_BYTES:
        raise PackageError("generated package input exceeds the single-file limit")
    key = path.as_posix()
    if key in files:
        raise PackageError("duplicate package path")
    files[key] = PackageFile(path, content, mode)


def _reject_duplicate_fields(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON field")
        result[key] = value
    return result


def load_json_bytes(content: bytes) -> dict[str, Any]:
    try:
        value = json.loads(content, object_pairs_hook=_reject_duplicate_fields)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise PackageError("package contract JSON is invalid") from error
    if not isinstance(value, dict):
        raise PackageError("package contract JSON root must be an object")
    return value


def load_json(path: Path) -> dict[str, Any]:
    return load_json_bytes(read_regular_input(path, nonempty=True))


def snapshot_additional_product_inputs(workspace: Path) -> dict[str, bytes]:
    product_root = workspace / "product"
    try:
        root_metadata = product_root.lstat()
        entries = sorted(product_root.iterdir())
    except OSError as error:
        raise PackageError("product contract directory is unavailable") from error
    if (
        stat.S_ISLNK(root_metadata.st_mode)
        or not stat.S_ISDIR(root_metadata.st_mode)
        or len(entries) > 64
    ):
        raise PackageError("product contract directory is missing or unsafe")
    result: dict[str, bytes] = {}
    for product_path in entries:
        if product_path.name in CORE_PRODUCT_INPUTS:
            continue
        relative = PurePosixPath(product_path.name)
        safe_archive_path(relative)
        if len(relative.parts) != 1:
            raise PackageError("product contract path is unsafe")
        result[product_path.name] = read_regular_input(
            product_path,
            nonempty=True,
        )
    if not {HOST_PROFILE_NAME, SERVER_INSTRUCTIONS_NAME}.issubset(result):
        raise PackageError("required product authority is missing")
    return result


def run_git(repository_root: Path, *arguments: str) -> bytes:
    try:
        result = run_bounded_process(
            ["/usr/bin/git", "-C", str(repository_root), *arguments],
            cwd=repository_root,
            timeout=10,
        )
    except PackageError as error:
        raise PackageError("source checkout could not be verified") from error
    if result.returncode != 0 or len(result.stdout) > 1024 * 1024:
        raise PackageError("source checkout could not be verified")
    return result.stdout


def verify_source_checkout(repository_root: Path, source_commit: str) -> None:
    if not COMMIT_RE.fullmatch(source_commit):
        raise PackageError("--source-commit must be one lowercase 40-hex commit")
    top_level_raw = run_git(repository_root, "rev-parse", "--show-toplevel")
    try:
        top_level = Path(top_level_raw.decode("utf-8").strip()).resolve(strict=True)
    except (UnicodeDecodeError, OSError) as error:
        raise PackageError("source checkout root is invalid") from error
    if top_level != repository_root:
        raise PackageError("--repository-root must be the exact Git worktree root")
    head = run_git(repository_root, "rev-parse", "--verify", "HEAD").decode(
        "ascii", errors="strict"
    ).strip()
    if head != source_commit:
        raise PackageError("--source-commit does not match the checked-out HEAD")
    status = run_git(
        repository_root,
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
    )
    if status:
        raise PackageError("source checkout must be completely clean")


def verify_workspace_binding(repository_root: Path, workspace: Path) -> None:
    expected = repository_root / "godot-codex-mcp"
    try:
        workspace_metadata = workspace.lstat()
        expected = expected.resolve(strict=True)
        resolved_workspace = workspace.resolve(strict=True)
    except OSError as error:
        raise PackageError("package workspace is unavailable") from error
    if (
        stat.S_ISLNK(workspace_metadata.st_mode)
        or not stat.S_ISDIR(workspace_metadata.st_mode)
        or not expected.is_dir()
        or not resolved_workspace.is_dir()
        or not os.path.samefile(expected, resolved_workspace)
    ):
        raise PackageError("--workspace must be the checked-out godot-codex-mcp directory")


def _snapshot_path_allowed(path: PurePosixPath) -> bool:
    if path in SOURCE_ARCHIVE_FILES:
        return True
    if any(root == path or root in path.parents for root in SOURCE_ARCHIVE_DIRECTORIES):
        return True
    requested = (*SOURCE_ARCHIVE_DIRECTORIES, *SOURCE_ARCHIVE_FILES)
    return any(path in item.parents for item in requested)


def _run_git_to_temporary(
    repository_root: Path,
    output: Any,
    *arguments: str,
    timeout: int = 30,
) -> None:
    try:
        result = run_bounded_process(
            ["/usr/bin/git", "-C", str(repository_root), *arguments],
            cwd=repository_root,
            stdout=output,
            timeout=timeout,
        )
    except PackageError as error:
        raise PackageError("source snapshot could not be created") from error
    if result.returncode != 0 or len(result.stderr) > 64 * 1024:
        raise PackageError("source snapshot could not be created")


def _preflight_source_snapshot(repository_root: Path, source_commit: str) -> None:
    with tempfile.TemporaryFile() as listing:
        _run_git_to_temporary(
            repository_root,
            listing,
            "ls-tree",
            "-rlz",
            source_commit,
            "--",
            *SOURCE_ARCHIVE_PATHS,
        )
        size = os.fstat(listing.fileno()).st_size
        if size > 8 * 1024 * 1024:
            raise PackageError("source snapshot inventory exceeds its byte bound")
        listing.seek(0)
        records = listing.read().split(b"\0")
    file_count = 0
    total_bytes = 0
    for record in records:
        if not record:
            continue
        try:
            metadata, raw_path = record.split(b"\t", 1)
            mode, object_type, _object_id, raw_size = metadata.split(b" ", 3)
            path = PurePosixPath(raw_path.decode("utf-8"))
            entry_size = int(raw_size)
        except (UnicodeDecodeError, ValueError) as error:
            raise PackageError("source snapshot inventory is invalid") from error
        safe_archive_path(path)
        if (
            not _snapshot_path_allowed(path)
            or object_type != b"blob"
            or mode not in {b"100644", b"100755"}
            or entry_size < 0
            or entry_size > MAX_SINGLE_FILE_BYTES
        ):
            raise PackageError("source snapshot contains an unsafe tracked entry")
        file_count += 1
        total_bytes += entry_size
        if file_count > MAX_SOURCE_FILES or total_bytes > MAX_SOURCE_BYTES:
            raise PackageError("source snapshot exceeds its aggregate bound")
    if file_count == 0:
        raise PackageError("source snapshot is empty")


def _ensure_owned_directory(root: Path, relative: PurePosixPath) -> Path:
    destination = root
    for part in relative.parts:
        destination /= part
        try:
            os.mkdir(destination, 0o700)
        except FileExistsError:
            try:
                metadata = destination.lstat()
            except OSError as error:
                raise PackageError("owned staging directory is unavailable") from error
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
                raise PackageError("owned staging path is unsafe")
    return destination


def write_owned_file(path: Path, content: bytes, mode: int = 0o644) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = -1
    try:
        descriptor = os.open(path, flags, mode)
        os.fchmod(descriptor, mode)
        view = memoryview(content)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise OSError("short write")
            view = view[written:]
        os.fsync(descriptor)
    except OSError as error:
        raise PackageError("owned staging file could not be written") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def write_owned_relative_file(
    root_descriptor: int,
    relative: PurePosixPath,
    content: bytes,
    mode: int,
) -> None:
    safe_archive_path(relative)
    directory_flags = os.O_RDONLY
    if hasattr(os, "O_DIRECTORY"):
        directory_flags |= os.O_DIRECTORY
    if hasattr(os, "O_CLOEXEC"):
        directory_flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        directory_flags |= os.O_NOFOLLOW
    current_descriptor = os.dup(root_descriptor)
    file_descriptor = -1
    try:
        for part in relative.parts[:-1]:
            try:
                os.mkdir(part, 0o700, dir_fd=current_descriptor)
            except FileExistsError:
                pass
            next_descriptor = os.open(
                part,
                directory_flags,
                dir_fd=current_descriptor,
            )
            metadata = os.fstat(next_descriptor)
            if not stat.S_ISDIR(metadata.st_mode):
                os.close(next_descriptor)
                raise PackageError("owned staging path is unsafe")
            os.close(current_descriptor)
            current_descriptor = next_descriptor
        file_flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        if hasattr(os, "O_CLOEXEC"):
            file_flags |= os.O_CLOEXEC
        if hasattr(os, "O_NOFOLLOW"):
            file_flags |= os.O_NOFOLLOW
        file_descriptor = os.open(
            relative.name,
            file_flags,
            mode,
            dir_fd=current_descriptor,
        )
        os.fchmod(file_descriptor, mode)
        view = memoryview(content)
        while view:
            written = os.write(file_descriptor, view)
            if written <= 0:
                raise OSError("short write")
            view = view[written:]
        os.fsync(file_descriptor)
        os.fsync(current_descriptor)
    except PackageError:
        raise
    except OSError as error:
        raise PackageError("owned staging file could not be written") from error
    finally:
        if file_descriptor >= 0:
            os.close(file_descriptor)
        os.close(current_descriptor)


def snapshot_source_checkout(
    repository_root: Path,
    source_commit: str,
    destination: Path,
) -> None:
    _preflight_source_snapshot(repository_root, source_commit)
    try:
        destination.mkdir(mode=0o700)
    except OSError as error:
        raise PackageError("private source snapshot could not be created") from error
    with tempfile.TemporaryFile() as archive_file:
        _run_git_to_temporary(
            repository_root,
            archive_file,
            "archive",
            "--format=tar",
            source_commit,
            "--",
            *SOURCE_ARCHIVE_PATHS,
            timeout=60,
        )
        archive_size = os.fstat(archive_file.fileno()).st_size
        if not 0 < archive_size <= MAX_SOURCE_ARCHIVE_BYTES:
            raise PackageError("source snapshot archive exceeds its byte bound")
        archive_file.seek(0)
        seen: set[str] = set()
        total_bytes = 0
        file_count = 0
        try:
            with tarfile.open(fileobj=archive_file, mode="r:") as archive:
                for member in archive:
                    path = PurePosixPath(member.name)
                    safe_archive_path(path)
                    name = path.as_posix()
                    if name in seen or not _snapshot_path_allowed(path):
                        raise PackageError("source snapshot archive path is invalid")
                    seen.add(name)
                    relative_parent = PurePosixPath(*path.parts[:-1])
                    parent = _ensure_owned_directory(destination, relative_parent)
                    target = parent / path.name
                    if member.isdir():
                        _ensure_owned_directory(destination, path)
                        continue
                    if (
                        not member.isfile()
                        or member.size < 0
                        or member.size > MAX_SINGLE_FILE_BYTES
                    ):
                        raise PackageError("source snapshot archive entry is unsafe")
                    stream = archive.extractfile(member)
                    if stream is None:
                        raise PackageError("source snapshot archive entry is unreadable")
                    content = stream.read(member.size + 1)
                    if len(content) != member.size:
                        raise PackageError("source snapshot archive entry changed")
                    file_count += 1
                    total_bytes += len(content)
                    if (
                        file_count > MAX_SOURCE_FILES
                        or total_bytes > MAX_SOURCE_BYTES
                    ):
                        raise PackageError("source snapshot exceeds its aggregate bound")
                    mode = 0o755 if member.mode & 0o111 else 0o644
                    write_owned_file(target, content, mode)
        except (OSError, tarfile.TarError) as error:
            raise PackageError("source snapshot archive is invalid") from error
    required = (
        *SOURCE_ARCHIVE_FILES,
        PurePosixPath(".agents/skills/godot-editor/SKILL.md"),
        PurePosixPath("godot-codex-mcp/Cargo.toml"),
        PurePosixPath("godot-codex-mcp/Cargo.lock"),
        PurePosixPath("godot-codex-mcp/rust-toolchain.toml"),
        PurePosixPath("godot-codex-mcp/packaging/install.sh"),
        PurePosixPath(
            "godot-codex-mcp/packaging/generate_third_party_licenses.py"
        ),
        PurePosixPath(
            "godot-codex-mcp/product/compatibility-matrix.v1.json"
        ),
        PurePosixPath("godot-codex-mcp/product/registry-profile.v1.json"),
        PurePosixPath("godot-codex-mcp/product/THIRD_PARTY_LICENSES.txt"),
        PurePosixPath(
            "godot-codex-mcp/schemas/godot_codex/compatibility-matrix.schema.json"
        ),
        PurePosixPath(
            "godot-codex-mcp/schemas/godot_codex/doctor-report.schema.json"
        ),
    )
    for relative in required:
        read_regular_input(destination.joinpath(*relative.parts), nonempty=True)


def verify_third_party_license_bundle(workspace: Path) -> bytes:
    generator = workspace / "packaging" / "generate_third_party_licenses.py"
    bundle = workspace / "product" / "THIRD_PARTY_LICENSES.txt"
    require_regular_input(generator)
    bundle_content = read_regular_input(bundle, nonempty=True)
    try:
        result = run_bounded_process(
            [
                sys.executable,
                str(generator),
                "--workspace",
                str(workspace),
                "--output",
                str(bundle),
                "--check",
            ],
            cwd=workspace,
            env=minimal_toolchain_environment(),
            timeout=90,
        )
    except PackageError as error:
        raise PackageError(
            "third-party license bundle could not be verified"
        ) from error
    if (
        result.returncode != 0
        or not 0 < len(result.stdout) <= MAX_LICENSE_CHECK_OUTPUT_BYTES
        or len(result.stderr) > MAX_LICENSE_CHECK_OUTPUT_BYTES
    ):
        raise PackageError("third-party license bundle is stale or invalid")
    try:
        report = json.loads(result.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise PackageError(
            "third-party license verification report is invalid"
        ) from error
    if (
        not isinstance(report, dict)
        or set(report) != {"bytes", "sha256", "status"}
        or report["status"] != "PASS"
        or report["bytes"] != len(bundle_content)
        or report["sha256"] != f"sha256:{sha256_bytes(bundle_content)}"
    ):
        raise PackageError("third-party license verification report differs")
    return bundle_content


def _schema_failure() -> None:
    raise PackageError("compatibility matrix does not satisfy its committed schema")


def _resolve_schema_reference(root: dict[str, Any], reference: str) -> dict[str, Any]:
    if not reference.startswith("#/"):
        _schema_failure()
    value: Any = root
    for raw_part in reference[2:].split("/"):
        part = raw_part.replace("~1", "/").replace("~0", "~")
        if not isinstance(value, dict) or part not in value:
            _schema_failure()
        value = value[part]
    if not isinstance(value, dict):
        _schema_failure()
    return value


def _matches_schema_type(value: Any, expected: str) -> bool:
    if expected == "object":
        return isinstance(value, dict)
    if expected == "array":
        return isinstance(value, list)
    if expected == "string":
        return isinstance(value, str)
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "null":
        return value is None
    _schema_failure()
    return False


def validate_json_schema(
    value: Any,
    schema: dict[str, Any],
    root_schema: dict[str, Any] | None = None,
) -> None:
    root_schema = schema if root_schema is None else root_schema
    reference = schema.get("$ref")
    if reference is not None:
        if not isinstance(reference, str):
            _schema_failure()
        validate_json_schema(
            value,
            _resolve_schema_reference(root_schema, reference),
            root_schema,
        )
        return
    one_of = schema.get("oneOf")
    if one_of is not None:
        if not isinstance(one_of, list):
            _schema_failure()
        matches = 0
        for candidate in one_of:
            if not isinstance(candidate, dict):
                _schema_failure()
            try:
                validate_json_schema(value, candidate, root_schema)
            except PackageError:
                continue
            matches += 1
        if matches != 1:
            _schema_failure()
        return
    expected_type = schema.get("type")
    if expected_type is not None:
        if not isinstance(expected_type, str) or not _matches_schema_type(
            value, expected_type
        ):
            _schema_failure()
    if "const" in schema and value != schema["const"]:
        _schema_failure()
    enumeration = schema.get("enum")
    if enumeration is not None:
        if not isinstance(enumeration, list) or value not in enumeration:
            _schema_failure()
    if isinstance(value, dict):
        required = schema.get("required", [])
        properties = schema.get("properties", {})
        if (
            not isinstance(required, list)
            or not all(isinstance(item, str) for item in required)
            or not isinstance(properties, dict)
            or not set(required).issubset(value)
        ):
            _schema_failure()
        if schema.get("additionalProperties") is False and not set(value).issubset(
            properties
        ):
            _schema_failure()
        for key, child in value.items():
            child_schema = properties.get(key)
            if child_schema is None:
                continue
            if not isinstance(child_schema, dict):
                _schema_failure()
            validate_json_schema(child, child_schema, root_schema)
    elif isinstance(value, list):
        minimum = schema.get("minItems")
        maximum = schema.get("maxItems")
        if (
            (minimum is not None and len(value) < minimum)
            or (maximum is not None and len(value) > maximum)
        ):
            _schema_failure()
        if schema.get("uniqueItems") is True:
            encoded = [
                json.dumps(
                    item,
                    allow_nan=False,
                    ensure_ascii=False,
                    separators=(",", ":"),
                    sort_keys=True,
                )
                for item in value
            ]
            if len(encoded) != len(set(encoded)):
                _schema_failure()
        child_schema = schema.get("items")
        if child_schema is not None:
            if not isinstance(child_schema, dict):
                _schema_failure()
            for child in value:
                validate_json_schema(child, child_schema, root_schema)
    elif isinstance(value, str):
        minimum = schema.get("minLength")
        maximum = schema.get("maxLength")
        pattern = schema.get("pattern")
        if (
            (minimum is not None and len(value) < minimum)
            or (maximum is not None and len(value) > maximum)
            or (
                pattern is not None
                and (
                    not isinstance(pattern, str)
                    or re.search(pattern, value) is None
                )
            )
        ):
            _schema_failure()
    elif isinstance(value, int) and not isinstance(value, bool):
        minimum = schema.get("minimum")
        maximum = schema.get("maximum")
        if (
            (minimum is not None and value < minimum)
            or (maximum is not None and value > maximum)
        ):
            _schema_failure()


def _validate_sorted_strings(value: Any, *, maximum: int) -> list[str]:
    if (
        not isinstance(value, list)
        or len(value) > maximum
        or not all(
            isinstance(item, str)
            and 0 < len(item.encode("utf-8")) <= 512
            and not any(ord(character) < 0x20 for character in item)
            for item in value
        )
        or value != sorted(set(value))
    ):
        raise PackageError("registry profile collection is invalid")
    return value


def validate_registry_profile(registry: dict[str, Any]) -> None:
    if set(registry) != REGISTRY_FIELDS:
        raise PackageError("registry profile fields are invalid")
    if (
        registry.get("schema_version") != "godot-codex-registry-profile/1.0"
        or not isinstance(registry.get("profile_id"), str)
    ):
        raise PackageError("registry profile identity is invalid")
    tools = _validate_sorted_strings(registry.get("tools"), maximum=256)
    read_only = _validate_sorted_strings(
        registry.get("read_only_tools"), maximum=256
    )
    fixed = _validate_sorted_strings(
        registry.get("fixed_resources"), maximum=64
    )
    templates = _validate_sorted_strings(
        registry.get("resource_templates"), maximum=64
    )
    if (
        registry.get("tool_count") != len(tools)
        or registry.get("fixed_resource_count") != len(fixed)
        or registry.get("resource_template_count") != len(templates)
        or not set(read_only).issubset(tools)
    ):
        raise PackageError("registry profile counts or membership differ")
    payload = {
        "profile_id": registry["profile_id"],
        "tools": tools,
        "read_only_tools": read_only,
        "fixed_resources": fixed,
        "resource_templates": templates,
    }
    digest = sha256_bytes(
        json.dumps(
            payload,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8")
    )
    if registry.get("digest") != digest:
        raise PackageError("registry profile semantic digest differs")


def validate_host_coordinate_profile(
    workspace: Path,
    host_profile_content: bytes,
    compatibility_content: bytes,
    server_instructions_content: bytes,
    matrix: dict[str, Any],
) -> None:
    host_profile = load_json_bytes(host_profile_content)
    schema = load_json(
        workspace
        / "schemas"
        / "godot_codex"
        / "host-coordinate-profile.schema.json"
    )
    validate_json_schema(host_profile, schema)
    matrix_binding = host_profile["compatibility_matrix"]
    instructions_binding = host_profile["server_instructions"]
    expected_matrix_path = (
        "godot-codex-mcp/product/compatibility-matrix.v1.json"
    )
    expected_instructions_path = (
        "godot-codex-mcp/product/server-instructions.v1.txt"
    )
    if (
        matrix_binding["path"] != expected_matrix_path
        or matrix_binding["sha256"]
        != f"sha256:{sha256_bytes(compatibility_content)}"
        or instructions_binding["path"] != expected_instructions_path
        or instructions_binding["file_sha256"]
        != f"sha256:{sha256_bytes(server_instructions_content)}"
    ):
        raise PackageError("host coordinate raw product binding differs")
    try:
        instructions = server_instructions_content.decode("utf-8")
    except UnicodeDecodeError as error:
        raise PackageError("server instructions are not UTF-8") from error
    wire_instructions = instructions.rstrip("\n").encode("utf-8")
    if (
        not wire_instructions
        or len(wire_instructions) > 4096
        or instructions_binding["wire_sha256"]
        != f"sha256:{sha256_bytes(wire_instructions)}"
    ):
        raise PackageError("host coordinate server-instructions binding differs")
    host_surfaces = {
        surface["surface"]: surface for surface in host_profile["surfaces"]
    }
    matrix_surfaces = {
        surface["surface"]: surface
        for surface in matrix["surfaces"]
        if surface["surface"] in {"app", "cli", "ide"}
    }
    if (
        set(host_surfaces) != {"app", "cli", "ide"}
        or len(host_surfaces) != len(host_profile["surfaces"])
        or set(matrix_surfaces) != {"app", "cli", "ide"}
    ):
        raise PackageError("host coordinate surface set differs")
    for surface_name, host_surface in host_surfaces.items():
        matrix_surface = matrix_surfaces[surface_name]
        if any(
            host_surface[field] != matrix_surface[field]
            for field in (
                "host_version",
                "ide_host_version",
                "qualification",
            )
        ):
            raise PackageError("host coordinate matrix projection differs")


def validate_product_contracts(
    workspace: Path,
    matrix: dict[str, Any],
    registry: dict[str, Any],
    compatibility_content: bytes,
    additional_product_inputs: dict[str, bytes],
) -> str:
    schema = load_json(
        workspace
        / "schemas"
        / "godot_codex"
        / "compatibility-matrix.schema.json"
    )
    validate_json_schema(matrix, schema)
    validate_registry_profile(registry)
    validate_host_coordinate_profile(
        workspace,
        additional_product_inputs[HOST_PROFILE_NAME],
        compatibility_content,
        additional_product_inputs[SERVER_INSTRUCTIONS_NAME],
        matrix,
    )
    package = matrix["package"]
    godot = matrix["godot"]
    protocols = matrix["protocols"]
    registry_binding = matrix["registry"]
    expected_target = {"architecture": TARGET_ARCH, "os": TARGET_OS}
    package_version = package["version"]
    if (
        package["target"] != expected_target
        or godot["target"] != expected_target
        or not isinstance(package_version, str)
        or VERSION_RE.fullmatch(package_version) is None
        or COMMIT_RE.fullmatch(godot["source_commit"]) is None
        or DIGEST_RE.fullmatch(godot["artifact_sha256"]) is None
    ):
        raise PackageError("compatibility matrix release coordinates differ")
    if (
        registry_binding["profile_id"] != registry["profile_id"]
        or registry_binding["digest"] != registry["digest"]
        or registry_binding["tool_count"] != registry["tool_count"]
        or registry_binding["fixed_resource_count"]
        != registry["fixed_resource_count"]
        or registry_binding["resource_template_count"]
        != registry["resource_template_count"]
    ):
        raise PackageError("compatibility matrix registry binding differs")
    minimum_minor = protocols["bridge_min_minor"]
    current_minor = protocols["bridge_current_minor"]
    if minimum_minor > current_minor:
        raise PackageError("compatibility matrix Bridge range is invalid")
    profiles = matrix["bridge_profiles"]
    by_minor = {profile["bridge_minor"]: profile for profile in profiles}
    if (
        len(by_minor) != len(profiles)
        or set(by_minor) != set(range(minimum_minor, current_minor + 1))
        or len({profile["profile_id"] for profile in profiles}) != len(profiles)
    ):
        raise PackageError("compatibility matrix Bridge profiles differ")
    previous: set[str] = set()
    for minor in range(minimum_minor, current_minor + 1):
        profile = by_minor[minor]
        expected_qualification = (
            "supported" if minor == current_minor else "compatible_reduced"
        )
        capabilities = set(profile["capabilities"])
        if (
            profile["qualification"] != expected_qualification
            or not previous.issubset(capabilities)
        ):
            raise PackageError("compatibility matrix Bridge capabilities differ")
        previous = capabilities
    surface_keys: set[str] = set()
    required_surfaces: set[str] = set()
    has_cursor_not_tested = False
    for rule in matrix["surfaces"]:
        key = json.dumps(
            {
                "host_version": rule["host_version"],
                "ide_host_version": rule["ide_host_version"],
                "surface": rule["surface"],
                "target": rule["target"],
            },
            allow_nan=False,
            separators=(",", ":"),
            sort_keys=True,
        )
        if key in surface_keys:
            raise PackageError("compatibility matrix surface rule is duplicated")
        surface_keys.add(key)
        if rule["qualification"] == "supported" and rule["target"] != expected_target:
            raise PackageError("supported surface target differs")
        if rule["surface"] in {"app", "cli", "ide"}:
            if (
                rule["target"] != expected_target
                or rule["qualification"] in {"incompatible", "not_tested"}
            ):
                raise PackageError("required surface qualification differs")
            required_surfaces.add(rule["surface"])
        has_cursor_not_tested |= (
            rule["surface"] == "cursor"
            and rule["qualification"] == "not_tested"
        )
    if required_surfaces != {"app", "cli", "ide"} or not has_cursor_not_tested:
        raise PackageError("compatibility matrix required surfaces differ")
    try:
        cargo_document = tomllib.loads(
            read_regular_input(workspace / "Cargo.toml", nonempty=True).decode(
                "utf-8"
            )
        )
        cargo_version = cargo_document["workspace"]["package"]["version"]
    except (KeyError, TypeError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise PackageError("Cargo workspace version is invalid") from error
    if cargo_version != package_version:
        raise PackageError("package version differs from Cargo workspace version")
    return package_version


def cargo_build_command(workspace: Path) -> list[str]:
    return [
        "cargo",
        "build",
        "--frozen",
        "--locked",
        "--release",
        "--target",
        "aarch64-apple-darwin",
        "--manifest-path",
        str(workspace / "Cargo.toml"),
        "--package",
        "godot-codex",
        "--bin",
        "godot-codex",
        "--package",
        "godot-codex-mcp",
        "--bin",
        "godot-codex-mcp",
    ]


def _bounded_tool_output(
    command: list[str],
    workspace: Path,
    environment: dict[str, str],
) -> str:
    try:
        result = run_bounded_process(
            command,
            cwd=workspace,
            env=environment,
            timeout=10,
        )
    except PackageError as error:
        raise PackageError("Rust toolchain identity could not be read") from error
    if (
        result.returncode != 0
        or not result.stdout
        or len(result.stdout) > 16 * 1024
        or len(result.stderr) > 16 * 1024
    ):
        raise PackageError("Rust toolchain identity could not be read")
    try:
        return result.stdout.decode("utf-8").strip()
    except UnicodeDecodeError as error:
        raise PackageError("Rust toolchain identity is invalid") from error


def collect_build_provenance(workspace: Path) -> dict[str, object]:
    environment = minimal_toolchain_environment()
    cargo_version = _bounded_tool_output(
        ["cargo", "--version"],
        workspace,
        environment,
    )
    rustc_verbose = _bounded_tool_output(
        ["rustc", "--version", "--verbose"],
        workspace,
        environment,
    )
    rustc_fields: dict[str, str] = {}
    for line in rustc_verbose.splitlines():
        if ": " not in line:
            continue
        key, value = line.split(": ", 1)
        if key in rustc_fields:
            raise PackageError("Rust toolchain identity is duplicated")
        rustc_fields[key] = value
    rustc_release = rustc_fields.get("release")
    rustc_commit = rustc_fields.get("commit-hash")
    if (
        not cargo_version.startswith("cargo ")
        or len(cargo_version.encode("utf-8")) > 256
        or not isinstance(rustc_release, str)
        or not rustc_release
        or len(rustc_release.encode("utf-8")) > 128
        or not isinstance(rustc_commit, str)
        or COMMIT_RE.fullmatch(rustc_commit) is None
    ):
        raise PackageError("Rust toolchain identity is invalid")
    toolchain_content = read_regular_input(
        workspace / "rust-toolchain.toml",
        nonempty=True,
    )
    lock_content = read_regular_input(workspace / "Cargo.lock", nonempty=True)
    return {
        "cargo_lock_sha256": f"sha256:{sha256_bytes(lock_content)}",
        "cargo_version": cargo_version,
        "fresh_target": True,
        "rust_toolchain_sha256": (
            f"sha256:{sha256_bytes(toolchain_content)}"
        ),
        "rustc_commit": rustc_commit,
        "rustc_release": rustc_release,
        "target_triple": "aarch64-apple-darwin",
    }


def build_release_binaries(
    workspace: Path,
    target_directory: Path,
) -> dict[str, bytes]:
    try:
        target_directory.mkdir(mode=0o700)
    except OSError as error:
        raise PackageError("isolated Cargo target could not be created") from error
    environment = minimal_toolchain_environment()
    environment.update(
        {
            "CARGO_TARGET_DIR": str(target_directory),
        }
    )
    try:
        with (
            tempfile.TemporaryFile() as stdout,
            tempfile.TemporaryFile() as stderr,
        ):
            result = run_bounded_process(
                cargo_build_command(workspace),
                cwd=workspace,
                env=environment,
                stdout=stdout,
                stderr=stderr,
                timeout=MAX_CARGO_BUILD_SECONDS,
            )
            stdout_size = os.fstat(stdout.fileno()).st_size
            stderr_size = os.fstat(stderr.fileno()).st_size
    except PackageError as error:
        raise PackageError("isolated Cargo release build failed") from error
    if (
        result.returncode != 0
        or stdout_size > MAX_CARGO_OUTPUT_BYTES
        or stderr_size > MAX_CARGO_OUTPUT_BYTES
    ):
        raise PackageError("isolated Cargo release build failed")
    release_root = (
        target_directory / "aarch64-apple-darwin" / "release"
    )
    binaries: dict[str, bytes] = {}
    for name in ("godot-codex", "godot-codex-mcp"):
        content = read_regular_input(
            release_root / name,
            executable=True,
            nonempty=True,
        )
        require_arm64_macho_bytes(content)
        binaries[name] = content
    return binaries


def snapshot_executable(source: Path, destination: Path) -> bytes:
    content = read_regular_input(source, executable=True, nonempty=True)
    require_arm64_macho_bytes(content)
    write_owned_file(destination, content, 0o500)
    if read_regular_input(destination, executable=True, nonempty=True) != content:
        raise PackageError("private executable snapshot differs")
    return content


def verify_godot_prerequisite(
    path: Path,
    content: bytes,
    matrix: dict[str, Any],
) -> dict[str, object]:
    require_arm64_macho_bytes(content)
    godot = matrix.get("godot")
    if not isinstance(godot, dict):
        raise PackageError("compatibility matrix omits Godot coordinate")
    expected_hash = godot.get("artifact_sha256")
    expected_build = godot.get("build_id")
    expected_commit = godot.get("source_commit")
    if not all(isinstance(value, str) for value in (expected_hash, expected_build, expected_commit)):
        raise PackageError("compatibility matrix Godot coordinate is invalid")
    observed_hash = sha256_bytes(content)
    if observed_hash != expected_hash:
        raise PackageError("Godot prerequisite hash does not match the matrix")
    try:
        result = run_bounded_process(
            [str(path), "--version"],
            cwd=path.parent,
            timeout=10,
        )
    except PackageError as error:
        raise PackageError("Godot prerequisite version could not be read") from error
    try:
        version = result.stdout.decode("utf-8").strip()
    except (AttributeError, UnicodeDecodeError) as error:
        raise PackageError("Godot prerequisite version is invalid") from error
    if result.returncode != 0 or version != expected_build:
        raise PackageError("Godot prerequisite build ID does not match the matrix")
    if read_regular_input(path, executable=True, nonempty=True) != content:
        raise PackageError("Godot prerequisite changed during verification")
    return {
        "architecture": TARGET_ARCH,
        "commit": expected_commit,
        "expected_install_path": GODOT_EXPECTED_INSTALL_PATH,
        "sha256": f"sha256:{observed_hash}",
        "verification": GODOT_VERIFICATION,
        "version": expected_build,
    }


def archive_bytes(root_name: str, files: dict[str, PackageFile]) -> bytes:
    uncompressed = io.BytesIO()
    with tarfile.open(
        fileobj=uncompressed,
        mode="w",
        format=tarfile.PAX_FORMAT,
        dereference=False,
    ) as archive:
        directories: set[PurePosixPath] = {PurePosixPath(root_name)}
        for name in files:
            path = PurePosixPath(root_name) / PurePosixPath(name)
            directories.update(
                parent
                for parent in path.parents
                if parent != PurePosixPath(".")
            )
        for directory in sorted(directories, key=lambda value: (len(value.parts), value.as_posix())):
            info = tarfile.TarInfo(directory.as_posix() + "/")
            info.type = tarfile.DIRTYPE
            info.mode = 0o755
            info.uid = 0
            info.gid = 0
            info.uname = "root"
            info.gname = "root"
            info.mtime = 0
            info.pax_headers = {}
            archive.addfile(info)
        for name, package_file in sorted(files.items()):
            content = package_file.read()
            info = tarfile.TarInfo(
                (PurePosixPath(root_name) / PurePosixPath(name)).as_posix()
            )
            info.size = len(content)
            info.mode = package_file.mode
            info.uid = 0
            info.gid = 0
            info.uname = "root"
            info.gname = "root"
            info.mtime = 0
            info.pax_headers = {}
            archive.addfile(info, io.BytesIO(content))
    compressed = io.BytesIO()
    with gzip.GzipFile(
        filename="",
        mode="wb",
        fileobj=compressed,
        compresslevel=9,
        mtime=0,
    ) as stream:
        stream.write(uncompressed.getvalue())
    return compressed.getvalue()


def collect_package_files(
    repository_root: Path,
    workspace: Path,
    binaries: dict[str, bytes],
    compatibility_content: bytes,
    registry_content: bytes,
    third_party_licenses_content: bytes,
    additional_product_inputs: dict[str, bytes],
    build_provenance: dict[str, object],
    package_version: str,
    source_commit: str,
    godot_prerequisite: dict[str, object],
) -> tuple[dict[str, PackageFile], dict[str, object]]:
    files: dict[str, PackageFile] = {}
    if set(binaries) != {"godot-codex", "godot-codex-mcp"}:
        raise PackageError("release binary set differs")
    for name, content in sorted(binaries.items()):
        require_arm64_macho_bytes(content)
        add_bytes(files, f"bin/{name}", content, mode=0o755)

    add_bytes(
        files,
        "share/godot-codex/product/compatibility-matrix.v1.json",
        compatibility_content,
    )
    add_bytes(
        files,
        "share/godot-codex/product/registry-profile.v1.json",
        registry_content,
    )
    for product_name, product_content in sorted(
        additional_product_inputs.items()
    ):
        relative = PurePosixPath(product_name)
        safe_archive_path(relative)
        if len(relative.parts) != 1:
            raise PackageError("product contract path is unsafe")
        add_bytes(
            files,
            f"share/godot-codex/product/{product_name}",
            product_content,
        )
    add_tree(
        files,
        workspace / "schemas" / "godot_codex",
        "share/godot-codex/schemas/godot_codex",
    )
    add_file(
        files,
        "share/godot-codex/config/config.toml.example",
        repository_root / ".codex" / "config.toml.example",
    )
    add_file(
        files,
        "share/godot-codex/guidance/AGENTS.godot.md",
        repository_root / "docs" / "codex-integration" / "templates" / "AGENTS.godot.md",
    )
    add_file(
        files,
        "share/godot-codex/docs/EXTERNAL-CODEX-BETA-GUIDE.md",
        repository_root
        / "docs"
        / "codex-integration"
            / "EXTERNAL-CODEX-BETA-GUIDE.md",
    )
    add_file(
        files,
        "share/godot-codex/licenses/Godot-LICENSE.txt",
        repository_root / "LICENSE.txt",
    )
    if not third_party_licenses_content:
        raise PackageError("third-party license bundle is empty")
    add_bytes(
        files,
        "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt",
        third_party_licenses_content,
    )
    add_tree(
        files,
        repository_root / ".agents" / "skills" / "godot-editor",
        "share/godot-codex/skills/godot-editor",
    )
    add_file(
        files,
        "install.sh",
        workspace / "packaging" / "install.sh",
        mode=0o755,
    )
    add_bytes(files, "VERSION", f"{package_version}\n".encode())
    add_bytes(files, "SOURCE_COMMIT", f"{source_commit}\n".encode())

    content_records = []
    total_bytes = 0
    for name, package_file in sorted(files.items()):
        content = package_file.read()
        total_bytes += len(content)
        content_records.append(
            {
                "mode": f"{package_file.mode:04o}",
                "path": name,
                "sha256": f"sha256:{sha256_bytes(content)}",
                "bytes": len(content),
            }
        )
    if len(files) > MAX_FILES or total_bytes > MAX_INPUT_BYTES:
        raise PackageError("package inputs exceed their aggregate limit")

    package_manifest: dict[str, object] = {
        "schema_version": PACKAGE_SCHEMA,
        "package_version": package_version,
        "source_commit": source_commit,
        "target": {"architecture": TARGET_ARCH, "os": TARGET_OS},
        "build_provenance": build_provenance,
        "godot_prerequisite": godot_prerequisite,
        "compatibility_matrix_sha256": (
            f"sha256:{sha256_bytes(compatibility_content)}"
        ),
        "registry_sha256": f"sha256:{sha256_bytes(registry_content)}",
        "third_party_licenses_sha256": (
            f"sha256:{sha256_bytes(third_party_licenses_content)}"
        ),
        "contents": content_records,
        "checksums_path": "checksums.sha256",
    }
    manifest_bytes = canonical_json(package_manifest)
    add_bytes(files, "package-manifest.json", manifest_bytes)

    checksum_lines = []
    for name, package_file in sorted(files.items()):
        checksum_lines.append(f"{sha256_bytes(package_file.read())}  {name}\n")
    checksum_bytes = "".join(checksum_lines).encode("utf-8")
    add_bytes(files, "checksums.sha256", checksum_bytes)
    return files, package_manifest


def _path_exists(path: Path) -> bool:
    try:
        path.lstat()
    except FileNotFoundError:
        return False
    except OSError as error:
        raise PackageError("output destination could not be inspected") from error
    return True


def resolve_new_output_destination(raw_path: Path) -> Path:
    absolute = raw_path if raw_path.is_absolute() else Path.cwd() / raw_path
    if (
        absolute.name in {"", ".", ".."}
        or any(character in absolute.name for character in "\0\r\n")
    ):
        raise PackageError("output directory name is unsafe")
    try:
        parent = absolute.parent.resolve(strict=True)
        metadata = parent.lstat()
    except OSError as error:
        raise PackageError("output parent directory must already exist") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise PackageError("output parent directory is unsafe")
    destination = parent / absolute.name
    if _path_exists(destination):
        raise PackageError("output directory already exists")
    return destination


def _rename_new_atomic(
    parent_descriptor: int,
    source_name: str,
    destination_name: str,
) -> None:
    library = ctypes.CDLL(None, use_errno=True)
    source = os.fsencode(source_name)
    destination = os.fsencode(destination_name)
    if sys.platform == "darwin":
        try:
            operation = library.renameatx_np
        except AttributeError as error:
            raise PackageError(
                "exclusive atomic output publication is unavailable"
            ) from error
        operation.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        operation.restype = ctypes.c_int
        result = operation(
            parent_descriptor,
            source,
            parent_descriptor,
            destination,
            0x00000004,  # RENAME_EXCL
        )
    elif sys.platform.startswith("linux"):
        try:
            operation = library.renameat2
        except AttributeError as error:
            raise PackageError(
                "exclusive atomic output publication is unavailable"
            ) from error
        operation.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        operation.restype = ctypes.c_int
        result = operation(
            parent_descriptor,
            source,
            parent_descriptor,
            destination,
            0x00000001,  # RENAME_NOREPLACE
        )
    else:
        raise PackageError("exclusive atomic output publication is unavailable")
    if result != 0:
        error_number = ctypes.get_errno()
        if error_number in {errno.EEXIST, errno.ENOTEMPTY}:
            raise PackageError("output directory appeared during build")
        raise PackageError("output directory could not be published") from OSError(
            error_number, os.strerror(error_number)
        )


def _remove_owned_stage(path: Path, identity: tuple[int, int]) -> None:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return
    except OSError:
        return
    if (
        stat.S_ISDIR(metadata.st_mode)
        and not stat.S_ISLNK(metadata.st_mode)
        and (metadata.st_dev, metadata.st_ino) == identity
    ):
        shutil.rmtree(path)


def publish_output_tree(
    output_dir: Path,
    files: dict[str, PackageFile],
    archive_name: str,
    archive: bytes,
    detached: bytes,
) -> tuple[Path, Path]:
    parent = output_dir.parent
    flags = os.O_RDONLY
    if hasattr(os, "O_DIRECTORY"):
        flags |= os.O_DIRECTORY
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        parent_descriptor = os.open(parent, flags)
    except OSError as error:
        raise PackageError("output parent directory is unsafe") from error
    stage_name = f".godot-codex-stage-{secrets.token_hex(16)}"
    stage_path = parent / stage_name
    stage_identity: tuple[int, int] | None = None
    stage_descriptor = -1
    published = False
    try:
        try:
            os.mkdir(stage_name, 0o700, dir_fd=parent_descriptor)
            stage_descriptor = os.open(
                stage_name,
                dir_fd=parent_descriptor,
                flags=flags,
            )
            metadata = os.fstat(stage_descriptor)
        except OSError as error:
            raise PackageError("private output staging tree could not be created") from error
        if not stat.S_ISDIR(metadata.st_mode):
            raise PackageError("private output staging tree is unsafe")
        stage_identity = (metadata.st_dev, metadata.st_ino)
        for name, package_file in sorted(files.items()):
            relative = PurePosixPath(name)
            safe_archive_path(relative)
            write_owned_relative_file(
                stage_descriptor,
                relative,
                package_file.read(),
                package_file.mode,
            )
        write_owned_relative_file(
            stage_descriptor,
            PurePosixPath(archive_name),
            archive,
            0o644,
        )
        write_owned_relative_file(
            stage_descriptor,
            PurePosixPath("sprint11-package-manifest.json"),
            detached,
            0o644,
        )
        if _path_exists(output_dir):
            raise PackageError("output directory appeared during build")
        source_metadata = os.stat(
            stage_name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        if (
            not stat.S_ISDIR(source_metadata.st_mode)
            or (source_metadata.st_dev, source_metadata.st_ino)
            != stage_identity
        ):
            raise PackageError("private output staging tree changed")
        _rename_new_atomic(
            parent_descriptor,
            stage_name,
            output_dir.name,
        )
        destination_metadata = os.stat(
            output_dir.name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        if (
            not stat.S_ISDIR(destination_metadata.st_mode)
            or (destination_metadata.st_dev, destination_metadata.st_ino)
            != stage_identity
        ):
            raise PackageError("published output tree identity differs")
        os.fsync(parent_descriptor)
        published = True
    finally:
        if stage_descriptor >= 0:
            os.close(stage_descriptor)
        os.close(parent_descriptor)
        if not published and stage_identity is not None:
            _remove_owned_stage(stage_path, stage_identity)
    return (
        output_dir / archive_name,
        output_dir / "sprint11-package-manifest.json",
    )


def build(args: argparse.Namespace) -> tuple[Path, Path]:
    try:
        repository_root = args.repository_root.resolve(strict=True)
    except OSError as error:
        raise PackageError("repository root is unavailable") from error
    workspace_argument = (
        args.workspace
        if args.workspace.is_absolute()
        else Path.cwd() / args.workspace
    )
    prerequisite = (
        args.godot_prerequisite
        if args.godot_prerequisite.is_absolute()
        else Path.cwd() / args.godot_prerequisite
    )
    verify_workspace_binding(repository_root, workspace_argument)
    workspace = (repository_root / "godot-codex-mcp").resolve(strict=True)
    output_dir = resolve_new_output_destination(args.output_dir)
    verify_source_checkout(repository_root, args.source_commit)
    with tempfile.TemporaryDirectory(prefix="godot-codex-release-") as temporary:
        private_root = Path(temporary)
        snapshot_root = private_root / "source"
        snapshot_source_checkout(
            repository_root,
            args.source_commit,
            snapshot_root,
        )
        snapshot_workspace = snapshot_root / "godot-codex-mcp"
        compatibility_content = read_regular_input(
            snapshot_workspace
            / "product"
            / "compatibility-matrix.v1.json",
            nonempty=True,
        )
        registry_content = read_regular_input(
            snapshot_workspace / "product" / "registry-profile.v1.json",
            nonempty=True,
        )
        additional_product_inputs = snapshot_additional_product_inputs(
            snapshot_workspace
        )
        matrix = load_json_bytes(compatibility_content)
        registry = load_json_bytes(registry_content)
        package_version = validate_product_contracts(
            snapshot_workspace,
            matrix,
            registry,
            compatibility_content,
            additional_product_inputs,
        )
        third_party_licenses_content = verify_third_party_license_bundle(
            snapshot_workspace
        )
        build_provenance = collect_build_provenance(snapshot_workspace)
        binaries = build_release_binaries(
            snapshot_workspace,
            private_root / "cargo-target",
        )
        godot_snapshot = private_root / "godot-prerequisite"
        godot_content = snapshot_executable(prerequisite, godot_snapshot)
        godot = verify_godot_prerequisite(
            godot_snapshot,
            godot_content,
            matrix,
        )
        files, package_manifest = collect_package_files(
            snapshot_root,
            snapshot_workspace,
            binaries,
            compatibility_content,
            registry_content,
            third_party_licenses_content,
            additional_product_inputs,
            build_provenance,
            package_version,
            args.source_commit,
            godot,
        )
        root_name = (
            f"godot-codex-{package_version}-{TARGET_OS}-{TARGET_ARCH}"
        )
        archive_name = f"{root_name}.tar.gz"
        archive = archive_bytes(root_name, files)
        content_records = []
        for name, package_file in sorted(files.items()):
            content = package_file.read()
            content_records.append(
                {
                    "bytes": len(content),
                    "mode": f"{package_file.mode:04o}",
                    "path": name,
                    "sha256": f"sha256:{sha256_bytes(content)}",
                }
            )
        detached = {
            "schema_version": DETACHED_SCHEMA,
            "package_version": package_version,
            "source_commit": args.source_commit,
            "build_provenance": build_provenance,
            "archive": {
                "path": archive_name,
                "sha256": f"sha256:{sha256_bytes(archive)}",
                "bytes": len(archive),
            },
            "compatibility_matrix_sha256": package_manifest[
                "compatibility_matrix_sha256"
            ],
            "registry_sha256": package_manifest["registry_sha256"],
            "third_party_licenses_sha256": package_manifest[
                "third_party_licenses_sha256"
            ],
            "godot_prerequisite": godot,
            "contents": content_records,
        }
        return publish_output_tree(
            output_dir,
            files,
            archive_name,
            archive,
            canonical_json(detached),
        )


def parse_args() -> argparse.Namespace:
    script = Path(__file__).resolve()
    default_workspace = script.parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repository-root",
        type=Path,
        default=default_workspace.parent,
    )
    parser.add_argument("--workspace", type=Path, default=default_workspace)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--godot-prerequisite", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    try:
        archive, manifest = build(parse_args())
        print(
            json.dumps(
                {
                    "archive": archive.name,
                    "archive_sha256": f"sha256:{sha256_file(archive)}",
                    "manifest": manifest.name,
                    "manifest_sha256": f"sha256:{sha256_file(manifest)}",
                    "status": "PASS",
                },
                sort_keys=True,
            )
        )
        return 0
    except (PackageError, OSError, ValueError) as error:
        print(f"Sprint 11 package build failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
