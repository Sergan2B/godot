#!/usr/bin/env python3
"""Validate the independent Sprint 5 script-semantics fixture and golden graph."""

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

import script_semantics_contract as contract

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
PROJECT_ROOT = SCRIPT_DIR / "fixtures" / "script_semantics_project"
ORACLE_ROOT = SCRIPT_DIR / "fixtures" / "script_semantics_oracle"
MANIFEST_PATH = ORACLE_ROOT / "fixture-manifest.json"
GOLDEN_PATH = ORACLE_ROOT / "golden-script-graph.json"
SCHEMA_CASES = (
    ("fixture-manifest.schema.json", "fixture-manifest.json"),
    ("golden-script-graph.schema.json", "golden-script-graph.json"),
)
PHASES = (
    "base",
    "body_edit",
    "line_shift",
    "symbol_rename",
    "override_change",
    "literal_dependency_change",
    "attachment_change",
    "parse_error",
    "csharp_discovery",
    "journal_gap",
)
COVERAGE = {
    "named_class",
    "path_only_script",
    "inner_class",
    "typed_declaration",
    "untyped_declaration",
    "constant_enum_signal",
    "parameter_local_lambda",
    "inheritance_override",
    "exact_reference_call",
    "dynamic_call",
    "literal_path_preload",
    "literal_uid_load",
    "dynamic_load",
    "string_node_path",
    "shorthand_node_path",
    "unicode_source",
    "parse_error",
    "missing_dependency",
    "cyclic_dependency",
    "csharp_discovery_only",
    "scene_attachment_replace_remove",
}
CONTENT_SCOPED_KINDS = {"parameter", "local", "lambda"}
FORBIDDEN_MATERIAL = ("/Users/", "C:\\", ".godot/imported", ".godot/codex")


