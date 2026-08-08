from __future__ import annotations

import copy
import json
import unittest
from pathlib import Path
from typing import Any

from tests.codex import sprint11_host_delta as host_delta


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SCHEMA_ROOT = REPOSITORY_ROOT / "tests/codex/schemas"


def digest(character: str) -> str:
    return "sha256:" + character * 64


def signature(
    *,
    identifier: str,
    team_id: str,
    cdhash: str,
    mode: str = "strict",
) -> dict[str, str]:
    return {
        "mode": mode,
        "identifier": identifier,
        "team_id": team_id,
        "cdhash": cdhash,
    }


def surface(
    name: str,
    *,
    version: str = "1.0.0",
    client_digest: str | None = None,
) -> dict[str, Any]:
    client_digest = client_digest or digest({"app": "a", "cli": "a", "ide": "b"}[name])
    client_signature = signature(
        identifier="codex",
        team_id="2DC432GLL2",
        cdhash={"app": "a", "cli": "a", "ide": "b"}[name] * 40,
    )
    if name == "app":
        return {
            "surface": "app",
            "host_name": "codex-desktop",
            "host_identifier": "com.openai.codex",
            "host_artifact_kind": "macos_bundle_executable",
            "host_version": version,
            "host_build": "100",
            "host_commit": None,
            "host_artifact_sha256": digest("c"),
            "host_metadata_sha256": None,
            "host_code_signature": signature(
                identifier="com.openai.codex",
                team_id="2DC432GLL2",
                cdhash="c" * 40,
                mode="deep_strict",
            ),
            "client_version": "0.147.0-alpha.1",
            "client_artifact_sha256": client_digest,
            "client_code_signature": client_signature,
            "ide_host_version": None,
            "ide_shell_identifier": None,
            "ide_shell_artifact_sha256": None,
            "ide_shell_team_id": None,
            "ide_shell_code_signature": None,
            "qualification": "candidate",
        }
    if name == "cli":
        return {
            "surface": "cli",
            "host_name": "codex-cli",
            "host_identifier": "codex-cli",
            "host_artifact_kind": "standalone_executable",
            "host_version": version,
            "host_build": version,
            "host_commit": None,
            "host_artifact_sha256": client_digest,
            "host_metadata_sha256": None,
            "host_code_signature": client_signature,
            "client_version": version,
            "client_artifact_sha256": client_digest,
            "client_code_signature": client_signature,
            "ide_host_version": None,
            "ide_shell_identifier": None,
            "ide_shell_artifact_sha256": None,
            "ide_shell_team_id": None,
            "ide_shell_code_signature": None,
            "qualification": "candidate",
        }
    return {
        "surface": "ide",
        "host_name": "openai.chatgpt",
        "host_identifier": "openai.chatgpt",
        "host_artifact_kind": "extension_tree",
        "host_version": version,
        "host_build": version,
        "host_commit": None,
        "host_artifact_sha256": digest("d"),
        "host_metadata_sha256": digest("e"),
        "host_code_signature": None,
        "client_version": "0.146.0-alpha.1",
        "client_artifact_sha256": client_digest,
        "client_code_signature": client_signature,
        "ide_host_version": "1.131.0",
        "ide_shell_identifier": "com.microsoft.VSCode",
        "ide_shell_artifact_sha256": digest("f"),
        "ide_shell_team_id": "UBF8T346G9",
        "ide_shell_code_signature": signature(
            identifier="com.microsoft.VSCode",
            team_id="UBF8T346G9",
            cdhash="f" * 40,
            mode="deep_strict",
        ),
        "qualification": "candidate",
    }


def profile(*, app_version: str = "1.0.0") -> dict[str, Any]:
    return {
        "schema_version": "godot-codex-host-coordinate-profile/1.0",
        "profile_id": "fixture-hosts-v1",
        "compatibility_matrix": {
            "path": "godot-codex-mcp/product/compatibility-matrix.v1.json",
            "sha256": digest("1"),
        },
        "server_instructions": {
            "path": "godot-codex-mcp/product/server-instructions.v1.txt",
            "file_sha256": digest("2"),
            "wire_sha256": digest("3"),
        },
        "surfaces": [
            surface("app", version=app_version),
            surface("cli"),
            surface("ide"),
        ],
    }


def matrix(*, package_version: str = "0.1.19") -> dict[str, Any]:
    return {
        "schema_version": "godot-codex-compatibility-matrix/1.0",
        "matrix_id": "fixture-matrix-v1",
        "package": {
            "version": package_version,
            "target": {"os": "macos", "architecture": "arm64"},
        },
        "godot": {"build_id": "fixture"},
        "protocols": {"mcp_protocol": "2025-11-25"},
        "schemas": {"index": "1.3"},
        "registry": {"digest": "1" * 64, "tool_count": 41},
        "bridge_profiles": [],
        "surfaces": [],
    }


def package_manifest(*, package_version: str = "0.1.19") -> dict[str, Any]:
    return {
        "schema_version": "s11-package-manifest/1.0",
        "package_version": package_version,
        "compatibility_matrix_sha256": digest("1"),
    }


