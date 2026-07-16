"""Unit tests for the Sprint 3 evidence validator."""

from __future__ import annotations

import copy
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

import sprint3_acceptance as acceptance
from sprint3_acceptance import (
    CLEANUP_FIELDS,
    MCP_CONTRACT_FIELDS,
    PHASES,
    REDACTION_FIELDS,
    STORAGE_BACKEND_CONFIGS,
    STORAGE_FAULT_NAMES,
    STORAGE_GATE_NAMES,
    TOOLS,
    AcceptanceError,
    merge_acceptance,
    validate_live,
    validate_storage,
)
from sprint3_stage4_index_mcp import (
    SOURCE_SCOPES,
    TELEMETRY_PREFIX,
    TOOL_NAMES,
    IndexMcpGateError,
    parse_bridge_telemetry,
    require_local_host_for_qualifying_evidence,
    require_schema_rejection,
    validate_tool_registry,
    verify_current_oracle,
    wait_for_current,
)

FIXTURE_COMMIT = "a" * 40
FIXTURE_SOURCE_SHA256 = "sha256:" + "b" * 64
FIXTURE_ORACLE_SHA256 = "sha256:" + "c" * 64
QUERY_SAMPLE_COUNTS = acceptance.expected_query_sample_counts()


def metric(samples: list[int | float]) -> dict[str, object]:
    ordered = sorted(samples)
    return {"samples": samples, "p50": ordered[0], "p95": ordered[-1]}


def replace_metric_samples(record: dict[str, object], samples: list[int | float]) -> None:
    record.clear()
    if samples:
        record.update(metric(samples))
    else:
        record.update({"samples": [], "p50": None, "p95": None})


def live_evidence(platform: str, graph_suffix: str = "") -> dict[str, Any]:
    phases: list[dict[str, Any]] = []
    for index, name in enumerate(PHASES, start=1):
        phase: dict[str, Any] = {
            "phase": name,
            "generation_id": "generation:sha256:" + f"{index:064x}",
            "index_revision": index,
            "resource_count": 12,
            "dependency_count": 12,
            "passed": True,
            "direct_reverse_oracle_match": True,
            "stable_project_scope": True,
            "diagnostic_count": 3 if name == "delete" else 2,
            "diagnostic_codes": ["missing_dependency", "stale_resource_uid"],
            "diagnostics_oracle_match": True,
            "normalized_graph_sha256": "sha256:" + (name + graph_suffix).encode("utf-8").hex().ljust(64, "0")[:64],
            "startup_visibility_ms": 4,
            "cached_resource_query_samples_ms": [1] * QUERY_SAMPLE_COUNTS[name],
            "bulk_status_ping_samples_ms": [3] if name == "journal_gap" else [],
            "bridge_main_thread_sessions": [
                {
                    "schema_version": 1,
                    "budget_usec": 2000,
                    "sample_capacity": 16384,
                    "busy_frame_count": 1,
                    "samples_usec": [100],
                    "max_elapsed_usec": 100,
                    "over_budget_count": 0,
                    "overflow": False,
                }
                for _ in range(2 if name == "base" else 1)
            ],
            "cleanup_verified": True,
        }
        if name == "base":
            phase["reopen_visibility_ms"] = 5
            phase["editor_reopened"] = True
            phase["sidecar_reopened"] = True
            phase["same_generation_after_reopen"] = True
            phase["immutable_segment_set_reused"] = True
        else:
            phase["startup_generation"] = "generation:sha256:" + f"{1:064x}"
            phase["startup_index_revision"] = 1
            phase["change_visibility_ms"] = 6 if name == "journal_gap" else 2
            phase["generation_advanced"] = True
        if name == "rename_uid":
            phase["entity_identity_preserved"] = True
            phase["full_rebuild_count_delta"] = 0
            phase["incremental_commit_count"] = 1
        if name == "journal_gap":
            phase["full_rebuild_after_gap"] = True
        if name == "re_add":
            phase["removal_visibility_ms"] = 2
        phases.append(phase)
    query_samples = [
        sample
        for phase in phases
        for sample in phase["cached_resource_query_samples_ms"]  # type: ignore[union-attr]
    ]
    bulk_status_samples = [
        sample
        for phase in phases
        for sample in phase["bulk_status_ping_samples_ms"]  # type: ignore[union-attr]
    ]
    visibility_samples = [
        sample
        for phase in phases
        if phase["phase"] not in {"base", "journal_gap"}
        for sample in (
            [phase["removal_visibility_ms"], phase["change_visibility_ms"]]
            if phase["phase"] == "re_add"
            else [phase["change_visibility_ms"]]
        )
    ]
    return {
        "schema_version": 3,
        "stage": "Sprint 3 Stage 5 / S3-09-S3-10",
        "status": "passed",
        "execution": "local_model_free",
        "profile": "acceptance",
        "platform": platform,
        "host": "fixture-host",
        "toolchain": {
            "python": "3.13.5",
            "scons": "SCons 4.9.1",
            "rustc": "rustc 1.94.1 (fixture)",
            "cargo": "cargo 1.94.1 (fixture)",
            "rust_target": ("aarch64-apple-darwin" if platform == "macos-arm64" else "x86_64-pc-windows-msvc"),
        },
        "git_commit": FIXTURE_COMMIT,
        "git_dirty": False,
        "source_tree_sha256": FIXTURE_SOURCE_SHA256,
        "artifacts": {
            "godot_version": f"fixture-godot-{FIXTURE_COMMIT[:9]}",
            "godot_sha256": "sha256:" + "d" * 64,
            "sidecar_version": "fixture-sidecar",
            "sidecar_sha256": "sha256:" + "e" * 64,
        },
        "mcp_protocol": "2025-11-25",
        "tools": sorted(TOOLS),
        "oracle_sha256": FIXTURE_ORACLE_SHA256,
        "fixture_digest_before": "f" * 64,
        "fixture_digest_after": "f" * 64,
        "canonical_fixture_unchanged": True,
        "phases": phases,
        "mcp_contract": {field: True for field in MCP_CONTRACT_FIELDS},
        "metrics_ms": {
            "cached_resource_query": metric(query_samples),
            "ordinary_incremental_visibility": metric(visibility_samples),
            "bulk_status_ping": metric(bulk_status_samples),
            "startup_visibility": metric([phase["startup_visibility_ms"] for phase in phases]),
            "compatible_reopen": metric([5]),
            "journal_gap_full_rebuild": metric([6]),
        },
        "bridge_main_thread": {
            **metric([100] * (len(PHASES) + 1)),
            "unit": "microseconds",
            "budget_usec": 2000,
            "busy_frame_count": len(PHASES) + 1,
            "max_elapsed_usec": 100,
            "over_budget_count": 0,
            "overflow": False,
        },
        "slo": {
            "cached_resource_query_p95_lte_300ms": True,
            "ordinary_incremental_visibility_p95_lte_2000ms": True,
            "bulk_status_ping_p95_lte_200ms": True,
            "bridge_main_thread_over_2000us_zero": True,
            "all_passed": True,
        },
        "completion": {
            "requested_phases": list(PHASES),
            "completed_phases": list(PHASES),
            "all_phases_completed": True,
            "format_import_matrix_verified": True,
            "resource_mcp_contract_verified": True,
        },
        "cleanup": {field: True for field in CLEANUP_FIELDS},
        "redaction": {field: True for field in REDACTION_FIELDS},
        "platform_evidence": {"remote_ci": "not_run"},
    }


