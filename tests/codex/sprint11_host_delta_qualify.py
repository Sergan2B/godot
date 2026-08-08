#!/usr/bin/env python3
"""One-command qualification for rolling Codex App/CLI/IDE host deltas."""

from __future__ import annotations

import argparse
import ctypes
import dataclasses
import json
import os
import re
import shutil
import sys
import tempfile
import uuid
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any, Final

try:
    from tests.codex import sprint11_acceptance
    from tests.codex import sprint11_host_app_server as app_server
    from tests.codex import sprint11_host_delta as host_delta
    from tests.codex import sprint11_host_delta_measure as host_measure
    from tests.codex.sprint11_host_provenance import (
        HostProvenanceError,
        _canonical_nonsymlink_path,
    )
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint11_acceptance
    import sprint11_host_app_server as app_server
    import sprint11_host_delta as host_delta
    import sprint11_host_delta_measure as host_measure
    from sprint11_host_provenance import (
        HostProvenanceError,
        _canonical_nonsymlink_path,
    )


PRODUCT_RELATIVE: Final = Path("share/godot-codex/product")
MATRIX_NAME: Final = "compatibility-matrix.v1.json"
PROFILE_NAME: Final = "host-coordinate-profile.v1.json"
REGISTRY_NAME: Final = "registry-profile.v1.json"
MAX_JSON_BYTES: Final = 2 * 1024 * 1024
MAX_TIMEOUT_SECONDS: Final = 180.0
MIN_TIMEOUT_SECONDS: Final = 5.0
SURFACES: Final = ("app", "cli", "ide")
COMMAND_SCHEMA: Final = "s11-host-delta-command/1.0"
TARGETED_SCHEMA: Final = "s11-host-targeted-interaction/1.0"


class QualificationError(RuntimeError):
    """The host delta could not authorize an active compatibility coordinate."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise QualificationError(message)


@dataclasses.dataclass(frozen=True)
class QualificationOptions:
    package_manifest: Path
    artifact_root: Path
    installed_launcher: Path
    previous_profile: Path | None
    previous_receipt: Path | None
    app_bundle: Path
    app_executable: Path
    app_client: Path
    cli_client: Path
    vscode_bundle: Path
    vscode_executable: Path
    extension_root: Path
    extension_package_json: Path
    ide_client: Path
    projects: Mapping[str, Path]
    output: Path
    timeout_seconds: float = 90.0
    apply: bool = False
    targeted_interaction_report: Path | None = None


def canonical_json(value: Any) -> bytes:
    try:
        return host_delta.canonical_json(value)
    except host_delta.HostDeltaError as error:
        raise QualificationError(str(error)) from error


def safe_file_digest(path: Path, *, label: str) -> tuple[str, int]:
    try:
        return host_measure.safe_file_digest(path, label=label)
    except host_measure.HostMeasurementError as error:
        raise QualificationError(str(error)) from error


def _canonical_path(path: Path, *, kind: str) -> Path:
    try:
        return _canonical_nonsymlink_path(path, kind=kind)
    except HostProvenanceError as error:
        raise QualificationError(str(error)) from error


def _read_bytes(path: Path, *, label: str, maximum: int = MAX_JSON_BYTES) -> bytes:
    path = _canonical_path(path, kind=label)
    digest, size = safe_file_digest(path, label=label)
    require(size <= maximum, f"{label} exceeds its byte bound")
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise QualificationError(f"{label} cannot be read") from error
    require(
        len(raw) == size and host_delta.sha256_bytes(raw) == digest,
        f"{label} changed while reading",
    )
    return raw


def _read_json(path: Path, *, label: str) -> tuple[dict[str, Any], bytes]:
    raw = _read_bytes(path, label=label)
    try:
        value = host_delta.strict_json_bytes(
            raw,
            maximum_bytes=MAX_JSON_BYTES,
            label=label,
        )
    except host_delta.HostDeltaError as error:
        raise QualificationError(str(error)) from error
    return value, raw


def validate_package_artifacts(
    manifest: Mapping[str, Any],
    *,
    manifest_path: Path,
    artifact_root: Path,
) -> None:
    """Run the package's detached-content verification without install/setup."""

    source_commit = manifest.get("source_commit")
    require(isinstance(source_commit, str), "package source commit differs")
    try:
        sprint11_acceptance.validate_detached_package_manifest(
            dict(manifest),
            manifest_relative_path=manifest_path.name,
            artifact_root=artifact_root,
            expected_source_commit=source_commit,
        )
    except sprint11_acceptance.AcceptanceError as error:
        raise QualificationError(str(error)) from error


