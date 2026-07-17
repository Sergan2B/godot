#!/usr/bin/env python3
"""Create and verify deterministic Sprint 3 host-transfer receipts."""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Any, Sequence

FREEZE_COMMIT = "1144694d2293af2ff80d72a47e622479e51ee6f6"
SOURCE_SHA256 = "sha256:46ef87410e472814f20cac0ff6d32a6b4634a90a28c5d378753a04006bc7dd2f"
ORACLE_SHA256 = "sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b"

PLATFORM_FILES = {
    "linux-x86_64": ("sprint-3-storage-spike-linux.json",),
    "windows-x86_64": (
        "sprint-3-resource-graph-windows.json",
        "sprint-3-storage-spike-windows.json",
    ),
}
PLATFORM_RUNNERS = {
    "linux-x86_64": "sprint3_linux_x86_64.sh",
    "windows-x86_64": "sprint3_windows_x86_64.ps1",
}
PLATFORM_RECEIPTS = {
    "linux-x86_64": "sprint-3-linux.receipt.json",
    "windows-x86_64": "sprint-3-windows.receipt.json",
}

TOP_LEVEL_FIELDS = {
    "files",
    "manifest_helper",
    "oracle_sha256",
    "platform",
    "runner",
    "schema_version",
    "source_freeze_commit",
    "source_tree_sha256",
}
IDENTITY_FIELDS = {"name", "sha256"}
FILE_FIELDS = {"bytes", "name", "sha256"}


class ManifestError(RuntimeError):
    """Raised when a transfer receipt is incomplete or inconsistent."""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return f"sha256:{digest.hexdigest()}"


def require_regular_file(path: Path, label: str) -> Path:
    resolved = path.resolve(strict=True)
    if path.is_symlink() or not resolved.is_file():
        raise ManifestError(f"{label} must be a regular non-symlink file: {path}")
    return resolved


def reject_duplicate_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ManifestError(f"duplicate JSON member in transfer receipt: {key}")
        result[key] = value
    return result


def load_receipt(path: Path) -> dict[str, Any]:
    receipt_path = require_regular_file(path, "transfer receipt")
    try:
        parsed = json.loads(receipt_path.read_text(encoding="utf-8"), object_pairs_hook=reject_duplicate_members)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ManifestError(f"unable to parse transfer receipt: {exc}") from exc
    if not isinstance(parsed, dict):
        raise ManifestError("transfer receipt root must be an object")
    return parsed


def expected_platform(platform: str) -> tuple[tuple[str, ...], str, str]:
    try:
        return PLATFORM_FILES[platform], PLATFORM_RUNNERS[platform], PLATFORM_RECEIPTS[platform]
    except KeyError as exc:
        raise ManifestError(f"unsupported transfer platform: {platform}") from exc


def normalized_inputs(platform: str, files: Sequence[Path]) -> list[Path]:
    expected_files, _, _ = expected_platform(platform)
    by_name: dict[str, Path] = {}
    for path in files:
        file_path = require_regular_file(path, "transferred evidence")
        if file_path.name in by_name:
            raise ManifestError(f"duplicate transferred evidence filename: {file_path.name}")
        by_name[file_path.name] = file_path
    if set(by_name) != set(expected_files):
        raise ManifestError(f"{platform} requires evidence files {list(expected_files)}, got {sorted(by_name)}")
    return [by_name[name] for name in expected_files]


def identity(path: Path) -> dict[str, str]:
    return {"name": path.name, "sha256": sha256(path)}


