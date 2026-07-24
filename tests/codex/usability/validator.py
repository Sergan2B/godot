#!/usr/bin/env python3
"""Fail-closed validator for the Sprint 11 human-usability acquisition kit.

Only content-minimized primary records are accepted.  Templates and synthetic
contract fixtures are structurally valid but can never qualify.  Real-human
records require an explicit acquisition-mode opt-in and remain subject to the
aggregate Sprint 11 provenance validator.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence, cast

REPOSITORY = Path(__file__).resolve().parents[3]
MAX_JSON_BYTES = 2 * 1024 * 1024
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
RUN_ID = re.compile(r"^s11u-run:[0-9a-f]{32}$")
SOURCE_RUN_ID = re.compile(r"^s11u-source:[0-9a-f]{32}$")
PACKAGE_RUN_ID = re.compile(r"^s11u-package:[0-9a-f]{32}$")
PARTICIPANT_ID = re.compile(r"^participant:sha256:[0-9a-f]{64}$")
OPERATOR_ID = re.compile(r"^operator:sha256:[0-9a-f]{64}$")
EVENT_ID = re.compile(r"^event:[0-9]{4}$")
DEFECT_ID = re.compile(r"^S11-UX-[0-9]{3}$")
SAFE_PATH = re.compile(r"^(?!/)(?!.*(?:^|/)\.\.?/)[A-Za-z0-9._+/-]+$")
SAFE_ID = re.compile(r"^[A-Za-z0-9._/-]{1,96}$")
AUTHORITY_TRUST_BOUNDARY = (
    "git_binds_exact_bytes_external_operator_attests_human_facts_"
    "without_cryptographic_identity_proof"
)
AUTHORITY_SCHEMA_ARTIFACT = (
    "s11-human-acquisition-authority-schema-v1",
    "godot-codex-mcp/schemas/godot_codex/"
    "sprint11-human-acquisition-authority.schema.json",
)
HUMAN_ACQUISITION_ROOT = "tests/codex/acquisition/sprint11/human"

GOALS = (
    "install_and_connect",
    "saved_semantic_context",
    "offline_context",
    "doctor_fault_matrix",
    "runtime_diagnostics",
    "compound_write_and_undo",
    "project_isolation",
)
HELP_SOURCES = (
    "package_beta_guide",
    "package_skill",
    "mcp_server_instructions",
    "godot_codex_help",
    "godot_codex_doctor_output",
    "project_agents_guidance",
)
COMPREHENSION = (
    "project_binding",
    "offline_limits",
    "sandbox_approval",
    "action_only_form",
    "validation",
    "undo",
)
FAULTS: dict[str, tuple[str, str]] = {
    "missing_binary": ("binary_missing", "upgrade_godot_codex"),
    "stale_discovery": ("bridge_discovery_stale", "start_matching_editor"),
    "version_mismatch": (
        "bridge_version_incompatible",
        "upgrade_godot_bridge",
    ),
    "authentication_failure": (
        "bridge_authentication_failed",
        "start_matching_editor",
    ),
    "invalid_project_config": ("project_config_invalid", "repair_project_config"),
}
ASSERTIONS: dict[str, tuple[str, ...]] = {
    "install_and_connect": (
        "connection.status",
        "host.approval_layers",
        "host.config_reload",
        "host.launcher",
    ),
    "saved_semantic_context": ("saved.current_scene",),
    "offline_context": ("host.offline_status", "offline.saved_query"),
    "doctor_fault_matrix": ("connection.status",),
    "runtime_diagnostics": ("runtime.error_stack",),
    "compound_write_and_undo": (
        "approval.accept",
        "approval.cancel",
        "approval.decline",
        "approval.timeout",
        "host.unsupported_form",
        "transaction.apply",
        "transaction.preview",
        "transaction.undo",
        "validation.result",
    ),
    "project_isolation": ("multi_project.reject",),
}
CRITERIA: dict[str, tuple[str, ...]] = {
    "install_and_connect": (
        "package_manifest_verified",
        "package_owned_launcher_used",
        "exact_project_bound",
        "first_status_obtained",
    ),
    "saved_semantic_context": (
        "saved_scene_identified",
        "fact_supported_by_bounded_evidence",
    ),
    "offline_context": (
        "editor_closed_confirmed",
        "offline_state_honest",
        "saved_context_only",
        "live_runtime_not_inferred",
    ),
    "doctor_fault_matrix": (
        "exact_five_faults_attempted",
        "diagnostic_codes_identified",
        "remediations_succeeded",
    ),
    "runtime_diagnostics": (
        "scene_started",
        "intentional_error_identified",
        "source_mapped_stack_bounded",
    ),
    "compound_write_and_undo": (
        "preview_understood",
        "sandbox_approval_distinguished",
        "action_form_understood",
        "accepted_apply_validated",
        "undo_exact",
        "negative_outcomes_no_mutation",
    ),
    "project_isolation": (
        "second_project_opened",
        "cross_project_identity_rejected",
        "no_project_fallback",
    ),
}
TARGETS: dict[str, tuple[int, str]] = {
    "first_useful_status_ms": (300_000, "less_than_or_equal"),
    "connected_live_query_ms": (900_000, "less_than_or_equal"),
    "secret_copy_count": (0, "equal"),
    "manual_toml_steps": (0, "equal"),
    "project_integrity_failures": (0, "equal"),
    "project_isolation_failures": (0, "equal"),
    "fault_recoveries": (5, "equal"),
}
SOURCE_ARTIFACTS: dict[str, tuple[str, str]] = {
    "prompt_pack": (
        "sprint11-external-beta-v1",
        "tests/codex/prompts/sprint11-external-beta-v1.json",
    ),
    "participant_script": (
        "sprint11-human-participant-script-v1",
        "tests/codex/usability/sprint11-participant-script-v1.json",
    ),
    "operator_protocol": (
        "sprint11-human-operator-protocol-v1",
        "tests/codex/usability/sprint11-operator-protocol-v1.json",
    ),
    "consent_document": (
        "sprint11-human-usability-consent-v1",
        "tests/codex/usability/sprint11-consent-v1.md",
    ),
    "trace_schema": (
        "s11-human-usability-trace-schema-v1",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-human-usability-trace.schema.json",
    ),
    "rubric_schema": (
        "s11-human-usability-rubric-schema-v1",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-human-usability-rubric.schema.json",
    ),
    "rubric_template": (
        "s11-human-usability-rubric-template-v1",
        "tests/codex/usability/templates/"
        "sprint11-human-usability-rubric.template.json",
    ),
}
TEMPLATES = {
    "trace": (
        "tests/codex/usability/templates/"
        "sprint11-human-usability-trace.template.json",
        "s11-human-usability-trace/1.0",
    ),
    "rubric": (
        "tests/codex/usability/templates/"
        "sprint11-human-usability-rubric.template.json",
        "s11-human-usability-rubric/1.0",
    ),
    "consent": (
        "tests/codex/usability/templates/"
        "sprint11-human-consent-receipt.template.json",
        "s11-human-consent-receipt/1.0",
    ),
    "defects": (
        "tests/codex/usability/templates/"
        "sprint11-human-usability-defect-ledger.template.json",
        "s11-human-usability-defect-ledger/1.0",
    ),
}
SCHEMAS = (
    "godot-codex-mcp/schemas/godot_codex/"
    "sprint11-human-acquisition-authority.schema.json",
    "godot-codex-mcp/schemas/godot_codex/"
    "sprint11-human-usability-trace.schema.json",
    "godot-codex-mcp/schemas/godot_codex/"
    "sprint11-human-usability-rubric.schema.json",
    "godot-codex-mcp/schemas/godot_codex/"
    "sprint11-human-consent-receipt.schema.json",
    "godot-codex-mcp/schemas/godot_codex/"
    "sprint11-human-usability-defect-ledger.schema.json",
)


class AcquisitionError(RuntimeError):
    """Raised when an artifact cannot prove the closed acquisition contract."""


@dataclass(frozen=True)
class SourceBoundAcquisitionAuthority:
    """Validated external attestation loaded by a source-binding caller.

    The source commit and digest prove which bytes were reviewed. They do not
    cryptographically prove that a person participated or that the operator's
    independence observation is true; that observation is the explicit
    external trust boundary.
    """

    source_commit: str
    relative_path: str
    sha256: str
    payload: Mapping[str, Any]


def human_acquisition_directory(run_id: str) -> str:
    _pattern(run_id, RUN_ID, "human acquisition run id")
    return f"{HUMAN_ACQUISITION_ROOT}/s11u-run-{run_id.removeprefix('s11u-run:')}"


def _reject_duplicate_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise AcquisitionError(f"duplicate JSON member: {key}")
        result[key] = value
    return result


def canonical_json(value: object) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
        + "\n"
    ).encode("utf-8")


def digest_bytes(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def digest_document(value: object) -> str:
    return digest_bytes(canonical_json(value))


def read_regular_file(path: Path, *, maximum: int = MAX_JSON_BYTES) -> bytes:
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise AcquisitionError(f"cannot open bounded regular file: {path}") from error
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_size < 0
            or before.st_size > maximum
        ):
            raise AcquisitionError(f"artifact is unsafe or oversized: {path}")
        chunks: list[bytes] = []
        remaining = maximum + 1
        while remaining:
            block = os.read(descriptor, min(remaining, 65536))
            if not block:
                break
            chunks.append(block)
            remaining -= len(block)
        value = b"".join(chunks)
        after = os.fstat(descriptor)
        if len(value) > maximum or (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
        ) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
        ):
            raise AcquisitionError(f"artifact changed during bounded read: {path}")
        return value
    finally:
        os.close(descriptor)


def file_digest(path: Path, *, maximum: int = MAX_JSON_BYTES) -> str:
    return digest_bytes(read_regular_file(path, maximum=maximum))


def load_json(path: Path) -> dict[str, Any]:
    raw = read_regular_file(path)
    try:
        value = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_members,
        )
    except (UnicodeError, json.JSONDecodeError) as error:
        raise AcquisitionError(f"invalid JSON artifact: {path}") from error
    if not isinstance(value, dict):
        raise AcquisitionError(f"JSON artifact must be an object: {path}")
    return value


def load_json_bytes(
    value: bytes,
    *,
    label: str,
    maximum: int = MAX_JSON_BYTES,
) -> dict[str, Any]:
    if not 0 < len(value) <= maximum:
        raise AcquisitionError(f"{label} has invalid byte bounds")
    try:
        document = json.loads(
            value.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_members,
        )
    except (UnicodeError, json.JSONDecodeError) as error:
        raise AcquisitionError(f"invalid JSON artifact: {label}") from error
    if not isinstance(document, dict):
        raise AcquisitionError(f"JSON artifact must be an object: {label}")
    return document


def _mapping(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise AcquisitionError(f"{label} must be an object")
    return cast(dict[str, Any], value)


def _array(
    value: Any,
    label: str,
    *,
    minimum: int = 0,
    maximum: int,
) -> list[Any]:
    if not isinstance(value, list) or not minimum <= len(value) <= maximum:
        raise AcquisitionError(f"{label} has invalid list bounds")
    return value


def _exact(value: Any, fields: Iterable[str], label: str) -> dict[str, Any]:
    result = _mapping(value, label)
    expected = set(fields)
    if set(result) != expected:
        raise AcquisitionError(
            f"{label} fields differ: expected {sorted(expected)}, "
            f"found {sorted(result)}"
        )
    return result


def _const(value: Any, expected: Any, label: str) -> None:
    if value != expected or (
        isinstance(expected, int)
        and not isinstance(expected, bool)
        and isinstance(value, bool)
    ):
        raise AcquisitionError(f"{label} differs")


def _integer(
    value: Any,
    label: str,
    *,
    minimum: int = 0,
    maximum: int,
) -> int:
    if (
        isinstance(value, bool)
        or not isinstance(value, int)
        or not minimum <= value <= maximum
    ):
        raise AcquisitionError(f"{label} is not a bounded integer")
    return value


def _pattern(value: Any, pattern: re.Pattern[str], label: str) -> str:
    if not isinstance(value, str) or pattern.fullmatch(value) is None:
        raise AcquisitionError(f"{label} has invalid format")
    return value


def _enum(value: Any, allowed: Iterable[str], label: str) -> str:
    choices = tuple(allowed)
    if not isinstance(value, str) or value not in choices:
        raise AcquisitionError(f"{label} is outside the closed enum")
    return value


def _exact_string_set(value: Any, expected: Iterable[str], label: str) -> list[str]:
    values = _array(value, label, maximum=128)
    if not all(isinstance(item, str) for item in values):
        raise AcquisitionError(f"{label} contains a non-string")
    strings = cast(list[str], values)
    if len(strings) != len(set(strings)) or set(strings) != set(expected):
        raise AcquisitionError(f"{label} differs from the frozen set")
    return strings


def _safe_path(value: Any, label: str) -> str:
    path = _pattern(value, SAFE_PATH, label)
    if len(path.encode("utf-8")) > 512:
        raise AcquisitionError(f"{label} exceeds byte bound")
    return path


def _artifact_binding(
    value: Any,
    label: str,
    *,
    expected: tuple[str, str] | None = None,
) -> dict[str, Any]:
    binding = _exact(value, ("id", "path", "sha256"), label)
    identifier = _pattern(binding["id"], SAFE_ID, f"{label} id")
    path = _safe_path(binding["path"], f"{label} path")
    _pattern(binding["sha256"], DIGEST, f"{label} digest")
    if expected is not None and (identifier, path) != expected:
        raise AcquisitionError(f"{label} identity differs")
    return binding


def _template_envelope(
    document: Mapping[str, Any],
    *,
    schema_version: str,
    template_id: str,
    intended: str,
    notice: str,
) -> None:
    _const(document["schema_version"], schema_version, "template schema")
    _const(document["acquisition_kind"], "template", "template acquisition kind")
    _const(document["qualification"], False, "template qualification")
    _const(document["status"], "unacquired", "template status")
    payload = _exact(
        document["payload"],
        ("template_id", "intended_acquisition_kind", "notice"),
        "template payload",
    )
    _const(payload["template_id"], template_id, "template id")
    _const(payload["intended_acquisition_kind"], intended, "template intent")
    _const(payload["notice"], notice, "template notice")


def _validate_envelope(
    document: Any,
    *,
    schema_version: str,
    allowed_real_kind: str,
    allow_real_human: bool,
    template: tuple[str, str, str],
    synthetic_status: str = "fixture_valid",
    real_statuses: tuple[str, ...] = ("passed", "failed"),
) -> tuple[str, dict[str, Any]]:
    value = _exact(
        document,
        ("schema_version", "acquisition_kind", "qualification", "status", "payload"),
        schema_version,
    )
    _const(value["schema_version"], schema_version, "schema version")
    kind = _enum(
        value["acquisition_kind"],
        ("template", "synthetic_contract_fixture", allowed_real_kind),
        "acquisition kind",
    )
    if not isinstance(value["qualification"], bool):
        raise AcquisitionError("qualification must be boolean")
    if kind == "template":
        _template_envelope(
            value,
            schema_version=schema_version,
            template_id=template[0],
            intended=template[1],
            notice=template[2],
        )
        return kind, {}
    if kind == "synthetic_contract_fixture":
        _const(value["qualification"], False, "synthetic qualification")
        _const(value["status"], synthetic_status, "synthetic status")
    else:
        if not allow_real_human:
            raise AcquisitionError(
                "real-human acquisition requires an explicit acquisition-mode opt-in"
            )
        status = _enum(value["status"], real_statuses, "real acquisition status")
        _const(
            value["qualification"],
            status in ("passed", "qualifying"),
            "real acquisition qualification",
        )
    return kind, _mapping(value["payload"], "acquisition payload")


def validate_frozen_materials(repository: Path = REPOSITORY) -> None:
    """Validate source contracts and every embedded digest binding."""

    script = load_json(
        repository / "tests/codex/usability/sprint11-participant-script-v1.json"
    )
    script = _exact(
        script,
        (
            "schema_version",
            "script_id",
            "artifact_kind",
            "qualification",
            "audience",
            "language",
            "prompt_pack",
            "allowed_help_sources",
            "prohibited_assistance",
            "opening",
            "goals",
            "comprehension_questions",
            "closing",
        ),
        "participant script",
    )
    _const(
        script["schema_version"],
        "s11-human-participant-script/1.0",
        "participant script schema",
    )
    _const(
        script["script_id"],
        "sprint11-human-participant-script-v1",
        "participant script id",
    )
    _const(
        script["artifact_kind"],
        "frozen_participant_script",
        "participant artifact kind",
    )
    _const(script["qualification"], False, "participant script qualification")
    _const(script["audience"], "participant", "participant script audience")
    _const(script["language"], "en", "participant script language")
    prompt = _exact(script["prompt_pack"], ("id", "path"), "script prompt binding")
    _const(prompt["id"], SOURCE_ARTIFACTS["prompt_pack"][0], "script prompt id")
    _const(prompt["path"], SOURCE_ARTIFACTS["prompt_pack"][1], "script prompt path")
    _exact_string_set(
        script["allowed_help_sources"], HELP_SOURCES, "script help sources"
    )
    _exact_string_set(
        script["prohibited_assistance"],
        (
            "developer_coaching",
            "fixture_oracle",
            "golden_outputs",
            "source_tests",
            "tool_call_recipe",
            "raw_project_config",
            "another_participant",
        ),
        "script prohibited assistance",
    )
    for field in ("opening", "closing"):
        values = _array(script[field], f"script {field}", minimum=1, maximum=16)
        if not all(
            isinstance(item, str) and 1 <= len(item.encode("utf-8")) <= 512
            for item in values
        ):
            raise AcquisitionError(f"script {field} contains invalid text")
    goals = _array(script["goals"], "participant goals", minimum=7, maximum=7)
    for sequence, (record, goal_id) in enumerate(zip(goals, GOALS), start=1):
        goal = _exact(
            record,
            (
                "sequence",
                "goal_id",
                "participant_prompt",
                "completion_prompt",
                "semantic_assertion_ids",
            ),
            "participant goal",
        )
        _const(goal["sequence"], sequence, "participant goal sequence")
        _const(goal["goal_id"], goal_id, "participant goal id")
        for field in ("participant_prompt", "completion_prompt"):
            if not isinstance(goal[field], str) or not 1 <= len(
                goal[field].encode("utf-8")
            ) <= 1024:
                raise AcquisitionError(f"participant goal {field} is invalid")
        _exact_string_set(
            goal["semantic_assertion_ids"],
            ASSERTIONS[goal_id],
            f"{goal_id} assertions",
        )
    questions = _array(
        script["comprehension_questions"],
        "comprehension questions",
        minimum=6,
        maximum=6,
    )
    for record, comprehension_id in zip(questions, COMPREHENSION):
        question = _exact(
            record,
            ("comprehension_id", "question"),
            "comprehension question",
        )
        _const(
            question["comprehension_id"],
            comprehension_id,
            "comprehension question id",
        )
        if not isinstance(question["question"], str) or not 1 <= len(
            question["question"].encode("utf-8")
        ) <= 512:
            raise AcquisitionError("comprehension question text is invalid")

    protocol_path = (
        repository / "tests/codex/usability/sprint11-operator-protocol-v1.json"
    )
    protocol = load_json(protocol_path)
    protocol = _exact(
        protocol,
        (
            "schema_version",
            "protocol_id",
            "artifact_kind",
            "qualification",
            "audience",
            "contract_bindings",
            "run_coordinate_contract",
            "eligibility",
            "consent",
            "pseudonymization",
            "fault_randomization",
            "operator_interventions",
            "clean_start_protocol",
            "faults",
            "task_and_comprehension_capture",
            "cleanup_protocol",
            "retest_policy",
        ),
        "operator protocol",
    )
    _const(
        protocol["schema_version"],
        "s11-human-operator-protocol/1.0",
        "operator protocol schema",
    )
    _const(
        protocol["protocol_id"],
        "sprint11-human-operator-protocol-v1",
        "operator protocol id",
    )
    _const(
        protocol["artifact_kind"],
        "frozen_operator_only_protocol",
        "operator protocol kind",
    )
    _const(protocol["qualification"], False, "operator protocol qualification")
    _const(protocol["audience"], "operator_only", "operator protocol audience")
    bindings = _array(
        protocol["contract_bindings"],
        "operator contract bindings",
        minimum=12,
        maximum=32,
    )
    seen_binding_ids: set[str] = set()
    for value in bindings:
        binding = _artifact_binding(value, "operator contract binding")
        identifier = cast(str, binding["id"])
        if identifier in seen_binding_ids:
            raise AcquisitionError("operator contract binding is duplicated")
        seen_binding_ids.add(identifier)
        path = repository / cast(str, binding["path"])
        expected_digest = file_digest(
            path,
            maximum=MAX_JSON_BYTES
            if path.suffix in (".json", ".py")
            else 256 * 1024,
        )
        _const(binding["sha256"], expected_digest, f"{identifier} bound digest")
    required_binding_ids = {
        "sprint11-external-beta-v1",
        "sprint11-human-usability-consent-v1",
        "sprint11-human-participant-script-v1",
        "sprint11-human-fault-harness-v1",
        "s11-human-acquisition-authority-schema-v1",
        "s11-human-usability-trace-schema-v1",
        "s11-human-usability-trace-template-v1",
        "s11-human-usability-rubric-schema-v1",
        "s11-human-usability-rubric-template-v1",
        "s11-human-consent-receipt-schema-v1",
        "s11-human-consent-receipt-template-v1",
        "s11-human-usability-defect-ledger-schema-v1",
        "s11-human-usability-defect-ledger-template-v1",
    }
    if seen_binding_ids != required_binding_ids:
        raise AcquisitionError("operator contract binding set differs")

    coordinate = _exact(
        protocol["run_coordinate_contract"],
        (
            "freeze_before_consent",
            "immutable_through_cleanup",
            "package_fields",
            "fixture_fields",
            "surface_host_fields",
            "source_contract_fields",
            "rules",
        ),
        "operator run coordinate contract",
    )
    _const(
        coordinate["freeze_before_consent"],
        True,
        "coordinate freeze before consent",
    )
    _const(
        coordinate["immutable_through_cleanup"],
        True,
        "coordinate immutable through cleanup",
    )
    _exact_string_set(
        coordinate["package_fields"],
        (
            "source_commit",
            "manifest_sha256",
            "archive_sha256",
            "package_tree_sha256",
            "operations_binary_sha256",
            "mcp_binary_sha256",
            "godot_prerequisite_sha256",
        ),
        "coordinate package fields",
    )
    _exact_string_set(
        coordinate["fixture_fields"],
        ("fixture_id", "tree_sha256", "project_file_sha256"),
        "coordinate fixture fields",
    )
    _exact_string_set(
        coordinate["surface_host_fields"],
        (
            "surface",
            "host_coordinate_id",
            "host_coordinate_sha256",
            "host_artifact_sha256",
            "host_tree_sha256",
            "host_version",
            "host_build",
            "architecture",
        ),
        "coordinate surface/host fields",
    )
    _exact_string_set(
        coordinate["source_contract_fields"],
        (
            "prompt_pack",
            "participant_script",
            "operator_protocol",
            "consent_document",
            "trace_schema",
            "rubric_schema",
            "rubric_template",
        ),
        "coordinate source-contract fields",
    )
    coordinate_rules = _array(
        coordinate["rules"], "coordinate rules", minimum=4, maximum=4
    )
    if not all(
        isinstance(item, str) and 1 <= len(item.encode("utf-8")) <= 1024
        for item in coordinate_rules
    ):
        raise AcquisitionError("coordinate rules are invalid")

    eligibility = _exact(
        protocol["eligibility"],
        (
            "native_os",
            "native_architecture",
            "minimum_participants",
            "minimum_implementation_independent_participants",
            "real_person_required",
            "clean_start_required",
            "disposable_fixture_required",
            "developer_coaching_allowed",
            "fixture_answers_allowed",
            "screen_recording_allowed",
            "raw_prompt_recording_allowed",
        ),
        "operator eligibility",
    )
    for field in (
        "real_person_required",
        "clean_start_required",
        "disposable_fixture_required",
    ):
        _const(eligibility.get(field), True, f"eligibility {field}")
    for field in (
        "developer_coaching_allowed",
        "fixture_answers_allowed",
        "screen_recording_allowed",
        "raw_prompt_recording_allowed",
    ):
        _const(eligibility.get(field), False, f"eligibility {field}")
    _const(eligibility.get("minimum_participants"), 3, "participant minimum")
    _const(
        eligibility.get("minimum_implementation_independent_participants"),
        1,
        "independent participant minimum",
    )
    _const(eligibility.get("native_os"), "macos", "native OS")
    _const(eligibility.get("native_architecture"), "arm64", "native architecture")

    consent = _exact(
        protocol["consent"],
        (
            "document_id",
            "version",
            "record_before_tasks",
            "operator_must_explain",
            "receipt_schema",
            "private_ledger_location",
        ),
        "operator consent",
    )
    _const(
        consent["document_id"],
        "sprint11-human-usability-consent",
        "operator consent document id",
    )
    _const(consent["version"], "v1", "operator consent version")
    _const(
        consent["record_before_tasks"],
        True,
        "operator consent before tasks",
    )
    _exact_string_set(
        consent["operator_must_explain"],
        (
            "voluntary_participation_and_stop",
            "collected_content_free_fields",
            "excluded_private_fields",
            "retention_period",
            "withdrawal_deadline",
            "allowed_help_sources",
            "developer_coaching_prohibition",
        ),
        "operator consent explanation set",
    )
    _const(
        consent["receipt_schema"],
        "s11-human-consent-receipt/1.0",
        "operator consent receipt schema",
    )
    _const(
        consent["private_ledger_location"],
        "outside_repository_and_release_evidence",
        "operator consent ledger location",
    )

    pseudonym = _exact(
        protocol["pseudonymization"],
        (
            "algorithm",
            "formula",
            "output_format",
            "salt_source",
            "salt_minimum_bytes",
            "salt_and_subject_key_storage",
            "salt_or_subject_key_in_trace",
            "retention",
        ),
        "operator pseudonymization",
    )
    _const(
        pseudonym["algorithm"],
        "salted-sha256-domain-separated-v1",
        "operator pseudonym algorithm",
    )
    _const(
        pseudonym["formula"],
        (
            "SHA256(UTF8('s11-human-participant-v1') || 0x00 || "
            "random_salt_32_bytes || 0x00 || UTF8(local_subject_key))"
        ),
        "operator pseudonym formula",
    )
    _const(
        pseudonym["output_format"],
        "participant:sha256:<lowercase-hex>",
        "operator pseudonym output",
    )
    _const(
        pseudonym["salt_source"],
        "cryptographically_secure_os_rng",
        "operator pseudonym salt source",
    )
    _const(
        pseudonym["salt_minimum_bytes"],
        32,
        "operator pseudonym salt size",
    )
    _const(
        pseudonym["salt_and_subject_key_storage"],
        "private_consent_ledger_only",
        "operator pseudonym custody",
    )
    _const(
        pseudonym["salt_or_subject_key_in_trace"],
        False,
        "operator pseudonym material exclusion",
    )
    _const(
        pseudonym["retention"],
        "until_declared_withdrawal_deadline_then_destroy",
        "operator pseudonym retention",
    )

    randomization = _exact(
        protocol["fault_randomization"],
        (
            "algorithm",
            "seed_source",
            "seed_bytes",
            "score_formula",
            "ordering",
            "seed_commitment_formula",
            "record",
            "do_not_record",
            "exact_scenarios",
        ),
        "fault randomization",
    )
    _const(
        randomization.get("algorithm"),
        "sha256-seeded-sort-v1",
        "fault ordering algorithm",
    )
    _const(randomization.get("seed_bytes"), 32, "fault seed bytes")
    _const(
        randomization["seed_source"],
        "cryptographically_secure_os_rng",
        "fault seed source",
    )
    _const(
        randomization["score_formula"],
        (
            "SHA256(UTF8('s11-fault-order-v1') || 0x00 || seed || 0x00 || "
            "UTF8(run_id) || 0x00 || UTF8(scenario))"
        ),
        "fault ordering formula",
    )
    _const(
        randomization["ordering"],
        "ascending_lexicographic_score_then_scenario",
        "fault ordering rule",
    )
    _const(
        randomization["seed_commitment_formula"],
        "SHA256(UTF8('s11-fault-seed-v1') || 0x00 || seed)",
        "fault seed commitment formula",
    )
    _exact_string_set(
        randomization["record"],
        (
            "algorithm",
            "seed_commitment",
            "committed_before_first_fault",
            "order",
        ),
        "fault randomization recorded fields",
    )
    _exact_string_set(
        randomization["do_not_record"],
        ("seed",),
        "fault randomization excluded fields",
    )
    _exact_string_set(
        randomization.get("exact_scenarios"),
        FAULTS,
        "operator fault scenarios",
    )

    interventions = _exact(
        protocol["operator_interventions"],
        ("allowed", "prohibited", "safety_stop_triggers"),
        "operator interventions",
    )
    _exact_string_set(
        interventions["allowed"],
        ("consent", "fault_inject", "fault_reset", "safety_stop"),
        "allowed operator interventions",
    )
    _exact_string_set(
        interventions["prohibited"],
        (
            "diagnosis",
            "remediation_answer",
            "tool_selection",
            "fixture_answer",
            "task_rephrasing_after_start",
            "configuration_edit_for_participant",
            "approval_choice",
        ),
        "prohibited operator interventions",
    )
    _exact_string_set(
        interventions["safety_stop_triggers"],
        (
            "participant_attempts_non_disposable_target",
            "participant_attempts_secret_or_account_disclosure",
            "fault_state_escapes_run_root",
            "unexpected_source_or_package_mutation",
            "reset_verification_fails",
        ),
        "operator safety-stop triggers",
    )

    clean_start = _array(
        protocol["clean_start_protocol"],
        "clean-start protocol",
        minimum=6,
        maximum=6,
    )
    clean_start_ids = (
        "verify_frozen_inputs",
        "create_private_run_root",
        "copy_disposable_fixture",
        "allocate_disposable_install",
        "start_fresh_surface_task",
        "emit_clean_start_receipt",
    )
    for value, step_id in zip(clean_start, clean_start_ids):
        step = _exact(value, ("step_id", "action"), "clean-start step")
        _const(step["step_id"], step_id, "clean-start step id")
        if not isinstance(step["action"], str) or not 1 <= len(
            step["action"].encode("utf-8")
        ) <= 1024:
            raise AcquisitionError("clean-start action is invalid")

    fault_records = _array(
        protocol["faults"], "operator faults", minimum=5, maximum=5
    )
    observed_faults: set[str] = set()
    for value in fault_records:
        fault = _mapping(value, "operator fault")
        required = {
            "scenario",
            "expected_diagnostic_code",
            "expected_remediation_id",
            "harness_commands",
            "preconditions",
            "inject",
            "participant_check",
            "reset",
            "postconditions",
        }
        if set(fault) != required:
            raise AcquisitionError("operator fault fields differ")
        scenario = _enum(fault["scenario"], FAULTS, "operator fault scenario")
        if scenario in observed_faults:
            raise AcquisitionError("operator fault scenario is duplicated")
        observed_faults.add(scenario)
        _const(
            fault["expected_diagnostic_code"],
            FAULTS[scenario][0],
            "operator fault diagnostic",
        )
        _const(
            fault["expected_remediation_id"],
            FAULTS[scenario][1],
            "operator fault remediation",
        )
        commands = _array(
            fault["harness_commands"],
            "operator fault harness commands",
            minimum=2,
            maximum=2,
        )
        if not all(
            isinstance(item, str)
            and 1 <= len(item.encode("utf-8")) <= 512
            and not item.startswith("/")
            for item in commands
        ):
            raise AcquisitionError("operator fault harness command differs")
        for field in (
            "preconditions",
            "inject",
            "participant_check",
            "reset",
            "postconditions",
        ):
            values = _array(
                fault[field],
                f"operator fault {field}",
                minimum=1,
                maximum=16,
            )
            if not all(
                isinstance(item, str) and 1 <= len(item.encode("utf-8")) <= 1024
                for item in values
            ):
                raise AcquisitionError(f"operator fault {field} is unbounded")
    if observed_faults != set(FAULTS):
        raise AcquisitionError("operator fault set differs")

    capture = _exact(
        protocol["task_and_comprehension_capture"],
        ("goal_order", "comprehension_ids", "record_only", "never_record"),
        "operator task/comprehension capture",
    )
    goal_order = _array(
        capture["goal_order"], "operator goal order", minimum=7, maximum=7
    )
    if goal_order != list(GOALS):
        raise AcquisitionError("operator goal order differs")
    comprehension_ids = _array(
        capture["comprehension_ids"],
        "operator comprehension ids",
        minimum=6,
        maximum=6,
    )
    if comprehension_ids != list(COMPREHENSION):
        raise AcquisitionError("operator comprehension order differs")
    _exact_string_set(
        capture["record_only"],
        (
            "bounded_duration_ms",
            "wrong_turn_count",
            "allowed_help_source_ids",
            "closed_outcome",
            "content_free_event_projection_digest",
            "closed_comprehension_result",
            "defect_id",
        ),
        "operator recorded field set",
    )
    _exact_string_set(
        capture["never_record"],
        (
            "participant_prompt",
            "participant_answer",
            "account_identity",
            "project_source",
            "property_value",
            "token",
            "endpoint",
            "pid",
            "approval_receipt",
            "native_handle",
            "private_absolute_path",
            "screen_recording",
        ),
        "operator excluded field set",
    )

    cleanup = _array(
        protocol["cleanup_protocol"],
        "operator cleanup protocol",
        minimum=7,
        maximum=7,
    )
    if not all(
        isinstance(item, str) and 1 <= len(item.encode("utf-8")) <= 1024
        for item in cleanup
    ):
        raise AcquisitionError("operator cleanup protocol is invalid")

    retest = _exact(
        protocol["retest_policy"],
        (
            "high_or_critical_requires_fix",
            "waiver_allowed",
            "required_resolution_fields",
            "source_change_discards_prior_package_and_human_qualification",
        ),
        "operator retest policy",
    )
    _const(
        retest["high_or_critical_requires_fix"],
        True,
        "operator high/critical retest policy",
    )
    _const(retest["waiver_allowed"], False, "operator retest waiver")
    _exact_string_set(
        retest["required_resolution_fields"],
        (
            "fixed_by_commit",
            "fixed_package_manifest_sha256",
            "source_retest_run_ids",
            "package_retest_run_ids",
            "human_retest_run_ids",
            "source_retest_passed",
            "package_retest_passed",
            "human_retest_passed",
        ),
        "operator required resolution fields",
    )
    _const(
        retest["source_change_discards_prior_package_and_human_qualification"],
        True,
        "operator source-change retest policy",
    )

    for schema_path in SCHEMAS:
        schema = load_json(repository / schema_path)
        _const(
            schema.get("$schema"),
            "https://json-schema.org/draft/2020-12/schema",
            f"{schema_path} dialect",
        )
        _const(schema.get("type"), "object", f"{schema_path} root type")
        _const(
            schema.get("additionalProperties"),
            False,
            f"{schema_path} root closure",
        )
        if not isinstance(schema.get("$id"), str) or not cast(
            str, schema["$id"]
        ).endswith(Path(schema_path).name):
            raise AcquisitionError(f"{schema_path} id differs")

    validate_trace(load_json(repository / TEMPLATES["trace"][0]))
    validate_rubric(load_json(repository / TEMPLATES["rubric"][0]))
    validate_consent_receipt(load_json(repository / TEMPLATES["consent"][0]))
    validate_defect_ledger(load_json(repository / TEMPLATES["defects"][0]))


def _validate_package_binding(value: Any) -> dict[str, Any]:
    package = _exact(
        value,
        (
            "source_commit",
            "manifest_sha256",
            "archive_sha256",
            "package_tree_sha256",
            "operations_binary_sha256",
            "mcp_binary_sha256",
            "godot_prerequisite_sha256",
        ),
        "package binding",
    )
    _pattern(package["source_commit"], COMMIT, "package source commit")
    for field in (
        "manifest_sha256",
        "archive_sha256",
        "package_tree_sha256",
        "operations_binary_sha256",
        "mcp_binary_sha256",
        "godot_prerequisite_sha256",
    ):
        _pattern(package[field], DIGEST, f"package {field}")
    return package


def _validate_fixture_binding(value: Any) -> dict[str, Any]:
    fixture = _exact(
        value,
        ("fixture_id", "tree_sha256", "project_file_sha256"),
        "fixture binding",
    )
    _pattern(fixture["fixture_id"], SAFE_ID, "fixture id")
    _pattern(fixture["tree_sha256"], DIGEST, "fixture tree digest")
    _pattern(
        fixture["project_file_sha256"], DIGEST, "fixture project-file digest"
    )
    return fixture


def _validate_surface_binding(value: Any) -> dict[str, Any]:
    surface = _exact(
        value,
        (
            "surface",
            "host_coordinate_id",
            "host_coordinate_sha256",
            "host_artifact_sha256",
            "host_tree_sha256",
            "host_version",
            "host_build",
            "architecture",
        ),
        "surface binding",
    )
    _enum(surface["surface"], ("app", "cli", "ide"), "surface")
    _pattern(surface["host_coordinate_id"], SAFE_ID, "host coordinate id")
    for field in (
        "host_coordinate_sha256",
        "host_artifact_sha256",
        "host_tree_sha256",
    ):
        _pattern(surface[field], DIGEST, f"surface {field}")
    for field in ("host_version", "host_build"):
        value = surface[field]
        if (
            not isinstance(value, str)
            or not 1 <= len(value.encode("utf-8")) <= 96
            or re.fullmatch(r"[A-Za-z0-9._+-]+", value) is None
        ):
            raise AcquisitionError(f"surface {field} is invalid")
    _const(surface["architecture"], "arm64", "surface architecture")
    return surface


def _validate_bindings(value: Any) -> dict[str, Any]:
    bindings = _exact(
        value,
        (
            "package",
            "fixture",
            "surface",
            "prompt_pack",
            "participant_script",
            "operator_protocol",
            "consent_document",
            "trace_schema",
            "rubric_schema",
            "rubric_template",
        ),
        "trace bindings",
    )
    _validate_package_binding(bindings["package"])
    _validate_fixture_binding(bindings["fixture"])
    _validate_surface_binding(bindings["surface"])
    for field, expected in SOURCE_ARTIFACTS.items():
        _artifact_binding(bindings[field], f"trace {field}", expected=expected)
    return bindings


def _verify_trace_source_bindings(
    bindings: Mapping[str, Any], repository: Path
) -> None:
    for field, expected in SOURCE_ARTIFACTS.items():
        binding = _artifact_binding(
            bindings[field], f"trace {field}", expected=expected
        )
        path = repository / cast(str, binding["path"])
        _const(
            binding["sha256"],
            file_digest(path),
            f"trace {field} source digest",
        )


def _walk(value: Any) -> Iterable[tuple[str | None, Any]]:
    if isinstance(value, dict):
        for key, child in value.items():
            yield key, child
            yield from _walk(child)
    elif isinstance(value, list):
        for child in value:
            yield None, child
            yield from _walk(child)


def _reject_trace_leaks(value: Mapping[str, Any]) -> None:
    forbidden_keys = {
        "participant_name",
        "participant_email",
        "account_identity",
        "prompt",
        "prompt_text",
        "participant_answer",
        "source_text",
        "property_value",
        "token",
        "endpoint",
        "pid",
        "approval_receipt",
        "approval_nonce",
        "approval_mac",
        "native_handle",
        "study_salt",
        "subject_key",
    }
    secret_markers = (
        "S11_SECRET_CANARY",
        "sk-proj-",
        "BEGIN PRIVATE KEY",
        "session_token",
        "approval_receipt",
    )
    for key, child in _walk(value):
        if key in forbidden_keys:
            raise AcquisitionError(f"trace contains forbidden field: {key}")
        if isinstance(child, str):
            if child.startswith("/") or re.match(r"^[A-Za-z]:[\\/]", child):
                raise AcquisitionError("trace contains an absolute path")
            if any(marker in child for marker in secret_markers):
                raise AcquisitionError("trace contains forbidden secret material")
            if len(child.encode("utf-8")) > 1024:
                raise AcquisitionError("trace contains an oversized string")


def validate_trace(
    document: Any,
    *,
    allow_real_human: bool = False,
    repository: Path | None = None,
) -> dict[str, Any]:
    """Validate a primary trace and return its payload."""

    kind, trace = _validate_envelope(
        document,
        schema_version="s11-human-usability-trace/1.0",
        allowed_real_kind="real_human",
        allow_real_human=allow_real_human,
        template=(
            "sprint11-human-usability-trace-template-v1",
            "real_human",
            "UNACQUIRED TEMPLATE: this file contains no participant observation "
            "and is not qualification evidence.",
        ),
    )
    if kind == "template":
        return {}
    trace = _exact(
        trace,
        (
            "run_id",
            "participant",
            "bindings",
            "clean_start",
            "assistance_policy",
            "fault_randomization",
            "tasks",
            "doctor_faults",
            "timeline",
            "comprehension",
            "metrics",
            "redaction",
            "cleanup",
            "attestation",
            "unresolved_defect_ids",
        ),
        "trace payload",
    )
    run_id = _pattern(trace["run_id"], RUN_ID, "trace run id")
    participant = _exact(
        trace["participant"],
        (
            "participant_id",
            "implementation_independent",
            "consent_recorded",
            "consent_receipt_sha256",
            "pseudonym_algorithm",
            "salt_custody",
        ),
        "trace participant",
    )
    _pattern(
        participant["participant_id"], PARTICIPANT_ID, "trace participant id"
    )
    if not isinstance(participant["implementation_independent"], bool):
        raise AcquisitionError("implementation-independent flag must be boolean")
    _const(participant["consent_recorded"], True, "consent recorded")
    _pattern(
        participant["consent_receipt_sha256"],
        DIGEST,
        "consent receipt digest",
    )
    _const(
        participant["pseudonym_algorithm"],
        "salted-sha256-domain-separated-v1",
        "participant pseudonym algorithm",
    )
    _const(
        participant["salt_custody"],
        "private_consent_ledger_until_withdrawal_deadline",
        "participant salt custody",
    )

    bindings = _validate_bindings(trace["bindings"])
    if repository is not None:
        _verify_trace_source_bindings(bindings, repository)

    clean = _exact(
        trace["clean_start"],
        (
            "receipt_path",
            "receipt_sha256",
            "disposable_fixture_copy",
            "source_fixture_unchanged",
            "no_preexisting_project_config",
            "no_preexisting_bridge_metadata",
            "no_preexisting_offline_cache",
            "fresh_surface_task",
            "frozen_archive_verified",
            "package_manifest_verified",
        ),
        "trace clean start",
    )
    _safe_path(clean["receipt_path"], "clean-start receipt path")
    _pattern(clean["receipt_sha256"], DIGEST, "clean-start receipt digest")
    for field in (
        "disposable_fixture_copy",
        "source_fixture_unchanged",
        "no_preexisting_project_config",
        "no_preexisting_bridge_metadata",
        "no_preexisting_offline_cache",
        "fresh_surface_task",
        "frozen_archive_verified",
        "package_manifest_verified",
    ):
        _const(clean[field], True, f"clean-start {field}")

    assistance = _exact(
        trace["assistance_policy"],
        (
            "allowed_help_sources",
            "developer_coaching",
            "fixture_answers_disclosed",
            "raw_prompts_recorded",
            "operator_intervention_codes",
        ),
        "trace assistance policy",
    )
    _exact_string_set(
        assistance["allowed_help_sources"], HELP_SOURCES, "allowed help sources"
    )
    for field in (
        "developer_coaching",
        "fixture_answers_disclosed",
        "raw_prompts_recorded",
    ):
        _const(assistance[field], False, f"assistance {field}")
    intervention_codes = _array(
        assistance["operator_intervention_codes"],
        "operator interventions",
        maximum=64,
    )
    if not all(
        isinstance(item, str)
        and item in ("consent", "fault_inject", "fault_reset", "safety_stop")
        for item in intervention_codes
    ):
        raise AcquisitionError("operator intervention code differs")

    randomization = _exact(
        trace["fault_randomization"],
        ("algorithm", "seed_commitment", "committed_before_first_fault", "order"),
        "fault randomization",
    )
    _const(
        randomization["algorithm"],
        "sha256-seeded-sort-v1",
        "fault randomization algorithm",
    )
    _pattern(
        randomization["seed_commitment"], DIGEST, "fault seed commitment"
    )
    _const(
        randomization["committed_before_first_fault"],
        True,
        "fault ordering commitment",
    )
    fault_order = _exact_string_set(
        randomization["order"], FAULTS, "fault randomization order"
    )

    tasks = _array(trace["tasks"], "trace tasks", minimum=7, maximum=7)
    task_statuses: list[str] = []
    referenced_events: list[str] = []
    for sequence, (value, goal_id) in enumerate(zip(tasks, GOALS), start=1):
        task = _exact(
            value,
            (
                "sequence",
                "goal_id",
                "status",
                "duration_ms",
                "wrong_turns",
                "help_source_ids",
                "remediation_succeeded",
                "semantic_assertion_ids",
                "started_event",
                "completed_event",
            ),
            "trace task",
        )
        _const(task["sequence"], sequence, "task sequence")
        _const(task["goal_id"], goal_id, "task goal id")
        status = _enum(
            task["status"], ("passed", "failed", "blocked"), "task status"
        )
        task_statuses.append(status)
        _integer(task["duration_ms"], "task duration", maximum=14_400_000)
        _integer(task["wrong_turns"], "task wrong turns", maximum=100)
        help_ids = _array(
            task["help_source_ids"], "task help sources", maximum=6
        )
        if (
            len(help_ids) != len(set(cast(list[str], help_ids)))
            or not all(item in HELP_SOURCES for item in help_ids)
        ):
            raise AcquisitionError("task used a prohibited or duplicate help source")
        if not isinstance(task["remediation_succeeded"], bool):
            raise AcquisitionError("task remediation result must be boolean")
        _exact_string_set(
            task["semantic_assertion_ids"],
            ASSERTIONS[goal_id],
            f"{goal_id} semantic assertions",
        )
        for field in ("started_event", "completed_event"):
            referenced_events.append(
                _pattern(task[field], EVENT_ID, f"task {field}")
            )

    fault_results = _array(
        trace["doctor_faults"], "doctor fault results", minimum=5, maximum=5
    )
    fault_statuses: list[str] = []
    for sequence, (value, scenario) in enumerate(
        zip(fault_results, fault_order), start=1
    ):
        fault = _exact(
            value,
            (
                "sequence",
                "scenario",
                "expected_diagnostic_code",
                "observed_diagnostic_code",
                "expected_remediation_id",
                "observed_remediation_id",
                "status",
                "remediation_succeeded",
                "reset_verified",
                "injected_event",
                "diagnosed_event",
                "reset_event",
            ),
            "doctor fault result",
        )
        _const(fault["sequence"], sequence, "doctor fault sequence")
        _const(fault["scenario"], scenario, "doctor fault scenario")
        expected_code, expected_remediation = FAULTS[scenario]
        _const(
            fault["expected_diagnostic_code"],
            expected_code,
            "expected diagnostic code",
        )
        _const(
            fault["expected_remediation_id"],
            expected_remediation,
            "expected remediation id",
        )
        status = _enum(
            fault["status"],
            ("passed", "failed", "safety_stopped"),
            "fault status",
        )
        fault_statuses.append(status)
        if status == "passed":
            _const(
                fault["observed_diagnostic_code"],
                expected_code,
                "observed diagnostic code",
            )
            _const(
                fault["observed_remediation_id"],
                expected_remediation,
                "observed remediation id",
            )
            _const(
                fault["remediation_succeeded"],
                True,
                "fault remediation result",
            )
        else:
            _enum(
                fault["observed_diagnostic_code"],
                (item[0] for item in FAULTS.values()),
                "failed fault diagnostic",
            )
            _enum(
                fault["observed_remediation_id"],
                (item[1] for item in FAULTS.values()),
                "failed fault remediation",
            )
            if not isinstance(fault["remediation_succeeded"], bool):
                raise AcquisitionError("fault remediation result must be boolean")
        _const(fault["reset_verified"], True, "fault reset verification")
        for field in ("injected_event", "diagnosed_event", "reset_event"):
            referenced_events.append(
                _pattern(fault[field], EVENT_ID, f"fault {field}")
            )

    timeline = _array(
        trace["timeline"], "event timeline", minimum=2, maximum=1024
    )
    timeline_ids: set[str] = set()
    elapsed_previous = -1
    expected_interventions: list[str] = []
    event_kinds: list[str] = []
    for sequence, value in enumerate(timeline, start=1):
        event = _exact(
            value,
            (
                "sequence",
                "event_id",
                "elapsed_ms",
                "actor",
                "kind",
                "subject_id",
                "outcome",
                "projection_sha256",
            ),
            "timeline event",
        )
        _const(event["sequence"], sequence, "timeline sequence")
        event_id = _pattern(event["event_id"], EVENT_ID, "timeline event id")
        _const(event_id, f"event:{sequence:04d}", "timeline event id sequence")
        if event_id in timeline_ids:
            raise AcquisitionError("timeline event id is duplicated")
        timeline_ids.add(event_id)
        elapsed = _integer(
            event["elapsed_ms"], "timeline elapsed time", maximum=14_400_000
        )
        if elapsed < elapsed_previous:
            raise AcquisitionError("timeline elapsed time regressed")
        elapsed_previous = elapsed
        actor = _enum(
            event["actor"], ("participant", "operator", "system"), "event actor"
        )
        kind_value = _enum(
            event["kind"],
            (
                "run_started",
                "consent_recorded",
                "clean_start_verified",
                "task_started",
                "status_observed",
                "help_opened",
                "wrong_turn",
                "task_completed",
                "fault_order_committed",
                "fault_injected",
                "fault_diagnosed",
                "fault_reset",
                "comprehension_asked",
                "comprehension_scored",
                "safety_stop",
                "run_completed",
            ),
            "event kind",
        )
        event_kinds.append(kind_value)
        subject = event["subject_id"]
        if (
            not isinstance(subject, str)
            or not 1 <= len(subject) <= 64
            or re.fullmatch(r"[a-z0-9_.:-]+", subject) is None
        ):
            raise AcquisitionError("event subject id is invalid")
        _enum(
            event["outcome"],
            ("observed", "passed", "failed", "not_applicable"),
            "event outcome",
        )
        _pattern(event["projection_sha256"], DIGEST, "event projection digest")
        if actor == "operator":
            intervention = {
                "consent_recorded": "consent",
                "fault_injected": "fault_inject",
                "fault_reset": "fault_reset",
                "safety_stop": "safety_stop",
            }.get(kind_value)
            if intervention is None:
                raise AcquisitionError(
                    "operator performed an intervention outside the closed policy"
                )
            expected_interventions.append(intervention)
    _const(event_kinds[0], "run_started", "first timeline event")
    _const(event_kinds[-1], "run_completed", "last timeline event")
    if "consent_recorded" not in event_kinds:
        raise AcquisitionError("timeline lacks consent")
    if "clean_start_verified" not in event_kinds:
        raise AcquisitionError("timeline lacks clean-start verification")
    if "fault_order_committed" not in event_kinds:
        raise AcquisitionError("timeline lacks fault-order commitment")
    if intervention_codes != expected_interventions:
        raise AcquisitionError("operator intervention projection differs")

    comprehension_results = _array(
        trace["comprehension"],
        "comprehension results",
        minimum=6,
        maximum=6,
    )
    comprehension_outcomes: list[str] = []
    for value, comprehension_id in zip(comprehension_results, COMPREHENSION):
        result = _exact(
            value,
            ("comprehension_id", "result", "asked_event", "scored_event"),
            "comprehension result",
        )
        _const(
            result["comprehension_id"],
            comprehension_id,
            "comprehension id",
        )
        comprehension_outcomes.append(
            _enum(
                result["result"],
                ("correct", "partial", "incorrect"),
                "comprehension outcome",
            )
        )
        for field in ("asked_event", "scored_event"):
            referenced_events.append(
                _pattern(result[field], EVENT_ID, f"comprehension {field}")
            )
    missing_events = set(referenced_events) - timeline_ids
    if missing_events:
        raise AcquisitionError(
            f"trace references missing timeline events: {sorted(missing_events)}"
        )

    metrics = _exact(
        trace["metrics"],
        (
            "first_useful_status_ms",
            "connected_live_query_ms",
            "secret_copy_count",
            "manual_toml_steps",
            "project_integrity_failures",
            "project_isolation_failures",
            "fault_recoveries",
        ),
        "trace metrics",
    )
    for field in TARGETS:
        maximum = 14_400_000 if field.endswith("_ms") else 100
        _integer(metrics[field], f"metric {field}", maximum=maximum)
    if metrics["fault_recoveries"] > 5:
        raise AcquisitionError("fault recovery count exceeds exact fault set")

    redaction = _exact(
        trace["redaction"],
        (
            "scan_definition_sha256",
            "scan_receipt_sha256",
            "forbidden_field_occurrences",
            "secret_canary_occurrences",
            "private_absolute_path_occurrences",
            "raw_prompt_occurrences",
            "pii_occurrences",
            "native_handle_occurrences",
            "unrestricted_content_occurrences",
        ),
        "trace redaction",
    )
    for field in ("scan_definition_sha256", "scan_receipt_sha256"):
        _pattern(redaction[field], DIGEST, f"redaction {field}")
    for field in (
        "forbidden_field_occurrences",
        "secret_canary_occurrences",
        "private_absolute_path_occurrences",
        "raw_prompt_occurrences",
        "pii_occurrences",
        "native_handle_occurrences",
        "unrestricted_content_occurrences",
    ):
        _const(redaction[field], 0, f"redaction {field}")

    cleanup = _exact(
        trace["cleanup"],
        (
            "all_faults_reset",
            "disposable_fixture_removed",
            "source_fixture_unchanged",
            "package_unchanged",
            "private_salt_not_in_artifacts",
            "private_consent_ledger_outside_repository",
        ),
        "trace cleanup",
    )
    for field in cleanup:
        _const(cleanup[field], True, f"cleanup {field}")

    attestation = _exact(
        trace["attestation"],
        (
            "operator_id",
            "operator_attestation_sha256",
            "participant_summary_confirmed",
            "participant_confirmation_sha256",
            "content_minimization_confirmed",
            "no_developer_coaching_confirmed",
        ),
        "trace attestation",
    )
    _pattern(attestation["operator_id"], OPERATOR_ID, "trace operator id")
    for field in (
        "operator_attestation_sha256",
        "participant_confirmation_sha256",
    ):
        _pattern(attestation[field], DIGEST, f"attestation {field}")
    for field in (
        "participant_summary_confirmed",
        "content_minimization_confirmed",
        "no_developer_coaching_confirmed",
    ):
        _const(attestation[field], True, f"attestation {field}")

    defect_ids = _array(
        trace["unresolved_defect_ids"],
        "unresolved defect ids",
        maximum=128,
    )
    if (
        len(defect_ids) != len(set(cast(list[str], defect_ids)))
        or not all(
            isinstance(defect_id, str) and DEFECT_ID.fullmatch(defect_id)
            for defect_id in defect_ids
        )
    ):
        raise AcquisitionError("unresolved defect IDs are invalid or duplicated")

    passed = (
        all(status == "passed" for status in task_statuses)
        and all(status == "passed" for status in fault_statuses)
        and all(result == "correct" for result in comprehension_outcomes)
        and metrics["first_useful_status_ms"] <= 300_000
        and metrics["connected_live_query_ms"] <= 900_000
        and metrics["secret_copy_count"] == 0
        and metrics["manual_toml_steps"] == 0
        and metrics["project_integrity_failures"] == 0
        and metrics["project_isolation_failures"] == 0
        and metrics["fault_recoveries"] == 5
    )
    envelope = cast(Mapping[str, Any], document)
    if kind == "real_human":
        _const(
            envelope["status"],
            "passed" if passed else "failed",
            "trace calculated status",
        )
        _const(envelope["qualification"], passed, "trace calculated qualification")
    _reject_trace_leaks(trace)
    _const(run_id, trace["run_id"], "trace run id stability")
    return trace


def validate_rubric(
    document: Any,
    *,
    allow_real_human: bool = False,
    repository: Path | None = None,
) -> dict[str, Any]:
    """Validate one closed rubric without accepting free-form assessment."""

    kind, rubric = _validate_envelope(
        document,
        schema_version="s11-human-usability-rubric/1.0",
        allowed_real_kind="real_human",
        allow_real_human=allow_real_human,
        template=(
            "sprint11-human-usability-rubric-template-v1",
            "real_human",
            "UNACQUIRED TEMPLATE: this file contains no participant score and "
            "is not qualification evidence.",
        ),
    )
    if kind == "template":
        return {}
    rubric = _exact(
        rubric,
        (
            "rubric_id",
            "trace",
            "coordinates",
            "task_scores",
            "fault_scores",
            "comprehension_scores",
            "target_scores",
            "defects",
            "overall",
            "attestation",
        ),
        "rubric payload",
    )
    _const(
        rubric["rubric_id"],
        "sprint11-human-usability-rubric-v1",
        "rubric id",
    )
    trace_binding = _exact(
        rubric["trace"],
        ("path", "sha256", "run_id", "participant_id"),
        "rubric trace binding",
    )
    _safe_path(trace_binding["path"], "rubric trace path")
    _pattern(trace_binding["sha256"], DIGEST, "rubric trace digest")
    _pattern(trace_binding["run_id"], RUN_ID, "rubric run id")
    _pattern(
        trace_binding["participant_id"], PARTICIPANT_ID, "rubric participant id"
    )

    coordinates = _exact(
        rubric["coordinates"],
        (
            "package_source_commit",
            "package_manifest_sha256",
            "fixture_tree_sha256",
            "surface",
            "host_coordinate_sha256",
            "prompt_pack",
            "participant_script",
            "rubric_schema",
            "rubric_template",
        ),
        "rubric coordinates",
    )
    _pattern(
        coordinates["package_source_commit"], COMMIT, "rubric source commit"
    )
    for field in (
        "package_manifest_sha256",
        "fixture_tree_sha256",
        "host_coordinate_sha256",
    ):
        _pattern(coordinates[field], DIGEST, f"rubric {field}")
    _enum(coordinates["surface"], ("app", "cli", "ide"), "rubric surface")
    for field in (
        "prompt_pack",
        "participant_script",
        "rubric_schema",
        "rubric_template",
    ):
        binding = _artifact_binding(
            coordinates[field],
            f"rubric {field}",
            expected=SOURCE_ARTIFACTS[field],
        )
        if repository is not None:
            _const(
                binding["sha256"],
                file_digest(repository / cast(str, binding["path"])),
                f"rubric {field} source digest",
            )

    task_scores = _array(
        rubric["task_scores"], "rubric task scores", minimum=7, maximum=7
    )
    task_passes: list[bool] = []
    referenced_events: set[str] = set()
    for value, goal_id in zip(task_scores, GOALS):
        score = _exact(
            value,
            ("goal_id", "result", "criteria"),
            "rubric task score",
        )
        _const(score["goal_id"], goal_id, "rubric goal id")
        result = _enum(
            score["result"], ("passed", "failed"), "rubric task result"
        )
        criteria = _array(
            score["criteria"],
            "rubric task criteria",
            minimum=len(CRITERIA[goal_id]),
            maximum=len(CRITERIA[goal_id]),
        )
        criterion_results: list[str] = []
        for criterion_value, criterion_id in zip(criteria, CRITERIA[goal_id]):
            criterion = _exact(
                criterion_value,
                ("criterion_id", "result", "evidence_event_ids"),
                "rubric criterion",
            )
            _const(
                criterion["criterion_id"], criterion_id, "rubric criterion id"
            )
            criterion_results.append(
                _enum(
                    criterion["result"],
                    ("passed", "failed", "not_observed"),
                    "rubric criterion result",
                )
            )
            event_ids = _array(
                criterion["evidence_event_ids"],
                "criterion evidence events",
                minimum=1,
                maximum=16,
            )
            if len(event_ids) != len(set(cast(list[str], event_ids))):
                raise AcquisitionError("criterion evidence event is duplicated")
            for event_id in event_ids:
                referenced_events.add(
                    _pattern(event_id, EVENT_ID, "criterion evidence event")
                )
        calculated = all(item == "passed" for item in criterion_results)
        _const(result, "passed" if calculated else "failed", "rubric task result")
        task_passes.append(calculated)

    fault_scores = _array(
        rubric["fault_scores"], "rubric fault scores", minimum=5, maximum=5
    )
    seen_faults: set[str] = set()
    fault_passes: list[bool] = []
    for value in fault_scores:
        score = _exact(
            value,
            (
                "scenario",
                "diagnostic_code",
                "remediation_id",
                "diagnosis_result",
                "remediation_result",
                "reset_result",
                "evidence_event_ids",
            ),
            "rubric fault score",
        )
        scenario = _enum(score["scenario"], FAULTS, "rubric fault scenario")
        if scenario in seen_faults:
            raise AcquisitionError("rubric fault scenario is duplicated")
        seen_faults.add(scenario)
        _const(
            score["diagnostic_code"],
            FAULTS[scenario][0],
            "rubric diagnostic code",
        )
        _const(
            score["remediation_id"],
            FAULTS[scenario][1],
            "rubric remediation id",
        )
        outcomes = [
            _enum(
                score[field],
                ("passed", "failed"),
                f"rubric fault {field}",
            )
            for field in (
                "diagnosis_result",
                "remediation_result",
                "reset_result",
            )
        ]
        fault_passes.append(all(item == "passed" for item in outcomes))
        event_ids = _array(
            score["evidence_event_ids"],
            "fault evidence events",
            minimum=3,
            maximum=16,
        )
        if len(event_ids) != len(set(cast(list[str], event_ids))):
            raise AcquisitionError("fault evidence event is duplicated")
        for event_id in event_ids:
            referenced_events.add(
                _pattern(event_id, EVENT_ID, "fault evidence event")
            )
    if seen_faults != set(FAULTS):
        raise AcquisitionError("rubric fault set differs")

    comprehension_scores = _array(
        rubric["comprehension_scores"],
        "rubric comprehension scores",
        minimum=6,
        maximum=6,
    )
    comprehension_passes: list[bool] = []
    for value, comprehension_id in zip(comprehension_scores, COMPREHENSION):
        score = _exact(
            value,
            ("comprehension_id", "result", "evidence_event_id"),
            "rubric comprehension score",
        )
        _const(
            score["comprehension_id"],
            comprehension_id,
            "rubric comprehension id",
        )
        comprehension_passes.append(
            _enum(
                score["result"],
                ("correct", "partial", "incorrect"),
                "rubric comprehension result",
            )
            == "correct"
        )
        referenced_events.add(
            _pattern(
                score["evidence_event_id"],
                EVENT_ID,
                "comprehension evidence event",
            )
        )

    target_scores = _array(
        rubric["target_scores"], "rubric target scores", minimum=7, maximum=7
    )
    target_passes: list[bool] = []
    for value, target_id in zip(target_scores, TARGETS):
        score = _exact(
            value,
            ("target_id", "observed", "limit", "comparison", "result"),
            "rubric target score",
        )
        _const(score["target_id"], target_id, "rubric target id")
        observed = _integer(
            score["observed"], "rubric target observation", maximum=14_400_000
        )
        limit, comparison = TARGETS[target_id]
        _const(score["limit"], limit, "rubric target limit")
        _const(score["comparison"], comparison, "rubric target comparison")
        passed = observed <= limit if comparison == "less_than_or_equal" else observed == limit
        _const(
            score["result"],
            "passed" if passed else "failed",
            "rubric target result",
        )
        target_passes.append(passed)

    defects = _exact(
        rubric["defects"],
        (
            "ledger_path",
            "ledger_sha256",
            "unresolved_defect_ids",
            "unresolved_high_or_critical",
        ),
        "rubric defect binding",
    )
    _safe_path(defects["ledger_path"], "rubric defect-ledger path")
    _pattern(defects["ledger_sha256"], DIGEST, "rubric defect-ledger digest")
    unresolved = _array(
        defects["unresolved_defect_ids"],
        "rubric unresolved defects",
        maximum=128,
    )
    if (
        len(unresolved) != len(set(cast(list[str], unresolved)))
        or not all(
            isinstance(item, str) and DEFECT_ID.fullmatch(item)
            for item in unresolved
        )
    ):
        raise AcquisitionError("rubric unresolved defect IDs differ")
    unresolved_high = _integer(
        defects["unresolved_high_or_critical"],
        "rubric unresolved high/critical defects",
        maximum=128,
    )

    overall = _exact(
        rubric["overall"],
        (
            "scoring_rule",
            "result",
            "all_required_task_criteria_passed",
            "all_faults_recovered",
            "all_comprehension_correct",
            "all_targets_met",
            "no_high_or_critical_open",
        ),
        "rubric overall",
    )
    _const(
        overall["scoring_rule"],
        "all-required-criteria-faults-comprehension-targets-v1",
        "rubric scoring rule",
    )
    calculated_flags = {
        "all_required_task_criteria_passed": all(task_passes),
        "all_faults_recovered": all(fault_passes),
        "all_comprehension_correct": all(comprehension_passes),
        "all_targets_met": all(target_passes),
        "no_high_or_critical_open": unresolved_high == 0,
    }
    for field, expected in calculated_flags.items():
        _const(overall[field], expected, f"rubric overall {field}")
    passed = all(calculated_flags.values())
    _const(overall["result"], "passed" if passed else "failed", "rubric result")

    attestation = _exact(
        rubric["attestation"],
        (
            "assessor_id",
            "trace_digest_verified",
            "frozen_rubric_verified",
            "no_freeform_notes_recorded",
            "no_developer_coaching_confirmed",
            "assessment_projection_sha256",
        ),
        "rubric attestation",
    )
    _pattern(attestation["assessor_id"], OPERATOR_ID, "rubric assessor id")
    for field in (
        "trace_digest_verified",
        "frozen_rubric_verified",
        "no_freeform_notes_recorded",
        "no_developer_coaching_confirmed",
    ):
        _const(attestation[field], True, f"rubric attestation {field}")
    _pattern(
        attestation["assessment_projection_sha256"],
        DIGEST,
        "rubric assessment projection digest",
    )

    if kind == "real_human":
        envelope = cast(Mapping[str, Any], document)
        _const(
            envelope["status"],
            "passed" if passed else "failed",
            "rubric calculated status",
        )
        _const(
            envelope["qualification"], passed, "rubric calculated qualification"
        )
    if not referenced_events:
        raise AcquisitionError("rubric has no trace-event evidence")
    return rubric


def validate_consent_receipt(
    document: Any,
    *,
    allow_real_human: bool = False,
    repository: Path | None = None,
) -> dict[str, Any]:
    """Validate a content-free consent receipt."""

    value = _exact(
        document,
        ("schema_version", "acquisition_kind", "qualification", "status", "payload"),
        "consent receipt",
    )
    _const(
        value["schema_version"],
        "s11-human-consent-receipt/1.0",
        "consent receipt schema",
    )
    kind = _enum(
        value["acquisition_kind"],
        ("template", "synthetic_contract_fixture", "real_human"),
        "consent acquisition kind",
    )
    _const(value["qualification"], False, "consent receipt qualification")
    if kind == "template":
        _template_envelope(
            value,
            schema_version="s11-human-consent-receipt/1.0",
            template_id="sprint11-human-consent-receipt-template-v1",
            intended="real_human",
            notice="UNACQUIRED TEMPLATE: no person has consented and this file "
            "is not qualification evidence.",
        )
        return {}
    if kind == "synthetic_contract_fixture":
        _const(value["status"], "fixture_valid", "synthetic consent status")
    else:
        if not allow_real_human:
            raise AcquisitionError(
                "real-human consent requires an explicit acquisition-mode opt-in"
            )
        _const(value["status"], "recorded", "real consent status")
    receipt = _exact(
        value["payload"],
        (
            "run_id",
            "participant_id",
            "operator_id",
            "consent_document",
            "accepted",
            "recorded_before_tasks",
            "voluntary_and_stop_explained",
            "withdrawal_deadline_explained",
            "retention_policy_id",
            "collected_data_categories",
            "excluded_data_categories",
            "pseudonym",
            "content_free",
            "receipt_projection_sha256",
        ),
        "consent payload",
    )
    _pattern(receipt["run_id"], RUN_ID, "consent run id")
    _pattern(
        receipt["participant_id"], PARTICIPANT_ID, "consent participant id"
    )
    _pattern(receipt["operator_id"], OPERATOR_ID, "consent operator id")
    binding = _exact(
        receipt["consent_document"],
        ("id", "version", "path", "sha256"),
        "consent document binding",
    )
    _const(
        binding["id"],
        "sprint11-human-usability-consent",
        "consent document id",
    )
    _const(binding["version"], "v1", "consent document version")
    _const(
        binding["path"],
        "tests/codex/usability/sprint11-consent-v1.md",
        "consent document path",
    )
    _pattern(binding["sha256"], DIGEST, "consent document digest")
    if repository is not None:
        _const(
            binding["sha256"],
            file_digest(repository / cast(str, binding["path"]), maximum=256 * 1024),
            "consent document source digest",
        )
    for field in (
        "accepted",
        "recorded_before_tasks",
        "voluntary_and_stop_explained",
        "withdrawal_deadline_explained",
    ):
        _const(receipt[field], True, f"consent {field}")
    _const(
        receipt["retention_policy_id"],
        "private-ledger-until-withdrawal-deadline-v1",
        "consent retention policy",
    )
    _exact_string_set(
        receipt["collected_data_categories"],
        (
            "salted_pseudonym",
            "bounded_task_outcomes",
            "bounded_timings",
            "wrong_turn_counts",
            "help_source_ids",
            "comprehension_scores",
            "content_free_event_digests",
        ),
        "consent collected categories",
    )
    _exact_string_set(
        receipt["excluded_data_categories"],
        (
            "name",
            "account_identity",
            "freeform_prompts",
            "project_source",
            "property_values",
            "authentication_material",
            "approval_material",
            "native_handles",
            "private_absolute_paths",
            "screen_recording",
        ),
        "consent excluded categories",
    )
    pseudonym = _exact(
        receipt["pseudonym"],
        (
            "algorithm",
            "domain_separator",
            "salt_minimum_bytes",
            "salt_stored_in_evidence",
            "subject_key_stored_in_evidence",
            "custody",
        ),
        "consent pseudonym",
    )
    _const(
        pseudonym["algorithm"],
        "salted-sha256-domain-separated-v1",
        "consent pseudonym algorithm",
    )
    _const(
        pseudonym["domain_separator"],
        "s11-human-participant-v1",
        "consent pseudonym domain",
    )
    _const(pseudonym["salt_minimum_bytes"], 32, "consent salt minimum")
    _const(
        pseudonym["salt_stored_in_evidence"],
        False,
        "consent salt evidence policy",
    )
    _const(
        pseudonym["subject_key_stored_in_evidence"],
        False,
        "consent subject-key evidence policy",
    )
    _const(
        pseudonym["custody"],
        "private_consent_ledger_until_withdrawal_deadline",
        "consent salt custody",
    )
    content_free = _exact(
        receipt["content_free"],
        ("pii_fields", "prompt_fields", "secret_fields", "private_path_fields"),
        "consent content-free projection",
    )
    for field in content_free:
        _const(content_free[field], 0, f"consent content-free {field}")
    _pattern(
        receipt["receipt_projection_sha256"],
        DIGEST,
        "consent receipt projection digest",
    )
    _reject_trace_leaks(receipt)
    return receipt


def _validate_found_on(value: Any, bound_runs: set[str]) -> dict[str, Any]:
    found = _exact(value, ("source", "package", "human_run_ids"), "defect found-on")
    source = _exact(
        found["source"], ("commit", "reproduction_run_ids"), "defect source"
    )
    _pattern(source["commit"], COMMIT, "defect source commit")
    source_runs = _array(
        source["reproduction_run_ids"],
        "source reproduction run ids",
        minimum=1,
        maximum=32,
    )
    if (
        len(source_runs) != len(set(cast(list[str], source_runs)))
        or not all(
            isinstance(run, str) and SOURCE_RUN_ID.fullmatch(run)
            for run in source_runs
        )
    ):
        raise AcquisitionError("source reproduction run IDs differ")
    package = _exact(
        found["package"],
        ("manifest_sha256", "reproduction_run_ids"),
        "defect package",
    )
    _pattern(
        package["manifest_sha256"], DIGEST, "defect package manifest digest"
    )
    package_runs = _array(
        package["reproduction_run_ids"],
        "package reproduction run ids",
        minimum=1,
        maximum=32,
    )
    if (
        len(package_runs) != len(set(cast(list[str], package_runs)))
        or not all(
            isinstance(run, str) and PACKAGE_RUN_ID.fullmatch(run)
            for run in package_runs
        )
    ):
        raise AcquisitionError("package reproduction run IDs differ")
    human_runs = _array(
        found["human_run_ids"],
        "defect human run ids",
        minimum=1,
        maximum=32,
    )
    if (
        len(human_runs) != len(set(cast(list[str], human_runs)))
        or not all(
            isinstance(run, str)
            and RUN_ID.fullmatch(run)
            and run in bound_runs
            for run in human_runs
        )
    ):
        raise AcquisitionError("defect human run IDs are unbound")
    return found


def _validate_resolution(
    value: Any,
    *,
    found_on: Mapping[str, Any],
) -> dict[str, Any]:
    resolution = _exact(
        value,
        (
            "fixed_by_commit",
            "fixed_package_manifest_sha256",
            "source_retest_run_ids",
            "package_retest_run_ids",
            "human_retest_run_ids",
            "source_retest_passed",
            "package_retest_passed",
            "human_retest_passed",
        ),
        "defect resolution",
    )
    fixed = _pattern(
        resolution["fixed_by_commit"], COMMIT, "defect fixed-by commit"
    )
    found_source = cast(Mapping[str, Any], found_on["source"])
    if fixed == found_source["commit"]:
        raise AcquisitionError("defect fix commit did not advance source")
    fixed_package = _pattern(
        resolution["fixed_package_manifest_sha256"],
        DIGEST,
        "fixed package manifest digest",
    )
    found_package = cast(Mapping[str, Any], found_on["package"])
    if fixed_package == found_package["manifest_sha256"]:
        raise AcquisitionError("defect fix did not rebuild the package")
    run_fields = (
        ("source_retest_run_ids", SOURCE_RUN_ID),
        ("package_retest_run_ids", PACKAGE_RUN_ID),
        ("human_retest_run_ids", RUN_ID),
    )
    for field, pattern in run_fields:
        runs = _array(
            resolution[field],
            f"defect {field}",
            minimum=1,
            maximum=32,
        )
        if (
            len(runs) != len(set(cast(list[str], runs)))
            or not all(isinstance(run, str) and pattern.fullmatch(run) for run in runs)
        ):
            raise AcquisitionError(f"defect {field} differs")
    if set(cast(list[str], resolution["source_retest_run_ids"])) & set(
        cast(list[str], found_source["reproduction_run_ids"])
    ):
        raise AcquisitionError("source retest reused a reproduction run")
    if set(cast(list[str], resolution["package_retest_run_ids"])) & set(
        cast(list[str], found_package["reproduction_run_ids"])
    ):
        raise AcquisitionError("package retest reused a reproduction run")
    if set(cast(list[str], resolution["human_retest_run_ids"])) & set(
        cast(list[str], found_on["human_run_ids"])
    ):
        raise AcquisitionError("human retest reused the finding run")
    for field in (
        "source_retest_passed",
        "package_retest_passed",
        "human_retest_passed",
    ):
        _const(resolution[field], True, f"defect {field}")
    return resolution


def validate_defect_ledger(
    document: Any,
    *,
    allow_real_human: bool = False,
) -> dict[str, Any]:
    """Validate the source/package-bound defect and retest contract."""

    kind, ledger = _validate_envelope(
        document,
        schema_version="s11-human-usability-defect-ledger/1.0",
        allowed_real_kind="real_human_derived",
        allow_real_human=allow_real_human,
        template=(
            "sprint11-human-usability-defect-ledger-template-v1",
            "real_human_derived",
            "UNACQUIRED TEMPLATE: this file contains no observed defect and is "
            "not qualification evidence.",
        ),
        real_statuses=("qualifying", "incomplete"),
    )
    if kind == "template":
        return {}
    ledger = _exact(
        ledger,
        ("ledger_id", "bound_human_run_ids", "defects", "redaction"),
        "defect ledger payload",
    )
    _const(
        ledger["ledger_id"],
        "sprint11-human-usability-defect-ledger-v1",
        "defect ledger id",
    )
    bound_run_values = _array(
        ledger["bound_human_run_ids"],
        "defect-ledger bound runs",
        minimum=1,
        maximum=32,
    )
    if (
        len(bound_run_values) != len(set(cast(list[str], bound_run_values)))
        or not all(
            isinstance(run, str) and RUN_ID.fullmatch(run)
            for run in bound_run_values
        )
    ):
        raise AcquisitionError("defect-ledger bound run IDs differ")
    bound_runs = set(cast(list[str], bound_run_values))
    defects = _array(ledger["defects"], "defect records", maximum=128)
    defect_ids: set[str] = set()
    unresolved_high_or_critical = 0
    for value in defects:
        defect = _mapping(value, "defect record")
        common_fields = {
            "defect_id",
            "severity",
            "category",
            "goal_id",
            "symptom_code",
            "private_issue_projection_sha256",
            "found_on",
            "status",
        }
        status = defect.get("status")
        expected_fields = (
            common_fields if status == "open" else common_fields | {"resolution"}
        )
        if set(defect) != expected_fields:
            raise AcquisitionError("defect record fields differ")
        defect_id = _pattern(defect["defect_id"], DEFECT_ID, "defect id")
        if defect_id in defect_ids:
            raise AcquisitionError("defect id is duplicated")
        defect_ids.add(defect_id)
        severity = _enum(
            defect["severity"],
            ("low", "medium", "high", "critical"),
            "defect severity",
        )
        _enum(
            defect["category"],
            (
                "installation",
                "connection",
                "documentation",
                "diagnostics",
                "offline_model",
                "runtime",
                "approval",
                "validation",
                "undo",
                "project_isolation",
                "privacy",
                "reliability",
            ),
            "defect category",
        )
        _enum(defect["goal_id"], GOALS, "defect goal")
        _enum(
            defect["symptom_code"],
            (
                "blocked_workflow",
                "wrong_diagnosis",
                "missing_remediation",
                "ambiguous_instruction",
                "approval_misunderstood",
                "unexpected_mutation",
                "failed_recovery",
                "incorrect_status",
                "timeout",
                "integrity_failure",
                "isolation_failure",
                "secret_exposure",
            ),
            "defect symptom",
        )
        _pattern(
            defect["private_issue_projection_sha256"],
            DIGEST,
            "private issue projection digest",
        )
        found_on = _validate_found_on(defect["found_on"], bound_runs)
        if status == "open":
            if severity in ("high", "critical"):
                unresolved_high_or_critical += 1
        elif status == "resolved":
            _validate_resolution(defect["resolution"], found_on=found_on)
        else:
            raise AcquisitionError("defect status differs")

    redaction = _exact(
        ledger["redaction"],
        (
            "scan_receipt_sha256",
            "freeform_issue_text_embedded",
            "participant_data_embedded",
            "secret_occurrences",
            "private_path_occurrences",
        ),
        "defect-ledger redaction",
    )
    _pattern(
        redaction["scan_receipt_sha256"], DIGEST, "defect redaction receipt"
    )
    for field in (
        "freeform_issue_text_embedded",
        "participant_data_embedded",
    ):
        _const(redaction[field], False, f"defect redaction {field}")
    for field in ("secret_occurrences", "private_path_occurrences"):
        _const(redaction[field], 0, f"defect redaction {field}")

    qualifying = unresolved_high_or_critical == 0
    if kind == "real_human_derived":
        envelope = cast(Mapping[str, Any], document)
        _const(
            envelope["status"],
            "qualifying" if qualifying else "incomplete",
            "defect ledger calculated status",
        )
        _const(
            envelope["qualification"],
            qualifying,
            "defect ledger calculated qualification",
        )
    return ledger


def derive_participant_id(random_salt: bytes, local_subject_key: str) -> str:
    """Derive the only participant identifier allowed in acquisition records."""

    if len(random_salt) < 32:
        raise AcquisitionError("participant salt must contain at least 32 bytes")
    if not isinstance(local_subject_key, str) or not 1 <= len(
        local_subject_key.encode("utf-8")
    ) <= 256:
        raise AcquisitionError("local subject key is empty or oversized")
    value = (
        b"s11-human-participant-v1\0"
        + random_salt
        + b"\0"
        + local_subject_key.encode("utf-8")
    )
    return f"participant:sha256:{hashlib.sha256(value).hexdigest()}"


def randomized_fault_order(
    run_id: str, random_seed: bytes
) -> tuple[list[str], str]:
    """Return the frozen exact-five order and a content-free seed commitment."""

    _pattern(run_id, RUN_ID, "fault-order run id")
    if len(random_seed) != 32:
        raise AcquisitionError("fault-order seed must contain exactly 32 bytes")
    scores = {}
    for scenario in FAULTS:
        value = (
            b"s11-fault-order-v1\0"
            + random_seed
            + b"\0"
            + run_id.encode("utf-8")
            + b"\0"
            + scenario.encode("utf-8")
        )
        scores[scenario] = hashlib.sha256(value).digest()
    order = sorted(FAULTS, key=lambda scenario: (scores[scenario], scenario))
    commitment = digest_bytes(b"s11-fault-seed-v1\0" + random_seed)
    return order, commitment


def _rubric_event_ids(rubric: Mapping[str, Any]) -> set[str]:
    values: set[str] = set()
    for task in cast(list[Mapping[str, Any]], rubric["task_scores"]):
        for criterion in cast(list[Mapping[str, Any]], task["criteria"]):
            values.update(cast(list[str], criterion["evidence_event_ids"]))
    for fault in cast(list[Mapping[str, Any]], rubric["fault_scores"]):
        values.update(cast(list[str], fault["evidence_event_ids"]))
    for result in cast(list[Mapping[str, Any]], rubric["comprehension_scores"]):
        values.add(cast(str, result["evidence_event_id"]))
    return values


def validate_source_bound_acquisition_authority(
    document_bytes: bytes,
    *,
    source_commit: str,
    relative_path: str,
    expected_sha256: str,
    repository: Path = REPOSITORY,
) -> SourceBoundAcquisitionAuthority:
    """Validate an exact-byte external-operator attestation envelope.

    The caller owns the source binding and must supply bytes read from
    ``relative_path`` at ``source_commit``. This function validates the digest,
    the closed envelope, and all cross-bindable observations. It deliberately
    does not claim that Git or SHA-256 proves a person's identity,
    participation, consent, or implementation independence.
    """

    _pattern(source_commit, COMMIT, "authority source commit")
    if (
        not isinstance(relative_path, str)
        or SAFE_PATH.fullmatch(relative_path) is None
        or not relative_path.startswith(
            f"{HUMAN_ACQUISITION_ROOT}/"
        )
    ):
        raise AcquisitionError("authority source path differs")
    _pattern(expected_sha256, DIGEST, "authority source digest")
    _const(
        digest_bytes(document_bytes),
        expected_sha256,
        "authority exact-byte source binding",
    )
    document = _exact(
        load_json_bytes(
            document_bytes,
            label="source-bound human acquisition authority",
            maximum=256 * 1024,
        ),
        (
            "schema_version",
            "authority_kind",
            "authority_schema",
            "trust_boundary",
            "operator_id",
            "run_id",
            "participant_id",
            "artifact_directory",
            "observations",
            "artifact_sha256",
            "attestation_projection_sha256",
        ),
        "human acquisition authority",
    )
    _const(
        document["schema_version"],
        "s11-human-acquisition-authority/1.0",
        "human acquisition authority schema",
    )
    _const(
        document["authority_kind"],
        "external_operator_attestation",
        "human acquisition authority kind",
    )
    schema_binding = _artifact_binding(
        document["authority_schema"],
        "human acquisition authority schema",
        expected=AUTHORITY_SCHEMA_ARTIFACT,
    )
    _const(
        schema_binding["sha256"],
        file_digest(repository / cast(str, schema_binding["path"])),
        "human acquisition authority schema source digest",
    )
    _const(
        document["trust_boundary"],
        AUTHORITY_TRUST_BOUNDARY,
        "human acquisition authority trust boundary",
    )
    _pattern(document["operator_id"], OPERATOR_ID, "authority operator id")
    _pattern(document["run_id"], RUN_ID, "authority run id")
    _pattern(
        document["participant_id"],
        PARTICIPANT_ID,
        "authority participant id",
    )
    expected_directory = human_acquisition_directory(
        cast(str, document["run_id"])
    )
    _const(
        document["artifact_directory"],
        expected_directory,
        "authority artifact directory",
    )
    _const(
        relative_path,
        f"{expected_directory}/authority.json",
        "authority canonical source path",
    )
    observations = _exact(
        document["observations"],
        (
            "real_person_observed",
            "consent_observed_before_tasks",
            "implementation_independence",
            "developer_coaching_absent",
            "fixture_answers_absent",
        ),
        "authority observations",
    )
    for field in (
        "real_person_observed",
        "consent_observed_before_tasks",
        "developer_coaching_absent",
        "fixture_answers_absent",
    ):
        _const(observations[field], True, f"authority observation {field}")
    _enum(
        observations["implementation_independence"],
        ("independent", "not_independent"),
        "authority implementation independence",
    )
    artifacts = _exact(
        document["artifact_sha256"],
        ("trace", "rubric", "consent", "defect_ledger"),
        "authority artifact digests",
    )
    for field in artifacts:
        _pattern(artifacts[field], DIGEST, f"authority {field} digest")
    projection = {
        key: document[key]
        for key in document
        if key != "attestation_projection_sha256"
    }
    _const(
        document["attestation_projection_sha256"],
        digest_document(projection),
        "authority attestation projection digest",
    )
    _reject_trace_leaks(document)
    return SourceBoundAcquisitionAuthority(
        source_commit=source_commit,
        relative_path=relative_path,
        sha256=expected_sha256,
        payload=document,
    )


def validate_bundle(
    *,
    trace_document: Mapping[str, Any],
    rubric_document: Mapping[str, Any],
    consent_document: Mapping[str, Any],
    defect_document: Mapping[str, Any],
    repository: Path = REPOSITORY,
    allow_real_human: bool = False,
    source_bound_authority: SourceBoundAcquisitionAuthority | None = None,
    artifact_sha256: Mapping[str, str] | None = None,
) -> bool:
    """Cross-validate one primary acquisition bundle.

    Bundle-only validation is structural and never qualifies. The return value
    is true only for an explicitly opted-in, internally passing real-human
    bundle plus a separately validated source-bound external-operator
    authority. Synthetic fixtures always return false.
    """

    validate_frozen_materials(repository)
    trace = validate_trace(
        trace_document,
        allow_real_human=allow_real_human,
        repository=repository,
    )
    rubric = validate_rubric(
        rubric_document,
        allow_real_human=allow_real_human,
        repository=repository,
    )
    consent = validate_consent_receipt(
        consent_document,
        allow_real_human=allow_real_human,
        repository=repository,
    )
    defects = validate_defect_ledger(
        defect_document,
        allow_real_human=allow_real_human,
    )
    if not all((trace, rubric, consent, defects)):
        raise AcquisitionError("an unacquired template cannot form a bundle")

    trace_kind = trace_document["acquisition_kind"]
    expected_kinds = (
        (
            "synthetic_contract_fixture",
            "synthetic_contract_fixture",
            "synthetic_contract_fixture",
            "synthetic_contract_fixture",
        )
        if trace_kind == "synthetic_contract_fixture"
        else ("real_human", "real_human", "real_human", "real_human_derived")
    )
    observed_kinds = (
        trace_kind,
        rubric_document["acquisition_kind"],
        consent_document["acquisition_kind"],
        defect_document["acquisition_kind"],
    )
    if observed_kinds != expected_kinds:
        raise AcquisitionError("bundle acquisition kinds are mixed")

    trace_run = cast(str, trace["run_id"])
    participant = cast(Mapping[str, Any], trace["participant"])
    trace_participant = cast(str, participant["participant_id"])
    attestation = cast(Mapping[str, Any], trace["attestation"])
    if (
        consent["run_id"] != trace_run
        or consent["participant_id"] != trace_participant
        or consent["operator_id"] != attestation["operator_id"]
    ):
        raise AcquisitionError("consent and trace identities differ")
    _const(
        participant["consent_receipt_sha256"],
        digest_document(consent_document),
        "trace consent receipt binding",
    )

    rubric_trace = cast(Mapping[str, Any], rubric["trace"])
    if (
        rubric_trace["run_id"] != trace_run
        or rubric_trace["participant_id"] != trace_participant
        or rubric_trace["sha256"] != digest_document(trace_document)
    ):
        raise AcquisitionError("rubric trace binding differs")

    trace_bindings = cast(Mapping[str, Any], trace["bindings"])
    trace_package = cast(Mapping[str, Any], trace_bindings["package"])
    trace_fixture = cast(Mapping[str, Any], trace_bindings["fixture"])
    trace_surface = cast(Mapping[str, Any], trace_bindings["surface"])
    coordinates = cast(Mapping[str, Any], rubric["coordinates"])
    coordinate_projection = (
        coordinates["package_source_commit"],
        coordinates["package_manifest_sha256"],
        coordinates["fixture_tree_sha256"],
        coordinates["surface"],
        coordinates["host_coordinate_sha256"],
    )
    trace_projection = (
        trace_package["source_commit"],
        trace_package["manifest_sha256"],
        trace_fixture["tree_sha256"],
        trace_surface["surface"],
        trace_surface["host_coordinate_sha256"],
    )
    if coordinate_projection != trace_projection:
        raise AcquisitionError("rubric and trace run coordinates differ")
    for field in (
        "prompt_pack",
        "participant_script",
        "rubric_schema",
        "rubric_template",
    ):
        if coordinates[field] != trace_bindings[field]:
            raise AcquisitionError(f"rubric and trace {field} binding differs")

    trace_tasks = cast(list[Mapping[str, Any]], trace["tasks"])
    rubric_tasks = cast(list[Mapping[str, Any]], rubric["task_scores"])
    for task, score in zip(trace_tasks, rubric_tasks):
        expected_result = "passed" if task["status"] == "passed" else "failed"
        if task["goal_id"] != score["goal_id"] or score["result"] != expected_result:
            raise AcquisitionError("rubric task score differs from trace")

    trace_faults = {
        cast(str, fault["scenario"]): fault
        for fault in cast(list[Mapping[str, Any]], trace["doctor_faults"])
    }
    for score in cast(list[Mapping[str, Any]], rubric["fault_scores"]):
        fault = trace_faults[cast(str, score["scenario"])]
        expected_result = "passed" if fault["status"] == "passed" else "failed"
        if (
            score["diagnostic_code"] != fault["observed_diagnostic_code"]
            or score["remediation_id"] != fault["observed_remediation_id"]
            or score["diagnosis_result"] != expected_result
            or score["remediation_result"]
            != ("passed" if fault["remediation_succeeded"] else "failed")
            or score["reset_result"]
            != ("passed" if fault["reset_verified"] else "failed")
        ):
            raise AcquisitionError("rubric fault score differs from trace")

    trace_comprehension = {
        cast(str, result["comprehension_id"]): result["result"]
        for result in cast(list[Mapping[str, Any]], trace["comprehension"])
    }
    for score in cast(list[Mapping[str, Any]], rubric["comprehension_scores"]):
        if (
            score["result"]
            != trace_comprehension[cast(str, score["comprehension_id"])]
        ):
            raise AcquisitionError("rubric comprehension differs from trace")

    trace_metrics = cast(Mapping[str, Any], trace["metrics"])
    for score in cast(list[Mapping[str, Any]], rubric["target_scores"]):
        if score["observed"] != trace_metrics[cast(str, score["target_id"])]:
            raise AcquisitionError("rubric target observation differs from trace")

    timeline_ids = {
        cast(str, event["event_id"])
        for event in cast(list[Mapping[str, Any]], trace["timeline"])
    }
    if not _rubric_event_ids(rubric) <= timeline_ids:
        raise AcquisitionError("rubric references an event outside the trace")

    defect_binding = cast(Mapping[str, Any], rubric["defects"])
    _const(
        defect_binding["ledger_sha256"],
        digest_document(defect_document),
        "rubric defect-ledger binding",
    )
    bound_runs = cast(list[str], defects["bound_human_run_ids"])
    if trace_run not in bound_runs:
        raise AcquisitionError("defect ledger does not bind the trace run")
    open_defects = {
        cast(str, defect["defect_id"]): cast(str, defect["severity"])
        for defect in cast(list[Mapping[str, Any]], defects["defects"])
        if defect["status"] == "open"
    }
    if set(cast(list[str], defect_binding["unresolved_defect_ids"])) != set(
        open_defects
    ):
        raise AcquisitionError("rubric unresolved defect projection differs")
    high_count = sum(
        severity in ("high", "critical") for severity in open_defects.values()
    )
    _const(
        defect_binding["unresolved_high_or_critical"],
        high_count,
        "rubric high/critical defect count",
    )
    if set(cast(list[str], trace["unresolved_defect_ids"])) != set(open_defects):
        raise AcquisitionError("trace unresolved defect projection differs")

    real_passing = (
        trace_kind == "real_human"
        and trace_document["qualification"] is True
        and rubric_document["qualification"] is True
        and defect_document["qualification"] is True
    )
    if not real_passing or source_bound_authority is None:
        return False
    if artifact_sha256 is None:
        raise AcquisitionError(
            "source-bound authority lacks exact-byte artifact digests"
        )
    observed_artifacts = _exact(
        artifact_sha256,
        ("trace", "rubric", "consent", "defect_ledger"),
        "bundle exact-byte artifact digests",
    )
    for field in observed_artifacts:
        _pattern(
            observed_artifacts[field],
            DIGEST,
            f"bundle exact-byte {field} digest",
        )
    authority = source_bound_authority.payload
    authority_artifacts = cast(Mapping[str, Any], authority["artifact_sha256"])
    if observed_artifacts != authority_artifacts:
        raise AcquisitionError(
            "source-bound authority and exact bundle bytes differ"
        )
    if (
        authority["run_id"] != trace_run
        or authority["participant_id"] != trace_participant
        or authority["operator_id"] != attestation["operator_id"]
    ):
        raise AcquisitionError("source-bound authority identities differ")
    authority_observations = cast(
        Mapping[str, Any],
        authority["observations"],
    )
    if participant["implementation_independent"] is not (
        authority_observations["implementation_independence"] == "independent"
    ):
        raise AcquisitionError(
            "trace independence projection differs from external authority"
        )
    return True


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="validate Sprint 11 human-usability acquisition contracts"
    )
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser(
        "validate-materials", help="validate frozen scripts, schemas, and templates"
    )
    bundle = commands.add_parser(
        "validate-bundle", help="validate one cross-bound primary record bundle"
    )
    bundle.add_argument("--trace", type=Path, required=True)
    bundle.add_argument("--rubric", type=Path, required=True)
    bundle.add_argument("--consent", type=Path, required=True)
    bundle.add_argument("--defects", type=Path, required=True)
    bundle.add_argument(
        "--real-human-acquisition",
        action="store_true",
        help="allow validation of externally acquired real-human records",
    )
    return parser


def main(arguments: Sequence[str] | None = None) -> int:
    options = _parser().parse_args(arguments)
    try:
        if options.command == "validate-materials":
            validate_frozen_materials()
            result = {
                "schema_version": "s11-human-acquisition-kit-validation/1.0",
                "status": "passed",
                "qualification": False,
            }
        elif options.command == "validate-bundle":
            qualifies = validate_bundle(
                trace_document=load_json(options.trace),
                rubric_document=load_json(options.rubric),
                consent_document=load_json(options.consent),
                defect_document=load_json(options.defects),
                allow_real_human=options.real_human_acquisition,
            )
            result = {
                "schema_version": "s11-human-acquisition-bundle-validation/1.0",
                "status": "passed",
                "qualification": qualifies,
            }
        else:
            raise AcquisitionError("unknown validation command")
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 0
    except (AcquisitionError, OSError, ValueError) as error:
        print(f"Sprint 11 human acquisition validation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
