#!/usr/bin/env python3
"""Compose package-bound Sprint 11 Technical/Private-Alpha evidence.

This compositor deliberately does not acquire evidence.  It revalidates the
frozen package receipts and the current automated host-delta acquisition, then
publishes only bounded references to those bodies.  It never asserts External
Beta usability or commercial readiness.
"""

from __future__ import annotations

import argparse
import dataclasses
import re
import sys
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any, Final

try:
    from tests.codex import sprint11_host_delta as host_delta
    from tests.codex import sprint11_host_delta_measure as host_measure
    from tests.codex import sprint11_host_delta_qualify as host_qualify
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_host_delta as host_delta
    import sprint11_host_delta_measure as host_measure
    import sprint11_host_delta_qualify as host_qualify


REPOSITORY_ROOT: Path = Path(__file__).resolve().parents[2]
SCHEMA_VERSION: Final = "s11-technical-private-alpha/1.0"
PACKAGE_VERSION: Final = "0.1.19"
MAX_JSON_BYTES: Final = 2 * 1024 * 1024
SOURCE_COMMIT_RE: Final = re.compile(r"[0-9a-f]{40}\Z")
SURFACES: Final = frozenset({"app", "cli", "ide"})
REFERENCE_FIELDS: Final = frozenset(
    {"path", "sha256", "schema_version", "status", "source_commit", "package_manifest_sha256"}
)
REDACTION: Final = {
    "absolute_paths_absent": True,
    "account_identity_absent": True,
    "artifact_content_absent": True,
    "environment_secrets_absent": True,
}


