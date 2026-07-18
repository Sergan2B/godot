#!/usr/bin/env python3
"""Produce and validate fail-closed evidence for Sprint 5 gate S5-01."""

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

import script_semantics_contract as contract

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint5_source_scopes.txt"
EVIDENCE_PATH = SCRIPT_DIR / "evidence" / "sprint-5-stage-1-contracts.json"
BASELINE = "73ec8ade7897109b8fed2e6fcb714a1ad01dd95b"

CHECKS = (
    "source_scope_hash",
    "strict_json",
    "draft_2020_12_schema",
    "named_negative_schema_cases",
    "identity_vectors_python_rust_cpp",
    "utf8_range_vectors_python_rust_cpp",
    "confidence_vectors_python_rust_cpp",
    "d07_godot_source_audit",
    "d07_saved_dirty_separation",
    "d07_valid_and_invalid_projection",
    "d07_no_lsp_client_dependency",
    "d07_bounded_retained_memory",
    "gdscript_disabled_build_and_tests",
    "evidence_redaction",
)
TOP_LEVEL_FIELDS = {
    "schema_version",
    "sprint",
    "stage",
    "baseline",
    "platform",
    "source",
    "contract",
    "decision",
    "builds",
    "artifacts",
    "toolchain",
    "checks",
    "redaction",
    "remote_ci",
    "status",
}
SOURCE_FIELDS = {"scope_manifest", "source_scope_sha256", "file_count"}
CONTRACT_FIELDS = {
    "schema_version",
    "vectors_sha256",
    "schema_sha256",
    "sources",
    "identity_vectors",
    "range_vectors",
    "confidence_vectors",
}
BUILD_FIELDS = {"module_gdscript_enabled", "test_cases", "passed", "assertions", "assertions_passed"}
RUNTIME_METRIC_FIELDS = {
    "schema_version",
    "selected",
    "rejected",
    "selected_path_requires_lsp_client",
    "execution_thread",
    "valid_projection",
    "parse_error_diagnostic",
    "cold_parse_usec",
    "warm_lookup_iterations",
    "warm_lookup_total_usec",
    "incremental_parse_usec",
    "retained_projection_count",
    "retained_memory_delta_bytes",
    "retained_memory_budget_bytes",
    "retained_memory_within_budget",
    "symbol_count",
}
FORBIDDEN_MATERIAL = ("/Users/", "C:\\", ".godot/imported", ".godot/codex")


class EvidenceError(RuntimeError):
    """Raised when S5-01 evidence is incomplete or inconsistent."""


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


def source_scopes() -> tuple[str, ...]:
    try:
        values = tuple(
            line
            for line in SOURCE_SCOPE_PATH.read_text(encoding="utf-8").splitlines()
            if line and not line.startswith("#")
        )
    except (OSError, UnicodeError) as error:
        raise EvidenceError("cannot read Sprint 5 source-scope manifest") from error
    require(values == tuple(sorted(values)), "source scopes must be sorted")
    require(len(values) == len(set(values)) and bool(values), "source scopes must be non-empty and unique")
    require("tests/codex/sprint5_source_scopes.txt" in values, "source-scope manifest must bind itself")
    require(
        "tests/codex/evidence/sprint-5-stage-1-contracts.json" not in values,
        "generated evidence must not create a circular source hash",
    )
    for value in values:
        relative = Path(value)
        require(not relative.is_absolute() and ".." not in relative.parts, f"unsafe source scope: {value}")
        require("\\" not in value and "\0" not in value, f"unsafe source scope: {value}")
        require(
            value.startswith(("docs/codex-integration/", "modules/gdscript/language_server/", "tests/codex/")),
            f"source scope is outside the S5-01 allowlist: {value}",
        )
        require((REPOSITORY_ROOT / relative).is_file(), f"source scope is not an exact file: {value}")
    return values


