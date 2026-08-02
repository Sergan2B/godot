from __future__ import annotations

import copy
import hashlib
import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from typing import Any

from tests.codex.usability import sprint11_fault_harness as harness
from tests.codex.usability import validator

D = "sha256:" + "a" * 64
D2 = "sha256:" + "b" * 64
COMMIT = "c" * 40
RUN = "s11u-run:" + "1" * 32
PARTICIPANT = "participant:sha256:" + "2" * 64
OPERATOR = "operator:sha256:" + "3" * 64


def artifact_binding(field: str) -> dict[str, str]:
    identifier, relative = validator.SOURCE_ARTIFACTS[field]
    return {
        "id": identifier,
        "path": relative,
        "sha256": validator.file_digest(validator.REPOSITORY / relative),
    }


def synthetic_consent() -> dict[str, Any]:
    consent_path = "tests/codex/usability/sprint11-consent-v1.md"
    return {
        "schema_version": "s11-human-consent-receipt/1.0",
        "acquisition_kind": "synthetic_contract_fixture",
        "qualification": False,
        "status": "fixture_valid",
        "payload": {
            "run_id": RUN,
            "participant_id": PARTICIPANT,
            "operator_id": OPERATOR,
            "consent_document": {
                "id": "sprint11-human-usability-consent",
                "version": "v1",
                "path": consent_path,
                "sha256": validator.file_digest(
                    validator.REPOSITORY / consent_path
                ),
            },
            "accepted": True,
            "recorded_before_tasks": True,
            "voluntary_and_stop_explained": True,
            "withdrawal_deadline_explained": True,
            "retention_policy_id": (
                "private-ledger-until-withdrawal-deadline-v1"
            ),
            "collected_data_categories": [
                "salted_pseudonym",
                "bounded_task_outcomes",
                "bounded_timings",
                "wrong_turn_counts",
                "help_source_ids",
                "comprehension_scores",
                "content_free_event_digests",
            ],
            "excluded_data_categories": [
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
            ],
            "pseudonym": {
                "algorithm": "salted-sha256-domain-separated-v1",
                "domain_separator": "s11-human-participant-v1",
                "salt_minimum_bytes": 32,
                "salt_stored_in_evidence": False,
                "subject_key_stored_in_evidence": False,
                "custody": (
                    "private_consent_ledger_until_withdrawal_deadline"
                ),
            },
            "content_free": {
                "pii_fields": 0,
                "prompt_fields": 0,
                "secret_fields": 0,
                "private_path_fields": 0,
            },
            "receipt_projection_sha256": D,
        },
    }


def synthetic_defects() -> dict[str, Any]:
    return {
        "schema_version": "s11-human-usability-defect-ledger/1.0",
        "acquisition_kind": "synthetic_contract_fixture",
        "qualification": False,
        "status": "fixture_valid",
        "payload": {
            "ledger_id": "sprint11-human-usability-defect-ledger-v1",
            "bound_human_run_ids": [RUN],
            "defects": [],
            "redaction": {
                "scan_receipt_sha256": D,
                "freeform_issue_text_embedded": False,
                "participant_data_embedded": False,
                "secret_occurrences": 0,
                "private_path_occurrences": 0,
            },
        },
    }


def timeline_and_refs(
    fault_order: list[str],
) -> tuple[
    list[dict[str, Any]],
    dict[str, tuple[str, str]],
    dict[str, tuple[str, str, str]],
    dict[str, tuple[str, str]],
    list[str],
]:
    events: list[dict[str, Any]] = []
    interventions: list[str] = []

    def add(actor: str, kind: str, subject: str, outcome: str = "observed") -> str:
        sequence = len(events) + 1
        event_id = f"event:{sequence:04d}"
        events.append(
            {
                "sequence": sequence,
                "event_id": event_id,
                "elapsed_ms": sequence * 100,
                "actor": actor,
                "kind": kind,
                "subject_id": subject,
                "outcome": outcome,
                "projection_sha256": D,
            }
        )
        mapping = {
            "consent_recorded": "consent",
            "fault_injected": "fault_inject",
            "fault_reset": "fault_reset",
        }
        if actor == "operator":
            interventions.append(mapping[kind])
        return event_id

    add("system", "run_started", "run")
    add("operator", "consent_recorded", "consent", "passed")
    add("system", "clean_start_verified", "clean_start", "passed")
    add("system", "fault_order_committed", "doctor_fault_matrix", "passed")
    task_refs = {}
    for goal in validator.GOALS:
        started = add("participant", "task_started", goal)
        completed = add("participant", "task_completed", goal, "passed")
        task_refs[goal] = (started, completed)
    fault_refs = {}
    for scenario in fault_order:
        injected = add("operator", "fault_injected", scenario)
        diagnosed = add("participant", "fault_diagnosed", scenario, "passed")
        reset = add("operator", "fault_reset", scenario, "passed")
        fault_refs[scenario] = (injected, diagnosed, reset)
    comprehension_refs = {}
    for comprehension_id in validator.COMPREHENSION:
        asked = add(
            "system", "comprehension_asked", comprehension_id, "observed"
        )
        scored = add(
            "system", "comprehension_scored", comprehension_id, "passed"
        )
        comprehension_refs[comprehension_id] = (asked, scored)
    add("system", "run_completed", "run", "passed")
    return (
        events,
        task_refs,
        fault_refs,
        comprehension_refs,
        interventions,
    )