def create_receipt(platform: str, runner: Path, output: Path, files: Sequence[Path]) -> dict[str, Any]:
    expected_files, expected_runner, expected_receipt = expected_platform(platform)
    runner_path = require_regular_file(runner, "platform runner")
    if runner_path.name != expected_runner:
        raise ManifestError(f"{platform} requires runner {expected_runner}, got {runner_path.name}")
    if output.name != expected_receipt:
        raise ManifestError(f"{platform} receipt must be named {expected_receipt}")
    if output.exists() or output.is_symlink():
        raise ManifestError(f"refusing to overwrite transfer receipt: {output}")

    evidence_files = normalized_inputs(platform, files)
    receipt: dict[str, Any] = {
        "files": [
            {"bytes": path.stat().st_size, "name": name, "sha256": sha256(path)}
            for name, path in zip(expected_files, evidence_files)
        ],
        "manifest_helper": identity(Path(__file__).resolve()),
        "oracle_sha256": ORACLE_SHA256,
        "platform": platform,
        "runner": identity(runner_path),
        "schema_version": 1,
        "source_freeze_commit": FREEZE_COMMIT,
        "source_tree_sha256": SOURCE_SHA256,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(receipt, stream, indent=2, sort_keys=True)
        stream.write("\n")
    return receipt


def require_exact_fields(value: Any, expected: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != expected:
        actual = sorted(value) if isinstance(value, dict) else type(value).__name__
        raise ManifestError(f"{label} fields differ: {actual}")
    return value


def verify_identity(value: Any, path: Path, label: str) -> None:
    record = require_exact_fields(value, IDENTITY_FIELDS, label)
    if record["name"] != path.name or record["sha256"] != sha256(path):
        raise ManifestError(f"{label} identity differs from {path.name}")


def verify_receipt(platform: str, runner: Path, receipt: Path, files: Sequence[Path]) -> dict[str, Any]:
    expected_files, expected_runner, expected_receipt = expected_platform(platform)
    runner_path = require_regular_file(runner, "platform runner")
    if runner_path.name != expected_runner:
        raise ManifestError(f"{platform} requires runner {expected_runner}, got {runner_path.name}")
    if receipt.name != expected_receipt:
        raise ManifestError(f"{platform} receipt must be named {expected_receipt}")
    evidence_files = normalized_inputs(platform, files)
    record = require_exact_fields(load_receipt(receipt), TOP_LEVEL_FIELDS, "transfer receipt")

    if type(record["schema_version"]) is not int or record["schema_version"] != 1:
        raise ManifestError("transfer receipt schema_version must be integer 1")
    if record["platform"] != platform:
        raise ManifestError("transfer receipt platform differs")
    if record["source_freeze_commit"] != FREEZE_COMMIT:
        raise ManifestError("transfer receipt source freeze differs")
    if record["source_tree_sha256"] != SOURCE_SHA256:
        raise ManifestError("transfer receipt source digest differs")
    if record["oracle_sha256"] != ORACLE_SHA256:
        raise ManifestError("transfer receipt oracle digest differs")

    verify_identity(record["runner"], runner_path, "runner")
    verify_identity(record["manifest_helper"], Path(__file__).resolve(), "manifest helper")

    file_records = record["files"]
    if not isinstance(file_records, list) or len(file_records) != len(expected_files):
        raise ManifestError("transfer receipt has the wrong file count")
    for expected_name, path, raw_file_record in zip(expected_files, evidence_files, file_records):
        file_record = require_exact_fields(raw_file_record, FILE_FIELDS, f"file {expected_name}")
        if file_record["name"] != expected_name:
            raise ManifestError(f"transfer receipt file order/name differs at {expected_name}")
        if type(file_record["bytes"]) is not int or file_record["bytes"] != path.stat().st_size:
            raise ManifestError(f"transferred evidence size differs: {expected_name}")
        if file_record["sha256"] != sha256(path):
            raise ManifestError(f"transferred evidence SHA-256 differs: {expected_name}")
    return record


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    for command in ("create", "verify"):
        command_parser = subparsers.add_parser(command)
        command_parser.add_argument("--platform", choices=sorted(PLATFORM_FILES), required=True)
        command_parser.add_argument("--runner", type=Path, required=True)
        command_parser.add_argument("--receipt", type=Path, required=True)
        command_parser.add_argument("--file", action="append", type=Path, required=True)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.command == "create":
            create_receipt(args.platform, args.runner, args.receipt, args.file)
            print(f"transfer receipt created: {args.receipt}")
        else:
            verify_receipt(args.platform, args.runner, args.receipt, args.file)
            print(f"transfer receipt passed: {args.receipt}")
    except (ManifestError, OSError) as exc:
        print(f"transfer receipt failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