def contract(value: str, *, complete: bool = True) -> dict[str, Any]:
    return {
        "contract_projection_sha256": value,
        "complete": complete,
    }


def passing_smoke(client_digest: str = digest("a")) -> dict[str, Any]:
    return {
        "schema_version": "s11-host-smoke/1.0",
        "status": "passed",
        "client_artifact_sha256": client_digest,
        "surfaces": ["app", "cli"],
        "protocol": "2025-11-25",
        "registry": {
            "digest": "1" * 64,
            "tools": 41,
            "fixed_resources": 4,
            "resource_templates": 1,
        },
        "connection": {
            "schema_version": "godot-connection-status/1.1",
            "status": "ready",
            "bridge_protocol": "1.8",
            "static_cache": "online_current",
            "project_scope_sha256": digest("4"),
        },
        "semantic_fact": {
            "freshness": "current",
            "evidence_sha256": digest("5"),
        },
        "assertions": {
            "model_turn_absent": True,
            "project_integrity_preserved": True,
            "clean_shutdown": True,
        },
        "redaction": {
            "absolute_paths_absent": True,
            "account_identity_absent": True,
            "artifact_content_absent": True,
            "environment_secrets_absent": True,
        },
    }


class HostDeltaModelTests(unittest.TestCase):
    def test_coordinate_only_reuses_unchanged_product_and_contract(self) -> None:
        assessment = host_delta.classify_delta(
            package_manifest=package_manifest(),
            embedded_matrix=matrix(),
            previous_profile=profile(app_version="1.0.0"),
            current_profile=profile(app_version="1.1.0"),
            previous_contract=contract(digest("6")),
            current_contract=contract(digest("6")),
        )
        self.assertEqual(
            assessment.delta_class,
            host_delta.DeltaClass.COORDINATE_ONLY,
        )
        self.assertEqual(assessment.affected_surfaces, ("app",))
        self.assertEqual(assessment.required_probes, ("host_smoke",))

    def test_interaction_digest_change_is_targeted(self) -> None:
        assessment = host_delta.classify_delta(
            package_manifest=package_manifest(),
            embedded_matrix=matrix(),
            previous_profile=profile(),
            current_profile=profile(),
            previous_contract=contract(digest("6")),
            current_contract=contract(digest("7")),
        )
        self.assertEqual(
            assessment.delta_class,
            host_delta.DeltaClass.INTERACTION_SENSITIVE,
        )
        self.assertEqual(
            assessment.required_probes,
            ("host_smoke", "interaction_contract"),
        )

    def test_missing_previous_contract_is_a_bounded_bootstrap(self) -> None:
        assessment = host_delta.classify_delta(
            package_manifest=package_manifest(),
            embedded_matrix=matrix(),
            previous_profile=profile(),
            current_profile=profile(),
            previous_contract=None,
            current_contract=contract(digest("7")),
        )
        self.assertEqual(
            assessment.delta_class,
            host_delta.DeltaClass.INTERACTION_SENSITIVE,
        )
        self.assertIn("bootstrap", assessment.reasons)

    def test_incomplete_bootstrap_is_unclassifiable(self) -> None:
        assessment = host_delta.classify_delta(
            package_manifest=package_manifest(),
            embedded_matrix=matrix(),
            previous_profile=None,
            current_profile=profile(),
            previous_contract=None,
            current_contract=contract(digest("7"), complete=False),
        )
        self.assertEqual(
            assessment.delta_class,
            host_delta.DeltaClass.UNCLASSIFIABLE,
        )

    def test_product_change_has_priority(self) -> None:
        assessment = host_delta.classify_delta(
            package_manifest=package_manifest(package_version="0.1.20"),
            embedded_matrix=matrix(),
            previous_profile=profile(),
            current_profile=profile(),
            previous_contract=contract(digest("6")),
            current_contract=contract(digest("6")),
        )
        self.assertEqual(
            assessment.delta_class,
            host_delta.DeltaClass.PRODUCT_CONTRACT,
        )

    def test_surface_identity_change_is_structural(self) -> None:
        changed = profile()
        changed["surfaces"][0]["host_identifier"] = "com.example.repacked"
        assessment = host_delta.classify_delta(
            package_manifest=package_manifest(),
            embedded_matrix=matrix(),
            previous_profile=profile(),
            current_profile=changed,
            previous_contract=contract(digest("6")),
            current_contract=contract(digest("6")),
        )
        self.assertEqual(
            assessment.delta_class,
            host_delta.DeltaClass.SURFACE_STRUCTURAL,
        )
        self.assertEqual(assessment.affected_surfaces, ("app",))

    def test_duplicate_or_missing_surface_is_unclassifiable(self) -> None:
        for current in (
            {**profile(), "surfaces": profile()["surfaces"][:2]},
            {
                **profile(),
                "surfaces": [
                    profile()["surfaces"][0],
                    profile()["surfaces"][0],
                    profile()["surfaces"][2],
                ],
            },
        ):
            with self.subTest(current=current):
                assessment = host_delta.classify_delta(
                    package_manifest=package_manifest(),
                    embedded_matrix=matrix(),
                    previous_profile=profile(),
                    current_profile=current,
                    previous_contract=contract(digest("6")),
                    current_contract=contract(digest("6")),
                )
                self.assertEqual(
                    assessment.delta_class,
                    host_delta.DeltaClass.UNCLASSIFIABLE,
                )

    def test_satisfied_assessment_can_build_exact_bundle(self) -> None:
        current = profile(app_version="1.1.0")
        supported = host_delta.build_host_profile(
            embedded_profile=profile(),
            measured_surfaces=current["surfaces"],
            qualification="supported",
            profile_id="host-delta-profile-1",
        )
        assessment = host_delta.classify_delta(
            package_manifest=package_manifest(),
            embedded_matrix=matrix(),
            previous_profile=profile(),
            current_profile=current,
            previous_contract=contract(digest("6")),
            current_contract=contract(digest("6")),
        )
        satisfied = host_delta.satisfy_assessment(
            assessment,
            passed_probes={"host_smoke"},
        )
        bundle = host_delta.build_surface_bundle(
            assessment=satisfied,
            matrix=matrix(),
            profile=supported,
            profile_sha256=digest("8"),
            sequence=4,
        )
        self.assertEqual(bundle["schema_version"], "godot-codex-surface-compatibility-bundle/1.0")
        self.assertEqual(bundle["bundle_id"], "host-delta-4-888888888888")
        self.assertEqual(bundle["sequence"], 4)
        self.assertEqual(
            {item["surface"] for item in bundle["surfaces"]},
            {"app", "cli", "ide"},
        )
        self.assertEqual(
            {item["qualification"] for item in bundle["surfaces"]},
            {"supported"},
        )

    def test_unsatisfied_or_product_assessment_cannot_build_bundle(self) -> None:
        coordinate = host_delta.classify_delta(
            package_manifest=package_manifest(),
            embedded_matrix=matrix(),
            previous_profile=profile(),
            current_profile=profile(app_version="1.1.0"),
            previous_contract=contract(digest("6")),
            current_contract=contract(digest("6")),
        )
        product = copy.copy(coordinate)
        object.__setattr__(product, "delta_class", host_delta.DeltaClass.PRODUCT_CONTRACT)
        supported = host_delta.build_host_profile(
            embedded_profile=profile(),
            measured_surfaces=profile()["surfaces"],
            qualification="supported",
            profile_id="host-delta-profile-1",
        )
        for assessment in (coordinate, product):
            with self.subTest(assessment=assessment):
                with self.assertRaises(host_delta.HostDeltaError):
                    host_delta.build_surface_bundle(
                        assessment=assessment,
                        matrix=matrix(),
                        profile=supported,
                        profile_sha256=digest("8"),
                        sequence=1,
                    )

    def test_sequence_and_digest_are_strict(self) -> None:
        supported = host_delta.build_host_profile(
            embedded_profile=profile(),
            measured_surfaces=profile()["surfaces"],
            qualification="supported",
            profile_id="host-delta-profile-1",
        )
        assessment = host_delta.DeltaAssessment(
            delta_class=host_delta.DeltaClass.COORDINATE_ONLY,
            affected_surfaces=(),
            required_probes=(),
            reasons=(),
        )
        for sequence, profile_digest in (
            (0, digest("8")),
            (2**53, digest("8")),
            (1, "8" * 64),
        ):
            with self.subTest(sequence=sequence, profile_digest=profile_digest):
                with self.assertRaises(host_delta.HostDeltaError):
                    host_delta.build_surface_bundle(
                        assessment=assessment,
                        matrix=matrix(),
                        profile=supported,
                        profile_sha256=profile_digest,
                        sequence=sequence,
                    )

    def test_smoke_validator_rejects_unknown_or_failed_fields(self) -> None:
        host_delta.validate_smoke_report(passing_smoke())
        unknown = {**passing_smoke(), "private_path": "/tmp/secret"}
        failed = {**passing_smoke(), "status": "failed"}
        for value in (unknown, failed):
            with self.subTest(value=value):
                with self.assertRaises(host_delta.HostDeltaError):
                    host_delta.validate_smoke_report(value)

    def test_schema_roots_are_closed_and_share_exact_enums(self) -> None:
        for name in (
            "sprint11-host-delta-measurement.schema.json",
            "sprint11-host-smoke.schema.json",
            "sprint11-host-delta-receipt.schema.json",
        ):
            schema = json.loads((SCHEMA_ROOT / name).read_text(encoding="utf-8"))
            self.assertFalse(schema["additionalProperties"])
            self.assertEqual(schema["type"], "object")
        receipt = json.loads(
            (SCHEMA_ROOT / "sprint11-host-delta-receipt.schema.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(
            receipt["properties"]["delta_class"]["enum"],
            [
                "coordinate_only",
                "interaction_sensitive",
                "surface_structural",
                "product_contract",
                "unclassifiable",
            ],
        )
        self.assertEqual(
            receipt["properties"]["outcome"]["enum"],
            [
                "compatible_observed",
                "compatible_bundle_issued",
                "targeted_surface_check_required",
                "full_package_qualification_required",
            ],
        )


if __name__ == "__main__":
    unittest.main()