def current_source_coordinate() -> dict[str, Any]:
    values = source_scopes()
    digest = hashlib.sha256()
    for value in values:
        digest.update(value.encode("utf-8"))
        digest.update(b"\0")
        digest.update((REPOSITORY_ROOT / value).read_bytes())
        digest.update(b"\0")
    return {
        "scope_manifest": "tests/codex/sprint5_source_scopes.txt",
        "source_scope_sha256": "sha256:" + digest.hexdigest(),
        "file_count": len(values),
    }


def platform_tag() -> str:
    machine = platform.machine().lower()
    if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
        return "macos-arm64"
    raise EvidenceError(f"S5-01 evidence producer is unsupported on {sys.platform}/{machine}")


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
    value = (result.stdout or result.stderr).strip().splitlines()
    require(bool(value), f"empty tool version: {command[0]}")
    return value[0]


def _integer(value: Any, field: str, *, minimum: int = 0) -> int:
    if not isinstance(value, int) or isinstance(value, bool):
        raise EvidenceError(f"D-07 metric must be an integer: {field}")
    if value < minimum:
        raise EvidenceError(f"D-07 metric is below its minimum: {field}")
    return value


def validate_runtime_metrics(value: Any) -> dict[str, Any]:
    require(isinstance(value, dict), "D-07 runtime metrics must be an object")
    metrics = cast(dict[str, Any], value)
    require(set(metrics) == RUNTIME_METRIC_FIELDS, "D-07 runtime metric fields differ")
    require(metrics["schema_version"] == 1, "D-07 runtime metric version differs")
    require(metrics["selected"] == "bridge_owned_exact_content_cache", "D-07 selected path differs")
    require(metrics["rejected"] == "active_lsp_peer_cache", "D-07 rejected path differs")
    require(metrics["selected_path_requires_lsp_client"] is False, "selected D-07 path requires LSP")
    require(metrics["execution_thread"] == "godot_test_main_thread", "D-07 execution thread differs")
    require(metrics["valid_projection"] is True, "valid GDScript projection failed")
    require(metrics["parse_error_diagnostic"] is True, "invalid GDScript produced no diagnostic")
    _integer(metrics["cold_parse_usec"], "cold_parse_usec")
    require(
        _integer(metrics["warm_lookup_iterations"], "warm_lookup_iterations", minimum=1) == 1000,
        "warm lookup population differs",
    )
    _integer(metrics["warm_lookup_total_usec"], "warm_lookup_total_usec")
    _integer(metrics["incremental_parse_usec"], "incremental_parse_usec")
    require(
        _integer(metrics["retained_projection_count"], "retained_projection_count", minimum=1) == 32,
        "retained projection population differs",
    )
    retained = _integer(metrics["retained_memory_delta_bytes"], "retained_memory_delta_bytes")
    budget = _integer(metrics["retained_memory_budget_bytes"], "retained_memory_budget_bytes", minimum=1)
    require(
        metrics["retained_memory_within_budget"] is True and retained <= budget,
        "D-07 retained memory exceeded its spike budget",
    )
    _integer(metrics["symbol_count"], "symbol_count", minimum=7)
    return metrics