def synthetic_trace(consent: dict[str, Any]) -> dict[str, Any]:
    fault_order, commitment = validator.randomized_fault_order(RUN, b"\x11" * 32)
    (
        events,
        task_refs,
        fault_refs,
        comprehension_refs,
        interventions,
    ) = timeline_and_refs(fault_order)
    tasks = []
    for sequence, goal in enumerate(validator.GOALS, start=1):
        tasks.append(
            {
                "sequence": sequence,
                "goal_id": goal,
                "status": "passed",
                "duration_ms": 1000,
                "wrong_turns": 0,
                "help_source_ids": [],
                "remediation_succeeded": True,
                "semantic_assertion_ids": list(validator.ASSERTIONS[goal]),
                "started_event": task_refs[goal][0],
                "completed_event": task_refs[goal][1],
            }
        )
    faults = []
    for sequence, scenario in enumerate(fault_order, start=1):
        code, remediation = validator.FAULTS[scenario]
        injected, diagnosed, reset = fault_refs[scenario]
        faults.append(
            {
                "sequence": sequence,
                "scenario": scenario,
                "expected_diagnostic_code": code,
                "observed_diagnostic_code": code,
                "expected_remediation_id": remediation,
                "observed_remediation_id": remediation,
                "status": "passed",
                "remediation_succeeded": True,
                "reset_verified": True,
                "injected_event": injected,
                "diagnosed_event": diagnosed,
                "reset_event": reset,
            }
        )
    bindings = {
        "package": {
            "source_commit": COMMIT,
            "manifest_sha256": D,
            "archive_sha256": D2,
            "package_tree_sha256": D,
            "operations_binary_sha256": D,
            "mcp_binary_sha256": D2,
            "godot_prerequisite_sha256": D,
        },
        "fixture": {
            "fixture_id": "sprint11-usability-fixture-v1",
            "tree_sha256": D,
            "project_file_sha256": D2,
        },
        "surface": {
            "surface": "cli",
            "host_coordinate_id": "codex-cli-candidate",
            "host_coordinate_sha256": D,
            "host_artifact_sha256": D2,
            "host_tree_sha256": D,
            "host_version": "0.146.0-alpha.3.1",
            "host_build": "0.146.0-alpha.3.1",
            "architecture": "arm64",
        },
        **{
            field: artifact_binding(field)
            for field in validator.SOURCE_ARTIFACTS
        },
    }
    return {
        "schema_version": "s11-human-usability-trace/1.0",
        "acquisition_kind": "synthetic_contract_fixture",
        "qualification": False,
        "status": "fixture_valid",
        "payload": {
            "run_id": RUN,
            "participant": {
                "participant_id": PARTICIPANT,
                "implementation_independent": False,
                "consent_recorded": True,
                "consent_receipt_sha256": validator.digest_document(consent),
                "pseudonym_algorithm": (
                    "salted-sha256-domain-separated-v1"
                ),
                "salt_custody": (
                    "private_consent_ledger_until_withdrawal_deadline"
                ),
            },
            "bindings": bindings,
            "clean_start": {
                "receipt_path": "acquisition/clean-start.json",
                "receipt_sha256": D,
                "disposable_fixture_copy": True,
                "source_fixture_unchanged": True,
                "no_preexisting_project_config": True,
                "no_preexisting_bridge_metadata": True,
                "no_preexisting_offline_cache": True,
                "fresh_surface_task": True,
                "frozen_archive_verified": True,
                "package_manifest_verified": True,
            },
            "assistance_policy": {
                "allowed_help_sources": list(validator.HELP_SOURCES),
                "developer_coaching": False,
                "fixture_answers_disclosed": False,
                "raw_prompts_recorded": False,
                "operator_intervention_codes": interventions,
            },
            "fault_randomization": {
                "algorithm": "sha256-seeded-sort-v1",
                "seed_commitment": commitment,
                "committed_before_first_fault": True,
                "order": fault_order,
            },
            "tasks": tasks,
            "doctor_faults": faults,
            "timeline": events,
            "comprehension": [
                {
                    "comprehension_id": item,
                    "result": "correct",
                    "asked_event": comprehension_refs[item][0],
                    "scored_event": comprehension_refs[item][1],
                }
                for item in validator.COMPREHENSION
            ],
            "metrics": {
                "first_useful_status_ms": 120_000,
                "connected_live_query_ms": 300_000,
                "secret_copy_count": 0,
                "manual_toml_steps": 0,
                "project_integrity_failures": 0,
                "project_isolation_failures": 0,
                "fault_recoveries": 5,
            },
            "redaction": {
                "scan_definition_sha256": D,
                "scan_receipt_sha256": D2,
                "forbidden_field_occurrences": 0,
                "secret_canary_occurrences": 0,
                "private_absolute_path_occurrences": 0,
                "raw_prompt_occurrences": 0,
                "pii_occurrences": 0,
                "native_handle_occurrences": 0,
                "unrestricted_content_occurrences": 0,
            },
            "cleanup": {
                "all_faults_reset": True,
                "disposable_fixture_removed": True,
                "source_fixture_unchanged": True,
                "package_unchanged": True,
                "private_salt_not_in_artifacts": True,
                "private_consent_ledger_outside_repository": True,
            },
            "attestation": {
                "operator_id": OPERATOR,
                "operator_attestation_sha256": D,
                "participant_summary_confirmed": True,
                "participant_confirmation_sha256": D2,
                "content_minimization_confirmed": True,
                "no_developer_coaching_confirmed": True,
            },
            "unresolved_defect_ids": [],
        },
    }


