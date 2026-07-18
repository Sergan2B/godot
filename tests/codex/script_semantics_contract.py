#!/usr/bin/env python3
"""Validate the independent Sprint 5 script-semantics contract vectors."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import shutil
import subprocess
import sys
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
CONTRACT_ROOT = SCRIPT_DIR / "fixtures" / "script_semantics_contract"
VECTORS_PATH = CONTRACT_ROOT / "contract-vectors.json"
SCHEMA_PATH = CONTRACT_ROOT / "contract-vectors.schema.json"


class ContractError(RuntimeError):
    """Raised when committed Sprint 5 contract truth is inconsistent."""


def strict_json_load(path: Path) -> Any:
    """Load UTF-8 JSON and reject duplicate members at every depth."""

    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise ContractError(f"duplicate JSON member in {path.name}: {key}")
            result[key] = value
        return result

    try:
        return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=reject_duplicates)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ContractError(f"cannot read strict JSON {path.name}: {error}") from error


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _digest(domain: str, parts: Sequence[str]) -> str:
    value = domain.encode("utf-8") + b"\0" + b"\0".join(part.encode("utf-8") for part in parts)
    return base64.urlsafe_b64encode(hashlib.sha256(value).digest()).decode("ascii").rstrip("=")


def _source_map(document: Mapping[str, Any]) -> dict[str, Mapping[str, Any]]:
    values = document.get("sources")
    if not isinstance(values, list):
        raise ContractError("sources must be an array")
    result: dict[str, Mapping[str, Any]] = {}
    for source in values:
        if not isinstance(source, dict) or not isinstance(source.get("name"), str):
            raise ContractError("source vector is malformed")
        name = source["name"]
        if name in result:
            raise ContractError(f"duplicate source vector: {name}")
        path = source.get("path")
        content = source.get("content")
        if (
            not isinstance(path, str)
            or not path.startswith("res://")
            or "\\" in path
            or "\0" in path
            or ".." in Path(path.removeprefix("res://")).parts
            or not isinstance(content, str)
        ):
            raise ContractError(f"unsafe source vector: {name}")
        if sha256_bytes(content.encode("utf-8")) != source.get("sha256"):
            raise ContractError(f"source hash mismatch: {name}")
        result[name] = source
    return result


def compute_identity(
    vector: Mapping[str, Any], algorithms: Mapping[str, str], sources: Mapping[str, Mapping[str, Any]]
) -> str:
    kind = vector["kind"]
    if kind == "named_symbol":
        domain = algorithms["named_symbol_domain"]
        prefix = "godot:script-symbol:named:v1:"
        parts = [vector["script_id"], vector["language"], vector["symbol_kind"], vector["qualified_key"]]
    elif kind == "content_symbol":
        source = sources[vector["source"]]
        domain = algorithms["content_symbol_domain"]
        prefix = "godot:script-symbol:content-revision:v1:"
        parts = [
            vector["script_id"],
            vector["language"],
            source["sha256"],
            vector["owner_qualified_key"],
            vector["symbol_kind"],
            str(vector["start_byte"]),
            str(vector["end_byte"]),
        ]
    elif kind == "diagnostic":
        source = sources[vector["source"]]
        domain = algorithms["diagnostic_domain"]
        prefix = "godot:script-diagnostic:v1:"
        parts = [
            vector["script_id"],
            vector["language"],
            source["sha256"],
            vector["severity"],
            vector["code"],
            str(vector["start_byte"]),
            str(vector["end_byte"]),
            vector["normalized_message"],
        ]
    else:
        raise ContractError(f"unsupported identity kind: {kind}")
    return prefix + _digest(domain, parts)


def validate_identity_vectors(document: Mapping[str, Any]) -> dict[str, str]:
    algorithms = document.get("algorithms")
    vectors = document.get("identity_vectors")
    if not isinstance(algorithms, dict) or not isinstance(vectors, list):
        raise ContractError("identity algorithms/vectors are malformed")
    sources = _source_map(document)
    computed: dict[str, str] = {}
    for vector in vectors:
        if not isinstance(vector, dict) or not isinstance(vector.get("name"), str):
            raise ContractError("identity vector is malformed")
        name = vector["name"]
        if name in computed:
            raise ContractError(f"duplicate identity vector: {name}")
        actual = compute_identity(vector, algorithms, sources)
        if actual != vector.get("expected_id"):
            raise ContractError(f"identity vector mismatch: {name}")
        computed[name] = actual
    for vector in vectors:
        name = vector["name"]
        same = vector.get("same_identity_as")
        different = vector.get("different_identity_from")
        if same is not None and (same not in computed or computed[name] != computed[same]):
            raise ContractError(f"same-identity relation failed: {name}")
        if different is not None and (different not in computed or computed[name] == computed[different]):
            raise ContractError(f"different-identity relation failed: {name}")
    return computed


def _occurrence_span(value: bytes, needle: bytes, occurrence: int) -> tuple[int, int]:
    if not needle:
        raise ContractError("range needle must not be empty")
    offset = 0
    found = -1
    for _ in range(occurrence + 1):
        found = value.find(needle, offset)
        if found < 0:
            raise ContractError("range occurrence does not exist")
        offset = found + 1
    return found, found + len(needle)


def _position(value: bytes, offset: int) -> tuple[int, int]:
    try:
        prefix = value[:offset].decode("utf-8")
    except UnicodeDecodeError as error:
        raise ContractError("range offset splits a UTF-8 scalar") from error
    return prefix.count("\n") + 1, len(prefix.rsplit("\n", 1)[-1]) + 1


def validate_range_vectors(document: Mapping[str, Any]) -> dict[str, tuple[int, int]]:
    values = document.get("range_vectors")
    if not isinstance(values, list):
        raise ContractError("range vectors must be an array")
    sources = _source_map(document)
    computed: dict[str, tuple[int, int]] = {}
    for vector in values:
        if not isinstance(vector, dict) or not isinstance(vector.get("name"), str):
            raise ContractError("range vector is malformed")
        name = vector["name"]
        if name in computed:
            raise ContractError(f"duplicate range vector: {name}")
        try:
            source = sources[vector["source"]]["content"].encode("utf-8")
            needle = vector["needle"].encode("utf-8")
            start, end = _occurrence_span(source, needle, vector["occurrence"])
        except (KeyError, AttributeError, TypeError) as error:
            raise ContractError(f"range vector is malformed: {name}") from error
        expected = (
            vector.get("start_byte"),
            vector.get("end_byte"),
            vector.get("start_line"),
            vector.get("start_column"),
            vector.get("end_line"),
            vector.get("end_column"),
        )
        actual = (start, end, *_position(source, start), *_position(source, end))
        if actual != expected:
            raise ContractError(f"range vector mismatch: {name}: {actual} != {expected}")
        computed[name] = (start, end)
    return computed


def classify_confidence(vector: Mapping[str, Any]) -> tuple[str, bool]:
    resolution = vector.get("resolution")
    authority = vector.get("authority")
    freshness = vector.get("freshness")
    if resolution == "resolved_unambiguous" and authority in {
        "gdscript_analyzer",
        "godot_resource_loader",
    }:
        confidence = "exact"
    elif resolution in {"dynamic_target", "string_node_path"} and authority == "gdscript_parser":
        confidence = "dynamic"
    else:
        raise ContractError(f"unsupported confidence classification: {vector.get('name')}")
    return confidence, freshness == "current"


def validate_confidence_vectors(document: Mapping[str, Any]) -> dict[str, str]:
    values = document.get("confidence_vectors")
    if not isinstance(values, list):
        raise ContractError("confidence vectors must be an array")
    computed: dict[str, str] = {}
    for vector in values:
        if not isinstance(vector, dict) or not isinstance(vector.get("name"), str):
            raise ContractError("confidence vector is malformed")
        name = vector["name"]
        if name in computed:
            raise ContractError(f"duplicate confidence vector: {name}")
        confidence, servable = classify_confidence(vector)
        if confidence != vector.get("expected_confidence") or servable != vector.get("servable_as_current"):
            raise ContractError(f"confidence vector mismatch: {name}")
        computed[name] = confidence
    return computed


def d07_source_audit() -> dict[str, Any]:
    protocol_path = REPOSITORY_ROOT / "modules/gdscript/language_server/gdscript_language_protocol.cpp"
    parser_path = REPOSITORY_ROOT / "modules/gdscript/language_server/gdscript_extend_parser.cpp"
    protocol = protocol_path.read_text(encoding="utf-8")
    parser = parser_path.read_text(encoding="utf-8")
    required_protocol = (
        "LSP_CLIENT_V(nullptr)",
        "managed_files.getptr(p_path)",
        "content = document->text",
        "stale_parsers.insert(p_path)",
        "remove_cached_parser(*stale_parsers.begin())",
    )
    required_parser = (
        "GDScriptParser::parse(p_code, p_path, false)",
        "GDScriptAnalyzer analyzer(this)",
        "parse_result = analyzer.analyze()",
        "update_diagnostics()",
        "update_symbols()",
    )
    missing = [value for value in required_protocol if value not in protocol]
    missing.extend(value for value in required_parser if value not in parser)
    if missing:
        raise ContractError(f"D-07 source invariant drifted: {missing}")
    return {
        "selected": "bridge_owned_exact_content_cache",
        "rejected": "active_lsp_peer_cache",
        "protocol_source_sha256": sha256_file(protocol_path),
        "projection_source_sha256": sha256_file(parser_path),
        "lsp_requires_client": True,
        "lsp_may_contain_dirty_text": True,
        "disk_only_lsp_parse_is_stale": True,
        "direct_projection_uses_godot_parser_analyzer": True,
    }


def run_draft_schema_tests() -> None:
    cargo = shutil.which("cargo")
    if cargo is None:
        raise ContractError("cargo is required for Draft 2020-12 validation")
    result = subprocess.run(
        [
            cargo,
            "test",
            "--manifest-path",
            str(SCRIPT_DIR / "Cargo.toml"),
            "--locked",
            "--offline",
            "script_semantics_contract",
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
        raise ContractError("Draft 2020-12 script contract tests failed:\n" + result.stdout + result.stderr)


def validate_all(run_schemas: bool = True) -> dict[str, Any]:
    document = strict_json_load(VECTORS_PATH)
    if not isinstance(document, dict):
        raise ContractError("contract vectors root must be an object")
    sources = _source_map(document)
    identities = validate_identity_vectors(document)
    ranges = validate_range_vectors(document)
    confidence = validate_confidence_vectors(document)
    audit = d07_source_audit()
    if run_schemas:
        run_draft_schema_tests()
    return {
        "schema_version": document.get("schema_version"),
        "vectors_sha256": sha256_file(VECTORS_PATH),
        "schema_sha256": sha256_file(SCHEMA_PATH),
        "sources": len(sources),
        "identity_vectors": len(identities),
        "range_vectors": len(ranges),
        "confidence_vectors": len(confidence),
        "d07": audit,
        "status": "passed",
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("validate", "d07-source-audit"))
    parser.add_argument("--skip-schemas", action="store_true")
    arguments = parser.parse_args(sys.argv[1:] if argv is None else argv)
    try:
        if arguments.command == "d07-source-audit":
            result = d07_source_audit()
        else:
            result = validate_all(run_schemas=not arguments.skip_schemas)
    except (ContractError, OSError, subprocess.SubprocessError) as error:
        print(f"script semantics contract validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
