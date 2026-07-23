#!/usr/bin/env python3
"""Validate the independent Sprint 9 transaction fixture and golden oracle."""

from __future__ import annotations

import argparse
import ast
import copy
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
from collections.abc import Iterable, Mapping, Sequence
from pathlib import Path
from typing import Any, cast

SCRIPT_ROOT = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_ROOT.parent.parent
PROJECT_ROOT = SCRIPT_ROOT / "fixtures" / "transaction_prepare_project"
ORACLE_ROOT = SCRIPT_ROOT / "fixtures" / "transaction_oracle"
MANIFEST_PATH = ORACLE_ROOT / "fixture-manifest.json"
GOLDEN_PATH = ORACLE_ROOT / "golden-transactions.json"

OPERATIONS = (
    "attach_script",
    "connect_signal",
    "create_node",
    "delete_node",
    "detach_script",
    "disconnect_signal",
    "reparent_node",
    "set_property",
)
FAULT_POINTS = (
    "after_action_created_before_commit",
    "after_approval_before_preflight",
    "after_bridge_response_before_journal_ack",
    "after_commit_before_response",
    "after_prepare",
    "before_commit",
    "restart_after_commit",
    "restart_while_prepared",
)
STALE_CASES = (
    "changed_target",
    "editor_session_replaced",
    "history_advanced",
    "operation_sequence_advanced",
    "scene_closed",
    "scene_revision_advanced",
)
CANARIES = (
    "S9_BIND_SECRET_SENTINEL",
    "S9_DELETED_SUBTREE_CANARY_7b1f0a26",
    "S9_PROPERTY_CANARY_3d842ac9",
)
FORBIDDEN_OUTPUT_FIELDS = (
    "approval",
    "binds",
    "handle",
    "mac",
    "native_history_id",
    "nonce",
    "object_id",
    "pointer",
    "preview_payload_json",
    "receipt",
    "rid",
    "session_token",
)
SAFE_CONTROL_METADATA_KEYS = {
    "approval_message_bytes",
    "approval_timeout_ms",
    "receipt_ttl_ms",
}
VOLATILE_ID_KEYS = {
    "editor_session_id",
    "history_id",
    "native_history_id",
    "node_id",
    "object_id",
    "transaction_id",
}
SAFE_ID_PATTERNS = {
    "transaction_id": re.compile(r"^transaction:[0-9a-f]{32}$"),
    "node_id": re.compile(r"^node:[0-9a-f]{32}$"),
    "history_id": re.compile(r"^history:[0-9a-f]{32}$"),
    "editor_session_id": re.compile(r"^editor:[0-9a-f]{32}$"),
}
MANIFEST_FIELDS = {
    "schema_version",
    "project",
    "golden",
    "golden_sha256",
    "files",
    "operations",
    "fault_points",
    "canary_classes",
    "forbidden_output_fields",
}
GOLDEN_FIELDS = {
    "schema_version",
    "identity_policy",
    "baseline",
    "operations",
    "faults",
    "stale_cases",
    "limits",
    "leak_policy",
}
PRIVATE_SNAPSHOT_FIELDS = {
    "schema_version",
    "probe_seq",
    "tree",
    "connections",
    "selection",
    "property_hashes",
    "native_probe",
}


class FixtureError(RuntimeError):
    """Raised with a bounded, value-free oracle failure."""


def require(condition: bool, scenario: str, invariant: str) -> None:
    if not condition:
        raise FixtureError(f"{scenario}: {invariant}")


def strict_json(path: Path) -> dict[str, Any]:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise FixtureError(f"{path.name}: duplicate JSON member")
            result[key] = value
        return result

    def reject_constant(_value: str) -> None:
        raise FixtureError(f"{path.name}: non-finite JSON number")

    try:
        parsed = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=reject_duplicates,
            parse_constant=reject_constant,
        )
    except FixtureError:
        raise
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise FixtureError(f"{path.name}: invalid strict JSON") from error
    require(isinstance(parsed, dict), path.name, "root is not an object")
    return cast(dict[str, Any], parsed)