def _validate_options(options: QualificationOptions) -> QualificationOptions:
    require(
        isinstance(options.timeout_seconds, (int, float))
        and not isinstance(options.timeout_seconds, bool)
        and MIN_TIMEOUT_SECONDS <= options.timeout_seconds <= MAX_TIMEOUT_SECONDS,
        "qualification timeout differs",
    )
    require(set(options.projects) == set(SURFACES), "qualification project surfaces differ")
    require(options.output.is_absolute(), "qualification output path must be absolute")
    require(not options.output.exists() and not options.output.is_symlink(), "qualification output already exists")
    canonical: dict[str, Path] = {}
    for field, kind in (
        ("package_manifest", "package manifest"),
        ("artifact_root", "package artifact root"),
        ("installed_launcher", "installed package launcher"),
        ("app_bundle", "Codex app bundle"),
        ("app_executable", "Codex app executable"),
        ("app_client", "Codex app client"),
        ("cli_client", "Codex CLI client"),
        ("vscode_bundle", "VS Code bundle"),
        ("vscode_executable", "VS Code executable"),
        ("extension_root", "Codex IDE extension root"),
        ("extension_package_json", "Codex IDE package metadata"),
        ("ide_client", "Codex IDE client"),
    ):
        canonical[field] = _canonical_path(getattr(options, field), kind=kind)
    previous_profile = (
        _canonical_path(options.previous_profile, kind="previous host profile")
        if options.previous_profile is not None
        else None
    )
    previous_receipt = (
        _canonical_path(options.previous_receipt, kind="previous host receipt")
        if options.previous_receipt is not None
        else None
    )
    targeted = (
        _canonical_path(options.targeted_interaction_report, kind="targeted interaction report")
        if options.targeted_interaction_report is not None
        else None
    )
    projects = {
        surface: _canonical_path(options.projects[surface], kind=f"{surface} qualification project")
        for surface in SURFACES
    }
    output_parent = _canonical_path(options.output.parent, kind="qualification output parent")
    require(output_parent.is_dir(), "qualification output parent differs")
    return dataclasses.replace(
        options,
        **canonical,
        previous_profile=previous_profile,
        previous_receipt=previous_receipt,
        targeted_interaction_report=targeted,
        projects=projects,
    )


def run_launcher_json(
    launcher: Path,
    arguments: tuple[str, ...],
    timeout: float,
) -> dict[str, Any]:
    require(
        arguments
        and arguments[0] == "compatibility"
        and "install" not in arguments[:1],
        "launcher command is outside compatibility scope",
    )
    operation = "compatibility_" + re.sub(r"[^a-z0-9]+", "_", "_".join(arguments[1:3])).strip("_")
    try:
        result = host_measure.run_bounded_command(
            (str(launcher), *arguments),
            timeout,
            operation[:64],
        )
    except host_measure.HostMeasurementError as error:
        raise QualificationError(str(error)) from error
    require(result.returncode == 0, "package compatibility command failed")
    try:
        return host_delta.strict_json_bytes(
            result.stdout,
            maximum_bytes=host_measure.MAX_COMMAND_BYTES,
            label="package compatibility response",
        )
    except host_delta.HostDeltaError as error:
        raise QualificationError(str(error)) from error


def _validate_status(value: Mapping[str, Any], *, package_version: str) -> dict[str, Any]:
    fields = {
        "schema_version",
        "status",
        "package_version",
        "source",
        "effective_matrix_sha256",
        "bundle_id",
        "sequence",
        "bundle_sha256",
        "host_coordinate_profile_sha256",
    }
    require(set(value) == fields, "compatibility status fields differ")
    require(
        value["schema_version"] == "godot-codex-surface-compatibility-status/1.0"
        and value["status"] == "ready"
        and value["package_version"] == package_version
        and value["source"] in {"embedded", "installed_bundle"},
        "compatibility status coordinate differs",
    )
    require(
        isinstance(value["effective_matrix_sha256"], str)
        and host_delta.DIGEST_RE.fullmatch(value["effective_matrix_sha256"]) is not None,
        "effective matrix digest differs",
    )
    if value["source"] == "embedded":
        require(
            all(value[field] is None for field in ("bundle_id", "sequence", "bundle_sha256", "host_coordinate_profile_sha256")),
            "embedded compatibility status differs",
        )
    else:
        require(
            isinstance(value["bundle_id"], str)
            and isinstance(value["sequence"], int)
            and 1 <= value["sequence"] <= host_delta.MAX_SAFE_SEQUENCE
            and isinstance(value["bundle_sha256"], str)
            and host_delta.DIGEST_RE.fullmatch(value["bundle_sha256"]) is not None
            and isinstance(value["host_coordinate_profile_sha256"], str)
            and host_delta.DIGEST_RE.fullmatch(value["host_coordinate_profile_sha256"]) is not None,
            "installed compatibility status differs",
        )
    return dict(value)


