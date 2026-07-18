#!/usr/bin/env python3
"""Produce and validate fail-closed evidence for Sprint 5 gate S5-03."""

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

import sprint5_stage1_evidence as stage1
import sprint5_stage2_evidence as stage2

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
SCHEMA_ROOT = REPOSITORY_ROOT / "schemas" / "codex_bridge" / "v1"
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint5_stage3_source_scopes.txt"
EVIDENCE_PATH = SCRIPT_DIR / "evidence" / "sprint-5-stage-3-bridge.json"
BASELINE = "56b7ace945daaa821207f09327abcbf6a266b842"

CAPABILITIES = (
    "script.gdscript_semantics",
    "script.incremental_index",
    "script.diagnostics",
    "script.csharp_discovery",
)
METHODS = ("script.snapshot.get", "script.delta.get")
NOTIFICATIONS = ("script_graph_changed", "script_journal_gap")
CHECKS = (
    "archived_s5_01_evidence",
    "archived_s5_02_evidence",
    "source_scope_hash",
    "strict_json",
    "draft_2020_12_schemas",
    "canonical_bundle_101_cases",
    "script_profile_18_cases",
    "resource_scene_1_4_compatibility",
    "snapshot_payload_checksum",
    "snapshot_aggregate_checksum",
    "delta_batch_checksum",
    "exact_dynamic_target_separation",
    "raw_source_and_absolute_path_rejection",
    "bridge_rpc_1_4_negotiation_cpp",
    "legacy_1_0_1_3_script_omission",
    "script_capability_unavailable_until_s5_04",
    "evidence_redaction",
)
TOP_LEVEL_FIELDS = {
    "schema_version",
    "sprint",
    "stage",
    "baseline",
    "platform",
    "source",
    "profile",
    "rust_conformance",
    "godot_protocol",
    "artifacts",
    "toolchain",
    "checks",
    "redaction",
    "remote_ci",
    "status",
}
SOURCE_FIELDS = {"scope_manifest", "source_scope_sha256", "file_count"}
PROFILE_FIELDS = {
    "bundle_version",
    "schema_files",
    "manifest_cases",
    "script_schema_cases",
    "compatibility_cases",
    "capabilities",
    "methods",
    "notifications",
    "revision",
    "readiness",
    "snapshot_payload_bytes",
    "snapshot_documents",
    "snapshot_symbols",
    "snapshot_relations",
    "snapshot_diagnostics",
}
GODOT_METRIC_FIELDS = {
    "schema_version",
    "protocol_version",
    "script_capabilities",
    "script_methods",
    "resource_scene_retained",
    "script_readiness",
}
FORBIDDEN_MATERIAL = ("/Users/", "C:\\", ".godot/imported", ".godot/codex", "extends Node")


class EvidenceError(RuntimeError):
    """Raised when S5-03 evidence is incomplete or inconsistent."""


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
        raise EvidenceError(f"cannot read strict JSON: {path.name}") from error
    require(isinstance(value, dict), "JSON root must be an object")
    return cast(dict[str, Any], value)