def canonical(value: Any) -> bytes:
    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise FixtureError("canonical: unsupported JSON value") from error


def digest_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def digest_file(path: Path) -> str:
    try:
        return digest_bytes(path.read_bytes())
    except OSError as error:
        raise FixtureError(f"{path.name}: cannot hash file") from error


def project_files(root: Path = PROJECT_ROOT) -> list[str]:
    return sorted(
        path.relative_to(root).as_posix()
        for path in root.rglob("*")
        if path.is_file() and ".godot" not in path.parts
    )


def source_fingerprint(root: Path) -> dict[str, dict[str, int | str]]:
    result: dict[str, dict[str, int | str]] = {}
    for relative in project_files(root):
        path = root / relative
        try:
            stat = path.stat()
            result[relative] = {
                "sha256": digest_file(path),
                "size": stat.st_size,
                "inode": stat.st_ino,
                "mtime_ns": stat.st_mtime_ns,
            }
        except OSError as error:
            raise FixtureError(f"source:{relative}: cannot fingerprint") from error
    return result


def assert_source_unchanged(
    before: Mapping[str, Mapping[str, int | str]],
    after: Mapping[str, Mapping[str, int | str]],
) -> None:
    require(set(before) == set(after), "source", "file set changed")
    for path in sorted(before):
        require(before[path] == after[path], f"source:{path}", "bytes or metadata changed")


def validate_manifest(manifest: Mapping[str, Any]) -> None:
    require(set(manifest) == MANIFEST_FIELDS, "manifest", "fields differ")
    require(
        manifest.get("schema_version") == "s9-transaction-fixture-manifest/1.0",
        "manifest",
        "schema version differs",
    )
    require(
        manifest.get("project") == "../transaction_prepare_project",
        "manifest",
        "project binding differs",
    )
    require(
        manifest.get("golden") == GOLDEN_PATH.name,
        "manifest",
        "golden path differs",
    )
    require(
        manifest.get("golden_sha256") == digest_file(GOLDEN_PATH),
        "manifest",
        "golden digest differs",
    )
    require(
        manifest.get("operations") == list(OPERATIONS),
        "manifest",
        "operation coverage differs",
    )
    require(
        manifest.get("fault_points") == list(FAULT_POINTS),
        "manifest",
        "fault coverage differs",
    )
    require(
        manifest.get("canary_classes")
        == sorted(value.upper() for value in CANARIES),
        "manifest",
        "canary classes differ",
    )
    require(
        manifest.get("forbidden_output_fields") == list(FORBIDDEN_OUTPUT_FIELDS),
        "manifest",
        "forbidden output fields differ",
    )
    records = manifest.get("files")
    require(isinstance(records, list), "manifest", "files are missing")
    paths: list[str] = []
    for index, record in enumerate(records):
        scenario = f"manifest:file:{index}"
        require(
            isinstance(record, dict) and set(record) == {"path", "sha256"},
            scenario,
            "fields differ",
        )
        path = record.get("path")
        digest = record.get("sha256")
        require(
            isinstance(path, str)
            and bool(path)
            and not path.startswith("/")
            and "\\" not in path
            and ".." not in Path(path).parts,
            scenario,
            "unsafe path",
        )
        require(
            isinstance(digest, str)
            and re.fullmatch(r"[0-9a-f]{64}", digest) is not None,
            scenario,
            "invalid digest",
        )
        candidate = PROJECT_ROOT / cast(str, path)
        require(candidate.is_file(), scenario, "fixture file is missing")
        require(digest_file(candidate) == digest, scenario, "fixture hash differs")
        paths.append(cast(str, path))
    require(paths == sorted(set(paths)), "manifest", "file order is not canonical")
    require(paths == project_files(), "manifest", "file closure differs")