def _project_id(project: Path) -> str:
    receipt_path = project / ".godot/codex/setup-receipt-v1.json"
    receipt, _raw = _read_json(receipt_path, label="project setup receipt")
    project_id = receipt.get("project_id")
    require(
        isinstance(project_id, str)
        and re.fullmatch(r"project:sha256:[0-9a-f]{64}", project_id) is not None,
        "project setup receipt identity differs",
    )
    return project_id


def _write_private(path: Path, payload: bytes) -> None:
    require(not path.exists() and not path.is_symlink(), "acquisition artifact already exists")
    descriptor: int | None = None
    try:
        descriptor = os.open(
            path,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0),
            0o600,
        )
        view = memoryview(payload)
        while view:
            written = os.write(descriptor, view)
            require(written > 0, "acquisition artifact write stalled")
            view = view[written:]
        os.fsync(descriptor)
    except OSError as error:
        raise QualificationError("acquisition artifact cannot be written") from error
    finally:
        if descriptor is not None:
            os.close(descriptor)


def _write_json(path: Path, value: Mapping[str, Any]) -> str:
    payload = canonical_json(value)
    _write_private(path, payload)
    return host_delta.sha256_bytes(payload)


def _write_digest(path: Path, digest: str) -> None:
    require(host_delta.DIGEST_RE.fullmatch(digest) is not None, "artifact digest differs")
    _write_private(path, (digest + "\n").encode("ascii"))


def _sync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _rename_exclusive(source: Path, destination: Path) -> None:
    require(not destination.exists(), "qualification output already exists")
    if sys.platform == "darwin":
        libc = ctypes.CDLL(None, use_errno=True)
        renamex_np = libc.renamex_np
        renamex_np.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        renamex_np.restype = ctypes.c_int
        if renamex_np(os.fsencode(source), os.fsencode(destination), 0x00000004) != 0:
            error_number = ctypes.get_errno()
            raise QualificationError(f"qualification output cannot be finalized: errno {error_number}")
    else:
        require(not destination.exists(), "qualification output already exists")
        os.rename(source, destination)
    _sync_directory(destination.parent)


def _validate_targeted_report(
    path: Path,
    *,
    previous_contract: str,
    current_contract: str,
    affected_surfaces: Sequence[str],
) -> None:
    report, _raw = _read_json(path, label="targeted interaction report")
    require(
        set(report)
        == {
            "schema_version",
            "status",
            "previous_contract_sha256",
            "current_contract_sha256",
            "affected_surfaces",
            "assertions",
        }
        and report["schema_version"] == TARGETED_SCHEMA
        and report["status"] == "passed"
        and report["previous_contract_sha256"] == previous_contract
        and report["current_contract_sha256"] == current_contract
        and report["affected_surfaces"] == list(affected_surfaces)
        and report["assertions"]
        == {
            "model_turn_absent": True,
            "manual_interaction_absent": True,
            "project_integrity_preserved": True,
        },
        "targeted_surface_check_required",
    )


def _validate_smoke_binding(
    report: Mapping[str, Any],
    *,
    client_digest: str,
    surfaces: tuple[str, ...],
) -> None:
    try:
        host_delta.validate_smoke_report(report)
    except host_delta.HostDeltaError as error:
        raise QualificationError(str(error)) from error
    require(
        report["client_artifact_sha256"] == client_digest
        and report["surfaces"] == list(surfaces),
        "host smoke client binding differs",
    )


