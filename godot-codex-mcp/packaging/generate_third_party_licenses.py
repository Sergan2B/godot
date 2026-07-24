#!/usr/bin/env python3
"""Generate the deterministic Rust third-party license bundle for packaging."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Final

TARGET_TRIPLE: Final = "aarch64-apple-darwin"
CRATES_IO_SOURCE: Final = (
    "registry+https://github.com/rust-lang/crates.io-index"
)
PRODUCTION_ROOTS: Final = {
    "godot-codex": Path("crates/godot-codex/Cargo.toml"),
    "godot-codex-mcp": Path("crates/godot-codex-mcp/Cargo.toml"),
}
MAX_METADATA_BYTES: Final = 32 * 1024 * 1024
MAX_PACKAGES: Final = 512
MAX_DEPENDENCY_EDGES: Final = 8192
MAX_DEPENDENCIES_PER_NODE: Final = 512
MAX_DIRECTORY_ENTRIES: Final = 1024
MAX_LICENSE_FILES_PER_PACKAGE: Final = 16
MAX_LICENSE_FILE_BYTES: Final = 1024 * 1024
MAX_BUNDLE_BYTES: Final = 16 * 1024 * 1024
MAX_TOKEN_BYTES: Final = 256
LICENSE_PREFIXES: Final = (
    "copying",
    "copyright",
    "license",
    "notice",
    "unlicense",
)
# Some crates published from workspaces declare a license but omit the
# repository-level license file from the crates.io archive. These records bind
# an exact package identity to the corresponding upstream notice at the exact
# VCS revision recorded in the crate archive. Unknown identities stay
# fail-closed.
#
# Sources:
# https://github.com/Stranger6667/jsonschema/blob/91dac5ee04f241b543f72e83c45b407f372dac6d/LICENSE
# https://github.com/Nugine/simd/blob/d74c030d9dc4f3cae02146d1f497ff62726ef09a/LICENSE
CURATED_LICENSE_DOCUMENTS: Final = {
    (
        "jsonschema-regex",
        "0.47.0",
        "MIT",
        "crates.io registry",
    ): (
        Path(
            "packaging/upstream-licenses/"
            "Stranger6667-jsonschema-"
            "91dac5ee04f241b543f72e83c45b407f372dac6d-LICENSE.txt"
        ),
        "117829c3ca21efb132d81a44b55363d395ab8eea18526873bc828da4c0e5f038",
    ),
    (
        "uuid-simd",
        "0.8.0",
        "MIT",
        "crates.io registry",
    ): (
        Path(
            "packaging/upstream-licenses/"
            "Nugine-simd-"
            "d74c030d9dc4f3cae02146d1f497ff62726ef09a-LICENSE.txt"
        ),
        "14e66de892a0e218a4d60b2cc41a17a28080c46621d812fa2471983d8c524748",
    ),
    (
        "vsimd",
        "0.8.0",
        "MIT",
        "crates.io registry",
    ): (
        Path(
            "packaging/upstream-licenses/"
            "Nugine-simd-"
            "d74c030d9dc4f3cae02146d1f497ff62726ef09a-LICENSE.txt"
        ),
        "14e66de892a0e218a4d60b2cc41a17a28080c46621d812fa2471983d8c524748",
    ),
}
PACKAGE_NAME_RE: Final = re.compile(r"[A-Za-z0-9_-]{1,128}\Z")
PACKAGE_VERSION_RE: Final = re.compile(r"[A-Za-z0-9.+_-]{1,128}\Z")
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


class LicenseBundleError(RuntimeError):
    """Raised when the locked dependency graph cannot produce a safe bundle."""


def minimal_toolchain_environment() -> dict[str, str]:
    environment: dict[str, str] = {}
    for name in TOOLCHAIN_ENV_ALLOWLIST:
        value = os.environ.get(name)
        if value is None:
            continue
        if (
            not value
            or "\0" in value
            or len(value.encode("utf-8")) > 4096
        ):
            raise LicenseBundleError("toolchain environment is invalid")
        environment[name] = value
    if "PATH" not in environment:
        raise LicenseBundleError("toolchain environment lacks PATH")
    if "HOME" not in environment and not {
        "CARGO_HOME",
        "RUSTUP_HOME",
    }.issubset(environment):
        raise LicenseBundleError("toolchain environment lacks tool homes")
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


def canonical_sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def canonical_license_text(value: bytes) -> bytes:
    """Preserve license text while removing non-semantic line-end padding."""

    canonical = b"\n".join(
        line.rstrip(b" \t")
        for line in value.split(b"\n")
    )
    if not canonical.strip():
        raise LicenseBundleError("license input contains no text")
    return canonical


def require_regular_file(file_path: Path, *, maximum: int) -> bytes:
    try:
        flags = os.O_RDONLY
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        descriptor = os.open(file_path, flags)
    except OSError as error:
        raise LicenseBundleError("required license input is unavailable") from error
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise LicenseBundleError(
                "license input must be a regular non-symlink file"
            )
        if not 0 < metadata.st_size <= maximum:
            raise LicenseBundleError("license input exceeds its byte bound")
        with os.fdopen(descriptor, "rb") as stream:
            descriptor = -1
            value = stream.read(maximum + 1)
    except OSError as error:
        raise LicenseBundleError("license input could not be read") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    if not 0 < len(value) <= maximum:
        raise LicenseBundleError("license input exceeds its byte bound")
    if b"\0" in value:
        raise LicenseBundleError("license input is not text")
    try:
        value.decode("utf-8")
    except UnicodeDecodeError as error:
        raise LicenseBundleError("license input is not UTF-8") from error
    return value.replace(b"\r\n", b"\n").replace(b"\r", b"\n")


def cargo_metadata(workspace: Path) -> dict[str, Any]:
    environment = minimal_toolchain_environment()
    try:
        with tempfile.TemporaryFile() as metadata_output:
            result = subprocess.run(
                [
                    "cargo",
                    "metadata",
                    "--frozen",
                    "--format-version",
                    "1",
                    "--filter-platform",
                    TARGET_TRIPLE,
                    "--manifest-path",
                    str(workspace / "Cargo.toml"),
                ],
                check=False,
                cwd=workspace,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=metadata_output,
                stderr=subprocess.DEVNULL,
                timeout=60,
            )
            output_size = os.fstat(metadata_output.fileno()).st_size
            if (
                result.returncode != 0
                or not 0 < output_size <= MAX_METADATA_BYTES
            ):
                raise LicenseBundleError(
                    "frozen Cargo metadata could not be read"
                )
            metadata_output.seek(0)
            metadata_bytes = metadata_output.read(MAX_METADATA_BYTES + 1)
    except (OSError, subprocess.SubprocessError) as error:
        raise LicenseBundleError(
            "frozen Cargo metadata could not be read"
        ) from error
    if len(metadata_bytes) != output_size:
        raise LicenseBundleError("frozen Cargo metadata exceeds its byte bound")
    try:
        value = json.loads(metadata_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise LicenseBundleError("Cargo metadata is invalid") from error
    if not isinstance(value, dict):
        raise LicenseBundleError("Cargo metadata root is invalid")
    return value


def _regular_manifest_path(value: Any) -> Path:
    if not isinstance(value, str) or not value or len(value.encode()) > 4096:
        raise LicenseBundleError("Cargo manifest path is invalid")
    manifest = Path(value)
    if not manifest.is_absolute():
        raise LicenseBundleError("Cargo manifest path is not absolute")
    try:
        file_metadata = manifest.lstat()
        resolved = manifest.resolve(strict=True)
    except OSError as error:
        raise LicenseBundleError("Cargo manifest path is unavailable") from error
    if stat.S_ISLNK(file_metadata.st_mode) or not stat.S_ISREG(file_metadata.st_mode):
        raise LicenseBundleError("Cargo manifest path is unsafe")
    return resolved


def _production_root_ids(
    packages_by_id: dict[str, dict[str, Any]],
    workspace_members: set[str],
    workspace: Path,
) -> set[str]:
    roots: set[str] = set()
    for expected_name, relative_manifest in PRODUCTION_ROOTS.items():
        expected_manifest = (workspace / relative_manifest).resolve(strict=True)
        matches = [
            (package_id, package)
            for package_id, package in packages_by_id.items()
            if _regular_manifest_path(package.get("manifest_path"))
            == expected_manifest
        ]
        if len(matches) != 1:
            raise LicenseBundleError("production Cargo root is unavailable")
        package_id, package = matches[0]
        if (
            package.get("name") != expected_name
            or package_id not in workspace_members
        ):
            raise LicenseBundleError("production Cargo root identity differs")
        targets = package.get("targets")
        if (
            not isinstance(targets, list)
            or not any(
                isinstance(target, dict)
                and target.get("name") == expected_name
                and isinstance(target.get("kind"), list)
                and "bin" in target["kind"]
                for target in targets
            )
        ):
            raise LicenseBundleError("production Cargo binary target is missing")
        roots.add(package_id)
    if len(roots) != len(PRODUCTION_ROOTS):
        raise LicenseBundleError("production Cargo roots are not unique")
    return roots


def production_dependency_ids(
    metadata: dict[str, Any],
    workspace: Path,
) -> set[str]:
    packages = metadata.get("packages")
    resolve = metadata.get("resolve")
    workspace_members = metadata.get("workspace_members")
    if (
        not isinstance(packages, list)
        or not isinstance(resolve, dict)
        or not isinstance(workspace_members, list)
        or not 1 <= len(packages) <= MAX_PACKAGES
    ):
        raise LicenseBundleError("Cargo dependency graph is invalid")
    packages_by_id: dict[str, dict[str, Any]] = {}
    for package in packages:
        if (
            not isinstance(package, dict)
            or not isinstance(package.get("id"), str)
            or not package["id"]
            or package["id"] in packages_by_id
        ):
            raise LicenseBundleError("Cargo package metadata is invalid")
        packages_by_id[package["id"]] = package
    if (
        not all(isinstance(item, str) and item for item in workspace_members)
        or len(set(workspace_members)) != len(workspace_members)
        or not set(workspace_members).issubset(packages_by_id)
    ):
        raise LicenseBundleError("Cargo workspace membership is invalid")
    workspace_ids = set(workspace_members)
    root_ids = _production_root_ids(
        packages_by_id,
        workspace_ids,
        workspace,
    )
    nodes = resolve.get("nodes")
    if (
        not isinstance(nodes, list)
        or not 1 <= len(nodes) <= MAX_PACKAGES
    ):
        raise LicenseBundleError("Cargo resolution nodes are invalid")
    by_id: dict[str, dict[str, Any]] = {}
    for node in nodes:
        if (
            not isinstance(node, dict)
            or not isinstance(node.get("id"), str)
            or not node["id"]
            or node["id"] in by_id
        ):
            raise LicenseBundleError("Cargo resolution node is invalid")
        by_id[node["id"]] = node
    if set(by_id) != set(packages_by_id):
        raise LicenseBundleError("filtered Cargo resolution is not closed")

    pending = sorted(root_ids)
    scheduled = set(root_ids)
    reached: set[str] = set()
    edge_count = 0
    while pending:
        package_id = pending.pop()
        scheduled.remove(package_id)
        if package_id in reached:
            continue
        node = by_id.get(package_id)
        if not isinstance(node, dict):
            raise LicenseBundleError("Cargo resolution is incomplete")
        reached.add(package_id)
        dependencies = node.get("deps")
        if (
            not isinstance(dependencies, list)
            or len(dependencies) > MAX_DEPENDENCIES_PER_NODE
        ):
            raise LicenseBundleError("Cargo dependency node is invalid")
        for dependency in dependencies:
            edge_count += 1
            if edge_count > MAX_DEPENDENCY_EDGES:
                raise LicenseBundleError("Cargo dependency graph exceeds edge bound")
            if not isinstance(dependency, dict) or not isinstance(
                dependency.get("pkg"), str
            ):
                raise LicenseBundleError("Cargo dependency edge is invalid")
            if dependency["pkg"] not in by_id:
                raise LicenseBundleError("Cargo dependency edge is unresolved")
            kinds = dependency.get("dep_kinds")
            if not isinstance(kinds, list) or not kinds:
                raise LicenseBundleError("Cargo dependency kind is invalid")
            production_edge = False
            for kind in kinds:
                if not isinstance(kind, dict):
                    raise LicenseBundleError("Cargo dependency kind is invalid")
                kind_name = kind.get("kind")
                target = kind.get("target")
                if (
                    kind_name not in {None, "normal", "build", "dev"}
                    or (
                        target is not None
                        and (
                            not isinstance(target, str)
                            or len(target.encode()) > 4096
                        )
                    )
                ):
                    raise LicenseBundleError("Cargo dependency kind is invalid")
                production_edge = production_edge or kind_name != "dev"
            if not production_edge:
                continue
            dependency_id = dependency["pkg"]
            if dependency_id not in reached and dependency_id not in scheduled:
                pending.append(dependency_id)
                scheduled.add(dependency_id)
            if len(reached) + len(scheduled) > MAX_PACKAGES:
                raise LicenseBundleError("Cargo production closure exceeds bound")
    return reached - workspace_ids


def is_license_name(file_path: Path) -> bool:
    lowered = file_path.name.lower()
    return any(lowered.startswith(prefix) for prefix in LICENSE_PREFIXES)


def license_files(package_root: Path, declared: str | None) -> tuple[Path, ...]:
    candidates: dict[str, Path] = {}
    if declared:
        declared_path = Path(declared)
        if (
            declared_path.is_absolute()
            or not declared_path.parts
            or any(part in {"", ".", ".."} for part in declared_path.parts)
            or any(character in declared for character in "\0\r\n")
        ):
            raise LicenseBundleError("declared dependency license path is unsafe")
        candidate = package_root.joinpath(*declared_path.parts)
        try:
            resolved = candidate.resolve(strict=True)
        except OSError as error:
            raise LicenseBundleError(
                "declared dependency license is unavailable"
            ) from error
        if resolved != candidate or package_root not in resolved.parents:
            raise LicenseBundleError("declared dependency license path escapes")
        candidates[declared_path.as_posix()] = candidate
    try:
        with os.scandir(package_root) as entries:
            for index, entry in enumerate(entries, start=1):
                if index > MAX_DIRECTORY_ENTRIES:
                    raise LicenseBundleError(
                        "dependency source directory exceeds entry bound"
                    )
                candidate = package_root / entry.name
                if (
                    entry.is_file(follow_symlinks=False)
                    and is_license_name(candidate)
                ):
                    candidates[entry.name] = candidate
    except OSError as error:
        raise LicenseBundleError("dependency source directory is unavailable") from error
    if len(candidates) > MAX_LICENSE_FILES_PER_PACKAGE:
        raise LicenseBundleError("dependency has too many license inputs")
    return tuple(candidates[name] for name in sorted(candidates))


def _safe_package_token(value: Any, *, label: str, pattern: re.Pattern[str]) -> str:
    if (
        not isinstance(value, str)
        or len(value.encode("utf-8")) > MAX_TOKEN_BYTES
        or pattern.fullmatch(value) is None
    ):
        raise LicenseBundleError(f"dependency {label} is invalid")
    return value


def _safe_license_expression(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode("utf-8")) > MAX_TOKEN_BYTES
        or any(ord(character) < 0x20 or ord(character) > 0x7E for character in value)
    ):
        raise LicenseBundleError("dependency license metadata is invalid")
    return value


def _dependency_source_label(
    source: Any,
    package_root: Path,
    workspace: Path,
) -> str:
    if source == CRATES_IO_SOURCE:
        return "crates.io registry"
    if source is None:
        vendor_root = (workspace / "vendor").resolve(strict=True)
        try:
            relative = package_root.relative_to(vendor_root)
        except ValueError as error:
            raise LicenseBundleError(
                "external path dependency is outside the vendored source root"
            ) from error
        if not relative.parts or any(part in {"", ".", ".."} for part in relative.parts):
            raise LicenseBundleError("vendored dependency source path is invalid")
        relative_label = (Path("vendor") / relative).as_posix()
        if len(relative_label.encode("utf-8")) > 512:
            raise LicenseBundleError("vendored dependency source path is too long")
        return f"vendored workspace source ({relative_label})"
    raise LicenseBundleError("unsupported dependency source")


def curated_license_document(
    workspace: Path,
    identity: tuple[str, str, str, str],
) -> tuple[str, bytes] | None:
    record = CURATED_LICENSE_DOCUMENTS.get(identity)
    if record is None:
        return None
    relative_path, expected_sha256 = record
    candidate = workspace.joinpath(*relative_path.parts)
    try:
        resolved = candidate.resolve(strict=True)
    except OSError as error:
        raise LicenseBundleError(
            "curated dependency license is unavailable"
        ) from error
    if resolved != candidate or workspace not in resolved.parents:
        raise LicenseBundleError("curated dependency license path is unsafe")
    content = require_regular_file(
        candidate,
        maximum=MAX_LICENSE_FILE_BYTES,
    )
    if canonical_sha256(content) != expected_sha256:
        raise LicenseBundleError("curated dependency license digest differs")
    return (f"curated-upstream/{candidate.name}", content)


def generate_bundle(workspace: Path) -> bytes:
    workspace = workspace.resolve(strict=True)
    lock_bytes = require_regular_file(
        workspace / "Cargo.lock",
        maximum=4 * 1024 * 1024,
    )
    metadata = cargo_metadata(workspace)
    external_ids = production_dependency_ids(metadata, workspace)
    packages = metadata["packages"]
    package_by_id = {
        item["id"]: item
        for item in packages
    }
    fallback = workspace / "vendor" / "rmcp" / "LICENSE-APACHE"
    fallback_bytes = canonical_license_text(
        require_regular_file(fallback, maximum=MAX_LICENSE_FILE_BYTES)
    )
    records: list[
        tuple[str, str, str, str, tuple[tuple[str, bytes], ...]]
    ] = []
    identities: set[tuple[str, str, str]] = set()
    total_license_bytes = 0
    for package_id in sorted(external_ids):
        package = package_by_id.get(package_id)
        if not isinstance(package, dict):
            raise LicenseBundleError("resolved dependency metadata is missing")
        name = _safe_package_token(
            package.get("name"),
            label="name",
            pattern=PACKAGE_NAME_RE,
        )
        version = _safe_package_token(
            package.get("version"),
            label="version",
            pattern=PACKAGE_VERSION_RE,
        )
        license_expression = _safe_license_expression(package.get("license"))
        manifest_path = package.get("manifest_path")
        package_root = _regular_manifest_path(manifest_path).parent
        source_label = _dependency_source_label(
            package.get("source"),
            package_root,
            workspace,
        )
        identity = (name, version, source_label)
        if identity in identities:
            raise LicenseBundleError("dependency package identity is duplicated")
        identities.add(identity)
        declared_license = package.get("license_file")
        if declared_license is not None and not isinstance(declared_license, str):
            raise LicenseBundleError("declared dependency license path is invalid")
        inputs = license_files(package_root, declared_license)
        documents: list[tuple[str, bytes]] = []
        for input_file in inputs:
            relative = input_file.relative_to(package_root).as_posix()
            if (
                not relative
                or len(relative.encode("utf-8")) > 512
                or any(character in relative for character in "\0\r\n")
                or ".." in Path(relative).parts
            ):
                raise LicenseBundleError("dependency license document path is unsafe")
            content = canonical_license_text(
                require_regular_file(
                    input_file,
                    maximum=MAX_LICENSE_FILE_BYTES,
                )
            )
            total_license_bytes += len(content)
            if total_license_bytes > MAX_BUNDLE_BYTES:
                raise LicenseBundleError(
                    "dependency license inputs exceed aggregate byte bound"
                )
            documents.append(
                (
                    relative,
                    content,
                )
            )
        if not documents:
            curated = curated_license_document(
                workspace,
                (name, version, license_expression, source_label),
            )
            if curated is not None:
                fallback_name, fallback_content = curated
            elif license_expression == "Apache-2.0":
                fallback_name = "canonical-Apache-2.0.txt"
                fallback_content = fallback_bytes
            else:
                raise LicenseBundleError("dependency has no redistributable license text")
            fallback_content = canonical_license_text(fallback_content)
            total_license_bytes += len(fallback_content)
            if total_license_bytes > MAX_BUNDLE_BYTES:
                raise LicenseBundleError(
                    "dependency license inputs exceed aggregate byte bound"
                )
            documents.append((fallback_name, fallback_content))
        records.append(
            (
                name,
                version,
                license_expression,
                source_label,
                tuple(documents),
            )
        )
    records.sort(key=lambda item: (item[0], item[1], item[3]))
    output = bytearray(
        (
            "Godot Codex third-party license bundle\n"
            "Generated deterministically from the frozen macOS arm64 production dependency graph.\n"
            f"Target: {TARGET_TRIPLE}\n"
            f"Cargo.lock SHA-256: {canonical_sha256(lock_bytes)}\n"
            f"External packages: {len(records)}\n"
        ).encode("utf-8")
    )
    for name, version, license_expression, source_label, documents in records:
        output.extend(
            (
                "\n"
                + "=" * 78
                + f"\nPackage: {name} {version}\n"
                + f"Declared license: {license_expression}\n"
                + f"Source: {source_label}\n"
            ).encode("utf-8")
        )
        for document_name, content in documents:
            output.extend(
                (
                    "-" * 78
                    + f"\nLicense document: {document_name}\n"
                    + f"SHA-256: {canonical_sha256(content)}\n"
                    + "-" * 78
                    + "\n"
                ).encode("utf-8")
            )
            output.extend(content)
            if not content.endswith(b"\n"):
                output.extend(b"\n")
    if len(output) > MAX_BUNDLE_BYTES:
        raise LicenseBundleError("third-party license bundle exceeds its byte bound")
    for local_root in {workspace.as_posix(), Path.home().as_posix()}:
        if local_root and local_root.encode("utf-8") in output:
            raise LicenseBundleError(
                "third-party license bundle contains a local absolute path"
            )
    return bytes(output)


def write_atomic(output_path: Path, content: bytes) -> None:
    output_path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{output_path.name}.",
        dir=output_path.parent,
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, 0o644)
        os.replace(temporary, output_path)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> int:
    script = Path(__file__).resolve()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, default=script.parents[1])
    parser.add_argument(
        "--output",
        type=Path,
        default=script.parents[1] / "product" / "THIRD_PARTY_LICENSES.txt",
    )
    parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()
    try:
        generated = generate_bundle(arguments.workspace)
        if arguments.check:
            existing = require_regular_file(
                arguments.output,
                maximum=MAX_BUNDLE_BYTES,
            )
            if existing != generated:
                raise LicenseBundleError("committed third-party license bundle is stale")
        else:
            write_atomic(arguments.output, generated)
        print(
            json.dumps(
                {
                    "bytes": len(generated),
                    "sha256": f"sha256:{canonical_sha256(generated)}",
                    "status": "PASS",
                },
                sort_keys=True,
            )
        )
        return 0
    except (LicenseBundleError, OSError, ValueError) as error:
        print(f"Third-party license generation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