def _assert_sorted_unique(
    records: Sequence[Mapping[str, Any]],
    key: str,
    scenario: str,
) -> None:
    values = [record.get(key) for record in records]
    require(
        all(isinstance(value, str) for value in values),
        scenario,
        f"{key} is malformed",
    )
    require(values == sorted(set(values)), scenario, f"{key} order is not canonical")


def _validate_state(state: Mapping[str, Any], scenario: str) -> None:
    require(
        set(state) == {"tree", "properties", "scripts", "connections", "selection"},
        scenario,
        "state fields differ",
    )
    tree = state.get("tree")
    properties = state.get("properties")
    scripts = state.get("scripts")
    connections = state.get("connections")
    selection = state.get("selection")
    require(isinstance(tree, list) and bool(tree), scenario, "tree is missing")
    require(isinstance(properties, list), scenario, "properties are missing")
    require(isinstance(scripts, list), scenario, "scripts are missing")
    require(isinstance(connections, list), scenario, "connections are missing")
    require(isinstance(selection, list), scenario, "selection is missing")
    _assert_sorted_unique(cast(list[Mapping[str, Any]], tree), "path", f"{scenario}:tree")
    property_keys = [
        (record.get("path"), record.get("name"))
        for record in cast(list[Mapping[str, Any]], properties)
    ]
    require(
        property_keys == sorted(set(property_keys)),
        f"{scenario}:properties",
        "order is not canonical",
    )
    _assert_sorted_unique(
        cast(list[Mapping[str, Any]], scripts),
        "path",
        f"{scenario}:scripts",
    )
    connection_keys = [
        (
            record.get("emitter"),
            record.get("signal"),
            record.get("receiver"),
            record.get("method"),
            record.get("flags"),
            record.get("unbinds"),
            tuple(record.get("bind_hashes", [])),
        )
        for record in cast(list[Mapping[str, Any]], connections)
    ]
    require(
        connection_keys == sorted(set(connection_keys)),
        f"{scenario}:connections",
        "order is not canonical",
    )
    require(selection == sorted(set(selection)), scenario, "selection is not canonical")
    serialized = canonical(state).decode("utf-8")
    for canary in CANARIES:
        require(canary not in serialized, scenario, "raw canary leaked into golden state")
    for volatile in VOLATILE_ID_KEYS:
        require(f'"{volatile}"' not in serialized, scenario, "volatile ID entered golden state")


