from __future__ import annotations

import copy
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

from tests.codex import sprint11_acceptance as acceptance
from tests.codex.test_sprint11_multi_project import (
    valid_report as valid_multi_project_report,
)
from tests.codex.usability import test_human_acquisition_kit as human_fixtures


def digest(character: str) -> str:
    return "sha256:" + character * 64


def registry_projection() -> dict[str, Any]:
    profile = acceptance.canonical_registry_profile()
    return {
        "tools": profile["tools"],
        "fixed_resources": profile["fixed_resources"],
        "resource_templates": profile["resource_templates"],
        "instructions_sha256": acceptance.canonical_server_instructions()[
            "wire_sha256"
        ],
    }


def synthetic_trace(
    surface: str,
    seed: str,
    revision_base: int,
) -> dict[str, Any]:
    editor_session = f"editor-session:{seed}:one"
    restarted_editor_session = f"editor-session:{seed}:two"
    runtime_before = f"runtime-session:{seed}:one"
    runtime_after = f"runtime-session:{seed}:two"
    transaction_id = f"transaction:{seed}:one"
    report_id = f"validation-report:{seed}:one"
    preview = {
        "editor_session_id": editor_session,
        "coordinates": {
            "event_seq": revision_base,
            "scene_revision": revision_base,
            "operation_seq": revision_base,
        },
        "operations": [
            {
                "kind": "create_node",
                "parent_scene_node_id": "scene-node:fixture-root",
                "godot_type": "Node",
                "name_redacted": True,
                "name_digest": digest("d"),
            }
        ],
        "risk": "low",
        "scope": "change_set.atomic",
    }
    preview_digest = acceptance.sha256_bytes(acceptance.canonical_json(preview))
    projections: dict[str, dict[str, Any]] = {
        "approval.accept": {
            "transaction_id": transaction_id,
            "action": "accept",
            "state": "committed",
        },
        "approval.cancel": {
            "transaction_id": f"transaction:{seed}:cancel",
            "action": "cancel",
            "state": "not_applied",
        },
        "approval.decline": {
            "transaction_id": f"transaction:{seed}:decline",
            "action": "decline",
            "state": "not_applied",
        },
        "approval.timeout": {
            "transaction_id": f"transaction:{seed}:timeout",
            "action": "timeout",
            "state": "not_applied",
        },
        "connection.status": {
            "project_id": "project:fixture",
            "state": "ready",
            "remediation_id": "none",
        },
        "host.approval_layers": {
            "sandbox_approval": "observed",
            "mcp_approval": "action_only_form",
            "independent": True,
        },
        "host.config_reload": {
            "config_reloaded": True,
            "root_rebound": True,
            "restart_required": False,
        },
        "host.launcher": {
            "path": "bin/godot-codex-mcp",
            "sha256": digest("8"),
            "path_lookup_used": False,
            "private_absolute_path_recorded": False,
        },
        "host.offline_status": {
            "state": "offline",
            "freshness": "offline_cached",
            "live_state_claimed": False,
            "remediation_id": "start_matching_editor",
        },
        "multi_project.reject": {
            "project_id": "project:fixture",
            "foreign_project_id": "project:other-fixture",
            "error": {
                "code": "project_binding_mismatch",
                "retryable": False,
                "remediation_id": "select_bound_project",
            },
        },
        "offline.saved_query": {
            "project_id": "project:fixture",
            "scene_id": "scene:res://main.tscn",
            "freshness": "offline_cached",
            "cursor": f"cursor:{seed}:offline",
        },
        "runtime.error_stack": {
            "runtime_session_before": runtime_before,
            "runtime_session_after": runtime_after,
            "runtime_node_id": f"runtime-node:{seed}:player",
            "stack_frame_id": f"stack-frame:{seed}:one",
            "error_code": "intentional_fixture_error",
            "source": {
                "path": "res://scripts/runtime_error.gd",
                "line": 12,
            },
        },
        "saved.current_scene": {
            "project_id": "project:fixture",
            "scene_id": "scene:res://main.tscn",
            "scene_node_id": "scene-node:fixture-root",
            "evidence_id": "evidence:persistent:main-root",
            "fact_code": "current_scene.root_type",
            "fact_value_redacted": True,
            "fact_value_digest": digest("e"),
            "confidence": "authoritative",
            "freshness": "current",
        },
        "transaction.apply": {
            "editor_session_id": editor_session,
            "transaction_id": transaction_id,
            "event_seq": revision_base + 1,
            "scene_revision": revision_base + 1,
            "operation_seq": revision_base + 1,
            "state": "committed",
        },
        "transaction.preview": {
            "transaction_id": transaction_id,
            "preview": preview,
            "preview_digest": preview_digest,
            "risk": "low",
            "scope": "change_set.atomic",
        },
        "transaction.undo": {
            "editor_session_id": editor_session,
            "transaction_id": transaction_id,
            "event_seq": revision_base + 2,
            "scene_revision": revision_base + 2,
            "operation_seq": revision_base + 2,
            "state": "undone",
        },
        "validation.result": {
            "transaction_id": transaction_id,
            "validation_report_id": report_id,
            "outcome": "passed",
            "diagnostics": [],
        },
    }
    return {
        "schema_version": "s11-surface-trace/1.0",
        "capture_kind": "synthetic_contract_fixture",
        "status": "fixture_valid",
        "surface": surface,
        "host": {
            "name": f"synthetic-{surface}",
            "identifier": f"synthetic-{surface}",
            "artifact_kind": "synthetic_fixture",
            "version": "0.0-fixture",
            "build": f"contract-{surface}",
            "commit": None,
            "client_version": "0.0-fixture",
            "host_metadata_sha256": None,
            "host_code_signature": None,
            "client_code_signature": None,
            "ide_host_version": None,
            "ide_shell_identifier": None,
            "ide_shell_artifact_sha256": None,
            "ide_shell_team_id": None,
            "ide_shell_code_signature": None,
            "architecture": "arm64",
        },
        "bindings": {
            "package_source_commit": "a" * 40,
            "package_manifest_sha256": digest("1"),
            "compatibility_matrix_sha256": acceptance.sha256_file(
                acceptance.COMPATIBILITY_MATRIX_PATH
            ),
            "host_coordinate_profile_sha256": acceptance.sha256_file(
                acceptance.HOST_PROFILE_PATH
            ),
            "project_fixture_sha256": digest("2"),
            "registry_sha256": digest("3"),
            "prompt_pack_sha256": digest("4"),
            "host_artifact_sha256": digest(
                {"app": "5", "cli": "6", "ide": "7"}[surface]
            ),
            "client_artifact_sha256": digest(
                {"app": "b", "cli": "c", "ide": "d"}[surface]
            ),
            "host_provenance_sha256": digest("e"),
            "mcp_binary_sha256": digest("8"),
            "godot_artifact_sha256": (
                acceptance.EXACT_GODOT_PREREQUISITE_SHA256
            ),
            "recorder_journal_sha256": digest("9"),
            "recorder_event_chain_sha256": digest("a"),
            "recorder_event_count": 1,
        },
        "registry": registry_projection(),
        "assertions": [
            {
                "assertion_id": assertion_id,
                "projection": projections[assertion_id],
            }
            for assertion_id in sorted(projections)
        ],
        "form_outcomes": [
            {
                "scenario": scenario,
                "action": scenario,
                "semantic_state": (
                    "committed" if scenario == "accept" else "not_applied"
                ),
                "mutation_observed": scenario == "accept",
                "content_recorded": False,
            }
            for scenario in ("accept", "cancel", "decline", "timeout")
        ],
        "revision_timeline": [
            {
                "step": "initial",
                "editor_session_id": editor_session,
                "event_seq": revision_base,
                "scene_revision": revision_base,
                "operation_seq": revision_base,
            },
            {
                "step": "prepared",
                "editor_session_id": editor_session,
                "event_seq": revision_base,
                "scene_revision": revision_base,
                "operation_seq": revision_base,
            },
            {
                "step": "applied",
                "editor_session_id": editor_session,
                "event_seq": revision_base + 1,
                "scene_revision": revision_base + 1,
                "operation_seq": revision_base + 1,
            },
            {
                "step": "restarted",
                "editor_session_id": restarted_editor_session,
                "event_seq": revision_base * 3,
                "scene_revision": revision_base * 4,
                "operation_seq": revision_base * 5,
            },
            {
                "step": "stale_guard_rejected",
                "editor_session_id": restarted_editor_session,
                "event_seq": revision_base * 3,
                "scene_revision": revision_base * 4,
                "operation_seq": revision_base * 5,
            },
        ],
        "redaction": {
            "absolute_paths_absent": True,
            "approval_content_absent": True,
            "native_ids_absent": True,
            "secrets_absent": True,
            "source_content_absent": True,
            "truncated": False,
        },
    }


def bind_synthetic_recorder_journal(
    trace: dict[str, Any],
) -> dict[str, Any]:
    event = {
        "seq": 0,
        "direction": "client_to_server",
        "kind": "notification",
        "method": "notifications/initialized",
        "frame_bytes": 64,
    }
    events = [event]
    product_bindings = {
        field: trace["bindings"][field]
        for field in acceptance.RECORDER_BINDING_FIELDS
    }
    journal = {
        "schema_version": "s11-recorder-journal/1.0",
        "capture_kind": "synthetic_contract_fixture",
        "status": "fixture_valid",
        "surface": trace["surface"],
        "host": copy.deepcopy(trace["host"]),
        "bindings": product_bindings,
        "host_controls": {
            "sandbox_approval_observed": True,
            "config_reload_observed": True,
            "root_binding_observed": True,
            "package_launcher_observed": True,
            "package_launcher_path": "bin/godot-codex-mcp",
            "package_launcher_sha256": trace["bindings"][
                "mcp_binary_sha256"
            ],
        },
        "protocol_version": "2025-11-25",
        "events": events,
        "integrity": {
            "input_bytes": 64,
            "output_bytes": 0,
            "input_frames": 1,
            "output_frames": 0,
            "event_count": 1,
            "event_chain_sha256": acceptance.recorder_event_chain_sha256(
                events
            ),
            "child_exit_code": 0,
            "passthrough_mode": True,
            "recorder_errors": [],
            "truncated": False,
        },
        "redaction": {
            "request_arguments_absent": True,
            "tool_content_absent": True,
            "elicitation_content_absent": True,
            "source_content_absent": True,
            "native_ids_absent": True,
            "absolute_paths_absent": True,
            "secrets_absent": True,
        },
    }
    trace["bindings"]["recorder_journal_sha256"] = (
        acceptance.recorder_journal_sha256(journal)
    )
    trace["bindings"]["recorder_event_chain_sha256"] = journal["integrity"][
        "event_chain_sha256"
    ]
    trace["bindings"]["recorder_event_count"] = 1
    return journal


