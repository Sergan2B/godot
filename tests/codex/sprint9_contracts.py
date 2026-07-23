#!/usr/bin/env python3
"""Validate the independent S9 approval boundary and current WRITE-001 surface."""

from __future__ import annotations

import argparse
import base64
import copy
import hashlib
import hmac
import json
import re
import shutil
import struct
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
CONTRACT_ROOT = SCRIPT_DIR / "fixtures" / "approval_boundary_contract"
VECTORS_PATH = CONTRACT_ROOT / "receipt-vectors.json"
SCHEMA_PATH = CONTRACT_ROOT / "receipt-vectors.schema.json"
WRITE_PATH = REPOSITORY_ROOT / "docs/codex-integration/WRITE-001-editor-transactions-and-undo.md"
PROTOCOL_PATH = REPOSITORY_ROOT / "docs/codex-integration/PROTOCOL-001-bridge-rpc-v1.md"
MCP_PATH = REPOSITORY_ROOT / "docs/codex-integration/MCP-001-project-scoped-read-tools.md"
PLAN_PATH = REPOSITORY_ROOT / "docs/codex-integration/SPRINT-9-PLAN.md"
SERVER_PATH = REPOSITORY_ROOT / "godot-codex-mcp/crates/mcp-server/src/lib.rs"
WORKSPACE_MANIFEST = REPOSITORY_ROOT / "godot-codex-mcp/Cargo.toml"
PROBE_SUPPORT = REPOSITORY_ROOT / "godot-codex-mcp/crates/mcp-server/tests/support/mod.rs"
HOST_PROBE = SCRIPT_DIR / "sprint9_approval_host_probe.py"

KEY_DOMAIN = b"godot-codex/approval-key/v1"
RECEIPT_DOMAIN = b"godot-codex/approval-receipt/v1\0"
SAFE_INTEGER_MAX = 9_007_199_254_740_991
RECEIPT_TTL_MS = 30_000
CLOCK_SKEW_MS = 2_000

RECEIPT_KEYS = {"kind", "scope", "nonce", "issued_at_ms", "expires_at_ms", "mac"}
COORDINATE_KEYS = {
    "project_id",
    "editor_session_id",
    "scene_id",
    "transaction_id",
    "preview_digest",
    "scope",
    "risk",
    "scene_revision",
    "operation_seq",
}
RESERVED_WRITE_TOOLS = {
    "godot_prepare_create_node",
    "godot_prepare_delete_node",
    "godot_prepare_reparent_node",
    "godot_prepare_set_property",
    "godot_prepare_attach_script",
    "godot_prepare_detach_script",
    "godot_prepare_connect_signal",
    "godot_prepare_disconnect_signal",
    "godot_apply_transaction",
    "godot_get_transaction_status",
    "godot_undo_transaction",
}


