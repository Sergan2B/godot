#!/usr/bin/env python3
"""Materialize and validate the Sprint 3 resource graph fixture."""

from __future__ import annotations

import argparse
import base64
import copy
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unicodedata
from collections.abc import Iterable, Mapping
from pathlib import Path
from typing import Any, cast

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
PROJECT_SOURCE = SCRIPT_DIR / "fixtures" / "resource_graph_project"
ORACLE_ROOT = SCRIPT_DIR / "fixtures" / "resource_graph_oracle"
MANIFEST_PATH = ORACLE_ROOT / "fixture-manifest.json"
IDENTITY_VECTORS_PATH = ORACLE_ROOT / "identity-vectors.json"
GOLDEN_PATH = ORACLE_ROOT / "golden-resource-graph.json"
PROBE_SCRIPT = SCRIPT_DIR / "resource_graph_probe.gd"
EVIDENCE_PATH = SCRIPT_DIR / "evidence" / "sprint-3-stage-1-contracts.json"
WORK_MARKER = ".codex-resource-graph-work"
UID_DOMAIN = b"godot-codex/resource-entity/uid/v1\0"
PATH_CONTENT_DOMAIN = b"godot-codex/resource-entity/path-content/v1\0"
EDGE_DOMAIN = b"godot-codex/resource-edge/references/v1\0"
DEFAULT_WORK_ROOT = (
    Path("/tmp/codex-rg/project") if os.name == "posix" else Path(tempfile.gettempdir()) / "codex-rg" / "project"
)


class FixtureError(RuntimeError):
    """Raised when fixture construction or validation fails."""


def strict_json_load(path: Path) -> Any:
    """Load JSON while rejecting duplicate object members."""

    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise FixtureError(f"duplicate JSON member in {path.name}: {key}")
            result[key] = value
        return result

    try:
        return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=reject_duplicates)
    except (OSError, json.JSONDecodeError) as error:
        raise FixtureError(f"cannot read strict JSON {path.name}: {error}") from error


def canonical_json(value: Any) -> str:
    """Return the repository's canonical human-readable JSON form."""

    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def base64url_sha256(value: bytes) -> str:
    digest = hashlib.sha256(value).digest()
    return base64.urlsafe_b64encode(digest).decode("ascii").rstrip("=")


def normalize_resource_path(value: str) -> tuple[str, str]:
    """Return display and NFC comparison paths according to INDEX-001."""

    if not value.startswith("res://"):
        raise FixtureError("invalid_path_scheme")
    if "\\" in value:
        raise FixtureError("invalid_path_separator")
    if "\x00" in value or any(ord(character) < 32 for character in value):
        raise FixtureError("invalid_path_character")
    if value.endswith("/"):
        raise FixtureError("invalid_resource_path")

    tail = value.removeprefix("res://")
    if not tail:
        raise FixtureError("invalid_resource_path")
    segments: list[str] = []
    for segment in tail.split("/"):
        if not segment or segment == ".":
            continue
        if segment == "..":
            raise FixtureError("invalid_path_traversal")
        if any(character in segment for character in (":", "?", "#")):
            raise FixtureError("invalid_path_character")
        segments.append(segment)
    if not segments:
        raise FixtureError("invalid_resource_path")

    display_path = "res://" + "/".join(segments)
    comparison_path = unicodedata.normalize("NFC", display_path)
    return display_path, comparison_path


def uid_entity_id(uid: str) -> str:
    if not uid.startswith("uid://") or len(uid) <= len("uid://"):
        raise FixtureError("invalid_resource_uid")
    digest = base64url_sha256(UID_DOMAIN + uid.encode("utf-8"))
    return f"godot:resource:uid:v1:{digest}"


def fallback_entity_id(path: str, content_generation: str) -> str:
    _display, comparison = normalize_resource_path(path)
    if not content_generation.startswith("sha256:") or len(content_generation) != 71:
        raise FixtureError("invalid_content_generation")
    canonical_input = PATH_CONTENT_DOMAIN + comparison.encode("utf-8") + b"\0" + content_generation.encode("ascii")
    return "godot:resource:path-content:v1:" + base64url_sha256(canonical_input)