def run_godot_contract(binary: Path, *, gdscript_enabled: bool) -> tuple[dict[str, Any], dict[str, Any] | None]:
    require(binary.is_file(), "Godot test binary is missing")
    try:
        result = subprocess.run(
            [str(binary), "--test", "--test-case=*CodexScriptContract*", "--no-colors"],
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=180,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError("Godot script-contract test execution failed") from error
    output = result.stdout + result.stderr
    require(result.returncode == 0 and "Status: SUCCESS!" in output, "Godot script-contract tests failed")
    cases = re.search(r"test cases:\s+(\d+) \|\s+(\d+) passed \| 0 failed", output)
    assertions = re.search(r"assertions:\s+(\d+) \|\s+(\d+) passed \| 0 failed", output)
    if cases is None or assertions is None:
        raise EvidenceError("Godot doctest summary is missing")
    expected_cases = 4 if gdscript_enabled else 3
    require(
        int(cases.group(1)) == expected_cases and int(cases.group(2)) == expected_cases, "Godot test-case count differs"
    )
    build = {
        "module_gdscript_enabled": gdscript_enabled,
        "test_cases": int(cases.group(1)),
        "passed": int(cases.group(2)),
        "assertions": int(assertions.group(1)),
        "assertions_passed": int(assertions.group(2)),
    }
    marker = re.search(r"^\[codex_d07_spike\] (\{.*\})$", output, re.MULTILINE)
    if gdscript_enabled:
        if marker is None:
            raise EvidenceError("D-07 runtime marker is missing")
        try:
            parsed = json.loads(marker.group(1))
        except (AttributeError, json.JSONDecodeError) as error:
            raise EvidenceError("D-07 runtime marker is invalid") from error
        return build, validate_runtime_metrics(parsed)
    require(marker is None, "GDScript-disabled binary unexpectedly ran the analyzer spike")
    return build, None


def _contract_coordinate(summary: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema_version": summary["schema_version"],
        "vectors_sha256": "sha256:" + summary["vectors_sha256"],
        "schema_sha256": "sha256:" + summary["schema_sha256"],
        "sources": summary["sources"],
        "identity_vectors": summary["identity_vectors"],
        "range_vectors": summary["range_vectors"],
        "confidence_vectors": summary["confidence_vectors"],
    }


def build_evidence(godot: Path, godot_no_gdscript: Path) -> dict[str, Any]:
    require(godot.resolve() != godot_no_gdscript.resolve(), "enabled and disabled Godot binaries must differ")
    summary = contract.validate_all(run_schemas=True)
    enabled, runtime = run_godot_contract(godot, gdscript_enabled=True)
    disabled, disabled_runtime = run_godot_contract(godot_no_gdscript, gdscript_enabled=False)
    require(runtime is not None and disabled_runtime is None, "D-07 runtime coordinates differ")
    evidence = {
        "schema_version": 1,
        "sprint": 5,
        "stage": "S5-01",
        "baseline": BASELINE,
        "platform": platform_tag(),
        "source": current_source_coordinate(),
        "contract": _contract_coordinate(summary),
        "decision": {"source_audit": summary["d07"], "runtime_spike": runtime},
        "builds": {"gdscript_enabled": enabled, "gdscript_disabled": disabled},
        "artifacts": {"godot_sha256": sha256_file(godot), "godot_no_gdscript_sha256": sha256_file(godot_no_gdscript)},
        "toolchain": {
            "python": platform.python_implementation() + " " + platform.python_version(),
            "rustc": command_version(["rustc", "--version"]),
            "cargo": command_version(["cargo", "--version"]),
            "godot": command_version([str(godot), "--version"]),
            "godot_no_gdscript": command_version([str(godot_no_gdscript), "--version"]),
        },
        "checks": list(CHECKS),
        "redaction": {"complete": True},
        "remote_ci": "not_run",
        "status": "passed",
    }
    return validate_evidence(evidence)


def _validate_build(value: Any, expected_gdscript: bool) -> None:
    require(isinstance(value, dict) and set(value) == BUILD_FIELDS, "Godot build evidence fields differ")
    require(value["module_gdscript_enabled"] is expected_gdscript, "Godot module profile differs")
    expected_cases = 4 if expected_gdscript else 3
    require(value["test_cases"] == expected_cases and value["passed"] == expected_cases, "Godot test evidence differs")
    require(
        isinstance(value["assertions"], int)
        and not isinstance(value["assertions"], bool)
        and value["assertions"] > 0
        and value["assertions_passed"] == value["assertions"],
        "Godot assertion evidence differs",
    )


def _sha_coordinate(value: Any, name: str) -> None:
    require(
        isinstance(value, str) and re.fullmatch(r"sha256:[0-9a-f]{64}", value) is not None,
        f"invalid SHA-256 coordinate: {name}",
    )


def validate_evidence(value: dict[str, Any], *, check_checkout: bool = True) -> dict[str, Any]:
    require(set(value) == TOP_LEVEL_FIELDS, "S5-01 evidence fields differ")
    require(
        value["schema_version"] == 1 and value["sprint"] == 5 and value["stage"] == "S5-01",
        "S5-01 evidence version differs",
    )
    require(value["baseline"] == BASELINE, "S5-01 baseline differs")
    require(value["platform"] == "macos-arm64", "S5-01 platform differs")
    require(value["remote_ci"] == "not_run" and value["status"] == "passed", "S5-01 evidence is not locally passing")

    source = value["source"]
    require(isinstance(source, dict) and set(source) == SOURCE_FIELDS, "source coordinates differ")
    require(source["scope_manifest"] == "tests/codex/sprint5_source_scopes.txt", "source manifest differs")
    _sha_coordinate(source["source_scope_sha256"], "source_scope_sha256")
    require(source["file_count"] == len(source_scopes()), "source file count differs")
    if check_checkout:
        require(source == current_source_coordinate(), "current S5-01 source differs from evidence")

    contract_value = value["contract"]
    require(isinstance(contract_value, dict) and set(contract_value) == CONTRACT_FIELDS, "contract coordinates differ")
    current_contract = _contract_coordinate(contract.validate_all(run_schemas=False))
    require(contract_value == current_contract, "script contract differs from evidence")
    _sha_coordinate(contract_value["vectors_sha256"], "vectors_sha256")
    _sha_coordinate(contract_value["schema_sha256"], "schema_sha256")

    decision = value["decision"]
    require(
        isinstance(decision, dict) and set(decision) == {"source_audit", "runtime_spike"}, "D-07 evidence fields differ"
    )
    require(decision["source_audit"] == contract.d07_source_audit(), "D-07 source audit differs")
    validate_runtime_metrics(decision["runtime_spike"])

    builds = value["builds"]
    require(
        isinstance(builds, dict) and set(builds) == {"gdscript_enabled", "gdscript_disabled"},
        "Godot build coordinates differ",
    )
    _validate_build(builds["gdscript_enabled"], True)
    _validate_build(builds["gdscript_disabled"], False)

    artifacts = value["artifacts"]
    require(
        isinstance(artifacts, dict) and set(artifacts) == {"godot_sha256", "godot_no_gdscript_sha256"},
        "artifact coordinates differ",
    )
    _sha_coordinate(artifacts["godot_sha256"], "godot_sha256")
    _sha_coordinate(artifacts["godot_no_gdscript_sha256"], "godot_no_gdscript_sha256")
    require(
        artifacts["godot_sha256"] != artifacts["godot_no_gdscript_sha256"],
        "Godot artifact profiles are indistinguishable",
    )

    toolchain = value["toolchain"]
    require(
        isinstance(toolchain, dict)
        and set(toolchain) == {"python", "rustc", "cargo", "godot", "godot_no_gdscript"}
        and all(isinstance(item, str) and item for item in toolchain.values()),
        "toolchain coordinates differ",
    )
    require(toolchain["godot"] == toolchain["godot_no_gdscript"], "Godot versions differ across feature profiles")
    require(value["checks"] == list(CHECKS), "S5-01 check list differs")
    require(value["redaction"] == {"complete": True}, "S5-01 evidence redaction is incomplete")
    serialized = json.dumps(value, ensure_ascii=False)
    require(
        not any(member in serialized for member in FORBIDDEN_MATERIAL), "S5-01 evidence leaks forbidden path material"
    )
    return value


def write_evidence(path: Path, value: dict[str, Any]) -> None:
    require(path.resolve() == EVIDENCE_PATH.resolve(), "S5-01 evidence output path differs")
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
    generate.add_argument("--godot-no-gdscript", type=Path, required=True)
    subparsers.add_parser("validate")
    arguments = parser.parse_args(sys.argv[1:] if argv is None else argv)
    try:
        if arguments.command == "generate":
            evidence = build_evidence(arguments.godot, arguments.godot_no_gdscript)
            write_evidence(EVIDENCE_PATH, evidence)
        else:
            evidence = validate_evidence(strict_json_load(EVIDENCE_PATH))
    except (EvidenceError, contract.ContractError, OSError, subprocess.SubprocessError) as error:
        print(f"S5-01 evidence validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(evidence, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
