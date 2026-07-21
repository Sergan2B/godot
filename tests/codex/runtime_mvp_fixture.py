#!/usr/bin/env python3
"""Validate the independent Sprint 8 runtime fixture and golden oracle."""

from __future__ import annotations

import ast
import hashlib
import json
from pathlib import Path
from typing import Any, cast

SCRIPT_DIR = Path(__file__).resolve().parent
PROJECT_ROOT = SCRIPT_DIR / "fixtures" / "runtime_mvp_project"
ORACLE_ROOT = SCRIPT_DIR / "fixtures" / "runtime_mvp_oracle"
MANIFEST_PATH = ORACLE_ROOT / "fixture-manifest.json"
GOLDEN_PATH = ORACLE_ROOT / "golden-runtime.json"

EXPECTED_COVERAGE = {
    "bounded_properties",
    "crash",
    "deep_tree",
    "diagnostic_bounds",
    "diagnostic_stack",
    "hang",
    "instanced_scene",
    "large_tree",
    "manual_lifecycle",
    "normal_quit",
    "pause_stack",
    "runtime_only_node",
    "source_mapping",
    "stale_object",
    "unsafe_values",
    "viewport_capture",
}
EXPECTED_MANIFEST_FIELDS = {
    "schema_version",
    "project",
    "golden",
    "golden_sha256",
    "main_scene",
    "script",
    "files",
    "coverage",
}
EXPECTED_GOLDEN_FIELDS = {
    "schema_version",
    "tree",
    "properties",
    "diagnostics",
    "viewport",
    "limits",
}