def dependency_edge_id(source_entity_id: str, target_uid: str | None, path: str) -> str:
    _display, comparison = normalize_resource_path(path)
    target_kind = "uid" if target_uid is not None else "path"
    target_value = target_uid if target_uid is not None else comparison
    canonical_input = (
        EDGE_DOMAIN
        + source_entity_id.encode("utf-8")
        + b"\0"
        + target_kind.encode("ascii")
        + b"\0"
        + target_value.encode("utf-8")
    )
    return "godot:edge:references:v1:" + base64url_sha256(canonical_input)


def project_path(work_root: Path, resource_path: str) -> Path:
    display, _comparison = normalize_resource_path(resource_path)
    candidate = work_root.joinpath(*display.removeprefix("res://").split("/"))
    resolved_root = work_root.resolve()
    resolved_parent = candidate.parent.resolve()
    if resolved_root != resolved_parent and resolved_root not in resolved_parent.parents:
        raise FixtureError("resource path escaped the fixture project")
    return candidate


def load_manifest() -> dict[str, Any]:
    value = strict_json_load(MANIFEST_PATH)
    if not isinstance(value, dict):
        raise FixtureError("fixture manifest must be an object")
    validate_manifest_semantics(value)
    return value


def validate_manifest_semantics(manifest: Mapping[str, Any]) -> None:
    resources = manifest.get("resources")
    phases = manifest.get("phases")
    if not isinstance(resources, list) or not isinstance(phases, list):
        raise FixtureError("manifest resources/phases must be arrays")

    oracle_ids: set[str] = set()
    comparison_paths: set[str] = set()
    uids: set[str] = set()
    for resource in resources:
        if not isinstance(resource, dict):
            raise FixtureError("manifest resource must be an object")
        oracle_id = resource.get("oracle_id")
        if not isinstance(oracle_id, str) or oracle_id in oracle_ids:
            raise FixtureError("duplicate_or_invalid_oracle_id")
        oracle_ids.add(oracle_id)
        path = resource.get("path")
        if not isinstance(path, str):
            raise FixtureError("resource path must be a string")
        _display, comparison = normalize_resource_path(path)
        if comparison in comparison_paths:
            raise FixtureError("path_normalization_collision")
        comparison_paths.add(comparison)
        uid = resource.get("uid")
        if uid is not None:
            if not isinstance(uid, str) or uid in uids:
                raise FixtureError("duplicate_resource_uid")
            uids.add(uid)

    phase_names = [phase.get("name") for phase in phases if isinstance(phase, dict)]
    required_phases = {
        "base",
        "rename_uid",
        "rename_uidless",
        "delete",
        "re_add",
        "reimport",
        "content_edit",
        "journal_gap",
    }
    if set(phase_names) != required_phases or len(phase_names) != len(required_phases):
        raise FixtureError("manifest phase set is incomplete or duplicated")

    for resource in resources:
        for dependency in resource.get("dependencies", []):
            target = dependency.get("target")
            resolution = dependency.get("resolution")
            uid = dependency.get("uid")
            if target is not None and target not in oracle_ids:
                raise FixtureError("dependency target does not exist")
            if resolution == "resolved" and target is None:
                raise FixtureError("resolved dependency lacks target")
            if resolution == "missing" and uid is not None:
                raise FixtureError("path-only missing dependency unexpectedly has UID")
            if resolution == "stale_uid" and uid is None:
                raise FixtureError("stale UID dependency lacks UID")


def _safe_remove_work_root(work_root: Path) -> None:
    if not work_root.exists():
        return
    marker = work_root / WORK_MARKER
    if not marker.is_file():
        raise FixtureError(f"refusing to replace unmarked work root: {work_root}")
    shutil.rmtree(work_root)


def restore_project(work_root: Path) -> None:
    """Restore the canonical project into a disposable, marked working directory."""

    _safe_remove_work_root(work_root)
    work_root.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(
        PROJECT_SOURCE,
        work_root,
        ignore=shutil.ignore_patterns(".godot"),
    )
    (work_root / WORK_MARKER).write_text("Sprint 3 resource graph worktree\n", encoding="utf-8")


