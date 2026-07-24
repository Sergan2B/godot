#!/usr/bin/env python3
"""Fail-closed Sprint 11 evidence, acquisition, and parity validator.

This module validates evidence that was acquired before the final evidence run.
It never drives App/CLI/IDE UI, conducts a human study, or creates a qualifying
artifact. Synthetic traces are accepted only when ``qualifying=False`` is
passed explicitly by contract tests.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import pwd
import re
import signal
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import tomllib
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, cast

try:
    from tests.codex import sprint11_external_acquisitions as external_acquisitions
    from tests.codex import sprint11_host_provenance as host_provenance
    from tests.codex import sprint11_multi_project as multi_project
    from tests.codex import sprint11_packaged_regressions as packaged_regressions
    from tests.codex.usability import validator as human_usability
except ModuleNotFoundError:  # Direct script execution from tests/codex.
    import sprint11_external_acquisitions as external_acquisitions
    import sprint11_host_provenance as host_provenance
    import sprint11_multi_project as multi_project
    import sprint11_packaged_regressions as packaged_regressions
    from usability import validator as human_usability

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
EVIDENCE_PATH = (
    SCRIPT_DIR / "evidence" / "sprint-11-external-codex-beta-macos.json"
)
EVIDENCE_RELATIVE_PATH = EVIDENCE_PATH.relative_to(REPOSITORY_ROOT).as_posix()
SOURCE_SCOPE_PATH = SCRIPT_DIR / "sprint11_source_scopes.txt"
PROMPT_PACK_PATH = SCRIPT_DIR / "prompts" / "sprint11-external-beta-v1.json"
EVIDENCE_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-evidence.schema.json"
)
TRACE_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-surface-trace.schema.json"
)
SURFACE_AUTHORITY_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-surface-acquisition-authority.schema.json"
)
PACKAGED_REGRESSION_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-packaged-regression-receipt.schema.json"
)
MULTI_PROJECT_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-multi-project-receipt.schema.json"
)
MULTI_PROJECT_REPORT_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-multi-project-report.schema.json"
)
REPRODUCIBILITY_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-reproducibility-receipt.schema.json"
)
USABILITY_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-usability-report.schema.json"
)
HOST_PROFILE_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "host-coordinate-profile.schema.json"
)
HOST_PROVENANCE_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-host-provenance.schema.json"
)
HOST_PROVENANCE_ACQUISITION_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-host-provenance-acquisition.schema.json"
)
HUMAN_TRACE_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-human-usability-trace.schema.json"
)
HUMAN_AUTHORITY_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-human-acquisition-authority.schema.json"
)
HUMAN_RUBRIC_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-human-usability-rubric.schema.json"
)
HUMAN_CONSENT_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-human-consent-receipt.schema.json"
)
HUMAN_DEFECT_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "sprint11-human-usability-defect-ledger.schema.json"
)
COMPATIBILITY_SCHEMA_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "schemas"
    / "godot_codex"
    / "compatibility-matrix.schema.json"
)
REGISTRY_PROFILE_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "product"
    / "registry-profile.v1.json"
)
COMPATIBILITY_MATRIX_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "product"
    / "compatibility-matrix.v1.json"
)
HOST_PROFILE_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "product"
    / "host-coordinate-profile.v1.json"
)
SERVER_INSTRUCTIONS_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "product"
    / "server-instructions.v1.txt"
)
HOST_CONTRACT_PATH = (
    REPOSITORY_ROOT
    / "docs"
    / "codex-integration"
    / "SPRINT-11-HOST-CONTRACT.md"
)
SPRINT11_PLAN_PATH = (
    REPOSITORY_ROOT
    / "docs"
    / "codex-integration"
    / "SPRINT-11-PLAN.md"
)
DOCUMENTED_HOST_ACQUISITION_COMMAND = """python3 tests/codex/sprint11_external_acquisitions.py host-provenance \\
  --app-bundle '/Applications/ChatGPT.app' \\
  --app-executable '/Applications/ChatGPT.app/Contents/MacOS/ChatGPT' \\
  --app-client '/Applications/ChatGPT.app/Contents/Resources/codex' \\
  --vscode-bundle '/Applications/Visual Studio Code.app' \\
  --vscode-executable '/Applications/Visual Studio Code.app/Contents/MacOS/Code' \\
  --extension-root "$HOME/.vscode/extensions/openai.chatgpt-26.721.30844-darwin-arm64" \\
  --extension-package-json "$HOME/.vscode/extensions/openai.chatgpt-26.721.30844-darwin-arm64/package.json" \\
  --ide-client "$HOME/.vscode/extensions/openai.chatgpt-26.721.30844-darwin-arm64/bin/macos-aarch64/codex" \\
  --output-root tests/codex/acquisition/sprint11/host-provenance"""

SPRINT10_SOURCE_COMMIT = "b225f77acf48648ef6f59a76ff5a7dbb824da7bb"
SPRINT10_EVIDENCE_COMMIT = "815bccedddcd7c64a387dc8e9d5de2e941003fea"
SPRINT10_EVIDENCE_PATH = "tests/codex/evidence/sprint-10-read-write-beta-macos.json"
SPRINT10_EVIDENCE_SHA256 = (
    "sha256:9785a50e1be25f511acd6bd67f6642998821f700aea31a0bb4a7c1670f5b1611"
)
SPRINT10_SOURCE_SHA256 = (
    "sha256:c5fe1303e005d5866b28be6c8162bb8bebe4507975fd37eb55f690d72d4dc978"
)
EXACT_GODOT_PREREQUISITE_SHA256 = (
    "sha256:1bb7641df7dde23e6dbd638d40cf2ad0de4d09f34e08614a19be3e93879d4371"
)
EXACT_GODOT_INSTALL_PATH = (
    "~/Applications/Godot Codex.app/Contents/MacOS/Godot"
)
EXACT_GODOT_VERIFICATION = {
    "sha256": ["/usr/bin/shasum", "-a", "256", "<godot-binary>"],
    "version": ["<godot-binary>", "--version"],
}

MAX_EVIDENCE_BYTES = 262_144
MAX_TRACE_BYTES = 524_288
MAX_RECORDER_JOURNAL_BYTES = 524_288
MAX_ARTIFACT_BYTES = 2_097_152
MAX_PACKAGE_MANIFEST_BYTES = 262_144
MAX_GATE_OUTPUT_BYTES = 1_048_576
MAX_HUMAN_AUTHORITY_BYTES = 262_144

COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")
DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
PARTICIPANT_RE = re.compile(r"participant:sha256:[0-9a-f]{64}\Z")
SEMANTIC_ID_RE = re.compile(r"[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+\Z")

REQUIRED_ASSERTIONS = frozenset(
    {
        "approval.accept",
        "approval.cancel",
        "approval.decline",
        "approval.timeout",
        "connection.status",
        "host.approval_layers",
        "host.config_reload",
        "host.launcher",
        "host.offline_status",
        "host.unsupported_form",
        "multi_project.reject",
        "offline.saved_query",
        "runtime.error_stack",
        "saved.current_scene",
        "transaction.apply",
        "transaction.preview",
        "transaction.undo",
        "validation.result",
    }
)
REQUIRED_USABILITY_GOALS = frozenset(
    {
        "compound_write_and_undo",
        "doctor_fault_matrix",
        "install_and_connect",
        "offline_context",
        "project_isolation",
        "runtime_diagnostics",
        "saved_semantic_context",
    }
)
PROMPT_GOALS = {
    "install_and_connect": (
        "Install the frozen manifest-bound package through its package-owned launcher and connect the exact trusted project.",
        (
            "connection.status",
            "host.approval_layers",
            "host.config_reload",
            "host.launcher",
        ),
    ),
    "saved_semantic_context": (
        "Identify the current saved scene and support one semantic fact with bounded evidence.",
        ("saved.current_scene",),
    ),
    "offline_context": (
        "Close the editor, inspect honest offline status, and use only eligible verified saved-project context.",
        ("host.offline_status", "offline.saved_query"),
    ),
    "doctor_fault_matrix": (
        "Diagnose and recover from each of the five frozen local doctor faults using its stable diagnostic code and remediation ID.",
        ("connection.status",),
    ),
    "runtime_diagnostics": (
        "Run the project and locate the intentional runtime error with its bounded source-mapped stack.",
        ("runtime.error_stack",),
    ),
    "compound_write_and_undo": (
        "Preview, independently approve, apply, validate, and exactly undo one supported compound change; also prove decline, cancel, timeout, and unsupported-form paths do not mutate.",
        (
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
    ),
    "project_isolation": (
        "Open a second project and prove that cross-project identities and selectors are rejected without fallback.",
        ("multi_project.reject",),
    ),
}
PROMPT_CONSTRAINTS = (
    "Use only the exact package-owned launcher at bin/godot-codex-mcp whose digest is bound by the package manifest, and bind it to the trusted project.",
    "Treat Codex sandbox approval and MCP action-only form approval as independent controls; neither substitutes for the other.",
    "Do not copy secrets, native handles, account identity, or unrestricted source/property content.",
    "Do not infer current live editor or runtime state while the editor is offline; offline access is read-only saved-project context.",
    "Do not use PATH lookup or record a private absolute launcher path.",
    "Exercise exactly the five named doctor faults and verify each stable diagnostic code and remediation ID.",
    "Do not disclose fixture-specific answers, golden outputs, tool-call sequences, or recovery steps in the task pack.",
)
PROMPT_GOAL_ORDER = (
    "install_and_connect",
    "saved_semantic_context",
    "offline_context",
    "doctor_fault_matrix",
    "runtime_diagnostics",
    "compound_write_and_undo",
    "project_isolation",
)
USABILITY_DOCTOR_FAULTS = {
    "missing_binary": ("binary_missing", "upgrade_godot_codex"),
    "stale_discovery": (
        "bridge_discovery_stale",
        "start_matching_editor",
    ),
    "version_mismatch": (
        "bridge_version_incompatible",
        "upgrade_godot_bridge",
    ),
    "authentication_failure": (
        "bridge_authentication_failed",
        "start_matching_editor",
    ),
    "invalid_project_config": (
        "project_config_invalid",
        "repair_project_config",
    ),
}
REQUIRED_GATES = frozenset(
    {
        "approval_and_trust",
        "compatibility_matrix",
        "connection_offline_reconnect",
        "doctor_fault_matrix",
        "final_integrity",
        "guidance_security",
        "mcp_profile",
        "multi_project_isolation",
        "package_reproducibility",
        "previous_sprint_contracts",
        "previous_sprint_regressions",
        "rust_quality",
        "setup_ownership",
        "surface_parity",
        "surface_real_workflows",
        "trace_contract",
        "usability_report",
    }
)
EXTERNAL_ARTIFACT_GATES = frozenset(
    {
        "previous_sprint_regressions",
        "multi_project_isolation",
        "package_reproducibility",
        "surface_parity",
        "surface_real_workflows",
        "usability_report",
    }
)
AUTOMATED_GATE_NAMES = REQUIRED_GATES - EXTERNAL_ARTIFACT_GATES

EVIDENCE_FIELDS = frozenset(
    {
        "acquisitions",
        "baseline",
        "cleanup",
        "compatibility",
        "deferred",
        "gates",
        "package",
        "profile",
        "prompt_pack",
        "protocols",
        "regressions",
        "redaction",
        "registry",
        "release_projection",
        "schema_version",
        "source",
        "sprint",
        "status",
        "surfaces",
        "usability",
    }
)
REQUIRED_SOURCE_PATHS = frozenset(
    {
        "godot-codex-mcp/crates/godot-codex-mcp/tests/offline_subprocess.rs",
        "godot-codex-mcp/packaging/build_macos.py",
        "godot-codex-mcp/packaging/generate_third_party_licenses.py",
        "godot-codex-mcp/packaging/upstream-licenses/"
        "Nugine-simd-d74c030d9dc4f3cae02146d1f497ff62726ef09a-LICENSE.txt",
        "godot-codex-mcp/packaging/upstream-licenses/"
        "Stranger6667-jsonschema-"
        "91dac5ee04f241b543f72e83c45b407f372dac6d-LICENSE.txt",
        "godot-codex-mcp/product/compatibility-matrix.v1.json",
        "godot-codex-mcp/product/host-coordinate-profile.v1.json",
        "godot-codex-mcp/product/registry-profile.v1.json",
        "godot-codex-mcp/product/THIRD_PARTY_LICENSES.txt",
        "godot-codex-mcp/schemas/godot_codex/host-coordinate-profile.schema.json",
        "godot-codex-mcp/schemas/godot_codex/compatibility-matrix.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-human-acquisition-authority.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-human-consent-receipt.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-human-usability-defect-ledger.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-human-usability-rubric.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-human-usability-trace.schema.json",
        "godot-codex-mcp/schemas/godot_codex/sprint11-evidence.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-host-provenance-acquisition.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-host-provenance.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-multi-project-report.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-multi-project-receipt.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-packaged-regression-receipt.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-reproducibility-receipt.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-surface-acquisition-authority.schema.json",
        "godot-codex-mcp/schemas/godot_codex/sprint11-surface-trace.schema.json",
        "godot-codex-mcp/schemas/godot_codex/"
        "sprint11-usability-report.schema.json",
        "tests/codex/prompts/sprint11-external-beta-v1.json",
        "tests/codex/sprint11_acceptance.py",
        "tests/codex/sprint11_acquisition_paths.py",
        "tests/codex/sprint11_approval_host_probe.py",
        "tests/codex/sprint11_external_acquisitions.py",
        "tests/codex/sprint11_host_provenance.py",
        "tests/codex/sprint11_multi_project.py",
        "tests/codex/sprint11_packaged_regressions.py",
        "tests/codex/sprint11_source_scopes.txt",
        "tests/codex/sprint11_surface_recorder.py",
        "tests/codex/test_sprint11_acceptance.py",
        "tests/codex/test_sprint11_acquisition_paths.py",
        "tests/codex/test_sprint11_approval_host_probe.py",
        "tests/codex/test_sprint11_external_acquisitions.py",
        "tests/codex/test_sprint11_guidance.py",
        "tests/codex/test_sprint11_host_provenance.py",
        "tests/codex/test_sprint11_multi_project.py",
        "tests/codex/test_sprint11_package.py",
        "tests/codex/test_sprint11_packaged_regressions.py",
        "tests/codex/test_sprint11_surface_recorder.py",
        "tests/codex/tests/sprint11_evidence_contract.rs",
        "tests/codex/usability/sprint11-consent-v1.md",
        "tests/codex/usability/sprint11-operator-protocol-v1.json",
        "tests/codex/usability/sprint11-participant-script-v1.json",
        "tests/codex/usability/test_human_acquisition_kit.py",
        "tests/codex/usability/validator.py",
    }
)
TRACE_FIELDS = frozenset(
    {
        "assertions",
        "bindings",
        "capture_kind",
        "form_outcomes",
        "host",
        "redaction",
        "registry",
        "revision_timeline",
        "schema_version",
        "status",
        "surface",
    }
)
RECORDER_JOURNAL_FIELDS = frozenset(
    {
        "bindings",
        "capture_kind",
        "events",
        "host_controls",
        "host",
        "integrity",
        "protocol_version",
        "redaction",
        "schema_version",
        "status",
        "surface",
    }
)
SURFACE_AUTHORITY_TRUST_BOUNDARY = (
    "git_binds_exact_surface_artifacts_external_operator_attests_actual_"
    "host_session_without_cryptographic_process_origin_proof"
)
SURFACE_AUTHORITY_DOMAIN = b"s11-surface-acquisition-authority-v1\0"
SURFACE_AUTHORITY_BINDING_FIELDS = frozenset(
    {
        "package_source_commit",
        "package_manifest_sha256",
        "host_provenance_sha256",
        "host_artifact_sha256",
        "client_artifact_sha256",
        "mcp_binary_sha256",
        "project_fixture_sha256",
        "prompt_pack_sha256",
        "trace_path",
        "trace_sha256",
        "recorder_journal_path",
        "recorder_journal_sha256",
    }
)
SURFACE_AUTHORITY_OBSERVATION_FIELDS = frozenset(
    {
        "actual_surface_session_observed",
        "official_host_process_observed",
        "recorder_stdio_bound_to_host_session",
        "exact_project_root_observed",
        "nested_cwd_resolution_observed",
        "project_config_reload_observed",
        "package_launcher_observed",
        "sandbox_approval_observed",
        "form_accept_observed",
        "form_decline_observed",
        "form_cancel_observed",
        "form_timeout_observed",
        "foreign_project_config_rejected",
        "foreign_project_root_rejected",
        "foreign_project_cwd_rejected",
        "package_digest_mismatch_rejected_before_launch",
        "package_version_mismatch_rejected_before_launch",
        "cross_project_fallback_absent",
        "developer_bypass_absent",
    }
)
REGISTRY_FIELDS = frozenset(
    {
        "digest",
        "fixed_resources",
        "fixed_resource_count",
        "profile_id",
        "read_only_tools",
        "resource_templates",
        "resource_template_count",
        "schema_version",
        "tool_count",
        "tools",
    }
)
PACKAGE_FIELDS = frozenset(
    {
        "archive",
        "build_provenance",
        "compatibility_matrix_sha256",
        "contents",
        "godot_prerequisite",
        "package_version",
        "registry_sha256",
        "schema_version",
        "source_commit",
        "third_party_licenses_sha256",
    }
)
BUILD_PROVENANCE_FIELDS = frozenset(
    {
        "cargo_lock_sha256",
        "cargo_version",
        "fresh_target",
        "rust_toolchain_sha256",
        "rustc_commit",
        "rustc_release",
        "target_triple",
    }
)
USABILITY_FIELDS = frozenset(
    {
        "acquisition_kind",
        "attestation_trust_boundary",
        "defects",
        "metrics",
        "package_manifest_sha256",
        "package_source_commit",
        "participants",
        "prompt_pack",
        "schema_version",
        "status",
    }
)
USABILITY_PARTICIPANT_FIELDS = frozenset(
    {
        "participant_id",
        "external_attestation_independence",
        "tasks",
        "doctor_faults",
        "trace_path",
        "trace_sha256",
        "rubric_path",
        "rubric_sha256",
        "consent_path",
        "consent_sha256",
        "defect_ledger_path",
        "defect_ledger_sha256",
        "authority_path",
        "authority_sha256",
    }
)
USABILITY_EVIDENCE_FIELDS = frozenset(
    {
        "report_path",
        "report_sha256",
        "participants",
        "externally_attested_independent_participant",
        "status",
    }
)
HUMAN_ARTIFACT_FILENAMES = {
    "trace": "trace.json",
    "rubric": "rubric.json",
    "consent": "consent.json",
    "defect_ledger": "defect-ledger.json",
    "authority": "authority.json",
}

IGNORED_PARITY_KEYS = frozenset(
    {
        "acquired_at",
        "host_request_id",
        "latency_ms",
        "model_prose",
        "request_id",
        "thread_id",
        "timestamp",
        "ui_label",
    }
)
FORBIDDEN_KEYS = frozenset(
    {
        "absolute_project_root",
        "account",
        "approval_content",
        "approval_grant",
        "authorization",
        "email",
        "endpoint",
        "hmac_proof",
        "instance_id",
        "native_handle",
        "native_id",
        "object_id",
        "participant_name",
        "participant_prompt",
        "pid",
        "raw_property_value",
        "session_token",
        "source_content",
        "source_text",
        "token",
        "window_handle",
    }
)
ABSOLUTE_PATH_PATTERNS = (
    re.compile(r"(?:^|[\s\"'])/Users/"),
    re.compile(r"(?:^|[\s\"'])/home/"),
    re.compile(r"(?:^|[\s\"'])/private/"),
    re.compile(r"(?:^|[\s\"'])/(?:tmp|var/folders)/"),
    re.compile(r"(?:^|[\s\"'])[A-Za-z]:[\\/]"),
    re.compile(r"\\\\[^\\\s]+\\[^\\\s]+"),
)
SECRET_PATTERNS = (
    re.compile(r"\bBearer\s+[A-Za-z0-9._~+/=-]{12,}", re.IGNORECASE),
    re.compile(r"\bsk-(?:proj-)?[A-Za-z0-9_-]{12,}"),
    re.compile(r"\bgh[opsu]_[A-Za-z0-9]{20,}"),
    re.compile(r"\bS11_(?:SECRET|TOKEN|PROOF|CANARY)_[A-Za-z0-9_-]+"),
)
PREVIEW_OPERATION_FIELDS = {
    "create_node": frozenset(
        {
            "kind",
            "parent_scene_node_id",
            "godot_type",
            "name_redacted",
            "name_digest",
        }
    ),
    "delete_node": frozenset(
        {"kind", "scene_node_id", "subtree_node_count"}
    ),
    "reparent_node": frozenset(
        {"kind", "scene_node_id", "new_parent_scene_node_id"}
    ),
    "set_property": frozenset(
        {
            "kind",
            "scene_node_id",
            "property_name",
            "value_type",
            "value_redacted",
            "value_digest",
        }
    ),
    "attach_script": frozenset(
        {
            "kind",
            "scene_node_id",
            "script_redacted",
            "script_digest",
        }
    ),
    "detach_script": frozenset(
        {
            "kind",
            "scene_node_id",
            "script_redacted",
            "script_digest",
        }
    ),
    "connect_signal": frozenset(
        {
            "kind",
            "emitter_scene_node_id",
            "receiver_scene_node_id",
            "signal_name",
            "method_name",
            "binds_redacted",
            "binds_digest",
        }
    ),
    "disconnect_signal": frozenset(
        {
            "kind",
            "emitter_scene_node_id",
            "receiver_scene_node_id",
            "signal_name",
            "method_name",
            "binds_redacted",
            "binds_digest",
        }
    ),
}

SESSION_KEYS = (
    "editor_session_id",
    "runtime_session_id",
)
REVISION_SUFFIXES = (
    "event_seq",
    "generation_revision",
    "index_revision",
    "operation_seq",
    "resource_revision",
    "runtime_revision",
    "scene_graph_revision",
    "scene_revision",
    "script_graph_revision",
)
EPHEMERAL_KEY_CATEGORIES = {
    "change_set_id": "transaction",
    "cursor": "cursor",
    "editor_node_id": "editor_node",
    "editor_session_id": "editor_session",
    "ephemeral_evidence_id": "ephemeral_evidence",
    "report_id": "report",
    "runtime_node_id": "runtime_node",
    "runtime_object_id": "runtime_object",
    "runtime_session_id": "runtime_session",
    "runtime_session_after": "runtime_session",
    "runtime_session_before": "runtime_session",
    "snapshot_id": "snapshot",
    "stack_frame_id": "stack_frame",
    "stack_id": "stack",
    "transaction_id": "transaction",
    "validation_report_id": "report",
}


class AcceptanceError(RuntimeError):
    """Raised when evidence does not prove a closed Sprint 11 invariant."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AcceptanceError(message)


@dataclass(frozen=True)
class GateCommand:
    cwd: str
    argv: tuple[str, ...]


@dataclass(frozen=True)
class GateCommandGroup:
    group_id: str
    gates: tuple[str, ...]
    commands: tuple[GateCommand, ...]


@dataclass(frozen=True)
class CommandObservation:
    command_sha256: str
    stdout_sha256: str
    stderr_sha256: str
    duration_ms: int


def _pinned_rust_toolchain() -> str:
    path = REPOSITORY_ROOT / "godot-codex-mcp" / "rust-toolchain.toml"
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
        channel = document["toolchain"]["channel"]
    except (OSError, KeyError, TypeError, tomllib.TOMLDecodeError) as error:
        raise AcceptanceError("pinned Rust toolchain is unavailable") from error
    require(
        isinstance(channel, str)
        and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", channel) is not None,
        "pinned Rust toolchain differs",
    )
    return channel


def _gate_executable(token: str) -> Path:
    if token == "{python}":
        candidate = Path(sys.executable)
    elif token == "cargo":
        try:
            account_home = Path(pwd.getpwuid(os.getuid()).pw_dir)
        except (KeyError, OSError) as error:
            raise AcceptanceError("gate account home is unavailable") from error
        candidate = account_home / ".cargo" / "bin" / "cargo"
    else:
        raise AcceptanceError("gate executable is not allowlisted")
    require(
        candidate.is_absolute()
        and candidate.is_file()
        and os.access(candidate, os.X_OK),
        "gate executable is unavailable",
    )
    return candidate


def _gate_executable_identity(path: Path) -> tuple[str, int, int]:
    try:
        resolved = path.resolve(strict=True)
        metadata = resolved.stat()
    except OSError as error:
        raise AcceptanceError("gate executable identity is unavailable") from error
    require(
        resolved.is_file()
        and 0 < metadata.st_size <= 256 * 1024 * 1024,
        "gate executable identity differs",
    )
    return sha256_file(resolved), metadata.st_size, metadata.st_mode


def _closed_gate_environment(temporary_root: Path) -> dict[str, str]:
    try:
        account = pwd.getpwuid(os.getuid())
    except (KeyError, OSError) as error:
        raise AcceptanceError("gate account is unavailable") from error
    home = Path(account.pw_dir)
    cargo_home = home / ".cargo"
    rustup_home = home / ".rustup"
    require(
        home.is_absolute()
        and cargo_home.is_dir()
        and rustup_home.is_dir()
        and temporary_root.is_absolute(),
        "gate tool homes are unavailable",
    )
    return {
        "CARGO_HOME": str(cargo_home),
        "CARGO_NET_OFFLINE": "true",
        "CARGO_TERM_COLOR": "never",
        "HOME": str(home),
        "LANG": "C",
        "LC_ALL": "C",
        "LOGNAME": account.pw_name,
        "NO_COLOR": "1",
        "PATH": f"{cargo_home}/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        "PYTHONHASHSEED": "0",
        "PYTHONNOUSERSITE": "1",
        "RUSTUP_HOME": str(rustup_home),
        "RUSTUP_TOOLCHAIN": _pinned_rust_toolchain(),
        "TMPDIR": str(temporary_root),
        "TZ": "UTC",
        "USER": account.pw_name,
    }


