#!/usr/bin/env python3
"""No-follow, atomic output helpers for Sprint 11 evidence acquisition."""

from __future__ import annotations

import ctypes
import errno
import hashlib
import os
import stat
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final

PRIVATE_DIRECTORY_MODE: Final = 0o700
AT_FDCWD: Final = -100
RENAME_NOREPLACE: Final = 1
RENAME_EXCL: Final = 0x00000004


class AcquisitionPathError(ValueError):
    """Raised when an acquisition destination is not a safe new path."""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise AcquisitionPathError(message)


def _lexical_absolute(path: Path) -> Path:
    _require(path.name not in {"", ".", ".."}, "output leaf differs")
    return Path(os.path.abspath(os.fspath(path)))


def _require_no_symlink_below(path: Path, root: Path) -> None:
    """Reject attacker-controlled indirection below one canonical root."""

    absolute = _lexical_absolute(path)
    canonical_root = root.resolve(strict=True)
    try:
        relative = absolute.relative_to(canonical_root)
    except ValueError as error:
        raise AcquisitionPathError("output path escapes its trusted root") from error
    current = canonical_root
    for part in relative.parts:
        current /= part
        try:
            metadata = current.lstat()
        except FileNotFoundError as error:
            raise AcquisitionPathError("output ancestor is missing") from error
        except OSError as error:
            raise AcquisitionPathError("output ancestor is unavailable") from error
        _require(not stat.S_ISLNK(metadata.st_mode), "output ancestor is a symlink")
        _require(stat.S_ISDIR(metadata.st_mode), "output ancestor is not a directory")


def _new_target(path: Path) -> tuple[Path, Path]:
    lexical = _lexical_absolute(path)
    try:
        parent = lexical.parent.resolve(strict=True)
        parent_metadata = parent.lstat()
    except OSError as error:
        raise AcquisitionPathError("output ancestor is unavailable") from error
    _require(
        stat.S_ISDIR(parent_metadata.st_mode) and not parent.is_symlink(),
        "resolved output ancestor is not a regular directory",
    )
    target = parent / lexical.name
    try:
        target.lstat()
    except FileNotFoundError:
        pass
    except OSError as error:
        raise AcquisitionPathError("output target is unavailable") from error
    else:
        raise AcquisitionPathError("output target must be new")
    return target, parent


def _inside(path: Path, root: Path) -> bool:
    return path == root or root in path.parents


def _open_regular_no_follow(path: Path, *, maximum: int) -> tuple[int, os.stat_result]:
    _require(maximum >= 0, "regular-file byte bound differs")
    no_follow = getattr(os, "O_NOFOLLOW", None)
    _require(no_follow is not None, "no-follow file open is unavailable")
    flags = os.O_RDONLY | no_follow
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise AcquisitionPathError("regular file cannot be opened safely") from error
    try:
        metadata = os.fstat(descriptor)
        _require(
            stat.S_ISREG(metadata.st_mode)
            and 0 <= metadata.st_size <= maximum,
            "regular file identity or byte bound differs",
        )
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor, metadata


def _verify_regular_file_after_read(
    path: Path,
    descriptor: int,
    before: os.stat_result,
    total: int,
) -> None:
    after = os.fstat(descriptor)
    try:
        path_after = path.lstat()
    except OSError as error:
        raise AcquisitionPathError("regular file disappeared during read") from error
    identity = (
        "st_dev",
        "st_ino",
        "st_mode",
        "st_uid",
        "st_size",
        "st_mtime_ns",
        "st_ctime_ns",
    )
    _require(
        total == before.st_size
        and all(getattr(after, field) == getattr(before, field) for field in identity)
        and all(
            getattr(path_after, field) == getattr(before, field)
            for field in identity
        ),
        "regular file changed or was swapped during read",
    )


def read_regular_file(path: Path, *, maximum: int) -> bytes:
    """Read one bounded regular file through an O_NOFOLLOW descriptor."""

    descriptor, before = _open_regular_no_follow(path, maximum=maximum)
    chunks: list[bytes] = []
    total = 0
    try:
        while True:
            chunk = os.read(descriptor, min(1024 * 1024, maximum - total + 1))
            if not chunk:
                break
            total += len(chunk)
            _require(total <= maximum, "regular file exceeds its byte bound")
            chunks.append(chunk)
        _verify_regular_file_after_read(path, descriptor, before, total)
    finally:
        os.close(descriptor)
    return b"".join(chunks)


def update_digest_from_regular_file(
    digest: Any,
    path: Path,
    *,
    maximum: int,
) -> int:
    """Stream one stable regular file into an existing hashlib-compatible digest."""

    descriptor, before = _open_regular_no_follow(path, maximum=maximum)
    total = 0
    try:
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            total += len(chunk)
            _require(total <= maximum, "regular file exceeds its byte bound")
            digest.update(chunk)
        _verify_regular_file_after_read(path, descriptor, before, total)
    finally:
        os.close(descriptor)
    return total


def sha256_regular_file(path: Path, *, maximum: int) -> str:
    digest = hashlib.sha256()
    update_digest_from_regular_file(digest, path, maximum=maximum)
    return "sha256:" + digest.hexdigest()