def _replace_resource_references(work_root: Path, old: str, new: str) -> None:
    for suffix in ("*.tres", "*.tscn"):
        for path in work_root.rglob(suffix):
            text = path.read_text(encoding="utf-8")
            if old in text:
                path.write_text(text.replace(old, new), encoding="utf-8")


def apply_phase(work_root: Path, phase: str, godot_bin: Path | None = None) -> None:
    """Apply one independent mutation to an already restored work project."""

    if not (work_root / WORK_MARKER).is_file():
        raise FixtureError("work root is not a restored resource graph fixture")

    if phase == "base" or phase == "journal_gap":
        return
    if phase == "rename_uid":
        old_resource = project_path(work_root, "res://resources/shared_leaf.tres")
        new_resource = project_path(work_root, "res://resources/renamed/shared_leaf_renamed.tres")
        new_resource.parent.mkdir(parents=True, exist_ok=True)
        old_resource.rename(new_resource)
        _replace_resource_references(
            work_root,
            "res://resources/shared_leaf.tres",
            "res://resources/renamed/shared_leaf_renamed.tres",
        )
        if godot_bin is not None:
            materialize_binary_resources(
                work_root,
                godot_bin,
                shared_path="res://resources/renamed/shared_leaf_renamed.tres",
            )
        return
    if phase == "rename_uidless":
        old_resource = project_path(work_root, "res://resources/twin_a.tres")
        new_resource = project_path(work_root, "res://resources/renamed/twin_a.tres")
        new_resource.parent.mkdir(parents=True, exist_ok=True)
        old_resource.rename(new_resource)
        return
    if phase == "delete":
        project_path(work_root, "res://resources/unique_leaf.res").unlink()
        return
    if phase == "re_add":
        path = project_path(work_root, "res://resources/unique_leaf.res")
        original = path.read_bytes()
        path.unlink()
        path.write_bytes(original)
        return
    if phase == "reimport":
        path = project_path(work_root, "res://assets/imported_icon.svg")
        text = path.read_text(encoding="utf-8")
        path.write_text(text.replace("#4a90e2", "#e24a90"), encoding="utf-8")
        return
    if phase == "content_edit":
        path = project_path(work_root, "res://resources/shared_leaf.tres")
        text = path.read_text(encoding="utf-8")
        path.write_text(text.replace("metadata/value = 1", "metadata/value = 101"), encoding="utf-8")
        return
    raise FixtureError(f"unknown fixture phase: {phase}")


def _run_godot(command: list[str], work_root: Path) -> str:
    result = subprocess.run(
        command,
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=120,
    )
    combined = result.stdout + result.stderr
    if result.returncode != 0:
        raise FixtureError(f"Godot fixture command failed ({result.returncode}):\n{combined}")
    absolute_root = str(work_root.resolve())
    if absolute_root in combined:
        raise FixtureError("Godot fixture log exposed the absolute work root")
    return combined


def materialize_binary_resources(
    work_root: Path,
    godot_bin: Path,
    *,
    shared_path: str = "res://resources/shared_leaf.tres",
) -> None:
    command = [
        str(godot_bin.resolve()),
        "--headless",
        "--editor",
        "--path",
        str(work_root.resolve()),
        "--script",
        str(PROBE_SCRIPT.resolve()),
        "--",
        "--mode",
        "materialize",
        "--shared-path",
        shared_path,
    ]
    _run_godot(command, work_root)


def _phase_resources(manifest: Mapping[str, Any], phase: str) -> list[dict[str, Any]]:
    resources = cast(list[dict[str, Any]], copy.deepcopy(manifest["resources"]))
    by_id = {resource["oracle_id"]: resource for resource in resources}
    if phase == "rename_uid":
        new = "res://resources/renamed/shared_leaf_renamed.tres"
        by_id["shared_leaf"]["path"] = new
        for resource in resources:
            for dependency in resource["dependencies"]:
                if dependency.get("target") == "shared_leaf":
                    dependency["fallback_path"] = new
    elif phase == "rename_uidless":
        by_id["twin_a"]["path"] = "res://resources/renamed/twin_a.tres"
    elif phase == "delete":
        resources = [resource for resource in resources if resource["oracle_id"] != "unique_leaf"]
        fan_out = next(resource for resource in resources if resource["oracle_id"] == "fan_out")
        for dependency in fan_out["dependencies"]:
            if dependency.get("target") == "unique_leaf":
                dependency["target"] = None
                dependency["resolution"] = "stale_uid"
    return resources