AUTOMATED_GATE_GROUPS = (
    GateCommandGroup(
        group_id="s11_python_contracts",
        gates=(
            "approval_and_trust",
            "final_integrity",
            "guidance_security",
            "trace_contract",
        ),
        commands=(
            GateCommand(
                cwd=".",
                argv=(
                    "{python}",
                    "-m",
                    "unittest",
                    "tests.codex.test_sprint11_acceptance",
                    "tests.codex.test_sprint11_acquisition_paths",
                    "tests.codex.test_sprint11_approval_host_probe",
                    "tests.codex.test_sprint11_external_acquisitions",
                    "tests.codex.test_sprint11_guidance",
                    "tests.codex.test_sprint11_host_provenance",
                    "tests.codex.test_sprint11_multi_project",
                    "tests.codex.test_sprint11_package",
                    "tests.codex.test_sprint11_packaged_regressions",
                    "tests.codex.test_sprint11_surface_recorder",
                    "tests.codex.usability.test_human_acquisition_kit",
                ),
            ),
            GateCommand(
                cwd="godot-codex-mcp",
                argv=(
                    "{python}",
                    "packaging/generate_third_party_licenses.py",
                    "--check",
                ),
            ),
            GateCommand(
                cwd="godot-codex-mcp",
                argv=(
                    "cargo",
                    "test",
                    "--locked",
                    "-p",
                    "godot-codex-mcp-server",
                    "--test",
                    "approval_boundary",
                ),
            ),
        ),
    ),
    GateCommandGroup(
        group_id="product_operations_contracts",
        gates=(
            "compatibility_matrix",
            "doctor_fault_matrix",
            "mcp_profile",
            "setup_ownership",
        ),
        commands=(
            GateCommand(
                cwd="godot-codex-mcp",
                argv=(
                    "cargo",
                    "test",
                    "--locked",
                    "-p",
                    "godot-codex-product",
                    "-p",
                    "godot-codex-operations",
                    "-p",
                    "godot-codex",
                ),
            ),
        ),
    ),
    GateCommandGroup(
        group_id="connection_offline_contracts",
        gates=("connection_offline_reconnect",),
        commands=(
            GateCommand(
                cwd="godot-codex-mcp",
                argv=(
                    "cargo",
                    "test",
                    "--locked",
                    "-p",
                    "godot-codex-resource-indexer",
                    "-p",
                    "godot-codex-mcp-server",
                    "-p",
                    "godot-codex-mcp",
                    "--all-targets",
                ),
            ),
        ),
    ),
    GateCommandGroup(
        group_id="previous_sprint_contracts",
        gates=("previous_sprint_contracts",),
        commands=(
            GateCommand(
                cwd=".",
                argv=(
                    "{python}",
                    "-m",
                    "unittest",
                    "tests.codex.test_sprint6_acceptance",
                    "tests.codex.test_sprint7_acceptance",
                    "tests.codex.test_sprint8_acceptance",
                    "tests.codex.test_sprint9_acceptance",
                    "tests.codex.test_sprint9_contracts",
                    "tests.codex.test_sprint10_acceptance",
                    "tests.codex.test_sprint10_fixture",
                ),
            ),
            GateCommand(
                cwd="tests/codex",
                argv=("cargo", "test", "--locked", "--all-targets"),
            ),
        ),
    ),
    GateCommandGroup(
        group_id="rust_quality",
        gates=("rust_quality",),
        commands=(
            GateCommand(
                cwd="godot-codex-mcp",
                argv=("cargo", "fmt", "--all", "--check"),
            ),
            GateCommand(
                cwd="godot-codex-mcp",
                argv=(
                    "cargo",
                    "test",
                    "--locked",
                    "--workspace",
                    "--all-targets",
                ),
            ),
            GateCommand(
                cwd="godot-codex-mcp",
                argv=(
                    "cargo",
                    "clippy",
                    "--locked",
                    "--workspace",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ),
            ),
        ),
    ),
)

EXTERNAL_GATE_DEFINITIONS = {
    "multi_project_isolation": {
        "kind": "source_bound_artifact_validator",
        "validator": "validate_multi_project_receipt/1",
        "requires": [
            "two_real_editors",
            "two_package_sidecars",
            "foreign_binding_faults",
        ],
    },
    "package_reproducibility": {
        "kind": "source_bound_artifact_validator",
        "validator": "validate_reproducibility_receipt/1",
        "requires": [
            "two_clean_public_cli_builds",
            "byte_identical_archive",
            "byte_identical_manifest",
        ],
    },
    "previous_sprint_regressions": {
        "kind": "source_bound_artifact_validator",
        "validator": "validate_packaged_regression_receipt/1",
        "requires": [
            "real_package_live_capture",
            "exact_package_sidecar",
            "sprint6_through_sprint10_reports",
        ],
    },
    "surface_parity": {
        "kind": "source_bound_artifact_validator",
        "validator": "compare_surface_traces/2",
        "requires": [
            "app_trace_and_recorder",
            "cli_trace_and_recorder",
            "ide_trace_and_recorder",
        ],
    },
    "surface_real_workflows": {
        "kind": "source_bound_artifact_validator",
        "validator": "validate_surface_trace/2",
        "requires": [
            "surface_transport_capture",
            "complete_recorder_journal",
            "git_bound_external_operator_authority",
            "semantic_assertions",
        ],
    },
    "usability_report": {
        "kind": "source_bound_artifact_validator",
        "validator": "validate_usability_report/2",
        "requires": [
            "real_human",
            "three_participants",
            "externally_attested_independent_participant",
            "git_bound_primary_bundle",
            "git_bound_external_operator_authority",
            "exact_doctor_fault_matrix",
        ],
    },
}


def _gate_group_by_name() -> dict[str, GateCommandGroup]:
    result: dict[str, GateCommandGroup] = {}
    for group in AUTOMATED_GATE_GROUPS:
        require(
            group.group_id
            and group.commands
            and set(group.gates).issubset(AUTOMATED_GATE_NAMES),
            "automated gate group definition differs",
        )
        for gate in group.gates:
            require(gate not in result, "automated gate is assigned twice")
            result[gate] = group
    require(
        set(result) == AUTOMATED_GATE_NAMES,
        "automated gate definition coverage differs",
    )
    return result


def gate_definition(gate: str) -> dict[str, Any]:
    require(gate in REQUIRED_GATES, "unknown acceptance gate")
    if gate in EXTERNAL_GATE_DEFINITIONS:
        return {
            "gate": gate,
            **EXTERNAL_GATE_DEFINITIONS[gate],
        }
    group = _gate_group_by_name()[gate]
    return {
        "gate": gate,
        "kind": "bounded_subprocess",
        "group_id": group.group_id,
        "commands": [
            {"cwd": command.cwd, "argv": list(command.argv)}
            for command in group.commands
        ],
    }


def gate_definition_sha256(gate: str) -> str:
    return sha256_bytes(canonical_json(gate_definition(gate)))


def canonical_gate_bindings() -> dict[str, dict[str, str]]:
    return {
        gate: {
            "runner": "sprint11_acceptance/1.0",
            "definition_sha256": gate_definition_sha256(gate),
        }
        for gate in sorted(REQUIRED_GATES)
    }


def _run_gate_command(
    command: GateCommand,
    timeout: float,
) -> CommandObservation:
    require(
        1 <= timeout <= 180,
        "gate timeout must be between 1 and 180 seconds",
    )
    cwd = REPOSITORY_ROOT.joinpath(*PurePosixPath(command.cwd).parts)
    require(
        cwd.is_dir() and REPOSITORY_ROOT in (cwd, *cwd.parents),
        "gate working directory differs",
    )
    require(bool(command.argv), "gate command is empty")
    executable = _gate_executable(command.argv[0])
    executable_identity = _gate_executable_identity(executable)
    argv = (str(executable), *command.argv[1:])
    command_binding = {
        "cwd": command.cwd,
        "argv": list(command.argv),
        "executable_sha256": executable_identity[0],
    }
    started = time.monotonic()
    # Rust integration tests create project-local Unix sockets below TMPDIR.
    # Keep the private gate root short enough for Darwin's sockaddr_un limit;
    # nesting it below the account's long per-user TMPDIR makes otherwise
    # valid socket fixtures fail before the gate under test can run.
    temporary_parent = Path("/tmp") if os.name == "posix" else None
    with tempfile.TemporaryDirectory(
        prefix="s11-gate-",
        dir=temporary_parent,
    ) as temporary:
        require(
            os.name != "posix" or len(os.fsencode(temporary)) <= 48,
            "gate temporary root exceeds Unix socket path budget",
        )
        environment = _closed_gate_environment(Path(temporary))
        try:
            process = subprocess.Popen(
                argv,
                cwd=cwd,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=os.name == "posix",
            )
        except OSError as error:
            raise AcceptanceError("acceptance gate command could not start") from error

        exceeded = threading.Event()
        stdout_buffer = bytearray()
        stderr_buffer = bytearray()

        def drain(stream: Any, destination: bytearray) -> None:
            try:
                while chunk := stream.read(64 * 1024):
                    remaining = MAX_GATE_OUTPUT_BYTES + 1 - len(destination)
                    if remaining > 0:
                        destination.extend(chunk[:remaining])
                    if len(destination) > MAX_GATE_OUTPUT_BYTES:
                        exceeded.set()
                        return
            finally:
                stream.close()

        require(
            process.stdout is not None and process.stderr is not None,
            "acceptance gate pipes are unavailable",
        )
        stdout_thread = threading.Thread(
            target=drain,
            args=(process.stdout, stdout_buffer),
            daemon=True,
        )
        stderr_thread = threading.Thread(
            target=drain,
            args=(process.stderr, stderr_buffer),
            daemon=True,
        )
        stdout_thread.start()
        stderr_thread.start()
        deadline = started + timeout
        timed_out = False
        while process.poll() is None:
            if exceeded.is_set():
                break
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                timed_out = True
                break
            exceeded.wait(min(remaining, 0.05))
        if process.poll() is None:
            if os.name == "posix":
                os.killpg(process.pid, signal.SIGKILL)
            else:
                process.kill()
            process.wait()
        return_code = process.returncode
        stdout_thread.join(timeout=2)
        stderr_thread.join(timeout=2)
        require(
            not stdout_thread.is_alive() and not stderr_thread.is_alive(),
            "acceptance gate output drain did not stop",
        )
        if timed_out:
            raise AcceptanceError("acceptance gate command timed out")
        require(
            not exceeded.is_set(),
            "acceptance gate output exceeds byte bound",
        )
        duration_ms = int((time.monotonic() - started) * 1_000)
        stdout_bytes = bytes(stdout_buffer)
        stderr_bytes = bytes(stderr_buffer)
        require(
            _gate_executable_identity(executable) == executable_identity,
            "gate executable changed during execution",
        )
    require(
        return_code == 0,
        "acceptance gate command failed",
    )
    return CommandObservation(
        command_sha256=sha256_bytes(canonical_json(command_binding)),
        stdout_sha256=sha256_bytes(stdout_bytes),
        stderr_sha256=sha256_bytes(stderr_bytes),
        duration_ms=duration_ms,
    )


def run_automated_gates(
    timeout: float,
    *,
    executor: Callable[[GateCommand, float], CommandObservation]
    = _run_gate_command,
) -> dict[str, dict[str, Any]]:
    """Run every automatable gate; no evidence field can substitute for this."""

    require(
        1 <= timeout <= 180,
        "--timeout must be between 1 and 180 seconds",
    )
    results: dict[str, dict[str, Any]] = {}
    for group in AUTOMATED_GATE_GROUPS:
        try:
            observations = [
                executor(command, timeout) for command in group.commands
            ]
        except AcceptanceError as error:
            raise AcceptanceError(
                f"automated gate group {group.group_id} failed: {error}"
            ) from error
        command_receipts = [
            {
                "command_sha256": item.command_sha256,
                "stdout_sha256": item.stdout_sha256,
                "stderr_sha256": item.stderr_sha256,
            }
            for item in observations
        ]
        receipt = sha256_bytes(canonical_json(command_receipts))
        duration = sum(item.duration_ms for item in observations)
        for gate in group.gates:
            results[gate] = {
                "status": "passed",
                "definition_sha256": gate_definition_sha256(gate),
                "receipt_sha256": receipt,
                "duration_ms": duration,
            }
    require(
        set(results) == AUTOMATED_GATE_NAMES,
        "automated gate execution coverage differs",
    )
    return results


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
        raise AcceptanceError("value is not canonical JSON") from error


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    try:
        return sha256_bytes(path.read_bytes())
    except OSError as error:
        raise AcceptanceError("required artifact is unavailable") from error


def strict_json_bytes(
    data: bytes,
    *,
    label: str,
    maximum_bytes: int = MAX_ARTIFACT_BYTES,
) -> dict[str, Any]:
    require(len(data) <= maximum_bytes, f"{label}: JSON exceeds byte bound")

    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in items:
            require(key not in value, f"{label}: duplicate JSON member")
            value[key] = item
        return value

    def constant(_name: str) -> None:
        raise AcceptanceError(f"{label}: non-finite JSON number")

    try:
        result = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=pairs,
            parse_constant=constant,
        )
    except (UnicodeError, json.JSONDecodeError) as error:
        raise AcceptanceError(f"{label}: invalid strict JSON") from error
    require(isinstance(result, dict), f"{label}: root is not an object")
    return cast(dict[str, Any], result)


def strict_json_load(
    path: Path,
    *,
    maximum_bytes: int = MAX_ARTIFACT_BYTES,
) -> dict[str, Any]:
    try:
        data = path.read_bytes()
    except OSError as error:
        raise AcceptanceError(f"{path.name}: cannot read artifact") from error
    return strict_json_bytes(
        data,
        label=path.name,
        maximum_bytes=maximum_bytes,
    )


def _safe_relative_path(value: Any, *, label: str) -> str:
    require(isinstance(value, str), f"{label}: path is not a string")
    require(
        0 < len(value.encode("utf-8")) <= 512 and "\0" not in value,
        f"{label}: path bound differs",
    )
    path = PurePosixPath(value)
    require(
        not path.is_absolute()
        and value == path.as_posix()
        and ".." not in path.parts
        and "." not in path.parts,
        f"{label}: path is not canonical relative",
    )
    return value


def _bounded_string(value: Any, *, label: str, maximum: int = 256) -> str:
    require(
        isinstance(value, str) and 0 < len(value.encode("utf-8")) <= maximum,
        f"{label}: string bound differs",
    )
    return value


def _digest(value: Any, *, label: str) -> str:
    require(
        isinstance(value, str) and DIGEST_RE.fullmatch(value) is not None,
        f"{label}: SHA-256 binding differs",
    )
    return value


def _commit(value: Any, *, label: str) -> str:
    require(
        isinstance(value, str) and COMMIT_RE.fullmatch(value) is not None,
        f"{label}: commit binding differs",
    )
    return value


def _exact_fields(
    value: Any,
    expected: Iterable[str],
    *,
    label: str,
) -> Mapping[str, Any]:
    require(isinstance(value, dict), f"{label}: object is missing")
    require(set(value) == set(expected), f"{label}: fields differ")
    return cast(Mapping[str, Any], value)


def _walk(value: Any, *, path: tuple[str, ...] = ()) -> Iterable[tuple[tuple[str, ...], Any]]:
    yield path, value
    if isinstance(value, dict):
        for key in sorted(value):
            yield from _walk(value[key], path=(*path, key))
    elif isinstance(value, list):
        for index, item in enumerate(value):
            yield from _walk(item, path=(*path, str(index)))


def safe_evidence_scan(value: Any) -> None:
    """Reject secret, native-ID, source-content, account, and path leakage."""

    for path, item in _walk(value):
        if path:
            key = path[-1].lower()
            require(key not in FORBIDDEN_KEYS, "forbidden evidence field")
        if not isinstance(item, str):
            continue
        require(
            len(item.encode("utf-8")) <= 16_384,
            "evidence string exceeds byte bound",
        )
        require(
            not any(pattern.search(item) for pattern in ABSOLUTE_PATH_PATTERNS),
            "absolute private path leaked",
        )
        require(
            not any(pattern.search(item) for pattern in SECRET_PATTERNS),
            "secret or canary leaked",
        )


def derive_surface_trace_redaction(value: Mapping[str, Any]) -> dict[str, bool]:
    """Derive redaction claims from the closed trace payload, never its flags."""

    payload = {key: item for key, item in value.items() if key != "redaction"}
    derived = {
        "absolute_paths_absent": True,
        "approval_content_absent": True,
        "native_ids_absent": True,
        "secrets_absent": True,
        "source_content_absent": True,
        "truncated": False,
    }
    approval_keys = {"approval_content", "approval_grant", "authorization"}
    native_keys = {
        "instance_id",
        "native_handle",
        "native_id",
        "object_id",
        "pid",
        "window_handle",
    }
    source_keys = {
        "raw_property_value",
        "source_content",
        "source_text",
    }
    for path, item in _walk(payload):
        key = path[-1].lower() if path else ""
        if key in approval_keys:
            derived["approval_content_absent"] = False
        if key in native_keys or key.endswith("_native_id"):
            derived["native_ids_absent"] = False
        if key in source_keys:
            derived["source_content_absent"] = False
        if key == "truncated" and item is True:
            derived["truncated"] = True
        if isinstance(item, str):
            if any(pattern.search(item) for pattern in ABSOLUTE_PATH_PATTERNS):
                derived["absolute_paths_absent"] = False
            if any(pattern.search(item) for pattern in SECRET_PATTERNS):
                derived["secrets_absent"] = False
    return derived