def synthetic_rubric(
    trace: dict[str, Any], defects: dict[str, Any]
) -> dict[str, Any]:
    payload = trace["payload"]
    task_scores = []
    for task in payload["tasks"]:
        event_id = task["completed_event"]
        task_scores.append(
            {
                "goal_id": task["goal_id"],
                "result": "passed",
                "criteria": [
                    {
                        "criterion_id": criterion,
                        "result": "passed",
                        "evidence_event_ids": [event_id],
                    }
                    for criterion in validator.CRITERIA[task["goal_id"]]
                ],
            }
        )
    fault_scores = []
    for fault in payload["doctor_faults"]:
        fault_scores.append(
            {
                "scenario": fault["scenario"],
                "diagnostic_code": fault["observed_diagnostic_code"],
                "remediation_id": fault["observed_remediation_id"],
                "diagnosis_result": "passed",
                "remediation_result": "passed",
                "reset_result": "passed",
                "evidence_event_ids": [
                    fault["injected_event"],
                    fault["diagnosed_event"],
                    fault["reset_event"],
                ],
            }
        )
    target_scores = []
    for target_id, (limit, comparison) in validator.TARGETS.items():
        target_scores.append(
            {
                "target_id": target_id,
                "observed": payload["metrics"][target_id],
                "limit": limit,
                "comparison": comparison,
                "result": "passed",
            }
        )
    bindings = payload["bindings"]
    return {
        "schema_version": "s11-human-usability-rubric/1.0",
        "acquisition_kind": "synthetic_contract_fixture",
        "qualification": False,
        "status": "fixture_valid",
        "payload": {
            "rubric_id": "sprint11-human-usability-rubric-v1",
            "trace": {
                "path": "acquisition/trace.json",
                "sha256": validator.digest_document(trace),
                "run_id": RUN,
                "participant_id": PARTICIPANT,
            },
            "coordinates": {
                "package_source_commit": (
                    bindings["package"]["source_commit"]
                ),
                "package_manifest_sha256": (
                    bindings["package"]["manifest_sha256"]
                ),
                "fixture_tree_sha256": bindings["fixture"]["tree_sha256"],
                "surface": bindings["surface"]["surface"],
                "host_coordinate_sha256": (
                    bindings["surface"]["host_coordinate_sha256"]
                ),
                "prompt_pack": bindings["prompt_pack"],
                "participant_script": bindings["participant_script"],
                "rubric_schema": bindings["rubric_schema"],
                "rubric_template": bindings["rubric_template"],
            },
            "task_scores": task_scores,
            "fault_scores": fault_scores,
            "comprehension_scores": [
                {
                    "comprehension_id": result["comprehension_id"],
                    "result": result["result"],
                    "evidence_event_id": result["scored_event"],
                }
                for result in payload["comprehension"]
            ],
            "target_scores": target_scores,
            "defects": {
                "ledger_path": "acquisition/defects.json",
                "ledger_sha256": validator.digest_document(defects),
                "unresolved_defect_ids": [],
                "unresolved_high_or_critical": 0,
            },
            "overall": {
                "scoring_rule": (
                    "all-required-criteria-faults-comprehension-targets-v1"
                ),
                "result": "passed",
                "all_required_task_criteria_passed": True,
                "all_faults_recovered": True,
                "all_comprehension_correct": True,
                "all_targets_met": True,
                "no_high_or_critical_open": True,
            },
            "attestation": {
                "assessor_id": OPERATOR,
                "trace_digest_verified": True,
                "frozen_rubric_verified": True,
                "no_freeform_notes_recorded": True,
                "no_developer_coaching_confirmed": True,
                "assessment_projection_sha256": D,
            },
        },
    }


