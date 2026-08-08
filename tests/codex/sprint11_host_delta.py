#!/usr/bin/env python3
"""Pure contracts for independent Sprint 11 Codex host qualification.

This module has no process, network, or filesystem side effects.  It classifies
one exact measured host snapshot against the frozen package coordinate and
builds only the detached profile/bundle shapes already accepted by package
0.1.19.
"""

from __future__ import annotations

import copy
import dataclasses
import enum
import hashlib
import json
import re
import stat
from collections.abc import Mapping, Sequence, Set
from pathlib import Path
from typing import Any, Final


SURFACES: Final = ("app", "cli", "ide")
SURFACE_SET: Final = frozenset(SURFACES)
MAX_SAFE_SEQUENCE: Final = (1 << 53) - 1
DIGEST_RE: Final = re.compile(r"sha256:[0-9a-f]{64}\Z")
UNPREFIXED_DIGEST_RE: Final = re.compile(r"[0-9a-f]{64}\Z")
SAFE_TOKEN_RE: Final = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+:/-]{0,127}\Z")
PROFILE_SCHEMA: Final = "godot-codex-host-coordinate-profile/1.0"
BUNDLE_SCHEMA: Final = "godot-codex-surface-compatibility-bundle/1.0"
SMOKE_SCHEMA: Final = "s11-host-smoke/1.0"
RECEIPT_SCHEMA: Final = "s11-host-delta-receipt/1.0"

PROFILE_FIELDS: Final = frozenset(
    {
        "schema_version",
        "profile_id",
        "compatibility_matrix",
        "server_instructions",
        "surfaces",
    }
)
SURFACE_FIELDS: Final = frozenset(
    {
        "surface",
        "host_name",
        "host_identifier",
        "host_artifact_kind",
        "host_version",
        "host_build",
        "host_commit",
        "host_artifact_sha256",
        "host_metadata_sha256",
        "host_code_signature",
        "client_version",
        "client_artifact_sha256",
        "client_code_signature",
        "ide_host_version",
        "ide_shell_identifier",
        "ide_shell_artifact_sha256",
        "ide_shell_team_id",
        "ide_shell_code_signature",
        "qualification",
    }
)
SIGNATURE_FIELDS: Final = frozenset({"mode", "identifier", "team_id", "cdhash"})
SMOKE_FIELDS: Final = frozenset(
    {
        "schema_version",
        "status",
        "client_artifact_sha256",
        "surfaces",
        "protocol",
        "registry",
        "connection",
        "semantic_fact",
        "execution",
        "assertions",
        "redaction",
    }
)


class HostDeltaError(RuntimeError):
    """The supplied host delta cannot authorize compatibility."""


class DeltaClass(str, enum.Enum):
    COORDINATE_ONLY = "coordinate_only"
    INTERACTION_SENSITIVE = "interaction_sensitive"
    SURFACE_STRUCTURAL = "surface_structural"
    PRODUCT_CONTRACT = "product_contract"
    UNCLASSIFIABLE = "unclassifiable"


@dataclasses.dataclass(frozen=True)
class DeltaAssessment:
    delta_class: DeltaClass
    affected_surfaces: tuple[str, ...]
    required_probes: tuple[str, ...]
    reasons: tuple[str, ...]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise HostDeltaError(message)


def canonical_json(value: Any) -> bytes:
    try:
        return json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise HostDeltaError("host delta is not canonical JSON") from error


def ordered_compact_json(value: Any) -> bytes:
    """Match serde struct-field order for the embedded matrix digest."""

    try:
        return json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise HostDeltaError("embedded matrix is not canonical JSON") from error


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def sha256_file(path: Path, *, maximum_bytes: int = 2 * 1024 * 1024) -> str:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise HostDeltaError("host delta input is unavailable") from error
    require(
        stat.S_ISREG(metadata.st_mode) and not path.is_symlink(),
        "host delta input must be a regular non-symlink file",
    )
    require(metadata.st_size <= maximum_bytes, "host delta input exceeds its byte bound")
    try:
        return sha256_bytes(path.read_bytes())
    except OSError as error:
        raise HostDeltaError("host delta input cannot be read") from error