def _validate_previous_receipt(value: Mapping[str, Any]) -> tuple[str, dict[str, Any]]:
    require(
        value.get("schema_version") == host_delta.RECEIPT_SCHEMA
        and value.get("status") == "passed"
        and isinstance(value.get("bindings"), dict)
        and isinstance(value.get("bundle"), dict),
        "previous host delta receipt differs",
    )
    contract_digest = value["bindings"].get("contract_projection_sha256")
    require(
        isinstance(contract_digest, str)
        and host_delta.DIGEST_RE.fullmatch(contract_digest) is not None,
        "previous interaction contract differs",
    )
    return contract_digest, dict(value)


def _validate_preview(
    preview: Mapping[str, Any],
    *,
    package_version: str,
    bundle: Mapping[str, Any],
    bundle_sha256: str,
    profile_sha256: str,
    previous_sequence: int | None,
) -> str:
    require(
        preview.get("schema_version") == "godot-codex-surface-compatibility-preview/1.0"
        and preview.get("status") == "planned"
        and preview.get("package_version") == package_version
        and preview.get("bundle_id") == bundle["bundle_id"]
        and preview.get("sequence") == bundle["sequence"]
        and preview.get("bundle_sha256") == bundle_sha256
        and preview.get("baseline_matrix_sha256") == bundle["baseline_matrix_sha256"]
        and preview.get("host_coordinate_profile_sha256") == profile_sha256
        and preview.get("previous_sequence") == previous_sequence,
        "compatibility preview differs",
    )
    plan_digest = preview.get("plan_digest")
    require(
        isinstance(plan_digest, str)
        and host_delta.DIGEST_RE.fullmatch(plan_digest) is not None,
        "compatibility preview plan digest differs",
    )
    return plan_digest


def _validate_apply_report(
    report: Mapping[str, Any],
    *,
    plan_digest: str,
    package_version: str,
    bundle: Mapping[str, Any],
    bundle_sha256: str,
    profile_sha256: str,
) -> None:
    require(
        report.get("schema_version") == "godot-codex-surface-compatibility-report/1.0"
        and report.get("status") in {"installed", "already_installed"}
        and report.get("plan_digest") == plan_digest
        and report.get("package_version") == package_version
        and report.get("bundle_id") == bundle["bundle_id"]
        and report.get("sequence") == bundle["sequence"]
        and report.get("bundle_sha256") == bundle_sha256
        and report.get("host_coordinate_profile_sha256") == profile_sha256,
        "compatibility apply report differs",
    )


def _validate_active_status(
    status: Mapping[str, Any],
    *,
    package_version: str,
    bundle: Mapping[str, Any],
    bundle_sha256: str,
    profile_sha256: str,
) -> None:
    active = _validate_status(status, package_version=package_version)
    require(
        active["source"] == "installed_bundle"
        and active["bundle_id"] == bundle["bundle_id"]
        and active["sequence"] == bundle["sequence"]
        and active["bundle_sha256"] == bundle_sha256
        and active["host_coordinate_profile_sha256"] == profile_sha256,
        "active compatibility status differs",
    )


