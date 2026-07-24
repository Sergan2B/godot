#!/usr/bin/env python3
"""Validate the independent Sprint 10 change-set oracle."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any, Mapping, cast

SCRIPT_ROOT = Path(__file__).resolve().parent
ORACLE_ROOT = SCRIPT_ROOT / "fixtures" / "change_set_oracle"
MANIFEST_PATH = ORACLE_ROOT / "fixture-manifest.json"
GOLDEN_PATH = ORACLE_ROOT / "golden-change-sets.json"

OPERATION_KINDS = {
    "attach_script",
    "connect_signal",
    "create_node",
    "create_resource",
    "delete_node",
    "detach_script",
    "disconnect_signal",
    "reparent_node",
    "set_property",
    "update_gdscript",
    "update_resource",
}
REQUIRED_CHECKS = {
    "diagnostics",
    "index_convergence",
    "intrinsic",
    "persistence",
    "reload_reparse",
    "semantic_graph",
}
MANIFEST_FIELDS = {
    "canaries",
    "forbidden_output_fields",
    "golden",
    "golden_sha256",
    "mcp_tools",
    "operation_count",
    "project",
    "rpc",
    "schema_version",
}
GOLDEN_FIELDS = {
    "confirmation_matrix",
    "fault_scenarios",
    "invalid_change_sets",
    "operation_kinds",
    "optional_checks",
    "report_limits",
    "required_checks",
    "rollback_matrix",
    "schema_version",
    "valid_change_sets",
}


class FixtureError(RuntimeError):
    """Raised with a bounded, value-free oracle failure."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise FixtureError(message)


def strict_json(path: Path) -> dict[str, Any]:
    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in items:
            require(key not in result, f"{path.name}: duplicate member")
            result[key] = value
        return result

    def constant(_name: str) -> None:
        raise FixtureError(f"{path.name}: non-finite number")

    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=pairs,
            parse_constant=constant,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise FixtureError(f"{path.name}: invalid strict JSON") from error
    require(isinstance(value, dict), f"{path.name}: root is not an object")
    return cast(dict[str, Any], value)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def validate_manifest(manifest: Mapping[str, Any]) -> None:
    require(set(manifest) == MANIFEST_FIELDS, "manifest fields differ")
    require(
        manifest.get("schema_version") == "s10-change-set-fixture-manifest/1.0",
        "manifest schema differs",
    )
    require(
        manifest.get("project") == "../transaction_prepare_project"
        and (ORACLE_ROOT / str(manifest["project"])).resolve().is_dir(),
        "fixture project binding differs",
    )
    require(
        manifest.get("golden") == GOLDEN_PATH.name
        and manifest.get("golden_sha256") == digest(GOLDEN_PATH),
        "golden binding differs",
    )
    require(
        manifest.get("operation_count")
        == {"minimum": 1, "maximum": 16, "first_rejected": 17},
        "operation bounds differ",
    )
    require(
        manifest.get("rpc")
        == {"minimum": "1.0", "maximum": "1.8", "change_set": "1.8"},
        "RPC matrix differs",
    )
    require(
        manifest.get("mcp_tools") == {"sprint9": 36, "sprint10": 40},
        "MCP registry bounds differ",
    )
    canaries = manifest.get("canaries")
    forbidden = manifest.get("forbidden_output_fields")
    require(
        isinstance(canaries, list)
        and len(canaries) == len(set(canaries)) == 3
        and all(isinstance(value, str) and value.startswith("S10_") for value in canaries),
        "canary classes differ",
    )
    require(
        isinstance(forbidden, list)
        and forbidden == sorted(set(forbidden))
        and {"approval", "native_history_id", "source_text"}.issubset(forbidden),
        "redaction field set differs",
    )


def validate_golden(golden: Mapping[str, Any]) -> None:
    require(set(golden) == GOLDEN_FIELDS, "golden fields differ")
    require(
        golden.get("schema_version") == "s10-change-set-golden/1.0",
        "golden schema differs",
    )
    kinds = golden.get("operation_kinds")
    require(
        isinstance(kinds, list)
        and kinds == sorted(OPERATION_KINDS)
        and len(kinds) == len(OPERATION_KINDS),
        "operation allowlist differs",
    )
    valid = golden.get("valid_change_sets")
    require(isinstance(valid, list) and len(valid) == 3, "valid matrix differs")
    counts = {
        item.get("operation_count")
        for item in valid
        if isinstance(item, dict)
    }
    require(counts == {2, 4, 16}, "valid operation boundaries differ")
    for item in valid:
        require(isinstance(item, dict), "valid case is not an object")
        operation_kinds = item.get("operation_kinds")
        require(
            isinstance(operation_kinds, list)
            and len(operation_kinds) == item.get("operation_count")
            and set(operation_kinds).issubset(OPERATION_KINDS)
            and item.get("native_actions") == 1,
            "valid case projection differs",
        )
        aliases = item.get("aliases")
        save_scope = item.get("save_scope")
        require(
            isinstance(aliases, list)
            and len(aliases) == len(set(aliases))
            and all(str(value).startswith("alias:") for value in aliases),
            "alias projection differs",
        )
        require(
            isinstance(save_scope, list)
            and save_scope == sorted(set(save_scope))
            and all(str(value).startswith("res://") for value in save_scope),
            "save scope projection differs",
        )
    invalid = golden.get("invalid_change_sets")
    require(
        isinstance(invalid, list)
        and {item.get("operation_count") for item in invalid if isinstance(item, dict)}
        .issuperset({0, 17})
        and {
            item.get("id") for item in invalid if isinstance(item, dict)
        }.issuperset(
            {
                "alias_used_before_create",
                "dependency_cycle",
                "duplicate_alias",
                "duplicate_persistent_path",
                "path_outside_save_scope",
            }
        ),
        "negative matrix differs",
    )
    require(
        set(cast(Mapping[str, Any], golden.get("rollback_matrix")))
        == {"never", "on_any_failure", "on_required_failure"},
        "rollback policy matrix differs",
    )
    confirmation = golden.get("confirmation_matrix")
    require(
        isinstance(confirmation, dict)
        and confirmation.get("always_ask") == ["low", "destructive"]
        and confirmation.get("session_grant") == ["low"]
        and confirmation.get("session_grant_ttl_ms") == 900_000,
        "confirmation policy matrix differs",
    )
    require(
        golden.get("report_limits")
        == {"page_bytes": 65_536, "pages": 4, "retained_bytes": 262_144},
        "report limits differ",
    )
    faults = golden.get("fault_scenarios")
    require(
        isinstance(faults, list)
        and faults == sorted(set(faults))
        and len(faults) == 11,
        "fault matrix differs",
    )
    require(
        set(cast(list[str], golden.get("required_checks"))) == REQUIRED_CHECKS
        and golden.get("optional_checks") == ["runtime"],
        "validation check graph differs",
    )


def validate() -> dict[str, Any]:
    manifest = strict_json(MANIFEST_PATH)
    golden = strict_json(GOLDEN_PATH)
    validate_manifest(manifest)
    validate_golden(golden)
    return {
        "schema_version": "s10-fixture-validation/1.0",
        "status": "passed",
        "operation_kinds": len(OPERATION_KINDS),
        "valid_cases": len(cast(list[Any], golden["valid_change_sets"])),
        "invalid_cases": len(cast(list[Any], golden["invalid_change_sets"])),
        "fault_scenarios": len(cast(list[Any], golden["fault_scenarios"])),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    print(json.dumps(validate(), sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except FixtureError as error:
        print(f"Sprint 10 fixture validation failed: {error}")
        raise SystemExit(1)
