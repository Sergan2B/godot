#!/usr/bin/env python3
"""Validate the independent Sprint 7 live-editor fixture and golden oracle."""

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
PROJECT_ROOT = SCRIPT_DIR / "fixtures" / "live_editor_project"
ORACLE_ROOT = SCRIPT_DIR / "fixtures" / "live_editor_oracle"
MANIFEST_PATH = ORACLE_ROOT / "fixture-manifest.json"
GOLDEN_PATH = ORACLE_ROOT / "golden-live-editor.json"
RESOURCE_1_4_PATH = REPOSITORY_ROOT / "schemas/codex_bridge/v1/fixtures/valid/resource-snapshot-request-1.4.json"
SCENE_1_4_PATH = REPOSITORY_ROOT / "schemas/codex_bridge/v1/fixtures/valid/scene-snapshot-request-1.4.json"
FORBIDDEN_MATERIAL = ("/Users/adiletturakulov", ".godot/imported", ".godot/codex/session.token")
REQUIRED_COVERAGE = {
    "clean_saved_scene",
    "dirty_saved_scene",
    "unsaved_scene",
    "multi_selection",
    "inspector_targets",
    "open_scripts",
    "native_history",
    "opaque_operation",
    "bounded_variants",
    "diagnostic_redaction",
    "viewport_metadata",
    "scene_close",
    "reimport",
    "event_gap_reconnect",
    "session_reset",
    "disk_live_composition",
    "rpc_1_4_compatibility",
}


class FixtureError(RuntimeError):
    """Raised when committed Sprint 7 truth is inconsistent."""


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
    phases = manifest.get("phases")
    require(isinstance(phases, list) and len(phases) >= 10, "lifecycle phases are incomplete")
    require(len(phases) == len(set(phases)), "lifecycle phases are duplicated")


def independent_compose(disk: Mapping[str, Any], live: Mapping[str, Any], coverage: str) -> dict[str, Any]:
    """Small oracle merge; intentionally independent from the Rust composer."""
    require(coverage in {"complete", "partial"}, "coverage token differs")
    effective = dict(disk)
    for name, value in live.items():
        effective[name] = value
    # Missing live properties are not absence evidence because Inspector and
    # serialized-property classes differ, even under complete node capture.
    return effective


def validate_golden(golden: Mapping[str, Any]) -> None:
    require(golden.get("schema_version") == 1, "golden schema differs")
    require(set(golden.get("coverage", [])) == REQUIRED_COVERAGE, "golden coverage differs")
    states = golden.get("states")
    require(isinstance(states, dict), "golden states are missing")
    scenes = states.get("open_scenes")
    require(isinstance(scenes, list) and len(scenes) == 3, "open-scene truth differs")
    require(sum(scene.get("dirty") is True for scene in scenes if isinstance(scene, dict)) == 2, "dirty scenes differ")
    require(any(scene.get("path") == "" for scene in scenes if isinstance(scene, dict)), "unsaved scene is missing")
    selection = states.get("selection")
    require(isinstance(selection, list) and len(selection) >= 3, "multi-selection truth differs")
    require(selection == states.get("selection_after_snapshot"), "live selection identities drifted")
    require(len(selection) == len(set(selection)), "selection identities are duplicated")
    require(states.get("inspector_targets") == ["node", "resource", "object"], "Inspector targets differ")
    scripts = states.get("scripts")
    require(isinstance(scripts, list) and len(scripts) == 2, "open-script truth differs")
    require(sum(script.get("dirty") is True for script in scripts if isinstance(script, dict)) == 1, "dirty script truth differs")
    require(sum(script.get("active") is True for script in scripts if isinstance(script, dict)) == 1, "active script truth differs")
    require(max(script.get("selection_count", 0) for script in scripts if isinstance(script, dict)) >= 2, "multi-caret truth differs")

    vectors = golden.get("composition_vectors")
    require(isinstance(vectors, list) and len(vectors) >= 2, "composition vectors are incomplete")
    for vector in vectors:
        require(isinstance(vector, dict), "composition vector is malformed")
        actual = independent_compose(
            vector.get("disk_properties", {}),
            vector.get("live_properties", {}),
            vector.get("coverage", ""),
        )
        require(actual == vector.get("expected_effective"), "independent composition differs")
        conflicts = sorted(
            name
            for name, value in vector.get("live_properties", {}).items()
            if name in vector.get("disk_properties", {}) and vector["disk_properties"][name] != value
        )
        require(conflicts == sorted(vector.get("expected_conflicts", [])), "composition conflicts differ")

    transitions = golden.get("revision_transitions")
    require(isinstance(transitions, list) and [item.get("kind") for item in transitions] == ["commit", "undo", "redo", "unknown"], "history transitions differ")
    operation_sequences = [item.get("operation_seq") for item in transitions]
    dirty_revisions = [item.get("dirty_scene_revision") for item in transitions]
    clean_revisions = [item.get("clean_scene_revision") for item in transitions]
    require(all(isinstance(value, int) for value in operation_sequences + dirty_revisions + clean_revisions), "revision coordinate is malformed")
    require(operation_sequences == sorted(set(operation_sequences)), "operation sequence is not strictly monotonic")
    require(dirty_revisions == sorted(set(dirty_revisions)), "dirty scene revision is not strictly monotonic")
    require(len(set(clean_revisions)) == 1, "unaffected scene revision changed")
    require(transitions[-1].get("payload") == {"kind": "opaque"}, "unknown operation payload is not opaque")

    stale = golden.get("stale_vectors")
    require(isinstance(stale, list) and {item.get("expected_error") for item in stale} == {"stale_editor_state", "stale_scene_revision"}, "stale guards differ")
    variants = golden.get("variant_vectors")
    require(isinstance(variants, list) and {item.get("kind") for item in variants} == {"cycle", "oversized_array", "oversized_string", "unsupported_object"}, "Variant limits differ")
    diagnostics = golden.get("diagnostics")
    require(isinstance(diagnostics, dict), "diagnostic limits are missing")
    require(
        diagnostics.get("maximum_records") == 200
        and diagnostics.get("maximum_message_bytes") == 16384
        and diagnostics.get("maximum_snapshot_bytes") == 262144,
        "diagnostic limits differ",
    )
    viewport = golden.get("viewport")
    require(isinstance(viewport, dict) and viewport.get("screenshot_available") is False, "viewport screenshot policy differs")
    require(set(viewport.get("forbidden", [])) >= {"pixels", "rid", "texture"}, "viewport forbidden fields differ")
    lifecycle = golden.get("lifecycle")
    require(isinstance(lifecycle, dict) and all(value is True for value in lifecycle.values()), "lifecycle truth differs")
    compatibility = golden.get("compatibility")
    require(isinstance(compatibility, dict), "compatibility truth is missing")
    require(compatibility.get("resource_snapshot_request_1_4_sha256") == sha256_file(RESOURCE_1_4_PATH), "resource 1.4 fixture drifted")
    require(compatibility.get("scene_snapshot_request_1_4_sha256") == sha256_file(SCENE_1_4_PATH), "scene 1.4 fixture drifted")


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
            "--test",
            "live_editor_fixture",
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
        raise FixtureError("Draft 2020-12 live-editor schema tests failed:\n" + result.stdout + result.stderr)


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
        "phases": len(manifest["phases"]),
        "coverage": len(golden["coverage"]),
        "composition_vectors": len(golden["composition_vectors"]),
        "revision_transitions": len(golden["revision_transitions"]),
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
        print(f"Sprint 7 fixture validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