def qualify(options: QualificationOptions) -> dict[str, Any]:
    options = _validate_options(options)
    staging = Path(
        tempfile.mkdtemp(
            prefix=f".{options.output.name}.staging-",
            dir=options.output.parent,
        )
    ).resolve()
    os.chmod(staging, 0o700)
    finalized = False
    apply_started = False
    try:
        manifest, manifest_raw = _read_json(options.package_manifest, label="package manifest")
        validate_package_artifacts(
            manifest,
            manifest_path=options.package_manifest,
            artifact_root=options.artifact_root,
        )
        package_version = manifest.get("package_version")
        require(package_version == "0.1.19", "full_package_qualification_required")
        product_root = options.artifact_root / PRODUCT_RELATIVE
        matrix_path = _canonical_path(product_root / MATRIX_NAME, kind="embedded compatibility matrix")
        embedded_profile_path = _canonical_path(product_root / PROFILE_NAME, kind="embedded host profile")
        registry_path = _canonical_path(product_root / REGISTRY_NAME, kind="embedded registry profile")
        matrix, matrix_raw = _read_json(matrix_path, label="embedded compatibility matrix")
        embedded_profile, _embedded_profile_raw = _read_json(
            embedded_profile_path,
            label="embedded host profile",
        )
        matrix_raw_digest = host_delta.sha256_bytes(matrix_raw)
        require(
            manifest.get("compatibility_matrix_sha256") == matrix_raw_digest
            and embedded_profile.get("compatibility_matrix", {}).get("sha256") == matrix_raw_digest,
            "full_package_qualification_required",
        )
        previous_profile: dict[str, Any] | None = None
        previous_profile_raw: bytes | None = None
        previous_profile_sha256: str | None = None
        if options.previous_profile is not None:
            previous_profile, previous_profile_raw = _read_json(
                options.previous_profile,
                label="previous host profile",
            )
            previous_profile_sha256 = host_delta.sha256_bytes(previous_profile_raw)
        previous_receipt: dict[str, Any] | None = None
        previous_contract: dict[str, Any] | None = None
        previous_contract_digest: str | None = None
        if options.previous_receipt is not None:
            raw_receipt, _receipt_bytes = _read_json(
                options.previous_receipt,
                label="previous host delta receipt",
            )
            previous_contract_digest, previous_receipt = _validate_previous_receipt(raw_receipt)
            previous_contract = {
                "contract_projection_sha256": previous_contract_digest,
                "complete": True,
            }

        measurement_path = staging / "measurement.json"
        measurement = host_measure.measure_host_delta(
            host_measure.MeasureOptions(
                app_bundle=options.app_bundle,
                app_executable=options.app_executable,
                app_client=options.app_client,
                cli_client=options.cli_client,
                vscode_bundle=options.vscode_bundle,
                vscode_executable=options.vscode_executable,
                extension_root=options.extension_root,
                extension_package_json=options.extension_package_json,
                ide_client=options.ide_client,
                embedded_profile=embedded_profile_path,
                embedded_matrix=matrix_path,
                output=measurement_path,
                timeout_seconds=options.timeout_seconds,
            )
        )
        require(
            measurement.get("schema_version") == host_measure.MEASUREMENT_SCHEMA
            and measurement.get("target") == {"os": "macos", "architecture": "arm64"}
            and measurement.get("package", {}).get("version") == package_version
            and isinstance(measurement.get("surfaces"), list)
            and isinstance(measurement.get("client_groups"), list)
            and measurement.get("feature_projection", {}).get("complete") is True
            and measurement.get("app_server_projection", {}).get("complete") is True
            and isinstance(measurement.get("contract_projection_sha256"), str)
            and host_delta.DIGEST_RE.fullmatch(measurement["contract_projection_sha256"]) is not None,
            "host measurement differs",
        )
        comparison_qualification = "supported" if previous_receipt is not None else "candidate"
        try:
            current_profile = host_delta.build_host_profile(
                embedded_profile=embedded_profile,
                measured_surfaces=measurement["surfaces"],
                qualification=comparison_qualification,
                profile_id="host-delta-comparison-profile",
            )
        except host_delta.HostDeltaError as error:
            raise QualificationError(str(error)) from error
        assessment = host_delta.classify_delta(
            package_manifest=manifest,
            embedded_matrix=matrix,
            previous_profile=previous_profile,
            current_profile=current_profile,
            previous_contract=previous_contract,
            current_contract={
                "contract_projection_sha256": measurement["contract_projection_sha256"],
                "complete": True,
            },
        )
        if assessment.delta_class in {
            host_delta.DeltaClass.PRODUCT_CONTRACT,
            host_delta.DeltaClass.UNCLASSIFIABLE,
        }:
            raise QualificationError("full_package_qualification_required")
        if assessment.delta_class == host_delta.DeltaClass.SURFACE_STRUCTURAL:
            raise QualificationError(
                "targeted_surface_check_required:" + ",".join(assessment.affected_surfaces)
            )
        bootstrap = previous_receipt is None
        if assessment.delta_class == host_delta.DeltaClass.INTERACTION_SENSITIVE:
            if bootstrap:
                assessment = host_delta.satisfy_assessment(
                    assessment,
                    passed_probes={"interaction_contract"},
                )
            else:
                require(
                    options.targeted_interaction_report is not None
                    and previous_contract_digest is not None,
                    "targeted_surface_check_required",
                )
                _validate_targeted_report(
                    options.targeted_interaction_report,
                    previous_contract=previous_contract_digest,
                    current_contract=measurement["contract_projection_sha256"],
                    affected_surfaces=assessment.affected_surfaces,
                )
                assessment = host_delta.satisfy_assessment(
                    assessment,
                    passed_probes={"interaction_contract"},
                )

        client_paths = {
            "app": options.app_client,
            "cli": options.cli_client,
            "ide": options.ide_client,
        }
        smoke_digests: list[str] = []
        seen_client_digests: set[str] = set()
        grouped_surfaces: dict[str, str] = {}
        measured_surface_digests = {
            surface["surface"]: surface["client_artifact_sha256"]
            for surface in measurement["surfaces"]
            if isinstance(surface, dict)
            and surface.get("surface") in SURFACES
            and isinstance(surface.get("client_artifact_sha256"), str)
        }
        require(set(measured_surface_digests) == set(SURFACES), "host measurement surfaces differ")
        groups = measurement["client_groups"]
        for index, group in enumerate(groups, start=1):
            require(
                isinstance(group, dict)
                and set(group) == {"client_artifact_sha256", "surfaces"}
                and isinstance(group["surfaces"], list)
                and group["surfaces"],
                "host measurement client group differs",
            )
            client_digest = group["client_artifact_sha256"]
            surfaces = tuple(group["surfaces"])
            require(
                isinstance(client_digest, str)
                and host_delta.DIGEST_RE.fullmatch(client_digest) is not None
                and client_digest not in seen_client_digests
                and len(surfaces) == len(set(surfaces))
                and all(surface in SURFACES for surface in surfaces)
                and all(surface not in grouped_surfaces for surface in surfaces)
                and all(measured_surface_digests[surface] == client_digest for surface in surfaces),
                "host measurement client group differs",
            )
            seen_client_digests.add(client_digest)
            grouped_surfaces.update({surface: client_digest for surface in surfaces})
            surface = surfaces[0]
            client_path = client_paths[surface]
            require(
                safe_file_digest(client_path, label="measured Codex client")[0] == client_digest,
                "host measurement client artifact changed",
            )
            smoke_path = staging / f"smoke-{index}.json"
            smoke = app_server.run_client_smoke(
                app_server.SmokeOptions(
                    client=client_path,
                    expected_client_sha256=client_digest,
                    launcher=options.installed_launcher,
                    project_root=options.projects[surface],
                    expected_project_id=_project_id(options.projects[surface]),
                    expected_registry=registry_path,
                    output=smoke_path,
                    surfaces=surfaces,
                    timeout_seconds=options.timeout_seconds,
                )
            )
            _validate_smoke_binding(smoke, client_digest=client_digest, surfaces=surfaces)
            smoke_digests.append(host_delta.sha256_file(smoke_path))
        require(
            set(grouped_surfaces) == set(SURFACES)
            and seen_client_digests == set(measured_surface_digests.values()),
            "host measurement did not probe every distinct client",
        )
        assessment = host_delta.satisfy_assessment(
            assessment,
            passed_probes={"host_smoke"},
        )
        require(not assessment.required_probes, "targeted_surface_check_required")

        status = _validate_status(
            run_launcher_json(
                options.installed_launcher,
                ("compatibility", "status", "--json"),
                options.timeout_seconds,
            ),
            package_version=package_version,
        )
        if previous_receipt is not None:
            require(
                previous_profile_sha256 is not None
                and status["source"] == "installed_bundle"
                and status["bundle_id"] == previous_receipt["bundle"]["bundle_id"]
                and status["sequence"] == previous_receipt["bundle"]["sequence"]
                and status["bundle_sha256"] == previous_receipt["bindings"]["bundle_sha256"]
                and status["host_coordinate_profile_sha256"] == previous_profile_sha256,
                "previous host qualification is not the active coordinate",
            )
        measurement_sha256 = host_delta.sha256_file(measurement_path)
        manifest_sha256 = host_delta.sha256_bytes(manifest_raw)
        no_op = (
            previous_receipt is not None
            and previous_profile is not None
            and previous_profile_raw is not None
            and not assessment.affected_surfaces
        )
        if no_op:
            profile_path = staging / "host-coordinate-profile.json"
            _write_private(profile_path, previous_profile_raw)
            profile_sha256 = previous_profile_sha256
            bundle_sha256 = status["bundle_sha256"]
            bundle_record = {
                "bundle_id": status["bundle_id"],
                "sequence": status["sequence"],
                "qualification": "supported",
            }
            receipt = host_delta.build_host_delta_receipt(
                assessment=assessment,
                outcome="compatible_observed",
                bindings={
                    "package_manifest_sha256": manifest_sha256,
                    "baseline_matrix_sha256": previous_receipt["bindings"]["baseline_matrix_sha256"],
                    "previous_profile_sha256": previous_profile_sha256,
                    "measurement_sha256": measurement_sha256,
                    "host_profile_sha256": profile_sha256,
                    "bundle_sha256": bundle_sha256,
                    "contract_projection_sha256": measurement["contract_projection_sha256"],
                    "smoke_report_sha256": smoke_digests,
                },
                bundle=bundle_record,
            )
            _write_json(staging / "host-delta-receipt.json", receipt)
            summary = {
                "schema_version": COMMAND_SCHEMA,
                "status": "passed",
                "outcome": "compatible_observed",
                "delta_class": assessment.delta_class.value,
                "bundle_id": bundle_record["bundle_id"],
                "sequence": bundle_record["sequence"],
            }
        else:
            previous_sequence = status["sequence"] if status["source"] == "installed_bundle" else None
            sequence = 1 if previous_sequence is None else previous_sequence + 1
            require(sequence <= host_delta.MAX_SAFE_SEQUENCE, "compatibility sequence is exhausted")
            profile = host_delta.build_host_profile(
                embedded_profile=embedded_profile,
                measured_surfaces=measurement["surfaces"],
                qualification="supported",
                profile_id=f"host-delta-supported-{sequence}",
            )
            profile_path = staging / "host-coordinate-profile.json"
            profile_sha256 = _write_json(profile_path, profile)
            _write_digest(staging / "host-coordinate-profile.sha256", profile_sha256)
            bundle = host_delta.build_surface_bundle(
                assessment=assessment,
                matrix=matrix,
                profile=profile,
                profile_sha256=profile_sha256,
                sequence=sequence,
            )
            bundle_path = staging / "surface-compatibility-bundle.json"
            bundle_sha256 = _write_json(bundle_path, bundle)
            _write_digest(staging / "surface-compatibility-bundle.sha256", bundle_sha256)
            outcome = "compatible_bundle_issued"
            if options.apply:
                preview = run_launcher_json(
                    options.installed_launcher,
                    (
                        "compatibility",
                        "install",
                        "--bundle",
                        str(bundle_path),
                        "--host-profile",
                        str(profile_path),
                        "--expected-sha256",
                        bundle_sha256,
                        "--dry-run",
                        "--json",
                    ),
                    options.timeout_seconds,
                )
                plan_digest = _validate_preview(
                    preview,
                    package_version=package_version,
                    bundle=bundle,
                    bundle_sha256=bundle_sha256,
                    profile_sha256=profile_sha256,
                    previous_sequence=previous_sequence,
                )
                _write_json(staging / "compatibility-preview.json", preview)
                _sync_directory(staging)
                apply_started = True
                apply_report = run_launcher_json(
                    options.installed_launcher,
                    ("compatibility", "install", "--apply-plan", plan_digest, "--json"),
                    options.timeout_seconds,
                )
                _validate_apply_report(
                    apply_report,
                    plan_digest=plan_digest,
                    package_version=package_version,
                    bundle=bundle,
                    bundle_sha256=bundle_sha256,
                    profile_sha256=profile_sha256,
                )
                _write_json(staging / "compatibility-apply-report.json", apply_report)
                active_status = run_launcher_json(
                    options.installed_launcher,
                    ("compatibility", "status", "--json"),
                    options.timeout_seconds,
                )
                _validate_active_status(
                    active_status,
                    package_version=package_version,
                    bundle=bundle,
                    bundle_sha256=bundle_sha256,
                    profile_sha256=profile_sha256,
                )
                _write_json(staging / "compatibility-active-status.json", active_status)
                outcome = "compatible_bundle_applied"
            receipt = host_delta.build_host_delta_receipt(
                assessment=assessment,
                outcome="compatible_bundle_issued",
                bindings={
                    "package_manifest_sha256": manifest_sha256,
                    "baseline_matrix_sha256": bundle["baseline_matrix_sha256"],
                    "previous_profile_sha256": previous_profile_sha256,
                    "measurement_sha256": measurement_sha256,
                    "host_profile_sha256": profile_sha256,
                    "bundle_sha256": bundle_sha256,
                    "contract_projection_sha256": measurement["contract_projection_sha256"],
                    "smoke_report_sha256": smoke_digests,
                },
                bundle={
                    "bundle_id": bundle["bundle_id"],
                    "sequence": bundle["sequence"],
                    "qualification": "supported",
                },
            )
            _write_json(staging / "host-delta-receipt.json", receipt)
            summary = {
                "schema_version": COMMAND_SCHEMA,
                "status": "passed",
                "outcome": outcome,
                "delta_class": assessment.delta_class.value,
                "bundle_id": bundle["bundle_id"],
                "sequence": bundle["sequence"],
            }
        _sync_directory(staging)
        _rename_exclusive(staging, options.output)
        finalized = True
        return summary
    except BaseException as error:
        if apply_started and staging.exists():
            try:
                failure = {
                    "schema_version": "s11-host-delta-failure/1.0",
                    "status": "failed",
                    "active_state_claimed": False,
                    "error_code": type(error).__name__,
                }
                _write_json(staging / "failure-report.json", failure)
                _sync_directory(staging)
                failed = options.output.parent / f".{options.output.name}.failed-{uuid.uuid4().hex}"
                _rename_exclusive(staging, failed)
                finalized = True
            except BaseException:
                pass
        if isinstance(error, QualificationError):
            raise
        if isinstance(error, (host_delta.HostDeltaError, host_measure.HostMeasurementError, app_server.AppServerProtocolError)):
            raise QualificationError(str(error)) from error
        raise
    finally:
        if not finalized and staging.exists():
            shutil.rmtree(staging, ignore_errors=True)