def synthetic_bundle() -> tuple[dict[str, Any], ...]:
    consent = synthetic_consent()
    defects = synthetic_defects()
    trace = synthetic_trace(consent)
    rubric = synthetic_rubric(trace, defects)
    return trace, rubric, consent, defects


def relabelled_real_bundle(
    *,
    implementation_independent: bool,
) -> tuple[dict[str, Any], ...]:
    trace, rubric, consent, defects = synthetic_bundle()
    consent.update(
        acquisition_kind="real_human",
        status="recorded",
    )
    defects.update(
        acquisition_kind="real_human_derived",
        qualification=True,
        status="qualifying",
    )
    trace.update(
        acquisition_kind="real_human",
        qualification=True,
        status="passed",
    )
    trace["payload"]["participant"]["implementation_independent"] = (
        implementation_independent
    )
    trace["payload"]["participant"]["consent_receipt_sha256"] = (
        validator.digest_document(consent)
    )
    rubric.update(
        acquisition_kind="real_human",
        qualification=True,
        status="passed",
    )
    rubric["payload"]["trace"]["sha256"] = validator.digest_document(trace)
    rubric["payload"]["defects"]["ledger_sha256"] = validator.digest_document(
        defects
    )
    return trace, rubric, consent, defects


def source_bound_authority(
    bundle: tuple[dict[str, Any], ...],
    *,
    implementation_independent: bool,
) -> tuple[
    validator.SourceBoundAcquisitionAuthority,
    dict[str, str],
]:
    trace, rubric, consent, defects = bundle
    artifact_sha256 = {
        "trace": validator.digest_bytes(validator.canonical_json(trace)),
        "rubric": validator.digest_bytes(validator.canonical_json(rubric)),
        "consent": validator.digest_bytes(validator.canonical_json(consent)),
        "defect_ledger": validator.digest_bytes(
            validator.canonical_json(defects)
        ),
    }
    projection = {
        "schema_version": "s11-human-acquisition-authority/1.0",
        "authority_kind": "external_operator_attestation",
        "authority_schema": {
            "id": validator.AUTHORITY_SCHEMA_ARTIFACT[0],
            "path": validator.AUTHORITY_SCHEMA_ARTIFACT[1],
            "sha256": validator.file_digest(
                validator.REPOSITORY / validator.AUTHORITY_SCHEMA_ARTIFACT[1]
            ),
        },
        "trust_boundary": validator.AUTHORITY_TRUST_BOUNDARY,
        "operator_id": OPERATOR,
        "run_id": RUN,
        "participant_id": PARTICIPANT,
        "artifact_directory": validator.human_acquisition_directory(RUN),
        "observations": {
            "real_person_observed": True,
            "consent_observed_before_tasks": True,
            "implementation_independence": (
                "independent"
                if implementation_independent
                else "not_independent"
            ),
            "developer_coaching_absent": True,
            "fixture_answers_absent": True,
        },
        "artifact_sha256": artifact_sha256,
    }
    document = {
        **projection,
        "attestation_projection_sha256": validator.digest_document(projection),
    }
    document_bytes = validator.canonical_json(document)
    digest = validator.digest_bytes(document_bytes)
    authority = validator.validate_source_bound_acquisition_authority(
        document_bytes,
        source_commit=COMMIT,
        relative_path=(
            f"{validator.human_acquisition_directory(RUN)}/authority.json"
        ),
        expected_sha256=digest,
    )
    return authority, artifact_sha256