def _phase_spec(manifest: Mapping[str, Any], phase: str) -> Mapping[str, Any]:
    phase_specs = cast(list[dict[str, Any]], manifest["phases"])
    for phase_spec in phase_specs:
        if phase_spec["name"] == phase:
            return phase_spec
    raise FixtureError(f"manifest does not define phase: {phase}")


def build_expected_phase(manifest: Mapping[str, Any], phase: str, work_root: Path) -> dict[str, Any]:
    """Build expected truth only from the human manifest and fixture bytes."""

    phase_resources = _phase_resources(manifest, phase)
    golden_resources: list[dict[str, Any]] = []
    entity_by_oracle: dict[str, str] = {}
    path_by_oracle: dict[str, str] = {}
    for resource in phase_resources:
        path = resource["path"]
        file_path = project_path(work_root, path)
        if not file_path.is_file():
            raise FixtureError(f"phase {phase} resource is missing: {path}")
        digest = sha256_file(file_path)
        content_generation = f"sha256:{digest}"
        uid = resource["uid"]
        entity_id = uid_entity_id(uid) if uid is not None else fallback_entity_id(path, content_generation)
        hash_assertion: dict[str, str] = {"mode": resource["hash_mode"]}
        if resource["hash_mode"] == "exact_sha256":
            hash_assertion["sha256"] = digest
        golden_resource: dict[str, Any] = {
            "entity_id": entity_id,
            "format": resource["format"],
            "hash_assertion": hash_assertion,
            "identity_scope": "persistent" if uid is not None else "content_revision",
            "identity_strength": ("resource_uid" if uid is not None else "path_content_generation"),
            "import_state": resource["import_state"],
            "imported": resource["imported"],
            "oracle_id": resource["oracle_id"],
            "path": path,
            "type": resource["type"],
            "uid": uid,
        }
        if resource["hash_mode"] == "exact_sha256" or uid is None:
            golden_resource["content_generation"] = content_generation
        golden_resources.append(golden_resource)
        entity_by_oracle[resource["oracle_id"]] = entity_id
        path_by_oracle[resource["oracle_id"]] = path

    dependencies: list[dict[str, Any]] = []
    diagnostics: list[dict[str, Any]] = []
    direct_queries: dict[str, list[str]] = {resource["oracle_id"]: [] for resource in phase_resources}
    for resource in phase_resources:
        source_id = resource["oracle_id"]
        source_entity_id = entity_by_oracle[source_id]
        for dependency in resource["dependencies"]:
            target_uid = dependency.get("uid")
            fallback_path = dependency["fallback_path"]
            target = dependency.get("target")
            resolution = dependency["resolution"]
            resolved_target_entity_id = entity_by_oracle.get(target) if target else None
            edge_id = dependency_edge_id(source_entity_id, target_uid, fallback_path)
            dependencies.append({
                "authority": "godot_resource_loader",
                "edge_id": edge_id,
                "fallback_path": fallback_path,
                "resolution": resolution,
                "resolved_target": target,
                "resolved_target_entity_id": resolved_target_entity_id,
                "source": source_id,
                "source_entity_id": source_entity_id,
                "target_uid": target_uid,
            })
            direct_queries[source_id].append(edge_id)
            if resolution == "missing":
                diagnostics.append({
                    "code": "missing_dependency",
                    "silent_rebind": False,
                    "source": source_id,
                    "target_reference": fallback_path,
                })
            elif resolution == "stale_uid":
                diagnostics.append({
                    "code": "stale_resource_uid",
                    "silent_rebind": False,
                    "source": source_id,
                    "target_reference": target_uid,
                })

    golden_resources.sort(key=lambda value: (normalize_resource_path(value["path"])[1], value["entity_id"]))
    dependencies.sort(
        key=lambda value: (
            path_by_oracle[value["source"]],
            value["target_uid"] or value["fallback_path"],
            value["edge_id"],
        )
    )
    diagnostics.sort(key=lambda value: (value["code"], value["source"], value["target_reference"]))
    for edge_ids in direct_queries.values():
        edge_ids.sort()
    phase_spec = _phase_spec(manifest, phase)
    return {
        "dependencies": dependencies,
        "diagnostics": diagnostics,
        "exclusions": list(manifest["exclusions"]),
        "expected_direct_queries": dict(sorted(direct_queries.items())),
        "name": phase,
        "resources": golden_resources,
        "transition": {
            "assertions": phase_spec["assertions"],
            "from": None if phase == "base" else "base",
            "mutation": phase_spec["mutation"],
        },
    }