def backend_evidence(name: str) -> dict[str, Any]:
    sample = 2 if name == "sqlite" else 1
    score = 0.5 if name == "sqlite" else 1.0
    return {
        "backend": name,
        "config": copy.deepcopy(STORAGE_BACKEND_CONFIGS[name]),
        "qualified": True,
        "gates": {gate: True for gate in STORAGE_GATE_NAMES},
        "fault_matrix": {fault: True for fault in STORAGE_FAULT_NAMES},
        "full_build_ns": [sample] * 5,
        "rename_ns": [sample] * 50,
        "reverse_query_ns": [sample] * 10_000,
        "reverse_query_p50_ns": sample,
        "reverse_query_p95_ns": sample,
        "artifact_bytes": sample,
        "rename_changed_blocks": 1,
        "rename_normalized_bytes": 2_048 if name == "sqlite" else 4_096,
        "rename_write_amplification": 2.0 if name == "sqlite" else 1.0,
        "binary_bytes": 1_026 if name == "sqlite" else 1,
        "dependency_count": 1,
        "dependencies": [{"name": "fixture", "version": "1", "source": "path", "license": None}],
        "weighted_score": score,
        "weighted_score_ci95_low": score,
        "weighted_score_ci95_high": score,
        "errors": [],
    }


def storage_evidence() -> dict[str, Any]:
    runs = []
    for os_name, architecture in (("linux", "x86_64"), ("macos", "aarch64"), ("windows", "x86_64")):
        runs.append({
            "schema_version": 2,
            "decision": "D-05",
            "profile": "decision",
            "seed": "D05-1",
            "git_commit": FIXTURE_COMMIT,
            "git_dirty": False,
            "source_tree_sha256": FIXTURE_SOURCE_SHA256,
            "rustc": "rustc 1.94.1 (fixture)",
            "os": os_name,
            "architecture": architecture,
            "host": {"runner": "local", "logical_cpus": 1},
            "oracle_sha256": FIXTURE_ORACLE_SHA256,
            "dataset": {
                "resources": 10_000,
                "edges": 50_000,
                "full_build_iterations": 5,
                "rename_iterations": 50,
                "query_warmup": 1_000,
                "query_iterations": 10_000,
                "stress_resources": 100_000,
                "stress_edges": 500_000,
                "fan_in_targets": [0, 1, 10, 100, 1_000],
            },
            "backends": [backend_evidence("sqlite"), backend_evidence("segment")],
            "chosen_backend": "segment",
            "decision_reason": ("Both backends qualified; segment had the higher weighted score (1.0000)."),
        })
    return {
        "schema_version": 2,
        "decision": "D-05",
        "platform_runs": runs,
        "backend_summaries": [
            {
                "backend": name,
                "qualified_all_platforms": True,
                "median_weighted_score": 0.5 if name == "sqlite" else 1.0,
                "weighted_score_ci95_low": 0.5 if name == "sqlite" else 1.0,
                "weighted_score_ci95_high": 0.5 if name == "sqlite" else 1.0,
            }
            for name in ("sqlite", "segment")
        ],
        "cross_platform_complete": True,
        "chosen_backend": "segment",
        "decision_reason": (
            "Both backends qualified cross-platform; segment had the higher median weighted score (1.0000)."
        ),
    }