class TechnicalAcceptanceError(RuntimeError):
    """Technical/Private-Alpha evidence is incomplete or inconsistently bound."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise TechnicalAcceptanceError(message)


@dataclasses.dataclass(frozen=True)
class TechnicalInputs:
    package_manifest: Path
    package_live_receipt: Path
    multi_project_receipt: Path
    reproducibility_receipt: Path
    host_delta_directory: Path
    active_compatibility_status: Path


def canonical_json(value: Any) -> bytes:
    try:
        return host_delta.canonical_json(value)
    except host_delta.HostDeltaError as error:
        raise TechnicalAcceptanceError(str(error)) from error


def _read_json(path: Path, *, label: str) -> tuple[dict[str, Any], bytes]:
    try:
        digest = host_delta.sha256_file(path, maximum_bytes=MAX_JSON_BYTES)
        raw = path.read_bytes()
        value = host_delta.strict_json_bytes(raw, maximum_bytes=MAX_JSON_BYTES, label=label)
    except (OSError, host_delta.HostDeltaError) as error:
        raise TechnicalAcceptanceError(f"{label} is invalid") from error
    require(host_delta.sha256_bytes(raw) == digest, f"{label} changed while reading")
    return value, raw


def _relative(path: Path, *, root: Path | None = None) -> str:
    try:
        root = REPOSITORY_ROOT if root is None else root
        resolved_root = root.resolve(strict=True)
        resolved_path = path.resolve(strict=True)
        relative = resolved_path.relative_to(resolved_root)
    except (OSError, ValueError) as error:
        raise TechnicalAcceptanceError("evidence path is not repository-relative") from error
    require(relative.parts and all(part not in {"", ".", ".."} for part in relative.parts), "evidence path differs")
    return relative.as_posix()


def _ref(
    path: Path,
    document: Mapping[str, Any],
    *,
    source_commit: str,
    manifest_digest: str,
    status: str | None = None,
) -> dict[str, str]:
    schema = document.get("schema_version")
    resolved_status = status if status is not None else document.get("status")
    require(isinstance(schema, str) and schema, "evidence schema differs")
    require(isinstance(resolved_status, str) and resolved_status, "evidence status differs")
    return {
        "path": _relative(path),
        "sha256": host_delta.sha256_file(path, maximum_bytes=MAX_JSON_BYTES),
        "schema_version": schema,
        "status": resolved_status,
        "source_commit": source_commit,
        "package_manifest_sha256": manifest_digest,
    }


def _mapping(value: Any, *, label: str) -> Mapping[str, Any]:
    require(isinstance(value, dict), f"{label} differs")
    return value


def _all_true(value: Any, fields: set[str] | frozenset[str], *, label: str) -> None:
    record = _mapping(value, label=label)
    require(fields <= set(record) and all(record[field] is True for field in fields), f"{label} differs")


def _validate_package_receipts(
    *,
    inputs: TechnicalInputs,
    manifest_digest: str,
    source_commit: str,
) -> tuple[
    dict[str, Any],
    dict[str, Any],
    dict[str, Any],
    dict[str, Any],
    Path,
]:
    package_live, _ = _read_json(inputs.package_live_receipt, label="package-live receipt")
    require(
        package_live.get("schema_version") == "s11-packaged-regression-receipt/1.0"
        and package_live.get("status") == "passed",
        "package-live receipt did not pass",
    )
    bindings = _mapping(package_live.get("bindings"), label="package-live bindings")
    require(
        bindings.get("package_manifest_sha256") == manifest_digest
        and bindings.get("package_source_commit") == source_commit,
        "package binding differs for package-live receipt",
    )
    coverage = _mapping(package_live.get("coverage"), label="package-live coverage")
    require(
        coverage.get("sprints") == [6, 7, 8, 9, 10]
        and coverage.get("sprint10_validation") is True,
        "package-live Sprint 6-10 coverage differs",
    )
    commands = package_live.get("commands")
    require(isinstance(commands, list), "package-live commands differ")
    same_commands = [item for item in commands if isinstance(item, dict) and item.get("id") == "s11_same_project"]
    require(len(same_commands) == 1, "same-project evidence is missing or ambiguous")
    same_command = same_commands[0]
    require(same_command.get("exit_code") == 0, "same-project acquisition failed")
    report_path_value = same_command.get("report_path")
    require(isinstance(report_path_value, str) and report_path_value, "same-project report path differs")
    same_path = REPOSITORY_ROOT / report_path_value
    _relative(same_path)
    same_project, same_raw = _read_json(same_path, label="same-project report")
    require(
        host_delta.sha256_bytes(same_raw) == same_command.get("report_sha256"),
        "same-project report digest differs",
    )
    require(
        same_project.get("schema_version") == "s11-same-project-live/1.1"
        and same_project.get("status") == "passed"
        and same_project.get("package_version") == PACKAGE_VERSION,
        "same-project report identity differs",
    )
    _all_true(
        same_project.get("assertions"),
        {
            "busy_not_syncing",
            "busy_projection_fail_closed",
            "crash_takeover",
            "exactly_one_owner",
            "graceful_takeover",
            "takeover_within_90_seconds",
            "transaction_coordinator_follows_index_lease",
        },
        label="same-project takeover assertions",
    )

    multi, _ = _read_json(inputs.multi_project_receipt, label="multi-project receipt")
    require(
        multi.get("schema_version") == "s11-multi-project-receipt/1.0"
        and multi.get("status") == "passed",
        "multi-project receipt did not pass",
    )
    multi_bindings = _mapping(multi.get("bindings"), label="multi-project bindings")
    require(
        multi_bindings.get("package_manifest_sha256") == manifest_digest
        and multi_bindings.get("package_source_commit") == source_commit
        and multi_bindings.get("package_version") == PACKAGE_VERSION,
        "package binding differs for multi-project receipt",
    )
    _all_true(
        multi.get("assertions"),
        {"isolation_matrix_complete", "no_cross_project_leakage", "foreign_ids_rejected", "target_fault_isolated", "source_unchanged"},
        label="multi-project isolation assertions",
    )

    repro, _ = _read_json(inputs.reproducibility_receipt, label="reproducibility receipt")
    require(
        repro.get("schema_version") == "s11-reproducibility-receipt/1.0"
        and repro.get("status") == "passed",
        "reproducibility receipt did not pass",
    )
    repro_bindings = _mapping(repro.get("bindings"), label="reproducibility bindings")
    require(
        repro_bindings.get("qualified_manifest_sha256") == manifest_digest
        and repro_bindings.get("source_commit") == source_commit,
        "package binding differs for reproducibility receipt",
    )
    _all_true(
        repro.get("reproducibility"),
        {"archive_bytes_equal", "manifest_bytes_equal", "package_tree_equal", "source_checkout_unchanged"},
        label="reproducibility assertions",
    )
    return package_live, same_project, multi, repro, same_path


def _validate_host_delta(
    *,
    inputs: TechnicalInputs,
    manifest_digest: str,
    source_commit: str,
    compatibility_matrix_sha256: str,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any], dict[str, Any], dict[str, Any], list[tuple[Path, dict[str, Any]]]]:
    root = inputs.host_delta_directory
    require(root.is_dir() and not root.is_symlink(), "host-delta directory differs")
    measurement_path = root / "measurement.json"
    profile_path = root / "host-coordinate-profile.json"
    bundle_path = root / "surface-compatibility-bundle.json"
    receipt_path = root / "host-delta-receipt.json"
    measurement, measurement_raw = _read_json(measurement_path, label="host measurement")
    profile_document, profile_raw = _read_json(profile_path, label="host profile")
    bundle, bundle_raw = _read_json(bundle_path, label="surface compatibility bundle")
    receipt, _ = _read_json(receipt_path, label="host-delta receipt")
    active, _ = _read_json(inputs.active_compatibility_status, label="active compatibility status")

    require(measurement.get("schema_version") == "s11-host-delta-measurement/1.0", "host measurement schema differs")
    package = _mapping(measurement.get("package"), label="host measurement package")
    require(
        package.get("version") == PACKAGE_VERSION
        and package.get("baseline_matrix_sha256")
        == compatibility_matrix_sha256,
        "host measurement package differs",
    )
    measurement_surfaces = measurement.get("surfaces")
    require(
        isinstance(measurement_surfaces, list)
        and len(measurement_surfaces) == 3
        and {item.get("surface") for item in measurement_surfaces if isinstance(item, dict)} == SURFACES,
        "host measurement surfaces differ",
    )
    assertions = _mapping(measurement.get("assertions"), label="host measurement assertions")
    require(assertions.get("manual_interaction_absent") is True, "manual surface loop was not eliminated")
    require(
        all(assertions.get(field) is True for field in ("package_unchanged", "all_required_surfaces_measured", "signatures_verified")),
        "host measurement assertions differ",
    )
    groups = measurement.get("client_groups")
    require(isinstance(groups, list) and groups, "host measurement client groups differ")
    group_surfaces: set[str] = set()
    client_digests: set[str] = set()
    for group in groups:
        record = _mapping(group, label="host measurement client group")
        digest = record.get("client_artifact_sha256")
        surfaces = record.get("surfaces")
        require(isinstance(digest, str) and host_delta.DIGEST_RE.fullmatch(digest) is not None, "client group digest differs")
        require(isinstance(surfaces, list) and surfaces, "client group surfaces differ")
        require(not group_surfaces.intersection(surfaces), "client group surfaces overlap")
        require(digest not in client_digests, "client group digest is duplicated")
        group_surfaces.update(surfaces)
        client_digests.add(digest)
    require(group_surfaces == SURFACES, "host measurement client groups omit a surface")

    try:
        supported_surfaces = host_delta._surface_map(profile_document, label="supported host profile")
    except host_delta.HostDeltaError as error:
        raise TechnicalAcceptanceError(str(error)) from error
    require(
        all(surface["qualification"] == "supported" for surface in supported_surfaces.values()),
        "all host surfaces must be supported",
    )
    profile_digest = host_delta.sha256_bytes(profile_raw)
    bundle_digest = host_delta.sha256_bytes(bundle_raw)
    baseline_digest = bundle.get("baseline_matrix_sha256")
    require(
        bundle.get("schema_version") == host_delta.BUNDLE_SCHEMA
        and bundle.get("package_version") == PACKAGE_VERSION
        and isinstance(baseline_digest, str)
        and host_delta.DIGEST_RE.fullmatch(baseline_digest) is not None
        and bundle.get("host_coordinate_profile_sha256") == profile_digest,
        "surface compatibility bundle binding differs",
    )
    bundle_surfaces = bundle.get("surfaces")
    require(
        isinstance(bundle_surfaces, list)
        and len(bundle_surfaces) == 3
        and {item.get("surface") for item in bundle_surfaces if isinstance(item, dict)} == SURFACES
        and all(item.get("qualification") == "supported" for item in bundle_surfaces if isinstance(item, dict)),
        "surface compatibility bundle surfaces differ",
    )

    require(
        receipt.get("schema_version") == host_delta.RECEIPT_SCHEMA
        and receipt.get("status") == "passed"
        and receipt.get("outcome") in {"compatible_bundle_issued", "compatible_observed"},
        "host-delta receipt did not pass",
    )
    receipt_bindings = _mapping(receipt.get("bindings"), label="host-delta receipt bindings")
    require(receipt_bindings.get("package_manifest_sha256") == manifest_digest, "host-delta package binding differs")
    require(
        receipt_bindings.get("baseline_matrix_sha256") == baseline_digest
        and receipt_bindings.get("measurement_sha256") == host_delta.sha256_bytes(measurement_raw)
        and receipt_bindings.get("host_profile_sha256") == profile_digest
        and receipt_bindings.get("bundle_sha256") == bundle_digest
        and receipt_bindings.get("contract_projection_sha256") == measurement.get("contract_projection_sha256"),
        "host-delta artifact binding differs",
    )
    receipt_assertions = _mapping(receipt.get("assertions"), label="host-delta assertions")
    require(receipt_assertions.get("manual_interaction_absent") is True, "manual surface loop was not eliminated")
    require(receipt_assertions.get("external_beta_claimed") is False, "host delta claimed External Beta")

    smoke_paths = sorted(root.glob("smoke-*.json"))
    smoke_digest_binding = receipt_bindings.get("smoke_report_sha256")
    require(
        isinstance(smoke_digest_binding, list)
        and len(smoke_paths) == len(smoke_digest_binding)
        and smoke_paths,
        "host smoke receipt binding differs",
    )
    smokes: list[tuple[Path, dict[str, Any]]] = []
    smoke_surfaces: set[str] = set()
    smoke_clients: set[str] = set()
    smoke_digests: list[str] = []
    for path in smoke_paths:
        smoke, _ = _read_json(path, label="host smoke report")
        try:
            host_delta.validate_smoke_report(smoke)
        except host_delta.HostDeltaError as error:
            raise TechnicalAcceptanceError("host smoke did not pass") from error
        smoke_surfaces.update(smoke["surfaces"])
        smoke_clients.add(smoke["client_artifact_sha256"])
        smoke_digests.append(host_delta.sha256_file(path))
        smokes.append((path, smoke))
    require(smoke_digests == smoke_digest_binding, "host smoke report digest binding differs")
    require(
        smoke_surfaces == SURFACES
        and smoke_clients == client_digests
        and len(smokes) == len(client_digests),
        "host smoke surface/client coverage differs",
    )

    try:
        active_record = host_qualify._validate_status(active, package_version=PACKAGE_VERSION)
    except host_qualify.QualificationError as error:
        raise TechnicalAcceptanceError("active compatibility status differs") from error
    receipt_bundle = _mapping(receipt.get("bundle"), label="host-delta bundle receipt")
    require(
        active_record.get("source") == "installed_bundle"
        and active_record.get("bundle_id") == bundle.get("bundle_id") == receipt_bundle.get("bundle_id")
        and active_record.get("sequence") == bundle.get("sequence") == receipt_bundle.get("sequence")
        and active_record.get("bundle_sha256") == bundle_digest
        and active_record.get("host_coordinate_profile_sha256") == profile_digest,
        "active compatibility status differs",
    )
    return measurement, profile_document, bundle, receipt, active_record, smokes


def compose(inputs: TechnicalInputs) -> dict[str, Any]:
    manifest, manifest_raw = _read_json(inputs.package_manifest, label="detached package manifest")
    manifest_digest = host_delta.sha256_bytes(manifest_raw)
    source_commit = manifest.get("source_commit")
    compatibility_matrix_sha256 = manifest.get("compatibility_matrix_sha256")
    require(
        manifest.get("schema_version") == "s11-package-manifest/1.0"
        and manifest.get("package_version") == PACKAGE_VERSION
        and isinstance(source_commit, str)
        and SOURCE_COMMIT_RE.fullmatch(source_commit) is not None,
        "frozen package manifest coordinate differs",
    )
    require(
        isinstance(compatibility_matrix_sha256, str)
        and host_delta.DIGEST_RE.fullmatch(compatibility_matrix_sha256)
        is not None,
        "frozen package compatibility matrix binding differs",
    )
    package_live, same_project, multi, repro, same_path = _validate_package_receipts(
        inputs=inputs,
        manifest_digest=manifest_digest,
        source_commit=source_commit,
    )
    measurement, profile, bundle, receipt, active, smokes = _validate_host_delta(
        inputs=inputs,
        manifest_digest=manifest_digest,
        source_commit=source_commit,
        compatibility_matrix_sha256=compatibility_matrix_sha256,
    )
    report = {
        "schema_version": SCHEMA_VERSION,
        "status": "passed",
        "track": "technical_private_alpha",
        "package": {
            "version": PACKAGE_VERSION,
            "source_commit": source_commit,
            "manifest": _ref(
                inputs.package_manifest,
                manifest,
                source_commit=source_commit,
                manifest_digest=manifest_digest,
                status="bound",
            ),
        },
        "package_evidence": {
            "package_live": _ref(inputs.package_live_receipt, package_live, source_commit=source_commit, manifest_digest=manifest_digest),
            "same_project": _ref(same_path, same_project, source_commit=source_commit, manifest_digest=manifest_digest),
            "multi_project": _ref(inputs.multi_project_receipt, multi, source_commit=source_commit, manifest_digest=manifest_digest),
            "reproducibility": _ref(inputs.reproducibility_receipt, repro, source_commit=source_commit, manifest_digest=manifest_digest),
        },
        "host_compatibility": {
            "measurement": _ref(inputs.host_delta_directory / "measurement.json", measurement, source_commit=source_commit, manifest_digest=manifest_digest, status="measured"),
            "profile": _ref(inputs.host_delta_directory / "host-coordinate-profile.json", profile, source_commit=source_commit, manifest_digest=manifest_digest, status="supported"),
            "bundle": _ref(inputs.host_delta_directory / "surface-compatibility-bundle.json", bundle, source_commit=source_commit, manifest_digest=manifest_digest, status="supported"),
            "receipt": _ref(inputs.host_delta_directory / "host-delta-receipt.json", receipt, source_commit=source_commit, manifest_digest=manifest_digest),
            "active_status": _ref(inputs.active_compatibility_status, active, source_commit=source_commit, manifest_digest=manifest_digest),
            "smokes": [_ref(path, smoke, source_commit=source_commit, manifest_digest=manifest_digest) for path, smoke in smokes],
        },
        "usability": "deferred_unacquired",
        "claims": {
            "technical_private_alpha": True,
            "external_codex_beta": False,
            "commercial_ready": False,
        },
        "assertions": {
            "package_evidence_reused": True,
            "host_delta_applied": True,
            "same_project_takeover_passed": True,
            "multi_project_isolation_passed": True,
            "all_host_clients_smoked": True,
            "manual_surface_loop_not_required": True,
        },
        "redaction": dict(REDACTION),
    }
    return validate_document(report)


def _validate_reference(value: Any, *, source_commit: str, manifest_digest: str) -> Mapping[str, Any]:
    record = _mapping(value, label="evidence reference")
    require(set(record) == REFERENCE_FIELDS, "evidence reference fields differ")
    require(
        isinstance(record.get("path"), str)
        and not record["path"].startswith("/")
        and ".." not in Path(record["path"]).parts,
        "evidence reference path differs",
    )
    require(isinstance(record.get("sha256"), str) and host_delta.DIGEST_RE.fullmatch(record["sha256"]) is not None, "evidence reference digest differs")
    require(
        record.get("source_commit") == source_commit
        and record.get("package_manifest_sha256") == manifest_digest,
        "evidence reference package binding differs",
    )
    require(isinstance(record.get("schema_version"), str) and isinstance(record.get("status"), str), "evidence reference identity differs")
    return record


def validate_document(value: Any) -> dict[str, Any]:
    report = _mapping(value, label="technical evidence")
    require(
        set(report)
        == {"schema_version", "status", "track", "package", "package_evidence", "host_compatibility", "usability", "claims", "assertions", "redaction"},
        "technical evidence fields differ",
    )
    require(
        report.get("schema_version") == SCHEMA_VERSION
        and report.get("status") == "passed"
        and report.get("track") == "technical_private_alpha"
        and report.get("usability") == "deferred_unacquired",
        "technical evidence identity differs",
    )
    package = _mapping(report.get("package"), label="technical package")
    require(set(package) == {"version", "source_commit", "manifest"}, "technical package fields differ")
    source_commit = package.get("source_commit")
    require(package.get("version") == PACKAGE_VERSION and isinstance(source_commit, str) and SOURCE_COMMIT_RE.fullmatch(source_commit) is not None, "technical package coordinate differs")
    manifest = _mapping(package.get("manifest"), label="manifest reference")
    manifest_digest = manifest.get("sha256")
    require(isinstance(manifest_digest, str), "manifest reference digest differs")
    _validate_reference(manifest, source_commit=source_commit, manifest_digest=manifest_digest)
    package_evidence = _mapping(report.get("package_evidence"), label="package evidence")
    require(set(package_evidence) == {"package_live", "same_project", "multi_project", "reproducibility"}, "package evidence fields differ")
    for reference in package_evidence.values():
        _validate_reference(reference, source_commit=source_commit, manifest_digest=manifest_digest)
    host = _mapping(report.get("host_compatibility"), label="host compatibility")
    require(set(host) == {"measurement", "profile", "bundle", "receipt", "active_status", "smokes"}, "host compatibility fields differ")
    for name in ("measurement", "profile", "bundle", "receipt", "active_status"):
        _validate_reference(host[name], source_commit=source_commit, manifest_digest=manifest_digest)
    smokes = host.get("smokes")
    require(isinstance(smokes, list) and 1 <= len(smokes) <= 3, "host smoke references differ")
    for reference in smokes:
        _validate_reference(reference, source_commit=source_commit, manifest_digest=manifest_digest)
    require(
        report.get("claims")
        == {"technical_private_alpha": True, "external_codex_beta": False, "commercial_ready": False},
        "technical claims differ",
    )
    require(
        report.get("assertions")
        == {
            "package_evidence_reused": True,
            "host_delta_applied": True,
            "same_project_takeover_passed": True,
            "multi_project_isolation_passed": True,
            "all_host_clients_smoked": True,
            "manual_surface_loop_not_required": True,
        },
        "technical assertions differ",
    )
    require(report.get("redaction") == REDACTION, "technical evidence redaction differs")
    canonical_json(report)
    return dict(report)


def _path_from_reference(reference: Mapping[str, Any], *, artifact_root: Path) -> Path:
    path = artifact_root / str(reference["path"])
    require(_relative(path, root=artifact_root) == reference["path"], "evidence reference path differs")
    require(host_delta.sha256_file(path, maximum_bytes=MAX_JSON_BYTES) == reference["sha256"], "evidence reference digest differs")
    document, _ = _read_json(path, label="referenced evidence")
    require(document.get("schema_version") == reference["schema_version"], "evidence reference schema differs")
    if reference["status"] not in {"bound", "measured", "supported"}:
        require(document.get("status") == reference["status"], "evidence reference status differs")
    return path


def validate(value: Any, *, artifact_root: Path) -> dict[str, Any]:
    report = validate_document(value)
    root = artifact_root.resolve(strict=True)
    package = report["package"]
    package_evidence = report["package_evidence"]
    host = report["host_compatibility"]
    manifest_path = _path_from_reference(package["manifest"], artifact_root=root)
    package_live_path = _path_from_reference(package_evidence["package_live"], artifact_root=root)
    _path_from_reference(package_evidence["same_project"], artifact_root=root)
    multi_path = _path_from_reference(package_evidence["multi_project"], artifact_root=root)
    repro_path = _path_from_reference(package_evidence["reproducibility"], artifact_root=root)
    receipt_path = _path_from_reference(host["receipt"], artifact_root=root)
    _path_from_reference(host["measurement"], artifact_root=root)
    _path_from_reference(host["profile"], artifact_root=root)
    _path_from_reference(host["bundle"], artifact_root=root)
    active_path = _path_from_reference(host["active_status"], artifact_root=root)
    for reference in host["smokes"]:
        _path_from_reference(reference, artifact_root=root)
    recomposed = compose(
        TechnicalInputs(
            package_manifest=manifest_path,
            package_live_receipt=package_live_path,
            multi_project_receipt=multi_path,
            reproducibility_receipt=repro_path,
            host_delta_directory=receipt_path.parent,
            active_compatibility_status=active_path,
        )
    )
    require(canonical_json(recomposed) == canonical_json(report), "technical evidence composition differs")
    return report


def _publish(path: Path, document: Mapping[str, Any]) -> None:
    require(path.is_absolute(), "technical evidence output must be absolute")
    require(path.parent.is_dir(), "technical evidence output parent differs")
    try:
        host_measure.publish_measurement(path, dict(document))
    except host_measure.HostMeasurementError as error:
        raise TechnicalAcceptanceError(str(error)) from error


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    compose_parser = subparsers.add_parser("compose")
    compose_parser.add_argument("--package-manifest", type=Path, required=True)
    compose_parser.add_argument("--package-live-receipt", type=Path, required=True)
    compose_parser.add_argument("--multi-project-receipt", type=Path, required=True)
    compose_parser.add_argument("--reproducibility-receipt", type=Path, required=True)
    compose_parser.add_argument("--host-delta-directory", type=Path, required=True)
    compose_parser.add_argument("--active-compatibility-status", type=Path, required=True)
    compose_parser.add_argument("--output", type=Path, required=True)
    validate_parser = subparsers.add_parser("validate")
    validate_parser.add_argument("--evidence", type=Path, required=True)
    validate_parser.add_argument("--artifact-root", type=Path, required=True)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    try:
        if args.command == "compose":
            document = compose(
                TechnicalInputs(
                    package_manifest=args.package_manifest,
                    package_live_receipt=args.package_live_receipt,
                    multi_project_receipt=args.multi_project_receipt,
                    reproducibility_receipt=args.reproducibility_receipt,
                    host_delta_directory=args.host_delta_directory,
                    active_compatibility_status=args.active_compatibility_status,
                )
            )
            _publish(args.output, document)
        else:
            document, _ = _read_json(args.evidence, label="technical evidence")
            validate(document, artifact_root=args.artifact_root)
        print(canonical_json({"schema_version": SCHEMA_VERSION, "status": "passed"}).decode("utf-8"))
        return 0
    except TechnicalAcceptanceError as error:
        print(str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