def _absolute_path(value: str) -> Path:
    path = Path(value)
    if not path.is_absolute():
        raise argparse.ArgumentTypeError("path must be absolute")
    return path


def _optional_path(value: str) -> Path | None:
    if value == "none":
        return None
    return _absolute_path(value)


def _project(value: str) -> tuple[str, Path]:
    surface, separator, raw_path = value.partition("=")
    if separator != "=" or surface not in SURFACES:
        raise argparse.ArgumentTypeError("project must be app=, cli=, or ide= with an absolute path")
    return surface, _absolute_path(raw_path)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-manifest", type=_absolute_path, required=True)
    parser.add_argument("--artifact-root", type=_absolute_path, required=True)
    parser.add_argument("--installed-launcher", type=_absolute_path, required=True)
    parser.add_argument("--previous-profile", type=_optional_path, required=True)
    parser.add_argument("--previous-receipt", type=_optional_path, required=True)
    parser.add_argument("--app-bundle", type=_absolute_path, required=True)
    parser.add_argument("--app-executable", type=_absolute_path, required=True)
    parser.add_argument("--app-client", type=_absolute_path, required=True)
    parser.add_argument("--cli-client", type=_absolute_path, required=True)
    parser.add_argument("--vscode-bundle", type=_absolute_path, required=True)
    parser.add_argument("--vscode-executable", type=_absolute_path, required=True)
    parser.add_argument("--extension-root", type=_absolute_path, required=True)
    parser.add_argument("--extension-package-json", type=_absolute_path, required=True)
    parser.add_argument("--ide-client", type=_absolute_path, required=True)
    parser.add_argument("--project", type=_project, action="append", required=True)
    parser.add_argument("--targeted-interaction-report", type=_optional_path, default=None)
    parser.add_argument("--output", type=_absolute_path, required=True)
    parser.add_argument("--timeout", type=float, default=90.0)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--json", action="store_true")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    projects: dict[str, Path] = {}
    for surface, path in arguments.project:
        if surface in projects:
            _parser().error(f"duplicate --project {surface}")
        projects[surface] = path
    try:
        result = qualify(
            QualificationOptions(
                package_manifest=arguments.package_manifest,
                artifact_root=arguments.artifact_root,
                installed_launcher=arguments.installed_launcher,
                previous_profile=arguments.previous_profile,
                previous_receipt=arguments.previous_receipt,
                app_bundle=arguments.app_bundle,
                app_executable=arguments.app_executable,
                app_client=arguments.app_client,
                cli_client=arguments.cli_client,
                vscode_bundle=arguments.vscode_bundle,
                vscode_executable=arguments.vscode_executable,
                extension_root=arguments.extension_root,
                extension_package_json=arguments.extension_package_json,
                ide_client=arguments.ide_client,
                projects=projects,
                output=arguments.output,
                timeout_seconds=arguments.timeout,
                apply=arguments.apply,
                targeted_interaction_report=arguments.targeted_interaction_report,
            )
        )
    except QualificationError as error:
        print(json.dumps({"status": "failed", "error": str(error)}, sort_keys=True), file=sys.stderr)
        return 2
    if arguments.json:
        print(json.dumps(result, separators=(",", ":"), sort_keys=True))
    else:
        print(
            f"{result['outcome']} {result['bundle_id']} sequence={result['sequence']}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
