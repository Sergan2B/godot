from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tests.codex import sprint11_host_delta as host_delta
from tests.codex import sprint11_host_delta_measure as host_measure
from tests.codex import sprint11_host_delta_qualify as qualify
from tests.codex.test_sprint11_host_delta import (
    contract,
    matrix,
    package_manifest,
    passing_smoke,
    profile,
)


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(host_delta.canonical_json(value))


class HostDeltaQualifierTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.artifact_root = self.root / "package"
        self.product_root = self.artifact_root / "share/godot-codex/product"
        self.product_root.mkdir(parents=True)
        self.matrix = matrix()
        self.matrix_path = self.product_root / "compatibility-matrix.v1.json"
        write_json(self.matrix_path, self.matrix)
        matrix_digest = host_delta.sha256_file(self.matrix_path)
        self.embedded_profile = profile(app_version="1.0.0")
        self.embedded_profile["compatibility_matrix"]["sha256"] = matrix_digest
        write_json(
            self.product_root / "host-coordinate-profile.v1.json",
            self.embedded_profile,
        )
        registry_source = (
            Path(__file__).resolve().parents[2]
            / "godot-codex-mcp/product/registry-profile.v1.json"
        )
        (self.product_root / "registry-profile.v1.json").write_bytes(
            registry_source.read_bytes()
        )
        self.manifest = package_manifest()
        self.manifest["compatibility_matrix_sha256"] = matrix_digest
        self.manifest_path = self.root / "sprint11-package-manifest.json"
        write_json(self.manifest_path, self.manifest)
        self.previous_profile = copy.deepcopy(self.embedded_profile)
        self.previous_profile["profile_id"] = "previous-supported-profile"
        for surface in self.previous_profile["surfaces"]:
            surface["qualification"] = "supported"
        self.previous_profile_path = self.root / "previous-profile.json"
        write_json(self.previous_profile_path, self.previous_profile)
        assessment = host_delta.DeltaAssessment(
            delta_class=host_delta.DeltaClass.COORDINATE_ONLY,
            affected_surfaces=("app",),
            required_probes=(),
            reasons=("fixture",),
        )
        self.previous_receipt = host_delta.build_host_delta_receipt(
            assessment=assessment,
            outcome="compatible_bundle_issued",
            bindings={
                "package_manifest_sha256": "sha256:" + "1" * 64,
                "baseline_matrix_sha256": "sha256:" + "2" * 64,
                "previous_profile_sha256": None,
                "measurement_sha256": "sha256:" + "3" * 64,
                "host_profile_sha256": host_delta.sha256_file(self.previous_profile_path),
                "bundle_sha256": "sha256:" + "4" * 64,
                "contract_projection_sha256": "sha256:" + "5" * 64,
                "smoke_report_sha256": ["sha256:" + "6" * 64],
            },
            bundle={
                "bundle_id": "previous-bundle",
                "sequence": 3,
                "qualification": "supported",
            },
        )
        self.previous_receipt_path = self.root / "previous-receipt.json"
        write_json(self.previous_receipt_path, self.previous_receipt)
        self.launcher = self.root / "godot-codex"
        self.launcher.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.launcher.chmod(0o700)
        self.app_client = self.root / "app-codex"
        self.app_client.write_bytes(b"app client")
        self.app_client.chmod(0o700)
        self.ide_client = self.root / "ide-codex"
        self.ide_client.write_bytes(b"ide client")
        self.ide_client.chmod(0o700)
        app_digest = host_delta.sha256_file(self.app_client)
        ide_digest = host_delta.sha256_file(self.ide_client)
        for document in (self.embedded_profile, self.previous_profile):
            document["surfaces"][0]["client_artifact_sha256"] = app_digest
            document["surfaces"][1]["client_artifact_sha256"] = app_digest
            document["surfaces"][2]["client_artifact_sha256"] = ide_digest
        write_json(
            self.product_root / "host-coordinate-profile.v1.json",
            self.embedded_profile,
        )
        write_json(self.previous_profile_path, self.previous_profile)
        self.previous_receipt["bindings"]["host_profile_sha256"] = (
            host_delta.sha256_file(self.previous_profile_path)
        )
        write_json(self.previous_receipt_path, self.previous_receipt)
        self.host_directories: dict[str, Path] = {}
        for name in ("app-bundle", "vscode-bundle", "extension"):
            path = self.root / name
            path.mkdir()
            self.host_directories[name] = path
        self.host_files: dict[str, Path] = {}
        for name in ("app-executable", "vscode-executable", "extension-package"):
            path = self.root / name
            path.write_bytes(name.encode())
            if name != "extension-package":
                path.chmod(0o700)
            self.host_files[name] = path
        self.projects: dict[str, Path] = {}
        for index, surface in enumerate(("app", "cli", "ide"), start=1):
            project = self.root / f"project-{surface}"
            (project / ".godot/codex").mkdir(parents=True)
            write_json(
                project / ".godot/codex/setup-receipt-v1.json",
                {"project_id": "project:sha256:" + str(index) * 64},
            )
            (project / "project.godot").write_text("[application]\n", encoding="utf-8")
            self.projects[surface] = project
        self.smoke_calls: list[object] = []
        self.launcher_calls: list[tuple[str, ...]] = []
        self.active = False
        self.generated: dict[str, object] = {}

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def options(
        self,
        *,
        apply: bool,
        previous_receipt: bool = True,
    ) -> qualify.QualificationOptions:
        return qualify.QualificationOptions(
            package_manifest=self.manifest_path,
            artifact_root=self.artifact_root,
            installed_launcher=self.launcher,
            previous_profile=self.previous_profile_path,
            previous_receipt=self.previous_receipt_path if previous_receipt else None,
            app_bundle=self.host_directories["app-bundle"],
            app_executable=self.host_files["app-executable"],
            app_client=self.app_client,
            cli_client=self.app_client,
            vscode_bundle=self.host_directories["vscode-bundle"],
            vscode_executable=self.host_files["vscode-executable"],
            extension_root=self.host_directories["extension"],
            extension_package_json=self.host_files["extension-package"],
            ide_client=self.ide_client,
            projects=self.projects,
            output=self.root / "acquisition",
            timeout_seconds=30.0,
            apply=apply,
            targeted_interaction_report=None,
        )

    def fake_measure(
        self,
        options: host_measure.MeasureOptions,
        *,
        app_version: str = "2.0.0",
        contract_digest: str = "sha256:" + "5" * 64,
    ) -> dict[str, object]:
        surfaces = copy.deepcopy(self.embedded_profile["surfaces"])
        surfaces[0]["host_version"] = app_version
        app_digest = qualify.safe_file_digest(options.app_client, label="app client")[0]
        ide_digest = qualify.safe_file_digest(options.ide_client, label="ide client")[0]
        surfaces[0]["client_artifact_sha256"] = app_digest
        surfaces[1]["client_artifact_sha256"] = app_digest
        surfaces[2]["client_artifact_sha256"] = ide_digest
        document: dict[str, object] = {
            "schema_version": "s11-host-delta-measurement/1.0",
            "target": {"os": "macos", "architecture": "arm64"},
            "package": {
                "version": self.matrix["package"]["version"],
                "baseline_matrix_sha256": host_delta.sha256_file(self.matrix_path),
            },
            "surfaces": surfaces,
            "client_groups": [
                {"client_artifact_sha256": app_digest, "surfaces": ["app", "cli"]},
                {"client_artifact_sha256": ide_digest, "surfaces": ["ide"]},
            ],
            "feature_projection": {"complete": True},
            "app_server_projection": {"complete": True},
            "contract_projection_sha256": contract_digest,
            "commands": [],
            "assertions": {"manual_interaction_absent": True},
            "redaction": {"absolute_paths_absent": True},
        }
        host_measure.publish_measurement(options.output, document)
        return document

    def fake_smoke(self, options: object) -> dict[str, object]:
        self.smoke_calls.append(options)
        report = passing_smoke(options.expected_client_sha256)
        report["surfaces"] = list(options.surfaces)
        host_measure.publish_measurement(options.output, report)
        return report

    def fake_launcher(self, launcher: Path, arguments: tuple[str, ...], timeout: float) -> dict[str, object]:
        del launcher, timeout
        self.launcher_calls.append(arguments)
        if arguments == ("compatibility", "status", "--json"):
            if self.active:
                return {
                    "schema_version": "godot-codex-surface-compatibility-status/1.0",
                    "status": "ready",
                    "package_version": "0.1.19",
                    "source": "installed_bundle",
                    **self.generated,
                }
            return {
                "schema_version": "godot-codex-surface-compatibility-status/1.0",
                "status": "ready",
                "package_version": "0.1.19",
                "source": "installed_bundle",
                "effective_matrix_sha256": "sha256:" + "7" * 64,
                "bundle_id": "previous-bundle",
                "sequence": 3,
                "bundle_sha256": "sha256:" + "4" * 64,
                "host_coordinate_profile_sha256": host_delta.sha256_file(
                    self.previous_profile_path
                ),
            }
        if "--dry-run" in arguments:
            bundle_path = Path(arguments[arguments.index("--bundle") + 1])
            profile_path = Path(arguments[arguments.index("--host-profile") + 1])
            bundle = json.loads(bundle_path.read_text(encoding="utf-8"))
            bundle_digest = host_delta.sha256_file(bundle_path)
            profile_digest = host_delta.sha256_file(profile_path)
            self.generated = {
                "effective_matrix_sha256": "sha256:" + "8" * 64,
                "bundle_id": bundle["bundle_id"],
                "sequence": bundle["sequence"],
                "bundle_sha256": bundle_digest,
                "host_coordinate_profile_sha256": profile_digest,
            }
            return {
                "schema_version": "godot-codex-surface-compatibility-preview/1.0",
                "status": "planned",
                "plan_digest": "sha256:" + "9" * 64,
                "expires_in_seconds": 600,
                "package_version": "0.1.19",
                "baseline_matrix_sha256": bundle["baseline_matrix_sha256"],
                "previous_sequence": 3,
                **{key: self.generated[key] for key in (
                    "bundle_id",
                    "sequence",
                    "bundle_sha256",
                    "host_coordinate_profile_sha256",
                )},
            }
        if arguments[:2] == ("compatibility", "install") and "--apply-plan" in arguments:
            self.active = True
            return {
                "schema_version": "godot-codex-surface-compatibility-report/1.0",
                "status": "installed",
                "plan_digest": "sha256:" + "9" * 64,
                "package_version": "0.1.19",
                **{key: self.generated[key] for key in (
                    "bundle_id",
                    "sequence",
                    "bundle_sha256",
                    "host_coordinate_profile_sha256",
                )},
            }
        raise AssertionError(arguments)

    def run_with_fakes(self, options: qualify.QualificationOptions, *, measure_fn: object | None = None) -> dict[str, object]:
        with mock.patch.object(qualify, "validate_package_artifacts", return_value=None), mock.patch.object(
            qualify.host_measure,
            "measure_host_delta",
            side_effect=measure_fn or self.fake_measure,
        ), mock.patch.object(
            qualify.app_server,
            "run_client_smoke",
            side_effect=self.fake_smoke,
        ), mock.patch.object(
            qualify,
            "run_launcher_json",
            side_effect=self.fake_launcher,
        ):
            return qualify.qualify(options)

    def test_coordinate_only_runs_one_smoke_per_distinct_client_and_applies(self) -> None:
        result = self.run_with_fakes(self.options(apply=True))
        self.assertEqual(result["outcome"], "compatible_bundle_applied")
        self.assertEqual(len(self.smoke_calls), 2)
        self.assertEqual(result["sequence"], 4)
        self.assertEqual(
            [call[:2] for call in self.launcher_calls],
            [
                ("compatibility", "status"),
                ("compatibility", "install"),
                ("compatibility", "install"),
                ("compatibility", "status"),
            ],
        )
        command_text = " ".join(" ".join(call) for call in self.launcher_calls)
        self.assertNotIn(" setup ", f" {command_text} ")
        self.assertNotIn(" install.sh ", f" {command_text} ")
        self.assertTrue((self.options(apply=True).output / "host-delta-receipt.json").is_file())

    def test_bootstrap_runs_all_smokes_and_issues_bundle(self) -> None:
        result = self.run_with_fakes(self.options(apply=False, previous_receipt=False))
        self.assertEqual(result["outcome"], "compatible_bundle_issued")
        self.assertEqual(len(self.smoke_calls), 2)

    def test_interaction_sensitive_change_stops_before_smoke_or_launcher(self) -> None:
        def changed(options: host_measure.MeasureOptions) -> dict[str, object]:
            return self.fake_measure(options, contract_digest="sha256:" + "a" * 64)

        with self.assertRaisesRegex(qualify.QualificationError, "targeted_surface_check_required"):
            self.run_with_fakes(self.options(apply=True), measure_fn=changed)
        self.assertEqual(self.smoke_calls, [])
        self.assertEqual(self.launcher_calls, [])

    def test_product_contract_change_never_invokes_smoke_or_launcher(self) -> None:
        self.manifest["package_version"] = "0.1.20"
        write_json(self.manifest_path, self.manifest)
        with self.assertRaisesRegex(qualify.QualificationError, "full_package_qualification_required"):
            self.run_with_fakes(self.options(apply=True))
        self.assertEqual(self.smoke_calls, [])
        self.assertEqual(self.launcher_calls, [])

    def test_unchanged_active_coordinate_is_observed_without_install(self) -> None:
        def unchanged(options: host_measure.MeasureOptions) -> dict[str, object]:
            return self.fake_measure(options, app_version="1.0.0")

        result = self.run_with_fakes(self.options(apply=True), measure_fn=unchanged)
        self.assertEqual(result["outcome"], "compatible_observed")
        self.assertEqual(len(self.smoke_calls), 2)
        self.assertEqual(self.launcher_calls, [("compatibility", "status", "--json")])

    def test_preview_failure_never_publishes_success_directory(self) -> None:
        def failing_launcher(launcher: Path, arguments: tuple[str, ...], timeout: float) -> dict[str, object]:
            if "--dry-run" in arguments:
                raise qualify.QualificationError("preview failed")
            return self.fake_launcher(launcher, arguments, timeout)

        options = self.options(apply=True)
        with mock.patch.object(qualify, "validate_package_artifacts", return_value=None), mock.patch.object(
            qualify.host_measure,
            "measure_host_delta",
            side_effect=self.fake_measure,
        ), mock.patch.object(
            qualify.app_server,
            "run_client_smoke",
            side_effect=self.fake_smoke,
        ), mock.patch.object(
            qualify,
            "run_launcher_json",
            side_effect=failing_launcher,
        ):
            with self.assertRaisesRegex(qualify.QualificationError, "preview"):
                qualify.qualify(options)
        self.assertFalse(options.output.exists())

    def test_apply_failure_preserves_private_failure_evidence_without_success(self) -> None:
        def failing_launcher(launcher: Path, arguments: tuple[str, ...], timeout: float) -> dict[str, object]:
            if "--apply-plan" in arguments:
                raise qualify.QualificationError("apply failed")
            return self.fake_launcher(launcher, arguments, timeout)

        options = self.options(apply=True)
        with mock.patch.object(qualify, "validate_package_artifacts", return_value=None), mock.patch.object(
            qualify.host_measure,
            "measure_host_delta",
            side_effect=self.fake_measure,
        ), mock.patch.object(
            qualify.app_server,
            "run_client_smoke",
            side_effect=self.fake_smoke,
        ), mock.patch.object(
            qualify,
            "run_launcher_json",
            side_effect=failing_launcher,
        ):
            with self.assertRaisesRegex(qualify.QualificationError, "apply"):
                qualify.qualify(options)
        self.assertFalse(options.output.exists())
        failures = list(self.root.glob(".acquisition.failed-*"))
        self.assertEqual(len(failures), 1)
        failure = json.loads((failures[0] / "failure-report.json").read_text(encoding="utf-8"))
        self.assertFalse(failure["active_state_claimed"])

    def test_existing_output_is_rejected_before_any_command(self) -> None:
        options = self.options(apply=True)
        options.output.mkdir()
        with self.assertRaisesRegex(qualify.QualificationError, "already exists"):
            self.run_with_fakes(options)
        self.assertEqual(self.smoke_calls, [])
        self.assertEqual(self.launcher_calls, [])


if __name__ == "__main__":
    unittest.main()