class HumanAcquisitionContractTests(unittest.TestCase):
    def copy_frozen_materials(self, destination: Path) -> Path:
        protocol_relative = (
            "tests/codex/usability/sprint11-operator-protocol-v1.json"
        )
        protocol = validator.load_json(validator.REPOSITORY / protocol_relative)
        relatives = {
            protocol_relative,
            "tests/codex/usability/sprint11-participant-script-v1.json",
            *(binding["path"] for binding in protocol["contract_bindings"]),
        }
        for relative in relatives:
            source = validator.REPOSITORY / relative
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
        return destination

    def test_frozen_materials_and_nonqualifying_templates_validate(self) -> None:
        validator.validate_frozen_materials()
        for kind, (relative, _schema) in validator.TEMPLATES.items():
            document = validator.load_json(validator.REPOSITORY / relative)
            self.assertEqual(document["acquisition_kind"], "template", kind)
            self.assertIs(document["qualification"], False, kind)
            self.assertEqual(document["status"], "unacquired", kind)

    def test_operator_protocol_is_closed_below_the_root(self) -> None:
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="s11u.") as directory:
            repository = self.copy_frozen_materials(Path(directory))
            protocol_path = (
                repository
                / "tests/codex/usability/sprint11-operator-protocol-v1.json"
            )
            protocol = validator.load_json(protocol_path)
            protocol["operator_interventions"]["prohibited"].remove(
                "fixture_answer"
            )
            protocol_path.write_text(
                json.dumps(protocol, sort_keys=True, separators=(",", ":")),
                encoding="utf-8",
            )
            with self.assertRaises(validator.AcquisitionError):
                validator.validate_frozen_materials(repository)

    def test_participant_script_has_seven_goals_and_no_fault_answers(self) -> None:
        script = validator.load_json(
            validator.REPOSITORY
            / "tests/codex/usability/sprint11-participant-script-v1.json"
        )
        self.assertEqual(
            [goal["goal_id"] for goal in script["goals"]],
            list(validator.GOALS),
        )
        participant_bytes = json.dumps(script, sort_keys=True)
        self.assertNotIn("host.unsupported_form", participant_bytes)
        for scenario, (code, remediation) in validator.FAULTS.items():
            self.assertNotIn(scenario, participant_bytes)
            self.assertNotIn(code, participant_bytes)
            self.assertNotIn(remediation, participant_bytes)

    def test_human_trace_rejects_surface_only_unsupported_form_assertion(
        self,
    ) -> None:
        trace, _rubric, _consent, _defects = synthetic_bundle()
        compound = next(
            task
            for task in trace["payload"]["tasks"]
            if task["goal_id"] == "compound_write_and_undo"
        )
        self.assertEqual(len(compound["semantic_assertion_ids"]), 8)
        compound["semantic_assertion_ids"].append("host.unsupported_form")
        with self.assertRaises(validator.AcquisitionError):
            validator.validate_trace(trace)

    def test_synthetic_bundle_is_valid_but_never_qualifies(self) -> None:
        trace, rubric, consent, defects = synthetic_bundle()
        self.assertFalse(
            validator.validate_bundle(
                trace_document=trace,
                rubric_document=rubric,
                consent_document=consent,
                defect_document=defects,
            )
        )

    def test_relabelled_bundle_requires_separate_source_bound_authority(
        self,
    ) -> None:
        bundle = relabelled_real_bundle(implementation_independent=True)
        trace, rubric, consent, defects = bundle
        self.assertFalse(
            validator.validate_bundle(
                trace_document=trace,
                rubric_document=rubric,
                consent_document=consent,
                defect_document=defects,
                allow_real_human=True,
            )
        )
        authority, artifact_sha256 = source_bound_authority(
            bundle,
            implementation_independent=True,
        )
        self.assertTrue(
            validator.validate_bundle(
                trace_document=trace,
                rubric_document=rubric,
                consent_document=consent,
                defect_document=defects,
                allow_real_human=True,
                source_bound_authority=authority,
                artifact_sha256=artifact_sha256,
            )
        )

    def test_authority_rejects_recomputed_wrong_bundle_digest(self) -> None:
        bundle = relabelled_real_bundle(implementation_independent=False)
        trace, rubric, consent, defects = bundle
        authority, artifact_sha256 = source_bound_authority(
            bundle,
            implementation_independent=False,
        )
        changed = dict(artifact_sha256)
        changed["trace"] = D2
        with self.assertRaises(validator.AcquisitionError):
            validator.validate_bundle(
                trace_document=trace,
                rubric_document=rubric,
                consent_document=consent,
                defect_document=defects,
                allow_real_human=True,
                source_bound_authority=authority,
                artifact_sha256=changed,
            )

    def test_template_relabelled_as_real_human_is_rejected(self) -> None:
        template = validator.load_json(
            validator.REPOSITORY / validator.TEMPLATES["trace"][0]
        )
        template["acquisition_kind"] = "real_human"
        template["qualification"] = True
        template["status"] = "passed"
        with self.assertRaises(validator.AcquisitionError):
            validator.validate_trace(template, allow_real_human=True)

    def test_trace_unknown_field_and_developer_coaching_are_rejected(self) -> None:
        trace, _rubric, consent, _defects = synthetic_bundle()
        changed = copy.deepcopy(trace)
        changed["payload"]["participant_name"] = "not allowed"
        with self.assertRaises(validator.AcquisitionError):
            validator.validate_trace(changed)
        changed = copy.deepcopy(trace)
        changed["payload"]["assistance_policy"]["developer_coaching"] = True
        with self.assertRaises(validator.AcquisitionError):
            validator.validate_trace(changed)
        self.assertEqual(
            trace["payload"]["participant"]["consent_receipt_sha256"],
            validator.digest_document(consent),
        )

    def test_missing_goal_and_changed_fault_mapping_are_rejected(self) -> None:
        trace, _rubric, _consent, _defects = synthetic_bundle()
        changed = copy.deepcopy(trace)
        changed["payload"]["tasks"][1]["goal_id"] = "install_and_connect"
        with self.assertRaises(validator.AcquisitionError):
            validator.validate_trace(changed)
        changed = copy.deepcopy(trace)
        changed["payload"]["doctor_faults"][0][
            "observed_diagnostic_code"
        ] = "project_config_invalid"
        with self.assertRaises(validator.AcquisitionError):
            validator.validate_trace(changed)

    def test_rubric_cannot_claim_pass_over_failed_criterion(self) -> None:
        _trace, rubric, _consent, _defects = synthetic_bundle()
        rubric["payload"]["task_scores"][0]["criteria"][0]["result"] = "failed"
        with self.assertRaises(validator.AcquisitionError):
            validator.validate_rubric(rubric)

    def test_cross_bundle_digest_swap_is_rejected(self) -> None:
        trace, rubric, consent, defects = synthetic_bundle()
        trace["payload"]["participant"]["consent_receipt_sha256"] = D2
        with self.assertRaises(validator.AcquisitionError):
            validator.validate_bundle(
                trace_document=trace,
                rubric_document=rubric,
                consent_document=consent,
                defect_document=defects,
            )

    def test_open_high_defect_cannot_be_relabelled_qualifying(self) -> None:
        defects = synthetic_defects()
        defects["payload"]["defects"] = [
            {
                "defect_id": "S11-UX-001",
                "severity": "high",
                "category": "diagnostics",
                "goal_id": "doctor_fault_matrix",
                "symptom_code": "wrong_diagnosis",
                "private_issue_projection_sha256": D,
                "found_on": {
                    "source": {
                        "commit": COMMIT,
                        "reproduction_run_ids": [
                            "s11u-source:" + "4" * 32
                        ],
                    },
                    "package": {
                        "manifest_sha256": D,
                        "reproduction_run_ids": [
                            "s11u-package:" + "5" * 32
                        ],
                    },
                    "human_run_ids": [RUN],
                },
                "status": "open",
            }
        ]
        validator.validate_defect_ledger(defects)
        defects["acquisition_kind"] = "real_human_derived"
        defects["qualification"] = True
        defects["status"] = "qualifying"
        with self.assertRaises(validator.AcquisitionError):
            validator.validate_defect_ledger(
                defects, allow_real_human=True
            )

    def test_pseudonym_and_fault_order_are_domain_separated(self) -> None:
        participant = validator.derive_participant_id(b"\x00" * 32, "local-01")
        self.assertRegex(participant, validator.PARTICIPANT_ID)
        order, commitment = validator.randomized_fault_order(RUN, b"\x01" * 32)
        self.assertEqual(set(order), set(validator.FAULTS))
        self.assertEqual(len(order), 5)
        self.assertRegex(commitment, validator.DIGEST)
        self.assertEqual(
            (order, commitment),
            validator.randomized_fault_order(RUN, b"\x01" * 32),
        )