class Sprint3AcceptanceTests(unittest.TestCase):
    def test_bulk_status_trace_samples_only_an_observed_active_window(self) -> None:
        class Process:
            tail: list[str] = []

        class Client:
            process = Process()

        previous = "generation:sha256:" + "1" * 64
        current = "generation:sha256:" + "2" * 64
        responses = iter((
            ({"freshness": "current", "generation_id": previous}, False, 1.0),
            ({"freshness": "current"}, False, 7.125),
            ({"freshness": "current", "generation_id": current}, False, 1.0),
        ))
        samples: list[float] = []
        with (
            mock.patch("sprint3_stage4_index_mcp.tool_call", side_effect=lambda *_args, **_kwargs: next(responses)),
            mock.patch("sprint3_stage4_index_mcp.time.sleep"),
        ):
            result, _ = wait_for_current(  # type: ignore[arg-type]
                Client(),
                "res://resource.tres",
                1.0,
                previous,
                samples,
            )
        self.assertEqual(result["generation_id"], current)
        self.assertEqual(samples, [7.125])

        samples = []
        with mock.patch(
            "sprint3_stage4_index_mcp.tool_call",
            return_value=({"freshness": "current", "generation_id": current}, False, 1.0),
        ):
            wait_for_current(  # type: ignore[arg-type]
                Client(),
                "res://resource.tres",
                1.0,
                previous,
                samples,
            )
        self.assertEqual(samples, [])

    def test_qualifying_live_producer_rejects_ci_environment_markers(self) -> None:
        with self.assertRaisesRegex(
            IndexMcpGateError,
            "CI, GITHUB_ACTIONS",
        ):
            require_local_host_for_qualifying_evidence(
                True,
                {"GITHUB_ACTIONS": "false", "CI": "false"},
            )
        require_local_host_for_qualifying_evidence(False, {"CI": "true"})
        require_local_host_for_qualifying_evidence(True, {})

    def test_schema_negative_requires_pinned_rmcp_tool_error(self) -> None:
        class Client:
            def __init__(self, response: dict[str, object]) -> None:
                self.response = response

            def request(self, _method: str, _params: dict[str, object]) -> dict[str, object]:
                return self.response

        require_schema_rejection(
            Client(  # type: ignore[arg-type]
                {
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": (
                                    "failed to deserialize parameters: unknown field `unknown_member`, "
                                    "expected one of `resource`, `limit`, `cursor`"
                                ),
                            }
                        ],
                        "isError": True,
                    }
                }
            ),
            "godot_get_resource_dependencies",
            {"unknown_member": True},
        )
        rejected = (
            {
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": (
                                "failed to deserialize parameters: unknown field `unknown_member`, "
                                "expected one of `resource`, `limit`, `cursor`"
                            ),
                        }
                    ],
                    "isError": False,
                }
            },
            {"error": {"code": -32603, "message": "Internal error"}},
            {"error": {"code": -32602, "message": "Invalid params"}},
            {
                "result": {
                    "isError": True,
                    "structuredContent": {"error": {"code": "invalid_params"}},
                }
            },
            {
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": (
                                "failed to deserialize parameters: unknown field `different_member`, "
                                "expected one of `resource`, `limit`, `cursor`"
                            ),
                        }
                    ],
                    "isError": True,
                }
            },
        )
        for response in rejected:
            with self.subTest(response=response):
                with self.assertRaisesRegex(IndexMcpGateError, "pinned rmcp schema-rejection envelope"):
                    require_schema_rejection(
                        Client(response),  # type: ignore[arg-type]
                        "godot_get_resource_dependencies",
                        {"unknown_member": True},
                    )

    def test_rejects_duplicate_entries_in_five_tool_registry(self) -> None:
        tools = [
            {
                "name": name,
                "inputSchema": {"type": "object", "additionalProperties": False},
            }
            for name in sorted(TOOL_NAMES)
        ]
        self.assertEqual(validate_tool_registry(tools), tools)
        with self.assertRaisesRegex(IndexMcpGateError, "five-tool contract"):
            validate_tool_registry([*tools, tools[0]])

    def test_live_source_scope_covers_editor_file_system_dependency_cache(self) -> None:
        self.assertTrue(
            {
                "docs/codex-integration/INDEX-001-semantic-index-storage-and-migrations.md",
                "docs/codex-integration/MCP-001-project-scoped-read-tools.md",
                "docs/codex-integration/PROTOCOL-001-bridge-rpc-v1.md",
                *{f"docs/codex-integration/SPRINT-3-STAGE-{stage}-PLAN.md" for stage in range(1, 6)},
                "editor/file_system/editor_file_system.cpp",
                "editor/file_system/editor_file_system.h",
                "tests/codex/evidence/sprint-3-stage-1-contracts.json",
            }.issubset(SOURCE_SCOPES)
        )

    def test_query_trace_cardinality_is_derived_per_oracle_phase(self) -> None:
        oracle = json.loads(acceptance.GOLDEN_ORACLE_PATH.read_text(encoding="utf-8"))
        phases = {phase["name"]: phase for phase in oracle["phases"]}

        def single_verification_calls(phase: dict[str, Any]) -> int:
            resources = phase["resources"]
            dependencies = phase["dependencies"]
            direct = phase["expected_direct_queries"]
            incoming = {resource["entity_id"]: 0 for resource in resources}  # type: ignore[union-attr]
            for dependency in dependencies:  # type: ignore[union-attr]
                target = dependency.get("resolved_target_entity_id")
                if target is not None:
                    incoming[target] += 1
            direct_calls = sum(
                max(1, len(direct[resource["oracle_id"]]))  # type: ignore[index]
                for resource in resources  # type: ignore[union-attr]
            )
            reverse_calls = sum(
                max(1, incoming[resource["entity_id"]])
                for resource in resources  # type: ignore[union-attr]
            )
            return direct_calls + reverse_calls

        self.assertEqual(QUERY_SAMPLE_COUNTS["base"], 2 * single_verification_calls(phases["base"]))
        self.assertEqual(QUERY_SAMPLE_COUNTS["delete"], single_verification_calls(phases["delete"]))
        self.assertEqual(
            QUERY_SAMPLE_COUNTS["re_add"],
            single_verification_calls(phases["delete"]) + single_verification_calls(phases["re_add"]),
        )
        self.assertLess(len(phases["delete"]["resources"]), len(phases["base"]["resources"]))

    def test_oracle_verification_is_bound_to_the_waited_generation(self) -> None:
        current = {"generation_id": "generation:sha256:" + "1" * 64, "index_revision": 10}
        verified = {
            "generation_id": current["generation_id"],
            "index_revision": current["index_revision"],
        }
        with mock.patch("sprint3_stage4_index_mcp.verify_oracle", return_value=verified):
            self.assertIs(
                verify_current_oracle(None, current, {}, [], "phase"),  # type: ignore[arg-type]
                verified,
            )

        for field, value in (
            ("generation_id", "generation:sha256:" + "2" * 64),
            ("index_revision", 11),
        ):
            with self.subTest(field=field):
                mismatched = {**verified, field: value}
                with (
                    mock.patch("sprint3_stage4_index_mcp.verify_oracle", return_value=mismatched),
                    self.assertRaisesRegex(IndexMcpGateError, "crossed index generations"),
                ):
                    verify_current_oracle(None, current, {}, [], "phase")  # type: ignore[arg-type]

    def test_checkout_anchor_allows_evidence_only_descendant_without_source_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def git(*arguments: str) -> str:
                result = subprocess.run(
                    ["git", *arguments],
                    cwd=root,
                    check=True,
                    capture_output=True,
                    text=True,
                )
                return result.stdout.strip()

            git("init", "--quiet")
            git("config", "user.email", "sprint3@example.invalid")
            git("config", "user.name", "Sprint 3 Test")
            scope_path = root / "scope.txt"
            oracle_path = root / "oracle.json"
            source_path = root / "source.txt"
            scopes = ("oracle.json", "scope.txt", "source.txt")
            scope_path.write_text("\n".join(scopes) + "\n", encoding="utf-8")
            oracle_path.write_text('{"oracle":true}\n', encoding="utf-8")
            source_path.write_text("source freeze\n", encoding="utf-8")
            git("add", *scopes)
            git("commit", "--quiet", "-m", "source freeze")
            source_commit = git("rev-parse", "HEAD")

            with mock.patch.multiple(
                acceptance,
                REPOSITORY_ROOT=root,
                SOURCE_SCOPE_PATH=scope_path,
                SOURCE_SCOPE_MANIFEST="scope.txt",
                SOURCE_SCOPES=scopes,
                GOLDEN_ORACLE_PATH=oracle_path,
            ):
                source_coordinates = acceptance.checkout_source_coordinates(source_commit)
                (root / "evidence.json").write_text('{"status":"passed"}\n', encoding="utf-8")
                git("add", "evidence.json")
                git("commit", "--quiet", "-m", "record evidence")
                evidence_head = git("rev-parse", "HEAD")

                anchored = acceptance.checkout_source_coordinates(source_commit)
                self.assertNotEqual(evidence_head, source_commit)
                self.assertEqual(anchored["head_commit"], evidence_head)
                self.assertEqual(anchored["evidence_commit"], source_commit)
                self.assertEqual(
                    anchored["source_tree_sha256"],
                    source_coordinates["source_tree_sha256"],
                )
                self.assertEqual(anchored["oracle_sha256"], source_coordinates["oracle_sha256"])

                source_path.write_text("drifted source\n", encoding="utf-8")
                with self.assertRaisesRegex(AcceptanceError, "source differs from the evidence source commit"):
                    acceptance.checkout_source_coordinates(source_commit)

    def write(self, root: Path, name: str, value: object) -> Path:
        path = root / name
        path.write_text(json.dumps(value, sort_keys=True), encoding="utf-8")
        return path

    def test_validates_and_merges_complete_local_platform_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            macos = self.write(root, "macos.json", live_evidence("macos-arm64"))
            windows = self.write(root, "windows.json", live_evidence("windows-x86_64"))
            storage = self.write(root, "storage.json", storage_evidence())
            output = root / "acceptance.json"
            self.assertEqual(validate_live(macos)["evidence"]["platform"], "macos-arm64")
            self.assertEqual(validate_storage(storage)["evidence"]["chosen_backend"], "segment")
            merged = merge_acceptance(macos, windows, storage, output, anchor_to_checkout=False)
            self.assertEqual(merged["schema_version"], 3)
            self.assertEqual(merged["status"], "passed")
            self.assertEqual(len(merged["acceptance_criteria"]), 12)
            self.assertTrue(all(merged["claims"].values()))
            self.assertTrue(all(item["passed"] for item in merged["acceptance_criteria"].values()))
            self.assertTrue(all(item["proof"] for item in merged["acceptance_criteria"].values()))
            self.assertTrue(
                all(
                    item["passed"] is all(merged["claims"][claim] for claim in item["proof"])
                    for item in merged["acceptance_criteria"].values()
                )
            )
            self.assertTrue(output.is_file())

    def test_rejects_summary_not_derived_from_raw_samples(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            evidence = live_evidence("macos-arm64")
            evidence["metrics_ms"]["cached_resource_query"]["p95"] = 999  # type: ignore[index]
            path = self.write(root, "macos.json", evidence)
            with self.assertRaisesRegex(AcceptanceError, "p95"):
                validate_live(path)

    def test_rejects_selective_or_unattributed_live_metric_populations(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def reject(name: str, evidence: dict[str, object], message: str) -> None:
                path = self.write(root, f"{name}.json", evidence)
                with self.assertRaisesRegex(AcceptanceError, message):
                    validate_live(path)

            ordinary = live_evidence("macos-arm64")
            ordinary["phases"][1]["change_visibility_ms"] = 999_999  # type: ignore[index]
            reject("ordinary", ordinary, "ordinary visibility population differs")

            startup = live_evidence("macos-arm64")
            replace_metric_samples(startup["metrics_ms"]["startup_visibility"], [4])  # type: ignore[arg-type,index]
            reject("startup", startup, "startup visibility population differs")

            reopen = live_evidence("macos-arm64")
            replace_metric_samples(reopen["metrics_ms"]["compatible_reopen"], [4])  # type: ignore[arg-type,index]
            reject("reopen", reopen, "compatible reopen population differs")

            rebuild = live_evidence("macos-arm64")
            replace_metric_samples(rebuild["metrics_ms"]["journal_gap_full_rebuild"], [5])  # type: ignore[arg-type,index]
            reject("rebuild", rebuild, "journal-gap rebuild population differs")

            selective_query = live_evidence("macos-arm64")
            selective_query["phases"][0]["cached_resource_query_samples_ms"].pop()  # type: ignore[index,union-attr]
            selected_query_samples = [
                sample
                for phase in selective_query["phases"]  # type: ignore[union-attr]
                for sample in phase["cached_resource_query_samples_ms"]
            ]
            replace_metric_samples(
                selective_query["metrics_ms"]["cached_resource_query"],  # type: ignore[arg-type,index]
                selected_query_samples,
            )
            reject("selective-query", selective_query, "query trace sample population differs")

            missing_bulk = live_evidence("macos-arm64")
            missing_bulk["phases"][-1]["bulk_status_ping_samples_ms"].clear()  # type: ignore[index,union-attr]
            replace_metric_samples(missing_bulk["metrics_ms"]["bulk_status_ping"], [])  # type: ignore[arg-type,index]
            reject("missing-bulk", missing_bulk, "bulk status/ping trace samples are empty")

            injected_bulk = live_evidence("macos-arm64")
            replace_metric_samples(injected_bulk["metrics_ms"]["bulk_status_ping"], [3, 3])  # type: ignore[arg-type,index]
            reject("injected-bulk", injected_bulk, "bulk status/ping population differs")

    def test_rejects_incomplete_or_tampered_storage_sample_populations(self) -> None:
        mutations = (
            (
                "full-build-count",
                lambda value: value["platform_runs"][0]["backends"][0]["full_build_ns"].pop(),
                "full_build_ns sample population differs",
            ),
            (
                "rename-count",
                lambda value: value["platform_runs"][0]["backends"][0]["rename_ns"].pop(),
                "rename_ns sample population differs",
            ),
            (
                "query-count",
                lambda value: value["platform_runs"][0]["backends"][0]["reverse_query_ns"].pop(),
                "reverse_query_ns sample population differs",
            ),
            (
                "query-percentile",
                lambda value: value["platform_runs"][0]["backends"][0].__setitem__("reverse_query_p95_ns", 3),
                "query percentiles differ",
            ),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, mutate, message in mutations:
                with self.subTest(name=name):
                    evidence = storage_evidence()
                    mutate(evidence)
                    path = self.write(root, f"{name}.json", evidence)
                    with self.assertRaisesRegex(AcceptanceError, message):
                        validate_storage(path)

    def test_rejects_invalid_storage_sizes_scores_dependencies_and_config(self) -> None:
        mutations = (
            (
                "artifact-size",
                lambda value: value["platform_runs"][0]["backends"][0].__setitem__("artifact_bytes", -1),
                "artifact bytes is invalid",
            ),
            (
                "dependency-count",
                lambda value: value["platform_runs"][0]["backends"][0].__setitem__("dependency_count", 2),
                "dependency count differs",
            ),
            (
                "config",
                lambda value: value["platform_runs"][0]["backends"][0]["config"].__setitem__("physical_version", "0"),
                "config differs",
            ),
            (
                "score",
                lambda value: value["platform_runs"][0]["backends"][0].__setitem__("weighted_score", float("inf")),
                "non-finite JSON number",
            ),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, mutate, message in mutations:
                with self.subTest(name=name):
                    evidence = storage_evidence()
                    mutate(evidence)
                    path = self.write(root, f"{name}.json", evidence)
                    with self.assertRaisesRegex(AcceptanceError, message):
                        validate_storage(path)

    def test_rejects_hand_authored_storage_scores_and_intervals(self) -> None:
        mutations = (
            (
                "raw-score",
                lambda value: value["platform_runs"][0]["backends"][0].__setitem__("weighted_score", 0.75),
            ),
            (
                "aggregate-interval",
                lambda value: value["backend_summaries"][0].__setitem__("weighted_score_ci95_high", 0.75),
            ),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, mutate in mutations:
                with self.subTest(name=name):
                    evidence = storage_evidence()
                    mutate(evidence)
                    path = self.write(root, f"{name}.json", evidence)
                    with self.assertRaisesRegex(AcceptanceError, "canonical Rust raw-sample scoring"):
                        validate_storage(path)

    def test_rejects_untrusted_redaction_local_runner_and_raw_choice(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            leaked = live_evidence("macos-arm64")
            leaked["host"] = "/Users/example/private-project"
            path = self.write(root, "leaked-host.json", leaked)
            with self.assertRaisesRegex(AcceptanceError, "absolute local path"):
                validate_live(path)

            remote = storage_evidence()
            remote["platform_runs"][0]["host"]["runner"] = "hosted-ci"  # type: ignore[index]
            path = self.write(root, "remote-runner.json", remote)
            with self.assertRaisesRegex(AcceptanceError, "host coordinates differ"):
                validate_storage(path)

            inconsistent = storage_evidence()
            inconsistent["platform_runs"][0]["chosen_backend"] = "sqlite"  # type: ignore[index]
            path = self.write(root, "raw-choice.json", inconsistent)
            with self.assertRaisesRegex(AcceptanceError, "choice differs from its raw backend evidence"):
                validate_storage(path)

    def test_rejects_nested_redaction_bypasses_and_unknown_phase_fields(self) -> None:
        mutations = (
            ("secret", lambda value: value.__setitem__("host", "sk-private-material"), "redacted"),
            (
                "github-token",
                lambda value: value.__setitem__("host", "ghp_0123456789abcdefghijklmnopqrstuvwxyz"),
                "redacted",
            ),
            (
                "aws-access-key",
                lambda value: value.__setitem__("host", "AKIAIOSFODNN7EXAMPLE"),
                "redacted",
            ),
            (
                "source",
                lambda value: value["artifacts"].__setitem__("sidecar_version", "extends Node\nfunc leaked():"),
                "redacted",
            ),
            (
                "gdscript-var",
                lambda value: value["artifacts"].__setitem__("sidecar_version", "var health := 10"),
                "redacted",
            ),
            (
                "session",
                lambda value: value.__setitem__("host", "123e4567-e89b-42d3-a456-426614174000"),
                "redacted",
            ),
            (
                "uuid-v7",
                lambda value: value.__setitem__("host", "01890f2e-7b21-7cc3-98c8-7b5f8d3f1234"),
                "redacted",
            ),
            (
                "opaque-session",
                lambda value: value.__setitem__("host", "sid_Kx9R2mQ7vL5nP4tB"),
                "redacted",
            ),
            (
                "endpoint",
                lambda value: value["artifacts"].__setitem__("sidecar_version", "unix:///private/tmp/bridge.sock"),
                "redacted",
            ),
            (
                "encoded-unix-path",
                lambda value: value.__setitem__(
                    "host",
                    "https://example.invalid/?next=%2FUsers%2Fexample%2Fprivate",
                ),
                "redacted",
            ),
            (
                "double-encoded-unix-path",
                lambda value: value.__setitem__("host", "%252fhome%252fexample%252fprivate"),
                "redacted",
            ),
            (
                "encoded-windows-drive",
                lambda value: value.__setitem__("host", "C%3A%2FUsers%2Fexample%2Fprivate"),
                "redacted",
            ),
            (
                "encoded-windows-backslashes",
                lambda value: value.__setitem__("host", "%43%3a%5cUsers%5cexample%5cprivate"),
                "redacted",
            ),
            (
                "encoded-unc-path",
                lambda value: value.__setitem__("host", "%5C%5Cserver%5Cshare%5Cprivate"),
                "redacted",
            ),
            (
                "comma-path",
                lambda value: value.__setitem__("host", "macOS,/Users/example/private"),
                "absolute local path",
            ),
            (
                "semicolon-path",
                lambda value: value.__setitem__("host", "macOS;/home/example/private"),
                "absolute local path",
            ),
            (
                "brace-path",
                lambda value: value.__setitem__("host", "macOS{/private/tmp/bridge"),
                "absolute local path",
            ),
            (
                "unknown-phase-field",
                lambda value: value["phases"][0].__setitem__("unexpected_note", "redacted"),
                "phase fields differ",
            ),
            (
                "unknown-main-thread-field",
                lambda value: value["bridge_main_thread"].__setitem__("unexpected_note", "redacted"),
                "aggregate main-thread telemetry fields differ",
            ),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, mutate, message in mutations:
                with self.subTest(name=name):
                    evidence = live_evidence("macos-arm64")
                    mutate(evidence)
                    path = self.write(root, f"{name}.json", evidence)
                    with self.assertRaisesRegex(AcceptanceError, message):
                        validate_live(path)

        self.assertTrue(
            all(acceptance.redaction_status({"public_url": "https://example.invalid/Users/public/project"}).values())
        )
        self.assertTrue(
            all(acceptance.redaction_status({"public_url": "https://example.invalid/?next=%2Fapi%2Fv1"}).values())
        )
        self.assertTrue(
            all(
                acceptance.redaction_status({
                    "public_url": "https%3A%2F%2Fexample.invalid%2Fdocs%2Fpublic",
                }).values()
            )
        )
        deeply_encoded_path = "%2fhome%2fexample%2fprivate"
        deeply_encoded_url = "https%3A%2F%2Fexample.invalid%2Fdocs%2Fpublic"
        for _ in range(4):
            deeply_encoded_path = deeply_encoded_path.replace("%", "%25")
            deeply_encoded_url = deeply_encoded_url.replace("%", "%25")
        self.assertFalse(acceptance.redaction_status({"host": deeply_encoded_path})["absolute_paths_absent"])
        self.assertTrue(all(acceptance.redaction_status({"public_url": deeply_encoded_url}).values()))

        over_limit_url = "https%3A%2F%2Fexample.invalid%2Fdocs%2Fpublic"
        for _ in range(acceptance.MAX_REDACTION_DECODE_LAYERS + 1):
            over_limit_url = over_limit_url.replace("%", "%25")
        self.assertFalse(acceptance.redaction_status({"public_url": over_limit_url})["absolute_paths_absent"])

    def test_rejects_cross_platform_graph_difference_and_dirty_storage(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            macos = self.write(root, "macos.json", live_evidence("macos-arm64"))
            windows = self.write(root, "windows.json", live_evidence("windows-x86_64", "-different"))
            storage_value = storage_evidence()
            storage = self.write(root, "storage.json", storage_value)
            with self.assertRaisesRegex(AcceptanceError, "normalized graphs"):
                merge_acceptance(
                    macos,
                    windows,
                    storage,
                    root / "acceptance.json",
                    anchor_to_checkout=False,
                )

            dirty = copy.deepcopy(storage_value)
            dirty["platform_runs"][0]["git_dirty"] = True  # type: ignore[index]
            dirty_path = self.write(root, "dirty-storage.json", dirty)
            with self.assertRaisesRegex(AcceptanceError, "clean D-05"):
                validate_storage(dirty_path)

    def test_rejects_incomplete_live_toolchain_completion_cleanup_and_redaction(self) -> None:
        mutations = (
            ("toolchain", lambda value: value["toolchain"].pop("cargo"), "toolchain fields"),
            (
                "completion",
                lambda value: value["completion"].__setitem__("all_phases_completed", False),
                "completion is incomplete",
            ),
            (
                "mcp-contract",
                lambda value: value["mcp_contract"].__setitem__("cross_project_cursor_rejected", False),
                "live MCP contract is incomplete",
            ),
            (
                "cleanup",
                lambda value: value["cleanup"].__setitem__("sidecar_processes_stopped", False),
                "cleanup is incomplete",
            ),
            (
                "redaction",
                lambda value: value["redaction"].pop("source_bytes_absent"),
                "redaction claims do not match",
            ),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, mutate, message in mutations:
                with self.subTest(name=name):
                    evidence = live_evidence("macos-arm64")
                    mutate(evidence)
                    path = self.write(root, f"{name}.json", evidence)
                    with self.assertRaisesRegex(AcceptanceError, message):
                        validate_live(path)

    def test_rejects_invalid_or_impossible_bridge_telemetry_capacity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for value in (-1, 0, True, "16384"):
                with self.subTest(capacity=value):
                    evidence = live_evidence("macos-arm64")
                    record = evidence["phases"][0]["bridge_main_thread_sessions"][0]  # type: ignore[index]
                    record["sample_capacity"] = value
                    path = self.write(root, f"capacity-{value!s}.json", evidence)
                    with self.assertRaisesRegex(AcceptanceError, "telemetry values are invalid"):
                        validate_live(path)
                    with self.assertRaisesRegex(IndexMcpGateError, "telemetry contract differs"):
                        parse_bridge_telemetry(TELEMETRY_PREFIX + json.dumps(record))

            evidence = live_evidence("macos-arm64")
            oversized = evidence["phases"][0]["bridge_main_thread_sessions"][0]  # type: ignore[index]
            oversized["samples_usec"] = [1] * (acceptance.BRIDGE_TELEMETRY_SAMPLE_CAPACITY + 1)
            oversized["busy_frame_count"] = len(oversized["samples_usec"])
            oversized["max_elapsed_usec"] = 1
            oversized["over_budget_count"] = 0
            path = self.write(root, "capacity-overflow.json", evidence)
            with self.assertRaisesRegex(AcceptanceError, "exceeds its sample capacity"):
                validate_live(path)
            with self.assertRaisesRegex(IndexMcpGateError, "samples are empty or invalid"):
                parse_bridge_telemetry(TELEMETRY_PREFIX + json.dumps(oversized))

    def test_requires_every_editor_telemetry_session_and_flattens_base_reopen(self) -> None:
        def refresh_aggregate(evidence: dict[str, Any]) -> None:
            records = [
                record
                for phase in evidence["phases"]  # type: ignore[union-attr]
                for record in phase["bridge_main_thread_sessions"]
            ]
            samples = [sample for record in records for sample in record["samples_usec"]]
            aggregate = evidence["bridge_main_thread"]  # type: ignore[assignment]
            aggregate.clear()
            aggregate.update({
                **metric(samples),
                "unit": "microseconds",
                "budget_usec": 2000,
                "busy_frame_count": sum(record["busy_frame_count"] for record in records),
                "max_elapsed_usec": max(samples),
                "over_budget_count": sum(record["over_budget_count"] for record in records),
                "overflow": any(record["overflow"] for record in records),
            })

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            complete = live_evidence("macos-arm64")
            second_base = complete["phases"][0]["bridge_main_thread_sessions"][1]  # type: ignore[index]
            second_base["samples_usec"] = [101]
            second_base["max_elapsed_usec"] = 101
            refresh_aggregate(complete)
            self.assertEqual(
                validate_live(self.write(root, "complete.json", complete))["evidence"]["bridge_main_thread"]["samples"],
                [100, 101, *([100] * (len(PHASES) - 1))],
            )

            missing_base = live_evidence("macos-arm64")
            missing_base["phases"][0]["bridge_main_thread_sessions"].pop()  # type: ignore[index,union-attr]
            refresh_aggregate(missing_base)
            with self.assertRaisesRegex(AcceptanceError, "base telemetry session population differs"):
                validate_live(self.write(root, "missing-base-session.json", missing_base))

            extra_non_base = live_evidence("macos-arm64")
            sessions = extra_non_base["phases"][1]["bridge_main_thread_sessions"]  # type: ignore[index]
            sessions.append(copy.deepcopy(sessions[0]))
            refresh_aggregate(extra_non_base)
            with self.assertRaisesRegex(AcceptanceError, "rename_uid telemetry session population differs"):
                validate_live(self.write(root, "extra-rename-session.json", extra_non_base))

    def test_producer_rejects_inexact_bridge_telemetry_summary(self) -> None:
        base = live_evidence("macos-arm64")["phases"][0]["bridge_main_thread_sessions"][0]  # type: ignore[index]
        mutations = (
            ("overflow", lambda record: record.__setitem__("overflow", True), "telemetry overflowed"),
            ("busy", lambda record: record.__setitem__("busy_frame_count", 2), "omitted a busy frame"),
            ("over-budget", lambda record: record.__setitem__("over_budget_count", 1), "summary contradicts"),
            ("maximum", lambda record: record.__setitem__("max_elapsed_usec", 101), "summary contradicts"),
        )
        for name, mutate, message in mutations:
            with self.subTest(name=name):
                record = copy.deepcopy(base)
                mutate(record)
                with self.assertRaisesRegex(IndexMcpGateError, message):
                    parse_bridge_telemetry(TELEMETRY_PREFIX + json.dumps(record))

    def test_rejects_unproven_diagnostics_and_uid_rename_claims(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            diagnostics = live_evidence("macos-arm64")
            diagnostics["phases"][0]["diagnostic_codes"] = ["missing_dependency"]  # type: ignore[index]
            path = self.write(root, "diagnostics.json", diagnostics)
            with self.assertRaisesRegex(AcceptanceError, "diagnostics differ"):
                validate_live(path)

            rename = live_evidence("macos-arm64")
            rename["phases"][1]["incremental_commit_count"] = 2  # type: ignore[index]
            path = self.write(root, "rename.json", rename)
            with self.assertRaisesRegex(AcceptanceError, "not proven incremental"):
                validate_live(path)

    def test_rejects_inconsistent_generation_transitions(self) -> None:
        mutations = (
            (
                "generation",
                lambda phase: phase.__setitem__("generation_id", phase["startup_generation"]),
            ),
            (
                "revision",
                lambda phase: phase.__setitem__("index_revision", phase["startup_index_revision"]),
            ),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, mutate in mutations:
                with self.subTest(name=name):
                    evidence = live_evidence("macos-arm64")
                    mutate(evidence["phases"][1])  # type: ignore[index]
                    path = self.write(root, f"{name}.json", evidence)
                    with self.assertRaisesRegex(AcceptanceError, "startup or generation transition is invalid"):
                        validate_live(path)

    def test_rejects_duplicate_or_inexact_platform_coordinates(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            storage = storage_evidence()
            storage["platform_runs"][1] = copy.deepcopy(storage["platform_runs"][0])  # type: ignore[index]
            path = self.write(root, "duplicate-storage.json", storage)
            with self.assertRaisesRegex(AcceptanceError, "coordinates differ or contain duplicates"):
                validate_storage(path)

            macos = self.write(root, "macos.json", live_evidence("macos-arm64"))
            duplicate_live = self.write(root, "also-macos.json", live_evidence("macos-arm64"))
            valid_storage = self.write(root, "storage.json", storage_evidence())
            with self.assertRaisesRegex(AcceptanceError, "expected windows-x86_64"):
                merge_acceptance(
                    macos,
                    duplicate_live,
                    valid_storage,
                    root / "acceptance.json",
                    anchor_to_checkout=False,
                )

    def test_rejects_missing_and_false_storage_outcomes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            missing_gate = storage_evidence()
            del missing_gate["platform_runs"][0]["backends"][1]["gates"]["canonical_oracle"]  # type: ignore[index]
            path = self.write(root, "missing-gate.json", missing_gate)
            with self.assertRaisesRegex(AcceptanceError, "storage gate map differs"):
                validate_storage(path)

            missing_fault = storage_evidence()
            fault_matrix = missing_fault["platform_runs"][0]["backends"][1]["fault_matrix"]  # type: ignore[index]
            del fault_matrix["hard_kill.capture"]
            path = self.write(root, "missing-fault.json", missing_fault)
            with self.assertRaisesRegex(AcceptanceError, "storage fault map differs"):
                validate_storage(path)

            failed_gate = storage_evidence()
            segment = failed_gate["platform_runs"][0]["backends"][1]  # type: ignore[index]
            segment["gates"]["canonical_oracle"] = False
            segment["qualified"] = False
            failed_gate["platform_runs"][0]["chosen_backend"] = "sqlite"  # type: ignore[index]
            path = self.write(root, "failed-gate.json", failed_gate)
            with self.assertRaisesRegex(AcceptanceError, "did not pass every D-05 outcome"):
                validate_storage(path)

            failed_fault = storage_evidence()
            segment = failed_fault["platform_runs"][0]["backends"][1]  # type: ignore[index]
            segment["fault_matrix"]["hard_kill.capture"] = False
            segment["gates"]["process_crash_recovery"] = False
            segment["qualified"] = False
            failed_fault["platform_runs"][0]["chosen_backend"] = "sqlite"  # type: ignore[index]
            path = self.write(root, "failed-fault.json", failed_fault)
            with self.assertRaisesRegex(AcceptanceError, "did not pass every D-05 outcome"):
                validate_storage(path)

    def test_rejects_cross_family_source_freeze_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            macos = self.write(root, "macos.json", live_evidence("macos-arm64"))
            windows = self.write(root, "windows.json", live_evidence("windows-x86_64"))
            storage_value = storage_evidence()
            for run in storage_value["platform_runs"]:  # type: ignore[union-attr]
                run["source_tree_sha256"] = "sha256:" + "f" * 64
            storage = self.write(root, "storage.json", storage_value)
            with self.assertRaisesRegex(AcceptanceError, "live/storage source digests differ"):
                merge_acceptance(
                    macos,
                    windows,
                    storage,
                    root / "acceptance.json",
                    anchor_to_checkout=False,
                )

    def test_rejects_duplicate_json_members(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "duplicate.json"
            path.write_text('{"schema_version":3,"schema_version":3}', encoding="utf-8")
            with self.assertRaisesRegex(AcceptanceError, "duplicate JSON member schema_version"):
                validate_live(path)

    def test_rejects_exponent_overflow_before_live_or_storage_validation(self) -> None:
        cases = (
            (
                "live",
                live_evidence("macos-arm64"),
                lambda value: value["metrics_ms"]["startup_visibility"].__setitem__("p95", "NUMERIC_OVERFLOW"),
                validate_live,
            ),
            (
                "storage",
                storage_evidence(),
                lambda value: value["platform_runs"][0]["backends"][0].__setitem__(
                    "weighted_score", "NUMERIC_OVERFLOW"
                ),
                validate_storage,
            ),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, evidence, mutate, validator in cases:
                with self.subTest(name=name):
                    mutate(evidence)
                    serialized = json.dumps(evidence, sort_keys=True).replace('"NUMERIC_OVERFLOW"', "1e999")
                    path = root / f"{name}.json"
                    path.write_text(serialized, encoding="utf-8")
                    with self.assertRaisesRegex(AcceptanceError, "non-finite JSON number 1e999"):
                        validator(path)


if __name__ == "__main__":
    unittest.main()