def _prepare_phase(manifest: Mapping[str, Any], phase: str, work_root: Path, godot_bin: Path | None) -> dict[str, Any]:
    restore_project(work_root)
    apply_phase(work_root, phase, godot_bin)
    return build_expected_phase(manifest, phase, work_root)


def update_golden(manifest: Mapping[str, Any], work_root: Path, godot_bin: Path) -> None:
    phases: list[dict[str, Any]] = []
    for phase_spec in manifest["phases"]:
        phases.append(_prepare_phase(manifest, phase_spec["name"], work_root, godot_bin))
    golden = {
        "fixture_version": manifest["fixture_version"],
        "phases": phases,
        "required_godot": manifest["required_godot"],
        "schema_version": 1,
    }
    GOLDEN_PATH.write_text(canonical_json(golden), encoding="utf-8")
    _safe_remove_work_root(work_root)


def run_probe(
    work_root: Path,
    godot_bin: Path,
    phase: str,
    expected_resources: Iterable[Mapping[str, Any]],
) -> tuple[dict[str, Any], str]:
    request_path = work_root.parent / f"probe-{phase}-request.json"
    output_path = work_root.parent / f"probe-{phase}-output.json"
    request = {
        "phase": phase,
        "paths": sorted(resource["path"] for resource in expected_resources),
    }
    request_path.write_text(canonical_json(request), encoding="utf-8")
    command = [
        str(godot_bin.resolve()),
        "--headless",
        "--editor",
        "--path",
        str(work_root.resolve()),
        "--script",
        str(PROBE_SCRIPT.resolve()),
        "--",
        "--mode",
        "probe",
        "--request",
        str(request_path.resolve()),
        "--output",
        str(output_path.resolve()),
    ]
    log = _run_godot(command, work_root)
    observed = strict_json_load(output_path)
    request_path.unlink(missing_ok=True)
    output_path.unlink(missing_ok=True)
    if not isinstance(observed, dict):
        raise FixtureError("Godot probe output must be an object")
    return observed, log


def _expected_probe_dependencies(phase: Mapping[str, Any], source: str) -> list[dict[str, Any]]:
    resources_by_id = {resource["oracle_id"]: resource for resource in phase["resources"]}
    expected: list[dict[str, Any]] = []
    for edge in phase["dependencies"]:
        if edge["source"] != source:
            continue
        target = edge["resolved_target"]
        expected.append({
            "fallback_path": edge["fallback_path"],
            "resolution": edge["resolution"],
            "resolved_path": resources_by_id[target]["path"] if target else None,
            "uid": edge["target_uid"],
        })
    expected.sort(key=lambda value: canonical_json(value))
    return expected