class FaultHarnessTests(unittest.TestCase):
    def create_run(self, parent: Path) -> Path:
        run = parent / "run"
        harness.initialize_run_root(run)
        return run

    def create_project(self, run: Path) -> Path:
        project = run / "project"
        project.mkdir()
        (project / "project.godot").write_text(
            "[application]\nconfig/name=\"S11\"\n", encoding="utf-8"
        )
        return project

    def test_discovery_reset_removes_harness_created_godot_byte_for_byte(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="s11u.") as directory:
            run = self.create_run(Path(directory))
            project = self.create_project(run)
            before = sorted(path.relative_to(project) for path in project.rglob("*"))
            fault = harness.DiscoveryFault(
                run_root=run,
                project_root=project,
                state_directory=run / "fault",
                scenario="stale_discovery",
            )
            ready = fault.inject()
            self.assertTrue((project / ".godot/codex/bridge.json").is_file())
            reset = fault.reset()
            self.assertTrue(reset["exact_state_restored"])
            self.assertFalse((project / ".godot").exists())
            after = sorted(path.relative_to(project) for path in project.rglob("*"))
            self.assertEqual(before, after)
            again = harness.recover_fault(
                run_root=run,
                state_directory=run / "fault",
                project_root=project,
                package_root=None,
            )
            self.assertEqual(
                ready["pre_state_sha256"], again["post_state_sha256"]
            )

    def test_discovery_crash_recovery_restores_original_after_delete_phase(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="s11u.") as directory:
            run = self.create_run(Path(directory))
            project = self.create_project(run)
            codex = project / ".godot/codex"
            codex.mkdir(parents=True, mode=0o700)
            sentinel = codex / "sentinel"
            sentinel.write_bytes(b"original")
            os.chmod(sentinel, 0o600)
            fault = harness.DiscoveryFault(
                run_root=run,
                project_root=project,
                state_directory=run / "fault",
                scenario="version_mismatch",
            )
            fault.inject()
            assert fault.listener is not None
            fault.listener.close()
            fault.listener = None
            state = harness._load_state(run / "fault")
            harness._remove_synthetic_bridge(codex, state)
            reset = harness.recover_fault(
                run_root=run,
                state_directory=run / "fault",
                project_root=project,
                package_root=None,
            )
            self.assertTrue(reset["exact_state_restored"])
            self.assertEqual(sentinel.read_bytes(), b"original")

    def test_discovery_crash_after_backup_restore_is_reconciled(self) -> None:
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="s11u.") as directory:
            run = self.create_run(Path(directory))
            project = self.create_project(run)
            codex = project / ".godot/codex"
            codex.mkdir(parents=True, mode=0o700)
            sentinel = codex / "sentinel"
            sentinel.write_bytes(b"original")
            os.chmod(sentinel, 0o600)
            original_token = codex / "session.token"
            original_token.write_bytes(b"o" * 32)
            os.chmod(original_token, 0o600)
            fault = harness.DiscoveryFault(
                run_root=run,
                project_root=project,
                state_directory=run / "fault",
                scenario="authentication_failure",
            )
            fault.inject()
            assert fault.listener is not None
            fault.listener.close()
            fault.listener = None
            state = harness._load_state(run / "fault")
            harness._remove_synthetic_bridge(codex, state)
            backup = run / "fault/original-bridge"
            os.rename(backup / "session.token", codex / "session.token")
            backup.rmdir()
            reset = harness.recover_fault(
                run_root=run,
                state_directory=run / "fault",
                project_root=project,
                package_root=None,
            )
            self.assertTrue(reset["exact_state_restored"])
            self.assertEqual(sentinel.read_bytes(), b"original")

    def test_discovery_fault_preserves_populated_cache_and_setup_receipt(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="s11u.") as directory:
            run = self.create_run(Path(directory))
            project = self.create_project(run)
            codex = project / ".godot/codex"
            segments = codex / "index/segments"
            segments.mkdir(parents=True, mode=0o700)
            os.chmod(codex, 0o700)
            receipt = codex / "setup-receipt-v1.json"
            receipt.write_bytes(b'{"receipt":"preserved"}\n')
            os.chmod(receipt, 0o600)
            for index in range(256):
                segment = segments / f"segment-{index:04d}.json"
                segment.write_bytes(f"segment-{index}\n".encode("ascii"))
                os.chmod(segment, 0o600)
            before = harness._discovery_state_digest(project)
            fault = harness.DiscoveryFault(
                run_root=run,
                project_root=project,
                state_directory=run / "fault",
                scenario="version_mismatch",
            )
            fault.inject()
            self.assertEqual(receipt.read_bytes(), b'{"receipt":"preserved"}\n')
            self.assertTrue((segments / "segment-0255.json").is_file())
            self.assertTrue((codex / "bridge.json").is_file())
            reset = fault.reset()
            self.assertTrue(reset["exact_state_restored"])
            self.assertEqual(before, harness._discovery_state_digest(project))

    def test_exact_three_discovery_fault_contracts_are_executable(self) -> None:
        for scenario in sorted(harness.DISCOVERY_SCENARIOS):
            with self.subTest(scenario=scenario):
                with tempfile.TemporaryDirectory(
                    dir="/tmp", prefix="s11u."
                ) as directory:
                    run = self.create_run(Path(directory))
                    project = self.create_project(run)
                    fault = harness.DiscoveryFault(
                        run_root=run,
                        project_root=project,
                        state_directory=run / "fault",
                        scenario=scenario,
                    )
                    ready = fault.inject()
                    self.assertEqual(
                        ready["expected_diagnostic_code"],
                        harness.SCENARIOS[scenario][0],
                    )
                    self.assertEqual(
                        ready["expected_remediation_id"],
                        harness.SCENARIOS[scenario][1],
                    )
                    self.assertTrue(fault.reset()["exact_state_restored"])

    def fake_installed_package(self, run: Path) -> Path:
        package = run / "install/versions/1.0"
        (package / "bin").mkdir(parents=True)
        files = {
            "VERSION": b"1.0\n",
            "package-manifest.json": b'{"package_version":"1.0"}\n',
            "bin/godot-codex": b"operations\n",
            "bin/godot-codex-mcp": b"sidecar\n",
        }
        for relative, value in files.items():
            path = package / relative
            path.write_bytes(value)
            os.chmod(path, 0o755 if relative.startswith("bin/") else 0o644)
        lines = []
        for relative, value in sorted(files.items()):
            lines.append(f"{hashlib.sha256(value).hexdigest()}  {relative}\n")
        (package / "checksums.sha256").write_text(
            "".join(lines), encoding="utf-8"
        )
        manifest_digest = hashlib.sha256(files["package-manifest.json"]).hexdigest()
        (package / ".godot-codex-owned").write_text(
            manifest_digest + "\n", encoding="utf-8"
        )
        os.chmod(package / ".godot-codex-owned", 0o600)
        return package

    def test_missing_binary_injection_and_crash_recovery_are_exact(self) -> None:
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="s11u.") as directory:
            run = self.create_run(Path(directory))
            package = self.fake_installed_package(run)
            before = harness._package_snapshot(package)
            ready = harness.inject_missing_binary(
                run_root=run,
                package_root=package,
                state_directory=run / "fault",
            )
            self.assertFalse((package / "bin/godot-codex-mcp").exists())
            reset = harness.recover_fault(
                run_root=run,
                state_directory=run / "fault",
                project_root=None,
                package_root=package,
            )
            self.assertEqual(ready["pre_state_sha256"], before["tree_sha256"])
            self.assertEqual(reset["post_state_sha256"], before["tree_sha256"])
            self.assertEqual(harness._package_snapshot(package), before)

    def configure_project(self, project: Path) -> bytes:
        config = (
            "[mcp_servers.godot_editor]\n"
            f"# godot-codex-setup-owner: {D2}\n"
            'command = "/disposable/current/bin/godot-codex"\n'
            'args = ["mcp", "--project-root", "."]\n'
            "required = true\n"
            "startup_timeout_sec = 10\n"
            "tool_timeout_sec = 60\n"
            'enabled_tools = ["godot_connection_status"]\n'
        ).encode()
        (project / ".codex").mkdir()
        config_path = project / ".codex/config.toml"
        config_path.write_bytes(config)
        os.chmod(config_path, 0o600)
        codex = project / ".godot/codex"
        codex.mkdir(parents=True, mode=0o700)
        stanza = config.decode().rstrip(" \t\r\n")
        receipt = {
            "schema_version": "godot-codex-setup-receipt/1.1",
            "project_id": harness._project_id(project.resolve()),
            "package_version": "1.0.0",
            "profile": "read-only",
            "guidance": "none",
            "package_identity_digest": D,
            "launcher_path_sha256": D,
            "launcher_file_sha256": D,
            "config_ownership_marker": D2,
            "config_table_digest": harness._digest_bytes(stanza.encode()),
            "config_created": True,
            "config_file_mode": 0o600,
            "agents_block_digest": None,
            "agents_separator": None,
            "agents_file_mode": None,
            "skill_file_digest": None,
            "skill_file_mode": None,
            "plan_digest": D,
        }
        receipt_path = codex / "setup-receipt-v1.json"
        receipt_path.write_bytes(harness._canonical_json(receipt))
        os.chmod(receipt_path, 0o600)
        return config

    def test_invalid_config_is_parseable_and_recovers_exactly(self) -> None:
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="s11u.") as directory:
            run = self.create_run(Path(directory))
            project = self.create_project(run)
            config = self.configure_project(project)
            ready = harness.inject_invalid_config(
                run_root=run,
                project_root=project,
                state_directory=run / "fault",
            )
            drifted = (project / ".codex/config.toml").read_text()
            self.assertIn("required = false", drifted)
            self.assertNotIn("cwd", drifted)
            reset = harness.recover_fault(
                run_root=run,
                state_directory=run / "fault",
                project_root=project,
                package_root=None,
            )
            self.assertEqual((project / ".codex/config.toml").read_bytes(), config)
            self.assertEqual(
                ready["pre_state_sha256"], reset["post_state_sha256"]
            )

    def test_invalid_config_rejects_legacy_receipt_before_state_creation(self) -> None:
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="s11u.") as directory:
            run = self.create_run(Path(directory))
            project = self.create_project(run)
            config = self.configure_project(project)
            receipt_path = project / ".godot/codex/setup-receipt-v1.json"
            receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
            receipt["schema_version"] = "godot-codex-setup-receipt/1.0"
            receipt.pop("package_identity_digest")
            receipt.pop("config_ownership_marker")
            receipt_path.write_bytes(harness._canonical_json(receipt))
            state_directory = run / "fault"
            with self.assertRaises(harness.FaultHarnessError):
                harness.inject_invalid_config(
                    run_root=run,
                    project_root=project,
                    state_directory=state_directory,
                )
            self.assertEqual((project / ".codex/config.toml").read_bytes(), config)
            self.assertFalse(state_directory.exists())

    def test_symlink_target_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="s11u.") as directory:
            parent = Path(directory)
            run = self.create_run(parent)
            project = self.create_project(run)
            external = parent / "external"
            external.mkdir()
            (project / ".godot").symlink_to(external, target_is_directory=True)
            with self.assertRaises(harness.FaultHarnessError):
                harness.DiscoveryFault(
                    run_root=run,
                    project_root=project,
                    state_directory=run / "fault",
                    scenario="stale_discovery",
                ).inject()


if __name__ == "__main__":
    unittest.main()
