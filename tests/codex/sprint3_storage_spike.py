#!/usr/bin/env python3
"""Run the Sprint 3 storage spike without coupling validation to package bumps."""

from __future__ import annotations

import copy
import re
import tomllib


class StorageSpikeLauncherError(RuntimeError):
    """The isolated Sprint 3 storage-spike launcher could not be prepared."""


def rewrite_path_package_version(
    lock_bytes: bytes,
    package_name: str,
    version: str,
) -> bytes:
    """Refresh one exact local path-package version without changing other lock data."""

    if not re.fullmatch(
        r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)",
        version,
    ):
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