class ContractError(RuntimeError):
    """Raised when committed S9-01 contract truth is inconsistent."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractError(message)


def strict_json_load(path: Path) -> Any:
    """Load strict UTF-8 JSON and reject duplicate members at every depth."""

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


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _mapping(value: Any, name: str) -> Mapping[str, Any]:
    require(isinstance(value, dict), f"{name} must be an object")
    return value


def _string(value: Any, name: str) -> str:
    require(isinstance(value, str), f"{name} must be a string")
    return value


def _integer(value: Any, name: str) -> int:
    require(isinstance(value, int) and not isinstance(value, bool), f"{name} must be an integer")
    require(0 <= value <= SAFE_INTEGER_MAX, f"{name} is outside the protocol-safe range")
    return value


def _base64url_32(value: Any, name: str) -> bytes:
    text = _string(value, name)
    require(re.fullmatch(r"[A-Za-z0-9_-]{43}", text) is not None, f"{name} is not canonical base64url")
    try:
        decoded = base64.b64decode(text + "=", altchars=b"-_", validate=True)
    except ValueError as error:
        raise ContractError(f"{name} is invalid base64url") from error
    require(len(decoded) == 32, f"{name} must decode to 32 bytes")
    return decoded


def _lp(value: str) -> bytes:
    encoded = value.encode("utf-8")
    require(len(encoded) <= 0xFFFFFFFF, "receipt string exceeds U32BE")
    return struct.pack(">I", len(encoded)) + encoded


def canonical_receipt_input(vector: Mapping[str, Any]) -> bytes:
    coordinates = _mapping(vector.get("coordinates"), "coordinates")
    receipt = _mapping(vector.get("receipt"), "receipt")
    output = bytearray(RECEIPT_DOMAIN)
    for field in (
        "project_id",
        "editor_session_id",
        "scene_id",
        "transaction_id",
        "preview_digest",
        "scope",
        "risk",
    ):
        output.extend(_lp(_string(coordinates.get(field), f"coordinates.{field}")))
    for field in ("scene_revision", "operation_seq"):
        output.extend(struct.pack(">Q", _integer(coordinates.get(field), f"coordinates.{field}")))
    for field in ("issued_at_ms", "expires_at_ms"):
        output.extend(struct.pack(">Q", _integer(receipt.get(field), f"receipt.{field}")))
    output.extend(_base64url_32(receipt.get("nonce"), "receipt.nonce"))
    return bytes(output)


def derive_approval_key(session_token_hex: Any) -> bytes:
    token_text = _string(session_token_hex, "session_token_hex")
    require(re.fullmatch(r"[0-9a-f]{64}", token_text) is not None, "session token vector must be 32-byte lowercase hex")
    return hmac.digest(bytes.fromhex(token_text), KEY_DOMAIN, "sha256")


def compute_receipt_mac(vector: Mapping[str, Any]) -> str:
    key = derive_approval_key(vector.get("session_token_hex"))
    digest = hmac.digest(key, canonical_receipt_input(vector), "sha256")
    return base64.urlsafe_b64encode(digest).decode("ascii").rstrip("=")


def validate_positive(vector: Mapping[str, Any]) -> dict[str, Any]:
    coordinates = _mapping(vector.get("coordinates"), "positive.coordinates")
    receipt = _mapping(vector.get("receipt"), "positive.receipt")
    require(set(coordinates) == COORDINATE_KEYS, "positive coordinate fields differ")
    require(set(receipt) == RECEIPT_KEYS, "positive receipt fields differ")
    require(receipt.get("kind") == "mcp_form_v1", "receipt kind differs")
    require(receipt.get("scope") == coordinates.get("scope"), "receipt scope is not bound to the operation")
    issued = _integer(receipt.get("issued_at_ms"), "receipt.issued_at_ms")
    expires = _integer(receipt.get("expires_at_ms"), "receipt.expires_at_ms")
    require(expires - issued == RECEIPT_TTL_MS, "receipt lifetime differs from 30 seconds")
    canonical = canonical_receipt_input(vector)
    canonical_hash = "sha256:" + hashlib.sha256(canonical).hexdigest()
    key_hex = derive_approval_key(vector.get("session_token_hex")).hex()
    mac = compute_receipt_mac(vector)
    require(key_hex == vector.get("approval_key_hex"), "approval key vector mismatch")
    require(len(canonical) == vector.get("canonical_length"), "canonical receipt length mismatch")
    require(canonical_hash == vector.get("canonical_sha256"), "canonical receipt hash mismatch")
    require(hmac.compare_digest(mac, _string(receipt.get("mac"), "receipt.mac")), "receipt MAC vector mismatch")
    verify_receipt(vector, coordinates, _integer(vector.get("current_ms"), "current_ms"), set())
    return {
        "approval_key_hex": key_hex,
        "canonical_length": len(canonical),
        "canonical_sha256": canonical_hash,
        "mac": mac,
    }


def verify_receipt(
    vector: Mapping[str, Any],
    expected: Mapping[str, Any],
    current_ms: int,
    used_nonces: set[str],
) -> None:
    """Independently apply WRITE-001 receipt guards or raise the stable code."""

    coordinates = _mapping(vector.get("coordinates"), "coordinates")
    receipt = _mapping(vector.get("receipt"), "receipt")
    if set(receipt) != RECEIPT_KEYS or set(coordinates) != COORDINATE_KEYS:
        raise ContractError("approval_invalid")
    try:
        nonce = _string(receipt.get("nonce"), "receipt.nonce")
        _base64url_32(nonce, "receipt.nonce")
        received_mac = _string(receipt.get("mac"), "receipt.mac")
        _base64url_32(received_mac, "receipt.mac")
        computed_mac = compute_receipt_mac(vector)
    except ContractError as error:
        raise ContractError("approval_invalid") from error
    if not hmac.compare_digest(computed_mac, received_mac):
        raise ContractError("approval_invalid")
    if receipt.get("kind") != "mcp_form_v1" or receipt.get("scope") != coordinates.get("scope"):
        raise ContractError("approval_invalid")
    if any(coordinates.get(field) != expected.get(field) for field in ("project_id", "editor_session_id", "scene_id", "transaction_id", "risk", "operation_seq")):
        raise ContractError("approval_invalid")
    if coordinates.get("preview_digest") != expected.get("preview_digest"):
        raise ContractError("preview_mismatch")
    if coordinates.get("scope") != expected.get("scope"):
        raise ContractError("approval_scope_mismatch")
    if coordinates.get("scene_revision") != expected.get("scene_revision"):
        raise ContractError("stale_scene_revision")
    issued = _integer(receipt.get("issued_at_ms"), "receipt.issued_at_ms")
    expires = _integer(receipt.get("expires_at_ms"), "receipt.expires_at_ms")
    if expires < issued or expires - issued > RECEIPT_TTL_MS:
        raise ContractError("approval_invalid")
    if current_ms + CLOCK_SKEW_MS < issued or current_ms > expires + CLOCK_SKEW_MS:
        raise ContractError("approval_invalid")
    if nonce in used_nonces:
        raise ContractError("approval_replayed")


def validate_negative_cases(document: Mapping[str, Any]) -> dict[str, str]:
    positive = _mapping(document.get("positive"), "positive")
    cases = document.get("negative_cases")
    require(isinstance(cases, list) and len(cases) == 10, "exactly ten named receipt negatives are required")
    outcomes: dict[str, str] = {}
    for case_value in cases:
        case = _mapping(case_value, "negative case")
        case_id = _string(case.get("id"), "negative case id")
        require(case_id not in outcomes, f"duplicate negative case: {case_id}")
        mutation = _mapping(case.get("mutation"), f"{case_id}.mutation")
        require(len(mutation) == 1, f"{case_id} must mutate exactly one field")
        candidate = copy.deepcopy(positive)
        expected = copy.deepcopy(_mapping(positive.get("coordinates"), "positive.coordinates"))
        current_ms = _integer(positive.get("current_ms"), "positive.current_ms")
        used_nonces: set[str] = set()
        field, value = next(iter(mutation.items()))
        if field == "mac":
            candidate["receipt"]["mac"] = value
        elif field == "current_ms":
            current_ms = _integer(value, f"{case_id}.current_ms")
        elif field == "used_nonce":
            require(value is True, f"{case_id}.used_nonce must be true")
            used_nonces.add(_string(candidate["receipt"]["nonce"], "receipt.nonce"))
        else:
            require(field in COORDINATE_KEYS, f"unsupported negative mutation: {field}")
            expected[field] = value
        try:
            verify_receipt(candidate, expected, current_ms, used_nonces)
        except ContractError as error:
            actual = str(error)
        else:
            raise ContractError(f"negative case unexpectedly passed: {case_id}")
        required_error = _string(case.get("expected_error"), f"{case_id}.expected_error")
        require(actual == required_error, f"{case_id} returned {actual}, expected {required_error}")
        outcomes[case_id] = actual
    return outcomes


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
            "approval_boundary_contract",
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
        raise ContractError("Draft 2020-12 approval tests failed:\n" + result.stdout + result.stderr)


def audit_documents_and_registry() -> dict[str, Any]:
    documents = {
        "write": WRITE_PATH.read_text(encoding="utf-8"),
        "protocol": PROTOCOL_PATH.read_text(encoding="utf-8"),
        "mcp": MCP_PATH.read_text(encoding="utf-8"),
        "plan": PLAN_PATH.read_text(encoding="utf-8"),
    }
    for phrase in (
        "preparing → previewed → awaiting_approval → applying",
        "EditorUndoRedoManager::commit_action()",
        "approval_host_unsupported",
        "approval_replayed",
        "transaction_in_doubt",
        "Prepared transactions per project | 64",
        "Receipt lifetime after acceptance | 30 seconds",
        "S9-01 does not register these tools",
        "form-compatible negotiated",
    ):
        require(phrase in documents["write"], f"WRITE-001 traceability phrase missing: {phrase}")
    for phrase in (
        "transaction.scene_v1",
        "transaction.prepare",
        "transaction.apply",
        "transaction.status",
        "transaction.undo",
        "Sessions negotiated at 1.0–1.6 omit every 1.7-only capability",
    ):
        require(phrase in documents["protocol"], f"PROTOCOL-001 traceability phrase missing: {phrase}")
    for tool in RESERVED_WRITE_TOOLS:
        require(f"`{tool}`" in documents["mcp"], f"MCP-001 reserved tool missing: {tool}")
    require("production registry exactly 36 tools" in documents["write"], "WRITE-001 registry gate missing")
    require("`WRITE-001` is frozen by `S9-01`" in documents["plan"], "Sprint plan does not bind S9-01")
    require("forward reference" not in documents["plan"].lower(), "Sprint plan still calls WRITE-001 a forward reference")

    server = SERVER_PATH.read_text(encoding="utf-8")
    tool_count = len(re.findall(r"^    #\[tool\(", server, flags=re.MULTILINE))
    require(tool_count == 36, f"production MCP registry source has {tool_count} tools, expected 36")
    present = sorted(tool for tool in RESERVED_WRITE_TOOLS if tool in server)
    require(
        present == sorted(RESERVED_WRITE_TOOLS),
        f"S9 write-tool registry differs: {present}",
    )
    manifest = WORKSPACE_MANIFEST.read_text(encoding="utf-8")
    require(
        '"elicitation"' in manifest and 'rmcp = { version = "=2.2.0"' in manifest,
        "pinned rmcp elicitation dependency differs",
    )
    probe = PROBE_SUPPORT.read_text(encoding="utf-8")
    require("exact_confirmation" in probe, "approval probe does not reject additional content")
    require(HOST_PROBE.is_file(), "macOS approval host probe runner is missing")
    return {"documents": len(documents), "production_tools": tool_count, "reserved_tools": len(RESERVED_WRITE_TOOLS)}


def validate_all(run_schemas: bool = True) -> dict[str, Any]:
    document = strict_json_load(VECTORS_PATH)
    require(isinstance(document, dict), "approval vectors root must be an object")
    canonicalization = _mapping(document.get("canonicalization"), "canonicalization")
    require(canonicalization.get("negative_mutation_target") == "validation_context_except_mac_and_used_nonce", "negative mutation semantics differ")
    positive = validate_positive(_mapping(document.get("positive"), "positive"))
    negatives = validate_negative_cases(document)
    audit = audit_documents_and_registry()
    if run_schemas:
        run_draft_schema_tests()
    return {
        "schema_version": document.get("schema_version"),
        "vectors_sha256": sha256_file(VECTORS_PATH),
        "schema_sha256": sha256_file(SCHEMA_PATH),
        "canonical_sha256": positive["canonical_sha256"],
        "negative_cases": len(negatives),
        **audit,
        "status": "passed",
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("validate",))
    parser.add_argument("--skip-schemas", action="store_true")
    arguments = parser.parse_args(sys.argv[1:] if argv is None else argv)
    try:
        result = validate_all(run_schemas=not arguments.skip_schemas)
    except (ContractError, OSError, subprocess.SubprocessError) as error:
        print(f"Sprint 9 contract validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