def validate_golden(golden: Mapping[str, Any]) -> None:
    require(set(golden) == GOLDEN_FIELDS, "golden", "fields differ")
    require(
        golden.get("schema_version") == "s9-transaction-golden/1.0",
        "golden",
        "schema version differs",
    )
    baseline = golden.get("baseline")
    require(isinstance(baseline, dict), "golden", "baseline is missing")
    _validate_state(cast(Mapping[str, Any], baseline), "golden:baseline")
    require(
        len(cast(Mapping[str, Any], baseline).get("tree", [])) == 18,
        "golden:baseline",
        "fixture topology differs",
    )

    operations = golden.get("operations")
    require(isinstance(operations, list), "golden", "operations are missing")
    _assert_sorted_unique(
        cast(list[Mapping[str, Any]], operations),
        "kind",
        "golden:operations",
    )
    require(
        [record.get("kind") for record in operations] == list(OPERATIONS),
        "golden:operations",
        "coverage differs",
    )
    for record in operations:
        require(
            isinstance(record, dict)
            and set(record)
            == {
                "kind",
                "target",
                "risk",
                "scope",
                "states",
                "applied_delta",
                "revision_deltas",
                "native_actions",
                "undo_eligible",
            },
            "golden:operation",
            "fields differ",
        )
        require(
            record.get("states")
            == {
                "pre": "baseline",
                "prepared": "baseline",
                "applied": "materialize_applied_delta",
                "undone": "baseline",
                "redone": "materialize_applied_delta",
            },
            f"golden:{record.get('kind')}",
            "state references differ",
        )
        require(
            record.get("revision_deltas")
            == {"prepare": 0, "apply": 1, "undo": 1, "redo": 1},
            f"golden:{record.get('kind')}",
            "revision truth differs",
        )
        require(
            record.get("native_actions") == 1
            and record.get("undo_eligible") is True,
            f"golden:{record.get('kind')}",
            "native action truth differs",
        )
        materialized = materialize_operation(
            cast(Mapping[str, Any], baseline),
            cast(Mapping[str, Any], record),
        )
        _validate_state(materialized, f"golden:{record.get('kind')}:applied")
        require(
            canonical(materialized) != canonical(baseline),
            f"golden:{record.get('kind')}",
            "applied state is a no-op",
        )

    faults = golden.get("faults")
    require(isinstance(faults, list), "golden", "faults are missing")
    _assert_sorted_unique(cast(list[Mapping[str, Any]], faults), "point", "golden:faults")
    require(
        [record.get("point") for record in faults] == list(FAULT_POINTS),
        "golden:faults",
        "coverage differs",
    )
    for fault in faults:
        native_actions = fault.get("native_actions")
        mutation = fault.get("mutation")
        require(
            (native_actions == 1) == (mutation is True),
            f"golden:fault:{fault.get('point')}",
            "mutation/action truth differs",
        )
        if mutation is True:
            require(
                fault.get("replay_forbidden") is True,
                f"golden:fault:{fault.get('point')}",
                "committed fault permits replay",
            )

    stale = golden.get("stale_cases")
    require(isinstance(stale, list), "golden", "stale cases are missing")
    _assert_sorted_unique(
        cast(list[Mapping[str, Any]], stale),
        "kind",
        "golden:stale",
    )
    require(
        [record.get("kind") for record in stale] == list(STALE_CASES),
        "golden:stale",
        "coverage differs",
    )
    require(
        all(
            record.get("native_actions") == 0
            and record.get("executor_reached") is False
            for record in stale
        ),
        "golden:stale",
        "stale path reaches mutation",
    )

    require(
        golden.get("limits")
        == {
            "preview_bytes": 65_536,
            "operation_bytes": 65_536,
            "structural_nodes": 1_000,
            "journal_records": 1_024,
            "journal_bytes": 8_388_608,
            "status_bytes": 65_536,
            "main_thread_slice_usec": 2_000,
        },
        "golden:limits",
        "limits differ",
    )
    leak_policy = golden.get("leak_policy")
    require(isinstance(leak_policy, dict), "golden", "leak policy is missing")
    require(
        leak_policy.get("forbidden_key_fragments") == list(FORBIDDEN_OUTPUT_FIELDS),
        "golden:leak",
        "forbidden key coverage differs",
    )
    require(
        leak_policy.get("private_snapshot_retention")
        == "ephemeral_delete_after_case",
        "golden:leak",
        "private snapshot retention differs",
    )


def _path_parent(path: str) -> str:
    return path.rsplit("/", 1)[0] if "/" in path else "."


def _replace_prefix(path: str, old: str, new: str) -> str:
    if path == old:
        return new
    prefix = old + "/"
    return new + path[len(old) :] if path.startswith(prefix) else path


def _sort_state(state: dict[str, Any]) -> dict[str, Any]:
    state["tree"] = sorted(state["tree"], key=lambda item: item["path"])
    state["properties"] = sorted(
        state["properties"], key=lambda item: (item["path"], item["name"])
    )
    state["scripts"] = sorted(state["scripts"], key=lambda item: item["path"])
    state["connections"] = sorted(
        state["connections"],
        key=lambda item: (
            item["emitter"],
            item["signal"],
            item["receiver"],
            item["method"],
            item["flags"],
            item["unbinds"],
            tuple(item["bind_hashes"]),
        ),
    )
    state["selection"] = sorted(set(state["selection"]))
    return state