def strict_json_bytes(raw: bytes, *, maximum_bytes: int, label: str) -> dict[str, Any]:
    require(len(raw) <= maximum_bytes, f"{label} exceeds its byte bound")

    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in items:
            require(key not in result, f"{label} has a duplicate member")
            result[key] = value
        return result

    def reject_constant(_value: str) -> None:
        raise HostDeltaError(f"{label} contains a non-finite number")

    try:
        value = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=pairs,
            parse_constant=reject_constant,
        )
    except (UnicodeError, json.JSONDecodeError) as error:
        raise HostDeltaError(f"{label} is invalid JSON") from error
    require(isinstance(value, dict), f"{label} root must be an object")
    return value


def _exact_fields(value: Any, fields: frozenset[str], *, label: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{label} must be an object")
    require(set(value) == fields, f"{label} fields differ")
    return value


def _digest(value: Any, *, label: str, prefixed: bool = True) -> str:
    pattern = DIGEST_RE if prefixed else UNPREFIXED_DIGEST_RE
    require(isinstance(value, str) and pattern.fullmatch(value) is not None, f"{label} digest differs")
    return value


def _safe_token(value: Any, *, label: str) -> str:
    require(
        isinstance(value, str) and SAFE_TOKEN_RE.fullmatch(value) is not None,
        f"{label} is unsafe",
    )
    return value


def _signature(value: Any, *, label: str, required: bool) -> dict[str, str] | None:
    if value is None:
        require(not required, f"{label} is required")
        return None
    record = _exact_fields(value, SIGNATURE_FIELDS, label=label)
    require(record["mode"] in {"strict", "deep_strict"}, f"{label} mode differs")
    _safe_token(record["identifier"], label=f"{label} identifier")
    _safe_token(record["team_id"], label=f"{label} team")
    require(
        isinstance(record["cdhash"], str)
        and re.fullmatch(r"[0-9a-f]{40}(?:[0-9a-f]{24})?", record["cdhash"])
        is not None,
        f"{label} cdhash differs",
    )
    return record


def _surface_map(profile: Any, *, label: str) -> dict[str, dict[str, Any]]:
    record = _exact_fields(profile, PROFILE_FIELDS, label=label)
    require(record["schema_version"] == PROFILE_SCHEMA, f"{label} schema differs")
    _safe_token(record["profile_id"], label=f"{label} ID")
    matrix_binding = _exact_fields(
        record["compatibility_matrix"],
        frozenset({"path", "sha256"}),
        label=f"{label} matrix binding",
    )
    require(
        matrix_binding["path"]
        == "godot-codex-mcp/product/compatibility-matrix.v1.json",
        f"{label} matrix path differs",
    )
    _digest(matrix_binding["sha256"], label=f"{label} matrix")
    instructions = _exact_fields(
        record["server_instructions"],
        frozenset({"path", "file_sha256", "wire_sha256"}),
        label=f"{label} instructions binding",
    )
    require(
        instructions["path"] == "godot-codex-mcp/product/server-instructions.v1.txt",
        f"{label} instructions path differs",
    )
    _digest(instructions["file_sha256"], label=f"{label} instruction file")
    _digest(instructions["wire_sha256"], label=f"{label} instruction wire")
    surfaces = record["surfaces"]
    require(isinstance(surfaces, list) and len(surfaces) == 3, f"{label} surface count differs")
    result: dict[str, dict[str, Any]] = {}
    for value in surfaces:
        surface = _validate_surface(value, label=f"{label} surface")
        name = surface["surface"]
        require(name not in result, f"{label} surface is duplicated")
        result[name] = surface
    require(set(result) == SURFACE_SET, f"{label} surfaces differ")
    return result


def _validate_surface(value: Any, *, label: str) -> dict[str, Any]:
    record = _exact_fields(value, SURFACE_FIELDS, label=label)
    name = record["surface"]
    require(name in SURFACE_SET, f"{label} kind differs")
    for field in (
        "host_name",
        "host_identifier",
        "host_version",
        "host_build",
        "client_version",
    ):
        _safe_token(record[field], label=f"{label} {field}")
    require(
        record["host_artifact_kind"]
        in {"macos_bundle_executable", "standalone_executable", "extension_tree"},
        f"{label} artifact kind differs",
    )
    require(
        record["host_commit"] is None
        or (
            isinstance(record["host_commit"], str)
            and re.fullmatch(r"[0-9a-f]{40}", record["host_commit"])
            is not None
        ),
        f"{label} commit differs",
    )
    _digest(record["host_artifact_sha256"], label=f"{label} host artifact")
    _digest(record["client_artifact_sha256"], label=f"{label} client artifact")
    if record["host_metadata_sha256"] is not None:
        _digest(record["host_metadata_sha256"], label=f"{label} metadata")
    if record["ide_shell_artifact_sha256"] is not None:
        _digest(record["ide_shell_artifact_sha256"], label=f"{label} IDE shell")
    _signature(
        record["host_code_signature"],
        label=f"{label} host signature",
        required=name != "ide",
    )
    _signature(
        record["client_code_signature"],
        label=f"{label} client signature",
        required=True,
    )
    _signature(
        record["ide_shell_code_signature"],
        label=f"{label} IDE signature",
        required=name == "ide",
    )
    require(
        record["qualification"] in {"candidate", "supported", "compatible_reduced"},
        f"{label} qualification differs",
    )
    if name == "ide":
        require(
            record["host_artifact_kind"] == "extension_tree"
            and record["host_metadata_sha256"] is not None
            and record["host_code_signature"] is None
            and record["ide_host_version"] is not None
            and record["ide_shell_identifier"] is not None
            and record["ide_shell_artifact_sha256"] is not None
            and record["ide_shell_team_id"] is not None,
            f"{label} IDE shape differs",
        )
        _safe_token(record["ide_host_version"], label=f"{label} IDE version")
        _safe_token(record["ide_shell_identifier"], label=f"{label} IDE identifier")
        _safe_token(record["ide_shell_team_id"], label=f"{label} IDE team")
    else:
        require(
            record["host_metadata_sha256"] is None
            and record["ide_host_version"] is None
            and record["ide_shell_identifier"] is None
            and record["ide_shell_artifact_sha256"] is None
            and record["ide_shell_team_id"] is None
            and record["ide_shell_code_signature"] is None,
            f"{label} standalone shape differs",
        )
        expected_kind = "macos_bundle_executable" if name == "app" else "standalone_executable"
        require(record["host_artifact_kind"] == expected_kind, f"{label} artifact kind differs")
    return record


def _signature_identity(value: Any) -> tuple[str, str, str] | None:
    if value is None:
        return None
    return value["mode"], value["identifier"], value["team_id"]


def _surface_structure(value: Mapping[str, Any]) -> tuple[Any, ...]:
    return (
        value["surface"],
        value["host_name"],
        value["host_identifier"],
        value["host_artifact_kind"],
        value["host_metadata_sha256"] is not None,
        _signature_identity(value["host_code_signature"]),
        _signature_identity(value["client_code_signature"]),
        value["ide_host_version"] is not None,
        value["ide_shell_identifier"],
        value["ide_shell_team_id"],
        _signature_identity(value["ide_shell_code_signature"]),
    )


def _contract_projection(value: Any, *, label: str) -> str:
    record = _exact_fields(
        value,
        frozenset({"contract_projection_sha256", "complete"}),
        label=label,
    )
    require(record["complete"] is True, f"{label} is incomplete")
    return _digest(record["contract_projection_sha256"], label=label)


def _unclassifiable(reason: str) -> DeltaAssessment:
    return DeltaAssessment(
        delta_class=DeltaClass.UNCLASSIFIABLE,
        affected_surfaces=(),
        required_probes=(),
        reasons=(reason,),
    )


def classify_delta(
    *,
    package_manifest: Mapping[str, Any],
    embedded_matrix: Mapping[str, Any],
    previous_profile: Mapping[str, Any] | None,
    current_profile: Mapping[str, Any],
    previous_contract: Mapping[str, Any] | None,
    current_contract: Mapping[str, Any],
) -> DeltaAssessment:
    """Classify one exact host delta with closed precedence."""

    try:
        package_version = package_manifest.get("package_version")
        package_matrix_digest = package_manifest.get("compatibility_matrix_sha256")
        package = embedded_matrix.get("package")
        if (
            not isinstance(package, dict)
            or package_version != package.get("version")
            or package.get("target") != {"os": "macos", "architecture": "arm64"}
            or not isinstance(package_matrix_digest, str)
            or DIGEST_RE.fullmatch(package_matrix_digest) is None
        ):
            return DeltaAssessment(
                delta_class=DeltaClass.PRODUCT_CONTRACT,
                affected_surfaces=(),
                required_probes=(),
                reasons=("package_or_matrix_changed",),
            )
        current = _surface_map(current_profile, label="current host profile")
        if current_profile["compatibility_matrix"]["sha256"] != package_matrix_digest:
            return DeltaAssessment(
                delta_class=DeltaClass.PRODUCT_CONTRACT,
                affected_surfaces=(),
                required_probes=(),
                reasons=("matrix_binding_changed",),
            )
        current_contract_digest = _contract_projection(
            current_contract,
            label="current interaction contract",
        )
    except HostDeltaError:
        return _unclassifiable("current_measurement_incomplete")

    if previous_profile is None:
        previous: dict[str, dict[str, Any]] | None = None
    else:
        try:
            previous = _surface_map(previous_profile, label="previous host profile")
            if (
                previous_profile["compatibility_matrix"]
                != current_profile["compatibility_matrix"]
                or previous_profile["server_instructions"]
                != current_profile["server_instructions"]
            ):
                return DeltaAssessment(
                    delta_class=DeltaClass.PRODUCT_CONTRACT,
                    affected_surfaces=(),
                    required_probes=(),
                    reasons=("host_independent_binding_changed",),
                )
        except HostDeltaError:
            return _unclassifiable("previous_profile_invalid")

    affected = tuple(
        name
        for name in SURFACES
        if previous is None or previous[name] != current[name]
    )
    if previous is not None:
        structural = tuple(
            name
            for name in SURFACES
            if _surface_structure(previous[name]) != _surface_structure(current[name])
        )
        if structural:
            return DeltaAssessment(
                delta_class=DeltaClass.SURFACE_STRUCTURAL,
                affected_surfaces=structural,
                required_probes=("host_provenance", "host_smoke"),
                reasons=("surface_structure_changed",),
            )

    if previous_contract is None:
        return DeltaAssessment(
            delta_class=DeltaClass.INTERACTION_SENSITIVE,
            affected_surfaces=affected or SURFACES,
            required_probes=("host_smoke", "interaction_contract"),
            reasons=("bootstrap",),
        )
    try:
        previous_contract_digest = _contract_projection(
            previous_contract,
            label="previous interaction contract",
        )
    except HostDeltaError:
        return _unclassifiable("previous_contract_invalid")
    if previous_contract_digest != current_contract_digest:
        return DeltaAssessment(
            delta_class=DeltaClass.INTERACTION_SENSITIVE,
            affected_surfaces=affected or SURFACES,
            required_probes=("host_smoke", "interaction_contract"),
            reasons=("interaction_contract_changed",),
        )
    return DeltaAssessment(
        delta_class=DeltaClass.COORDINATE_ONLY,
        affected_surfaces=affected,
        required_probes=("host_smoke",),
        reasons=(("host_coordinates_changed",) if affected else ("host_coordinates_current",)),
    )


def satisfy_assessment(
    assessment: DeltaAssessment,
    *,
    passed_probes: Set[str],
) -> DeltaAssessment:
    required = set(assessment.required_probes)
    require(set(passed_probes) <= required, "unexpected host delta probe result")
    remaining = tuple(item for item in assessment.required_probes if item not in passed_probes)
    return dataclasses.replace(assessment, required_probes=remaining)


def build_host_profile(
    *,
    embedded_profile: Mapping[str, Any],
    measured_surfaces: Sequence[Mapping[str, Any]],
    qualification: str,
    profile_id: str,
) -> dict[str, Any]:
    _surface_map(embedded_profile, label="embedded host profile")
    _safe_token(profile_id, label="host delta profile ID")
    require(
        qualification in {"candidate", "supported", "compatible_reduced"},
        "host delta qualification differs",
    )
    candidate = copy.deepcopy(dict(embedded_profile))
    candidate["profile_id"] = profile_id
    candidate["surfaces"] = []
    measured: dict[str, dict[str, Any]] = {}
    for value in measured_surfaces:
        record = copy.deepcopy(dict(value))
        _validate_surface(record, label="measured host surface")
        name = record["surface"]
        require(name not in measured, "measured host surface is duplicated")
        record["qualification"] = qualification
        measured[name] = record
    require(set(measured) == SURFACE_SET, "measured host surfaces differ")
    candidate["surfaces"] = [measured[name] for name in SURFACES]
    _surface_map(candidate, label="generated host profile")
    return candidate


def build_surface_bundle(
    *,
    assessment: DeltaAssessment,
    matrix: Mapping[str, Any],
    profile: Mapping[str, Any],
    profile_sha256: str,
    sequence: int,
) -> dict[str, Any]:
    require(
        assessment.delta_class
        in {DeltaClass.COORDINATE_ONLY, DeltaClass.INTERACTION_SENSITIVE},
        "full package qualification is required for this delta",
    )
    require(not assessment.required_probes, "host delta probes are not complete")
    _digest(profile_sha256, label="host profile")
    require(
        isinstance(sequence, int)
        and not isinstance(sequence, bool)
        and 1 <= sequence <= MAX_SAFE_SEQUENCE,
        "surface bundle sequence is invalid",
    )
    package = matrix.get("package")
    require(isinstance(package, dict), "embedded package coordinate differs")
    version = _safe_token(package.get("version"), label="package version")
    target = package.get("target")
    require(
        target == {"os": "macos", "architecture": "arm64"},
        "package target differs",
    )
    surfaces = _surface_map(profile, label="supported host profile")
    rules: list[dict[str, Any]] = []
    for name in SURFACES:
        coordinate = surfaces[name]
        require(
            coordinate["qualification"] == "supported",
            "host surface was not promoted to supported",
        )
        rules.append(
            {
                "surface": name,
                "host_version": coordinate["host_version"],
                "ide_host_version": coordinate["ide_host_version"],
                "target": copy.deepcopy(target),
                "qualification": "supported",
            }
        )
    return {
        "schema_version": BUNDLE_SCHEMA,
        "bundle_id": f"host-delta-{sequence}-{profile_sha256[7:19]}",
        "sequence": sequence,
        "package_version": version,
        "baseline_matrix_sha256": sha256_bytes(ordered_compact_json(matrix)),
        "host_coordinate_profile_sha256": profile_sha256,
        "target": copy.deepcopy(target),
        "surfaces": rules,
    }


def validate_smoke_report(value: Any) -> dict[str, Any]:
    report = _exact_fields(value, SMOKE_FIELDS, label="host smoke report")
    require(
        report["schema_version"] == SMOKE_SCHEMA and report["status"] == "passed",
        "host smoke did not pass",
    )
    _digest(report["client_artifact_sha256"], label="smoke client")
    surfaces = report["surfaces"]
    require(
        isinstance(surfaces, list)
        and surfaces
        and len(surfaces) == len(set(surfaces))
        and set(surfaces) <= SURFACE_SET,
        "host smoke surfaces differ",
    )
    require(report["protocol"] == "2025-11-25", "host smoke protocol differs")
    registry = _exact_fields(
        report["registry"],
        frozenset({"digest", "tools", "fixed_resources", "resource_templates"}),
        label="host smoke registry",
    )
    _digest(registry["digest"], label="host smoke registry", prefixed=False)
    require(
        registry["tools"] == 41
        and registry["fixed_resources"] == 4
        and registry["resource_templates"] == 1,
        "host smoke registry counts differ",
    )
    connection = _exact_fields(
        report["connection"],
        frozenset(
            {
                "schema_version",
                "status",
                "bridge_protocol",
                "static_cache",
                "project_scope_sha256",
            }
        ),
        label="host smoke connection",
    )
    require(
        connection["schema_version"] == "godot-connection-status/1.1"
        and connection["status"] == "ready"
        and connection["bridge_protocol"] == "1.8"
        and connection["static_cache"] == "online_current",
        "host smoke connection differs",
    )
    _digest(connection["project_scope_sha256"], label="host smoke project scope")
    semantic = _exact_fields(
        report["semantic_fact"],
        frozenset({"freshness", "evidence_sha256"}),
        label="host smoke semantic fact",
    )
    require(semantic["freshness"] == "current", "host smoke semantic freshness differs")
    _digest(semantic["evidence_sha256"], label="host smoke evidence")
    execution = _exact_fields(
        report["execution"],
        frozenset({"command_sha256", "duration_ms"}),
        label="host smoke execution",
    )
    _digest(execution["command_sha256"], label="host smoke command")
    require(
        isinstance(execution["duration_ms"], int)
        and not isinstance(execution["duration_ms"], bool)
        and 0 <= execution["duration_ms"] <= 180_000,
        "host smoke duration differs",
    )
    assertions = _exact_fields(
        report["assertions"],
        frozenset(
            {
                "model_turn_absent",
                "project_integrity_preserved",
                "isolated_codex_home",
                "exact_child_shutdown",
                "clean_shutdown",
            }
        ),
        label="host smoke assertions",
    )
    require(all(value is True for value in assertions.values()), "host smoke assertion failed")
    redaction = _exact_fields(
        report["redaction"],
        frozenset(
            {
                "absolute_paths_absent",
                "account_identity_absent",
                "artifact_content_absent",
                "environment_secrets_absent",
            }
        ),
        label="host smoke redaction",
    )
    require(all(value is True for value in redaction.values()), "host smoke redaction failed")
    canonical_json(report)
    return dict(report)


def build_host_delta_receipt(
    *,
    assessment: DeltaAssessment,
    outcome: str,
    bindings: Mapping[str, Any],
    bundle: Mapping[str, Any],
) -> dict[str, Any]:
    require(
        outcome
        in {
            "compatible_observed",
            "compatible_bundle_issued",
            "targeted_surface_check_required",
            "full_package_qualification_required",
        },
        "host delta outcome differs",
    )
    expected_bindings = {
        "package_manifest_sha256",
        "baseline_matrix_sha256",
        "previous_profile_sha256",
        "measurement_sha256",
        "host_profile_sha256",
        "bundle_sha256",
        "contract_projection_sha256",
        "smoke_report_sha256",
    }
    require(set(bindings) == expected_bindings, "host delta receipt bindings differ")
    for field in expected_bindings - {"previous_profile_sha256", "smoke_report_sha256"}:
        _digest(bindings[field], label=f"host delta {field}")
    require(
        bindings["previous_profile_sha256"] is None
        or DIGEST_RE.fullmatch(bindings["previous_profile_sha256"]) is not None,
        "previous profile digest differs",
    )
    smoke_digests = bindings["smoke_report_sha256"]
    require(
        isinstance(smoke_digests, list) and smoke_digests,
        "host smoke receipt bindings differ",
    )
    for value in smoke_digests:
        _digest(value, label="host smoke report")
    bundle_record = _exact_fields(
        bundle,
        frozenset({"bundle_id", "sequence", "qualification"}),
        label="host delta bundle receipt",
    )
    _safe_token(bundle_record["bundle_id"], label="host delta bundle ID")
    require(
        isinstance(bundle_record["sequence"], int)
        and 1 <= bundle_record["sequence"] <= MAX_SAFE_SEQUENCE
        and bundle_record["qualification"] == "supported",
        "host delta bundle receipt differs",
    )
    receipt = {
        "schema_version": RECEIPT_SCHEMA,
        "status": "passed",
        "outcome": outcome,
        "delta_class": assessment.delta_class.value,
        "affected_surfaces": list(assessment.affected_surfaces),
        "required_probes": list(assessment.required_probes),
        "bindings": copy.deepcopy(dict(bindings)),
        "bundle": copy.deepcopy(dict(bundle_record)),
        "assertions": {
            "package_unchanged": True,
            "all_required_surfaces_measured": True,
            "all_distinct_clients_probed": True,
            "project_integrity_preserved": True,
            "manual_interaction_absent": True,
            "external_beta_claimed": False,
        },
        "redaction": {
            "absolute_paths_absent": True,
            "account_identity_absent": True,
            "artifact_content_absent": True,
            "environment_secrets_absent": True,
        },
    }
    canonical_json(receipt)
    return receipt