def bind_negotiated_recorder_protocol(
    trace: dict[str, Any],
    protocol_version: str,
) -> dict[str, Any]:
    journal = bind_synthetic_recorder_journal(trace)
    journal["capture_kind"] = "surface_transport_capture"
    journal["status"] = "complete"
    journal["protocol_version"] = protocol_version
    return journal


def synthetic_surface_authority(
    trace: dict[str, Any],
    *,
    trace_sha256: str,
    journal_sha256: str,
) -> tuple[dict[str, Any], str, str, str]:
    surface = trace["surface"]
    directory = acceptance.surface_acquisition_directory(surface)
    trace_path = f"{directory}/trace.json"
    journal_path = f"{directory}/recorder-journal.json"
    authority_path = f"{directory}/authority.json"
    trace_bindings = trace["bindings"]
    projection = {
        "schema_version": "s11-surface-acquisition-authority/1.0",
        "authority_kind": "external_operator_attestation",
        "trust_boundary": acceptance.SURFACE_AUTHORITY_TRUST_BOUNDARY,
        "operator_id": f"operator:sha256:{'a' * 64}",
        "surface": surface,
        "artifact_directory": directory,
        "bindings": {
            "package_source_commit": trace_bindings[
                "package_source_commit"
            ],
            "package_manifest_sha256": trace_bindings[
                "package_manifest_sha256"
            ],
            "host_provenance_sha256": trace_bindings[
                "host_provenance_sha256"
            ],
            "host_artifact_sha256": trace_bindings[
                "host_artifact_sha256"
            ],
            "client_artifact_sha256": trace_bindings[
                "client_artifact_sha256"
            ],
            "mcp_binary_sha256": trace_bindings["mcp_binary_sha256"],
            "project_fixture_sha256": trace_bindings[
                "project_fixture_sha256"
            ],
            "prompt_pack_sha256": trace_bindings["prompt_pack_sha256"],
            "trace_path": trace_path,
            "trace_sha256": trace_sha256,
            "recorder_journal_path": journal_path,
            "recorder_journal_sha256": journal_sha256,
        },
        "observations": {
            field: True
            for field in acceptance.SURFACE_AUTHORITY_OBSERVATION_FIELDS
        },
    }
    authority = {
        **projection,
        "attestation_projection_sha256": acceptance.sha256_bytes(
            acceptance.SURFACE_AUTHORITY_DOMAIN
            + acceptance.canonical_json(projection)
        ),
    }
    return authority, authority_path, trace_path, journal_path


def synthetic_usability_report() -> dict[str, Any]:
    tasks = [
        {
            "goal_id": goal,
            "status": "passed",
            "duration_seconds": 10,
            "wrong_turns": 0,
            "help_used": False,
            "remediation_succeeded": True,
        }
        for goal in sorted(acceptance.REQUIRED_USABILITY_GOALS)
    ]
    doctor_faults = [
        {
            "scenario": scenario,
            "diagnostic_code": diagnostic_code,
            "remediation_id": remediation_id,
            "status": "passed",
            "remediation_succeeded": True,
        }
        for scenario, (diagnostic_code, remediation_id) in sorted(
            acceptance.USABILITY_DOCTOR_FAULTS.items()
        )
    ]
    return {
        "schema_version": "s11-usability-report/1.0",
        "acquisition_kind": "synthetic_contract_fixture",
        "attestation_trust_boundary": (
            acceptance.human_usability.AUTHORITY_TRUST_BOUNDARY
        ),
        "status": "fixture_valid",
        "package_source_commit": "a" * 40,
        "package_manifest_sha256": digest("1"),
        "prompt_pack": {
            "id": "sprint11-external-beta-v1",
            "path": "tests/codex/prompts/sprint11-external-beta-v1.json",
            "sha256": digest("4"),
        },
        "participants": [
            {
                "participant_id": f"participant:sha256:{index:064x}",
                "external_attestation_independence": (
                    "independent" if index == 1 else "not_independent"
                ),
                "tasks": copy.deepcopy(tasks),
                "doctor_faults": copy.deepcopy(doctor_faults),
                "trace_path": (
                    "tests/codex/acquisition/sprint11/human/"
                    f"s11u-run-{index:032x}/trace.json"
                ),
                "trace_sha256": digest(str(index)),
                "rubric_path": (
                    "tests/codex/acquisition/sprint11/human/"
                    f"s11u-run-{index:032x}/rubric.json"
                ),
                "rubric_sha256": digest(chr(ord("a") + index)),
                "consent_path": (
                    "tests/codex/acquisition/sprint11/human/"
                    f"s11u-run-{index:032x}/consent.json"
                ),
                "consent_sha256": digest(str(index + 3)),
                "defect_ledger_path": (
                    "tests/codex/acquisition/sprint11/human/"
                    f"s11u-run-{index:032x}/defect-ledger.json"
                ),
                "defect_ledger_sha256": digest(str(index + 6)),
                "authority_path": (
                    "tests/codex/acquisition/sprint11/human/"
                    f"s11u-run-{index:032x}/authority.json"
                ),
                "authority_sha256": digest(chr(ord("a") + index)),
            }
            for index in range(1, 4)
        ],
        "metrics": {
            "first_useful_status_seconds": 120,
            "connected_live_query_seconds": 420,
            "secret_copy_count": 0,
            "manual_toml_steps": 0,
            "project_integrity_failures": 0,
            "project_isolation_failures": 0,
            "fault_recoveries": 15,
        },
        "defects": [],
    }


class HumanBlobRepository:
    def __init__(self, blobs: dict[str, bytes]) -> None:
        self.blobs = blobs

    def blob_at(self, _commit: str, relative: str) -> bytes:
        try:
            return self.blobs[relative]
        except KeyError as error:
            raise acceptance.AcceptanceError(
                f"missing synthetic Git blob: {relative}"
            ) from error


def qualifying_usability_report() -> tuple[dict[str, Any], dict[str, bytes]]:
    participants: list[dict[str, Any]] = []
    blobs: dict[str, bytes] = {}
    package_source_commit = "c" * 40
    package_manifest_sha256 = digest("a")
    prompt_pack: dict[str, Any] | None = None
    for index in range(1, 4):
        independent = index == 1
        trace, rubric, consent, defects = (
            human_fixtures.relabelled_real_bundle(
                implementation_independent=independent,
            )
        )
        run_id = f"s11u-run:{index:032x}"
        participant_id = f"participant:sha256:{index:064x}"
        operator_id = f"operator:sha256:{(index + 10):064x}"
        consent["payload"]["run_id"] = run_id
        consent["payload"]["participant_id"] = participant_id
        consent["payload"]["operator_id"] = operator_id
        defects["payload"]["bound_human_run_ids"] = [run_id]
        trace_payload = trace["payload"]
        trace_payload["run_id"] = run_id
        trace_payload["participant"]["participant_id"] = participant_id
        trace_payload["participant"]["consent_receipt_sha256"] = (
            acceptance.human_usability.digest_document(consent)
        )
        trace_payload["attestation"]["operator_id"] = operator_id
        rubric_payload = rubric["payload"]
        rubric_payload["trace"]["run_id"] = run_id
        rubric_payload["trace"]["participant_id"] = participant_id
        rubric_payload["trace"]["sha256"] = (
            acceptance.human_usability.digest_document(trace)
        )
        rubric_payload["defects"]["ledger_sha256"] = (
            acceptance.human_usability.digest_document(defects)
        )
        rubric_payload["attestation"]["assessor_id"] = operator_id
        artifact_directory = (
            acceptance.human_usability.human_acquisition_directory(run_id)
        )
        paths = {
            "trace": f"{artifact_directory}/trace.json",
            "rubric": f"{artifact_directory}/rubric.json",
            "consent": f"{artifact_directory}/consent.json",
            "defect_ledger": f"{artifact_directory}/defect-ledger.json",
            "authority": f"{artifact_directory}/authority.json",
        }
        documents = {
            "trace": trace,
            "rubric": rubric,
            "consent": consent,
            "defect_ledger": defects,
        }
        artifact_sha256: dict[str, str] = {}
        for kind, document in documents.items():
            blob = acceptance.human_usability.canonical_json(document)
            blobs[paths[kind]] = blob
            artifact_sha256[kind] = acceptance.sha256_bytes(blob)
        authority_projection = {
            "schema_version": "s11-human-acquisition-authority/1.0",
            "authority_kind": "external_operator_attestation",
            "authority_schema": {
                "id": acceptance.human_usability.AUTHORITY_SCHEMA_ARTIFACT[0],
                "path": acceptance.human_usability.AUTHORITY_SCHEMA_ARTIFACT[1],
                "sha256": acceptance.sha256_file(
                    acceptance.HUMAN_AUTHORITY_SCHEMA_PATH
                ),
            },
            "trust_boundary": (
                acceptance.human_usability.AUTHORITY_TRUST_BOUNDARY
            ),
            "operator_id": operator_id,
            "run_id": run_id,
            "participant_id": participant_id,
            "artifact_directory": artifact_directory,
            "observations": {
                "real_person_observed": True,
                "consent_observed_before_tasks": True,
                "implementation_independence": (
                    "independent" if independent else "not_independent"
                ),
                "developer_coaching_absent": True,
                "fixture_answers_absent": True,
            },
            "artifact_sha256": artifact_sha256,
        }
        authority = {
            **authority_projection,
            "attestation_projection_sha256": (
                acceptance.human_usability.digest_document(
                    authority_projection
                )
            ),
        }
        authority_blob = acceptance.human_usability.canonical_json(authority)
        blobs[paths["authority"]] = authority_blob
        participants.append(
            {
                "participant_id": participant_id,
                "external_attestation_independence": (
                    "independent" if independent else "not_independent"
                ),
                "tasks": [
                    {
                        "goal_id": task["goal_id"],
                        "status": task["status"],
                        "duration_seconds": task["duration_ms"] / 1000,
                        "wrong_turns": task["wrong_turns"],
                        "help_used": bool(task["help_source_ids"]),
                        "remediation_succeeded": task[
                            "remediation_succeeded"
                        ],
                    }
                    for task in trace_payload["tasks"]
                ],
                "doctor_faults": [
                    {
                        "scenario": fault["scenario"],
                        "diagnostic_code": fault[
                            "observed_diagnostic_code"
                        ],
                        "remediation_id": fault[
                            "observed_remediation_id"
                        ],
                        "status": fault["status"],
                        "remediation_succeeded": fault[
                            "remediation_succeeded"
                        ],
                    }
                    for fault in trace_payload["doctor_faults"]
                ],
                **{
                    f"{kind}_path": path
                    for kind, path in paths.items()
                },
                **{
                    f"{kind}_sha256": (
                        acceptance.sha256_bytes(blobs[paths[kind]])
                    )
                    for kind in paths
                },
            }
        )
        trace_bindings = trace_payload["bindings"]
        trace_bindings["package"]["source_commit"] = package_source_commit
        trace_bindings["package"]["manifest_sha256"] = (
            package_manifest_sha256
        )
        if prompt_pack is None:
            prompt_pack = copy.deepcopy(trace_bindings["prompt_pack"])
    assert prompt_pack is not None
    return (
        {
            "schema_version": "s11-usability-report/1.0",
            "acquisition_kind": "real_human",
            "attestation_trust_boundary": (
                acceptance.human_usability.AUTHORITY_TRUST_BOUNDARY
            ),
            "status": "passed",
            "package_source_commit": package_source_commit,
            "package_manifest_sha256": package_manifest_sha256,
            "prompt_pack": prompt_pack,
            "participants": participants,
            "metrics": {
                "first_useful_status_seconds": 120,
                "connected_live_query_seconds": 300,
                "secret_copy_count": 0,
                "manual_toml_steps": 0,
                "project_integrity_failures": 0,
                "project_isolation_failures": 0,
                "fault_recoveries": 15,
            },
            "defects": [],
        },
        blobs,
    )


