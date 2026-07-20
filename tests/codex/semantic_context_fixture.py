#!/usr/bin/env python3
"""Validate the independent Sprint 6 semantic-context fixture and oracle."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
PROJECT_ROOT = SCRIPT_DIR / "fixtures" / "semantic_context_project"
ORACLE_ROOT = SCRIPT_DIR / "fixtures" / "semantic_context_oracle"
MANIFEST_PATH = ORACLE_ROOT / "fixture-manifest.json"
GOLDEN_PATH = ORACLE_ROOT / "golden-usages.json"
FORBIDDEN_MATERIAL = ("/Users/", "C:\\", ".godot/imported", ".godot/codex")
COVERAGE = {
    "bounded_summaries",
    "conflict_retention",
    "duplicate_provenance",
    "node_usage",
    "partial_domains",
    "resource_uid_rename",
    "scene_instance",
    "signal_connection",
    "symbol_call",
    "symbol_override",
}
PHASES = (
    "base",
    "resource_uid_rename",
    "symbol_rename",
    "duplicate_provenance",
    "partial_domains",
    "conflict",
    "budget_pressure",
    "journal_gap",
)


class FixtureError(RuntimeError):
    """Raised when committed Sprint 6 truth is inconsistent."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise FixtureError(message)


def strict_json_load(path: Path) -> dict[str, Any]:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise FixtureError(f"duplicate JSON member in {path.name}: {key}")
            result[key] = value
        return result

    try:
        value = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=reject_duplicates)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise FixtureError(f"cannot read strict JSON {path.name}") from error
    require(isinstance(value, dict), f"{path.name} root must be an object")
    return value


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixture_files() -> list[str]:
    return sorted(
        path.relative_to(PROJECT_ROOT).as_posix()
        for path in PROJECT_ROOT.rglob("*")
        if path.is_file() and ".godot" not in path.parts
    )


def validate_manifest(manifest: Mapping[str, Any]) -> None:
    require(manifest.get("fixture_version") == 1, "fixture version differs")
    require(manifest.get("project") == "res://", "fixture project root differs")
    records = manifest.get("files")
    require(isinstance(records, list), "fixture files are missing")
    paths: list[str] = []
    for record in records:
        require(isinstance(record, dict), "fixture file record is malformed")
        path = record.get("path")
        digest = record.get("sha256")
        require(
            isinstance(path, str)
            and path
            and not path.startswith("/")
            and "\\" not in path
            and ".." not in Path(path).parts,
            "fixture path is unsafe",
        )
        require(isinstance(digest, str) and len(digest) == 64, f"fixture hash is malformed: {path}")
        candidate = PROJECT_ROOT / path
        require(candidate.is_file(), f"fixture file is missing: {path}")
        require(sha256_file(candidate) == digest, f"fixture hash differs: {path}")
        paths.append(path)
    require(paths == sorted(set(paths)), "fixture paths are not canonical")
    require(paths == fixture_files(), "fixture manifest does not close over project files")
    require(manifest.get("mutations") == [
        "none",
        "rename_resource_keep_uid",
        "rename_symbol",
        "duplicate_provenance",
        "disable_script_domain",
        "inject_conflict",
        "add_budget_pressure",
        "force_journal_gap",
    ], "fixture mutations differ")


def validate_golden(golden: Mapping[str, Any]) -> None:
    require(golden.get("schema_version") == 1, "golden schema differs")
    require(set(golden.get("coverage", [])) == COVERAGE, "golden coverage differs")
    require(golden.get("phases") == list(PHASES), "golden phases differ")
    require(golden.get("false_exact_allowed") == 0, "false exact policy differs")
    require(golden.get("budgets") == {
        "method": "utf8_byte_upper_bound_v1",
        "project": 4096,
        "scene": 2048,
    }, "summary budgets differ")
    queries = golden.get("queries")
    require(isinstance(queries, list) and len(queries) >= 4, "golden queries are incomplete")
    query_ids: set[str] = set()
    usage_ids: set[str] = set()
    resolvable = 0
    for query in queries:
        require(isinstance(query, dict), "golden query is malformed")
        query_id = query.get("oracle_id")
        require(isinstance(query_id, str) and query_id not in query_ids, "duplicate query ID")
        query_ids.add(query_id)
        expected = query.get("expected")
        require(isinstance(expected, list) and expected, f"query truth is empty: {query_id}")
        for usage in expected:
            require(isinstance(usage, dict), "usage truth is malformed")
            usage_id = usage.get("oracle_id")
            require(isinstance(usage_id, str) and usage_id not in usage_ids, "duplicate usage ID")
            usage_ids.add(usage_id)
            confidence = usage.get("confidence")
            require(confidence in {"exact", "probable", "dynamic", "runtime_confirmed"}, "usage confidence differs")
            require(isinstance(usage.get("evidence_source"), str), "usage evidence is missing")
            if confidence == "exact":
                resolvable += 1
    require(resolvable >= 6, "resolvable usage truth is incomplete")
    dedup = golden.get("dedup_vectors")
    require(isinstance(dedup, list) and len(dedup) == 1, "dedup truth differs")
    sources = dedup[0].get("expected_evidence_sources")
    require(isinstance(sources, list) and len(set(sources)) >= 2, "dedup provenance is incomplete")
    conflicts = golden.get("conflict_vectors")
    require(isinstance(conflicts, list) and len(conflicts) == 1, "conflict truth differs")
    facts = conflicts[0].get("facts")
    require(
        conflicts[0].get("single_valued") is True
        and isinstance(facts, list)
        and len({fact.get("value") for fact in facts if isinstance(fact, dict)}) >= 2,
        "conflict values are not retained",
    )


def run_draft_schema_tests() -> None:
    cargo = shutil.which("cargo")
    if cargo is None:
        raise FixtureError("cargo is required for Draft 2020-12 validation")
    result = subprocess.run(
        [
            cargo,
            "test",
            "--manifest-path",
            str(SCRIPT_DIR / "Cargo.toml"),
            "--locked",
            "--offline",
            "semantic_context_fixture",
            "--",
            "--nocapture",
        ],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if result.returncode != 0:
        raise FixtureError("Draft 2020-12 semantic-context schema tests failed:\n" + result.stdout + result.stderr)


def validate_all(run_schemas: bool = True) -> dict[str, Any]:
    manifest = strict_json_load(MANIFEST_PATH)
    golden = strict_json_load(GOLDEN_PATH)
    validate_manifest(manifest)
    validate_golden(golden)
    serialized = json.dumps({"manifest": manifest, "golden": golden}, ensure_ascii=False)
    require(not any(value in serialized for value in FORBIDDEN_MATERIAL), "oracle leaks forbidden material")
    if run_schemas:
        run_draft_schema_tests()
    return {
        "schema_version": 1,
        "fixture_version": 1,
        "files": len(manifest["files"]),
        "phases": len(golden["phases"]),
        "queries": len(golden["queries"]),
        "expected_usages": sum(len(query["expected"]) for query in golden["queries"]),
        "manifest_sha256": sha256_file(MANIFEST_PATH),
        "golden_sha256": sha256_file(GOLDEN_PATH),
        "status": "passed",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-schemas", action="store_true")
    arguments = parser.parse_args()
    try:
        result = validate_all(run_schemas=not arguments.no_schemas)
    except (FixtureError, OSError, subprocess.SubprocessError) as error:
        print(f"Sprint 6 fixture validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