def _reindex_tree(tree: list[dict[str, Any]]) -> None:
    groups: dict[str, list[dict[str, Any]]] = {}
    for node in tree:
        if node["path"] == ".":
            continue
        groups.setdefault(_path_parent(node["path"]), []).append(node)
    for children in groups.values():
        children.sort(key=lambda item: (item["index"], item["path"]))
        for index, child in enumerate(children):
            child["index"] = index


def materialize_operation(
    baseline: Mapping[str, Any],
    operation: Mapping[str, Any],
) -> dict[str, Any]:
    state = cast(dict[str, Any], copy.deepcopy(dict(baseline)))
    delta = operation.get("applied_delta")
    require(isinstance(delta, dict), "materialize", "delta is missing")
    removed = set(cast(list[str], delta.get("remove_nodes", [])))
    state["tree"] = [
        node
        for node in state["tree"]
        if not any(
            node["path"] == path or node["path"].startswith(path + "/")
            for path in removed
        )
    ]
    for collection in ("properties", "scripts"):
        state[collection] = [
            item
            for item in state[collection]
            if not any(
                item["path"] == path or item["path"].startswith(path + "/")
                for path in removed
            )
        ]
    state["connections"] = [
        item
        for item in state["connections"]
        if not any(
            item["emitter"] == path
            or item["emitter"].startswith(path + "/")
            or item["receiver"] == path
            or item["receiver"].startswith(path + "/")
            for path in removed
        )
    ]
    remove_connections = {
        canonical(item) for item in delta.get("remove_connections", [])
    }
    state["connections"] = [
        item
        for item in state["connections"]
        if canonical(item) not in remove_connections
    ]

    for move in cast(list[dict[str, Any]], delta.get("move_nodes", [])):
        old = move["from"]
        new = move["to"]
        for collection in ("tree", "properties", "scripts"):
            for item in state[collection]:
                item["path"] = _replace_prefix(item["path"], old, new)
        for item in state["connections"]:
            item["emitter"] = _replace_prefix(item["emitter"], old, new)
            item["receiver"] = _replace_prefix(item["receiver"], old, new)
        for index, selected in enumerate(state["selection"]):
            state["selection"][index] = _replace_prefix(selected, old, new)
        moved = next(node for node in state["tree"] if node["path"] == new)
        moved["owner"] = move["owner"]
        moved["index"] = move["index"]

    state["tree"].extend(copy.deepcopy(delta.get("add_nodes", [])))

    for change in cast(list[dict[str, Any]], delta.get("property_changes", [])):
        state["properties"] = [
            item
            for item in state["properties"]
            if not (
                item["path"] == change["path"] and item["name"] == change["name"]
            )
        ]
        state["properties"].append(copy.deepcopy(change["after"]))
    for change in cast(list[dict[str, Any]], delta.get("script_changes", [])):
        script = next(
            item for item in state["scripts"] if item["path"] == change["path"]
        )
        require(
            script["script"] == change["before"],
            f"materialize:{operation.get('kind')}",
            "script precondition differs",
        )
        script["script"] = change["after"]
    for change in cast(list[dict[str, Any]], delta.get("transform_changes", [])):
        node = next(item for item in state["tree"] if item["path"] == change["path"])
        require(
            node["transform"] == change["before"],
            f"materialize:{operation.get('kind')}",
            "transform precondition differs",
        )
        node["transform"] = copy.deepcopy(change["after"])

    state["connections"].extend(copy.deepcopy(delta.get("add_connections", [])))
    _reindex_tree(state["tree"])
    return _sort_state(state)


def _connection_from_private(value: Mapping[str, Any]) -> dict[str, Any]:
    binds = value.get("binds")
    require(isinstance(binds, list), "private:connection", "binds are missing")
    bind_hashes: list[str] = []
    for bind in binds:
        if isinstance(bind, str):
            bind_hashes.append(digest_bytes(bind.encode("utf-8")))
        else:
            bind_hashes.append(digest_bytes(canonical(bind)))
    return {
        "emitter": value.get("emitter"),
        "signal": value.get("signal"),
        "receiver": value.get("receiver"),
        "method": value.get("method"),
        "flags": value.get("flags"),
        "unbinds": value.get("unbinds"),
        "bind_hashes": bind_hashes,
    }