class Sprint11AcceptanceTests(unittest.TestCase):
    def test_surface_contract_has_seventeen_assertions_and_global_no_form_gate(
        self,
    ) -> None:
        self.assertEqual(len(acceptance.REQUIRED_ASSERTIONS), 17)
        self.assertNotIn(
            "host.unsupported_form",
            acceptance.REQUIRED_ASSERTIONS,
        )
        report = acceptance.strict_json_load(
            acceptance.REPOSITORY_ROOT
            / acceptance.GLOBAL_UNSUPPORTED_FORM_REPORT_PATH,
            maximum_bytes=acceptance.MAX_ARTIFACT_BYTES,
        )
        acceptance.validate_global_unsupported_form_probe(report)
        for field, value in (
            ("elicitation_count", 1),
            ("error", "approval_invalid"),
            ("native_actions", 1),
            ("source_unchanged", False),
        ):
            with self.subTest(field=field):
                changed = copy.deepcopy(report)
                unsupported = next(
                    item
                    for item in changed["negatives"]["approval"]
                    if item["decision"] == "unsupported"
                )
                unsupported[field] = value
                with self.assertRaises(acceptance.AcceptanceError):
                    acceptance.validate_global_unsupported_form_probe(
                        changed
                    )
        missing = copy.deepcopy(report)
        missing["negatives"]["approval"] = [
            item
            for item in missing["negatives"]["approval"]
            if item["decision"] != "unsupported"
        ]
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_global_unsupported_form_probe(missing)
        duplicated = copy.deepcopy(report)
        duplicated["negatives"]["approval"].append(
            copy.deepcopy(
                next(
                    item
                    for item in report["negatives"]["approval"]
                    if item["decision"] == "unsupported"
                )
            )
        )
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_global_unsupported_form_probe(duplicated)

    def setUp(self) -> None:
        self.vectors = acceptance.strict_json_load(
            acceptance.SCRIPT_DIR
            / "fixtures"
            / "external_beta_oracle"
            / "contract-vectors.json"
        )

    def traces(self) -> dict[str, dict[str, Any]]:
        return {
            "app": synthetic_trace("app", "app-seed", 10),
            "cli": synthetic_trace("cli", "cli-seed", 100),
            "ide": synthetic_trace("ide", "ide-seed", 1_000),
        }

    def test_contract_files_and_immutable_sprint10_baseline(self) -> None:
        result = acceptance.validate_contract_files(acceptance.GitRepository())
        self.assertEqual(result["status"], "contract_valid")
        self.assertFalse(result["qualifying_evidence_generated"])
        self.assertEqual(result["tools"], 41)
        self.assertEqual(result["fixed_resources"], 4)
        self.assertEqual(result["resource_templates"], 1)

    def test_automated_runner_executes_every_canonical_gate_command(self) -> None:
        seen: list[acceptance.GateCommand] = []

        def execute(
            command: acceptance.GateCommand,
            timeout: float,
        ) -> acceptance.CommandObservation:
            self.assertEqual(timeout, 17)
            seen.append(command)
            command_digest = acceptance.sha256_bytes(
                acceptance.canonical_json(
                    {"cwd": command.cwd, "argv": list(command.argv)}
                )
            )
            return acceptance.CommandObservation(
                command_sha256=command_digest,
                stdout_sha256=digest("a"),
                stderr_sha256=digest("b"),
                duration_ms=1,
            )

        results = acceptance.run_automated_gates(17, executor=execute)
        self.assertEqual(set(results), acceptance.AUTOMATED_GATE_NAMES)
        self.assertEqual(
            len(seen),
            sum(
                len(group.commands)
                for group in acceptance.AUTOMATED_GATE_GROUPS
            ),
        )
        self.assertTrue(all(item["status"] == "passed" for item in results.values()))
        bindings = acceptance.canonical_gate_bindings()
        self.assertEqual(set(bindings), acceptance.REQUIRED_GATES)
        self.assertTrue(
            all(
                set(record) == {"runner", "definition_sha256"}
                for record in bindings.values()
            )
        )
        offline_group = next(
            group
            for group in acceptance.AUTOMATED_GATE_GROUPS
            if group.group_id == "connection_offline_contracts"
        )
        offline_argv = offline_group.commands[0].argv
        self.assertIn(
            ("-p", "godot-codex-mcp"),
            tuple(zip(offline_argv, offline_argv[1:], strict=False)),
        )
        product_group = next(
            group
            for group in acceptance.AUTOMATED_GATE_GROUPS
            if group.group_id == "product_operations_contracts"
        )
        self.assertIn(
            ("-p", "godot-codex-surface-capture"),
            tuple(
                zip(
                    product_group.commands[0].argv,
                    product_group.commands[0].argv[1:],
                    strict=False,
                )
            ),
        )
        python_group = next(
            group
            for group in acceptance.AUTOMATED_GATE_GROUPS
            if group.group_id == "s11_python_contracts"
        )
        python_argv = python_group.commands[0].argv
        self.assertIn(
            "tests.codex.test_sprint11_surface_artifacts",
            python_argv,
        )
        self.assertTrue(
            any(
                command.cwd == "tests/codex"
                and command.argv
                == (
                    "cargo",
                    "test",
                    "--locked",
                    "--test",
                    "sprint11_surface_capture_schema_contract",
                )
                for command in python_group.commands
            )
        )
        self.assertIn(
            "godot-codex-mcp/crates/godot-codex-mcp/tests/"
            "offline_subprocess.rs",
            acceptance.REQUIRED_SOURCE_PATHS,
        )
        self.assertIn(
            "tests/codex/README.md",
            acceptance.REQUIRED_SOURCE_PATHS,
        )
        self.assertTrue(
            all(
                command.argv[1:4] == ("-E", "-s", "-S")
                for group in acceptance.AUTOMATED_GATE_GROUPS
                for command in group.commands
                if command.argv[0] == "{python}"
            )
        )

    def test_automated_runner_rejects_timeout_and_failed_command(self) -> None:
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.run_automated_gates(0)

        def fail(
            _command: acceptance.GateCommand,
            _timeout: float,
        ) -> acceptance.CommandObservation:
            raise acceptance.AcceptanceError("fixture gate failed")

        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.run_automated_gates(17, executor=fail)

        with self.assertRaisesRegex(
            acceptance.AcceptanceError,
            "timed out",
        ):
            acceptance._run_gate_command(
                acceptance.GateCommand(
                    cwd=".",
                    argv=(
                        "{python}",
                        "-c",
                        "import time; time.sleep(2)",
                    ),
                ),
                1,
            )

        result = subprocess.run(
            [
                sys.executable,
                str(acceptance.SCRIPT_DIR / "sprint11_acceptance.py"),
                "--timeout",
                "0",
            ],
            cwd=acceptance.REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "--timeout must be between 1 and 180 seconds",
            result.stdout,
        )

    @unittest.skipUnless(os.name == "posix", "POSIX process scope contract")
    def test_gate_runner_preserves_primary_failure_during_cleanup(self) -> None:
        real_popen = acceptance.subprocess.Popen
        real_thread = acceptance.threading.Thread
        real_close_scope = acceptance.process_scope.close_scope
        gate_processes: list[subprocess.Popen[bytes]] = []
        drain_threads: list[threading.Thread] = []

        def recording_popen(*args: Any, **kwargs: Any) -> Any:
            process = real_popen(*args, **kwargs)
            argv = args[0] if args else kwargs["args"]
            if (
                len(argv) >= len(acceptance.process_scope.STOPPED_LAUNCHER)
                and tuple(
                    argv[: len(acceptance.process_scope.STOPPED_LAUNCHER)]
                )
                == acceptance.process_scope.STOPPED_LAUNCHER
            ):
                gate_processes.append(process)
            return process

        def recording_thread(*args: Any, **kwargs: Any) -> threading.Thread:
            thread = real_thread(*args, **kwargs)
            target = kwargs.get("target")
            if getattr(target, "__name__", None) == "drain":
                drain_threads.append(thread)
            return thread

        def close_then_fail(
            scope: acceptance.process_scope.ProcessScope,
        ) -> bool:
            real_close_scope(scope)
            raise RuntimeError("fixture cleanup failure")

        with (
            mock.patch.object(
                acceptance.subprocess,
                "Popen",
                side_effect=recording_popen,
            ),
            mock.patch.object(
                acceptance.threading,
                "Thread",
                side_effect=recording_thread,
            ),
            mock.patch.object(
                acceptance.process_scope,
                "close_scope",
                side_effect=close_then_fail,
            ),
        ):
            with self.assertRaisesRegex(
                acceptance.AcceptanceError,
                "timed out",
            ) as raised:
                acceptance._run_gate_command(
                    acceptance.GateCommand(
                        cwd=".",
                        argv=(
                            "{python}",
                            "-E",
                            "-s",
                            "-S",
                            "-c",
                            "import time; time.sleep(2)",
                        ),
                    ),
                    1,
                )

        self.assertTrue(
            any(
                "detached process scope could not be closed" in note
                for note in getattr(raised.exception, "__notes__", ())
            )
        )
        self.assertEqual(len(gate_processes), 1)
        process = gate_processes[0]
        self.assertIsNotNone(process.stdout)
        self.assertIsNotNone(process.stderr)
        self.assertTrue(process.stdout.closed)
        self.assertTrue(process.stderr.closed)
        self.assertEqual(len(drain_threads), 2)
        self.assertTrue(all(not thread.is_alive() for thread in drain_threads))

    @unittest.skipUnless(os.name == "posix", "POSIX process scope contract")
    def test_gate_runner_aborts_when_scope_tracker_fails(self) -> None:
        scope_module = acceptance.process_scope
        real_process_table = scope_module._process_table
        calls = 0

        def fail_tracker(
            *,
            include_environment: bool = True,
        ) -> dict[int, Any]:
            nonlocal calls
            calls += 1
            if calls <= 2:
                return real_process_table(
                    include_environment=include_environment
                )
            raise scope_module.ProcessScopeError(
                "fixture process table failure"
            )

        started = time.monotonic()
        with (
            mock.patch.object(
                scope_module,
                "_process_table",
                side_effect=fail_tracker,
            ),
            self.assertRaisesRegex(
                acceptance.AcceptanceError,
                "process scope tracker failed",
            ),
        ):
            acceptance._run_gate_command(
                acceptance.GateCommand(
                    cwd=".",
                    argv=(
                        "{python}",
                        "-E",
                        "-s",
                        "-S",
                        "-c",
                        "import time; time.sleep(10)",
                    ),
                ),
                5,
            )
        self.assertLess(time.monotonic() - started, 4)

    def test_gate_runner_ignores_path_stubs_and_scrubs_secrets(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fake_bin = Path(directory)
            marker = fake_bin / "cargo-was-executed"
            fake_cargo = fake_bin / "cargo"
            fake_cargo.write_text(
                "#!/bin/sh\n"
                f"touch {marker!s}\n"
                "exit 0\n",
                encoding="utf-8",
            )
            fake_cargo.chmod(0o755)
            with mock.patch.dict(
                os.environ,
                {
                    "PATH": str(fake_bin),
                    "SPRINT11_SENTINEL_SECRET": "must-not-cross",
                },
                clear=False,
            ):
                cargo = acceptance._run_gate_command(
                    acceptance.GateCommand(
                        cwd="godot-codex-mcp",
                        argv=("cargo", "--version"),
                    ),
                    30,
                )
                python = acceptance._run_gate_command(
                    acceptance.GateCommand(
                        cwd=".",
                        argv=(
                            "{python}",
                            "-c",
                            (
                                "import os,sys;"
                                "sys.exit(1 if "
                                "'SPRINT11_SENTINEL_SECRET' in os.environ else 0)"
                            ),
                        ),
                    ),
                    30,
                )
            self.assertFalse(marker.exists())
            self.assertRegex(cargo.command_sha256, r"^sha256:[0-9a-f]{64}$")
            self.assertRegex(python.command_sha256, r"^sha256:[0-9a-f]{64}$")

    @unittest.skipUnless(os.name == "posix", "POSIX process scope contract")
    def test_gate_runner_rejects_a_detached_session(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="s11-gate-detached."
        ) as temporary:
            marker = Path(temporary) / "detached-survived"
            child = (
                "import pathlib,time;"
                "time.sleep(0.8);"
                f"pathlib.Path({str(marker)!r}).write_text('leak')"
            )
            parent = (
                "import subprocess,sys;"
                "subprocess.Popen("
                f"[sys.executable,'-c',{child!r}],"
                "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,"
                "stderr=subprocess.DEVNULL,start_new_session=True)"
            )
            with self.assertRaisesRegex(
                acceptance.AcceptanceError,
                "detached descendants",
            ):
                acceptance._run_gate_command(
                    acceptance.GateCommand(
                        cwd=".",
                        argv=("{python}", "-E", "-s", "-S", "-c", parent),
                    ),
                    5,
                )
            time.sleep(1.0)
            self.assertFalse(marker.exists())

    @unittest.skipUnless(os.name == "posix", "Unix socket path contract")
    def test_gate_runner_uses_a_private_short_root_for_nested_sockets(
        self,
    ) -> None:
        real_temporary_directory = tempfile.TemporaryDirectory
        observed: dict[str, Any] = {}

        def capture_directory(*args: Any, **kwargs: Any) -> Any:
            directory = real_temporary_directory(*args, **kwargs)
            root = Path(directory.name)
            observed.update(
                {
                    "root": root,
                    "mode": root.stat().st_mode & 0o777,
                    "parent": kwargs.get("dir"),
                }
            )
            return directory

        nested_socket_probe = (
            "import pathlib,socket,tempfile;"
            "temporary=tempfile.TemporaryDirectory();"
            "endpoint=pathlib.Path(temporary.name)/"
            "'.godot/codex/run/bridge-test.sock';"
            "endpoint.parent.mkdir(parents=True);"
            "server=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);"
            "server.bind(str(endpoint));"
            "server.close();"
            "temporary.cleanup()"
        )
        with mock.patch.object(
            acceptance.tempfile,
            "TemporaryDirectory",
            side_effect=capture_directory,
        ):
            acceptance._run_gate_command(
                acceptance.GateCommand(
                    cwd=".",
                    argv=("{python}", "-c", nested_socket_probe),
                ),
                30,
            )

        root = observed["root"]
        self.assertEqual(observed["parent"], Path("/tmp"))
        self.assertEqual(observed["mode"] & 0o077, 0)
        self.assertLessEqual(len(os.fsencode(root)), 48)
        self.assertFalse(root.exists())

    def test_gate_runner_stops_output_at_the_byte_cap(self) -> None:
        started = time.monotonic()
        with self.assertRaisesRegex(
            acceptance.AcceptanceError,
            "output exceeds byte bound",
        ):
            acceptance._run_gate_command(
                acceptance.GateCommand(
                    cwd=".",
                    argv=(
                        "{python}",
                        "-c",
                        (
                            "import sys;"
                            "sys.stdout.buffer.write(b'x' * "
                            f"{acceptance.MAX_GATE_OUTPUT_BYTES + 1})"
                        ),
                    ),
                ),
                30,
            )
        self.assertLess(time.monotonic() - started, 5)

    def test_skipped_checkout_binding_is_explicitly_nonqualifying(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory) / "evidence.json"
            evidence.write_text('{"fixture":true}\n', encoding="utf-8")
            skipped = acceptance.evidence_validation_summary(
                evidence,
                checkout_binding_validated=False,
            )
            self.assertEqual(skipped["status"], "validated_nonqualifying")
            self.assertFalse(skipped["qualifying_evidence_validated"])
            self.assertEqual(skipped["checkout_binding"], "skipped")

            qualified = acceptance.evidence_validation_summary(
                evidence,
                checkout_binding_validated=True,
            )
            self.assertEqual(qualified["status"], "passed")
            self.assertTrue(qualified["qualifying_evidence_validated"])
            self.assertEqual(qualified["checkout_binding"], "validated")

    def test_exact_registry_contract_rejects_add_remove_and_duplicate(self) -> None:
        projection = registry_projection()
        acceptance.validate_registry_projection(projection)
        for mutation in ("remove", "add", "duplicate"):
            changed = copy.deepcopy(projection)
            if mutation == "remove":
                changed["tools"].pop()
            elif mutation == "add":
                changed["tools"].append("godot_unreviewed_tool")
            else:
                changed["tools"][-1] = changed["tools"][0]
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_registry_projection(changed)

    def test_isolated_surface_coordinates_normalize_equally(self) -> None:
        result = acceptance.compare_surface_traces(
            self.traces(),
            qualifying=False,
        )
        self.assertRegex(result, r"^sha256:[0-9a-f]{64}$")

    def test_normalization_vectors_cover_positive_and_negative_cases(self) -> None:
        cases = self.vectors["normalization_cases"]
        self.assertEqual(
            {case["expected"] for case in cases},
            {"match", "reject"},
        )
        for case in cases:
            traces = self.traces()
            mutation = case["mutation"]
            if mutation == "isolated_run_coordinates":
                acceptance.compare_surface_traces(traces, qualifying=False)
                self.assertEqual(case["expected"], "match")
                continue
            target = traces["cli"]
            if mutation == "persistent_scene_id":
                for assertion in target["assertions"]:
                    if assertion["assertion_id"] == "saved.current_scene":
                        assertion["projection"]["scene_id"] = (
                            "scene:res://different.tscn"
                        )
            elif mutation == "runtime_session_alias":
                for assertion in target["assertions"]:
                    if assertion["assertion_id"] == "runtime.error_stack":
                        assertion["projection"]["runtime_session_after"] = (
                            assertion["projection"]["runtime_session_before"]
                        )
            elif mutation == "missing_assertion":
                target["assertions"].pop()
            elif mutation == "preview_digest":
                for assertion in target["assertions"]:
                    if assertion["assertion_id"] == "transaction.preview":
                        assertion["projection"]["preview_digest"] = digest("0")
            elif mutation == "revision_order":
                target["revision_timeline"][2], target["revision_timeline"][3] = (
                    target["revision_timeline"][3],
                    target["revision_timeline"][2],
                )
            elif mutation == "decline_mutation":
                for outcome in target["form_outcomes"]:
                    if outcome["scenario"] == "decline":
                        outcome["mutation_observed"] = True
            else:
                self.fail(f"unknown vector mutation: {mutation}")
            with self.assertRaises(
                acceptance.AcceptanceError,
                msg=case["id"],
            ):
                acceptance.compare_surface_traces(traces, qualifying=False)

    def test_synthetic_acquisition_cannot_satisfy_qualification(self) -> None:
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_surface_trace(
                synthetic_trace("app", "fixture", 1),
                qualifying=True,
            )
        report = synthetic_usability_report()
        acceptance.validate_usability_report(report, qualifying=False)
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_usability_report(report, qualifying=True)

    def test_surface_transport_capture_requires_separate_source_authority(
        self,
    ) -> None:
        trace = synthetic_trace("cli", "authority", 100)
        journal = bind_synthetic_recorder_journal(trace)
        trace_sha256 = acceptance.sha256_bytes(
            acceptance.canonical_json(trace)
        )
        journal_sha256 = acceptance.recorder_journal_sha256(journal)
        authority, authority_path, trace_path, journal_path = (
            synthetic_surface_authority(
                trace,
                trace_sha256=trace_sha256,
                journal_sha256=journal_sha256,
            )
        )
        validated = acceptance.validate_surface_acquisition_authority(
            authority,
            surface="cli",
            authority_path=authority_path,
            authority_sha256=acceptance.sha256_bytes(
                acceptance.canonical_json(authority)
            ),
            trace_path=trace_path,
            trace_sha256=trace_sha256,
            recorder_journal_path=journal_path,
            recorder_journal_sha256=journal_sha256,
            trace=trace,
        )
        self.assertEqual(validated, authority)

        transport_only = copy.deepcopy(trace)
        transport_only["capture_kind"] = "surface_transport_capture"
        transport_only["status"] = "passed"
        with self.assertRaisesRegex(
            acceptance.AcceptanceError,
            "external authority",
        ):
            acceptance.validate_surface_trace(
                transport_only,
                qualifying=True,
                recorder_journal=journal,
                recorder_journal_digest=journal_sha256,
            )

        tampered = copy.deepcopy(authority)
        tampered["observations"][
            "project_trust_reviewed_and_accepted"
        ] = False
        projection = dict(tampered)
        projection.pop("attestation_projection_sha256")
        tampered["attestation_projection_sha256"] = acceptance.sha256_bytes(
            acceptance.SURFACE_AUTHORITY_DOMAIN
            + acceptance.canonical_json(projection)
        )
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_surface_acquisition_authority(
                tampered,
                surface="cli",
                authority_path=authority_path,
                authority_sha256=acceptance.sha256_bytes(
                    acceptance.canonical_json(tampered)
                ),
                trace_path=trace_path,
                trace_sha256=trace_sha256,
                recorder_journal_path=journal_path,
                recorder_journal_sha256=journal_sha256,
                trace=trace,
            )
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_surface_acquisition_authority(
                authority,
                surface="cli",
                authority_path=(
                    "tests/codex/acquisition/sprint11/surfaces/app/"
                    "authority.json"
                ),
                authority_sha256=acceptance.sha256_bytes(
                    acceptance.canonical_json(authority)
                ),
                trace_path=trace_path,
                trace_sha256=trace_sha256,
                recorder_journal_path=journal_path,
                recorder_journal_sha256=journal_sha256,
                trace=trace,
            )

    def test_legacy_recorder_cannot_qualify_for_any_protocol(
        self,
    ) -> None:
        trace = synthetic_trace("cli", "protocol", 100)
        for protocol in [
            *sorted(acceptance.QUALIFYING_MCP_PROTOCOLS),
            "2025-03-26",
        ]:
            with self.subTest(protocol=protocol):
                journal = bind_negotiated_recorder_protocol(trace, protocol)
                with self.assertRaisesRegex(
                    acceptance.AcceptanceError,
                    "not an in-process official-host capture",
                ):
                    acceptance.validate_recorder_journal(
                        journal,
                        qualifying=True,
                    )

    def test_legacy_form_capable_transport_journal_is_always_rejected(
        self,
    ) -> None:
        trace = synthetic_trace("cli", "no-form-probe", 100)
        journal = bind_negotiated_recorder_protocol(trace, "2025-11-25")
        registry = acceptance.canonical_registry_profile()
        events = [
            {
                "seq": 0,
                "direction": "server_to_client",
                "kind": "response",
                "method": "initialize",
                "frame_bytes": 64,
                "protocol_version": "2025-11-25",
                "instructions_sha256": (
                    acceptance.canonical_server_instructions()[
                        "wire_sha256"
                    ]
                ),
            },
            {
                "seq": 1,
                "direction": "server_to_client",
                "kind": "response",
                "method": "tools/list",
                "frame_bytes": 64,
                "tools": registry["tools"],
            },
            {
                "seq": 2,
                "direction": "server_to_client",
                "kind": "response",
                "method": "resources/list",
                "frame_bytes": 64,
                "resources": registry["fixed_resources"],
            },
            {
                "seq": 3,
                "direction": "server_to_client",
                "kind": "response",
                "method": "resources/templates/list",
                "frame_bytes": 64,
                "resource_templates": registry["resource_templates"],
            },
            {
                "seq": 4,
                "direction": "server_to_client",
                "kind": "response",
                "method": "tools/call",
                "tool": "godot_get_connection_status",
                "frame_bytes": 64,
                "semantic_status": "offline_cached",
                "is_error": False,
            },
            *[
                {
                    "seq": sequence,
                    "direction": "client_to_server",
                    "kind": "response",
                    "method": "elicitation/create",
                    "frame_bytes": 64,
                    "form_mode": "form",
                    "form_action": action,
                    "content_recorded": False,
                }
                for sequence, action in enumerate(
                    ("accept", "decline", "cancel"),
                    start=5,
                )
            ],
            {
                "seq": 8,
                "direction": "server_to_client",
                "kind": "response",
                "method": "tools/call",
                "frame_bytes": 64,
                "form_action": "timeout",
                "error_code": "approval_timeout",
                "is_error": True,
            },
        ]
        journal["events"] = events
        journal["integrity"].update(
            {
                "event_count": len(events),
                "event_chain_sha256": (
                    acceptance.recorder_event_chain_sha256(events)
                ),
                "input_frames": 3,
                "output_frames": 6,
                "input_bytes": 192,
                "output_bytes": 384,
            }
        )
        self.assertFalse(
            any(
                event.get("error_code") == "approval_host_unsupported"
                for event in events
            )
        )
        for qualifying in (False, True):
            with self.subTest(qualifying=qualifying):
                with self.assertRaises(acceptance.AcceptanceError):
                    acceptance.validate_recorder_journal(
                        journal,
                        qualifying=qualifying,
                    )

    def test_every_semantic_assertion_rejects_an_empty_projection(self) -> None:
        for assertion_id in sorted(acceptance.REQUIRED_ASSERTIONS):
            trace = synthetic_trace("cli", f"empty-{assertion_id}", 100)
            for assertion in trace["assertions"]:
                if assertion["assertion_id"] == assertion_id:
                    assertion["projection"] = {}
                    break
            with self.assertRaises(
                acceptance.AcceptanceError,
                msg=assertion_id,
            ):
                acceptance.validate_surface_trace(trace, qualifying=False)

            trace = synthetic_trace("cli", f"extra-{assertion_id}", 100)
            for assertion in trace["assertions"]:
                if assertion["assertion_id"] == assertion_id:
                    assertion["projection"]["unbound_claim"] = True
                    break
            with self.assertRaises(
                acceptance.AcceptanceError,
                msg=f"{assertion_id}: extra field",
            ):
                acceptance.validate_surface_trace(trace, qualifying=False)

    def test_preview_and_saved_fact_projections_reject_free_payloads(self) -> None:
        for leaked_key, leaked_value in (
            ("property_value", {"type": "string", "value": "private"}),
            ("source", "extends Node"),
            ("native_id", 9_223_372_036_854_775_001),
        ):
            trace = synthetic_trace("cli", f"leak-{leaked_key}", 100)
            preview_projection = next(
                item["projection"]
                for item in trace["assertions"]
                if item["assertion_id"] == "transaction.preview"
            )
            preview_projection["preview"]["operations"][0][
                leaked_key
            ] = leaked_value
            preview_projection["preview_digest"] = acceptance.sha256_bytes(
                acceptance.canonical_json(preview_projection["preview"])
            )
            with self.assertRaises(
                acceptance.AcceptanceError,
                msg=leaked_key,
            ):
                acceptance.validate_surface_trace(trace, qualifying=False)

        trace = synthetic_trace("cli", "free-saved-fact", 100)
        saved = next(
            item["projection"]
            for item in trace["assertions"]
            if item["assertion_id"] == "saved.current_scene"
        )
        saved["fact"] = "arbitrary source or property content"
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_surface_trace(trace, qualifying=False)

    def test_trace_redaction_flags_are_derived_not_trusted(self) -> None:
        trace = synthetic_trace("cli", "redaction-lie", 100)
        trace["redaction"]["native_ids_absent"] = False
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_surface_trace(trace, qualifying=False)

        trace = synthetic_trace("cli", "absolute-path-lie", 100)
        trace["host"]["build"] = "/Users/private/build"
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_surface_trace(trace, qualifying=False)

    def test_post_package_allowlist_rejects_every_runner_and_fixture(self) -> None:
        class ChangedRepository:
            def __init__(self, changed: str) -> None:
                self.changed = changed

            def changed_paths(
                self,
                _parent: str,
                _child: str,
            ) -> list[str]:
                return [self.changed]

        fixture_root = acceptance.multi_project.s9.FIXTURE_ROOT
        multi_fixtures = {
            path.relative_to(acceptance.REPOSITORY_ROOT).as_posix()
            for path in fixture_root.rglob("*")
            if path.is_file() and ".godot" not in path.parts
        }
        multi_fixtures.add(
            acceptance.multi_project.s9.GOLDEN_PATH.relative_to(
                acceptance.REPOSITORY_ROOT
            ).as_posix()
        )
        immutable_inputs = (
            {
                spec.runner_path
                for spec in acceptance.packaged_regressions.canonical_command_specs()
            }
            | set(acceptance.packaged_regressions.FIXTURE_PATHS)
            | multi_fixtures
            | {
                acceptance.external_acquisitions.MULTI_RUNNER,
                acceptance.external_acquisitions.BUILD_RUNNER,
                acceptance.external_acquisitions.HOST_RUNNER,
                acceptance.external_acquisitions.HOST_PROFILE_PATH,
                acceptance.external_acquisitions.HOST_MEASUREMENT_SCHEMA_PATH,
                acceptance.external_acquisitions.HOST_ACQUISITION_SCHEMA_PATH,
            }
        )
        allowed = {
            "tests/codex/acquisition/sprint11/multi/report.json",
            "tests/codex/acquisition/sprint11/multi/receipt.json",
        }
        for relative in sorted(immutable_inputs):
            with self.assertRaises(
                acceptance.AcceptanceError,
                msg=relative,
            ):
                acceptance.validate_post_package_changed_paths(
                    repository=ChangedRepository(relative),  # type: ignore[arg-type]
                    package_source_commit="a" * 40,
                    source_commit="b" * 40,
                    acquisition_paths=allowed,
                )
        acceptance.validate_post_package_changed_paths(
            repository=ChangedRepository(  # type: ignore[arg-type]
                "tests/codex/acquisition/sprint11/multi/report.json"
            ),
            package_source_commit="a" * 40,
            source_commit="b" * 40,
            acquisition_paths=allowed,
        )

    def test_multi_project_receipt_binds_version_and_exact_fault_matrix(
        self,
    ) -> None:
        package_source_commit = "a" * 40
        source_commit = "b" * 40
        report_path = "tests/codex/acquisition/sprint11/multi/report.json"
        fixture_path = (
            "tests/codex/fixtures/transaction_prepare_project/project.godot"
        )
        runner_path = acceptance.external_acquisitions.MULTI_RUNNER
        fixture_blob = b"[application]\nconfig/name=\"S11 fixture\"\n"
        runner_blob = b"#!/usr/bin/env python3\n"
        bindings = {
            "package_source_commit": package_source_commit,
            "package_version": "0.1.0-beta.1",
            "package_manifest_sha256": digest("1"),
            "package_build_provenance_sha256": digest("2"),
            "package_archive_sha256": digest("3"),
            "package_sidecar_path": (
                acceptance.packaged_regressions.PACKAGE_SIDECAR_PATH
            ),
            "package_sidecar_sha256": digest("4"),
            "godot_version": "4.8.dev.codex.b225f77ac",
            "godot_commit": acceptance.SPRINT10_SOURCE_COMMIT,
            "godot_artifact_sha256": (
                acceptance.EXACT_GODOT_PREREQUISITE_SHA256
            ),
            "registry_sha256": digest("5"),
        }
        report = valid_multi_project_report()
        report["artifacts"] = {
            "godot_sha256": bindings["godot_artifact_sha256"],
            "sidecar_sha256": bindings["package_sidecar_sha256"],
        }
        report_blob = acceptance.canonical_json(report)
        template = list(acceptance.external_acquisitions.multi_command_template())
        assertions = {
            "two_real_editors": True,
            "two_package_sidecars": True,
            "separate_action_only_approvals": True,
            "validation_reports_isolated": True,
            "readback_and_targeted_undo": True,
            "foreign_ids_rejected": True,
            "target_fault_isolated": True,
            "isolation_matrix_complete": True,
            "copied_and_swapped_discovery_token_rejected": True,
            "config_root_cwd_swaps_rejected": True,
            "editor_restart_isolated": True,
            "cache_rebuild_isolated": True,
            "package_version_mismatch_rejected": True,
            "no_cross_project_leakage": True,
            "source_unchanged": True,
        }
        receipt = {
            "schema_version": (
                acceptance.external_acquisitions.MULTI_RECEIPT_SCHEMA
            ),
            "capture_kind": (
                acceptance.external_acquisitions.MULTI_CAPTURE_KIND
            ),
            "status": "passed",
            "bindings": bindings,
            "fixtures": [
                {
                    "path": fixture_path,
                    "sha256": acceptance.sha256_bytes(fixture_blob),
                }
            ],
            "command": {
                "id": "multi_project_live",
                "cwd": ".",
                "argv_template": template,
                "command_sha256": acceptance.sha256_bytes(
                    acceptance.canonical_json({"cwd": ".", "argv": template})
                ),
                "runner_path": runner_path,
                "runner_sha256": acceptance.sha256_bytes(runner_blob),
                "report_path": report_path,
                "report_sha256": acceptance.sha256_bytes(report_blob),
                "stdout_sha256": digest("6"),
                "stderr_sha256": digest("7"),
                "exit_code": 0,
                "duration_ms": 1,
            },
            "assertions": assertions,
            "cleanup": {
                "editor_processes_stopped": True,
                "game_processes_stopped": True,
                "sidecar_processes_stopped": True,
                "temporary_workspaces_removed": True,
            },
            "redaction": {
                "absolute_paths_absent": True,
                "native_ids_absent": True,
                "secrets_absent": True,
                "source_content_absent": True,
            },
        }

        class BlobRepository:
            def blob_at(self, commit: str, relative: str) -> bytes:
                blobs = {
                    (package_source_commit, fixture_path): fixture_blob,
                    (package_source_commit, runner_path): runner_blob,
                    (source_commit, report_path): report_blob,
                }
                return blobs[(commit, relative)]

        repository = BlobRepository()
        self.assertEqual(
            acceptance.validate_multi_project_receipt(
                receipt,
                repository=repository,  # type: ignore[arg-type]
                source_commit=source_commit,
                expected_bindings=bindings,
            ),
            {report_path},
        )
        for field in (
            "isolation_matrix_complete",
            "copied_and_swapped_discovery_token_rejected",
            "config_root_cwd_swaps_rejected",
            "editor_restart_isolated",
            "cache_rebuild_isolated",
            "package_version_mismatch_rejected",
            "no_cross_project_leakage",
        ):
            with self.subTest(field=field):
                tampered = copy.deepcopy(receipt)
                tampered["assertions"].pop(field)
                with self.assertRaises(acceptance.AcceptanceError):
                    acceptance.validate_multi_project_receipt(
                        tampered,
                        repository=repository,  # type: ignore[arg-type]
                        source_commit=source_commit,
                        expected_bindings=bindings,
                    )
        tampered = copy.deepcopy(receipt)
        tampered["bindings"]["package_version"] = "0.1.0-beta.2"
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_multi_project_receipt(
                tampered,
                repository=repository,  # type: ignore[arg-type]
                source_commit=source_commit,
                expected_bindings=bindings,
            )

    def test_trace_is_bound_to_exact_recorder_journal_and_event_chain(self) -> None:
        trace = synthetic_trace("cli", "journal", 100)
        journal = bind_synthetic_recorder_journal(trace)
        acceptance.validate_surface_trace(
            trace,
            qualifying=False,
            recorder_journal=journal,
            recorder_journal_digest=acceptance.recorder_journal_sha256(journal),
        )

        tampered = copy.deepcopy(journal)
        tampered["events"][0]["frame_bytes"] += 1
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_surface_trace(
                trace,
                qualifying=False,
                recorder_journal=tampered,
                recorder_journal_digest=acceptance.recorder_journal_sha256(
                    tampered
                ),
            )

        rebound = copy.deepcopy(journal)
        rebound["events"][0]["frame_bytes"] += 2
        rebound["integrity"]["event_chain_sha256"] = (
            acceptance.recorder_event_chain_sha256(rebound["events"])
        )
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_surface_trace(
                trace,
                qualifying=False,
                recorder_journal=rebound,
                recorder_journal_digest=acceptance.recorder_journal_sha256(
                    rebound
                ),
            )

    def test_v11_recorder_is_independently_bound_to_trace_semantics(self) -> None:
        from tests.codex import sprint11_surface_artifacts as artifacts
        from tests.codex.test_sprint11_surface_artifacts import (
            build_fixture_bundle,
        )

        bundle = build_fixture_bundle()
        trace = bundle["final"]
        journal = bundle["journal"]
        journal_digest = acceptance.sha256_bytes(bundle["journal_payload"])
        validated = acceptance.validate_recorder_journal(
            journal,
            qualifying=True,
        )
        self.assertEqual(validated["schema_version"], "s11-recorder-journal/1.1")
        acceptance.validate_surface_trace(
            trace,
            qualifying=False,
            recorder_journal=journal,
            recorder_journal_digest=journal_digest,
        )

        tampered_projection = copy.deepcopy(journal)
        tampered_projection["semantic_projections"][0]["value"][
            "unbound"
        ] = True
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_recorder_journal(
                tampered_projection,
                qualifying=True,
            )

        tampered_observation = copy.deepcopy(journal)
        event = next(
            item
            for item in tampered_observation["events"]
            if "tool_observation" in item
        )
        event["tool_observation"]["result"]["unbound"] = True
        tampered_observation["integrity"]["event_chain_sha256"] = (
            artifacts.recorder_event_chain_sha256(
                tampered_observation["events"]
            )
        )
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_recorder_journal(
                tampered_observation,
                qualifying=True,
            )

        rebound_trace = copy.deepcopy(trace)
        connection = next(
            item
            for item in rebound_trace["assertions"]
            if item["assertion_id"] == "connection.status"
        )
        connection["projection"]["project_id"] = (
            f"project:sha256:{'f' * 64}"
        )
        with self.assertRaisesRegex(
            acceptance.AcceptanceError,
            "trace assertions differ from recorder",
        ):
            acceptance.validate_surface_trace(
                rebound_trace,
                qualifying=False,
                recorder_journal=journal,
                recorder_journal_digest=journal_digest,
            )

    def test_usability_targets_and_independence_fail_closed(self) -> None:
        report = synthetic_usability_report()
        report["metrics"]["first_useful_status_seconds"] = 300.001
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_usability_report(report, qualifying=False)
        report = synthetic_usability_report()
        for participant in report["participants"]:
            participant["external_attestation_independence"] = (
                "not_independent"
            )
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_usability_report(report, qualifying=False)
        report = synthetic_usability_report()
        report["defects"] = [
            {
                "id": "critical-1",
                "severity": "critical",
                "status": "open",
                "retest_passed": False,
            }
        ]
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_usability_report(report, qualifying=False)

    def test_qualifying_usability_validates_every_primary_bundle_blob(
        self,
    ) -> None:
        report, blobs = qualifying_usability_report()
        artifact_paths: set[str] = set()
        acceptance.validate_usability_report(
            report,
            qualifying=True,
            repository=HumanBlobRepository(blobs),  # type: ignore[arg-type]
            source_commit="c" * 40,
            artifact_paths=artifact_paths,
        )
        self.assertEqual(len(artifact_paths), 15)

        missing_consent = copy.deepcopy(report)
        del missing_consent["participants"][0]["consent_path"]
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_usability_report(
                missing_consent,
                qualifying=True,
                repository=HumanBlobRepository(blobs),  # type: ignore[arg-type]
                source_commit="c" * 40,
            )

    def test_human_bundle_paths_cannot_point_at_source_templates(self) -> None:
        report, blobs = qualifying_usability_report()
        report["participants"][0]["trace_path"] = (
            "tests/codex/usability/templates/"
            "sprint11-human-usability-trace.template.json"
        )
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_usability_report(
                report,
                qualifying=True,
                repository=HumanBlobRepository(blobs),  # type: ignore[arg-type]
                source_commit="c" * 40,
            )

    def test_recomputed_authority_cannot_override_trace_independence(
        self,
    ) -> None:
        report, blobs = qualifying_usability_report()
        participant = report["participants"][1]
        authority_path = participant["authority_path"]
        authority = json.loads(blobs[authority_path])
        authority["observations"]["implementation_independence"] = (
            "independent"
        )
        projection = {
            key: value
            for key, value in authority.items()
            if key != "attestation_projection_sha256"
        }
        authority["attestation_projection_sha256"] = (
            acceptance.human_usability.digest_document(projection)
        )
        authority_blob = acceptance.human_usability.canonical_json(authority)
        blobs[authority_path] = authority_blob
        participant["authority_sha256"] = acceptance.sha256_bytes(
            authority_blob
        )
        participant["external_attestation_independence"] = "independent"
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.validate_usability_report(
                report,
                qualifying=True,
                repository=HumanBlobRepository(blobs),  # type: ignore[arg-type]
                source_commit="c" * 40,
            )

    def test_redaction_vectors_are_rejected(self) -> None:
        for case in self.vectors["redaction_cases"]:
            value = {"status": "fixture_valid", case["field"]: case["value"]}
            with self.assertRaises(
                acceptance.AcceptanceError,
                msg=case["id"],
            ):
                acceptance.safe_evidence_scan(value)

    def test_redaction_rejects_embedded_absolute_paths_but_keeps_project_uris(
        self,
    ) -> None:
        leaks = (
            "project=/Users/alice/private/main.tscn",
            "root:/opt/private/package",
            "open(/Applications/ChatGPT.app)",
            "roots[/Volumes/private/project]",
            "source=file:///private/project/main.gd",
            r"project=C:\Users\alice\private\main.tscn",
            "project=C:/Users/alice/private/main.tscn",
            r"share=\\private-server\account\project",
            r"path=\Users\alice\private\main.tscn",
            "share=//private-server/account/project",
        )
        for leak in leaks:
            with self.subTest(leak=leak):
                with self.assertRaises(acceptance.AcceptanceError):
                    acceptance.safe_evidence_scan({"value": leak})
                self.assertFalse(
                    acceptance.derive_surface_trace_redaction(
                        {"value": leak}
                    )["absolute_paths_absent"]
                )

        for intended in (
            "godot://runtime/status",
            "godot://scene/{scene_id}",
            "godot://resource/{id}@revision",
            "res://main.tscn",
            "tests/codex/fixtures/runtime_mvp_project/project.godot",
            "project/main.tscn",
            "./main.tscn",
            "../shared/main.tscn",
            r"project\main.tscn",
            "/usr/bin/shasum",
        ):
            with self.subTest(intended=intended):
                acceptance.safe_evidence_scan({"value": intended})
                self.assertTrue(
                    acceptance.derive_surface_trace_redaction(
                        {"value": intended}
                    )["absolute_paths_absent"]
                )

        for account_identity in (
            "alice@example.com",
            "owner=alice+capture@example.co.uk",
        ):
            with self.subTest(account_identity=account_identity):
                with self.assertRaises(acceptance.AcceptanceError):
                    acceptance.safe_evidence_scan(
                        {"value": account_identity}
                    )

        for uppercase_secret in (
            "READY:SK-PROJ-ABCDEFGHIJKLMNOP",
            "READY_SK-PROJ-ABCDEFGHIJKLMNOP",
        ):
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.safe_evidence_scan({"value": uppercase_secret})
            self.assertFalse(
                acceptance.derive_surface_trace_redaction(
                    {"value": uppercase_secret}
                )["secrets_absent"]
            )

    def test_detached_manifest_verifies_files_spaces_and_unicode(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            content = {
                "bin/godot-codex": b"operations-cli",
                "bin/godot-codex-mcp": b"mcp-server",
                "share/godot-codex/licenses/Godot-LICENSE.txt": (
                    b"Godot Engine license fixture\n"
                ),
                "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt": (
                    b"Third-party license fixture\n"
                ),
                "share/Godot Beta/readme.txt": b"space path",
                "share/\u0421\u0446\u0435\u043d\u0430/readme.txt": b"unicode path",
                (
                    "share/godot-codex/schemas/godot_codex/"
                    "sprint11-recorder-journal.schema.json"
                ): b'{"schema_version":"fixture-recorder"}\n',
                (
                    "share/godot-codex/schemas/godot_codex/"
                    "sprint11-surface-capture-artifact.schema.json"
                ): b'{"schema_version":"fixture-capture"}\n',
                (
                    "share/godot-codex/schemas/godot_codex/"
                    "sprint11-surface-metadata.schema.json"
                ): b'{"schema_version":"fixture-metadata"}\n',
            }
            records = []
            for relative, data in sorted(content.items()):
                path = root.joinpath(*Path(relative).parts)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
                records.append(
                    {
                        "path": relative,
                        "sha256": acceptance.sha256_bytes(data),
                        "bytes": len(data),
                        "mode": "0755" if relative.startswith("bin/") else "0644",
                    }
                )
            archive_buffer = io.BytesIO()
            with tarfile.open(fileobj=archive_buffer, mode="w:gz") as archive:
                directories = {"godot-codex-beta"}
                for relative in content:
                    path = Path("godot-codex-beta") / relative
                    directories.update(
                        parent.as_posix()
                        for parent in path.parents
                        if parent != Path(".")
                    )
                for directory in sorted(
                    directories,
                    key=lambda value: (len(Path(value).parts), value),
                ):
                    info = tarfile.TarInfo(directory + "/")
                    info.type = tarfile.DIRTYPE
                    info.mode = 0o755
                    info.uid = 0
                    info.gid = 0
                    info.uname = "root"
                    info.gname = "root"
                    info.mtime = 0
                    archive.addfile(info)
                for relative, data in sorted(content.items()):
                    info = tarfile.TarInfo(
                        f"godot-codex-beta/{relative}"
                    )
                    info.size = len(data)
                    info.mode = (
                        0o755 if relative.startswith("bin/") else 0o644
                    )
                    info.uid = 0
                    info.gid = 0
                    info.uname = "root"
                    info.gname = "root"
                    info.mtime = 0
                    archive.addfile(info, io.BytesIO(data))
            archive_data = archive_buffer.getvalue()
            archive_path = root / "dist" / "godot-codex-beta.tar.gz"
            archive_path.parent.mkdir(parents=True)
            archive_path.write_bytes(archive_data)
            manifest = {
                "schema_version": "s11-package-manifest/1.0",
                "package_version": "0.1.0-beta.1",
                "source_commit": "a" * 40,
                "build_provenance": {
                    "cargo_lock_sha256": digest("1"),
                    "cargo_version": "cargo 1.91.0 fixture",
                    "fresh_target": True,
                    "rust_toolchain_sha256": digest("2"),
                    "rustc_commit": "b" * 40,
                    "rustc_release": "1.91.0",
                    "target_triple": "aarch64-apple-darwin",
                },
                "archive": {
                    "path": "dist/godot-codex-beta.tar.gz",
                    "sha256": acceptance.sha256_bytes(archive_data),
                    "bytes": len(archive_data),
                },
                "contents": records,
                "compatibility_matrix_sha256": digest("9"),
                "registry_sha256": digest("3"),
                "third_party_licenses_sha256": next(
                    record["sha256"]
                    for record in records
                    if record["path"]
                    == "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt"
                ),
                "godot_prerequisite": {
                    "version": "4.8.dev.codex.b225f77ac",
                    "commit": acceptance.SPRINT10_SOURCE_COMMIT,
                    "sha256": acceptance.EXACT_GODOT_PREREQUISITE_SHA256,
                    "architecture": "arm64",
                    "expected_install_path": acceptance.EXACT_GODOT_INSTALL_PATH,
                    "verification": copy.deepcopy(
                        acceptance.EXACT_GODOT_VERIFICATION
                    ),
                },
            }
            acceptance.validate_detached_package_manifest(
                manifest,
                manifest_relative_path=(
                    "dist/godot-codex-beta.manifest.json"
                ),
                artifact_root=root,
                expected_source_commit="a" * 40,
            )
            for required_schema in (
                set(acceptance.CAPTURE_REQUIRED_PACKAGE_MODES)
                - {"bin/godot-codex", "bin/godot-codex-mcp"}
            ):
                changed = copy.deepcopy(manifest)
                changed["contents"] = [
                    record
                    for record in changed["contents"]
                    if record["path"] != required_schema
                ]
                with self.subTest(required_schema=required_schema):
                    with self.assertRaisesRegex(
                        acceptance.AcceptanceError,
                        "capture schema",
                    ):
                        acceptance.validate_detached_package_manifest(
                            changed,
                            manifest_relative_path=(
                                "dist/godot-codex-beta.manifest.json"
                            ),
                            artifact_root=root,
                            expected_source_commit="a" * 40,
                        )
            changed = copy.deepcopy(manifest)
            changed["contents"][0]["path"] = (
                "dist/godot-codex-beta.manifest.json"
            )
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_detached_package_manifest(
                    changed,
                    manifest_relative_path=(
                        "dist/godot-codex-beta.manifest.json"
                    ),
                    artifact_root=root,
                    expected_source_commit="a" * 40,
                )
            changed = copy.deepcopy(manifest)
            changed["third_party_licenses_sha256"] = digest("0")
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_detached_package_manifest(
                    changed,
                    manifest_relative_path=(
                        "dist/godot-codex-beta.manifest.json"
                    ),
                    artifact_root=root,
                    expected_source_commit="a" * 40,
                )
            changed = copy.deepcopy(manifest)
            changed["godot_prerequisite"]["verification"]["version"] = [
                "<godot-binary>",
                "--verbose",
            ]
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_detached_package_manifest(
                    changed,
                    manifest_relative_path=(
                        "dist/godot-codex-beta.manifest.json"
                    ),
                    artifact_root=root,
                    expected_source_commit="a" * 40,
                )
            changed = copy.deepcopy(manifest)
            changed["archive"]["sha256"] = digest("0")
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_detached_package_manifest(
                    changed,
                    manifest_relative_path=(
                        "dist/godot-codex-beta.manifest.json"
                    ),
                    artifact_root=root,
                    expected_source_commit="a" * 40,
                )
            malicious = io.BytesIO()
            with tarfile.open(fileobj=malicious, mode="w:gz") as archive:
                root_info = tarfile.TarInfo("godot-codex-beta/")
                root_info.type = tarfile.DIRTYPE
                root_info.mode = 0o755
                root_info.uid = root_info.gid = root_info.mtime = 0
                root_info.uname = root_info.gname = "root"
                archive.addfile(root_info)
                link = tarfile.TarInfo(
                    "godot-codex-beta/bin/godot-codex"
                )
                link.type = tarfile.SYMTYPE
                link.linkname = "../outside"
                link.mode = 0o755
                link.uid = link.gid = link.mtime = 0
                link.uname = link.gname = "root"
                archive.addfile(link)
            malicious_bytes = malicious.getvalue()
            archive_path.write_bytes(malicious_bytes)
            changed = copy.deepcopy(manifest)
            changed["archive"]["sha256"] = acceptance.sha256_bytes(
                malicious_bytes
            )
            changed["archive"]["bytes"] = len(malicious_bytes)
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_detached_package_manifest(
                    changed,
                    manifest_relative_path=(
                        "dist/godot-codex-beta.manifest.json"
                    ),
                    artifact_root=root,
                    expected_source_commit="a" * 40,
                )

    def test_package_path_vectors(self) -> None:
        for case in self.vectors["package_path_cases"]:
            if case["expected"] == "accept":
                self.assertEqual(
                    acceptance._safe_relative_path(
                        case["path"],
                        label="vector",
                    ),
                    case["path"],
                )
            else:
                with self.assertRaises(acceptance.AcceptanceError):
                    acceptance._safe_relative_path(
                        case["path"],
                        label="vector",
                    )

    def test_strict_json_rejects_duplicate_and_non_finite_values(self) -> None:
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.strict_json_bytes(
                b'{"status":"one","status":"two"}',
                label="duplicate",
            )
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.strict_json_bytes(
                b'{"duration":NaN}',
                label="non-finite",
            )

    def test_source_digest_and_evidence_only_relation_use_git_objects(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", root], check=True)
            for key, value in (
                ("user.name", "Sprint 11 Contract Test"),
                ("user.email", "s11-contract@example.invalid"),
            ):
                subprocess.run(
                    ["git", "-C", root, "config", key, value],
                    check=True,
                )
            required = sorted(acceptance.REQUIRED_SOURCE_PATHS)
            for relative in required:
                path = root.joinpath(*Path(relative).parts)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(
                    "\n".join(required) + "\n"
                    if relative.endswith("sprint11_source_scopes.txt")
                    else relative,
                    encoding="utf-8",
                )
            subprocess.run(["git", "-C", root, "add", "."], check=True)
            subprocess.run(
                ["git", "-C", root, "commit", "-qm", "source"],
                check=True,
            )
            repository = acceptance.GitRepository(root)
            source_commit = repository.head()
            source_digest, file_count = repository.source_digest(
                source_commit,
                "tests/codex/sprint11_source_scopes.txt",
            )
            source = {
                "commit": source_commit,
                "sha256": source_digest,
                "file_count": file_count,
                "scope_manifest": "tests/codex/sprint11_source_scopes.txt",
            }
            self.assertEqual(
                acceptance.validate_source_binding(source, repository),
                source_commit,
            )
            evidence = (
                root
                / "tests"
                / "codex"
                / "evidence"
                / "sprint-11-external-codex-beta-macos.json"
            )
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_evidence_checkout_relation(
                    source_commit,
                    evidence,
                    repository,
                )
            evidence.parent.mkdir(parents=True)
            evidence.write_text('{"fixture":true}', encoding="utf-8")
            self.assertEqual(
                acceptance.validate_evidence_checkout_relation(
                    source_commit,
                    evidence,
                    repository,
                ),
                "qualifying_parent",
            )
            subprocess.run(
                ["git", "-C", root, "add", evidence.relative_to(root)],
                check=True,
            )
            subprocess.run(
                ["git", "-C", root, "commit", "-qm", "evidence"],
                check=True,
            )
            self.assertEqual(
                acceptance.validate_evidence_checkout_relation(
                    source_commit,
                    evidence,
                    repository,
                ),
                "evidence_commit",
            )
            (root / "unrelated.txt").write_text("dirty", encoding="utf-8")
            with self.assertRaises(acceptance.AcceptanceError):
                acceptance.validate_evidence_checkout_relation(
                    source_commit,
                    evidence,
                    repository,
                )

    def test_source_digest_binds_git_mode_and_scope_excludes_evidence_ancestors(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", root], check=True)
            for key, value in (
                ("user.name", "Sprint 11 Contract Test"),
                ("user.email", "s11-contract@example.invalid"),
            ):
                subprocess.run(
                    ["git", "-C", root, "config", key, value],
                    check=True,
                )
            manifest = root / "scope.txt"
            payload = root / "payload.txt"
            manifest.write_text("payload.txt\nscope.txt\n", encoding="utf-8")
            payload.write_text("same bytes\n", encoding="utf-8")
            subprocess.run(["git", "-C", root, "add", "."], check=True)
            subprocess.run(
                ["git", "-C", root, "commit", "-qm", "source"],
                check=True,
            )
            repository = acceptance.GitRepository(root)
            with mock.patch.object(
                acceptance,
                "REQUIRED_SOURCE_PATHS",
                ("payload.txt", "scope.txt"),
            ):
                first_digest, first_count = repository.source_digest(
                    repository.head(),
                    "scope.txt",
                )
            payload.chmod(0o755)
            subprocess.run(["git", "-C", root, "add", "payload.txt"], check=True)
            subprocess.run(
                ["git", "-C", root, "commit", "-qm", "mode"],
                check=True,
            )
            with mock.patch.object(
                acceptance,
                "REQUIRED_SOURCE_PATHS",
                ("payload.txt", "scope.txt"),
            ):
                second_digest, second_count = repository.source_digest(
                    repository.head(),
                    "scope.txt",
                )
            self.assertEqual(first_count, second_count)
            self.assertNotEqual(first_digest, second_digest)

        required = "\n".join(
            sorted(
                {
                    *acceptance.REQUIRED_SOURCE_PATHS,
                    "tests/codex",
                }
            )
        )
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.parse_source_scopes(required + "\n")

    def test_reproducibility_must_match_the_qualified_release(self) -> None:
        source_commit = "a" * 40
        godot_sha256 = digest("b")
        runner = b"qualified build runner\n"
        runner_sha256 = acceptance.sha256_bytes(runner)
        reference = {
            "qualified_manifest_sha256": digest("c"),
            "qualified_archive_sha256": digest("d"),
            "qualified_archive_bytes": 1024,
            "qualified_package_tree_sha256": digest("e"),
            "qualified_build_provenance_sha256": digest("f"),
        }
        commands = []
        for build_id in ("a", "b"):
            template = list(
                acceptance.external_acquisitions.reproducibility_command_template(
                    build_id
                )
            )
            commands.append(
                {
                    "id": f"clean_rebuild_{build_id}",
                    "cwd": ".",
                    "argv_template": template,
                    "command_sha256": acceptance.sha256_bytes(
                        acceptance.canonical_json(
                            {"cwd": ".", "argv": template}
                        )
                    ),
                    "runner_path": acceptance.external_acquisitions.BUILD_RUNNER,
                    "runner_sha256": runner_sha256,
                    "stdout_sha256": digest("1"),
                    "stderr_sha256": digest("2"),
                    "exit_code": 0,
                    "duration_ms": 1,
                }
            )
        outputs = [
            {
                "id": build_id,
                "manifest_sha256": reference["qualified_manifest_sha256"],
                "build_provenance_sha256": reference[
                    "qualified_build_provenance_sha256"
                ],
                "archive_sha256": reference["qualified_archive_sha256"],
                "archive_bytes": reference["qualified_archive_bytes"],
                "package_tree_sha256": reference[
                    "qualified_package_tree_sha256"
                ],
            }
            for build_id in ("a", "b")
        ]
        receipt = {
            "schema_version":
                acceptance.external_acquisitions.REPRO_RECEIPT_SCHEMA,
            "capture_kind":
                acceptance.external_acquisitions.REPRO_CAPTURE_KIND,
            "status": "passed",
            "bindings": {
                "source_commit": source_commit,
                "godot_artifact_sha256": godot_sha256,
                "build_runner_path":
                    acceptance.external_acquisitions.BUILD_RUNNER,
                "build_runner_sha256": runner_sha256,
                **reference,
            },
            "commands": commands,
            "outputs": outputs,
            "reproducibility": {
                "manifest_bytes_equal": True,
                "archive_bytes_equal": True,
                "package_tree_equal": True,
                "source_checkout_unchanged": True,
            },
            "cleanup": {
                "build_processes_stopped": True,
                "repository_unchanged": True,
            },
            "redaction": {
                "absolute_paths_absent": True,
                "environment_secrets_absent": True,
                "source_content_absent": True,
            },
        }
        repository = mock.Mock()
        repository.blob_at.return_value = runner
        self.assertEqual(
            acceptance.validate_reproducibility_receipt(
                receipt,
                repository=repository,
                expected_source_commit=source_commit,
                godot_artifact_sha256=godot_sha256,
                qualified_reference=reference,
            ),
            set(),
        )

        unrelated_twins = copy.deepcopy(receipt)
        for output in unrelated_twins["outputs"]:
            output["archive_sha256"] = digest("9")
        with self.assertRaisesRegex(
            acceptance.AcceptanceError,
            "differ from the qualified release package",
        ):
            acceptance.validate_reproducibility_receipt(
                unrelated_twins,
                repository=repository,
                expected_source_commit=source_commit,
                godot_artifact_sha256=godot_sha256,
                qualified_reference=reference,
            )


if __name__ == "__main__":
    unittest.main()
