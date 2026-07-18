#!/usr/bin/env python3
"""Validate the independent Sprint 4 scene graph contract fixture."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import re
import shutil
import subprocess
import sys
import unicodedata
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
PROJECT_SOURCE = SCRIPT_DIR / "fixtures" / "scene_graph_project"
ORACLE_ROOT = SCRIPT_DIR / "fixtures" / "scene_graph_oracle"
MANIFEST_PATH = ORACLE_ROOT / "fixture-manifest.json"
IDENTITY_VECTORS_PATH = ORACLE_ROOT / "identity-vectors.json"
GOLDEN_PATH = ORACLE_ROOT / "golden-scene-graph.json"

SCHEMA_CASES = (
    ("fixture-manifest.schema.json", "fixture-manifest.json"),
    ("identity-vectors.schema.json", "identity-vectors.json"),
    ("golden-scene-graph.schema.json", "golden-scene-graph.json"),
)
PHASES = (
    "base",
    "node_rename",
    "node_reparent",
    "property_override",
    "instance_mutation",
    "signal_group",
    "animation_fix",
    "journal_gap",
)
PROJECT_KEYS = {
    "application/run/main_scene",
    "autoload/SceneService",
    "input/jump",
    "layer_names/2d_physics/1",
    "layer_names/2d_render/1",
    "layer_names/3d_physics/1",
    "layer_names/3d_render/1",
    "layer_names/navigation/1",
}


class FixtureError(RuntimeError):
    """Raised when committed Sprint 4 contract truth is inconsistent."""


def strict_json_load(path: Path) -> Any:
    """Load UTF-8 JSON and reject duplicate members at every depth."""

    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise FixtureError(f"duplicate JSON member in {path.name}: {key}")
            result[key] = value
        return result

    try:
        return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=reject_duplicates)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise FixtureError(f"cannot read strict JSON {path.name}: {error}") from error


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _digest(domain: str, parts: Sequence[str]) -> str:
    value = domain.encode("utf-8") + b"\0" + b"\0".join(part.encode("utf-8") for part in parts)
    return base64.urlsafe_b64encode(hashlib.sha256(value).digest()).decode("ascii").rstrip("=")


def normalize_resource_path(value: str) -> str:
    if not value.startswith("res://") or "\\" in value or "\0" in value:
        raise FixtureError("invalid_resource_path")
    parts: list[str] = []
    for part in value.removeprefix("res://").split("/"):
        if not part or part == ".":
            continue
        if part == ".." or any(character in part for character in (":", "?", "#")):
            raise FixtureError("invalid_resource_path")
        if any(ord(character) < 32 for character in part):
            raise FixtureError("invalid_resource_path")
        parts.append(part)
    if not parts:
        raise FixtureError("invalid_resource_path")
    return unicodedata.normalize("NFC", "res://" + "/".join(parts))


def compute_identity(vector: Mapping[str, Any], algorithms: Mapping[str, str]) -> str:
    kind = vector["kind"]
    if kind == "scene_uid":
        prefix, domain, parts = "godot:scene:uid:v1:", algorithms["scene_uid_domain"], [vector["uid"]]
    elif kind == "scene_path_content":
        prefix = "godot:scene:path-content:v1:"
        domain = algorithms["scene_path_content_domain"]
        parts = [normalize_resource_path(vector["path"]), vector["content_generation"]]
    elif kind == "node_scene_unique":
        prefix = "godot:node:scene-id:v1:"
        domain = algorithms["node_scene_unique_domain"]
        parts = [vector["scene_id"], str(vector["unique_scene_id"])]
    elif kind == "node_path_content":
        prefix = "godot:node:path-content:v1:"
        domain = algorithms["node_path_content_domain"]
        parts = [vector["scene_id"], vector["node_path"], vector["content_generation"]]
    elif kind == "node_occurrence":
        prefix = "godot:node-occurrence:v1:"
        domain = algorithms["node_occurrence_domain"]
        parts = [vector["root_scene_id"], *vector["instance_chain"], vector["node_definition_id"]]
    elif kind == "subresource_scene_unique":
        prefix = "godot:subresource:scene-id:v1:"
        domain = algorithms["subresource_scene_unique_domain"]
        parts = [vector["owner_entity_id"], vector["scene_unique_id"]]
    elif kind == "subresource_content_revision":
        prefix = "godot:subresource:content-revision:v1:"
        domain = algorithms["subresource_content_revision_domain"]
        parts = [
            vector["owner_entity_id"],
            vector["owner_content_generation"],
            *sorted(vector["ownership_paths"]),
            vector["resource_type"],
            str(vector["ordinal"]),
        ]
    else:
        raise FixtureError(f"unsupported identity vector kind: {kind}")
    return prefix + _digest(domain, parts)


def validate_identity_vectors() -> dict[str, str]:
    document = strict_json_load(IDENTITY_VECTORS_PATH)
    if not isinstance(document, dict):
        raise FixtureError("identity vectors must be an object")
    algorithms = document.get("algorithms")
    vectors = document.get("vectors")
    if not isinstance(algorithms, dict) or not isinstance(vectors, list):
        raise FixtureError("identity algorithms/vectors are malformed")

    computed: dict[str, str] = {}
    for vector in vectors:
        if not isinstance(vector, dict) or not isinstance(vector.get("name"), str):
            raise FixtureError("identity vector is malformed")
        name = vector["name"]
        if name in computed:
            raise FixtureError("duplicate identity vector name")
        actual = compute_identity(vector, algorithms)
        if actual != vector.get("expected_id"):
            raise FixtureError(f"identity vector mismatch: {name}")
        computed[name] = actual

    for vector in vectors:
        name = vector["name"]
        same = vector.get("same_identity_as")
        different = vector.get("different_identity_from")
        if same is not None and (same not in computed or computed[name] != computed[same]):
            raise FixtureError(f"same-identity relation failed: {name}")
        if different is not None and (different not in computed or computed[name] == computed[different]):
            raise FixtureError(f"different-identity relation failed: {name}")
    return computed


def load_manifest() -> dict[str, Any]:
    value = strict_json_load(MANIFEST_PATH)
    if not isinstance(value, dict):
        raise FixtureError("fixture manifest must be an object")
    validate_manifest(value)
    return value


def validate_manifest(manifest: Mapping[str, Any]) -> None:
    files = manifest.get("files")
    phases = manifest.get("phases")
    if not isinstance(files, list) or not isinstance(phases, list):
        raise FixtureError("manifest files/phases must be arrays")
    if tuple(phase.get("name") for phase in phases if isinstance(phase, dict)) != PHASES:
        raise FixtureError("manifest phase sequence is not canonical")

    declared: set[str] = set()
    for item in files:
        if not isinstance(item, dict) or not isinstance(item.get("path"), str):
            raise FixtureError("manifest file is malformed")
        relative = item["path"]
        if relative in declared or Path(relative).is_absolute() or ".." in Path(relative).parts:
            raise FixtureError("duplicate_or_unsafe_manifest_path")
        declared.add(relative)
        path = PROJECT_SOURCE / relative
        if not path.is_file() or sha256_file(path) != item.get("sha256"):
            raise FixtureError(f"fixture file hash mismatch: {relative}")

    actual = {
        path.relative_to(PROJECT_SOURCE).as_posix()
        for path in PROJECT_SOURCE.rglob("*")
        if path.is_file() and path.name != ".gitignore" and ".godot" not in path.parts
    }
    if actual != declared:
        raise FixtureError("fixture manifest does not cover the source tree exactly")


def _records(golden: Mapping[str, Any], category: str) -> dict[str, Mapping[str, Any]]:
    values = golden.get(category)
    if not isinstance(values, list):
        raise FixtureError(f"golden category is not an array: {category}")
    records: dict[str, Mapping[str, Any]] = {}
    for value in values:
        if not isinstance(value, dict) or not isinstance(value.get("oracle_id"), str):
            raise FixtureError(f"malformed golden record: {category}")
        if value["oracle_id"] in records:
            raise FixtureError(f"duplicate oracle ID in {category}: {value['oracle_id']}")
        records[value["oracle_id"]] = value
    return records


def _require_reference(value: Any, records: Mapping[str, Any], label: str, nullable: bool = False) -> None:
    if value is None and nullable:
        return
    if not isinstance(value, str) or value not in records:
        raise FixtureError(f"invalid {label} reference: {value}")


def validate_golden() -> dict[str, dict[str, Mapping[str, Any]]]:
    golden = strict_json_load(GOLDEN_PATH)
    if not isinstance(golden, dict):
        raise FixtureError("golden scene graph must be an object")
    categories = (
        "scenes", "nodes", "occurrences", "properties", "resource_references", "instances", "subresources",
        "connections", "groups", "animation_tracks", "project_context", "diagnostics",
    )
    records = {category: _records(golden, category) for category in categories}
    scenes, nodes, occurrences = records["scenes"], records["nodes"], records["occurrences"]

    all_oracle_ids: set[str] = set()
    for category, items in records.items():
        overlap = all_oracle_ids & set(items)
        if overlap:
            raise FixtureError(f"oracle IDs overlap across categories: {sorted(overlap)}")
        all_oracle_ids.update(items)

    algorithms = strict_json_load(IDENTITY_VECTORS_PATH)["algorithms"]
    for scene in scenes.values():
        path = normalize_resource_path(scene["path"])
        relative = path.removeprefix("res://")
        if sha256_file(PROJECT_SOURCE / relative) != scene["content_generation"].removeprefix("sha256:"):
            raise FixtureError(f"golden scene generation mismatch: {scene['oracle_id']}")
        vector = {"kind": "scene_uid", "uid": scene["uid"]}
        if compute_identity(vector, algorithms) != scene["entity_id"]:
            raise FixtureError(f"golden scene identity mismatch: {scene['oracle_id']}")
        _require_reference(scene.get("base_scene"), scenes, "base scene", nullable=True)

    node_ids: set[str] = set()
    for node in nodes.values():
        _require_reference(node.get("scene"), scenes, "node scene")
        _require_reference(node.get("parent"), nodes, "node parent", nullable=True)
        _require_reference(node.get("owner"), nodes, "node owner", nullable=True)
        vector = {
            "kind": "node_scene_unique",
            "scene_id": scenes[node["scene"]]["entity_id"],
            "unique_scene_id": node["unique_scene_id"],
        }
        if compute_identity(vector, algorithms) != node["entity_id"]:
            raise FixtureError(f"golden node identity mismatch: {node['oracle_id']}")
        if node["entity_id"] in node_ids:
            raise FixtureError("duplicate canonical node identity")
        node_ids.add(node["entity_id"])

    for occurrence in occurrences.values():
        _require_reference(occurrence.get("root_scene"), scenes, "occurrence root scene")
        _require_reference(occurrence.get("definition"), nodes, "occurrence definition")
        for member in occurrence.get("instance_chain", []):
            _require_reference(member, nodes, "occurrence instance chain")
        vector = {
            "kind": "node_occurrence",
            "root_scene_id": scenes[occurrence["root_scene"]]["entity_id"],
            "instance_chain": [nodes[item]["entity_id"] for item in occurrence["instance_chain"]],
            "node_definition_id": nodes[occurrence["definition"]]["entity_id"],
        }
        if compute_identity(vector, algorithms) != occurrence["entity_id"]:
            raise FixtureError(f"golden occurrence identity mismatch: {occurrence['oracle_id']}")

    subjects = {**nodes, **occurrences}
    for fact in records["properties"].values():
        _require_reference(fact.get("subject"), subjects, "property subject")
        _require_reference(fact.get("declaring_node"), nodes, "property declaring node")
        _require_reference(fact.get("declaring_scene"), scenes, "property declaring scene")
        _require_reference(fact.get("overrides"), records["properties"], "overridden property", nullable=True)
        if fact.get("origin") not in {"local", "inherited", "instance_override"}:
            raise FixtureError("invalid property origin")

    for instance in records["instances"].values():
        _require_reference(instance.get("declaration_scope"), scenes, "instance declaration scope")
        _require_reference(instance.get("instance_root"), nodes, "instance root")
        _require_reference(instance.get("target_scene"), scenes, "instance target scene")
    for subresource in records["subresources"].values():
        _require_reference(subresource.get("owner"), scenes, "subresource owner")
        if (
            subresource["identity_scope"] == "content_revision"
            and subresource.get("diagnostic") != "weak_subresource_identity"
        ):
            raise FixtureError("weak subresource lacks its diagnostic")
        if subresource["identity_scope"] == "persistent":
            vector = {
                "kind": "subresource_scene_unique",
                "owner_entity_id": scenes[subresource["owner"]]["entity_id"],
                "scene_unique_id": subresource["scene_unique_id"],
            }
            if compute_identity(vector, algorithms) != subresource.get("entity_id"):
                raise FixtureError(f"golden subresource identity mismatch: {subresource['oracle_id']}")
    reference_owners = {**nodes, **records["subresources"]}
    reference_targets = {**records["subresources"]}
    for reference in records["resource_references"].values():
        _require_reference(reference.get("owner"), reference_owners, "resource reference owner")
        if reference.get("kind") == "subresource":
            _require_reference(reference.get("target"), reference_targets, "subresource reference target")
    for connection in records["connections"].values():
        _require_reference(connection.get("emitter"), subjects, "connection emitter")
        _require_reference(connection.get("receiver"), subjects, "connection receiver")
    for group in records["groups"].values():
        _require_reference(group.get("member"), subjects, "group member")
    for track in records["animation_tracks"].values():
        _require_reference(track.get("mixer"), nodes, "animation mixer")
        _require_reference(track.get("target"), subjects, "animation target", nullable=True)
        if track.get("resolution") not in {"resolved", "broken", "unresolved"}:
            raise FixtureError("invalid animation path resolution")
    for diagnostic in records["diagnostics"].values():
        diagnostic_subjects = {**subjects, **records["subresources"], **records["animation_tracks"]}
        _require_reference(diagnostic.get("subject"), diagnostic_subjects, "diagnostic subject")

    if {item["key"] for item in records["project_context"].values()} != PROJECT_KEYS:
        raise FixtureError("project context allowlist drifted")
    serialized = json.dumps(golden, ensure_ascii=False)
    for forbidden in ("/Users/", "C:\\", ".godot/imported", ".godot/codex"):
        if forbidden in serialized:
            raise FixtureError(f"golden graph leaks forbidden path material: {forbidden}")
    return records


def _fixture_only_scene_semantics(path: Path) -> dict[str, Any]:
    """Project the order pair only; production code must never import this parser."""

    text = path.read_text(encoding="utf-8")
    blocks: dict[str, tuple[str, ...]] = {}
    pattern = re.compile(r'^\[sub_resource type="([^"]+)" id="([^"]+)"\]\n(.*?)(?=^\[|\Z)', re.MULTILINE | re.DOTALL)
    for resource_type, resource_id, body in pattern.findall(text):
        blocks[resource_id] = (resource_type, *sorted(line for line in body.splitlines() if line))
    node_lines = sorted(line for line in text.splitlines() if line.startswith("metadata/"))
    return {"subresources": blocks, "node_properties": node_lines}


def validate_order_equivalence(manifest: Mapping[str, Any]) -> None:
    relation = manifest["order_equivalence"]
    left = _fixture_only_scene_semantics(PROJECT_SOURCE / relation["left"])
    right = _fixture_only_scene_semantics(PROJECT_SOURCE / relation["right"])
    if left != right:
        raise FixtureError("subresource order changed semantic truth")
    if (PROJECT_SOURCE / relation["left"]).read_bytes() == (PROJECT_SOURCE / relation["right"]).read_bytes():
        raise FixtureError("order-equivalence fixtures are not physically distinct")


def run_draft_schema_tests() -> None:
    cargo = shutil.which("cargo")
    if cargo is None:
        raise FixtureError("cargo is required for Draft 2020-12 validation")
    result = subprocess.run(
        [
            cargo, "test", "--manifest-path", str(SCRIPT_DIR / "Cargo.toml"), "--locked", "--offline",
            "scene_graph_contract", "--", "--nocapture",
        ],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if result.returncode != 0:
        raise FixtureError("Draft 2020-12 scene graph tests failed:\n" + result.stdout + result.stderr)


def validate_all(run_schemas: bool = True) -> dict[str, Any]:
    manifest = load_manifest()
    identities = validate_identity_vectors()
    records = validate_golden()
    validate_order_equivalence(manifest)
    if run_schemas:
        run_draft_schema_tests()
    return {
        "fixture_version": manifest["fixture_version"],
        "golden_sha256": sha256_file(GOLDEN_PATH),
        "identity_vectors": len(identities),
        "nodes": len(records["nodes"]),
        "occurrences": len(records["occurrences"]),
        "phases": len(manifest["phases"]),
        "status": "passed",
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("validate",))
    parser.add_argument("--skip-schemas", action="store_true")
    arguments = parser.parse_args(sys.argv[1:] if argv is None else argv)
    try:
        summary = validate_all(run_schemas=not arguments.skip_schemas)
    except FixtureError as error:
        print(f"scene graph fixture validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