class GitRepository:
    """Minimal immutable Git-object reader used by source-bound validation."""

    def __init__(self, root: Path = REPOSITORY_ROOT) -> None:
        self.root = root.resolve()

    def _run(
        self,
        arguments: Sequence[str],
        *,
        text: bool = False,
        check: bool = True,
    ) -> bytes | str:
        result = subprocess.run(
            ["git", *arguments],
            cwd=self.root,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=text,
        )
        if check and result.returncode != 0:
            raise AcceptanceError("required Git object or relation is unavailable")
        return result.stdout

    def head(self) -> str:
        value = cast(str, self._run(["rev-parse", "HEAD"], text=True)).strip()
        return _commit(value, label="HEAD")

    def parent(self, commit: str) -> str:
        value = cast(
            str,
            self._run(["rev-parse", f"{commit}^"], text=True),
        ).strip()
        return _commit(value, label="commit parent")

    def blob_at(self, commit: str, relative: str) -> bytes:
        _commit(commit, label="blob commit")
        _safe_relative_path(relative, label="blob")
        return cast(bytes, self._run(["show", f"{commit}:{relative}"]))

    def changed_paths(self, parent: str, child: str) -> list[str]:
        output = cast(
            bytes,
            self._run(
                [
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    "-z",
                    parent,
                    child,
                ]
            ),
        )
        return sorted(
            item.decode("utf-8")
            for item in output.split(b"\0")
            if item
        )

    def is_ancestor(self, ancestor: str, descendant: str) -> bool:
        _commit(ancestor, label="ancestor")
        _commit(descendant, label="descendant")
        result = subprocess.run(
            ["git", "merge-base", "--is-ancestor", ancestor, descendant],
            cwd=self.root,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        return result.returncode == 0

    def status_lines(self) -> list[str]:
        output = cast(
            str,
            self._run(
                ["status", "--porcelain=v1", "--untracked-files=all"],
                text=True,
            ),
        )
        return output.splitlines()

    def source_digest(
        self,
        commit: str,
        scope_manifest: str,
    ) -> tuple[str, int]:
        manifest = self.blob_at(commit, scope_manifest).decode("utf-8")
        scopes = parse_source_scopes(manifest)
        output = cast(
            bytes,
            self._run(
                [
                    "--literal-pathspecs",
                    "ls-tree",
                    "-r",
                    "-z",
                    commit,
                    "--",
                    *scopes,
                ]
            ),
        )
        entries: list[tuple[str, str, str]] = []
        for raw_entry in output.split(b"\0"):
            if not raw_entry:
                continue
            try:
                raw_header, raw_path = raw_entry.split(b"\t", 1)
                raw_mode, raw_type, _raw_object = raw_header.split(b" ", 2)
                mode = raw_mode.decode("ascii")
                object_type = raw_type.decode("ascii")
                relative = raw_path.decode("utf-8")
            except (UnicodeDecodeError, ValueError) as error:
                raise AcceptanceError("source tree entry is invalid") from error
            require(object_type == "blob", "source scope contains a non-blob entry")
            entries.append((relative, mode, object_type))
        entries.sort()
        require(entries, "source scope resolves to no committed files")
        digest = hashlib.sha256()
        for relative, mode, object_type in entries:
            digest.update(relative.encode("utf-8"))
            digest.update(b"\0")
            digest.update(mode.encode("ascii"))
            digest.update(b"\0")
            digest.update(object_type.encode("ascii"))
            digest.update(b"\0")
            digest.update(self.blob_at(commit, relative))
            digest.update(b"\0")
        return "sha256:" + digest.hexdigest(), len(entries)


def parse_source_scopes(value: str) -> tuple[str, ...]:
    scopes = tuple(
        line
        for line in value.splitlines()
        if line and not line.startswith("#")
    )
    require(scopes == tuple(sorted(set(scopes))), "source scopes are not canonical")
    for scope in scopes:
        _safe_relative_path(scope, label="source scope")
        require(
            not (
                EVIDENCE_RELATIVE_PATH == scope
                or EVIDENCE_RELATIVE_PATH.startswith(scope.rstrip("/") + "/")
            ),
            "source scope includes final evidence",
        )
    require(
        all(
            any(
                required_path == scope
                or required_path.startswith(scope.rstrip("/") + "/")
                for scope in scopes
            )
            for required_path in REQUIRED_SOURCE_PATHS
        ),
        "source scope omits acceptance/package contract",
    )
    return scopes


def validate_sprint10_baseline(repository: GitRepository) -> None:
    require(
        repository.parent(SPRINT10_EVIDENCE_COMMIT) == SPRINT10_SOURCE_COMMIT,
        "Sprint 10 evidence parent differs",
    )
    require(
        repository.changed_paths(
            SPRINT10_SOURCE_COMMIT,
            SPRINT10_EVIDENCE_COMMIT,
        )
        == [SPRINT10_EVIDENCE_PATH],
        "Sprint 10 evidence commit is not evidence-only",
    )
    blob = repository.blob_at(
        SPRINT10_EVIDENCE_COMMIT,
        SPRINT10_EVIDENCE_PATH,
    )
    require(
        sha256_bytes(blob) == SPRINT10_EVIDENCE_SHA256,
        "Sprint 10 immutable evidence digest differs",
    )
    evidence = strict_json_bytes(
        blob,
        label="Sprint 10 evidence",
        maximum_bytes=MAX_EVIDENCE_BYTES,
    )
    source = evidence.get("source")
    require(
        isinstance(source, dict)
        and source.get("commit") == SPRINT10_SOURCE_COMMIT
        and source.get("sha256") == SPRINT10_SOURCE_SHA256
        and evidence.get("status") == "passed",
        "Sprint 10 immutable source binding differs",
    )


def validate_source_binding(
    source: Any,
    repository: GitRepository,
) -> str:
    value = _exact_fields(
        source,
        {"commit", "sha256", "file_count", "scope_manifest"},
        label="source",
    )
    commit = _commit(value["commit"], label="source")
    require(
        value["scope_manifest"] == "tests/codex/sprint11_source_scopes.txt",
        "source scope manifest differs",
    )
    digest, count = repository.source_digest(
        commit,
        cast(str, value["scope_manifest"]),
    )
    require(
        value["sha256"] == digest and value["file_count"] == count,
        "source digest binding differs",
    )
    return commit


def validate_evidence_checkout_relation(
    source_commit: str,
    evidence_path: Path,
    repository: GitRepository,
) -> str:
    try:
        relative = evidence_path.resolve().relative_to(repository.root).as_posix()
    except ValueError as error:
        raise AcceptanceError("evidence path is outside repository") from error
    head = repository.head()
    status = repository.status_lines()
    if head == source_commit:
        require(
            status == [f"?? {relative}"],
            "qualifying source checkout has unrelated changes",
        )
        return "qualifying_parent"
    require(
        repository.parent(head) == source_commit,
        "evidence is not based on its declared source commit",
    )
    require(
        repository.changed_paths(source_commit, head) == [relative],
        "final evidence commit is not evidence-only",
    )
    require(not status, "post-evidence checkout is dirty")
    return "evidence_commit"


def validate_post_package_changed_paths(
    *,
    repository: GitRepository,
    package_source_commit: str,
    source_commit: str,
    acquisition_paths: set[str],
) -> None:
    changed = set(
        repository.changed_paths(package_source_commit, source_commit)
    )
    require(
        changed <= acquisition_paths,
        "source changed outside package-bound acquisition artifacts",
    )


def validate_prompt_pack(value: Any) -> None:
    document = _exact_fields(
        value,
        {
            "schema_version",
            "pack_id",
            "scope",
            "generic_goals",
            "doctor_faults",
            "semantic_assertions",
            "constraints",
        },
        label="prompt pack",
    )
    require(
        document["schema_version"] == "s11-prompt-pack/1.0"
        and document["pack_id"] == "sprint11-external-beta-v1"
        and document["scope"] == "generic_external_codex_beta_workflow",
        "prompt pack identity differs",
    )
    assertions = document["semantic_assertions"]
    require(
        isinstance(assertions, list)
        and assertions == sorted(REQUIRED_ASSERTIONS),
        "prompt pack assertion set differs",
    )
    goals = document["generic_goals"]
    require(
        isinstance(goals, list) and len(goals) == len(PROMPT_GOAL_ORDER),
        "prompt pack goals are missing",
    )
    goal_ids: set[str] = set()
    projected: set[str] = set()
    for index, goal in enumerate(goals):
        record = _exact_fields(
            goal,
            {"goal_id", "intent", "semantic_assertion_ids"},
            label="prompt goal",
        )
        goal_id = _bounded_string(record["goal_id"], label="goal ID", maximum=64)
        require(goal_id not in goal_ids, "prompt goal is duplicated")
        goal_ids.add(goal_id)
        require(
            goal_id == PROMPT_GOAL_ORDER[index]
            and goal_id in PROMPT_GOALS,
            "prompt goal order or identity differs",
        )
        intent = _bounded_string(
            record["intent"],
            label="generic intent",
            maximum=512,
        )
        ids = record["semantic_assertion_ids"]
        expected_intent, expected_ids = PROMPT_GOALS[goal_id]
        require(
            isinstance(ids, list)
            and len(ids) == len(set(ids))
            and set(ids).issubset(REQUIRED_ASSERTIONS),
            "prompt goal assertion projection differs",
        )
        require(
            intent == expected_intent and ids == list(expected_ids),
            "prompt goal frozen task contract differs",
        )
        projected.update(cast(list[str], ids))
    require(
        goal_ids == REQUIRED_USABILITY_GOALS
        and projected == REQUIRED_ASSERTIONS,
        "prompt pack coverage differs",
    )
    fault_records = document["doctor_faults"]
    require(
        isinstance(fault_records, list)
        and len(fault_records) == len(USABILITY_DOCTOR_FAULTS),
        "prompt doctor fault count differs",
    )
    observed_faults: dict[str, tuple[str, str]] = {}
    for item in fault_records:
        record = _exact_fields(
            item,
            {"scenario", "diagnostic_code", "remediation_id"},
            label="prompt doctor fault",
        )
        scenario = _bounded_string(
            record["scenario"],
            label="doctor scenario",
            maximum=64,
        )
        require(
            scenario not in observed_faults,
            "prompt doctor fault is duplicated",
        )
        diagnostic_code = _bounded_string(
            record["diagnostic_code"],
            label="doctor diagnostic code",
            maximum=64,
        )
        remediation_id = _bounded_string(
            record["remediation_id"],
            label="doctor remediation ID",
            maximum=64,
        )
        observed_faults[scenario] = (diagnostic_code, remediation_id)
    require(
        observed_faults == USABILITY_DOCTOR_FAULTS,
        "prompt doctor fault contract differs",
    )
    require(
        document["constraints"] == list(PROMPT_CONSTRAINTS),
        "prompt constraints or anti-coaching boundary differs",
    )
    constraints = document["constraints"]
    require(
        isinstance(constraints, list)
        and 1 <= len(constraints) <= 16
        and all(
            isinstance(item, str)
            and 0 < len(item.encode("utf-8")) <= 512
            for item in constraints
        ),
        "prompt constraints differ",
    )
    safe_evidence_scan(document)


def registry_profile_digest(value: Mapping[str, Any]) -> str:
    payload = {
        "profile_id": value["profile_id"],
        "tools": value["tools"],
        "read_only_tools": value["read_only_tools"],
        "fixed_resources": value["fixed_resources"],
        "resource_templates": value["resource_templates"],
    }
    encoded = json.dumps(
        payload,
        allow_nan=False,
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def validate_registry_profile_document(value: Any) -> dict[str, Any]:
    profile = _exact_fields(value, REGISTRY_FIELDS, label="registry profile")
    require(
        profile["schema_version"] == "godot-codex-registry-profile/1.0"
        and profile["profile_id"] == "external-codex-beta-v1"
        and profile["tool_count"] == 41
        and profile["fixed_resource_count"] == 4
        and profile["resource_template_count"] == 1,
        "registry profile identity/counts differ",
    )
    for field, count in (
        ("tools", 41),
        ("fixed_resources", 4),
        ("resource_templates", 1),
    ):
        items = profile[field]
        require(
            isinstance(items, list)
            and len(items) == count
            and items == sorted(set(items))
            and all(isinstance(item, str) for item in items),
            f"registry profile {field} differs",
        )
    tools = cast(list[str], profile["tools"])
    require(
        "godot_get_connection_status" in tools,
        "connection-status tool is absent from canonical registry",
    )
    fixed = cast(list[str], profile["fixed_resources"])
    require(
        "godot://connection/status" in fixed,
        "connection-status resource is absent from canonical registry",
    )
    read_only = profile["read_only_tools"]
    require(
        isinstance(read_only, list)
        and read_only == sorted(set(read_only))
        and set(read_only).issubset(tools)
        and "godot_get_connection_status" in read_only
        and "godot_apply_transaction" not in read_only
        and "godot_prepare_change_set" not in read_only
        and "godot_run_project" not in read_only,
        "read-only registry profile differs",
    )
    digest = profile["digest"]
    require(
        isinstance(digest, str)
        and re.fullmatch(r"[0-9a-f]{64}", digest) is not None
        and digest == registry_profile_digest(profile),
        "registry profile semantic digest differs",
    )
    safe_evidence_scan(profile)
    return dict(profile)


def canonical_registry_profile() -> dict[str, Any]:
    return validate_registry_profile_document(
        strict_json_load(REGISTRY_PROFILE_PATH)
    )


def canonical_server_instructions() -> dict[str, str]:
    try:
        raw = SERVER_INSTRUCTIONS_PATH.read_bytes()
    except OSError as error:
        raise AcceptanceError("canonical server instructions are unavailable") from error
    require(
        0 < len(raw) <= 8_192
        and raw.endswith(b"\n")
        and not raw.endswith(b"\n\n")
        and b"\0" not in raw,
        "canonical server instructions file differs",
    )
    wire = raw[:-1]
    try:
        instructions = wire.decode("utf-8")
    except UnicodeDecodeError as error:
        raise AcceptanceError("canonical server instructions are not UTF-8") from error
    first_window = instructions[:512]
    required_first_window_terms = (
        "bound to the trusted project",
        "godot_get_connection_status",
        "godot://connection/status",
        "Offline data is saved-project cache only",
        "never claim live editor/runtime state",
        "writes are unavailable",
        "immutable preview",
        "Codex sandbox approval",
        "MCP action-only form approval",
        "independent and both required",
        "neither substitutes for the other",
    )
    require(
        len(instructions) <= 4_096
        and all(term in first_window for term in required_first_window_terms),
        "canonical server instructions first window differs",
    )
    return {
        "file_sha256": sha256_bytes(raw),
        "wire_sha256": sha256_bytes(wire),
    }


HOST_PROFILE_SURFACE_FIELDS = frozenset(
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
EXACT_HOST_COORDINATES: Mapping[str, Mapping[str, Any]] = {
    "app": {
        "host_name": "codex-desktop",
        "host_identifier": "com.openai.codex",
        "host_artifact_kind": "macos_bundle_executable",
        "host_version": "26.721.31836",
        "host_build": "5828",
        "host_commit": None,
        "host_artifact_sha256": (
            "sha256:1e69df41e05969f1487dfdc9f72a600e"
            "f3b7627a7f8ee9d481cb8f88400eda45"
        ),
        "host_metadata_sha256": None,
        "host_code_signature": {
            "mode": "deep_strict",
            "identifier": "com.openai.codex",
            "team_id": "2DC432GLL2",
            "cdhash": "56572d4c92d53f0c09776d3654f86432716ca2f5",
        },
        "client_version": "0.146.0-alpha.3.1",
        "client_artifact_sha256": (
            "sha256:a2b6198fd61327f54542716bd96e588c"
            "5b10789522fee4bbacaeff1aa7836efb"
        ),
        "client_code_signature": {
            "mode": "strict",
            "identifier": "codex",
            "team_id": "2DC432GLL2",
            "cdhash": "b81d53d6df5ab26ce419cf2637a859d8c7f1f56e",
        },
        "ide_host_version": None,
        "ide_shell_identifier": None,
        "ide_shell_artifact_sha256": None,
        "ide_shell_team_id": None,
        "ide_shell_code_signature": None,
    },
    "cli": {
        "host_name": "codex-cli",
        "host_identifier": "codex-cli",
        "host_artifact_kind": "standalone_executable",
        "host_version": "0.146.0-alpha.3.1",
        "host_build": "0.146.0-alpha.3.1",
        "host_commit": None,
        "host_artifact_sha256": (
            "sha256:a2b6198fd61327f54542716bd96e588c"
            "5b10789522fee4bbacaeff1aa7836efb"
        ),
        "host_metadata_sha256": None,
        "host_code_signature": {
            "mode": "strict",
            "identifier": "codex",
            "team_id": "2DC432GLL2",
            "cdhash": "b81d53d6df5ab26ce419cf2637a859d8c7f1f56e",
        },
        "client_version": "0.146.0-alpha.3.1",
        "client_artifact_sha256": (
            "sha256:a2b6198fd61327f54542716bd96e588c"
            "5b10789522fee4bbacaeff1aa7836efb"
        ),
        "client_code_signature": {
            "mode": "strict",
            "identifier": "codex",
            "team_id": "2DC432GLL2",
            "cdhash": "b81d53d6df5ab26ce419cf2637a859d8c7f1f56e",
        },
        "ide_host_version": None,
        "ide_shell_identifier": None,
        "ide_shell_artifact_sha256": None,
        "ide_shell_team_id": None,
        "ide_shell_code_signature": None,
    },
    "ide": {
        "host_name": "openai.chatgpt",
        "host_identifier": "openai.chatgpt",
        "host_artifact_kind": "extension_tree",
        "host_version": "26.721.30844",
        "host_build": "26.721.30844",
        "host_commit": "4fe60c8b1cdac1c4c174f2fb180d0d758272d713",
        "host_artifact_sha256": (
            "sha256:3ff47b070a08d02acc9c596756b017cf"
            "264e3dd4169002613a9171e1219778b0"
        ),
        "host_metadata_sha256": (
            "sha256:497c84587406f0cb7022dece0752202d"
            "c4490781fde5bc88469207ab9b5cac13"
        ),
        "host_code_signature": None,
        "client_version": "0.146.0-alpha.3",
        "client_artifact_sha256": (
            "sha256:5ab45f8f9819c120bede3743f896e70"
            "da47ffe920b48d9a04cc25ecc9e2dd757"
        ),
        "client_code_signature": {
            "mode": "strict",
            "identifier": "codex",
            "team_id": "2DC432GLL2",
            "cdhash": "432912b777fd97aa5c40d2216aeb3102f433f852",
        },
        "ide_host_version": "1.127.0",
        "ide_shell_identifier": "com.microsoft.VSCode",
        "ide_shell_artifact_sha256": (
            "sha256:d2dbd60db1c2e63e6b844a1c13b61e"
            "6de12bc20391c8ec1bf7bf663b67b105a1"
        ),
        "ide_shell_team_id": "UBF8T346G9",
        "ide_shell_code_signature": {
            "mode": "deep_strict",
            "identifier": "com.microsoft.VSCode",
            "team_id": "UBF8T346G9",
            "cdhash": "4f0317b8f0d8d6f3d2e1b1b0e979514ad42fdd9b",
        },
    },
}


def _validate_observed_code_signature(value: Any, *, label: str) -> None:
    if value is None:
        return
    signature = _exact_fields(
        value,
        {"verified", "mode", "identifier", "team_id", "cdhash"},
        label=label,
    )
    require(
        signature["verified"] is True
        and signature["mode"] in {"strict", "deep_strict"}
        and isinstance(signature["cdhash"], str)
        and re.fullmatch(
            r"[0-9a-f]{40}(?:[0-9a-f]{24})?",
            signature["cdhash"],
        )
        is not None,
        f"{label}: observed signature differs",
    )
    for field in ("identifier", "team_id"):
        _semantic_token(
            signature[field],
            label=f"{label} {field}",
            maximum=128,
        )


def validate_host_coordinate_profile(
    value: Any,
    *,
    qualifying: bool,
) -> dict[str, Any]:
    profile = _exact_fields(
        value,
        {
            "schema_version",
            "profile_id",
            "compatibility_matrix",
            "server_instructions",
            "surfaces",
        },
        label="host coordinate profile",
    )
    require(
        profile["schema_version"]
        == "godot-codex-host-coordinate-profile/1.0"
        and profile["profile_id"]
        == "external-codex-beta-macos-arm64-hosts-v1",
        "host coordinate profile identity differs",
    )
    matrix_binding = _exact_fields(
        profile["compatibility_matrix"],
        {"path", "sha256"},
        label="host compatibility-matrix binding",
    )
    require(
        matrix_binding["path"]
        == "godot-codex-mcp/product/compatibility-matrix.v1.json"
        and matrix_binding["sha256"] == sha256_file(COMPATIBILITY_MATRIX_PATH),
        "host compatibility-matrix binding differs",
    )
    instructions_binding = _exact_fields(
        profile["server_instructions"],
        {"path", "file_sha256", "wire_sha256"},
        label="host server-instructions binding",
    )
    require(
        instructions_binding["path"]
        == "godot-codex-mcp/product/server-instructions.v1.txt"
        and {
            field: instructions_binding[field]
            for field in ("file_sha256", "wire_sha256")
        }
        == canonical_server_instructions(),
        "host server-instructions binding differs",
    )
    matrix = strict_json_load(COMPATIBILITY_MATRIX_PATH)
    matrix_surfaces = {
        item["surface"]: item
        for item in cast(list[dict[str, Any]], matrix.get("surfaces"))
        if isinstance(item, dict)
    }
    surfaces = profile["surfaces"]
    require(
        isinstance(surfaces, list) and len(surfaces) == 3,
        "host coordinate surface count differs",
    )
    observed: dict[str, dict[str, Any]] = {}
    for item in surfaces:
        record = _exact_fields(
            item,
            HOST_PROFILE_SURFACE_FIELDS,
            label="host coordinate",
        )
        surface = record["surface"]
        require(
            isinstance(surface, str)
            and surface in EXACT_HOST_COORDINATES
            and surface not in observed,
            "host coordinate surface differs or is duplicated",
        )
        expected = EXACT_HOST_COORDINATES[surface]
        require(
            all(record[field] == expected[field] for field in expected),
            f"{surface} exact host coordinate differs",
        )
        for field in (
            "host_artifact_sha256",
            "client_artifact_sha256",
        ):
            _digest(record[field], label=f"{surface} {field}")
        if record["host_metadata_sha256"] is not None:
            _digest(
                record["host_metadata_sha256"],
                label=f"{surface} host metadata",
            )
        if record["host_commit"] is not None:
            _commit(record["host_commit"], label=f"{surface} host commit")
        if record["ide_shell_artifact_sha256"] is not None:
            _digest(
                record["ide_shell_artifact_sha256"],
                label=f"{surface} IDE shell",
            )
        for field in (
            "host_code_signature",
            "client_code_signature",
            "ide_shell_code_signature",
        ):
            signature = record[field]
            if signature is None:
                continue
            signature_record = _exact_fields(
                signature,
                {"mode", "identifier", "team_id", "cdhash"},
                label=f"{surface} {field}",
            )
            require(
                signature_record["mode"] in {"strict", "deep_strict"}
                and isinstance(signature_record["cdhash"], str)
                and re.fullmatch(
                    r"[0-9a-f]{40}(?:[0-9a-f]{24})?",
                    signature_record["cdhash"],
                )
                is not None,
                f"{surface} code-signature coordinate differs",
            )
            for token_field in ("identifier", "team_id"):
                _semantic_token(
                    signature_record[token_field],
                    label=f"{surface} {field} {token_field}",
                    maximum=128,
                )
        require(
            record["qualification"]
            in {"candidate", "supported", "compatible_reduced"},
            f"{surface} qualification differs",
        )
        matrix_record = matrix_surfaces.get(surface)
        require(
            isinstance(matrix_record, dict)
            and matrix_record.get("host_version") == record["host_version"]
            and matrix_record.get("ide_host_version")
            == record["ide_host_version"]
            and matrix_record.get("qualification")
            == record["qualification"],
            f"{surface} host/matrix qualification differs",
        )
        if qualifying:
            require(
                record["qualification"] == "supported",
                f"{surface} host coordinate is not promoted to supported",
            )
        observed[surface] = record
    require(set(observed) == {"app", "cli", "ide"}, "host surfaces differ")
    safe_evidence_scan(profile)
    return dict(profile)


def render_host_coordinate_authority(profile: Mapping[str, Any]) -> str:
    rows = [
        "<!-- BEGIN S11 HOST COORDINATE AUTHORITY -->",
        "| Surface | Host identifier | Host version/build | Host artifact SHA-256 | Client version/SHA-256 | IDE shell | Qualification |",
        "|---|---|---|---|---|---|---|",
    ]
    for item in cast(list[Mapping[str, Any]], profile["surfaces"]):
        shell = (
            "—"
            if item["ide_shell_identifier"] is None
            else (
                f"{item['ide_shell_identifier']} {item['ide_host_version']} "
                f"`{item['ide_shell_artifact_sha256']}`"
            )
        )
        rows.append(
            f"| {item['surface']} | `{item['host_identifier']}` | "
            f"`{item['host_version']}` / `{item['host_build']}` | "
            f"`{item['host_artifact_sha256']}` | "
            f"`{item['client_version']}` / `{item['client_artifact_sha256']}` | "
            f"{shell} | {item['qualification']} |"
        )
    rows.append("<!-- END S11 HOST COORDINATE AUTHORITY -->")
    return "\n".join(rows)


def validate_rendered_host_authority(profile: Mapping[str, Any]) -> None:
    try:
        document = HOST_CONTRACT_PATH.read_text(encoding="utf-8")
    except OSError as error:
        raise AcceptanceError("host contract Markdown is unavailable") from error
    expected = render_host_coordinate_authority(profile)
    require(
        document.count("<!-- BEGIN S11 HOST COORDINATE AUTHORITY -->") == 1
        and document.count("<!-- END S11 HOST COORDINATE AUTHORITY -->") == 1
        and expected in document,
        "rendered host coordinate authority differs from JSON",
    )


def validate_sprint_plan_assertion_authority() -> None:
    try:
        document = SPRINT11_PLAN_PATH.read_text(encoding="utf-8")
    except OSError as error:
        raise AcceptanceError("Sprint 11 plan is unavailable") from error
    marker = "The exact set is "
    require(
        "Each of the eighteen assertion IDs" in document
        and document.count(marker) == 1,
        "Sprint 11 plan assertion count authority differs",
    )
    paragraph = document.split(marker, 1)[1].split("\n\n", 1)[0]
    assertion_ids = re.findall(r"`([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+)`", paragraph)
    require(
        len(assertion_ids) == len(REQUIRED_ASSERTIONS)
        and set(assertion_ids) == REQUIRED_ASSERTIONS,
        "Sprint 11 plan assertion set differs",
    )


def validate_documented_host_acquisition() -> str:
    for path, label in (
        (HOST_CONTRACT_PATH, "host contract"),
        (SPRINT11_PLAN_PATH, "Sprint 11 plan"),
    ):
        try:
            document = path.read_text(encoding="utf-8")
        except OSError as error:
            raise AcceptanceError(f"{label} is unavailable") from error
        require(
            document.count(DOCUMENTED_HOST_ACQUISITION_COMMAND) == 1
            and "receipt.json" in document
            and "measurement.json" in document
            and "measurement digest" in document,
            f"{label} host acquisition authority differs",
        )
    return sha256_bytes(DOCUMENTED_HOST_ACQUISITION_COMMAND.encode("utf-8"))


def validate_registry_projection(value: Any) -> dict[str, Any]:
    registry = _exact_fields(
        value,
        {
            "tools",
            "fixed_resources",
            "resource_templates",
            "instructions_sha256",
        },
        label="MCP registry projection",
    )
    tools = registry["tools"]
    fixed = registry["fixed_resources"]
    templates = registry["resource_templates"]
    profile = canonical_registry_profile()
    require(
        isinstance(tools, list)
        and tools == profile["tools"],
        "MCP tool registry differs from exact 41-tool contract",
    )
    require(
        isinstance(fixed, list)
        and fixed == profile["fixed_resources"],
        "fixed MCP resources differ from exact four-resource contract",
    )
    require(
        isinstance(templates, list)
        and templates == profile["resource_templates"],
        "MCP resource-template contract differs",
    )
    require(
        _digest(
            registry["instructions_sha256"],
            label="server instructions",
        )
        == canonical_server_instructions()["wire_sha256"],
        "MCP server instructions are not canonical",
    )
    return dict(registry)


def validate_registry_document(value: Any) -> dict[str, Any]:
    return validate_registry_profile_document(value)


def _assertion_map(trace: Mapping[str, Any]) -> dict[str, Mapping[str, Any]]:
    assertions = trace["assertions"]
    require(isinstance(assertions, list), "trace assertions are missing")
    result: dict[str, Mapping[str, Any]] = {}
    for assertion in assertions:
        record = _exact_fields(
            assertion,
            {"assertion_id", "projection"},
            label="trace assertion",
        )
        assertion_id = record["assertion_id"]
        require(
            isinstance(assertion_id, str)
            and assertion_id in REQUIRED_ASSERTIONS
            and assertion_id not in result,
            "trace assertion ID differs or is duplicated",
        )
        projection = record["projection"]
        require(
            isinstance(projection, dict),
            "trace assertion projection is not an object",
        )
        result[assertion_id] = cast(Mapping[str, Any], projection)
    require(set(result) == REQUIRED_ASSERTIONS, "trace assertion coverage differs")
    return result


def _bounded_integer(
    value: Any,
    *,
    label: str,
    minimum: int = 0,
    maximum: int = 9_007_199_254_740_991,
) -> int:
    require(
        isinstance(value, int)
        and not isinstance(value, bool)
        and minimum <= value <= maximum,
        f"{label}: integer bound differs",
    )
    return value


def _semantic_token(value: Any, *, label: str, maximum: int = 256) -> str:
    token = _bounded_string(value, label=label, maximum=maximum)
    require(
        "\0" not in token
        and not any(character.isspace() for character in token),
        f"{label}: token differs",
    )
    return token


def _semantic_string(value: Any, *, label: str, maximum: int = 512) -> str:
    text = _bounded_string(value, label=label, maximum=maximum)
    require(
        "\0" not in text
        and "\r" not in text
        and "\n" not in text,
        f"{label}: semantic string differs",
    )
    return text


def _project_identity(value: Any, *, label: str) -> str:
    token = _semantic_token(value, label=label)
    require(token.startswith("project:"), f"{label}: project identity differs")
    return token


def _scene_identity(value: Any, *, label: str) -> str:
    token = _semantic_string(value, label=label)
    require(
        token.startswith("scene:res://"),
        f"{label}: scene identity differs",
    )
    return token


def _transaction_identity(value: Any, *, label: str) -> str:
    token = _semantic_token(value, label=label)
    require(
        token.startswith("transaction:"),
        f"{label}: transaction identity differs",
    )
    return token


def _validate_approval_assertions(
    assertions: Mapping[str, Mapping[str, Any]],
) -> None:
    transactions: set[str] = set()
    for scenario in ("accept", "cancel", "decline", "timeout"):
        record = _exact_fields(
            assertions[f"approval.{scenario}"],
            {"transaction_id", "action", "state"},
            label=f"approval.{scenario}",
        )
        transaction = _transaction_identity(
            record["transaction_id"],
            label=f"approval.{scenario} transaction",
        )
        require(transaction not in transactions, "approval transactions overlap")
        transactions.add(transaction)
        require(record["action"] == scenario, "approval action differs")
        require(
            record["state"]
            == ("committed" if scenario == "accept" else "not_applied"),
            "approval semantic state differs",
        )


def _validate_connection_assertion(
    projection: Mapping[str, Any],
) -> None:
    record = _exact_fields(
        projection,
        {"project_id", "state", "remediation_id"},
        label="connection.status",
    )
    _project_identity(record["project_id"], label="connection project")
    require(
        record["state"] == "ready"
        and record["remediation_id"] == "none",
        "connection status is not the required ready projection",
    )


def _validate_multi_project_assertion(
    projection: Mapping[str, Any],
) -> None:
    record = _exact_fields(
        projection,
        {"project_id", "foreign_project_id", "error"},
        label="multi_project.reject",
    )
    project = _project_identity(record["project_id"], label="bound project")
    foreign = _project_identity(
        record["foreign_project_id"],
        label="foreign project",
    )
    require(project != foreign, "multi-project rejection uses one project")
    error = _exact_fields(
        record["error"],
        {"code", "retryable", "remediation_id"},
        label="multi-project error",
    )
    require(
        error["code"] == "project_binding_mismatch"
        and error["retryable"] is False
        and error["remediation_id"] == "select_bound_project",
        "multi-project rejection semantics differ",
    )


def _validate_offline_assertion(projection: Mapping[str, Any]) -> None:
    record = _exact_fields(
        projection,
        {"project_id", "scene_id", "freshness", "cursor"},
        label="offline.saved_query",
    )
    _project_identity(record["project_id"], label="offline project")
    _scene_identity(record["scene_id"], label="offline scene")
    require(
        record["freshness"] == "offline_cached",
        "offline saved query freshness differs",
    )
    _semantic_token(record["cursor"], label="offline cursor")


def _validate_host_assertions(
    assertions: Mapping[str, Mapping[str, Any]],
) -> None:
    approval = _exact_fields(
        assertions["host.approval_layers"],
        {
            "sandbox_approval",
            "mcp_approval",
            "independent",
        },
        label="host.approval_layers",
    )
    require(
        approval
        == {
            "sandbox_approval": "observed",
            "mcp_approval": "action_only_form",
            "independent": True,
        },
        "host approval-layer projection differs",
    )
    reload_record = _exact_fields(
        assertions["host.config_reload"],
        {"config_reloaded", "root_rebound", "restart_required"},
        label="host.config_reload",
    )
    require(
        reload_record
        == {
            "config_reloaded": True,
            "root_rebound": True,
            "restart_required": False,
        },
        "host config/root reload projection differs",
    )
    launcher = _exact_fields(
        assertions["host.launcher"],
        {
            "path",
            "sha256",
            "path_lookup_used",
            "private_absolute_path_recorded",
        },
        label="host.launcher",
    )
    require(
        launcher["path"] == "bin/godot-codex-mcp"
        and launcher["path_lookup_used"] is False
        and launcher["private_absolute_path_recorded"] is False,
        "host launcher projection differs",
    )
    _digest(launcher["sha256"], label="host launcher")
    offline = _exact_fields(
        assertions["host.offline_status"],
        {
            "state",
            "freshness",
            "live_state_claimed",
            "remediation_id",
        },
        label="host.offline_status",
    )
    require(
        offline
        == {
            "state": "offline",
            "freshness": "offline_cached",
            "live_state_claimed": False,
            "remediation_id": "start_matching_editor",
        },
        "host offline-status projection differs",
    )
    unsupported = _exact_fields(
        assertions["host.unsupported_form"],
        {
            "error_code",
            "mutation_observed",
            "read_only_available",
        },
        label="host.unsupported_form",
    )
    require(
        unsupported
        == {
            "error_code": "approval_host_unsupported",
            "mutation_observed": False,
            "read_only_available": True,
        },
        "unsupported-form projection differs",
    )


def _validate_runtime_assertion(projection: Mapping[str, Any]) -> None:
    record = _exact_fields(
        projection,
        {
            "runtime_session_before",
            "runtime_session_after",
            "runtime_node_id",
            "stack_frame_id",
            "error_code",
            "source",
        },
        label="runtime.error_stack",
    )
    before = _semantic_token(
        record["runtime_session_before"],
        label="runtime session before",
    )
    after = _semantic_token(
        record["runtime_session_after"],
        label="runtime session after",
    )
    require(
        before.startswith("runtime-session:")
        and after.startswith("runtime-session:")
        and before != after,
        "runtime restart did not create a distinct session",
    )
    require(
        _semantic_token(
            record["runtime_node_id"],
            label="runtime node",
        ).startswith("runtime-node:")
        and _semantic_token(
            record["stack_frame_id"],
            label="stack frame",
        ).startswith("stack-frame:"),
        "runtime evidence identity differs",
    )
    _semantic_token(record["error_code"], label="runtime error code")
    source = _exact_fields(
        record["source"],
        {"path", "line"},
        label="runtime stack source",
    )
    source_path = _semantic_string(
        source["path"],
        label="runtime source path",
        maximum=512,
    )
    require(
        source_path.startswith("res://")
        and not source_path.startswith("res://.godot/"),
        "runtime stack source is not a project-relative source path",
    )
    _bounded_integer(
        source["line"],
        label="runtime source line",
        minimum=1,
        maximum=10_000_000,
    )


def _validate_saved_assertion(projection: Mapping[str, Any]) -> None:
    record = _exact_fields(
        projection,
        {
            "project_id",
            "scene_id",
            "scene_node_id",
            "evidence_id",
            "fact_code",
            "fact_value_redacted",
            "fact_value_digest",
            "confidence",
            "freshness",
        },
        label="saved.current_scene",
    )
    _project_identity(record["project_id"], label="saved project")
    _scene_identity(record["scene_id"], label="saved scene")
    require(
        _semantic_token(
            record["scene_node_id"],
            label="saved scene node",
        ).startswith("scene-node:")
        and _semantic_token(
            record["evidence_id"],
            label="saved evidence",
        ).startswith("evidence:")
        and record["confidence"] == "authoritative"
        and record["freshness"] == "current",
        "saved-scene evidence projection differs",
    )
    require(
        SEMANTIC_ID_RE.fullmatch(
            _semantic_token(record["fact_code"], label="saved fact code")
        )
        is not None
        and record["fact_value_redacted"] is True,
        "saved fact projection is not closed/redacted",
    )
    _digest(record["fact_value_digest"], label="saved fact value")


def _validate_transaction_result(
    projection: Mapping[str, Any],
    *,
    assertion_id: str,
    expected_state: str,
) -> tuple[str, str, tuple[int, int, int]]:
    record = _exact_fields(
        projection,
        {
            "editor_session_id",
            "transaction_id",
            "event_seq",
            "scene_revision",
            "operation_seq",
            "state",
        },
        label=assertion_id,
    )
    editor = _semantic_token(
        record["editor_session_id"],
        label=f"{assertion_id} editor session",
    )
    require(
        editor.startswith("editor-session:"),
        f"{assertion_id}: editor session differs",
    )
    transaction = _transaction_identity(
        record["transaction_id"],
        label=f"{assertion_id} transaction",
    )
    revisions = tuple(
        _bounded_integer(record[field], label=f"{assertion_id} {field}")
        for field in ("event_seq", "scene_revision", "operation_seq")
    )
    require(record["state"] == expected_state, f"{assertion_id}: state differs")
    return editor, transaction, cast(tuple[int, int, int], revisions)


def _validate_preview_operation(value: Any) -> None:
    require(isinstance(value, dict), "preview operation is not an object")
    kind = _semantic_token(value.get("kind"), label="preview operation kind")
    require(
        kind in PREVIEW_OPERATION_FIELDS,
        "preview operation kind is unsupported",
    )
    operation = _exact_fields(
        value,
        PREVIEW_OPERATION_FIELDS[kind],
        label=f"preview {kind} operation",
    )
    for field in (
        "parent_scene_node_id",
        "scene_node_id",
        "new_parent_scene_node_id",
        "emitter_scene_node_id",
        "receiver_scene_node_id",
    ):
        if field in operation:
            require(
                _semantic_token(
                    operation[field],
                    label=f"preview {field}",
                ).startswith("scene-node:"),
                f"preview {field} is not a semantic scene-node identity",
            )
    for field in (
        "property_name",
        "value_type",
        "signal_name",
        "method_name",
    ):
        if field in operation:
            _semantic_token(operation[field], label=f"preview {field}")
    if "godot_type" in operation:
        godot_type = _bounded_string(
            operation["godot_type"],
            label="preview Godot type",
            maximum=128,
        )
        require(
            re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]{0,127}", godot_type)
            is not None,
            "preview Godot type differs",
        )
    if "subtree_node_count" in operation:
        _bounded_integer(
            operation["subtree_node_count"],
            label="preview subtree node count",
            minimum=1,
            maximum=10_000,
        )
    for marker in (
        "name_redacted",
        "value_redacted",
        "script_redacted",
        "binds_redacted",
    ):
        if marker in operation:
            require(
                operation[marker] is True,
                f"preview {marker} differs",
            )
    for field in (
        "name_digest",
        "value_digest",
        "script_digest",
        "binds_digest",
    ):
        if field in operation:
            _digest(operation[field], label=f"preview {field}")


def _validate_preview(assertions: Mapping[str, Mapping[str, Any]]) -> None:
    projection = _exact_fields(
        assertions["transaction.preview"],
        {
            "transaction_id",
            "preview",
            "preview_digest",
            "risk",
            "scope",
        },
        label="transaction.preview",
    )
    _transaction_identity(
        projection["transaction_id"],
        label="transaction preview",
    )
    require(
        isinstance(projection["preview"], dict),
        "transaction preview binding is incomplete",
    )
    digest = _digest(
        projection["preview_digest"],
        label="transaction preview",
    )
    require(
        digest == sha256_bytes(canonical_json(projection["preview"])),
        "transaction preview digest is not locally bound",
    )
    require(
        projection["risk"] in {"low", "medium", "high"}
        and projection["scope"] == "change_set.atomic",
        "transaction preview risk/scope differs",
    )
    preview = _exact_fields(
        projection["preview"],
        {
            "editor_session_id",
            "coordinates",
            "operations",
            "risk",
            "scope",
        },
        label="canonical preview",
    )
    editor = _semantic_token(
        preview["editor_session_id"],
        label="preview editor session",
    )
    require(editor.startswith("editor-session:"), "preview editor session differs")
    coordinates = _exact_fields(
        preview["coordinates"],
        {"event_seq", "scene_revision", "operation_seq"},
        label="preview coordinates",
    )
    for field in coordinates:
        _bounded_integer(coordinates[field], label=f"preview {field}")
    operations = preview["operations"]
    require(
        isinstance(operations, list) and 1 <= len(operations) <= 64,
        "preview operation count differs",
    )
    for operation in operations:
        _validate_preview_operation(operation)
    require(
        preview["risk"] == projection["risk"]
        and preview["scope"] == projection["scope"],
        "preview envelope risk/scope differs",
    )


def _validate_validation_assertion(projection: Mapping[str, Any]) -> None:
    record = _exact_fields(
        projection,
        {
            "transaction_id",
            "validation_report_id",
            "outcome",
            "diagnostics",
        },
        label="validation.result",
    )
    _transaction_identity(
        record["transaction_id"],
        label="validation transaction",
    )
    require(
        _semantic_token(
            record["validation_report_id"],
            label="validation report",
        ).startswith("validation-report:")
        and record["outcome"] == "passed",
        "validation result identity/outcome differs",
    )
    diagnostics = record["diagnostics"]
    require(
        isinstance(diagnostics, list) and len(diagnostics) <= 256,
        "validation diagnostics bound differs",
    )


def _validate_semantic_assertions(
    assertions: Mapping[str, Mapping[str, Any]],
) -> None:
    _validate_approval_assertions(assertions)
    _validate_connection_assertion(assertions["connection.status"])
    _validate_host_assertions(assertions)
    _validate_multi_project_assertion(assertions["multi_project.reject"])
    _validate_offline_assertion(assertions["offline.saved_query"])
    _validate_runtime_assertion(assertions["runtime.error_stack"])
    _validate_saved_assertion(assertions["saved.current_scene"])
    _validate_preview(assertions)
    apply_editor, apply_transaction, apply_revisions = _validate_transaction_result(
        assertions["transaction.apply"],
        assertion_id="transaction.apply",
        expected_state="committed",
    )
    undo_editor, undo_transaction, undo_revisions = _validate_transaction_result(
        assertions["transaction.undo"],
        assertion_id="transaction.undo",
        expected_state="undone",
    )
    _validate_validation_assertion(assertions["validation.result"])
    preview = assertions["transaction.preview"]
    accepted = assertions["approval.accept"]
    validation = assertions["validation.result"]
    require(
        apply_editor == undo_editor
        and apply_transaction
        == undo_transaction
        == preview["transaction_id"]
        == accepted["transaction_id"]
        == validation["transaction_id"]
        and all(
            undo_revision > apply_revision
            for undo_revision, apply_revision in zip(
                undo_revisions,
                apply_revisions,
            )
        ),
        "transaction preview/apply/validation/Undo relation differs",
    )


def _validate_form_outcomes(value: Any) -> None:
    require(
        isinstance(value, list) and len(value) == 4,
        "form outcome record count differs",
    )
    outcomes: dict[str, Mapping[str, Any]] = {}
    for item in value:
        record = _exact_fields(
            item,
            {
                "scenario",
                "action",
                "semantic_state",
                "mutation_observed",
                "content_recorded",
            },
            label="form outcome",
        )
        scenario = record["scenario"]
        require(
            isinstance(scenario, str)
            and scenario in {"accept", "decline", "cancel", "timeout"}
            and scenario not in outcomes,
            "form scenario differs or is duplicated",
        )
        require(
            record["action"] == scenario
            and record["content_recorded"] is False,
            "form action or redaction differs",
        )
        if scenario == "accept":
            require(
                record["semantic_state"] == "committed"
                and record["mutation_observed"] is True,
                "accepted form did not bind the committed mutation",
            )
        else:
            require(
                record["semantic_state"] == "not_applied"
                and record["mutation_observed"] is False,
                "negative form outcome observed a mutation",
            )
        outcomes[scenario] = record
    require(
        set(outcomes) == {"accept", "decline", "cancel", "timeout"},
        "form outcome matrix differs",
    )


def _validate_revision_timeline(value: Any) -> None:
    require(
        isinstance(value, list) and 5 <= len(value) <= 32,
        "revision timeline bound differs",
    )
    required_order = (
        "initial",
        "prepared",
        "applied",
        "restarted",
        "stale_guard_rejected",
    )
    rows: dict[str, Mapping[str, Any]] = {}
    positions: dict[str, int] = {}
    per_session: dict[str, list[tuple[int, int, int]]] = {}
    for position, item in enumerate(value):
        row = _exact_fields(
            item,
            {
                "step",
                "editor_session_id",
                "event_seq",
                "scene_revision",
                "operation_seq",
            },
            label="revision row",
        )
        step = _bounded_string(row["step"], label="revision step", maximum=64)
        session = _bounded_string(
            row["editor_session_id"],
            label="editor session",
            maximum=128,
        )
        numbers: list[int] = []
        for field in ("event_seq", "scene_revision", "operation_seq"):
            number = row[field]
            require(
                isinstance(number, int)
                and not isinstance(number, bool)
                and 0 <= number <= 9_007_199_254_740_991,
                "revision value differs",
            )
            numbers.append(number)
        require(step not in rows, "revision step is duplicated")
        rows[step] = row
        positions[step] = position
        history = per_session.setdefault(session, [])
        if history:
            require(
                all(current >= previous for current, previous in zip(numbers, history[-1])),
                "revision timeline is not monotonic within a session",
            )
        history.append(cast(tuple[int, int, int], tuple(numbers)))
    require(
        set(required_order).issubset(rows)
        and [positions[step] for step in required_order]
        == sorted(positions[step] for step in required_order),
        "required revision transitions are missing or reordered",
    )
    initial = rows["initial"]
    prepared = rows["prepared"]
    applied = rows["applied"]
    restarted = rows["restarted"]
    stale = rows["stale_guard_rejected"]
    revision_fields = ("event_seq", "scene_revision", "operation_seq")
    require(
        initial["editor_session_id"] == prepared["editor_session_id"]
        == applied["editor_session_id"]
        and all(initial[field] == prepared[field] for field in revision_fields),
        "prepare mutated revisions or changed editor session",
    )
    require(
        all(applied[field] > prepared[field] for field in revision_fields),
        "apply did not advance the revision guards",
    )
    require(
        restarted["editor_session_id"] != applied["editor_session_id"]
        and stale["editor_session_id"] == restarted["editor_session_id"]
        and all(stale[field] == restarted[field] for field in revision_fields),
        "restart/stale-guard relation differs",
    )


RECORDER_EVENT_CHAIN_DOMAIN = b"godot-codex/s11-recorder-event-chain/v1\0"
RECORDER_EVENT_FIELDS = frozenset(
    {
        "content_recorded",
        "direction",
        "error_code",
        "form_action",
        "form_mode",
        "frame_bytes",
        "instructions_sha256",
        "is_error",
        "kind",
        "method",
        "protocol_version",
        "resource",
        "resource_templates",
        "resources",
        "semantic_state",
        "semantic_status",
        "seq",
        "tool",
        "tool_error",
        "tools",
    }
)
RECORDER_BINDING_FIELDS = frozenset(
    {
        "package_source_commit",
        "package_manifest_sha256",
        "compatibility_matrix_sha256",
        "host_coordinate_profile_sha256",
        "project_fixture_sha256",
        "registry_sha256",
        "prompt_pack_sha256",
        "host_artifact_sha256",
        "client_artifact_sha256",
        "host_provenance_sha256",
        "mcp_binary_sha256",
        "godot_artifact_sha256",
    }
)


def recorder_event_chain_sha256(events: Sequence[Mapping[str, Any]]) -> str:
    state = hashlib.sha256(RECORDER_EVENT_CHAIN_DOMAIN).digest()
    for event in events:
        state = hashlib.sha256(
            state + b"\0" + canonical_json(event)
        ).digest()
    return "sha256:" + state.hex()


def recorder_journal_sha256(value: Mapping[str, Any]) -> str:
    """Digest the exact canonical on-disk journal representation."""

    return sha256_bytes(canonical_json(value) + b"\n")


def validate_recorder_journal(
    value: Any,
    *,
    qualifying: bool,
) -> dict[str, Any]:
    journal = _exact_fields(
        value,
        RECORDER_JOURNAL_FIELDS,
        label="recorder journal",
    )
    require(
        len(canonical_json(journal)) + 1 <= MAX_RECORDER_JOURNAL_BYTES,
        "recorder journal exceeds byte bound",
    )
    require(
        journal["schema_version"] == "s11-recorder-journal/1.0",
        "recorder journal schema differs",
    )
    if qualifying:
        require(
            journal["capture_kind"] == "surface_transport_capture"
            and journal["status"] == "complete",
            "qualifying recorder transport capture is incomplete or synthetic",
        )
    else:
        require(
            journal["capture_kind"]
            in {"surface_transport_capture", "synthetic_contract_fixture"}
            and journal["status"] in {"complete", "fixture_valid"},
            "recorder journal capture/status differs",
        )
    require(journal["surface"] in {"app", "cli", "ide"}, "journal surface differs")
    host = _exact_fields(
        journal["host"],
        {
            "name",
            "identifier",
            "artifact_kind",
            "version",
            "build",
            "commit",
            "client_version",
            "host_metadata_sha256",
            "host_code_signature",
            "client_code_signature",
            "ide_host_version",
            "ide_shell_identifier",
            "ide_shell_artifact_sha256",
            "ide_shell_team_id",
            "ide_shell_code_signature",
            "architecture",
        },
        label="recorder host",
    )
    for field in (
        "name",
        "identifier",
        "artifact_kind",
        "version",
        "build",
        "client_version",
    ):
        _bounded_string(host[field], label=f"recorder host {field}", maximum=128)
    for field in (
        "commit",
        "ide_host_version",
        "ide_shell_identifier",
        "ide_shell_team_id",
    ):
        if host[field] is not None:
            _bounded_string(host[field], label=f"recorder host {field}", maximum=128)
    if host["commit"] is not None:
        _commit(host["commit"], label="recorder host")
    if host["ide_shell_artifact_sha256"] is not None:
        _digest(host["ide_shell_artifact_sha256"], label="recorder IDE shell")
    if host["host_metadata_sha256"] is not None:
        _digest(host["host_metadata_sha256"], label="recorder host metadata")
    for field in (
        "host_code_signature",
        "client_code_signature",
        "ide_shell_code_signature",
    ):
        _validate_observed_code_signature(
            host[field],
            label=f"recorder {field}",
        )
    require(host["architecture"] == "arm64", "recorder architecture differs")
    bindings = _exact_fields(
        journal["bindings"],
        RECORDER_BINDING_FIELDS,
        label="recorder bindings",
    )
    _commit(bindings["package_source_commit"], label="recorder package source")
    for field in set(bindings) - {"package_source_commit"}:
        _digest(bindings[field], label=f"recorder {field}")
    controls = _exact_fields(
        journal["host_controls"],
        {
            "sandbox_approval_observed",
            "config_reload_observed",
            "root_binding_observed",
            "package_launcher_observed",
            "package_launcher_path",
            "package_launcher_sha256",
        },
        label="recorder host controls",
    )
    require(
        controls["sandbox_approval_observed"] is True
        and controls["config_reload_observed"] is True
        and controls["root_binding_observed"] is True
        and controls["package_launcher_observed"] is True
        and controls["package_launcher_path"] == "bin/godot-codex-mcp"
        and controls["package_launcher_sha256"]
        == bindings["mcp_binary_sha256"],
        "recorder host controls differ",
    )
    protocol = _semantic_token(
        journal["protocol_version"],
        label="recorder protocol version",
        maximum=64,
    )
    if qualifying:
        require(
            protocol == "2025-11-25",
            "recorder did not observe the qualified MCP protocol",
        )
    events = journal["events"]
    require(
        isinstance(events, list) and 1 <= len(events) <= 4_096,
        "recorder event bound differs",
    )
    projected_events: list[Mapping[str, Any]] = []
    response_methods: set[str] = set()
    tool_response_seen = False
    instructions_seen = False
    form_seen = False
    form_actions: set[str] = set()
    offline_seen = False
    unsupported_seen = False
    registry_observed: set[str] = set()
    canonical_registry = canonical_registry_profile()
    for index, item in enumerate(events):
        require(isinstance(item, dict), "recorder event is not an object")
        event = cast(dict[str, Any], item)
        require(
            set(event).issubset(RECORDER_EVENT_FIELDS)
            and {
                "seq",
                "direction",
                "kind",
                "frame_bytes",
            }.issubset(event),
            "recorder event fields differ",
        )
        require(event["seq"] == index, "recorder event sequence has a gap")
        require(
            event["direction"] in {"client_to_server", "server_to_client"}
            and event["kind"] in {"request", "notification", "response"},
            "recorder event direction/kind is non-qualifying",
        )
        _bounded_integer(
            event["frame_bytes"],
            label="recorder frame bytes",
            minimum=1,
            maximum=1_048_576,
        )
        method = event.get("method")
        if method is not None:
            _semantic_token(method, label="recorder method")
            if event["kind"] == "response":
                response_methods.add(cast(str, method))
                if method == "tools/call":
                    tool_response_seen = True
        for field in (
            "tool",
            "resource",
            "form_mode",
            "form_action",
            "error_code",
            "semantic_state",
            "semantic_status",
            "protocol_version",
        ):
            if field in event:
                _semantic_token(event[field], label=f"recorder event {field}")
        for field in ("is_error", "tool_error", "content_recorded"):
            if field in event:
                require(
                    isinstance(event[field], bool),
                    f"recorder event {field} differs",
                )
        if "instructions_sha256" in event:
            require(
                _digest(
                    event["instructions_sha256"],
                    label="recorder instructions",
                )
                == canonical_server_instructions()["wire_sha256"],
                "recorder observed non-canonical server instructions",
            )
            instructions_seen = True
        for field, expected in (
            ("tools", 41),
            ("resources", 4),
            ("resource_templates", 1),
        ):
            if field in event:
                values = event[field]
                require(
                    isinstance(values, list)
                    and values == sorted(set(values))
                    and len(values) == expected
                    and all(isinstance(member, str) for member in values),
                    f"recorder {field} enumeration differs",
                )
                expected_values = {
                    "tools": canonical_registry["tools"],
                    "resources": canonical_registry["fixed_resources"],
                    "resource_templates": canonical_registry[
                        "resource_templates"
                    ],
                }[field]
                require(
                    values == expected_values,
                    f"recorder {field} differs from canonical registry",
                )
                registry_observed.add(field)
        if "form_mode" in event:
            require(event["form_mode"] == "form", "recorder form mode differs")
            form_seen = True
        if "form_action" in event:
            action = event["form_action"]
            require(
                action in {"accept", "decline", "cancel", "timeout"},
                "recorder form action differs",
            )
            if action == "timeout":
                require(
                    event.get("method") == "tools/call"
                    and event.get("error_code") == "approval_timeout"
                    and (
                        event.get("is_error") is True
                        or event.get("tool_error") is True
                    ),
                    "recorder timeout is not correlated to its parent apply",
                )
            else:
                require(
                    event.get("method") == "elicitation/create",
                    "recorder form response method differs",
                )
            form_actions.add(cast(str, action))
        if "content_recorded" in event:
            require(
                event["content_recorded"] is False,
                "recorder retained elicitation content",
            )
        if event.get("semantic_status") == "offline_cached":
            offline_seen = True
        if event.get("error_code") == "approval_host_unsupported":
            unsupported_seen = True
        projected_events.append(event)
    integrity = _exact_fields(
        journal["integrity"],
        {
            "input_bytes",
            "output_bytes",
            "input_frames",
            "output_frames",
            "event_count",
            "event_chain_sha256",
            "child_exit_code",
            "passthrough_mode",
            "recorder_errors",
            "truncated",
        },
        label="recorder integrity",
    )
    for field in (
        "input_bytes",
        "output_bytes",
        "input_frames",
        "output_frames",
        "event_count",
    ):
        _bounded_integer(
            integrity[field],
            label=f"recorder {field}",
            maximum=1 << 53,
        )
    require(
        integrity["event_count"] == len(events)
        and integrity["event_chain_sha256"]
        == recorder_event_chain_sha256(projected_events)
        and integrity["child_exit_code"] == 0
        and integrity["passthrough_mode"] is True
        and integrity["recorder_errors"] == []
        and integrity["truncated"] is False,
        "recorder integrity/hash-chain differs",
    )
    redaction = _exact_fields(
        journal["redaction"],
        {
            "request_arguments_absent",
            "tool_content_absent",
            "elicitation_content_absent",
            "source_content_absent",
            "native_ids_absent",
            "absolute_paths_absent",
            "secrets_absent",
        },
        label="recorder redaction",
    )
    require(
        all(flag is True for flag in redaction.values()),
        "recorder redaction differs",
    )
    if qualifying:
        require(
            {
                "initialize",
                "tools/list",
                "resources/list",
                "resources/templates/list",
            }.issubset(response_methods)
            and tool_response_seen
            and instructions_seen
            and form_seen
            and form_actions == {"accept", "decline", "cancel", "timeout"}
            and offline_seen
            and unsupported_seen
            and registry_observed
            == {"tools", "resources", "resource_templates"},
            "recorder journal omits required protocol observations",
        )
    safe_evidence_scan(journal)
    return dict(journal)


def surface_acquisition_directory(surface: str) -> str:
    require(surface in {"app", "cli", "ide"}, "surface authority differs")
    return f"tests/codex/acquisition/sprint11/surfaces/{surface}"


def validate_surface_acquisition_authority(
    value: Any,
    *,
    surface: str,
    authority_path: str,
    authority_sha256: str,
    trace_path: str,
    trace_sha256: str,
    recorder_journal_path: str,
    recorder_journal_sha256: str,
    trace: Mapping[str, Any],
) -> dict[str, Any]:
    authority = _exact_fields(
        value,
        {
            "schema_version",
            "authority_kind",
            "trust_boundary",
            "operator_id",
            "surface",
            "artifact_directory",
            "bindings",
            "observations",
            "attestation_projection_sha256",
        },
        label="surface acquisition authority",
    )
    _digest(authority_sha256, label="surface authority artifact")
    require(
        authority["schema_version"]
        == "s11-surface-acquisition-authority/1.0"
        and authority["authority_kind"] == "external_operator_attestation"
        and authority["trust_boundary"] == SURFACE_AUTHORITY_TRUST_BOUNDARY
        and authority["surface"] == surface,
        "surface acquisition authority identity differs",
    )
    require(
        trace.get("surface") == surface,
        "surface authority is bound to a different trace surface",
    )
    require(
        isinstance(authority["operator_id"], str)
        and re.fullmatch(
            r"operator:sha256:[0-9a-f]{64}",
            authority["operator_id"],
        )
        is not None,
        "surface authority operator differs",
    )
    artifact_directory = surface_acquisition_directory(surface)
    expected_paths = {
        "authority": f"{artifact_directory}/authority.json",
        "trace": f"{artifact_directory}/trace.json",
        "recorder_journal": f"{artifact_directory}/recorder-journal.json",
    }
    require(
        authority["artifact_directory"] == artifact_directory
        and authority_path == expected_paths["authority"]
        and trace_path == expected_paths["trace"]
        and recorder_journal_path == expected_paths["recorder_journal"],
        "surface authority artifact layout differs",
    )
    bindings = _exact_fields(
        authority["bindings"],
        SURFACE_AUTHORITY_BINDING_FIELDS,
        label="surface authority bindings",
    )
    trace_bindings = _exact_fields(
        trace.get("bindings"),
        {
            "package_source_commit",
            "package_manifest_sha256",
            "compatibility_matrix_sha256",
            "host_coordinate_profile_sha256",
            "project_fixture_sha256",
            "registry_sha256",
            "prompt_pack_sha256",
            "host_artifact_sha256",
            "client_artifact_sha256",
            "host_provenance_sha256",
            "mcp_binary_sha256",
            "godot_artifact_sha256",
            "recorder_journal_sha256",
            "recorder_event_chain_sha256",
            "recorder_event_count",
        },
        label="surface authority trace bindings",
    )
    expected_bindings = {
        "package_source_commit": trace_bindings["package_source_commit"],
        "package_manifest_sha256": trace_bindings["package_manifest_sha256"],
        "host_provenance_sha256": trace_bindings["host_provenance_sha256"],
        "host_artifact_sha256": trace_bindings["host_artifact_sha256"],
        "client_artifact_sha256": trace_bindings["client_artifact_sha256"],
        "mcp_binary_sha256": trace_bindings["mcp_binary_sha256"],
        "project_fixture_sha256": trace_bindings["project_fixture_sha256"],
        "prompt_pack_sha256": trace_bindings["prompt_pack_sha256"],
        "trace_path": trace_path,
        "trace_sha256": trace_sha256,
        "recorder_journal_path": recorder_journal_path,
        "recorder_journal_sha256": recorder_journal_sha256,
    }
    require(
        bindings == expected_bindings
        and recorder_journal_sha256
        == trace_bindings["recorder_journal_sha256"],
        "surface authority artifact binding differs",
    )
    _commit(bindings["package_source_commit"], label="surface authority source")
    for field in SURFACE_AUTHORITY_BINDING_FIELDS - {
        "package_source_commit",
        "trace_path",
        "recorder_journal_path",
    }:
        _digest(bindings[field], label=f"surface authority {field}")
    observations = _exact_fields(
        authority["observations"],
        SURFACE_AUTHORITY_OBSERVATION_FIELDS,
        label="surface authority observations",
    )
    require(
        all(observation is True for observation in observations.values()),
        "surface authority observation differs",
    )
    projection = dict(authority)
    projection.pop("attestation_projection_sha256")
    require(
        authority["attestation_projection_sha256"]
        == sha256_bytes(
            SURFACE_AUTHORITY_DOMAIN + canonical_json(projection)
        ),
        "surface authority projection digest differs",
    )
    safe_evidence_scan(authority)
    return dict(authority)


def validate_surface_trace(
    value: Any,
    *,
    qualifying: bool,
    recorder_journal: Mapping[str, Any] | None = None,
    recorder_journal_digest: str | None = None,
    source_bound_authority: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    trace = _exact_fields(value, TRACE_FIELDS, label="surface trace")
    require(
        len(canonical_json(trace)) <= MAX_TRACE_BYTES,
        "surface trace exceeds byte bound",
    )
    require(
        trace["schema_version"] == "s11-surface-trace/1.0",
        "surface trace schema differs",
    )
    if qualifying:
        require(
            trace["capture_kind"] == "surface_transport_capture"
            and trace["status"] == "passed"
            and source_bound_authority is not None,
            "qualifying trace lacks a passing transport capture and external authority",
        )
    else:
        require(
            (trace["capture_kind"], trace["status"])
            in {
                ("surface_transport_capture", "passed"),
                ("synthetic_contract_fixture", "fixture_valid"),
            },
            "surface trace capture/status pairing differs",
        )
    require(trace["surface"] in {"app", "cli", "ide"}, "surface differs")
    host = _exact_fields(
        trace["host"],
        {
            "name",
            "identifier",
            "artifact_kind",
            "version",
            "build",
            "commit",
            "client_version",
            "host_metadata_sha256",
            "host_code_signature",
            "client_code_signature",
            "ide_host_version",
            "ide_shell_identifier",
            "ide_shell_artifact_sha256",
            "ide_shell_team_id",
            "ide_shell_code_signature",
            "architecture",
        },
        label="trace host",
    )
    for field in (
        "name",
        "identifier",
        "artifact_kind",
        "version",
        "build",
        "client_version",
    ):
        _bounded_string(host[field], label=f"host {field}", maximum=128)
    for field in (
        "commit",
        "ide_host_version",
        "ide_shell_identifier",
        "ide_shell_team_id",
    ):
        if host[field] is not None:
            _bounded_string(host[field], label=f"host {field}", maximum=128)
    if host["commit"] is not None:
        _commit(host["commit"], label="trace host")
    if host["ide_shell_artifact_sha256"] is not None:
        _digest(host["ide_shell_artifact_sha256"], label="trace IDE shell")
    if host["host_metadata_sha256"] is not None:
        _digest(host["host_metadata_sha256"], label="trace host metadata")
    for field in (
        "host_code_signature",
        "client_code_signature",
        "ide_shell_code_signature",
    ):
        _validate_observed_code_signature(host[field], label=f"trace {field}")
    require(host["architecture"] == "arm64", "host architecture differs")
    bindings = _exact_fields(
        trace["bindings"],
        {
            "package_source_commit",
            "package_manifest_sha256",
            "compatibility_matrix_sha256",
            "host_coordinate_profile_sha256",
            "project_fixture_sha256",
            "registry_sha256",
            "prompt_pack_sha256",
            "host_artifact_sha256",
            "client_artifact_sha256",
            "host_provenance_sha256",
            "mcp_binary_sha256",
            "godot_artifact_sha256",
            "recorder_journal_sha256",
            "recorder_event_chain_sha256",
            "recorder_event_count",
        },
        label="trace bindings",
    )
    _commit(bindings["package_source_commit"], label="package source")
    for field in set(bindings) - {
        "package_source_commit",
        "recorder_event_count",
    }:
        _digest(bindings[field], label=f"trace {field}")
    _bounded_integer(
        bindings["recorder_event_count"],
        label="trace recorder event count",
        minimum=1,
        maximum=4_096,
    )
    require(
        bindings["godot_artifact_sha256"] == EXACT_GODOT_PREREQUISITE_SHA256,
        "trace uses a non-qualified Godot prerequisite",
    )
    require(
        bindings["compatibility_matrix_sha256"]
        == sha256_file(COMPATIBILITY_MATRIX_PATH)
        and bindings["host_coordinate_profile_sha256"]
        == sha256_file(HOST_PROFILE_PATH),
        "trace compatibility authority binding differs",
    )
    profile = validate_host_coordinate_profile(
        strict_json_load(HOST_PROFILE_PATH),
        qualifying=qualifying,
    )
    host_coordinate = next(
        (
            item
            for item in cast(list[dict[str, Any]], profile["surfaces"])
            if item["surface"] == trace["surface"]
        ),
        None,
    )
    require(isinstance(host_coordinate, dict), "trace host coordinate is absent")
    expected_host = {
            "name": host_coordinate["host_name"],
            "identifier": host_coordinate["host_identifier"],
            "artifact_kind": host_coordinate["host_artifact_kind"],
            "version": host_coordinate["host_version"],
            "build": host_coordinate["host_build"],
            "commit": host_coordinate["host_commit"],
            "client_version": host_coordinate["client_version"],
            "host_metadata_sha256": host_coordinate[
                "host_metadata_sha256"
            ],
            "host_code_signature": (
                None
                if host_coordinate["host_code_signature"] is None
                else {
                    **host_coordinate["host_code_signature"],
                    "verified": True,
                }
            ),
            "client_code_signature": (
                None
                if host_coordinate["client_code_signature"] is None
                else {
                    **host_coordinate["client_code_signature"],
                    "verified": True,
                }
            ),
            "ide_host_version": host_coordinate["ide_host_version"],
            "ide_shell_identifier": host_coordinate["ide_shell_identifier"],
            "ide_shell_artifact_sha256": host_coordinate[
                "ide_shell_artifact_sha256"
            ],
            "ide_shell_team_id": host_coordinate["ide_shell_team_id"],
            "ide_shell_code_signature": (
                None
                if host_coordinate["ide_shell_code_signature"] is None
                else {
                    **host_coordinate["ide_shell_code_signature"],
                    "verified": True,
                }
            ),
            "architecture": "arm64",
    }
    if qualifying or trace["capture_kind"] == "surface_transport_capture":
        require(
            host == expected_host
            and bindings["host_artifact_sha256"]
            == host_coordinate["host_artifact_sha256"]
            and bindings["client_artifact_sha256"]
            == host_coordinate["client_artifact_sha256"],
            "trace host artifact coordinate differs",
        )
    validate_registry_projection(trace["registry"])
    assertions = _assertion_map(trace)
    _validate_semantic_assertions(assertions)
    require(
        assertions["host.launcher"]["sha256"]
        == bindings["mcp_binary_sha256"],
        "trace launcher is not package-manifest bound",
    )
    _validate_form_outcomes(trace["form_outcomes"])
    _validate_revision_timeline(trace["revision_timeline"])
    redaction = _exact_fields(
        trace["redaction"],
        {
            "absolute_paths_absent",
            "approval_content_absent",
            "native_ids_absent",
            "secrets_absent",
            "source_content_absent",
            "truncated",
        },
        label="trace redaction",
    )
    require(
        redaction == derive_surface_trace_redaction(trace),
        "surface trace is redacted incompletely or truncated",
    )
    if qualifying:
        require(
            recorder_journal is not None
            and recorder_journal_digest is not None,
            "qualifying trace is not bound to a recorder journal",
        )
    if recorder_journal is not None:
        journal = validate_recorder_journal(
            recorder_journal,
            qualifying=qualifying,
        )
        digest = (
            recorder_journal_digest
            if recorder_journal_digest is not None
            else recorder_journal_sha256(journal)
        )
        _digest(digest, label="recorder journal artifact")
        journal_integrity = cast(Mapping[str, Any], journal["integrity"])
        journal_bindings = cast(Mapping[str, Any], journal["bindings"])
        require(
            digest == bindings["recorder_journal_sha256"]
            and journal_integrity["event_chain_sha256"]
            == bindings["recorder_event_chain_sha256"]
            and journal_integrity["event_count"]
            == bindings["recorder_event_count"]
            and journal["surface"] == trace["surface"]
            and journal["host"] == trace["host"]
            and all(
                journal_bindings[field] == bindings[field]
                for field in RECORDER_BINDING_FIELDS
            ),
            "surface trace and recorder journal binding differs",
        )
    if qualifying:
        authority = _exact_fields(
            source_bound_authority,
            {
                "schema_version",
                "authority_kind",
                "trust_boundary",
                "operator_id",
                "surface",
                "artifact_directory",
                "bindings",
                "observations",
                "attestation_projection_sha256",
            },
            label="validated surface authority",
        )
        authority_bindings = _exact_fields(
            authority["bindings"],
            SURFACE_AUTHORITY_BINDING_FIELDS,
            label="validated surface authority bindings",
        )
        authority_observations = _exact_fields(
            authority["observations"],
            SURFACE_AUTHORITY_OBSERVATION_FIELDS,
            label="validated surface authority observations",
        )
        require(
            authority["schema_version"]
            == "s11-surface-acquisition-authority/1.0"
            and authority["authority_kind"] == "external_operator_attestation"
            and authority["trust_boundary"]
            == SURFACE_AUTHORITY_TRUST_BOUNDARY
            and authority["surface"] == trace["surface"]
            and all(value is True for value in authority_observations.values())
            and all(
                authority_bindings[field] == bindings[field]
                for field in (
                    "package_source_commit",
                    "package_manifest_sha256",
                    "host_provenance_sha256",
                    "host_artifact_sha256",
                    "client_artifact_sha256",
                    "mcp_binary_sha256",
                    "project_fixture_sha256",
                    "prompt_pack_sha256",
                    "recorder_journal_sha256",
                )
            ),
            "surface trace external authority binding differs",
        )
    safe_evidence_scan(trace)
    return dict(trace)


def _revision_domain(key: str) -> str | None:
    normalized = key
    for prefix in (
        "after_",
        "base_",
        "before_",
        "current_",
        "expected_",
        "result_",
    ):
        if normalized.startswith(prefix):
            normalized = normalized[len(prefix) :]
            break
    return normalized if normalized in REVISION_SUFFIXES else None


def _local_context(
    value: Mapping[str, Any],
    inherited: tuple[tuple[str, str], ...],
) -> tuple[tuple[str, str], ...]:
    result = dict(inherited)
    for key in SESSION_KEYS:
        session = value.get(key)
        if isinstance(session, str):
            result[key] = session
    return tuple(sorted(result.items()))


def _collect_revisions(
    value: Any,
    target: dict[tuple[str, tuple[tuple[str, str], ...]], set[int]],
    *,
    context: tuple[tuple[str, str], ...] = (),
) -> None:
    if isinstance(value, dict):
        local = _local_context(value, context)
        for key in sorted(value):
            item = value[key]
            domain = _revision_domain(key)
            if (
                domain is not None
                and isinstance(item, int)
                and not isinstance(item, bool)
            ):
                target.setdefault((domain, local), set()).add(item)
            _collect_revisions(item, target, context=local)
    elif isinstance(value, list):
        for item in value:
            _collect_revisions(item, target, context=context)


class _AlphaRenamer:
    def __init__(self) -> None:
        self.values: dict[str, dict[str, str]] = {}

    def rename(self, category: str, value: str) -> str:
        category_values = self.values.setdefault(category, {})
        if value not in category_values:
            category_values[value] = f"{category}:{len(category_values)}"
        return category_values[value]


def _ephemeral_category(key: str, value: Any) -> str | None:
    if key in EPHEMERAL_KEY_CATEGORIES:
        return EPHEMERAL_KEY_CATEGORIES[key]
    for base, category in EPHEMERAL_KEY_CATEGORIES.items():
        if key.endswith("_" + base) or key.startswith(base + "_"):
            return category
    if (
        key == "evidence_id"
        and isinstance(value, str)
        and value.startswith("evidence:ephemeral:")
    ):
        return "ephemeral_evidence"
    return None


def _normalize_semantic_value(
    value: Any,
    *,
    renamer: _AlphaRenamer,
    revision_ranks: Mapping[
        tuple[str, tuple[tuple[str, str], ...]],
        Mapping[int, int],
    ],
    context: tuple[tuple[str, str], ...] = (),
    parent_key: str = "",
) -> Any:
    if isinstance(value, dict):
        local = _local_context(value, context)
        result: dict[str, Any] = {}
        for key in sorted(value):
            if key in IGNORED_PARITY_KEYS:
                continue
            item = value[key]
            if key == "preview_digest":
                result[key] = "preview_digest:locally_valid"
                continue
            domain = _revision_domain(key)
            if (
                domain is not None
                and isinstance(item, int)
                and not isinstance(item, bool)
            ):
                rank = revision_ranks[(domain, local)][item]
                result[key] = f"revision:{domain}:{rank}"
                continue
            category = _ephemeral_category(key, item)
            if category is not None and isinstance(item, str):
                result[key] = renamer.rename(category, item)
                continue
            result[key] = _normalize_semantic_value(
                item,
                renamer=renamer,
                revision_ranks=revision_ranks,
                context=local,
                parent_key=key,
            )
        return result
    if isinstance(value, list):
        normalized = [
            _normalize_semantic_value(
                item,
                renamer=renamer,
                revision_ranks=revision_ranks,
                context=context,
                parent_key=parent_key,
            )
            for item in value
        ]
        if parent_key in {"conflicts", "diagnostics", "partial_reasons"}:
            return sorted(normalized, key=canonical_json)
        return normalized
    require(
        not isinstance(value, float) or math.isfinite(value),
        "non-finite parity value",
    )
    return value


def normalize_surface_trace(
    value: Any,
    *,
    qualifying: bool,
    recorder_journal: Mapping[str, Any] | None = None,
    recorder_journal_digest: str | None = None,
    source_bound_authority: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    trace = validate_surface_trace(
        value,
        qualifying=qualifying,
        recorder_journal=recorder_journal,
        recorder_journal_digest=recorder_journal_digest,
        source_bound_authority=source_bound_authority,
    )
    revisions: dict[
        tuple[str, tuple[tuple[str, str], ...]],
        set[int],
    ] = {}
    semantic_input = {
        "assertions": sorted(
            trace["assertions"],
            key=lambda item: cast(Mapping[str, Any], item)["assertion_id"],
        ),
        "form_outcomes": sorted(
            trace["form_outcomes"],
            key=lambda item: cast(Mapping[str, Any], item)["scenario"],
        ),
        "revision_timeline": trace["revision_timeline"],
    }
    _collect_revisions(semantic_input, revisions)
    ranks = {
        key: {
            revision: rank
            for rank, revision in enumerate(sorted(values))
        }
        for key, values in revisions.items()
    }
    renamer = _AlphaRenamer()
    normalized_semantics = _normalize_semantic_value(
        semantic_input,
        renamer=renamer,
        revision_ranks=ranks,
    )
    bindings = trace["bindings"]
    require(isinstance(bindings, dict), "trace bindings are missing")
    return {
        "bindings": {
            key: bindings[key]
            for key in (
                "godot_artifact_sha256",
                "mcp_binary_sha256",
                "package_manifest_sha256",
                "package_source_commit",
                "project_fixture_sha256",
                "prompt_pack_sha256",
                "registry_sha256",
            )
        },
        "registry": trace["registry"],
        "semantics": normalized_semantics,
    }


def compare_surface_traces(
    traces: Mapping[str, Any],
    *,
    qualifying: bool,
    recorder_journals: Mapping[str, Mapping[str, Any]] | None = None,
    recorder_journal_digests: Mapping[str, str] | None = None,
    source_bound_authorities: Mapping[str, Mapping[str, Any]] | None = None,
) -> str:
    require(
        set(traces) == {"app", "cli", "ide"},
        "App/CLI/IDE trace set differs",
    )
    if qualifying:
        require(
            recorder_journals is not None
            and set(recorder_journals) == {"app", "cli", "ide"}
            and recorder_journal_digests is not None
            and set(recorder_journal_digests) == {"app", "cli", "ide"},
            "qualifying parity comparison lacks recorder journals",
        )
        require(
            source_bound_authorities is not None
            and set(source_bound_authorities) == {"app", "cli", "ide"},
            "qualifying parity comparison lacks external surface authorities",
        )
    normalized: dict[str, dict[str, Any]] = {}
    for surface in ("app", "cli", "ide"):
        trace = traces[surface]
        require(
            isinstance(trace, dict) and trace.get("surface") == surface,
            "trace is bound to the wrong surface",
        )
        normalized[surface] = normalize_surface_trace(
            trace,
            qualifying=qualifying,
            recorder_journal=(
                recorder_journals[surface]
                if recorder_journals is not None
                else None
            ),
            recorder_journal_digest=(
                recorder_journal_digests[surface]
                if recorder_journal_digests is not None
                else None
            ),
            source_bound_authority=(
                source_bound_authorities[surface]
                if source_bound_authorities is not None
                else None
            ),
        )
    reference = canonical_json(normalized["app"])
    for surface in ("cli", "ide"):
        require(
            canonical_json(normalized[surface]) == reference,
            f"surface_semantic_divergence:{surface}",
        )
    return sha256_bytes(reference)


def validate_detached_package_manifest(
    value: Any,
    *,
    manifest_relative_path: str,
    artifact_root: Path | None,
    expected_source_commit: str,
) -> dict[str, Any]:
    manifest = _exact_fields(value, PACKAGE_FIELDS, label="package manifest")
    require(
        len(canonical_json(manifest)) <= MAX_PACKAGE_MANIFEST_BYTES,
        "package manifest exceeds byte bound",
    )
    require(
        manifest["schema_version"] == "s11-package-manifest/1.0",
        "package manifest schema differs",
    )
    require(
        manifest["source_commit"] == expected_source_commit,
        "package manifest source commit differs",
    )
    provenance = _exact_fields(
        manifest["build_provenance"],
        BUILD_PROVENANCE_FIELDS,
        label="package build provenance",
    )
    _digest(provenance["cargo_lock_sha256"], label="Cargo.lock provenance")
    _digest(
        provenance["rust_toolchain_sha256"],
        label="Rust toolchain provenance",
    )
    _bounded_string(
        provenance["cargo_version"],
        label="Cargo version provenance",
        maximum=256,
    )
    _bounded_string(
        provenance["rustc_release"],
        label="rustc release provenance",
        maximum=128,
    )
    _commit(provenance["rustc_commit"], label="rustc provenance")
    require(
        provenance["fresh_target"] is True
        and provenance["target_triple"] == "aarch64-apple-darwin",
        "package build isolation provenance differs",
    )
    _bounded_string(
        manifest["package_version"],
        label="package version",
        maximum=64,
    )
    _digest(
        manifest["compatibility_matrix_sha256"],
        label="compatibility matrix",
    )
    _digest(manifest["registry_sha256"], label="package registry")
    third_party_licenses_digest = _digest(
        manifest["third_party_licenses_sha256"],
        label="third-party licenses",
    )
    archive = _exact_fields(
        manifest["archive"],
        {"path", "sha256", "bytes"},
        label="package archive",
    )
    archive_path = _safe_relative_path(archive["path"], label="package archive")
    archive_digest = _digest(archive["sha256"], label="package archive")
    require(
        isinstance(archive["bytes"], int)
        and not isinstance(archive["bytes"], bool)
        and 0 < archive["bytes"] <= 512 * 1024 * 1024,
        "package archive byte count differs",
    )
    detached_path = _safe_relative_path(
        manifest_relative_path,
        label="detached package manifest",
    )
    contents = manifest["contents"]
    require(
        isinstance(contents, list) and 2 <= len(contents) <= 256,
        "package content manifest bound differs",
    )
    seen: set[str] = set()
    content_records: dict[str, Mapping[str, Any]] = {}
    for item in contents:
        record = _exact_fields(
            item,
            {"path", "sha256", "bytes", "mode"},
            label="package content",
        )
        relative = _safe_relative_path(record["path"], label="package content")
        require(
            relative not in seen
            and relative != detached_path
            and not relative.endswith("/sprint11-package-manifest.json"),
            "package manifest is duplicated, self-listed, or path-colliding",
        )
        seen.add(relative)
        _digest(record["sha256"], label="package content")
        require(
            isinstance(record["bytes"], int)
            and not isinstance(record["bytes"], bool)
            and 0 <= record["bytes"] <= 256 * 1024 * 1024
            and record["mode"] in {"0644", "0755"},
            "package content metadata differs",
        )
        content_records[relative] = record
    require(
        sum(cast(int, record["bytes"]) for record in content_records.values())
        <= 512 * 1024 * 1024,
        "package content aggregate exceeds byte bound",
    )
    require(
        {
            "bin/godot-codex",
            "bin/godot-codex-mcp",
            "share/godot-codex/licenses/Godot-LICENSE.txt",
            "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt",
        }.issubset(content_records),
        "package omits a required binary or license bundle",
    )
    require(
        content_records["bin/godot-codex"]["mode"] == "0755"
        and content_records["bin/godot-codex-mcp"]["mode"] == "0755",
        "packaged binaries are not executable",
    )
    godot_license = content_records[
        "share/godot-codex/licenses/Godot-LICENSE.txt"
    ]
    require(
        godot_license["mode"] == "0644"
        and isinstance(godot_license["bytes"], int)
        and godot_license["bytes"] > 0,
        "packaged Godot license metadata differs",
    )
    third_party_licenses = content_records[
        "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt"
    ]
    require(
        third_party_licenses["mode"] == "0644"
        and isinstance(third_party_licenses["bytes"], int)
        and third_party_licenses["bytes"] > 0
        and third_party_licenses["sha256"] == third_party_licenses_digest,
        "packaged third-party license metadata differs",
    )
    prerequisite = _exact_fields(
        manifest["godot_prerequisite"],
        {
            "version",
            "commit",
            "sha256",
            "architecture",
            "expected_install_path",
            "verification",
        },
        label="Godot prerequisite",
    )
    _bounded_string(
        prerequisite["version"],
        label="Godot prerequisite version",
        maximum=64,
    )
    _commit(prerequisite["commit"], label="Godot prerequisite")
    require(
        prerequisite["sha256"] == EXACT_GODOT_PREREQUISITE_SHA256
        and prerequisite["architecture"] == "arm64"
        and prerequisite["expected_install_path"] == EXACT_GODOT_INSTALL_PATH,
        "Godot prerequisite identity differs",
    )
    verification = _exact_fields(
        prerequisite["verification"],
        {"sha256", "version"},
        label="Godot prerequisite verification",
    )
    require(
        verification == EXACT_GODOT_VERIFICATION,
        "Godot prerequisite verification commands differ",
    )
    if artifact_root is not None:
        root = artifact_root.resolve()

        def bound_file(relative: str) -> Path:
            candidate = root.joinpath(*PurePosixPath(relative).parts)
            require(
                not candidate.is_symlink()
                and candidate.is_file()
                and root in candidate.resolve().parents,
                "package artifact is missing, symlinked, or escapes artifact root",
            )
            return candidate

        archive_file = bound_file(archive_path)
        require(
            archive_file.stat().st_size == archive["bytes"]
            and sha256_file(archive_file) == archive_digest,
            "package archive digest/size differs",
        )
        archive_root_name = PurePosixPath(archive_path).name
        if archive_root_name.endswith(".tar.gz"):
            archive_root_name = archive_root_name[: -len(".tar.gz")]
        else:
            raise AcceptanceError("package archive extension differs")
        archived_files: set[str] = set()
        declared_bytes = 0
        try:
            with tarfile.open(archive_file, mode="r:gz") as package_archive:
                member_count = 0
                for member in package_archive:
                    member_count += 1
                    require(
                        member_count <= 1024,
                        "package archive member bound differs",
                    )
                    member_path = PurePosixPath(member.name)
                    require(
                        not member_path.is_absolute()
                        and ".." not in member_path.parts
                        and member_path.parts
                        and member_path.parts[0] == archive_root_name
                        and member.uid == 0
                        and member.gid == 0
                        and member.mtime == 0
                        and member.uname == "root"
                        and member.gname == "root",
                        "package archive metadata/path differs",
                    )
                    if member.isdir():
                        require(
                            member.mode == 0o755,
                            "package archive directory mode differs",
                        )
                        continue
                    require(
                        member.isfile() and len(member_path.parts) > 1,
                        "package archive contains a non-regular member",
                    )
                    relative = PurePosixPath(*member_path.parts[1:]).as_posix()
                    record = content_records.get(relative)
                    require(
                        record is not None
                        and relative not in archived_files
                        and member.mode == int(cast(str, record["mode"]), 8)
                        and member.size == record["bytes"],
                        "package archive content manifest differs",
                    )
                    declared_bytes += member.size
                    require(
                        declared_bytes <= 512 * 1024 * 1024,
                        "package archive expands beyond byte bound",
                    )
                    stream = package_archive.extractfile(member)
                    require(stream is not None, "package member is unreadable")
                    content = stream.read(cast(int, record["bytes"]) + 1)
                    require(
                        len(content) == record["bytes"]
                        and sha256_bytes(content) == record["sha256"],
                        "package archive member digest differs",
                    )
                    archived_files.add(relative)
                require(member_count > 0, "package archive is empty")
        except (OSError, tarfile.TarError) as error:
            raise AcceptanceError("package archive is invalid") from error
        require(
            archived_files == set(content_records),
            "package archive and detached contents are not bijective",
        )
        for relative, record in content_records.items():
            path = bound_file(relative)
            require(
                path.stat().st_size == record["bytes"]
                and sha256_file(path) == record["sha256"],
                "packaged file digest/size differs",
            )
    safe_evidence_scan(manifest)
    return dict(manifest)


def package_content_record(
    package: Mapping[str, Any],
    relative_path: str,
) -> Mapping[str, Any]:
    contents = package.get("contents")
    require(isinstance(contents, list), "package content manifest is missing")
    matches = [
        item
        for item in contents
        if isinstance(item, dict) and item.get("path") == relative_path
    ]
    require(len(matches) == 1, "package content binding is missing or ambiguous")
    return cast(Mapping[str, Any], matches[0])


def validate_packaged_regression_receipt(
    receipt: Any,
    *,
    repository: GitRepository,
    source_commit: str,
    package_source_commit: str,
    package_manifest_sha256: str,
    package_archive_sha256: str,
    package: Mapping[str, Any],
    registry_sha256: str,
    godot_artifact_sha256: str,
) -> set[str]:
    root = _exact_fields(
        receipt,
        {
            "schema_version",
            "capture_kind",
            "status",
            "bindings",
            "fixtures",
            "commands",
            "coverage",
            "cleanup",
            "redaction",
        },
        label="packaged regression receipt",
    )
    require(
        len(canonical_json(root)) <= packaged_regressions.MAX_RECEIPT_BYTES
        and root["schema_version"] == packaged_regressions.RECEIPT_SCHEMA
        and root["capture_kind"] == packaged_regressions.CAPTURE_KIND
        and root["status"] == "passed",
        "packaged regression receipt identity differs",
    )
    sidecar_record = package_content_record(
        package,
        packaged_regressions.PACKAGE_SIDECAR_PATH,
    )
    sidecar_sha256 = _digest(
        sidecar_record["sha256"],
        label="packaged sidecar",
    )
    prerequisite = cast(Mapping[str, Any], package["godot_prerequisite"])
    bindings = _exact_fields(
        root["bindings"],
        {
            "package_source_commit",
            "package_manifest_sha256",
            "package_build_provenance_sha256",
            "package_archive_sha256",
            "package_sidecar_path",
            "package_sidecar_sha256",
            "godot_version",
            "godot_commit",
            "godot_artifact_sha256",
            "registry_sha256",
        },
        label="packaged regression bindings",
    )
    require(
        bindings
        == {
            "package_source_commit": package_source_commit,
            "package_manifest_sha256": package_manifest_sha256,
            "package_build_provenance_sha256": sha256_bytes(
                canonical_json(package["build_provenance"])
            ),
            "package_archive_sha256": package_archive_sha256,
            "package_sidecar_path": packaged_regressions.PACKAGE_SIDECAR_PATH,
            "package_sidecar_sha256": sidecar_sha256,
            "godot_version": prerequisite["version"],
            "godot_commit": prerequisite["commit"],
            "godot_artifact_sha256": godot_artifact_sha256,
            "registry_sha256": registry_sha256,
        },
        "packaged regression product binding differs",
    )
    fixtures = root["fixtures"]
    require(
        isinstance(fixtures, list)
        and len(fixtures) == len(packaged_regressions.FIXTURE_PATHS),
        "packaged regression fixture set differs",
    )
    fixture_paths: set[str] = set()
    for item in fixtures:
        record = _exact_fields(
            item,
            {"id", "path", "sha256"},
            label="packaged regression fixture",
        )
        relative = _safe_relative_path(
            record["path"],
            label="packaged regression fixture",
        )
        require(
            relative in packaged_regressions.FIXTURE_PATHS
            and relative not in fixture_paths
            and sha256_bytes(repository.blob_at(package_source_commit, relative))
            == record["sha256"],
            "packaged regression fixture binding differs",
        )
        _bounded_string(record["id"], label="fixture ID", maximum=128)
        fixture_paths.add(relative)
    require(
        fixture_paths == set(packaged_regressions.FIXTURE_PATHS),
        "packaged regression fixtures are incomplete",
    )
    specs = packaged_regressions.canonical_command_specs()
    commands = root["commands"]
    require(
        isinstance(commands, list) and len(commands) == len(specs),
        "packaged regression command set differs",
    )
    reports: dict[str, Mapping[str, Any]] = {}
    acquisition_paths: set[str] = set()
    for spec, item in zip(specs, commands):
        record = _exact_fields(
            item,
            {
                "id",
                "cwd",
                "argv_template",
                "command_sha256",
                "runner_path",
                "runner_sha256",
                "report_path",
                "report_sha256",
                "stdout_sha256",
                "stderr_sha256",
                "exit_code",
                "duration_ms",
            },
            label="packaged regression command",
        )
        template = list(spec.argv_template())
        require(
            record["id"] == spec.command_id
            and record["cwd"] == "."
            and record["argv_template"] == template
            and record["command_sha256"]
            == sha256_bytes(canonical_json({"cwd": ".", "argv": template}))
            and record["runner_path"] == spec.runner_path
            and record["runner_sha256"]
            == sha256_bytes(
                repository.blob_at(package_source_commit, spec.runner_path)
            )
            and record["exit_code"] == 0
            and isinstance(record["duration_ms"], int)
            and not isinstance(record["duration_ms"], bool)
            and 0 <= record["duration_ms"] <= 180_000,
            "packaged regression command binding differs",
        )
        _digest(record["stdout_sha256"], label="packaged regression stdout")
        _digest(record["stderr_sha256"], label="packaged regression stderr")
        report_path = _safe_relative_path(
            record["report_path"],
            label="packaged regression report",
        )
        require(
            report_path.startswith(
                "tests/codex/acquisition/sprint11/package-live/"
            )
            and report_path not in acquisition_paths,
            "packaged regression report path differs",
        )
        report_blob = repository.blob_at(source_commit, report_path)
        require(
            len(report_blob) <= packaged_regressions.MAX_REPORT_BYTES
            and sha256_bytes(report_blob) == record["report_sha256"],
            "packaged regression report digest differs",
        )
        report = strict_json_bytes(
            report_blob,
            label=f"{spec.command_id} report",
            maximum_bytes=packaged_regressions.MAX_REPORT_BYTES,
        )
        try:
            packaged_regressions.validate_live_report(
                spec.command_id,
                report,
                godot_sha256=godot_artifact_sha256,
                sidecar_sha256=sidecar_sha256,
            )
        except packaged_regressions.PackagedRegressionError as error:
            raise AcceptanceError(
                f"{spec.command_id} package-live report is invalid"
            ) from error
        reports[spec.command_id] = report
        acquisition_paths.add(report_path)
    try:
        packaged_regressions.validate_aggregate_reports(reports)
    except packaged_regressions.PackagedRegressionError as error:
        raise AcceptanceError("packaged regression shard coverage differs") from error
    coverage = _exact_fields(
        root["coverage"],
        {
            "sprints",
            "sprint9_operations",
            "sprint9_negative_scenarios",
            "sprint9_fault_scenarios",
            "sprint10_validation",
        },
        label="packaged regression coverage",
    )
    require(
        coverage
        == {
            "sprints": [6, 7, 8, 9, 10],
            "sprint9_operations": sorted(packaged_regressions.SPRINT9_OPERATIONS),
            "sprint9_negative_scenarios": sorted(
                packaged_regressions.SPRINT9_NEGATIVES
            ),
            "sprint9_fault_scenarios": sorted(
                packaged_regressions.SPRINT9_FAULTS
            ),
            "sprint10_validation": True,
        },
        "packaged regression coverage declaration differs",
    )
    cleanup = _exact_fields(
        root["cleanup"],
        {
            "editor_processes_stopped",
            "game_processes_stopped",
            "sidecar_processes_stopped",
            "temporary_workspaces_removed",
            "package_unchanged",
        },
        label="packaged regression cleanup",
    )
    redaction = _exact_fields(
        root["redaction"],
        {
            "absolute_paths_absent",
            "native_ids_absent",
            "secrets_absent",
            "source_content_absent",
        },
        label="packaged regression redaction",
    )
    require(
        all(item is True for item in cleanup.values())
        and all(item is True for item in redaction.values()),
        "packaged regression cleanup or redaction differs",
    )
    safe_evidence_scan(root)
    return acquisition_paths


def _expected_package_receipt_bindings(
    *,
    package_source_commit: str,
    package_manifest_sha256: str,
    package_archive_sha256: str,
    package: Mapping[str, Any],
    registry_sha256: str,
    godot_artifact_sha256: str,
) -> dict[str, Any]:
    sidecar = package_content_record(
        package,
        packaged_regressions.PACKAGE_SIDECAR_PATH,
    )
    prerequisite = cast(Mapping[str, Any], package["godot_prerequisite"])
    return {
        "package_source_commit": package_source_commit,
        "package_version": package["package_version"],
        "package_manifest_sha256": package_manifest_sha256,
        "package_build_provenance_sha256": sha256_bytes(
            canonical_json(package["build_provenance"])
        ),
        "package_archive_sha256": package_archive_sha256,
        "package_sidecar_path": packaged_regressions.PACKAGE_SIDECAR_PATH,
        "package_sidecar_sha256": sidecar["sha256"],
        "godot_version": prerequisite["version"],
        "godot_commit": prerequisite["commit"],
        "godot_artifact_sha256": godot_artifact_sha256,
        "registry_sha256": registry_sha256,
    }


def validate_host_provenance_receipt(
    value: Any,
    *,
    expected_profile_sha256: str,
) -> dict[str, Mapping[str, Any]]:
    require(
        sha256_bytes(HOST_PROFILE_PATH.read_bytes()) == expected_profile_sha256,
        "host provenance profile authority differs",
    )
    try:
        receipt = host_provenance.validate_host_provenance_document(
            value,
            profile_path=HOST_PROFILE_PATH,
        )
    except host_provenance.HostProvenanceError as error:
        raise AcceptanceError("host provenance receipt differs") from error
    require(
        receipt["host_coordinate_profile_sha256"] == expected_profile_sha256,
        "host provenance receipt profile differs",
    )
    safe_evidence_scan(receipt)
    return {
        cast(str, item["surface"]): cast(Mapping[str, Any], item)
        for item in cast(list[Mapping[str, Any]], receipt["surfaces"])
    }


def validate_host_provenance_acquisition(
    value: Any,
    *,
    evidence_binding: Mapping[str, Any],
    repository: GitRepository,
    source_commit: str,
    expected_source_commit: str,
    expected_profile_sha256: str,
) -> tuple[set[str], dict[str, Mapping[str, Any]]]:
    receipt_path = _safe_relative_path(
        evidence_binding["receipt_path"],
        label="host provenance acquisition receipt",
    )
    measurement_evidence_path = _safe_relative_path(
        evidence_binding["measurement_path"],
        label="host provenance acquisition measurement",
    )
    receipt_object = PurePosixPath(receipt_path)
    measurement_object = PurePosixPath(measurement_evidence_path)
    require(
        receipt_object.name == "receipt.json"
        and measurement_object.name == "measurement.json"
        and receipt_object.parent == measurement_object.parent
        and receipt_object.parent.parts[:4]
        == ("tests", "codex", "acquisition", "sprint11")
        and len(receipt_object.parent.parts) == 5
        and re.fullmatch(
            r"[A-Za-z0-9._+-]+",
            receipt_object.parent.name,
        )
        is not None,
        "host provenance acquisition output paths differ",
    )
    receipt = _exact_fields(
        value,
        {
            "schema_version",
            "capture_kind",
            "status",
            "source_commit",
            "bindings",
            "command",
            "assertions",
            "redaction",
        },
        label="host provenance acquisition receipt",
    )
    require(
        receipt["schema_version"]
        == external_acquisitions.HOST_ACQUISITION_SCHEMA
        and receipt["capture_kind"]
        == external_acquisitions.HOST_CAPTURE_KIND
        and receipt["status"] == "passed"
        and receipt["source_commit"] == expected_source_commit
        and len(canonical_json(receipt))
        <= external_acquisitions.MAX_RECEIPT_BYTES,
        "host provenance acquisition identity/source differs",
    )
    bindings = _exact_fields(
        receipt["bindings"],
        {
            "runner_path",
            "runner_sha256",
            "measurement_schema_path",
            "measurement_schema_sha256",
            "acquisition_schema_path",
            "acquisition_schema_sha256",
            "host_coordinate_profile_path",
            "host_coordinate_profile_sha256",
            "measurement_path",
            "measurement_sha256",
        },
        label="host provenance acquisition bindings",
    )
    immutable_inputs = {
        "runner_path": external_acquisitions.HOST_RUNNER,
        "measurement_schema_path":
            external_acquisitions.HOST_MEASUREMENT_SCHEMA_PATH,
        "acquisition_schema_path":
            external_acquisitions.HOST_ACQUISITION_SCHEMA_PATH,
        "host_coordinate_profile_path":
            external_acquisitions.HOST_PROFILE_PATH,
    }
    for field, expected_path in immutable_inputs.items():
        require(bindings[field] == expected_path, f"{field} differs")
        digest_field = field.replace("_path", "_sha256")
        require(
            bindings[digest_field]
            == sha256_bytes(repository.blob_at(expected_source_commit, expected_path)),
            f"{field} source binding differs",
        )
    require(
        bindings["host_coordinate_profile_sha256"]
        == expected_profile_sha256,
        "host provenance profile binding differs",
    )
    measurement_path = _safe_relative_path(
        bindings["measurement_path"],
        label="host provenance measurement",
    )
    measurement_sha256 = _digest(
        bindings["measurement_sha256"],
        label="host provenance measurement",
    )
    require(
        measurement_evidence_path == measurement_path
        and evidence_binding["measurement_sha256"] == measurement_sha256,
        "evidence host measurement binding differs",
    )
    command = _exact_fields(
        receipt["command"],
        {
            "id",
            "cwd",
            "argv_template",
            "command_sha256",
            "runner_path",
            "runner_sha256",
            "stdout_sha256",
            "stderr_sha256",
            "exit_code",
            "duration_ms",
            "report_path",
            "report_sha256",
        },
        label="host provenance acquisition command",
    )
    template = list(external_acquisitions.host_provenance_command_template())
    require(
        command["id"] == "host_provenance"
        and command["cwd"] == "."
        and command["argv_template"] == template
        and command["command_sha256"]
        == sha256_bytes(canonical_json({"cwd": ".", "argv": template}))
        and command["runner_path"] == bindings["runner_path"]
        and command["runner_sha256"] == bindings["runner_sha256"]
        and command["report_path"] == measurement_path
        and command["report_sha256"] == measurement_sha256
        and command["exit_code"] == 0
        and isinstance(command["duration_ms"], int)
        and not isinstance(command["duration_ms"], bool)
        and 0
        <= command["duration_ms"]
        <= external_acquisitions.MAX_COMMAND_SECONDS * 1_000,
        "host provenance acquisition command differs",
    )
    _digest(command["stdout_sha256"], label="host provenance stdout")
    _digest(command["stderr_sha256"], label="host provenance stderr")
    assertions = _exact_fields(
        receipt["assertions"],
        {
            "profile_matched",
            "extension_tree_complete",
            "code_signatures_verified",
            "symlinks_rejected",
            "source_checkout_unchanged",
        },
        label="host provenance acquisition assertions",
    )
    redaction = _exact_fields(
        receipt["redaction"],
        {
            "absolute_paths_absent",
            "account_identity_absent",
            "artifact_content_absent",
            "environment_secrets_absent",
        },
        label="host provenance acquisition redaction",
    )
    require(
        all(flag is True for flag in assertions.values())
        and all(flag is True for flag in redaction.values()),
        "host provenance acquisition assertions/redaction differ",
    )
    measurement_blob = repository.blob_at(source_commit, measurement_path)
    require(
        sha256_bytes(measurement_blob) == measurement_sha256,
        "host provenance measurement Git binding differs",
    )
    measurement = strict_json_bytes(
        measurement_blob,
        label="host provenance measurement",
        maximum_bytes=host_provenance.MAX_OUTPUT_BYTES,
    )
    observed = validate_host_provenance_receipt(
        measurement,
        expected_profile_sha256=expected_profile_sha256,
    )
    safe_evidence_scan(receipt)
    return {measurement_path}, observed


def validate_multi_project_receipt(
    value: Any,
    *,
    repository: GitRepository,
    source_commit: str,
    expected_bindings: Mapping[str, Any],
) -> set[str]:
    receipt = _exact_fields(
        value,
        {
            "schema_version",
            "capture_kind",
            "status",
            "bindings",
            "fixtures",
            "command",
            "assertions",
            "cleanup",
            "redaction",
        },
        label="multi-project receipt",
    )
    require(
        receipt["schema_version"] == external_acquisitions.MULTI_RECEIPT_SCHEMA
        and receipt["capture_kind"] == external_acquisitions.MULTI_CAPTURE_KIND
        and receipt["status"] == "passed"
        and len(canonical_json(receipt))
        <= external_acquisitions.MAX_RECEIPT_BYTES,
        "multi-project receipt identity differs",
    )
    bindings = _exact_fields(
        receipt["bindings"],
        set(expected_bindings),
        label="multi-project package bindings",
    )
    require(
        bindings == dict(expected_bindings),
        "multi-project package binding differs",
    )
    fixture_paths: set[str] = set()
    fixtures = receipt["fixtures"]
    require(
        isinstance(fixtures, list) and 1 <= len(fixtures) <= 128,
        "multi-project fixture bound differs",
    )
    for item in fixtures:
        record = _exact_fields(
            item,
            {"path", "sha256"},
            label="multi-project fixture",
        )
        relative = _safe_relative_path(record["path"], label="multi-project fixture")
        require(relative not in fixture_paths, "multi-project fixture is duplicated")
        require(
            sha256_bytes(
                repository.blob_at(
                    cast(str, bindings["package_source_commit"]),
                    relative,
                )
            )
            == record["sha256"],
            "multi-project fixture source binding differs",
        )
        fixture_paths.add(relative)
    command = _exact_fields(
        receipt["command"],
        {
            "id",
            "cwd",
            "argv_template",
            "command_sha256",
            "runner_path",
            "runner_sha256",
            "report_path",
            "report_sha256",
            "stdout_sha256",
            "stderr_sha256",
            "exit_code",
            "duration_ms",
        },
        label="multi-project command",
    )
    template = list(external_acquisitions.multi_command_template())
    require(
        command["id"] == "multi_project_live"
        and command["cwd"] == "."
        and command["argv_template"] == template
        and command["command_sha256"]
        == sha256_bytes(canonical_json({"cwd": ".", "argv": template}))
        and command["runner_path"] == external_acquisitions.MULTI_RUNNER
        and command["runner_sha256"]
        == sha256_bytes(
            repository.blob_at(
                cast(str, bindings["package_source_commit"]),
                external_acquisitions.MULTI_RUNNER,
            )
        )
        and command["exit_code"] == 0
        and isinstance(command["duration_ms"], int)
        and not isinstance(command["duration_ms"], bool)
        and 0
        <= command["duration_ms"]
        <= external_acquisitions.MAX_COMMAND_SECONDS * 1_000,
        "multi-project command binding differs",
    )
    for field in ("stdout_sha256", "stderr_sha256"):
        _digest(command[field], label=f"multi-project {field}")
    report_path = _safe_relative_path(
        command["report_path"],
        label="multi-project report",
    )
    report_blob = repository.blob_at(source_commit, report_path)
    require(
        sha256_bytes(report_blob) == command["report_sha256"],
        "multi-project report Git binding differs",
    )
    report = strict_json_bytes(
        report_blob,
        label="multi-project report",
        maximum_bytes=multi_project.REPORT_LIMIT,
    )
    try:
        validated_report = multi_project.validate_report(report)
    except multi_project.s9.WorkflowError as error:
        raise AcceptanceError("multi-project live report differs") from error
    artifacts = cast(Mapping[str, Any], validated_report["artifacts"])
    require(
        artifacts["godot_sha256"] == bindings["godot_artifact_sha256"]
        and artifacts["sidecar_sha256"] == bindings["package_sidecar_sha256"],
        "multi-project report artifact binding differs",
    )
    assertions = _exact_fields(
        receipt["assertions"],
        {
            "two_real_editors",
            "two_package_sidecars",
            "separate_action_only_approvals",
            "validation_reports_isolated",
            "readback_and_targeted_undo",
            "foreign_ids_rejected",
            "target_fault_isolated",
            "isolation_matrix_complete",
            "copied_and_swapped_discovery_token_rejected",
            "config_root_cwd_swaps_rejected",
            "editor_restart_isolated",
            "cache_rebuild_isolated",
            "package_version_mismatch_rejected",
            "no_cross_project_leakage",
            "source_unchanged",
        },
        label="multi-project receipt assertions",
    )
    cleanup = _exact_fields(
        receipt["cleanup"],
        {
            "editor_processes_stopped",
            "game_processes_stopped",
            "sidecar_processes_stopped",
            "temporary_workspaces_removed",
        },
        label="multi-project cleanup",
    )
    redaction = _exact_fields(
        receipt["redaction"],
        {
            "absolute_paths_absent",
            "native_ids_absent",
            "secrets_absent",
            "source_content_absent",
        },
        label="multi-project redaction",
    )
    require(
        all(item is True for item in assertions.values())
        and all(item is True for item in cleanup.values())
        and all(item is True for item in redaction.values()),
        "multi-project assertions/cleanup/redaction differ",
    )
    safe_evidence_scan(receipt)
    # Only the report is produced after the package source commit. Runners and
    # fixtures are immutable inputs that were verified above and must never
    # become allowed post-package changes.
    return {report_path}


def validate_reproducibility_receipt(
    value: Any,
    *,
    repository: GitRepository,
    expected_source_commit: str,
    godot_artifact_sha256: str,
    qualified_reference: Mapping[str, Any],
) -> set[str]:
    receipt = _exact_fields(
        value,
        {
            "schema_version",
            "capture_kind",
            "status",
            "bindings",
            "commands",
            "outputs",
            "reproducibility",
            "cleanup",
            "redaction",
        },
        label="reproducibility receipt",
    )
    require(
        receipt["schema_version"] == external_acquisitions.REPRO_RECEIPT_SCHEMA
        and receipt["capture_kind"] == external_acquisitions.REPRO_CAPTURE_KIND
        and receipt["status"] == "passed"
        and len(canonical_json(receipt))
        <= external_acquisitions.MAX_RECEIPT_BYTES,
        "reproducibility receipt identity differs",
    )
    runner_blob = repository.blob_at(
        expected_source_commit,
        external_acquisitions.BUILD_RUNNER,
    )
    qualified = _exact_fields(
        qualified_reference,
        {
            "qualified_manifest_sha256",
            "qualified_archive_sha256",
            "qualified_archive_bytes",
            "qualified_package_tree_sha256",
            "qualified_build_provenance_sha256",
        },
        label="qualified reproducibility reference",
    )
    for field in set(qualified) - {"qualified_archive_bytes"}:
        _digest(qualified[field], label=field)
    require(
        isinstance(qualified["qualified_archive_bytes"], int)
        and not isinstance(qualified["qualified_archive_bytes"], bool)
        and 0 < qualified["qualified_archive_bytes"] <= 512 * 1024 * 1024,
        "qualified archive byte count differs",
    )
    bindings = _exact_fields(
        receipt["bindings"],
        {
            "source_commit",
            "godot_artifact_sha256",
            "build_runner_path",
            "build_runner_sha256",
            *qualified,
        },
        label="reproducibility bindings",
    )
    require(
        bindings
        == {
            "source_commit": expected_source_commit,
            "godot_artifact_sha256": godot_artifact_sha256,
            "build_runner_path": external_acquisitions.BUILD_RUNNER,
            "build_runner_sha256": sha256_bytes(runner_blob),
            **qualified,
        },
        "reproducibility source/runner/release binding differs",
    )
    commands = receipt["commands"]
    require(isinstance(commands, list) and len(commands) == 2, "rebuild commands differ")
    for build_id, item in zip(("a", "b"), commands):
        command = _exact_fields(
            item,
            {
                "id",
                "cwd",
                "argv_template",
                "command_sha256",
                "runner_path",
                "runner_sha256",
                "stdout_sha256",
                "stderr_sha256",
                "exit_code",
                "duration_ms",
            },
            label="rebuild command",
        )
        template = list(
            external_acquisitions.reproducibility_command_template(build_id)
        )
        require(
            command["id"] == f"clean_rebuild_{build_id}"
            and command["cwd"] == "."
            and command["argv_template"] == template
            and command["command_sha256"]
            == sha256_bytes(canonical_json({"cwd": ".", "argv": template}))
            and command["runner_path"] == external_acquisitions.BUILD_RUNNER
            and command["runner_sha256"] == bindings["build_runner_sha256"]
            and command["exit_code"] == 0
            and isinstance(command["duration_ms"], int)
            and not isinstance(command["duration_ms"], bool)
            and 0
            <= command["duration_ms"]
            <= external_acquisitions.MAX_COMMAND_SECONDS * 1_000,
            "rebuild command differs",
        )
        _digest(command["stdout_sha256"], label="rebuild stdout")
        _digest(command["stderr_sha256"], label="rebuild stderr")
    outputs = receipt["outputs"]
    require(isinstance(outputs, list) and len(outputs) == 2, "rebuild outputs differ")
    normalized: list[dict[str, Any]] = []
    for build_id, item in zip(("a", "b"), outputs):
        output = _exact_fields(
            item,
            {
                "id",
                "manifest_sha256",
                "build_provenance_sha256",
                "archive_sha256",
                "archive_bytes",
                "package_tree_sha256",
            },
            label="rebuild output",
        )
        require(
            output["id"] == build_id
            and isinstance(output["archive_bytes"], int)
            and not isinstance(output["archive_bytes"], bool)
            and 0 < output["archive_bytes"] <= 512 * 1024 * 1024,
            "rebuild output metadata differs",
        )
        for field in set(output) - {"id", "archive_bytes"}:
            _digest(output[field], label=f"rebuild output {field}")
        normalized.append({key: output[key] for key in output if key != "id"})
    require(normalized[0] == normalized[1], "clean rebuild outputs differ")
    require(
        normalized[0]
        == {
            "manifest_sha256": qualified["qualified_manifest_sha256"],
            "build_provenance_sha256": qualified[
                "qualified_build_provenance_sha256"
            ],
            "archive_sha256": qualified["qualified_archive_sha256"],
            "archive_bytes": qualified["qualified_archive_bytes"],
            "package_tree_sha256": qualified[
                "qualified_package_tree_sha256"
            ],
        },
        "clean rebuilds differ from the qualified release package",
    )
    reproducibility = _exact_fields(
        receipt["reproducibility"],
        {
            "manifest_bytes_equal",
            "archive_bytes_equal",
            "package_tree_equal",
            "source_checkout_unchanged",
        },
        label="reproducibility assertions",
    )
    cleanup = _exact_fields(
        receipt["cleanup"],
        {"build_processes_stopped", "repository_unchanged"},
        label="reproducibility cleanup",
    )
    redaction = _exact_fields(
        receipt["redaction"],
        {
            "absolute_paths_absent",
            "environment_secrets_absent",
            "source_content_absent",
        },
        label="reproducibility redaction",
    )
    require(
        all(value is True for value in reproducibility.values())
        and all(value is True for value in cleanup.values())
        and all(value is True for value in redaction.values()),
        "reproducibility assertions/cleanup/redaction differ",
    )
    safe_evidence_scan(receipt)
    # Rebuild outputs live outside the repository. The public build runner is
    # an immutable package-source input, not an allowed post-package change.
    return set()


def validate_usability_report(
    value: Any,
    *,
    qualifying: bool,
    repository: GitRepository | None = None,
    source_commit: str | None = None,
    artifact_paths: set[str] | None = None,
) -> dict[str, Any]:
    report = _exact_fields(value, USABILITY_FIELDS, label="usability report")
    require(
        report["schema_version"] == "s11-usability-report/1.0",
        "usability report schema differs",
    )
    require(
        report["attestation_trust_boundary"]
        == human_usability.AUTHORITY_TRUST_BOUNDARY,
        "usability attestation trust boundary differs",
    )
    if qualifying:
        require(
            report["acquisition_kind"] == "real_human"
            and report["status"] == "passed",
            "qualifying usability report is synthetic or non-passing",
        )
    else:
        require(
            (report["acquisition_kind"], report["status"])
            in {
                ("real_human", "passed"),
                ("synthetic_contract_fixture", "fixture_valid"),
            },
            "usability acquisition/status pairing differs",
        )
    _commit(report["package_source_commit"], label="usability package source")
    _digest(
        report["package_manifest_sha256"],
        label="usability package manifest",
    )
    prompt = _exact_fields(
        report["prompt_pack"],
        {"id", "path", "sha256"},
        label="usability prompt pack",
    )
    require(
        prompt["id"] == "sprint11-external-beta-v1"
        and prompt["path"]
        == "tests/codex/prompts/sprint11-external-beta-v1.json",
        "usability prompt-pack binding differs",
    )
    _digest(prompt["sha256"], label="usability prompt pack")
    participants = report["participants"]
    require(
        isinstance(participants, list) and 3 <= len(participants) <= 32,
        "usability participant count differs",
    )
    participant_ids: set[str] = set()
    independent = 0
    for item in participants:
        participant = _exact_fields(
            item,
            USABILITY_PARTICIPANT_FIELDS,
            label="usability participant",
        )
        participant_id = participant["participant_id"]
        require(
            isinstance(participant_id, str)
            and PARTICIPANT_RE.fullmatch(participant_id) is not None
            and participant_id not in participant_ids,
            "participant pseudonym differs or is duplicated",
        )
        participant_ids.add(participant_id)
        require(
            participant["external_attestation_independence"]
            in {"independent", "not_independent"},
            "participant independence attestation projection differs",
        )
        independent += int(
            participant["external_attestation_independence"] == "independent"
        )
        bound_artifacts: list[tuple[str, str, str]] = []
        for kind in (
            "trace",
            "rubric",
            "consent",
            "defect_ledger",
            "authority",
        ):
            relative = _safe_relative_path(
                participant[f"{kind}_path"],
                label=f"participant {kind}",
            )
            digest_value = _digest(
                participant[f"{kind}_sha256"],
                label=f"participant {kind}",
            )
            bound_artifacts.append((kind, relative, digest_value))
        require(
            len({relative for _, relative, _ in bound_artifacts})
            == len(bound_artifacts),
            "participant primary bundle and authority paths overlap",
        )
        artifact_parents = {
            PurePosixPath(relative).parent.as_posix()
            for _, relative, _ in bound_artifacts
        }
        require(
            len(artifact_parents) == 1
            and all(
                PurePosixPath(relative).name
                == HUMAN_ARTIFACT_FILENAMES[kind]
                for kind, relative, _ in bound_artifacts
            ),
            "participant artifacts do not share the canonical filenames/directory",
        )
        artifact_parent = next(iter(artifact_parents))
        require(
            re.fullmatch(
                (
                    r"tests/codex/acquisition/sprint11/human/"
                    r"s11u-run-[0-9a-f]{32}"
                ),
                artifact_parent,
            )
            is not None,
            "participant artifact directory is outside canonical acquisition scope",
        )
        validated_human_trace: Mapping[str, Any] | None = None
        if qualifying:
            require(
                repository is not None and source_commit is not None,
                "qualifying usability artifacts lack Git authority",
            )
            artifact_blobs: dict[str, bytes] = {}
            artifact_documents: dict[str, dict[str, Any]] = {}
            for kind, relative, digest_value in bound_artifacts:
                blob = repository.blob_at(source_commit, relative)
                maximum = (
                    MAX_HUMAN_AUTHORITY_BYTES
                    if kind == "authority"
                    else MAX_ARTIFACT_BYTES
                )
                require(
                    0 < len(blob) <= maximum
                    and sha256_bytes(blob) == digest_value,
                    f"participant {kind} Git binding differs",
                )
                artifact_blobs[kind] = blob
                artifact_documents[kind] = strict_json_bytes(
                    blob,
                    label=f"participant {kind}",
                    maximum_bytes=maximum,
                )
                safe_evidence_scan(artifact_documents[kind])
            exact_bundle_digests = {
                kind: cast(str, participant[f"{kind}_sha256"])
                for kind in ("trace", "rubric", "consent", "defect_ledger")
            }
            try:
                authority = (
                    human_usability.validate_source_bound_acquisition_authority(
                        artifact_blobs["authority"],
                        source_commit=source_commit,
                        relative_path=next(
                            relative
                            for kind, relative, _ in bound_artifacts
                            if kind == "authority"
                        ),
                        expected_sha256=cast(
                            str,
                            participant["authority_sha256"],
                        ),
                    )
                )
                qualifies = human_usability.validate_bundle(
                    trace_document=artifact_documents["trace"],
                    rubric_document=artifact_documents["rubric"],
                    consent_document=artifact_documents["consent"],
                    defect_document=artifact_documents["defect_ledger"],
                    repository=REPOSITORY_ROOT,
                    allow_real_human=True,
                    source_bound_authority=authority,
                    artifact_sha256=exact_bundle_digests,
                )
            except human_usability.AcquisitionError as error:
                raise AcceptanceError(
                    "participant human acquisition bundle differs"
                ) from error
            require(
                qualifies,
                "participant human acquisition bundle is nonqualifying",
            )
            authority_observations = cast(
                Mapping[str, Any],
                authority.payload["observations"],
            )
            require(
                authority.payload["participant_id"] == participant_id
                and authority.payload["artifact_directory"] == artifact_parent
                and authority_observations["implementation_independence"]
                == participant["external_attestation_independence"],
                "participant external authority projection differs",
            )
            validated_human_trace = cast(
                Mapping[str, Any],
                artifact_documents["trace"]["payload"],
            )
            primary_bindings = cast(
                Mapping[str, Any],
                validated_human_trace["bindings"],
            )
            primary_package = cast(
                Mapping[str, Any],
                primary_bindings["package"],
            )
            require(
                primary_package["source_commit"]
                == report["package_source_commit"]
                and primary_package["manifest_sha256"]
                == report["package_manifest_sha256"]
                and primary_bindings["prompt_pack"] == prompt,
                "participant primary bundle package/prompt binding differs",
            )
        if artifact_paths is not None:
            artifact_paths.update(relative for _, relative, _ in bound_artifacts)
        tasks = participant["tasks"]
        require(
            isinstance(tasks, list) and len(tasks) == len(REQUIRED_USABILITY_GOALS),
            "participant task count differs",
        )
        task_ids: set[str] = set()
        task_summaries: dict[str, Mapping[str, Any]] = {}
        for task in tasks:
            task_record = _exact_fields(
                task,
                {
                    "goal_id",
                    "status",
                    "duration_seconds",
                    "wrong_turns",
                    "help_used",
                    "remediation_succeeded",
                },
                label="usability task",
            )
            goal_id = task_record["goal_id"]
            require(
                goal_id in REQUIRED_USABILITY_GOALS
                and goal_id not in task_ids
                and task_record["status"] == "passed"
                and isinstance(task_record["duration_seconds"], (int, float))
                and not isinstance(task_record["duration_seconds"], bool)
                and 0 <= task_record["duration_seconds"] <= 3_600
                and isinstance(task_record["wrong_turns"], int)
                and not isinstance(task_record["wrong_turns"], bool)
                and 0 <= task_record["wrong_turns"] <= 100
                and isinstance(task_record["help_used"], bool)
                and isinstance(task_record["remediation_succeeded"], bool),
                "participant task result differs",
            )
            task_ids.add(cast(str, goal_id))
            task_summaries[cast(str, goal_id)] = task_record
        require(
            task_ids == REQUIRED_USABILITY_GOALS,
            "participant task coverage differs",
        )
        if validated_human_trace is not None:
            primary_tasks = {
                cast(str, task["goal_id"]): task
                for task in cast(
                    list[Mapping[str, Any]],
                    validated_human_trace["tasks"],
                )
            }
            require(
                set(primary_tasks) == task_ids,
                "aggregate tasks differ from primary human trace",
            )
            for goal_id, summary in task_summaries.items():
                primary = primary_tasks[goal_id]
                require(
                    summary["status"] == primary["status"]
                    and summary["duration_seconds"]
                    == cast(int, primary["duration_ms"]) / 1000
                    and summary["wrong_turns"] == primary["wrong_turns"]
                    and summary["help_used"]
                    is bool(cast(list[Any], primary["help_source_ids"]))
                    and summary["remediation_succeeded"]
                    is primary["remediation_succeeded"],
                    "aggregate task projection differs from primary trace",
                )
        doctor_faults = participant["doctor_faults"]
        require(
            isinstance(doctor_faults, list)
            and len(doctor_faults) == len(USABILITY_DOCTOR_FAULTS),
            "participant doctor fault count differs",
        )
        observed_faults: dict[str, tuple[str, str]] = {}
        for fault in doctor_faults:
            record = _exact_fields(
                fault,
                {
                    "scenario",
                    "diagnostic_code",
                    "remediation_id",
                    "status",
                    "remediation_succeeded",
                },
                label="participant doctor fault",
            )
            scenario = record["scenario"]
            require(
                isinstance(scenario, str)
                and scenario in USABILITY_DOCTOR_FAULTS
                and scenario not in observed_faults
                and record["status"] == "passed"
                and record["remediation_succeeded"] is True,
                "participant doctor fault result differs",
            )
            observed_faults[scenario] = (
                cast(str, record["diagnostic_code"]),
                cast(str, record["remediation_id"]),
            )
        require(
            observed_faults == USABILITY_DOCTOR_FAULTS,
            "participant doctor fault mapping differs",
        )
        if validated_human_trace is not None:
            primary_faults = {
                cast(str, fault["scenario"]): fault
                for fault in cast(
                    list[Mapping[str, Any]],
                    validated_human_trace["doctor_faults"],
                )
            }
            require(
                set(primary_faults) == set(observed_faults),
                "aggregate doctor faults differ from primary trace",
            )
            for scenario, (diagnostic_code, remediation_id) in (
                observed_faults.items()
            ):
                primary = primary_faults[scenario]
                require(
                    primary["observed_diagnostic_code"] == diagnostic_code
                    and primary["observed_remediation_id"] == remediation_id
                    and primary["status"] == "passed"
                    and primary["remediation_succeeded"] is True,
                    "aggregate doctor-fault projection differs from primary trace",
                )
    require(
        independent >= 1,
        "no externally attested implementation-independent participant",
    )
    metrics = _exact_fields(
        report["metrics"],
        {
            "first_useful_status_seconds",
            "connected_live_query_seconds",
            "secret_copy_count",
            "manual_toml_steps",
            "project_integrity_failures",
            "project_isolation_failures",
            "fault_recoveries",
        },
        label="usability metrics",
    )
    require(
        isinstance(metrics["first_useful_status_seconds"], (int, float))
        and not isinstance(metrics["first_useful_status_seconds"], bool)
        and 0 <= metrics["first_useful_status_seconds"] <= 300
        and isinstance(metrics["connected_live_query_seconds"], (int, float))
        and not isinstance(metrics["connected_live_query_seconds"], bool)
        and 0 <= metrics["connected_live_query_seconds"] <= 900
        and metrics["secret_copy_count"] == 0
        and metrics["manual_toml_steps"] == 0
        and metrics["project_integrity_failures"] == 0
        and metrics["project_isolation_failures"] == 0
        and isinstance(metrics["fault_recoveries"], int)
        and not isinstance(metrics["fault_recoveries"], bool)
        and metrics["fault_recoveries"]
        == len(participants) * len(USABILITY_DOCTOR_FAULTS),
        "usability target metrics differ",
    )
    defects = report["defects"]
    require(isinstance(defects, list) and len(defects) <= 128, "defect bound differs")
    defect_ids: set[str] = set()
    for defect in defects:
        record = _exact_fields(
            defect,
            {"id", "severity", "status", "retest_passed"},
            label="usability defect",
        )
        defect_id = _bounded_string(record["id"], label="defect ID", maximum=64)
        require(
            defect_id not in defect_ids
            and record["severity"] in {"low", "medium", "high", "critical"}
            and record["status"] in {"open", "resolved"},
            "usability defect projection differs",
        )
        defect_ids.add(defect_id)
        if record["severity"] in {"high", "critical"}:
            require(
                record["status"] == "resolved"
                and record["retest_passed"] is True,
                "high/critical usability defect lacks a passing retest",
            )
        else:
            require(
                isinstance(record["retest_passed"], bool),
                "defect retest flag differs",
            )
    safe_evidence_scan(report)
    return dict(report)


def _preacquired_json(
    binding: Mapping[str, Any],
    *,
    path_field: str,
    digest_field: str,
    source_commit: str,
    repository: GitRepository,
    maximum_bytes: int,
) -> dict[str, Any]:
    relative = _safe_relative_path(binding[path_field], label="acquired artifact")
    digest = _digest(binding[digest_field], label="acquired artifact")
    require(
        relative
        != "tests/codex/evidence/sprint-11-external-codex-beta-macos.json",
        "final evidence cannot be its own acquired input",
    )
    blob = repository.blob_at(source_commit, relative)
    require(
        sha256_bytes(blob) == digest,
        "pre-acquired artifact digest differs at source commit",
    )
    return strict_json_bytes(
        blob,
        label=PurePosixPath(relative).name,
        maximum_bytes=maximum_bytes,
    )


def validate_evidence(
    value: Any,
    *,
    repository: GitRepository,
    evidence_path: Path = EVIDENCE_PATH,
    artifact_root: Path | None,
    check_checkout: bool,
    observed_gates: Mapping[str, Mapping[str, Any]] | None,
) -> dict[str, Any]:
    require(
        artifact_root is not None,
        "qualifying evidence requires the detached package artifact root",
    )
    evidence = _exact_fields(value, EVIDENCE_FIELDS, label="Sprint 11 evidence")
    require(
        len(canonical_json(evidence)) <= MAX_EVIDENCE_BYTES,
        "Sprint 11 evidence exceeds byte bound",
    )
    require(
        evidence["schema_version"] == "s11-external-codex-beta-evidence/1.0"
        and evidence["sprint"] == 11
        and evidence["profile"] == "external_codex_beta_macos_arm64"
        and evidence["status"] == "passed",
        "Sprint 11 evidence identity differs",
    )
    baseline = _exact_fields(
        evidence["baseline"],
        {
            "sprint10_source_commit",
            "sprint10_evidence_commit",
            "sprint10_evidence_sha256",
            "sprint10_source_sha256",
        },
        label="Sprint 10 baseline",
    )
    require(
        baseline
        == {
            "sprint10_source_commit": SPRINT10_SOURCE_COMMIT,
            "sprint10_evidence_commit": SPRINT10_EVIDENCE_COMMIT,
            "sprint10_evidence_sha256": SPRINT10_EVIDENCE_SHA256,
            "sprint10_source_sha256": SPRINT10_SOURCE_SHA256,
        },
        "immutable Sprint 10 baseline declaration differs",
    )
    validate_sprint10_baseline(repository)
    source_commit = validate_source_binding(evidence["source"], repository)
    if check_checkout:
        validate_evidence_checkout_relation(
            source_commit,
            evidence_path,
            repository,
        )
    protocols = _exact_fields(
        evidence["protocols"],
        {
            "bridge_rpc",
            "mcp",
            "approval_floor",
            "tools",
            "fixed_resources",
            "resource_templates",
        },
        label="protocols",
    )
    require(
        protocols
        == {
            "bridge_rpc": "1.8",
            "mcp": "2025-11-25",
            "approval_floor": "2025-06-18",
            "tools": 41,
            "fixed_resources": 4,
            "resource_templates": 1,
        },
        "protocol/profile coordinates differ",
    )
    prompt_binding = _exact_fields(
        evidence["prompt_pack"],
        {"id", "path", "sha256"},
        label="evidence prompt pack",
    )
    require(
        prompt_binding["id"] == "sprint11-external-beta-v1"
        and prompt_binding["path"]
        == "tests/codex/prompts/sprint11-external-beta-v1.json",
        "evidence prompt-pack identity differs",
    )
    prompt = _preacquired_json(
        prompt_binding,
        path_field="path",
        digest_field="sha256",
        source_commit=source_commit,
        repository=repository,
        maximum_bytes=MAX_ARTIFACT_BYTES,
    )
    validate_prompt_pack(prompt)
    registry_binding = _exact_fields(
        evidence["registry"],
        {
            "path",
            "sha256",
            "tools",
            "fixed_resources",
            "resource_templates",
        },
        label="evidence registry",
    )
    require(
        registry_binding["path"]
        == "godot-codex-mcp/product/registry-profile.v1.json"
        and registry_binding["tools"] == 41
        and registry_binding["fixed_resources"] == 4
        and registry_binding["resource_templates"] == 1,
        "evidence registry path/counts differ",
    )
    registry_document = _preacquired_json(
        registry_binding,
        path_field="path",
        digest_field="sha256",
        source_commit=source_commit,
        repository=repository,
        maximum_bytes=MAX_ARTIFACT_BYTES,
    )
    registry = validate_registry_document(registry_document)
    compatibility_binding = _exact_fields(
        evidence["compatibility"],
        {
            "matrix_path",
            "matrix_sha256",
            "host_profile_path",
            "host_profile_sha256",
            "server_instructions_path",
            "server_instructions_file_sha256",
            "server_instructions_wire_sha256",
        },
        label="evidence compatibility authority",
    )
    require(
        compatibility_binding["matrix_path"]
        == "godot-codex-mcp/product/compatibility-matrix.v1.json"
        and compatibility_binding["host_profile_path"]
        == "godot-codex-mcp/product/host-coordinate-profile.v1.json"
        and compatibility_binding["server_instructions_path"]
        == "godot-codex-mcp/product/server-instructions.v1.txt",
        "compatibility authority paths differ",
    )
    matrix_document = _preacquired_json(
        {
            "path": compatibility_binding["matrix_path"],
            "sha256": compatibility_binding["matrix_sha256"],
        },
        path_field="path",
        digest_field="sha256",
        source_commit=source_commit,
        repository=repository,
        maximum_bytes=MAX_ARTIFACT_BYTES,
    )
    require(
        matrix_document.get("schema_version")
        == "godot-codex-compatibility-matrix/1.0",
        "compatibility matrix identity differs",
    )
    host_profile_document = _preacquired_json(
        {
            "path": compatibility_binding["host_profile_path"],
            "sha256": compatibility_binding["host_profile_sha256"],
        },
        path_field="path",
        digest_field="sha256",
        source_commit=source_commit,
        repository=repository,
        maximum_bytes=MAX_ARTIFACT_BYTES,
    )
    host_profile = validate_host_coordinate_profile(
        host_profile_document,
        qualifying=True,
    )
    instructions_blob = repository.blob_at(
        source_commit,
        cast(str, compatibility_binding["server_instructions_path"]),
    )
    require(
        sha256_bytes(instructions_blob)
        == compatibility_binding["server_instructions_file_sha256"]
        and instructions_blob.endswith(b"\n")
        and sha256_bytes(instructions_blob[:-1])
        == compatibility_binding["server_instructions_wire_sha256"]
        and {
            "file_sha256": compatibility_binding[
                "server_instructions_file_sha256"
            ],
            "wire_sha256": compatibility_binding[
                "server_instructions_wire_sha256"
            ],
        }
        == canonical_server_instructions(),
        "server-instructions Git/wire binding differs",
    )
    validate_rendered_host_authority(host_profile)
    validate_sprint_plan_assertion_authority()
    validate_documented_host_acquisition()
    package_binding = _exact_fields(
        evidence["package"],
        {
            "source_commit",
            "manifest_path",
            "manifest_sha256",
            "archive_sha256",
            "godot_prerequisite_sha256",
        },
        label="evidence package",
    )
    package_source = _commit(
        package_binding["source_commit"],
        label="package source",
    )
    require(
        repository.is_ancestor(package_source, source_commit),
        "package source is not an ancestor of evidence source",
    )
    require(
        package_binding["godot_prerequisite_sha256"]
        == EXACT_GODOT_PREREQUISITE_SHA256,
        "evidence Godot prerequisite differs",
    )
    package_manifest = _preacquired_json(
        package_binding,
        path_field="manifest_path",
        digest_field="manifest_sha256",
        source_commit=source_commit,
        repository=repository,
        maximum_bytes=MAX_PACKAGE_MANIFEST_BYTES,
    )
    package = validate_detached_package_manifest(
        package_manifest,
        manifest_relative_path=cast(str, package_binding["manifest_path"]),
        artifact_root=artifact_root,
        expected_source_commit=package_source,
    )
    archive = cast(Mapping[str, Any], package["archive"])
    require(
        package["registry_sha256"] == registry_binding["sha256"]
        and package["compatibility_matrix_sha256"]
        == compatibility_binding["matrix_sha256"]
        and archive["sha256"] == package_binding["archive_sha256"],
        "package registry/archive binding differs",
    )
    package_sidecar = package_content_record(
        package,
        packaged_regressions.PACKAGE_SIDECAR_PATH,
    )
    package_sidecar_sha256 = _digest(
        package_sidecar["sha256"],
        label="package sidecar",
    )
    regression_binding = _exact_fields(
        evidence["regressions"],
        {
            "receipt_path",
            "receipt_sha256",
            "package_manifest_sha256",
            "package_sidecar_sha256",
            "godot_artifact_sha256",
        },
        label="packaged regression evidence",
    )
    require(
        regression_binding["package_manifest_sha256"]
        == package_binding["manifest_sha256"]
        and regression_binding["package_sidecar_sha256"]
        == package_sidecar_sha256
        and regression_binding["godot_artifact_sha256"]
        == package_binding["godot_prerequisite_sha256"],
        "packaged regression evidence binding differs",
    )
    regression_receipt = _preacquired_json(
        regression_binding,
        path_field="receipt_path",
        digest_field="receipt_sha256",
        source_commit=source_commit,
        repository=repository,
        maximum_bytes=packaged_regressions.MAX_RECEIPT_BYTES,
    )
    acquisition_paths = validate_packaged_regression_receipt(
        regression_receipt,
        repository=repository,
        source_commit=source_commit,
        package_source_commit=package_source,
        package_manifest_sha256=cast(
            str,
            package_binding["manifest_sha256"],
        ),
        package_archive_sha256=cast(str, package_binding["archive_sha256"]),
        package=package,
        registry_sha256=cast(str, registry_binding["sha256"]),
        godot_artifact_sha256=cast(
            str,
            package_binding["godot_prerequisite_sha256"],
        ),
    )
    acquisition_paths.add(
        _safe_relative_path(
            regression_binding["receipt_path"],
            label="packaged regression receipt",
        )
    )
    acquisitions_binding = _exact_fields(
        evidence["acquisitions"],
        {"host_provenance", "multi_project", "reproducibility"},
        label="external acquisition bindings",
    )
    host_provenance_binding = _exact_fields(
        acquisitions_binding["host_provenance"],
        {
            "receipt_path",
            "receipt_sha256",
            "measurement_path",
            "measurement_sha256",
            "source_commit",
        },
        label="host provenance acquisition binding",
    )
    require(
        host_provenance_binding["source_commit"] == package_source,
        "host provenance acquisition source differs from package source",
    )
    multi_binding = _exact_fields(
        acquisitions_binding["multi_project"],
        {"receipt_path", "receipt_sha256"},
        label="multi-project acquisition binding",
    )
    reproducibility_binding = _exact_fields(
        acquisitions_binding["reproducibility"],
        {"receipt_path", "receipt_sha256"},
        label="reproducibility acquisition binding",
    )
    expected_receipt_bindings = _expected_package_receipt_bindings(
        package_source_commit=package_source,
        package_manifest_sha256=cast(str, package_binding["manifest_sha256"]),
        package_archive_sha256=cast(str, package_binding["archive_sha256"]),
        package=package,
        registry_sha256=cast(str, registry_binding["sha256"]),
        godot_artifact_sha256=cast(
            str,
            package_binding["godot_prerequisite_sha256"],
        ),
    )
    host_provenance_envelope = _preacquired_json(
        host_provenance_binding,
        path_field="receipt_path",
        digest_field="receipt_sha256",
        source_commit=source_commit,
        repository=repository,
        maximum_bytes=external_acquisitions.MAX_RECEIPT_BYTES,
    )
    host_paths, host_measurements = validate_host_provenance_acquisition(
        host_provenance_envelope,
        evidence_binding=host_provenance_binding,
        repository=repository,
        source_commit=source_commit,
        expected_source_commit=package_source,
        expected_profile_sha256=cast(
            str,
            compatibility_binding["host_profile_sha256"],
        ),
    )
    acquisition_paths.update(host_paths)
    multi_receipt = _preacquired_json(
        multi_binding,
        path_field="receipt_path",
        digest_field="receipt_sha256",
        source_commit=source_commit,
        repository=repository,
        maximum_bytes=external_acquisitions.MAX_RECEIPT_BYTES,
    )
    acquisition_paths.update(
        validate_multi_project_receipt(
            multi_receipt,
            repository=repository,
            source_commit=source_commit,
            expected_bindings=expected_receipt_bindings,
        )
    )
    reproducibility_receipt = _preacquired_json(
        reproducibility_binding,
        path_field="receipt_path",
        digest_field="receipt_sha256",
        source_commit=source_commit,
        repository=repository,
        maximum_bytes=external_acquisitions.MAX_RECEIPT_BYTES,
    )
    try:
        qualified_package_tree_sha256 = (
            external_acquisitions._package_tree_digest(
                artifact_root.resolve(),
                package,
            )
        )
    except external_acquisitions.AcquisitionError as error:
        raise AcceptanceError(
            "qualified package tree binding is unavailable"
        ) from error
    acquisition_paths.update(
        validate_reproducibility_receipt(
            reproducibility_receipt,
            repository=repository,
            expected_source_commit=package_source,
            godot_artifact_sha256=cast(
                str,
                package_binding["godot_prerequisite_sha256"],
            ),
            qualified_reference={
                "qualified_manifest_sha256": package_binding[
                    "manifest_sha256"
                ],
                "qualified_archive_sha256": archive["sha256"],
                "qualified_archive_bytes": archive["bytes"],
                "qualified_package_tree_sha256":
                    qualified_package_tree_sha256,
                "qualified_build_provenance_sha256": sha256_bytes(
                    canonical_json(package["build_provenance"])
                ),
            },
        )
    )
    for binding in (
        host_provenance_binding,
        multi_binding,
        reproducibility_binding,
    ):
        acquisition_paths.add(
            _safe_relative_path(
                binding["receipt_path"],
                label="external acquisition receipt",
            )
        )
    acquisition_paths.add(
        _safe_relative_path(
            package_binding["manifest_path"],
            label="package manifest",
        )
    )
    surface_bindings = _exact_fields(
        evidence["surfaces"],
        {"app", "cli", "ide"},
        label="surface evidence",
    )
    traces: dict[str, Any] = {}
    recorder_journals: dict[str, Mapping[str, Any]] = {}
    recorder_journal_digests: dict[str, str] = {}
    surface_authorities: dict[str, Mapping[str, Any]] = {}
    for surface in ("app", "cli", "ide"):
        record = _exact_fields(
            surface_bindings[surface],
            {
                "status",
                "host_name",
                "host_version",
                "host_build",
                "host_artifact_sha256",
                "tool_artifact_sha256",
                "trace_path",
                "trace_sha256",
                "recorder_journal_path",
                "recorder_journal_sha256",
                "authority_path",
                "authority_sha256",
            },
            label=f"{surface} evidence",
        )
        require(record["status"] == "passed", "required surface did not pass")
        for field in ("host_name", "host_version", "host_build"):
            _bounded_string(
                record[field],
                label=f"{surface} {field}",
                maximum=128,
            )
        _digest(record["host_artifact_sha256"], label=f"{surface} host")
        _digest(record["tool_artifact_sha256"], label=f"{surface} tool")
        journal = _preacquired_json(
            record,
            path_field="recorder_journal_path",
            digest_field="recorder_journal_sha256",
            source_commit=source_commit,
            repository=repository,
            maximum_bytes=MAX_RECORDER_JOURNAL_BYTES,
        )
        trace = _preacquired_json(
            record,
            path_field="trace_path",
            digest_field="trace_sha256",
            source_commit=source_commit,
            repository=repository,
            maximum_bytes=MAX_TRACE_BYTES,
        )
        authority_document = _preacquired_json(
            record,
            path_field="authority_path",
            digest_field="authority_sha256",
            source_commit=source_commit,
            repository=repository,
            maximum_bytes=MAX_ARTIFACT_BYTES,
        )
        authority = validate_surface_acquisition_authority(
            authority_document,
            surface=surface,
            authority_path=cast(str, record["authority_path"]),
            authority_sha256=cast(str, record["authority_sha256"]),
            trace_path=cast(str, record["trace_path"]),
            trace_sha256=cast(str, record["trace_sha256"]),
            recorder_journal_path=cast(
                str,
                record["recorder_journal_path"],
            ),
            recorder_journal_sha256=cast(
                str,
                record["recorder_journal_sha256"],
            ),
            trace=cast(Mapping[str, Any], trace),
        )
        validated = validate_surface_trace(
            trace,
            qualifying=True,
            recorder_journal=journal,
            recorder_journal_digest=cast(
                str,
                record["recorder_journal_sha256"],
            ),
            source_bound_authority=authority,
        )
        require(validated["surface"] == surface, "surface trace binding differs")
        host = cast(Mapping[str, Any], validated["host"])
        bindings = cast(Mapping[str, Any], validated["bindings"])
        trace_registry = cast(Mapping[str, Any], validated["registry"])
        measured_host = host_measurements[surface]
        require(
            host["name"] == record["host_name"]
            and host["version"] == record["host_version"]
            and host["build"] == record["host_build"]
            and bindings["host_artifact_sha256"]
            == record["host_artifact_sha256"]
            and bindings["mcp_binary_sha256"]
            == record["tool_artifact_sha256"]
            and record["tool_artifact_sha256"] == package_sidecar_sha256
            and bindings["package_source_commit"] == package_source
            and bindings["package_manifest_sha256"]
            == package_binding["manifest_sha256"]
            and bindings["registry_sha256"] == registry_binding["sha256"]
            and bindings["prompt_pack_sha256"] == prompt_binding["sha256"]
            and bindings["godot_artifact_sha256"]
            == package_binding["godot_prerequisite_sha256"]
            and bindings["host_provenance_sha256"]
            == host_provenance_binding["measurement_sha256"]
            and bindings["host_artifact_sha256"]
            == measured_host["host_artifact_sha256"]
            and bindings["client_artifact_sha256"]
            == measured_host["client_artifact_sha256"]
            and host["host_metadata_sha256"]
            == measured_host["host_metadata_sha256"]
            and host["host_code_signature"]
            == measured_host["host_code_signature"]
            and host["client_code_signature"]
            == measured_host["client_code_signature"]
            and host["ide_shell_artifact_sha256"]
            == measured_host["ide_shell_artifact_sha256"]
            and host["ide_shell_code_signature"]
            == measured_host["ide_shell_code_signature"]
            and trace_registry["tools"] == registry["tools"]
            and trace_registry["fixed_resources"] == registry["fixed_resources"]
            and trace_registry["resource_templates"]
            == registry["resource_templates"],
            "surface trace host/tool/source binding differs",
        )
        traces[surface] = validated
        recorder_journals[surface] = journal
        recorder_journal_digests[surface] = cast(
            str,
            record["recorder_journal_sha256"],
        )
        surface_authorities[surface] = authority
        acquisition_paths.add(
            _safe_relative_path(
                record["trace_path"],
                label=f"{surface} trace",
            )
        )
        acquisition_paths.add(
            _safe_relative_path(
                record["recorder_journal_path"],
                label=f"{surface} recorder journal",
            )
        )
        acquisition_paths.add(
            _safe_relative_path(
                record["authority_path"],
                label=f"{surface} acquisition authority",
            )
        )
    compare_surface_traces(
        traces,
        qualifying=True,
        recorder_journals=recorder_journals,
        recorder_journal_digests=recorder_journal_digests,
        source_bound_authorities=surface_authorities,
    )
    usability_binding = _exact_fields(
        evidence["usability"],
        USABILITY_EVIDENCE_FIELDS,
        label="usability evidence",
    )
    require(
        usability_binding["status"] == "passed"
        and isinstance(usability_binding["participants"], int)
        and usability_binding["participants"] >= 3
        and usability_binding[
            "externally_attested_independent_participant"
        ]
        is True,
        "usability evidence summary differs",
    )
    usability_document = _preacquired_json(
        usability_binding,
        path_field="report_path",
        digest_field="report_sha256",
        source_commit=source_commit,
        repository=repository,
        maximum_bytes=MAX_ARTIFACT_BYTES,
    )
    usability = validate_usability_report(
        usability_document,
        qualifying=True,
        repository=repository,
        source_commit=source_commit,
        artifact_paths=acquisition_paths,
    )
    acquisition_paths.add(
        _safe_relative_path(
            usability_binding["report_path"],
            label="usability report",
        )
    )
    participants = cast(list[Any], usability["participants"])
    require(
        len(participants) == usability_binding["participants"]
        and any(
            isinstance(item, dict)
            and item.get("external_attestation_independence") == "independent"
            for item in participants
        )
        and usability["package_source_commit"] == package_source
        and usability["package_manifest_sha256"]
        == package_binding["manifest_sha256"]
        and usability["prompt_pack"] == prompt_binding,
        "usability report source/package/prompt binding differs",
    )
    validate_post_package_changed_paths(
        repository=repository,
        package_source_commit=package_source,
        source_commit=source_commit,
        acquisition_paths=acquisition_paths,
    )
    gates = evidence["gates"]
    require(
        isinstance(gates, dict) and set(gates) == REQUIRED_GATES,
        "acceptance gate set differs",
    )
    require(
        observed_gates is not None
        and set(observed_gates) == AUTOMATED_GATE_NAMES,
        "qualifying evidence was not preceded by every automated gate",
    )
    for name, observation in observed_gates.items():
        observed = _exact_fields(
            observation,
            {
                "status",
                "definition_sha256",
                "receipt_sha256",
                "duration_ms",
            },
            label=f"observed gate {name}",
        )
        require(
            observed["status"] == "passed"
            and observed["definition_sha256"] == gate_definition_sha256(name)
            and DIGEST_RE.fullmatch(cast(str, observed["receipt_sha256"]))
            is not None
            and isinstance(observed["duration_ms"], int)
            and not isinstance(observed["duration_ms"], bool)
            and observed["duration_ms"] >= 0,
            "automated gate observation differs",
        )
    require(
        gates == canonical_gate_bindings(),
        "evidence gate definitions differ from the executed/validated gates",
    )
    release = _exact_fields(
        evidence["release_projection"],
        {"external_codex_beta_macos", "r1_08", "r1_10", "beta_gate"},
        label="release projection",
    )
    require(
        release
        == {
            "external_codex_beta_macos": "passed",
            "r1_08": "partial",
            "r1_10": "partial",
            "beta_gate": "not_reached",
        },
        "release projection overclaims Sprint 11",
    )
    redaction = _exact_fields(
        evidence["redaction"],
        {
            "absolute_paths_absent",
            "accounts_absent",
            "approval_content_absent",
            "native_ids_absent",
            "participant_prompts_absent",
            "secrets_absent",
            "source_content_absent",
        },
        label="evidence redaction",
    )
    require(all(item is True for item in redaction.values()), "redaction differs")
    cleanup = _exact_fields(
        evidence["cleanup"],
        {
            "editor_processes_stopped",
            "game_processes_stopped",
            "sidecar_processes_stopped",
            "temporary_workspaces_removed",
            "unexpected_listeners_absent",
        },
        label="cleanup",
    )
    require(all(item is True for item in cleanup.values()), "cleanup differs")
    deferred = _exact_fields(
        evidence["deferred"],
        {"cursor", "dock", "linux", "notarization", "remote_ci", "windows"},
        label="deferred coordinates",
    )
    for item in deferred.values():
        record = _exact_fields(
            item,
            {"status", "reason"},
            label="deferred coordinate",
        )
        require(
            record["status"] == "not_run"
            and isinstance(record["reason"], str)
            and 0 < len(record["reason"].encode("utf-8")) <= 256,
            "deferred coordinate overclaims coverage",
        )
    safe_evidence_scan(evidence)
    return dict(evidence)


def validate_contract_files(repository: GitRepository) -> dict[str, Any]:
    validate_sprint10_baseline(repository)
    parse_source_scopes(SOURCE_SCOPE_PATH.read_text(encoding="utf-8"))
    prompt = strict_json_load(PROMPT_PACK_PATH)
    validate_prompt_pack(prompt)
    try:
        human_usability.validate_frozen_materials(REPOSITORY_ROOT)
    except human_usability.AcquisitionError as error:
        raise AcceptanceError("human usability source contracts differ") from error
    registry = canonical_registry_profile()
    host_profile = validate_host_coordinate_profile(
        strict_json_load(HOST_PROFILE_PATH),
        qualifying=False,
    )
    validate_rendered_host_authority(host_profile)
    validate_sprint_plan_assertion_authority()
    documented_host_acquisition_sha256 = validate_documented_host_acquisition()
    for path, expected_id in (
        (
            COMPATIBILITY_SCHEMA_PATH,
            "https://godot-codex.local/schemas/compatibility-matrix-v1.json",
        ),
        (
            EVIDENCE_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-evidence.schema.json",
        ),
        (
            TRACE_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-surface-trace.schema.json",
        ),
        (
            SURFACE_AUTHORITY_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-surface-acquisition-authority.schema.json",
        ),
        (
            PACKAGED_REGRESSION_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-packaged-regression-receipt.schema.json",
        ),
        (
            MULTI_PROJECT_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-multi-project-receipt.schema.json",
        ),
        (
            MULTI_PROJECT_REPORT_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-multi-project-report.schema.json",
        ),
        (
            REPRODUCIBILITY_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-reproducibility-receipt.schema.json",
        ),
        (
            USABILITY_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-usability-report.schema.json",
        ),
        (
            HOST_PROFILE_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "host-coordinate-profile.schema.json",
        ),
        (
            HOST_PROVENANCE_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-host-provenance.schema.json",
        ),
        (
            HOST_PROVENANCE_ACQUISITION_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-host-provenance-acquisition.schema.json",
        ),
        (
            HUMAN_TRACE_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-human-usability-trace.schema.json",
        ),
        (
            HUMAN_AUTHORITY_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-human-acquisition-authority.schema.json",
        ),
        (
            HUMAN_RUBRIC_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-human-usability-rubric.schema.json",
        ),
        (
            HUMAN_CONSENT_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-human-consent-receipt.schema.json",
        ),
        (
            HUMAN_DEFECT_SCHEMA_PATH,
            "https://godot-codex.invalid/schemas/godot_codex/"
            "sprint11-human-usability-defect-ledger.schema.json",
        ),
    ):
        schema = strict_json_load(path)
        require(
            schema.get("$schema")
            == "https://json-schema.org/draft/2020-12/schema"
            and schema.get("$id") == expected_id
            and schema.get("additionalProperties") is False,
            "Sprint 11 JSON Schema envelope differs",
        )
    evidence_schema = strict_json_load(EVIDENCE_SCHEMA_PATH)
    require(
        set(evidence_schema["required"]) == EVIDENCE_FIELDS
        and set(evidence_schema["properties"]) == EVIDENCE_FIELDS,
        "evidence schema/Python root fields differ",
    )
    usability_evidence_schema = evidence_schema["properties"]["usability"]
    require(
        set(usability_evidence_schema["required"])
        == USABILITY_EVIDENCE_FIELDS
        and set(usability_evidence_schema["properties"])
        == USABILITY_EVIDENCE_FIELDS,
        "evidence usability schema/Python fields differ",
    )
    usability_schema = strict_json_load(USABILITY_SCHEMA_PATH)
    usability_participant_schema = usability_schema["$defs"]["participant"]
    require(
        set(usability_schema["required"]) == USABILITY_FIELDS
        and set(usability_schema["properties"]) == USABILITY_FIELDS
        and set(usability_participant_schema["required"])
        == USABILITY_PARTICIPANT_FIELDS
        and set(usability_participant_schema["properties"])
        == USABILITY_PARTICIPANT_FIELDS,
        "usability report schema/Python fields differ",
    )
    acquisitions_schema = evidence_schema["properties"]["acquisitions"]
    acquisition_fields = {"host_provenance", "multi_project", "reproducibility"}
    host_binding_fields = {
        "receipt_path",
        "receipt_sha256",
        "measurement_path",
        "measurement_sha256",
        "source_commit",
    }
    host_binding_schema = evidence_schema["$defs"][
        "host_provenance_acquisition"
    ]
    require(
        set(acquisitions_schema["required"]) == acquisition_fields
        and set(acquisitions_schema["properties"]) == acquisition_fields
        and set(host_binding_schema["required"]) == host_binding_fields
        and set(host_binding_schema["properties"]) == host_binding_fields,
        "evidence acquisition schema/Python fields differ",
    )
    surface_fields = {
        "status",
        "host_name",
        "host_version",
        "host_build",
        "host_artifact_sha256",
        "tool_artifact_sha256",
        "trace_path",
        "trace_sha256",
        "recorder_journal_path",
        "recorder_journal_sha256",
        "authority_path",
        "authority_sha256",
    }
    surface_schema = evidence_schema["$defs"]["surface"]
    require(
        set(surface_schema["required"]) == surface_fields
        and set(surface_schema["properties"]) == surface_fields,
        "surface evidence schema/Python fields differ",
    )
    envelope_schema = strict_json_load(
        HOST_PROVENANCE_ACQUISITION_SCHEMA_PATH
    )
    envelope_bindings = envelope_schema["$defs"]["bindings"]
    expected_envelope_bindings = {
        "runner_path",
        "runner_sha256",
        "measurement_schema_path",
        "measurement_schema_sha256",
        "acquisition_schema_path",
        "acquisition_schema_sha256",
        "host_coordinate_profile_path",
        "host_coordinate_profile_sha256",
        "measurement_path",
        "measurement_sha256",
    }
    require(
        set(envelope_bindings["required"]) == expected_envelope_bindings
        and set(envelope_bindings["properties"]) == expected_envelope_bindings
        and envelope_schema["$defs"]["command"]["properties"][
            "argv_template"
        ]["const"]
        == list(external_acquisitions.host_provenance_command_template()),
        "host acquisition schema/Python authority differs",
    )
    return {
        "schema_version": "s11-contract-validation/1.0",
        "status": "contract_valid",
        "qualifying_evidence_generated": False,
        "tools": registry["tool_count"],
        "fixed_resources": registry["fixed_resource_count"],
        "resource_templates": registry["resource_template_count"],
        "registry_profile_sha256": sha256_file(REGISTRY_PROFILE_PATH),
        "assertions": len(REQUIRED_ASSERTIONS),
        "prompt_pack_sha256": sha256_file(PROMPT_PACK_PATH),
        "host_coordinate_profile_sha256": sha256_file(HOST_PROFILE_PATH),
        "server_instructions_wire_sha256": canonical_server_instructions()[
            "wire_sha256"
        ],
        "host_acquisition_command_sha256":
            documented_host_acquisition_sha256,
    }


def evidence_validation_summary(
    evidence_path: Path,
    *,
    checkout_binding_validated: bool,
) -> dict[str, Any]:
    return {
        "schema_version": "s11-evidence-validation/1.0",
        "status": (
            "passed"
            if checkout_binding_validated
            else "validated_nonqualifying"
        ),
        "qualifying_evidence_validated": checkout_binding_validated,
        "checkout_binding": (
            "validated" if checkout_binding_validated else "skipped"
        ),
        "evidence_sha256": sha256_file(evidence_path),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--validate",
        type=Path,
        help="validate pre-existing final evidence; never generates it",
    )
    parser.add_argument(
        "--artifact-root",
        type=Path,
        help="root containing the detached archive and package content files",
    )
    parser.add_argument(
        "--no-checkout-binding",
        action="store_true",
        help="skip HEAD/evidence-only relation (contract debugging only)",
    )
    parser.add_argument(
        "--compare-traces",
        nargs=3,
        type=Path,
        metavar=("APP", "CLI", "IDE"),
        help=(
            "compare three pre-existing surface transport traces; this "
            "standalone parity check is nonqualifying without final evidence "
            "authorities"
        ),
    )
    parser.add_argument(
        "--compare-journals",
        nargs=3,
        type=Path,
        metavar=("APP_JOURNAL", "CLI_JOURNAL", "IDE_JOURNAL"),
        help="source-bound recorder journals for --compare-traces",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        help=(
            "run every automated gate with this per-command timeout "
            "(1..180 seconds)"
        ),
    )
    arguments = parser.parse_args()
    repository = GitRepository()
    if arguments.validate is not None:
        require(
            arguments.artifact_root is not None,
            "--validate requires --artifact-root",
        )
        timeout = arguments.timeout if arguments.timeout is not None else 180.0
        observed_gates = run_automated_gates(timeout)
        report = strict_json_load(
            arguments.validate,
            maximum_bytes=MAX_EVIDENCE_BYTES,
        )
        validate_evidence(
            report,
            repository=repository,
            evidence_path=arguments.validate,
            artifact_root=arguments.artifact_root,
            check_checkout=not arguments.no_checkout_binding,
            observed_gates=observed_gates,
        )
        result = evidence_validation_summary(
            arguments.validate,
            checkout_binding_validated=not arguments.no_checkout_binding,
        )
    elif arguments.compare_traces is not None:
        require(
            arguments.compare_journals is not None,
            "--compare-traces requires three --compare-journals artifacts",
        )
        traces = {
            surface: strict_json_load(path, maximum_bytes=MAX_TRACE_BYTES)
            for surface, path in zip(
                ("app", "cli", "ide"),
                arguments.compare_traces,
            )
        }
        journal_paths = cast(tuple[Path, Path, Path], arguments.compare_journals)
        journals = {
            surface: strict_json_load(
                path,
                maximum_bytes=MAX_RECORDER_JOURNAL_BYTES,
            )
            for surface, path in zip(("app", "cli", "ide"), journal_paths)
        }
        journal_digests = {
            surface: sha256_file(path)
            for surface, path in zip(("app", "cli", "ide"), journal_paths)
        }
        result = {
            "schema_version": "s11-parity-validation/1.0",
            "status": "parity_valid_nonqualifying",
            "external_surface_authority_validated": False,
            "normalized_sha256": compare_surface_traces(
                traces,
                qualifying=False,
                recorder_journals=journals,
                recorder_journal_digests=journal_digests,
            ),
        }
    elif arguments.compare_journals is not None:
        raise AcceptanceError("--compare-journals requires --compare-traces")
    elif arguments.timeout is not None:
        observed_gates = run_automated_gates(arguments.timeout)
        result = {
            "schema_version": "s11-automated-gates/1.0",
            "status": "automated_gates_passed",
            "qualifying_evidence_generated": False,
            "external_acquisition_validated": False,
            "gates": observed_gates,
            "evidence_gate_bindings": canonical_gate_bindings(),
        }
    else:
        result = validate_contract_files(repository)
    print(json.dumps(result, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except AcceptanceError as error:
        print(f"Sprint 11 acceptance validation failed: {error}")
        raise SystemExit(1)