class FixtureError(RuntimeError):
    """Raised when committed Sprint 8 fixture truth is inconsistent."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise FixtureError(message)


def strict_json(path: Path) -> dict[str, Any]:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise FixtureError(f"duplicate JSON member in {path.name}: {key}")
            result[key] = value
        return result

    def reject_constant(value: str) -> None:
        raise FixtureError(f"non-finite JSON number in {path.name}: {value}")

    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=reject_duplicates,
            parse_constant=reject_constant,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise FixtureError(f"cannot read strict JSON: {path.name}") from error
    require(isinstance(value, dict), f"{path.name} root must be an object")
    return cast(dict[str, Any], value)


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def project_files() -> list[str]:
    return sorted(
        path.relative_to(PROJECT_ROOT).as_posix()
        for path in PROJECT_ROOT.rglob("*")
        if path.is_file() and ".godot" not in path.parts
    )


def validate_manifest(manifest: dict[str, Any]) -> None:
    require(set(manifest) == EXPECTED_MANIFEST_FIELDS, "manifest fields differ")
    require(manifest["schema_version"] == 1, "manifest schema version differs")
    require(manifest["project"] == "../runtime_mvp_project", "fixture project differs")
    require(manifest["golden"] == GOLDEN_PATH.name, "golden path differs")
    require(manifest["main_scene"] == "res://main.tscn", "main scene differs")
    require(
        manifest["script"] == "res://scripts/runtime_fixture.gd",
        "fixture script differs",
    )
    require(
        manifest["golden_sha256"] == sha256_file(GOLDEN_PATH),
        "golden digest differs",
    )
    coverage = manifest.get("coverage")
    require(
        isinstance(coverage, list)
        and coverage == sorted(EXPECTED_COVERAGE)
        and len(coverage) == len(set(coverage)),
        "fixture coverage differs",
    )

    records = manifest.get("files")
    require(isinstance(records, list), "fixture file records are missing")
    paths: list[str] = []
    for record in records:
        require(
            isinstance(record, dict) and set(record) == {"path", "sha256"},
            "fixture file record fields differ",
        )
        path = record["path"]
        digest = record["sha256"]
        require(
            isinstance(path, str)
            and path
            and not path.startswith("/")
            and "\\" not in path
            and ".." not in Path(path).parts,
            "fixture path is unsafe",
        )
        require(
            isinstance(digest, str)
            and len(digest) == 64
            and all(character in "0123456789abcdef" for character in digest),
            f"fixture digest is malformed: {path}",
        )
        candidate = PROJECT_ROOT / path
        require(candidate.is_file(), f"fixture file is missing: {path}")
        require(sha256_file(candidate) == digest, f"fixture digest differs: {path}")
        paths.append(path)
    require(paths == sorted(set(paths)), "fixture paths are not canonical")
    require(paths == project_files(), "manifest does not close over fixture files")


def validate_golden(golden: dict[str, Any]) -> None:
    require(set(golden) == EXPECTED_GOLDEN_FIELDS, "golden fields differ")
    require(golden["schema_version"] == 1, "golden schema version differs")

    tree = golden.get("tree")
    require(
        isinstance(tree, dict)
        and set(tree) == {"required_names", "runtime_only", "static_mapped"},
        "tree oracle fields differ",
    )
    required_names = tree["required_names"]
    require(
        isinstance(required_names, list)
        and len(required_names) >= 6
        and len(required_names) == len(set(required_names)),
        "runtime tree oracle is incomplete",
    )
    require(tree["runtime_only"] in required_names, "runtime-only node is absent")
    require(tree["static_mapped"] in required_names, "mapped node is absent")
    require(tree["runtime_only"] == "RuntimeOnlyNode", "runtime-only identity differs")
    require(tree["static_mapped"] == "StaticNode", "mapped-node identity differs")

    properties = golden.get("properties")
    require(
        isinstance(properties, dict)
        and set(properties)
        == {
            "fixture_number",
            "fixture_text",
            "truncated",
            "unsafe_reasons",
            "object_reference",
            "bounded_node",
            "bounded_first",
            "bounded_omitted",
            "max_getters",
        }
        and properties.get("fixture_number") == 42
        and properties.get("fixture_text") == "runtime-mvp"
        and set(properties.get("truncated", []))
        == {
            "Members/oversized_text",
            "Members/cyclic_value",
            "Members/cyclic_dictionary",
            "Members/large_values",
            "Members/unsafe_absolute_path",
            "Members/unsafe_rid",
            "Members/unsafe_callable",
            "Members/unsafe_signal",
        }
        and properties.get("unsafe_reasons")
        == {
            "Members/unsafe_absolute_path": "unsafe_absolute_path",
            "Members/unsafe_rid": "unsupported_handle",
            "Members/unsafe_callable": "unsupported_handle",
            "Members/unsafe_signal": "unsupported_handle",
        }
        and properties.get("object_reference") == "Members/runtime_object_reference"
        and properties.get("bounded_node") == "BoundedPropertiesFixture"
        and properties.get("bounded_first") == "bounded_0000"
        and properties.get("bounded_omitted") == "bounded_0512"
        and properties.get("max_getters") == 512,
        "property oracle differs",
    )

    diagnostics = golden.get("diagnostics")
    require(
        isinstance(diagnostics, dict)
        and set(diagnostics)
        == {
            "warning",
            "error",
            "stack_functions",
            "script_path",
            "repeat_increment",
            "redacted_message",
            "utf8_prefix",
            "diagnostic_flood_count",
            "stack_flood_count",
            "pause_stack_functions",
        }
        and diagnostics.get("warning") == "CODEX_RUNTIME_FIXTURE_WARNING"
        and diagnostics.get("error") == "CODEX_RUNTIME_FIXTURE_ERROR"
        and diagnostics.get("stack_functions")
        == ["_error_leaf", "_error_middle", "_error_entry"]
        and diagnostics.get("script_path") == "res://scripts/runtime_fixture.gd"
        and diagnostics.get("repeat_increment") == 3
        and diagnostics.get("redacted_message")
        == "<redacted sensitive runtime output>"
        and diagnostics.get("utf8_prefix") == "CODEX_RUNTIME_UTF8_"
        and diagnostics.get("diagnostic_flood_count") == 205
        and diagnostics.get("stack_flood_count") == 65
        and diagnostics.get("pause_stack_functions")
        == ["_pause_stack_leaf", "_pause_stack_middle", "_pause_stack_entry"],
        "diagnostic oracle differs",
    )

    viewport = golden.get("viewport")
    require(
        viewport
        == {
            "mime_type": "image/png",
            "max_width": 1280,
            "max_height": 720,
            "max_bytes": 524288,
            "marker_rgb": [31, 158, 240],
            "core_rgb": [255, 115, 31],
            "color_tolerance": 12,
            "min_color_pixels": 100,
        },
        "viewport limits differ",
    )
    limits = golden.get("limits")
    require(
        limits == {"tree_nodes": 10000, "tree_depth": 256, "properties": 512},
        "runtime limits differ",
    )


def validate_fixture_sources(
    golden: dict[str, Any], source_overrides: dict[str, str] | None = None
) -> None:
    sources = {
        "main_scene": (PROJECT_ROOT / "main.tscn").read_text(encoding="utf-8"),
        "instance_scene": (PROJECT_ROOT / "instance.tscn").read_text(encoding="utf-8"),
        "script": (PROJECT_ROOT / "scripts/runtime_fixture.gd").read_text(
            encoding="utf-8"
        ),
        "bounded_script": (
            PROJECT_ROOT / "scripts/bounded_properties_fixture.gd"
        ).read_text(encoding="utf-8"),
        "plugin": (
            PROJECT_ROOT
            / "addons/codex_sprint8_runtime/codex_sprint8_runtime.gd"
        ).read_text(encoding="utf-8"),
    }
    if source_overrides:
        require(
            set(source_overrides) <= set(sources),
            "fixture source override is unknown",
        )
        sources.update(source_overrides)
    main_scene = sources["main_scene"]
    instance_scene = sources["instance_scene"]
    script = sources["script"]
    bounded_script = sources["bounded_script"]
    plugin = sources["plugin"]

    for name in golden["tree"]["required_names"]:
        if name == "RuntimeOnlyNode":
            require(f'&"{name}"' in script or f'"{name}"' in script, f"dynamic node is missing: {name}")
        else:
            require(name in main_scene or name in instance_scene, f"scene node is missing: {name}")
    for token in (
        "CODEX_RUNTIME_FIXTURE_WARNING",
        "CODEX_RUNTIME_FIXTURE_ERROR",
        "_error_leaf",
        "_error_middle",
        "_error_entry",
        '"large_tree"',
        '"deep_tree"',
        '"repeat_diagnostic"',
        '"sensitive_diagnostic"',
        '"diagnostic_flood"',
        '"stack_flood"',
        '"pause_stack"',
        '"crash_after_diagnostic"',
        '"spawn_ephemeral"',
        '"free_ephemeral"',
        '"crash"',
        '"hang"',
        '"quit"',
        'EngineDebugger.send_message("request_quit", [])',
    ):
        require(token in script, f"fixture behavior is missing: {token}")
    for token in ("range(513)", '"bounded_%04d"', "getter_count += 1"):
        require(token in bounded_script, f"bounded-property behavior is missing: {token}")
    for token in ('"manual_run"', '"manual_stop"'):
        require(token in plugin, f"manual lifecycle command is missing: {token}")


def validate_oracle_independence() -> None:
    validator = Path(__file__).read_text(encoding="utf-8")
    module = ast.parse(validator, filename=Path(__file__).name)
    imported_roots: set[str] = set()
    for node in ast.walk(module):
        if isinstance(node, ast.Import):
            imported_roots.update(alias.name.split(".", 1)[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            imported_roots.add(node.module.split(".", 1)[0])
    require(
        imported_roots
        <= {"__future__", "ast", "hashlib", "json", "pathlib", "typing"},
        "fixture oracle imports production code",
    )
    fixture_text = "\n".join(
        path.read_text(encoding="utf-8")
        for path in PROJECT_ROOT.rglob("*")
        if path.is_file() and path.suffix in {".gd", ".tscn", ".godot"}
    )
    require(
        not any(
            token in fixture_text
            for token in (
                "modules/codex_bridge",
                "godot-codex-mcp",
                "runtime_node_view",
                "RuntimeSourceContext",
            )
        ),
        "fixture imports production mapping/query code",
    )


def validate() -> dict[str, Any]:
    manifest = strict_json(MANIFEST_PATH)
    golden = strict_json(GOLDEN_PATH)
    validate_manifest(manifest)
    validate_golden(golden)
    validate_fixture_sources(golden)
    validate_oracle_independence()
    return {
        "schema_version": 1,
        "sprint": 8,
        "status": "passed",
        "fixture_files": len(manifest["files"]),
        "coverage": manifest["coverage"],
        "manifest_sha256": sha256_file(MANIFEST_PATH),
        "golden_sha256": sha256_file(GOLDEN_PATH),
    }


def main() -> int:
    try:
        print(json.dumps(validate(), indent=2, sort_keys=True))
        return 0
    except FixtureError as error:
        print(f"Sprint 8 fixture validation failed: {error}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