def _normalize_semantic_numbers(value: Any) -> Any:
    """Match Godot's JSON float projection to the integer-valued golden state."""
    if isinstance(value, float) and math.isfinite(value) and value.is_integer():
        return int(value)
    if isinstance(value, list):
        return [_normalize_semantic_numbers(item) for item in value]
    if isinstance(value, dict):
        return {
            key: _normalize_semantic_numbers(item)
            for key, item in value.items()
        }
    return value


def normalize_private_snapshot(
    snapshot: Mapping[str, Any],
    golden: Mapping[str, Any],
    expected_state: Mapping[str, Any] | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    require(set(snapshot) == PRIVATE_SNAPSHOT_FIELDS, "private", "fields differ")
    require(
        snapshot.get("schema_version") == "s9-transaction-private-snapshot/1.0",
        "private",
        "schema version differs",
    )
    raw_tree = snapshot.get("tree")
    raw_connections = snapshot.get("connections")
    raw_selection = snapshot.get("selection")
    hashes = snapshot.get("property_hashes")
    native_probe = snapshot.get("native_probe")
    require(isinstance(raw_tree, list), "private", "tree is missing")
    require(isinstance(raw_connections, list), "private", "connections are missing")
    require(isinstance(raw_selection, list), "private", "selection is missing")
    require(isinstance(hashes, dict), "private", "property hashes are missing")
    require(isinstance(native_probe, dict), "private", "native probe is missing")
    rows = {
        item.get("path"): item
        for item in raw_tree
        if isinstance(item, dict) and isinstance(item.get("path"), str)
    }
    expected = (
        expected_state
        if expected_state is not None
        else cast(Mapping[str, Any], golden["baseline"])
    )
    tree: list[dict[str, Any]] = []
    for expected_node in cast(list[Mapping[str, Any]], expected["tree"]):
        path = cast(str, expected_node["path"])
        row = rows.get(path)
        require(isinstance(row, dict), f"private:{path}", "node is missing")
        transform = row.get("transform")
        tree.append(
            {
                "path": path,
                "type": row.get("type"),
                "owner": row.get("owner"),
                "index": row.get("index"),
                "transform": (
                    _normalize_semantic_numbers(transform) if transform else None
                ),
            }
        )
    properties: list[dict[str, Any]] = []
    for expected_property in cast(list[Mapping[str, Any]], expected["properties"]):
        path = cast(str, expected_property["path"])
        name = cast(str, expected_property["name"])
        if expected_property.get("redacted_sha256") is not None:
            properties.append(
                {
                    "path": path,
                    "name": name,
                    "type": expected_property["type"],
                    "value": None,
                    "redacted_sha256": hashes.get(f"{path}.{name}"),
                }
            )
        elif path == "Player" and name == "position":
            transform = rows[path].get("transform", {})
            local = transform.get("local", [])
            properties.append(
                {
                    "path": path,
                    "name": name,
                    "type": "vector2",
                    "value": (
                        _normalize_semantic_numbers(local[-2:])
                        if isinstance(local, list)
                        else None
                    ),
                    "redacted_sha256": None,
                }
            )
        else:
            properties.append(
                {
                    "path": path,
                    "name": name,
                    "type": expected_property["type"],
                    "value": _normalize_semantic_numbers(rows[path].get(name)),
                    "redacted_sha256": None,
                }
            )
    scripts = [
        {
            "path": expected_script["path"],
            "script": rows[expected_script["path"]].get("script"),
        }
        for expected_script in cast(list[Mapping[str, Any]], expected["scripts"])
    ]
    state = _sort_state(
        {
            "tree": tree,
            "properties": properties,
            "scripts": scripts,
            "connections": [
                _connection_from_private(item)
                for item in raw_connections
                if isinstance(item, dict)
            ],
            "selection": list(raw_selection),
        }
    )
    return state, cast(dict[str, Any], copy.deepcopy(native_probe))


def validate_cycle(
    operation_kind: str,
    states: Mapping[str, Mapping[str, Any]],
    transitions: Sequence[Mapping[str, int]],
    golden: Mapping[str, Any],
) -> None:
    operation = next(
        (
            record
            for record in cast(list[Mapping[str, Any]], golden["operations"])
            if record.get("kind") == operation_kind
        ),
        None,
    )
    require(operation is not None, operation_kind, "golden operation is missing")
    require(
        set(states) == {"pre", "prepared", "applied", "undone", "redone"},
        operation_kind,
        "observed states differ",
    )
    baseline = cast(Mapping[str, Any], golden["baseline"])
    applied = materialize_operation(baseline, cast(Mapping[str, Any], operation))
    require(
        canonical(states["pre"]) == canonical(baseline),
        operation_kind,
        "pre-state differs",
    )
    require(
        canonical(states["prepared"]) == canonical(states["pre"]),
        operation_kind,
        "prepare mutated editor state",
    )
    require(
        canonical(states["applied"]) == canonical(applied),
        operation_kind,
        "applied state differs",
    )
    require(
        canonical(states["undone"]) == canonical(states["pre"]),
        operation_kind,
        "Undo is not exact",
    )
    require(
        canonical(states["redone"]) == canonical(states["applied"]),
        operation_kind,
        "Redo differs from apply",
    )
    require(len(transitions) == 5, operation_kind, "transition vector differs")
    sequences = [record.get("operation_seq") for record in transitions]
    require(
        all(isinstance(value, int) and value >= 0 for value in sequences),
        operation_kind,
        "operation sequence is malformed",
    )
    require(
        sequences[1] == sequences[0]
        and sequences[2] == sequences[1] + 1
        and sequences[3] == sequences[2] + 1
        and sequences[4] == sequences[3] + 1,
        operation_kind,
        "revision deltas differ",
    )


def validate_fault_observation(
    observation: Mapping[str, Any],
    golden: Mapping[str, Any],
) -> None:
    point = observation.get("point")
    truth = next(
        (
            record
            for record in cast(list[Mapping[str, Any]], golden["faults"])
            if record.get("point") == point
        ),
        None,
    )
    require(truth is not None, "fault", "unknown point")
    for field in (
        "expected_state",
        "mutation",
        "native_actions",
        "replay_forbidden",
        "bridge_responsive",
    ):
        require(
            observation.get(field) == truth.get(field),
            f"fault:{point}",
            f"{field} differs",
        )


def _walk_keys(value: Any) -> Iterable[str]:
    if isinstance(value, dict):
        for key, child in value.items():
            yield str(key)
            yield from _walk_keys(child)
    elif isinstance(value, list):
        for child in value:
            yield from _walk_keys(child)


def scan_safe_surface(
    surface: str,
    value: Any,
    *,
    native_ids: Iterable[str | int] = (),
    approval_material: Iterable[str] = (),
    absolute_paths: Iterable[Path | str] = (),
    check_forbidden_keys: bool = True,
) -> None:
    encoded = canonical(value)
    text = encoded.decode("utf-8")
    lower_text = text.lower()
    allow_preview = surface == "bridge_prepare"
    if check_forbidden_keys:
        for key in _walk_keys(value):
            lowered = key.lower()
            if lowered in SAFE_CONTROL_METADATA_KEYS:
                continue
            for forbidden in FORBIDDEN_OUTPUT_FIELDS:
                if forbidden == "preview_payload_json" and allow_preview:
                    continue
                require(
                    forbidden not in lowered,
                    f"surface:{surface}",
                    f"forbidden output key class: {forbidden}",
                )
    for canary in CANARIES:
        require(canary not in text, f"surface:{surface}", "raw canary leaked")
    for secret in approval_material:
        require(
            bool(secret) and secret not in text,
            f"surface:{surface}",
            "approval material leaked",
        )
    for path in absolute_paths:
        path_text = os.fspath(path)
        require(
            not path_text or path_text not in text,
            f"surface:{surface}",
            "absolute path leaked",
        )
    for native_id in native_ids:
        native_text = str(native_id)
        leaked = native_text in text
        if isinstance(native_id, int):
            leaked = leaked or f"0x{native_id:x}" in lower_text
        require(
            bool(native_text) and not leaked,
            f"surface:{surface}",
            "native ID leaked",
        )


def validate_volatile_identity(
    first: Mapping[str, Any],
    replay: Mapping[str, Any],
    distinct: Mapping[str, Any],
) -> None:
    for key, pattern in SAFE_ID_PATTERNS.items():
        if key not in first:
            continue
        require(
            isinstance(first[key], str) and pattern.fullmatch(cast(str, first[key])),
            "identity",
            f"{key} format differs",
        )
    require(
        first.get("transaction_id") == replay.get("transaction_id"),
        "identity",
        "idempotency replay changed transaction ID",
    )
    require(
        first.get("transaction_id") != distinct.get("transaction_id"),
        "identity",
        "distinct request reused transaction ID",
    )


def validate_oracle_independence() -> None:
    path = Path(__file__)
    source = path.read_text(encoding="utf-8")
    parsed = ast.parse(source)
    forbidden_imports = {
        "bridge_client",
        "godot_codex_mcp",
        "mcp_server",
        "sprint9_apply_core_live",
        "sprint9_bindings_live",
        "sprint9_prepare_live",
        "sprint9_structural_live",
        "transactions",
    }
    imported: set[str] = set()
    for node in ast.walk(parsed):
        if isinstance(node, ast.Import):
            imported.update(alias.name.split(".")[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            imported.add(node.module.split(".")[0])
    require(
        not (imported & forbidden_imports),
        "oracle",
        "production or runner import detected",
    )
    forbidden_source_tokens = (
        "/".join(("modules", "codex_bridge")),
        "/".join(("godot-codex-mcp", "crates")),
        "_".join(("transaction", "preview", "builder")),
        "_".join(("transaction", "coordinator")) + ".cpp",
    )
    for token in forbidden_source_tokens:
        require(token not in source, "oracle", "production source dependency detected")


def run_schema_tests() -> None:
    cargo = shutil.which("cargo")
    require(cargo is not None, "schema", "cargo is unavailable")
    result = subprocess.run(
        [
            cast(str, cargo),
            "test",
            "--manifest-path",
            str(SCRIPT_ROOT / "Cargo.toml"),
            "--locked",
            "--offline",
            "--test",
            "transaction_fixture",
        ],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=180,
    )
    require(result.returncode == 0, "schema", "Draft 2020-12 validation failed")


def validate(run_schemas: bool = False) -> dict[str, Any]:
    manifest = strict_json(MANIFEST_PATH)
    golden = strict_json(GOLDEN_PATH)
    validate_manifest(manifest)
    validate_golden(golden)
    validate_oracle_independence()
    if run_schemas:
        run_schema_tests()
    return {
        "schema_version": "s9-transaction-oracle-validation/1.0",
        "status": "passed",
        "fixture_files": len(manifest["files"]),
        "operations": len(golden["operations"]),
        "fault_points": len(golden["faults"]),
        "stale_cases": len(golden["stale_cases"]),
        "baseline_nodes": len(golden["baseline"]["tree"]),
        "manifest_sha256": digest_file(MANIFEST_PATH),
        "golden_sha256": digest_file(GOLDEN_PATH),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--schemas",
        action="store_true",
        help="Also execute the offline Draft 2020-12 Rust schema gate.",
    )
    arguments = parser.parse_args()
    print(json.dumps(validate(run_schemas=arguments.schemas), indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