def sha256_file(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def _scope_lines(payload: bytes) -> tuple[str, ...]:
    try:
        values = tuple(line for line in payload.decode("utf-8").splitlines() if line and not line.startswith("#"))
    except UnicodeError as error:
        raise EvidenceError("S5-03 source-scope manifest is not UTF-8") from error
    require(values == tuple(sorted(values)), "S5-03 source scopes must be sorted")
    require(len(values) == len(set(values)) and bool(values), "S5-03 source scopes must be unique")
    require("tests/codex/sprint5_stage3_source_scopes.txt" in values, "S5-03 scope manifest must bind itself")
    require(
        "tests/codex/evidence/sprint-5-stage-3-bridge.json" not in values,
        "generated S5-03 evidence must not create a circular source hash",
    )
    for value in values:
        path = Path(value)
        require(
            not path.is_absolute() and ".." not in path.parts and "\\" not in value,
            f"unsafe S5-03 source scope: {value}",
        )
    return values


def current_source_files() -> tuple[str, ...]:
    scopes = _scope_lines(SOURCE_SCOPE_PATH.read_bytes())
    expanded: set[str] = set()
    for scope in scopes:
        path = REPOSITORY_ROOT / scope
        require(path.exists(), f"S5-03 source scope is missing: {scope}")
        if path.is_file():
            expanded.add(scope)
        else:
            for candidate in path.rglob("*"):
                if candidate.is_file() and ".godot" not in candidate.parts:
                    expanded.add(candidate.relative_to(REPOSITORY_ROOT).as_posix())
    require(bool(expanded), "S5-03 source scope expands to no files")
    return tuple(sorted(expanded))


def _coordinate(paths: tuple[str, ...], reader: Any) -> dict[str, Any]:
    digest = hashlib.sha256()
    for path in paths:
        digest.update(path.encode("utf-8"))
        digest.update(b"\0")
        digest.update(reader(path))
        digest.update(b"\0")
    return {
        "scope_manifest": "tests/codex/sprint5_stage3_source_scopes.txt",
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
                ["git", *args], cwd=REPOSITORY_ROOT, check=True, capture_output=True, timeout=30
            ).stdout
        except (OSError, subprocess.SubprocessError) as error:
            raise EvidenceError("cannot inspect archived S5-03 source") from error

    manifest_path = "tests/codex/sprint5_stage3_source_scopes.txt"
    scopes = _scope_lines(git(["show", f"{commit}:{manifest_path}"]))
    expanded: set[str] = set()
    for scope in scopes:
        listed = git(["ls-tree", "-r", "--name-only", commit, "--", scope]).decode("utf-8").splitlines()
        require(bool(listed), f"archived S5-03 source scope is missing: {scope}")
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
        raise EvidenceError("cannot enumerate archived S5-03 source") from error
    return any(archived_source_coordinate(commit) == source for commit in commits)


def platform_tag() -> str:
    machine = platform.machine().lower()
    if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
        return "macos-arm64"
    raise EvidenceError(f"S5-03 evidence producer is unsupported on {sys.platform}/{machine}")


def command_version(command: list[str]) -> str:
    try:
        result = subprocess.run(command, cwd=REPOSITORY_ROOT, check=True, capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError(f"cannot obtain tool version: {command[0]}") from error
    lines = (result.stdout or result.stderr).strip().splitlines()
    require(bool(lines), f"empty tool version: {command[0]}")
    return lines[0]


def bridge_profile_summary() -> dict[str, Any]:
    manifest = strict_json_load(SCHEMA_ROOT / "fixtures" / "manifest.json")
    cases_value = manifest.get("cases")
    require(isinstance(cases_value, list), "canonical bundle cases are missing")
    cases = cast(list[Any], cases_value)
    case_ids = [case.get("id") for case in cases if isinstance(case, dict)]
    require(len(case_ids) == len(cases) and len(set(case_ids)) == len(case_ids), "bundle case IDs differ")
    script_cases = [value for value in case_ids if isinstance(value, str) and value.startswith("schema.script.")]
    compatibility_cases = [value for value in case_ids if isinstance(value, str) and value.startswith("schema.compat.")]

    schema_files = tuple(sorted(path.name for path in SCHEMA_ROOT.glob("*.schema.json")))
    require("script.schema.json" in schema_files, "script schema is missing")
    chunk = strict_json_load(SCHEMA_ROOT / "fixtures" / "valid" / "script-snapshot-chunk.json")
    payload_json_value = chunk.get("payload_json")
    require(isinstance(payload_json_value, str), "script snapshot canonical payload is missing")
    payload_json = cast(str, payload_json_value)
    require(json.loads(payload_json) == chunk.get("payload"), "script snapshot payload JSON differs")
    chunk_checksum = hashlib.sha256(payload_json.encode("utf-8")).hexdigest()
    require(chunk.get("checksum") == chunk_checksum, "script snapshot chunk checksum differs")

    end = strict_json_load(SCHEMA_ROOT / "fixtures" / "valid" / "script-snapshot-end.json")
    end_params_value = end.get("params")
    require(isinstance(end_params_value, dict), "script snapshot end parameters are missing")
    end_params = cast(dict[str, Any], end_params_value)
    require(
        end_params.get("checksum") == hashlib.sha256(chunk_checksum.encode("ascii")).hexdigest(),
        "script snapshot aggregate checksum differs",
    )

    delta = strict_json_load(SCHEMA_ROOT / "fixtures" / "valid" / "script-delta-batch-response.json")
    batch_value = delta.get("result", {}).get("batch") if isinstance(delta.get("result"), dict) else None
    require(isinstance(batch_value, dict), "script delta batch is missing")
    batch = cast(dict[str, Any], batch_value)
    operations_value = batch.get("operations")
    require(isinstance(operations_value, list), "script delta operations are missing")
    operations = cast(list[Any], operations_value)
    operations_json = json.dumps(operations, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    require(
        batch.get("checksum") == hashlib.sha256(operations_json.encode()).hexdigest(), "script delta checksum differs"
    )

    payload_value = chunk.get("payload")
    require(isinstance(payload_value, dict), "script snapshot payload is missing")
    payload = cast(dict[str, Any], payload_value)
    relations_value = payload.get("relations")
    require(isinstance(relations_value, list) and len(relations_value) == 2, "script relation profile differs")
    relations_list = cast(list[Any], relations_value)
    require(all(isinstance(relation, dict) for relation in relations_list), "script relations must be objects")
    relations = cast(list[dict[str, Any]], relations_list)
    require(
        relations[0].get("confidence") == "exact" and isinstance(relations[0].get("target"), dict),
        "exact relation target differs",
    )
    require(
        relations[1].get("confidence") == "dynamic" and relations[1].get("target") is None,
        "dynamic relation gained a target",
    )
    serialized_valid = json.dumps(payload, ensure_ascii=False)
    require(
        "raw_source" not in serialized_valid and "/Users/" not in serialized_valid, "valid wire payload leaks source"
    )

    return {
        "bundle_version": manifest.get("bundle_version"),
        "schema_files": len(schema_files),
        "manifest_cases": len(cases),
        "script_schema_cases": len(script_cases),
        "compatibility_cases": len(compatibility_cases),
        "capabilities": list(CAPABILITIES),
        "methods": list(METHODS),
        "notifications": list(NOTIFICATIONS),
        "revision": "script_graph_revision",
        "readiness": "unavailable_until_s5_04",
        "snapshot_payload_bytes": len(payload_json.encode("utf-8")),
        "snapshot_documents": end_params.get("document_count"),
        "snapshot_symbols": end_params.get("symbol_count"),
        "snapshot_relations": end_params.get("relation_count"),
        "snapshot_diagnostics": end_params.get("diagnostic_count"),
    }


def run_rust_conformance() -> dict[str, int]:
    try:
        result = subprocess.run(
            [
                "cargo",
                "test",
                "--manifest-path",
                "tests/codex/Cargo.toml",
                "--locked",
                "--offline",
                "bundle::tests::",
            ],
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=180,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError("Rust Bridge RPC 1.4 conformance failed to execute") from error
    output = result.stdout + result.stderr
    require(result.returncode == 0, "Rust Bridge RPC 1.4 conformance failed")
    summary = re.search(r"test result: ok\. 3 passed; 0 failed;", output)
    require(summary is not None, "Rust Bridge RPC 1.4 conformance count differs")
    return {"test_cases": 3, "passed": 3}


def validate_godot_metrics(value: Any) -> dict[str, Any]:
    require(isinstance(value, dict) and set(value) == GODOT_METRIC_FIELDS, "Godot bridge metric fields differ")
    require(value["schema_version"] == 1, "Godot bridge metric version differs")
    require(value["protocol_version"] == "1.4", "Godot negotiated protocol differs")
    require(value["script_capabilities"] == 4 and value["script_methods"] == 2, "Godot script profile differs")
    require(value["resource_scene_retained"] is True, "Godot 1.4 lost resource/scene methods")
    require(value["script_readiness"] == "unavailable", "S5-03 readiness is not honest")
    return cast(dict[str, Any], value)


def run_godot_protocol(binary: Path) -> dict[str, Any]:
    require(binary.is_file(), "Godot test binary is missing")
    try:
        result = subprocess.run(
            [str(binary), "--test", "--test-case=*CodexS5BridgeProfile*", "--no-colors"],
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=180,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError("Godot Bridge RPC 1.4 tests failed to execute") from error
    output = result.stdout + result.stderr
    require(result.returncode == 0 and "Status: SUCCESS!" in output, "Godot Bridge RPC 1.4 tests failed")
    cases = re.search(r"test cases:\s+(\d+) \|\s+(\d+) passed \| 0 failed", output)
    assertions = re.search(r"assertions:\s+(\d+) \|\s+(\d+) passed \| 0 failed", output)
    marker = re.search(r"^\[codex_s5_bridge\] (\{.*\})$", output, re.MULTILINE)
    require(cases is not None and assertions is not None and marker is not None, "Godot bridge summary is incomplete")
    cases_match = cast(re.Match[str], cases)
    assertions_match = cast(re.Match[str], assertions)
    marker_match = cast(re.Match[str], marker)
    require(cases_match.group(1) == "3" and cases_match.group(2) == "3", "Godot bridge test count differs")
    require(
        assertions_match.group(1) == assertions_match.group(2) and int(assertions_match.group(1)) >= 45,
        "Godot bridge assertion count differs",
    )
    try:
        metrics = json.loads(marker_match.group(1))
    except json.JSONDecodeError as error:
        raise EvidenceError("Godot bridge marker is invalid") from error
    return {
        "build": {
            "test_cases": 3,
            "passed": 3,
            "assertions": int(assertions_match.group(1)),
            "assertions_passed": int(assertions_match.group(2)),
        },
        "metrics": validate_godot_metrics(metrics),
    }


def build_evidence(godot: Path) -> dict[str, Any]:
    stage1.validate_evidence(stage1.strict_json_load(stage1.EVIDENCE_PATH))
    stage2.validate_evidence(stage2.strict_json_load(stage2.EVIDENCE_PATH))
    evidence = {
        "schema_version": 1,
        "sprint": 5,
        "stage": "S5-03",
        "baseline": BASELINE,
        "platform": platform_tag(),
        "source": current_source_coordinate(),
        "profile": bridge_profile_summary(),
        "rust_conformance": run_rust_conformance(),
        "godot_protocol": run_godot_protocol(godot),
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
    require(set(value) == TOP_LEVEL_FIELDS, "S5-03 evidence fields differ")
    require(
        value["schema_version"] == 1 and value["sprint"] == 5 and value["stage"] == "S5-03",
        "S5-03 evidence version differs",
    )
    require(value["baseline"] == BASELINE, "S5-03 baseline differs")
    require(value["platform"] == "macos-arm64", "S5-03 platform differs")
    require(value["remote_ci"] == "not_run" and value["status"] == "passed", "S5-03 is not locally passing")

    source = value["source"]
    require(isinstance(source, dict) and set(source) == SOURCE_FIELDS, "S5-03 source coordinates differ")
    require(source["scope_manifest"] == "tests/codex/sprint5_stage3_source_scopes.txt", "S5-03 scope differs")
    _sha_coordinate(source["source_scope_sha256"], "source_scope_sha256")
    require(isinstance(source["file_count"], int) and source["file_count"] > 0, "S5-03 file count differs")
    if check_source:
        require(source_matches_current_or_archive(source), "current and archived S5-03 source differ from evidence")

    profile_value = value["profile"]
    require(isinstance(profile_value, dict) and set(profile_value) == PROFILE_FIELDS, "S5-03 profile fields differ")
    require(profile_value == bridge_profile_summary(), "S5-03 canonical profile differs from evidence")
    require(
        profile_value["bundle_version"] == "1.4"
        and profile_value["schema_files"] == 10
        and profile_value["manifest_cases"] == 101
        and profile_value["script_schema_cases"] == 18
        and profile_value["compatibility_cases"] == 2,
        "S5-03 bundle population differs",
    )

    rust = value["rust_conformance"]
    require(rust == {"test_cases": 3, "passed": 3}, "S5-03 Rust conformance differs")
    godot = value["godot_protocol"]
    require(isinstance(godot, dict) and set(godot) == {"build", "metrics"}, "S5-03 Godot fields differ")
    build = godot["build"]
    require(
        isinstance(build, dict)
        and set(build) == {"test_cases", "passed", "assertions", "assertions_passed"}
        and build["test_cases"] == 3
        and build["passed"] == 3
        and isinstance(build["assertions"], int)
        and build["assertions"] >= 45
        and build["assertions_passed"] == build["assertions"],
        "S5-03 Godot build differs",
    )
    validate_godot_metrics(godot["metrics"])

    artifacts = value["artifacts"]
    require(isinstance(artifacts, dict) and set(artifacts) == {"godot_sha256"}, "S5-03 artifact differs")
    _sha_coordinate(artifacts["godot_sha256"], "godot_sha256")
    toolchain = value["toolchain"]
    require(
        isinstance(toolchain, dict)
        and set(toolchain) == {"python", "rustc", "cargo", "godot"}
        and all(isinstance(item, str) and item for item in toolchain.values()),
        "S5-03 toolchain differs",
    )
    require(value["checks"] == list(CHECKS), "S5-03 check list differs")
    require(value["redaction"] == {"complete": True}, "S5-03 redaction is incomplete")
    serialized = json.dumps(value, ensure_ascii=False)
    require(not any(item in serialized for item in FORBIDDEN_MATERIAL), "S5-03 evidence leaks forbidden material")
    return value


def write_evidence(path: Path, value: dict[str, Any]) -> None:
    require(path.resolve() == EVIDENCE_PATH.resolve(), "S5-03 evidence output path differs")
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
    except (EvidenceError, OSError, subprocess.SubprocessError) as error:
        print(f"S5-03 evidence validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(evidence, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