class FixtureError(RuntimeError):
    """Raised when committed S5-02 truth is inconsistent or incomplete."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise FixtureError(message)


def strict_json_load(path: Path) -> Any:
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


def _object_map(values: Any, label: str) -> dict[str, Mapping[str, Any]]:
    if not isinstance(values, list):
        raise FixtureError(f"{label} must be an array")
    result: dict[str, Mapping[str, Any]] = {}
    for value in values:
        if not isinstance(value, dict) or not isinstance(value.get("oracle_id"), str):
            raise FixtureError(f"malformed {label} record")
        oracle_id = value["oracle_id"]
        if oracle_id in result:
            raise FixtureError(f"duplicate {label} ID: {oracle_id}")
        result[oracle_id] = value
    return result


def _string_member(value: Mapping[str, Any], key: str, label: str) -> str:
    member = value.get(key)
    if not isinstance(member, str):
        raise FixtureError(f"{label} string member is missing: {key}")
    return member


def _integer_member(value: Mapping[str, Any], key: str, label: str) -> int:
    member = value.get(key)
    if not isinstance(member, int) or isinstance(member, bool):
        raise FixtureError(f"{label} integer member is missing: {key}")
    return member


def _array_member(value: Mapping[str, Any], key: str, label: str) -> list[Any]:
    member = value.get(key)
    if not isinstance(member, list):
        raise FixtureError(f"{label} array member is missing: {key}")
    return member


def load_manifest() -> Mapping[str, Any]:
    value = strict_json_load(MANIFEST_PATH)
    if not isinstance(value, dict):
        raise FixtureError("fixture manifest root must be an object")
    return value


def load_golden() -> Mapping[str, Any]:
    value = strict_json_load(GOLDEN_PATH)
    if not isinstance(value, dict):
        raise FixtureError("golden graph root must be an object")
    return value


def validate_manifest(manifest: Mapping[str, Any]) -> dict[str, Mapping[str, Any]]:
    values = manifest.get("files")
    if not isinstance(values, list):
        raise FixtureError("fixture file manifest must be an array")
    files: dict[str, Mapping[str, Any]] = {}
    order: list[str] = []
    for value in values:
        if not isinstance(value, dict):
            raise FixtureError("malformed fixture file record")
        path = value.get("path")
        digest = value.get("sha256")
        if not isinstance(path, str) or not isinstance(digest, str):
            raise FixtureError("malformed fixture file coordinates")
        relative = Path(path)
        require(not relative.is_absolute() and ".." not in relative.parts and "\\" not in path, "unsafe fixture path")
        require(path not in files, f"duplicate fixture path: {path}")
        actual_path = PROJECT_ROOT / relative
        require(actual_path.is_file(), f"fixture file is missing: {path}")
        require(sha256_file(actual_path) == digest, f"fixture file hash differs: {path}")
        files[path] = value
        order.append(path)
    actual = sorted(
        path.relative_to(PROJECT_ROOT).as_posix()
        for path in PROJECT_ROOT.rglob("*")
        if path.is_file() and ".godot" not in path.parts
    )
    require(order == sorted(order), "fixture file manifest must be sorted")
    require(order == actual, "fixture file manifest does not close over the project tree")
    phases = manifest.get("phases")
    if not isinstance(phases, list):
        raise FixtureError("fixture phases must be an array")
    require(tuple(phase.get("name") for phase in phases if isinstance(phase, dict)) == PHASES, "fixture phases differ")
    require(
        len({phase.get("mutation") for phase in phases if isinstance(phase, dict)}) == len(PHASES),
        "fixture mutations must be unique",
    )
    return files


def _nth_span(value: bytes, needle: bytes, occurrence: int) -> tuple[int, int]:
    require(bool(needle), "range needle must not be empty")
    offset = 0
    found = -1
    for _ in range(occurrence + 1):
        found = value.find(needle, offset)
        if found < 0:
            raise FixtureError("range occurrence is missing")
        offset = found + 1
    return found, found + len(needle)


def _position(value: bytes, offset: int) -> tuple[int, int]:
    try:
        prefix = value[:offset].decode("utf-8")
    except UnicodeDecodeError as error:
        raise FixtureError("range splits a UTF-8 scalar") from error
    return prefix.count("\n") + 1, len(prefix.rsplit("\n", 1)[-1]) + 1


def validate_documents(
    golden: Mapping[str, Any], files: Mapping[str, Mapping[str, Any]]
) -> dict[str, Mapping[str, Any]]:
    documents = _object_map(golden.get("documents"), "document")
    diagnostics = _object_map(golden.get("diagnostics"), "diagnostic")
    expected_states = {
        "base": "complete",
        "player": "complete",
        "path_only": "complete",
        "broken": "invalid",
        "missing_base": "partial",
        "cycle_a": "partial",
        "cycle_b": "partial",
        "enemy_cs": "unavailable",
    }
    require(
        {key: value.get("semantic_state") for key, value in documents.items()} == expected_states,
        "document state matrix differs",
    )
    for oracle_id, document in documents.items():
        path = document.get("path")
        language = document.get("language")
        if not isinstance(path, str) or not path.startswith("res://"):
            raise FixtureError(f"unsafe document path: {oracle_id}")
        relative = path.removeprefix("res://")
        require(relative in files, f"document is absent from manifest: {oracle_id}")
        require(
            "sha256:" + files[relative]["sha256"] == document.get("content_sha256"),
            f"document hash differs: {oracle_id}",
        )
        require(
            (language == "gdscript" and path.endswith(".gd")) or (language == "csharp" and path.endswith(".cs")),
            f"document language differs: {oracle_id}",
        )
        uid_path = PROJECT_ROOT / (relative + ".uid")
        require(uid_path.is_file(), f"document UID sidecar is missing: {oracle_id}")
        uid = uid_path.read_text(encoding="utf-8").strip()
        require(
            document.get("script_id") == "godot:resource:uid:v1:" + uid.removeprefix("uid://"),
            f"document script ID differs: {oracle_id}",
        )
        listed_diagnostics = _array_member(document, "diagnostics", f"document {oracle_id}")
        for diagnostic_id in listed_diagnostics:
            require(
                diagnostic_id in diagnostics and diagnostics[diagnostic_id].get("document") == oracle_id,
                f"diagnostic closure differs: {oracle_id}",
            )
    require(
        documents["enemy_cs"].get("adapter_profile") == "csharp_discovery_only_v1",
        "C# profile must remain discovery-only",
    )
    require(documents["enemy_cs"].get("class_symbol") is None, "discovery-only C# must not claim a class symbol")
    return documents


def validate_resources(
    golden: Mapping[str, Any], files: Mapping[str, Mapping[str, Any]]
) -> dict[str, Mapping[str, Any]]:
    resources = _object_map(golden.get("resources"), "resource")
    for oracle_id, resource in resources.items():
        path = resource.get("path")
        if not isinstance(path, str) or not path.startswith("res://"):
            raise FixtureError(f"unsafe resource path: {oracle_id}")
        relative = path.removeprefix("res://")
        require(relative in files, f"resource is absent from manifest: {oracle_id}")
        require(resource.get("sha256") == "sha256:" + files[relative]["sha256"], f"resource hash differs: {oracle_id}")
        text = (PROJECT_ROOT / relative).read_text(encoding="utf-8")
        require(f'uid="{resource.get("uid")}"' in text, f"resource UID differs: {oracle_id}")
    return resources


def validate_ranges(
    golden: Mapping[str, Any], documents: Mapping[str, Mapping[str, Any]]
) -> dict[str, Mapping[str, Any]]:
    ranges = _object_map(golden.get("ranges"), "range")
    source_cache: dict[str, bytes] = {}
    for oracle_id, value in ranges.items():
        document_id = _string_member(value, "document", f"range {oracle_id}")
        require(document_id in documents, f"range document is missing: {oracle_id}")
        document = documents[document_id]
        path = document["path"].removeprefix("res://")
        source = source_cache.setdefault(document_id, (PROJECT_ROOT / path).read_bytes())
        needle = _string_member(value, "needle", f"range {oracle_id}")
        occurrence = _integer_member(value, "occurrence", f"range {oracle_id}")
        start, end = _nth_span(source, needle.encode("utf-8"), occurrence)
        expected = (value.get("bytes"), value.get("start"), value.get("end"))
        actual = ([start, end], list(_position(source, start)), list(_position(source, end)))
        require(actual == expected, f"content-bound range differs: {oracle_id}: {actual} != {expected}")
    return ranges


def _owner_coordinate(
    symbol: Mapping[str, Any], symbols: Mapping[str, Mapping[str, Any]], ranges: Mapping[str, Mapping[str, Any]]
) -> str:
    qualified = symbol.get("qualified_key")
    if isinstance(qualified, str):
        return qualified
    owner_id = _string_member(symbol, "owner", f"content-scoped symbol {symbol.get('oracle_id')}")
    require(owner_id in symbols, f"content-scoped symbol owner is missing: {symbol.get('oracle_id')}")
    owner = symbols[owner_id]
    base = _owner_coordinate(owner, symbols, ranges)
    if owner.get("kind") == "lambda":
        owner_range = ranges[owner["range"]]
        return base + "/lambda@" + str(owner_range["bytes"][0])
    return base


def validate_symbols(
    golden: Mapping[str, Any],
    documents: Mapping[str, Mapping[str, Any]],
    ranges: Mapping[str, Mapping[str, Any]],
) -> tuple[dict[str, Mapping[str, Any]], dict[str, str]]:
    symbols = _object_map(golden.get("symbols"), "symbol")
    contract_document = contract.strict_json_load(contract.VECTORS_PATH)
    if not isinstance(contract_document, dict):
        raise FixtureError("S5-01 contract root is malformed")
    algorithms = contract_document["algorithms"]
    source_map = {oracle_id: {"sha256": document["content_sha256"]} for oracle_id, document in documents.items()}
    identities: dict[str, str] = {}
    for oracle_id, symbol in symbols.items():
        document_id = _string_member(symbol, "document", f"symbol {oracle_id}")
        range_id = _string_member(symbol, "range", f"symbol {oracle_id}")
        require(document_id in documents and range_id in ranges, f"symbol closure differs: {oracle_id}")
        require(ranges[range_id].get("document") == document_id, f"symbol range document differs: {oracle_id}")
        require(documents[document_id].get("language") == "gdscript", f"non-GDScript symbol is claimed: {oracle_id}")
        owner_id = symbol.get("owner")
        if owner_id is not None:
            require(
                owner_id in symbols and symbols[owner_id].get("document") == document_id,
                f"symbol owner differs: {oracle_id}",
            )
        content_scoped = symbol.get("kind") in CONTENT_SCOPED_KINDS
        require(
            (symbol.get("identity_scope") == "content_revision") == content_scoped,
            f"symbol identity scope differs: {oracle_id}",
        )
        qualified = symbol.get("qualified_key")
        require((qualified is None) == content_scoped, f"symbol qualified key differs: {oracle_id}")
        needle = ranges[range_id]["needle"]
        require(
            symbol.get("kind") == "lambda" or str(symbol.get("name")) in needle,
            f"symbol range does not name declaration: {oracle_id}",
        )
        document = documents[document_id]
        if content_scoped:
            byte_range = ranges[range_id]["bytes"]
            vector = {
                "kind": "content_symbol",
                "script_id": document["script_id"],
                "language": document["language"],
                "source": document_id,
                "owner_qualified_key": _owner_coordinate(symbol, symbols, ranges),
                "symbol_kind": symbol["kind"],
                "start_byte": byte_range[0],
                "end_byte": byte_range[1],
            }
        else:
            vector = {
                "kind": "named_symbol",
                "script_id": document["script_id"],
                "language": document["language"],
                "symbol_kind": symbol["kind"],
                "qualified_key": qualified,
            }
        identities[oracle_id] = contract.compute_identity(vector, algorithms, source_map)
    require(len(set(identities.values())) == len(identities), "canonical symbol identities collide")
    return symbols, identities


def validate_relations(
    golden: Mapping[str, Any],
    symbols: Mapping[str, Mapping[str, Any]],
    resources: Mapping[str, Mapping[str, Any]],
    ranges: Mapping[str, Mapping[str, Any]],
) -> dict[str, Mapping[str, Any]]:
    relations = _object_map(golden.get("relations"), "relation")
    exact = 0
    dynamic = 0
    for oracle_id, relation in relations.items():
        source_id = _string_member(relation, "source_symbol", f"relation {oracle_id}")
        range_id = _string_member(relation, "evidence_range", f"relation {oracle_id}")
        require(source_id in symbols and range_id in ranges, f"relation closure differs: {oracle_id}")
        require(
            ranges[range_id].get("document") == symbols[source_id].get("document"),
            f"relation evidence document differs: {oracle_id}",
        )
        target_symbol = relation.get("target_symbol")
        target_resource = relation.get("target_resource")
        if relation.get("confidence") == "exact":
            exact += 1
            require(relation.get("resolvable_truth") is True, f"exact relation lacks resolvable truth: {oracle_id}")
            require(
                (target_symbol is not None) != (target_resource is not None),
                f"exact relation target differs: {oracle_id}",
            )
            require(target_symbol is None or target_symbol in symbols, f"exact symbol target is missing: {oracle_id}")
            require(
                target_resource is None or target_resource in resources,
                f"exact resource target is missing: {oracle_id}",
            )
        else:
            dynamic += 1
            require(relation.get("confidence") == "dynamic", f"unsupported confidence: {oracle_id}")
            require(relation.get("resolvable_truth") is False, f"dynamic relation is marked resolvable: {oracle_id}")
            require(
                target_symbol is None and target_resource is None,
                f"dynamic relation claims an exact target: {oracle_id}",
            )
    truth = golden.get("accuracy_truth")
    require(
        isinstance(truth, dict)
        and truth.get("analyzer_resolvable_relations") == exact
        and truth.get("dynamic_relations") == dynamic
        and truth.get("false_exact_allowed") == 0,
        "accuracy truth counts differ",
    )
    return relations


def validate_diagnostics_and_attachments(
    golden: Mapping[str, Any],
    documents: Mapping[str, Mapping[str, Any]],
    symbols: Mapping[str, Mapping[str, Any]],
    ranges: Mapping[str, Mapping[str, Any]],
) -> None:
    diagnostics = _object_map(golden.get("diagnostics"), "diagnostic")
    listed = {item for document in documents.values() for item in document["diagnostics"]}
    require(listed == set(diagnostics), "document diagnostic references do not close")
    for oracle_id, diagnostic in diagnostics.items():
        document_id = _string_member(diagnostic, "document", f"diagnostic {oracle_id}")
        range_id = _string_member(diagnostic, "evidence_range", f"diagnostic {oracle_id}")
        require(document_id in documents and range_id in ranges, f"diagnostic closure differs: {oracle_id}")
        require(ranges[range_id].get("document") == document_id, f"diagnostic evidence document differs: {oracle_id}")

    attachments = _object_map(golden.get("attachments"), "attachment")
    require(
        set(attachments) == {"attachment_player", "attachment_replaced", "attachment_removed"},
        "attachment state matrix differs",
    )
    for oracle_id, attachment in attachments.items():
        scene = _string_member(attachment, "scene", f"attachment {oracle_id}")
        require(scene.startswith("res://"), f"attachment scene path differs: {oracle_id}")
        text = (PROJECT_ROOT / scene.removeprefix("res://")).read_text(encoding="utf-8")
        require(f'[node name="{attachment.get("node_path")}"' in text, f"attachment node is missing: {oracle_id}")
        attachment_document_id = attachment.get("script_document")
        class_symbol = attachment.get("class_symbol")
        if attachment.get("state") == "removed":
            require(
                attachment_document_id is None and class_symbol is None and "script =" not in text,
                "removed attachment still claims a script",
            )
        else:
            if not isinstance(attachment_document_id, str) or attachment_document_id not in documents:
                raise FixtureError(f"attachment script is missing: {oracle_id}")
            require(
                documents[attachment_document_id]["path"] in text,
                f"attachment scene text differs: {oracle_id}",
            )
            if class_symbol is not None:
                require(
                    class_symbol in symbols and symbols[class_symbol]["document"] == attachment_document_id,
                    f"attachment class differs: {oracle_id}",
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
            "script_semantics_fixture",
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
        raise FixtureError("Draft 2020-12 script fixture tests failed:\n" + result.stdout + result.stderr)


def validate_all(run_schemas: bool = True) -> dict[str, Any]:
    manifest = load_manifest()
    golden = load_golden()
    files = validate_manifest(manifest)
    require(set(golden.get("coverage", [])) == COVERAGE, "fixture coverage matrix differs")
    documents = validate_documents(golden, files)
    resources = validate_resources(golden, files)
    ranges = validate_ranges(golden, documents)
    symbols, identities = validate_symbols(golden, documents, ranges)
    relations = validate_relations(golden, symbols, resources, ranges)
    validate_diagnostics_and_attachments(golden, documents, symbols, ranges)
    player_source = (PROJECT_ROOT / "scripts" / "player.gd").read_text(encoding="utf-8")
    require("Café 🚀" in player_source, "Unicode fixture truth differs")
    serialized = json.dumps({"manifest": manifest, "golden": golden}, ensure_ascii=False)
    require(not any(value in serialized for value in FORBIDDEN_MATERIAL), "fixture truth leaks forbidden path material")
    if run_schemas:
        run_draft_schema_tests()
    identity_digest = hashlib.sha256()
    for oracle_id, identity in sorted(identities.items()):
        identity_digest.update(oracle_id.encode("utf-8") + b"\0" + identity.encode("utf-8") + b"\0")
    return {
        "schema_version": 1,
        "fixture_version": manifest["fixture_version"],
        "manifest_sha256": sha256_file(MANIFEST_PATH),
        "golden_sha256": sha256_file(GOLDEN_PATH),
        "canonical_symbol_ids_sha256": identity_digest.hexdigest(),
        "files": len(files),
        "phases": len(manifest["phases"]),
        "coverage_cases": len(COVERAGE),
        "documents": len(documents),
        "resources": len(resources),
        "ranges": len(ranges),
        "symbols": len(symbols),
        "relations": len(relations),
        "diagnostics": len(golden["diagnostics"]),
        "attachments": len(golden["attachments"]),
        "status": "passed",
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("validate",))
    parser.add_argument("--skip-schemas", action="store_true")
    arguments = parser.parse_args(sys.argv[1:] if argv is None else argv)
    try:
        summary = validate_all(run_schemas=not arguments.skip_schemas)
    except (FixtureError, contract.ContractError, OSError, subprocess.SubprocessError) as error:
        print(f"script semantics fixture validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
