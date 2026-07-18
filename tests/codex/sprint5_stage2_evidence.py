#!/usr/bin/env python3
"""Produce and validate fail-closed evidence for Sprint 5 gate S5-02."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, cast

import script_semantics_fixture as fixture
import sprint5_stage1_evidence as stage1

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint5_stage2_source_scopes.txt"
EVIDENCE_PATH = SCRIPT_DIR / "evidence" / "sprint-5-stage-2-oracle.json"
BASELINE = "923a7855a5ff4698a6e99720f4645e7d10984e43"

CHECKS = (
    "archived_s5_01_evidence",
    "source_scope_hash",
    "strict_json",
    "draft_2020_12_schemas",
    "named_negative_schema_cases",
    "manifest_hash_closure",
    "canonical_phase_matrix",
    "document_state_matrix",
    "content_bound_ranges_python_rust_cpp",
    "canonical_symbol_ids_python_rust",
    "relation_reference_closure",
    "zero_false_exact_truth",
    "dynamic_target_separation",
    "diagnostic_isolation",
    "csharp_discovery_only",
    "scene_attachment_replace_remove",
    "godot_parser_valid_broken_matrix",
    "oracle_independence",
    "evidence_redaction",
)
TOP_LEVEL_FIELDS = {
    "schema_version",
    "sprint",
    "stage",
    "baseline",
    "platform",
    "source",
    "fixture",
    "godot_parser",
    "artifacts",
    "toolchain",
    "checks",
    "redaction",
    "remote_ci",
    "status",
}
SOURCE_FIELDS = {"scope_manifest", "source_scope_sha256", "file_count"}
FIXTURE_FIELDS = {
    "schema_version",
    "fixture_version",
    "manifest_sha256",
    "golden_sha256",
    "canonical_symbol_ids_sha256",
    "files",
    "phases",
    "coverage_cases",
    "documents",
    "resources",
    "ranges",
    "symbols",
    "relations",
    "diagnostics",
    "attachments",
}
PARSER_FIELDS = {
    "schema_version",
    "fixture_files_loaded_absolute",
    "gdscript_valid_documents",
    "gdscript_parse_error_documents",
    "csharp_discovery_documents",
    "attachment_scenes",
}
FORBIDDEN_MATERIAL = ("/Users/", "C:\\", ".godot/imported", ".godot/codex")


class EvidenceError(RuntimeError):
    """Raised when S5-02 evidence is incomplete or inconsistent."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise EvidenceError(message)


def strict_json_load(path: Path) -> dict[str, Any]:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise EvidenceError(f"duplicate JSON member: {key}")
            result[key] = value
        return result

    try:
        value = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=reject_duplicates)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"cannot read strict evidence: {path.name}") from error
    require(isinstance(value, dict), "evidence root must be an object")
    return cast(dict[str, Any], value)