def compare_probe(phase: Mapping[str, Any], observed: Mapping[str, Any]) -> None:
    expected_by_path = {resource["path"]: resource for resource in phase["resources"]}
    observed_resources = observed.get("resources")
    if not isinstance(observed_resources, list):
        raise FixtureError("probe resources must be an array")
    observed_by_path = {resource.get("path"): resource for resource in observed_resources}
    if set(observed_by_path) != set(expected_by_path):
        raise FixtureError(f"probe resource path set differs in phase {phase['name']}")

    oracle_by_path = {resource["path"]: resource["oracle_id"] for resource in phase["resources"]}
    for path, expected in expected_by_path.items():
        actual = observed_by_path[path]
        for field in ("uid", "type", "imported", "import_state"):
            if actual.get(field) != expected[field]:
                raise FixtureError(f"probe {field} mismatch for {path}: {actual.get(field)!r} != {expected[field]!r}")
        actual_dependencies = [
            {
                "fallback_path": dependency.get("fallback_path"),
                "resolution": dependency.get("resolution"),
                "resolved_path": dependency.get("resolved_path"),
                "uid": dependency.get("uid"),
            }
            for dependency in actual.get("dependencies", [])
        ]
        actual_dependencies.sort(key=lambda value: canonical_json(value))
        expected_dependencies = _expected_probe_dependencies(phase, oracle_by_path[path])
        if actual_dependencies != expected_dependencies:
            raise FixtureError(
                f"probe dependency mismatch for {path}:\n"
                f"observed={canonical_json(actual_dependencies)}"
                f"expected={canonical_json(expected_dependencies)}"
            )


def validate_phase_invariants(phase: Mapping[str, Any]) -> None:
    resources = phase["resources"]
    dependencies = phase["dependencies"]
    resource_ids = [resource["oracle_id"] for resource in resources]
    entity_ids = [resource["entity_id"] for resource in resources]
    paths = [normalize_resource_path(resource["path"])[1] for resource in resources]
    if len(resource_ids) != len(set(resource_ids)):
        raise FixtureError("golden phase contains duplicate oracle IDs")
    if len(entity_ids) != len(set(entity_ids)):
        raise FixtureError("golden phase contains duplicate entity IDs")
    if len(paths) != len(set(paths)):
        raise FixtureError("golden phase contains path normalization collision")

    direct: dict[str, list[str]] = {oracle_id: [] for oracle_id in resource_ids}
    reverse: dict[str, list[str]] = {oracle_id: [] for oracle_id in resource_ids}
    for edge in dependencies:
        direct[edge["source"]].append(edge["edge_id"])
        target = edge["resolved_target"]
        if target is not None:
            reverse[target].append(edge["edge_id"])
    for edge_ids in direct.values():
        edge_ids.sort()
    if dict(sorted(direct.items())) != phase["expected_direct_queries"]:
        raise FixtureError("golden direct queries differ from the direct edge set")
    if sum(map(len, reverse.values())) != sum(1 for edge in dependencies if edge["resolved_target"] is not None):
        raise FixtureError("derived reverse edge parity failed")

    semantic_payload = {
        "dependencies": dependencies,
        "diagnostics": phase["diagnostics"],
        "resources": resources,
    }
    serialized = canonical_json(semantic_payload)
    if "/Users/" in serialized or "C:\\" in serialized or ".godot/imported/" in serialized:
        raise FixtureError("golden phase exposes a forbidden path")


def validate_transition_relations(golden: Mapping[str, Any]) -> None:
    phases = {phase["name"]: phase for phase in golden["phases"]}
    base_resources = {resource["oracle_id"]: resource for resource in phases["base"]["resources"]}
    renamed_uid = {resource["oracle_id"]: resource for resource in phases["rename_uid"]["resources"]}
    if base_resources["shared_leaf"]["entity_id"] != renamed_uid["shared_leaf"]["entity_id"]:
        raise FixtureError("UID-preserving rename changed the shared entity ID")
    renamed_uidless = {resource["oracle_id"]: resource for resource in phases["rename_uidless"]["resources"]}
    if base_resources["twin_a"]["entity_id"] == renamed_uidless["twin_a"]["entity_id"]:
        raise FixtureError("UID-less rename retained a persistent identity")
    if base_resources["twin_a"]["content_generation"] != base_resources["twin_b"]["content_generation"]:
        raise FixtureError("equal-content twin bytes differ")
    if base_resources["twin_a"]["entity_id"] == base_resources["twin_b"]["entity_id"]:
        raise FixtureError("equal-content resources share an entity ID")
    reimport = {resource["oracle_id"]: resource for resource in phases["reimport"]["resources"]}
    if base_resources["imported_icon"]["entity_id"] != reimport["imported_icon"]["entity_id"]:
        raise FixtureError("reimport changed imported source identity")
    if base_resources["imported_icon"]["content_generation"] == reimport["imported_icon"]["content_generation"]:
        raise FixtureError("reimport did not change source content generation")
    content_edit = {resource["oracle_id"]: resource for resource in phases["content_edit"]["resources"]}
    if base_resources["shared_leaf"]["entity_id"] != content_edit["shared_leaf"]["entity_id"]:
        raise FixtureError("UID-backed content edit changed entity identity")
    if base_resources["shared_leaf"]["content_generation"] == content_edit["shared_leaf"]["content_generation"]:
        raise FixtureError("content edit did not change content generation")