def _exclusive_rename(source: Path, target: Path) -> None:
    """Atomically publish *source* while refusing every existing target."""

    try:
        before = source.lstat()
    except OSError as error:
        raise AcquisitionPathError("staged output is unavailable") from error
    _require(
        not stat.S_ISLNK(before.st_mode)
        and (
            stat.S_ISREG(before.st_mode)
            or stat.S_ISDIR(before.st_mode)
        )
        and before.st_uid == os.geteuid(),
        "staged output identity differs",
    )
    source_bytes = os.fsencode(source)
    target_bytes = os.fsencode(target)
    library = ctypes.CDLL(None, use_errno=True)
    result: int
    if sys.platform == "darwin":
        renamex_np = library.renamex_np
        renamex_np.argtypes = [
            ctypes.c_char_p,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        renamex_np.restype = ctypes.c_int
        result = renamex_np(source_bytes, target_bytes, RENAME_EXCL)
    elif sys.platform.startswith("linux") and hasattr(library, "renameat2"):
        renameat2 = library.renameat2
        renameat2.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        renameat2.restype = ctypes.c_int
        result = renameat2(
            AT_FDCWD,
            source_bytes,
            AT_FDCWD,
            target_bytes,
            RENAME_NOREPLACE,
        )
    elif stat.S_ISREG(before.st_mode):
        try:
            os.link(source, target, follow_symlinks=False)
            source.unlink()
        except FileExistsError as error:
            raise AcquisitionPathError("output target already exists") from error
        except OSError as error:
            raise AcquisitionPathError(
                "exclusive output publication is unavailable"
            ) from error
        result = 0
    else:
        raise AcquisitionPathError(
            "exclusive directory publication is unavailable on this platform"
        )
    if result != 0:
        failure = ctypes.get_errno()
        if failure in {errno.EEXIST, errno.ENOTEMPTY}:
            raise AcquisitionPathError("output target already exists")
        raise AcquisitionPathError(
            f"exclusive output publication failed with errno {failure}"
        )
    try:
        after = target.lstat()
    except OSError as error:
        raise AcquisitionPathError("published output is unavailable") from error
    _require(
        (after.st_dev, after.st_ino, after.st_mode, after.st_uid)
        == (before.st_dev, before.st_ino, before.st_mode, before.st_uid),
        "published output identity differs",
    )


@dataclass(frozen=True)
class AtomicDirectory:
    """A private sibling stage and its new final destination."""

    target: Path
    staging: Path

    def publish(self) -> None:
        _require(self.staging.parent == self.target.parent, "staging sibling differs")
        _exclusive_rename(self.staging, self.target)


@dataclass(frozen=True)
class StagedFile:
    """A path an external producer may write before one atomic publication."""

    target: Path
    staging_directory: Path
    staged_path: Path

    def publish(self) -> None:
        metadata = self.staged_path.lstat()
        _require(
            stat.S_ISREG(metadata.st_mode) and not self.staged_path.is_symlink(),
            "staged output is not a regular file",
        )
        _exclusive_rename(self.staged_path, self.target)
        self.staging_directory.rmdir()


def prepare_staged_file(path: Path, *, prefix: str) -> StagedFile:
    """Create a private sibling directory for an external file producer."""

    target, parent = _new_target(path)
    staging = Path(tempfile.mkdtemp(prefix=prefix, dir=parent))
    os.chmod(staging, PRIVATE_DIRECTORY_MODE)
    metadata = staging.lstat()
    _require(
        stat.S_ISDIR(metadata.st_mode)
        and not staging.is_symlink()
        and metadata.st_uid == os.geteuid(),
        "private staging directory ownership differs",
    )
    return StagedFile(
        target=target,
        staging_directory=staging,
        staged_path=staging / target.name,
    )


def prepare_repository_directory(
    path: Path,
    *,
    repository: Path,
    prefix: str,
) -> AtomicDirectory:
    """Reserve a private sibling stage for a new repository-local directory."""

    target, parent = _new_target(path)
    root = repository.resolve(strict=True)
    _require_no_symlink_below(_lexical_absolute(path).parent, root)
    _require(_inside(target, root), "output target must be repository-local")
    staging = Path(tempfile.mkdtemp(prefix=prefix, dir=parent))
    os.chmod(staging, PRIVATE_DIRECTORY_MODE)
    metadata = staging.lstat()
    _require(
        stat.S_ISDIR(metadata.st_mode)
        and not staging.is_symlink()
        and metadata.st_uid == os.geteuid(),
        "private staging directory ownership differs",
    )
    return AtomicDirectory(target=target, staging=staging)


def create_external_build_directory(path: Path, *, repository: Path) -> Path:
    """Create one new private build directory strictly outside the repository."""

    target, _parent = _new_target(path)
    root = repository.resolve(strict=True)
    _require(not _inside(target, root), "build root must be outside the repository")
    target.mkdir(mode=PRIVATE_DIRECTORY_MODE)
    metadata = target.lstat()
    _require(
        stat.S_ISDIR(metadata.st_mode)
        and not target.is_symlink()
        and metadata.st_uid == os.geteuid(),
        "private build directory ownership differs",
    )
    return target


def atomic_write_new_repository_file(
    path: Path,
    payload: bytes,
    *,
    repository: Path,
    prefix: str,
) -> Path:
    """Publish a new repository-local file through a private sibling file."""

    target, parent = _new_target(path)
    root = repository.resolve(strict=True)
    _require(_inside(target, root), "output file must be repository-local")
    return _atomic_write_new_file(
        target,
        parent,
        payload,
        prefix=prefix,
    )


def atomic_write_new_file(path: Path, payload: bytes, *, prefix: str) -> Path:
    """Publish a new file without following any destination-parent symlink."""

    target, parent = _new_target(path)
    return _atomic_write_new_file(target, parent, payload, prefix=prefix)


def _atomic_write_new_file(
    target: Path,
    parent: Path,
    payload: bytes,
    *,
    prefix: str,
) -> Path:
    descriptor, temporary_name = tempfile.mkstemp(prefix=prefix, dir=parent)
    temporary = Path(temporary_name)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        _exclusive_rename(temporary, target)
    except BaseException:
        try:
            os.close(descriptor)
        except OSError:
            pass
        temporary.unlink(missing_ok=True)
        raise
    return target