def sha256_file(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def _scope_lines(payload: bytes) -> tuple[str, ...]:
    try:
        values = tuple(line for line in payload.decode("utf-8").splitlines() if line and not line.startswith("#"))
    except UnicodeError as error:
        raise EvidenceError("S5-02 source-scope manifest is not UTF-8") from error
    require(values == tuple(sorted(values)), "S5-02 source scopes must be sorted")
    require(len(values) == len(set(values)) and bool(values), "S5-02 source scopes must be unique")
    require("tests/codex/sprint5_stage2_source_scopes.txt" in values, "S5-02 scope manifest must bind itself")
    require(
        "tests/codex/evidence/sprint-5-stage-2-oracle.json" not in values,
        "generated S5-02 evidence must not create a circular source hash",
    )
    for value in values:
        path = Path(value)
        require(
            not path.is_absolute() and ".." not in path.parts and "\\" not in value,
            f"unsafe S5-02 source scope: {value}",
        )
    return values


def current_source_files() -> tuple[str, ...]:
    scopes = _scope_lines(SOURCE_SCOPE_PATH.read_bytes())
    expanded: set[str] = set()
    for scope in scopes:
        path = REPOSITORY_ROOT / scope
        require(path.exists(), f"S5-02 source scope is missing: {scope}")
        if path.is_file():
            expanded.add(scope)
        else:
            for candidate in path.rglob("*"):
                if candidate.is_file() and ".godot" not in candidate.parts:
                    expanded.add(candidate.relative_to(REPOSITORY_ROOT).as_posix())
    require(bool(expanded), "S5-02 source scope expands to no files")
    return tuple(sorted(expanded))


def _coordinate(paths: tuple[str, ...], reader: Any) -> dict[str, Any]:
    digest = hashlib.sha256()
    for path in paths:
        digest.update(path.encode("utf-8"))
        digest.update(b"\0")
        digest.update(reader(path))
        digest.update(b"\0")
    return {
        "scope_manifest": "tests/codex/sprint5_stage2_source_scopes.txt",
        "source_scope_sha256": "sha256:" + digest.hexdigest(),
        "file_count": len(paths),
    }


def current_source_coordinate() -> dict[str, Any]:
    return _coordinate(current_source_files(), lambda path: (REPOSITORY_ROOT / path).read_bytes())


def archived_source_coordinate(commit: str) -> dict[str, Any]:
    require(re.fullmatch(r"[0-9a-f]{40}", commit) is not None, "archived source commit is invalid")

    def git(args: list[str]) -> bytes:
        try:
            return subprocess.run(
                ["git", *args],
                cwd=REPOSITORY_ROOT,
                check=True,
                capture_output=True,
                timeout=30,
            ).stdout
        except (OSError, subprocess.SubprocessError) as error:
            raise EvidenceError("cannot inspect archived S5-02 source") from error

    manifest_path = "tests/codex/sprint5_stage2_source_scopes.txt"
    scopes = _scope_lines(git(["show", f"{commit}:{manifest_path}"]))
    expanded: set[str] = set()
    for scope in scopes:
        listed = git(["ls-tree", "-r", "--name-only", commit, "--", scope]).decode("utf-8").splitlines()
        require(bool(listed), f"archived S5-02 source scope is missing: {scope}")
        expanded.update(path for path in listed if "/.godot/" not in path)
    return _coordinate(tuple(sorted(expanded)), lambda path: git(["show", f"{commit}:{path}"]))


def source_matches_current_or_archive(source: dict[str, Any]) -> bool:
    if source == current_source_coordinate():
        return True
    try:
        commits = subprocess.run(
            ["git", "rev-list", "--reverse", f"{BASELINE}..HEAD"],
            cwd=REPOSITORY_ROOT,
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
        ).stdout.splitlines()
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError("cannot enumerate archived S5-02 source") from error
    return any(archived_source_coordinate(commit) == source for commit in commits)


def platform_tag() -> str:
    machine = platform.machine().lower()
    if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
        return "macos-arm64"
    raise EvidenceError(f"S5-02 evidence producer is unsupported on {sys.platform}/{machine}")


def command_version(command: list[str]) -> str:
    try:
        result = subprocess.run(
            command,
            cwd=REPOSITORY_ROOT,
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError(f"cannot obtain tool version: {command[0]}") from error
    lines = (result.stdout or result.stderr).strip().splitlines()
    require(bool(lines), f"empty tool version: {command[0]}")
    return lines[0]


def validate_parser_metrics(value: Any) -> dict[str, Any]:
    require(isinstance(value, dict) and set(value) == PARSER_FIELDS, "Godot parser metric fields differ")
    require(value["schema_version"] == 1, "Godot parser metric version differs")
    require(value["fixture_files_loaded_absolute"] is True, "Godot did not load exact fixture files")
    require(value["gdscript_valid_documents"] == 6, "Godot valid-document population differs")
    require(value["gdscript_parse_error_documents"] == 1, "Godot parse-error population differs")
    require(value["csharp_discovery_documents"] == 1, "C# discovery population differs")
    require(value["attachment_scenes"] == 3, "attachment-scene population differs")
    return cast(dict[str, Any], value)


def run_godot_oracle(binary: Path) -> tuple[dict[str, int], dict[str, Any]]:
    require(binary.is_file(), "Godot test binary is missing")
    try:
        result = subprocess.run(
            [str(binary), "--test", "--test-case=*CodexScriptOracle*", "--no-colors"],
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=180,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError("Godot script-oracle test execution failed") from error
    output = result.stdout + result.stderr
    require(result.returncode == 0 and "Status: SUCCESS!" in output, "Godot script-oracle tests failed")
    cases = re.search(r"test cases:\s+(\d+) \|\s+(\d+) passed \| 0 failed", output)
    assertions = re.search(r"assertions:\s+(\d+) \|\s+(\d+) passed \| 0 failed", output)
    marker = re.search(r"^\[codex_s5_oracle\] (\{.*\})$", output, re.MULTILINE)
    if cases is None or assertions is None or marker is None:
        raise EvidenceError("Godot script-oracle summary is incomplete")
    require(cases.group(1) == "2" and cases.group(2) == "2", "Godot script-oracle test count differs")
    require(
        assertions.group(1) == assertions.group(2) and int(assertions.group(1)) >= 688,
        "Godot script-oracle assertions differ",
    )
    try:
        metrics = json.loads(marker.group(1))
    except json.JSONDecodeError as error:
        raise EvidenceError("Godot script-oracle marker is invalid") from error
    return {
        "test_cases": 2,
        "passed": 2,
        "assertions": int(assertions.group(1)),
        "assertions_passed": int(assertions.group(2)),
    }, validate_parser_metrics(metrics)


def _fixture_coordinate(summary: dict[str, Any]) -> dict[str, Any]:
    result = {key: summary[key] for key in FIXTURE_FIELDS}
    for key in ("manifest_sha256", "golden_sha256", "canonical_symbol_ids_sha256"):
        result[key] = "sha256:" + result[key]
    return result


def build_evidence(godot: Path) -> dict[str, Any]:
    stage1.validate_evidence(stage1.strict_json_load(stage1.EVIDENCE_PATH))
    summary = fixture.validate_all(run_schemas=True)
    build, parser_metrics = run_godot_oracle(godot)
    evidence = {
        "schema_version": 1,
        "sprint": 5,
        "stage": "S5-02",
        "baseline": BASELINE,
        "platform": platform_tag(),
        "source": current_source_coordinate(),
        "fixture": _fixture_coordinate(summary),
        "godot_parser": {"build": build, "metrics": parser_metrics},
        "artifacts": {"godot_sha256": sha256_file(godot)},
        "toolchain": {
            "python": platform.python_implementation() + " " + platform.python_version(),
            "rustc": command_version(["rustc", "--version"]),
            "cargo": command_version(["cargo", "--version"]),
            "godot": command_version([str(godot), "--version"]),
        },
        "checks": list(CHECKS),
        "redaction": {"complete": True},
        "remote_ci": "not_run",
        "status": "passed",
    }
    return validate_evidence(evidence)


def _sha_coordinate(value: Any, name: str) -> None:
    require(
        isinstance(value, str) and re.fullmatch(r"sha256:[0-9a-f]{64}", value) is not None,
        f"invalid SHA-256 coordinate: {name}",
    )


def validate_evidence(value: dict[str, Any], *, check_source: bool = True) -> dict[str, Any]:
    require(set(value) == TOP_LEVEL_FIELDS, "S5-02 evidence fields differ")
    require(
        value["schema_version"] == 1 and value["sprint"] == 5 and value["stage"] == "S5-02",
        "S5-02 evidence version differs",
    )
    require(value["baseline"] == BASELINE, "S5-02 baseline differs")
    require(value["platform"] == "macos-arm64", "S5-02 platform differs")
    require(value["remote_ci"] == "not_run" and value["status"] == "passed", "S5-02 evidence is not locally passing")

    source = value["source"]
    require(isinstance(source, dict) and set(source) == SOURCE_FIELDS, "S5-02 source coordinates differ")
    require(source["scope_manifest"] == "tests/codex/sprint5_stage2_source_scopes.txt", "S5-02 scope manifest differs")
    _sha_coordinate(source["source_scope_sha256"], "source_scope_sha256")
    require(isinstance(source["file_count"], int) and source["file_count"] > 0, "S5-02 source file count differs")
    if check_source:
        require(source_matches_current_or_archive(source), "current and archived S5-02 source differ from evidence")

    fixture_value = value["fixture"]
    require(isinstance(fixture_value, dict) and set(fixture_value) == FIXTURE_FIELDS, "fixture coordinates differ")
    require(
        fixture_value == _fixture_coordinate(fixture.validate_all(run_schemas=False)),
        "fixture truth differs from evidence",
    )
    for key in ("manifest_sha256", "golden_sha256", "canonical_symbol_ids_sha256"):
        _sha_coordinate(fixture_value[key], key)

    parser_value = value["godot_parser"]
    require(
        isinstance(parser_value, dict) and set(parser_value) == {"build", "metrics"}, "Godot parser coordinates differ"
    )
    build = parser_value["build"]
    require(
        isinstance(build, dict)
        and set(build) == {"test_cases", "passed", "assertions", "assertions_passed"}
        and build["test_cases"] == 2
        and build["passed"] == 2
        and isinstance(build["assertions"], int)
        and build["assertions"] >= 688
        and build["assertions_passed"] == build["assertions"],
        "Godot parser build evidence differs",
    )
    validate_parser_metrics(parser_value["metrics"])

    artifacts = value["artifacts"]
    require(isinstance(artifacts, dict) and set(artifacts) == {"godot_sha256"}, "S5-02 artifact coordinates differ")
    _sha_coordinate(artifacts["godot_sha256"], "godot_sha256")
    toolchain = value["toolchain"]
    require(
        isinstance(toolchain, dict)
        and set(toolchain) == {"python", "rustc", "cargo", "godot"}
        and all(isinstance(item, str) and item for item in toolchain.values()),
        "S5-02 toolchain coordinates differ",
    )
    require(value["checks"] == list(CHECKS), "S5-02 check list differs")
    require(value["redaction"] == {"complete": True}, "S5-02 evidence redaction is incomplete")
    serialized = json.dumps(value, ensure_ascii=False)
    require(
        not any(member in serialized for member in FORBIDDEN_MATERIAL), "S5-02 evidence leaks forbidden path material"
    )
    return value


def write_evidence(path: Path, value: dict[str, Any]) -> None:
    require(path.resolve() == EVIDENCE_PATH.resolve(), "S5-02 evidence output path differs")
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", dir=path.parent, delete=False) as temporary:
        temporary.write(payload)
        temporary_path = Path(temporary.name)
    os.replace(temporary_path, path)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    generate = subparsers.add_parser("generate")
    generate.add_argument("--godot", type=Path, required=True)
    subparsers.add_parser("validate")
    arguments = parser.parse_args(sys.argv[1:] if argv is None else argv)
    try:
        if arguments.command == "generate":
            evidence = build_evidence(arguments.godot)
            write_evidence(EVIDENCE_PATH, evidence)
        else:
            evidence = validate_evidence(strict_json_load(EVIDENCE_PATH))
    except (EvidenceError, fixture.FixtureError, OSError, subprocess.SubprocessError) as error:
        print(f"S5-02 evidence validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(evidence, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