def run_draft_schema_tests() -> None:
    cargo = shutil.which("cargo")
    if cargo is None:
        raise FixtureError("cargo is required for Draft 2020-12 oracle validation")
    result = subprocess.run(
        [
            cargo,
            "test",
            "--manifest-path",
            str(SCRIPT_DIR / "Cargo.toml"),
            "--locked",
            "--offline",
            "resource_graph_contract",
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
        raise FixtureError("Draft 2020-12 resource graph schema tests failed:\n" + result.stdout + result.stderr)


def validate_all(manifest: Mapping[str, Any], work_root: Path, godot_bin: Path) -> dict[str, Any]:
    run_draft_schema_tests()
    golden = strict_json_load(GOLDEN_PATH)
    if not isinstance(golden, dict):
        raise FixtureError("golden graph must be an object")
    golden_phases = {phase["name"]: phase for phase in golden.get("phases", [])}
    expected_phase_names = [phase["name"] for phase in manifest["phases"]]
    if list(golden_phases) != expected_phase_names:
        raise FixtureError("golden phase order differs from the manifest")

    phase_summaries: list[dict[str, Any]] = []
    for phase_name in expected_phase_names:
        computed = _prepare_phase(manifest, phase_name, work_root, godot_bin)
        committed = golden_phases[phase_name]
        if computed != committed:
            raise FixtureError(f"committed golden truth drifted for phase {phase_name}")
        validate_phase_invariants(committed)
        observed, log = run_probe(work_root, godot_bin, phase_name, committed["resources"])
        compare_probe(committed, observed)
        if str(work_root.resolve()) in canonical_json(observed) or str(work_root.resolve()) in log:
            raise FixtureError("probe evidence exposed the absolute work root")
        phase_summaries.append({
            "dependencies": len(committed["dependencies"]),
            "diagnostics": len(committed["diagnostics"]),
            "name": phase_name,
            "resources": len(committed["resources"]),
            "status": "passed",
        })

    validate_transition_relations(golden)
    restore_project(work_root)
    first = tree_digest(work_root)
    restore_project(work_root)
    second = tree_digest(work_root)
    if first != second:
        raise FixtureError("two fixture restores are not byte-identical")
    _safe_remove_work_root(work_root)

    version_result = subprocess.run(
        [str(godot_bin.resolve()), "--version"],
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    evidence = {
        "checks": [
            "strict_json",
            "draft_2020_12_schemas",
            "identity_vectors",
            "path_security_vectors",
            "golden_graph",
            "direct_reverse_parity",
            "godot_api_projection",
            "mutation_transitions",
            "restore_idempotence",
            "path_redaction",
        ],
        "fixture_version": manifest["fixture_version"],
        "godot_version": version_result.stdout.strip(),
        "golden_sha256": sha256_file(GOLDEN_PATH),
        "identity_vectors_sha256": sha256_file(IDENTITY_VECTORS_PATH),
        "phases": phase_summaries,
        "schema_version": 1,
        "status": "passed",
    }
    EVIDENCE_PATH.parent.mkdir(parents=True, exist_ok=True)
    EVIDENCE_PATH.write_text(canonical_json(evidence), encoding="utf-8")
    return evidence


def tree_digest(root: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted(candidate for candidate in root.rglob("*") if candidate.is_file()):
        relative = path.relative_to(root).as_posix()
        if relative == WORK_MARKER or relative.startswith(".godot/"):
            continue
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def find_godot_binary(value: str | None) -> Path:
    if value:
        candidate = Path(value)
    else:
        candidate = REPOSITORY_ROOT / "bin" / "godot.macos.editor.dev.arm64"
    if not candidate.is_file() or not os.access(candidate, os.X_OK):
        raise FixtureError(f"Godot editor binary is unavailable: {candidate}")
    return candidate


def validate_identity_vectors() -> None:
    vectors = strict_json_load(IDENTITY_VECTORS_PATH)
    if not isinstance(vectors, dict):
        raise FixtureError("identity vectors must be an object")
    names: set[str] = set()
    computed_by_name: dict[str, str] = {}
    for vector in vectors["identity_vectors"]:
        name = vector["name"]
        if name in names:
            raise FixtureError("identity vector names must be unique")
        names.add(name)
        if vector["kind"] == "resource_uid":
            computed = uid_entity_id(vector["uid"])
        else:
            computed = fallback_entity_id(vector["path"], vector["content_generation"])
        if computed != vector["expected_entity_id"]:
            raise FixtureError(f"identity vector mismatch: {name}")
        computed_by_name[name] = computed
    for vector in vectors["identity_vectors"]:
        if "same_identity_as" in vector:
            if computed_by_name[vector["name"]] != computed_by_name[vector["same_identity_as"]]:
                raise FixtureError(f"same-identity relation failed: {vector['name']}")
        if "different_identity_from" in vector:
            if computed_by_name[vector["name"]] == computed_by_name[vector["different_identity_from"]]:
                raise FixtureError(f"different-identity relation failed: {vector['name']}")

    for vector in vectors["path_vectors"]:
        try:
            display, comparison = normalize_resource_path(vector["input"])
        except FixtureError as error:
            if vector["valid"] or str(error) != vector["error"]:
                raise FixtureError(f"path vector mismatch: {vector['name']}") from error
        else:
            if not vector["valid"]:
                raise FixtureError(f"invalid path vector was accepted: {vector['name']}")
            if display != vector["display_path"] or comparison != vector["comparison_path"]:
                raise FixtureError(f"normalized path vector mismatch: {vector['name']}")


def parse_arguments(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "command",
        choices=("restore", "apply", "materialize", "probe", "update-golden", "validate"),
    )
    parser.add_argument("--phase", default="base")
    parser.add_argument("--all-phases", action="store_true")
    parser.add_argument("--work-root", type=Path, default=DEFAULT_WORK_ROOT)
    parser.add_argument("--godot-bin")
    parser.add_argument("--output", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    arguments = parse_arguments(sys.argv[1:] if argv is None else argv)
    try:
        manifest = load_manifest()
        validate_identity_vectors()
        work_root = arguments.work_root.resolve()
        if arguments.command == "restore":
            restore_project(work_root)
            print(work_root)
        elif arguments.command == "apply":
            apply_phase(work_root, arguments.phase, find_godot_binary(arguments.godot_bin))
            print(arguments.phase)
        elif arguments.command == "materialize":
            materialize_binary_resources(work_root, find_godot_binary(arguments.godot_bin))
            print("materialized")
        elif arguments.command == "probe":
            godot_bin = find_godot_binary(arguments.godot_bin)
            expected = _prepare_phase(manifest, arguments.phase, work_root, godot_bin)
            observed, _log = run_probe(work_root, godot_bin, arguments.phase, expected["resources"])
            output = canonical_json(observed)
            if arguments.output:
                arguments.output.write_text(output, encoding="utf-8")
            else:
                print(output, end="")
        elif arguments.command == "update-golden":
            update_golden(manifest, work_root, find_godot_binary(arguments.godot_bin))
            print(GOLDEN_PATH.relative_to(REPOSITORY_ROOT))
        elif arguments.command == "validate":
            evidence = validate_all(manifest, work_root, find_godot_binary(arguments.godot_bin))
            print(canonical_json(evidence), end="")
        return 0
    except (FixtureError, OSError, subprocess.SubprocessError) as error:
        print(f"resource graph fixture: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
