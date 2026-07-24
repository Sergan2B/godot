"""Fail-closed semantic-consistency gate for Sprint 11 Godot guidance."""

from __future__ import annotations

import hashlib
import itertools
import json
import re
import unittest
from pathlib import Path
from typing import Any, Callable, Iterable


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
PRODUCT_ROOT = REPOSITORY_ROOT / "godot-codex-mcp" / "product"
REGISTRY_PATH = PRODUCT_ROOT / "registry-profile.v1.json"
HOST_PROFILE_PATH = PRODUCT_ROOT / "host-coordinate-profile.v1.json"
SERVER_INSTRUCTIONS_PATH = PRODUCT_ROOT / "server-instructions.v1.txt"
REGISTRY_SOURCE_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "crates"
    / "product-core"
    / "src"
    / "registry.rs"
)
INSTALLER_PATH = REPOSITORY_ROOT / "godot-codex-mcp" / "packaging" / "install.sh"
LAUNCHER_SOURCE_PATH = (
    REPOSITORY_ROOT
    / "godot-codex-mcp"
    / "crates"
    / "operations"
    / "src"
    / "launcher.rs"
)

GUIDANCE_PATHS = {
    "server instructions": SERVER_INSTRUCTIONS_PATH,
    "AGENTS template": (
        REPOSITORY_ROOT / "docs" / "codex-integration" / "templates" / "AGENTS.godot.md"
    ),
    "godot-editor skill": (
        REPOSITORY_ROOT / ".agents" / "skills" / "godot-editor" / "SKILL.md"
    ),
    "external beta guide": (
        REPOSITORY_ROOT
        / "docs"
        / "codex-integration"
        / "EXTERNAL-CODEX-BETA-GUIDE.md"
    ),
}

OPERATIONS_LAUNCHER = (
    '"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex"'
)
STATUS_TOOLS = {"godot_get_connection_status"}
STATUS_RESOURCES = {"godot://connection/status"}
SAVED_READ_TOOLS = {
    "godot_find_resource_owners",
    "godot_find_usages",
    "godot_get_resource_dependencies",
    "godot_get_scene_graph",
    "godot_inspect_node",
    "godot_inspect_symbol",
    "godot_search_symbols",
}
LIVE_READ_TOOLS = {
    "godot_get_current_scene",
    "godot_get_editor_state",
    "godot_get_inspector_state",
    "godot_get_open_scenes",
    "godot_get_open_scripts",
    "godot_get_selected_nodes",
    "godot_get_viewport_state",
}
RUNTIME_CONTROL_TOOLS = {
    "godot_continue_project",
    "godot_pause_project",
    "godot_run_current_scene",
    "godot_run_project",
    "godot_stop_project",
}
RUNTIME_READ_TOOLS = {
    "godot_capture_viewport",
    "godot_get_runtime_tree",
    "godot_get_stack_trace",
    "godot_inspect_runtime_object",
}
WRITE_TOOLS = {
    "godot_apply_transaction",
    "godot_prepare_attach_script",
    "godot_prepare_change_set",
    "godot_prepare_connect_signal",
    "godot_prepare_create_node",
    "godot_prepare_delete_node",
    "godot_prepare_detach_script",
    "godot_prepare_disconnect_signal",
    "godot_prepare_reparent_node",
    "godot_prepare_set_property",
    "godot_undo_transaction",
}
POST_WRITE_READ_TOOLS = {
    "godot_get_transaction_status",
    "godot_get_validation_report",
}


class GuidanceContractError(AssertionError):
    """A committed guidance or registry fact diverged from the contract."""


def _reject_duplicate_members(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, member in pairs:
        if key in value:
            raise GuidanceContractError(f"duplicate JSON member: {key}")
        value[key] = member
    return value


def strict_json_load(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=_reject_duplicate_members,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise GuidanceContractError(f"{path}: invalid JSON") from error
    if not isinstance(value, dict):
        raise GuidanceContractError(f"{path}: root must be an object")
    return value


def _normalized(text: str) -> str:
    return re.sub(r"\s+", " ", text.lower()).strip()


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise GuidanceContractError(message)


def _has_nearby_terms(
    text: str,
    groups: Iterable[Iterable[str]],
    *,
    maximum_span: int,
) -> bool:
    """Accept wording variants while requiring one local semantic statement."""

    normalized = _normalized(text)
    positions: list[list[int]] = []
    for group in groups:
        group_positions = [
            match.start()
            for term in group
            for match in re.finditer(re.escape(term.lower()), normalized)
        ]
        if not group_positions:
            return False
        positions.append(group_positions)
    return any(
        max(candidate) - min(candidate) <= maximum_span
        for candidate in itertools.product(*positions)
    )


def _has_ordered_terms(
    text: str,
    groups: Iterable[Iterable[str]],
    *,
    maximum_span: int,
) -> bool:
    normalized = _normalized(text)
    semantic_groups = tuple(tuple(term.lower() for term in group) for group in groups)
    if not semantic_groups:
        return False
    first_positions = sorted(
        match.start()
        for term in semantic_groups[0]
        for match in re.finditer(re.escape(term), normalized)
    )
    for first in first_positions:
        cursor = first + 1
        last = first
        for group in semantic_groups[1:]:
            matches = [
                normalized.find(term, cursor)
                for term in group
                if normalized.find(term, cursor) >= 0
            ]
            if not matches:
                break
            last = min(matches)
            cursor = last + 1
        else:
            if last - first <= maximum_span:
                return True
    return False


def _semantic_checks() -> dict[str, Callable[[str], bool]]:
    def status_first(text: str) -> bool:
        normalized = _normalized(text)
        status = min(
            (
                position
                for token in (
                    "godot_get_connection_status",
                    "godot://connection/status",
                )
                if (position := normalized.find(token)) >= 0
            ),
            default=-1,
        )
        explicit = _has_nearby_terms(
            text,
            (
                ("start every workflow", "before assuming", "first", "1. call"),
                ("godot_get_connection_status", "godot://connection/status"),
            ),
            maximum_span=220,
        )
        return status >= 0 and explicit

    def offline_saved_only(text: str) -> bool:
        saved_only = _has_nearby_terms(
            text,
            (
                ("offline", "offline_cached"),
                ("saved", "saved-project", "saved semantic"),
                ("only", "only the documented", "use only"),
            ),
            maximum_span=320,
        )
        no_live = _has_nearby_terms(
            text,
            (
                ("offline", "offline_cached"),
                ("live", "editor", "runtime"),
                ("unavailable", "cannot", "never", "must fail closed"),
            ),
            maximum_span=700,
        )
        no_write = _has_nearby_terms(
            text,
            (
                ("offline", "offline_cached"),
                ("write", "transaction", "prepare/apply"),
                ("unavailable", "cannot", "must fail closed"),
            ),
            maximum_span=750,
        )
        return saved_only and no_live and no_write

    def guarded_write_sequence(text: str) -> bool:
        before_undo = _has_ordered_terms(
            text,
            (
                ("read a current", "read a consistent", "use read", "read →"),
                ("prepare",),
                ("approval", "approve", "confirmation"),
                ("apply",),
                ("read back", "readback"),
                ("undo",),
            ),
            maximum_span=1_600,
        )
        validation = _has_nearby_terms(
            text,
            (
                ("apply",),
                ("read back", "readback"),
                ("validation", "validate"),
                ("undo",),
            ),
            maximum_span=900,
        )
        return before_undo and validation

    def independent_approvals(text: str) -> bool:
        return _has_nearby_terms(
            text,
            (
                ("codex sandbox", "host's normal tool", "normal codex tool"),
                ("form approval", "form confirmation", "semantic form"),
                ("independent", "separate", "separately", "distinct"),
            ),
            maximum_span=850,
        )

    def no_uncertain_replay(text: str) -> bool:
        return _has_nearby_terms(
            text,
            (
                ("timeout", "response loss", "uncertain response", "commit point"),
                (
                    "never replay",
                    "do not replay",
                    "never infer approval or replay",
                    "or replay apply",
                ),
                ("apply",),
            ),
            maximum_span=360,
        )

    def runtime_binding(text: str) -> bool:
        return "runtime_session_id" in text and _has_nearby_terms(
            text,
            (
                ("runtime_session_id",),
                ("bind", "pass", "retain"),
            ),
            maximum_span=180,
        )

    def project_isolation(text: str) -> bool:
        return _has_nearby_terms(
            text,
            (
                (
                    "project-scoped",
                    "exact project",
                    "project roots",
                    "across projects",
                    "multiple projects",
                ),
                (
                    "never reuse",
                    "do not reuse",
                    "never copy",
                    "another project",
                    "project isolation",
                ),
            ),
            maximum_span=900,
        )

    def absolute_package_launcher(text: str) -> bool:
        normalized = _normalized(text.replace("\\\n", " "))
        return (
            OPERATIONS_LAUNCHER.lower() in normalized
            and "doctor" in normalized
            and "setup" in normalized
            and _has_nearby_terms(
                text,
                (
                    ("package-owned", "installed stable package", "installer-owned"),
                    ("absolute launcher", "absolute stable launcher"),
                    ("path basename", "checkout binary", "checkout-relative"),
                ),
                maximum_span=1_500,
            )
        )

    return {
        "status-first": status_first,
        "offline saved-only with live/write forbidden": offline_saved_only,
        "read/prepare/approval/apply/readback/validation/targeted Undo": (
            guarded_write_sequence
        ),
        "independent host and semantic approvals": independent_approvals,
        "no replay after uncertain commit": no_uncertain_replay,
        "runtime_session_id binding": runtime_binding,
        "project isolation": project_isolation,
        "package-owned absolute doctor/setup launcher": absolute_package_launcher,
    }


def audit_guidance(documents: dict[str, str]) -> None:
    checks = _semantic_checks()
    _require(
        set(documents) == set(GUIDANCE_PATHS),
        "guidance document set differs",
    )
    for label, text in documents.items():
        _require(bool(text.strip()), f"{label}: guidance is empty")
        for concept, check in checks.items():
            _require(check(text), f"{label}: missing or divergent {concept}")


def _registry_digest(profile: dict[str, Any]) -> str:
    payload = {
        "profile_id": profile["profile_id"],
        "tools": profile["tools"],
        "read_only_tools": profile["read_only_tools"],
        "fixed_resources": profile["fixed_resources"],
        "resource_templates": profile["resource_templates"],
    }
    encoded = json.dumps(
        payload,
        allow_nan=False,
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _rust_string_array(source: str, constant: str) -> list[str]:
    match = re.search(
        rf"pub const {re.escape(constant)}:\s*&\[&str\]\s*=\s*&\[(.*?)\];",
        source,
        flags=re.DOTALL,
    )
    _require(match is not None, f"Rust registry constant {constant} is absent")
    return re.findall(r'"([^"]+)"', match.group(1))


def audit_registry(profile: dict[str, Any], registry_source: str) -> None:
    expected_fields = {
        "schema_version",
        "profile_id",
        "tool_count",
        "fixed_resource_count",
        "resource_template_count",
        "tools",
        "read_only_tools",
        "fixed_resources",
        "resource_templates",
        "digest",
    }
    _require(set(profile) == expected_fields, "registry profile fields differ")
    _require(
        profile["schema_version"] == "godot-codex-registry-profile/1.0"
        and profile["profile_id"] == "external-codex-beta-v1",
        "registry profile identity differs",
    )
    tools = profile["tools"]
    read_only = profile["read_only_tools"]
    resources = profile["fixed_resources"]
    templates = profile["resource_templates"]
    for label, values, declared_count in (
        ("tools", tools, profile["tool_count"]),
        ("fixed resources", resources, profile["fixed_resource_count"]),
        ("resource templates", templates, profile["resource_template_count"]),
    ):
        _require(
            isinstance(values, list)
            and all(isinstance(value, str) for value in values)
            and values == sorted(set(values))
            and declared_count == len(values),
            f"registry {label} are not closed, sorted, and count-bound",
        )
    _require(
        isinstance(read_only, list)
        and read_only == sorted(set(read_only))
        and set(read_only).issubset(tools),
        "read-only registry is not a closed subset",
    )
    _require(profile["digest"] == _registry_digest(profile), "registry digest differs")

    tool_set = set(tools)
    read_only_set = set(read_only)
    required_tools = (
        STATUS_TOOLS
        | SAVED_READ_TOOLS
        | LIVE_READ_TOOLS
        | RUNTIME_CONTROL_TOOLS
        | RUNTIME_READ_TOOLS
        | WRITE_TOOLS
        | POST_WRITE_READ_TOOLS
    )
    _require(required_tools.issubset(tool_set), "guidance names an unavailable tool class")
    _require(
        STATUS_TOOLS | SAVED_READ_TOOLS | LIVE_READ_TOOLS | RUNTIME_READ_TOOLS
        | POST_WRITE_READ_TOOLS
        <= read_only_set,
        "observational tools left the read-only registry",
    )
    _require(
        not (RUNTIME_CONTROL_TOOLS | WRITE_TOOLS) & read_only_set,
        "runtime control or write tool entered the read-only registry",
    )
    _require(
        STATUS_RESOURCES <= set(resources)
        and {
            "godot://editor/summary",
            "godot://project/summary",
            "godot://runtime/summary",
        }
        <= set(resources)
        and {"godot://scene/{scene_id}/summary"} <= set(templates),
        "guidance resources differ from the registry",
    )

    _require(
        _rust_string_array(registry_source, "FULL_BETA_TOOLS") == tools,
        "Rust full-beta registry differs from the product profile",
    )
    _require(
        _rust_string_array(registry_source, "READ_ONLY_TOOLS") == read_only,
        "Rust read-only registry differs from the product profile",
    )
    _require(
        _rust_string_array(registry_source, "FIXED_RESOURCE_URIS") == resources,
        "Rust fixed-resource registry differs from the product profile",
    )
    _require(
        _rust_string_array(registry_source, "RESOURCE_TEMPLATE_URIS") == templates,
        "Rust resource-template registry differs from the product profile",
    )


def audit_host_binding(host_profile: dict[str, Any]) -> None:
    binding = host_profile.get("server_instructions")
    _require(isinstance(binding, dict), "host profile has no instructions binding")
    raw = SERVER_INSTRUCTIONS_PATH.read_bytes()
    _require(raw.endswith(b"\n") and not raw.endswith(b"\n\n"), "wire text differs")
    wire = raw[:-1]
    _require(
        binding
        == {
            "path": "godot-codex-mcp/product/server-instructions.v1.txt",
            "file_sha256": f"sha256:{hashlib.sha256(raw).hexdigest()}",
            "wire_sha256": f"sha256:{hashlib.sha256(wire).hexdigest()}",
        },
        "host profile does not bind the exact server instructions",
    )


def audit_package_launcher(installer: str, launcher_source: str) -> None:
    _require(
        "Library/Application Support/GodotCodex" in installer
        and 'current_link="$data_root/current"' in installer
        and "bin/godot-codex" in installer
        and "bin/godot-codex-mcp" in installer,
        "installer no longer owns the stable package launchers",
    )
    _require(
        'home.join("Library/Application Support/GodotCodex")' in launcher_source,
        "setup/doctor launcher root differs from the package-owned absolute root",
    )


class Sprint11GuidanceTests(unittest.TestCase):
    def test_guidance_documents_share_the_safety_contract(self) -> None:
        audit_guidance(
            {
                label: path.read_text(encoding="utf-8")
                for label, path in GUIDANCE_PATHS.items()
            }
        )

    def test_registry_and_production_projection_support_the_guidance(self) -> None:
        audit_registry(
            strict_json_load(REGISTRY_PATH),
            REGISTRY_SOURCE_PATH.read_text(encoding="utf-8"),
        )

    def test_packaged_server_instructions_binding_is_current(self) -> None:
        audit_host_binding(strict_json_load(HOST_PROFILE_PATH))

    def test_absolute_launcher_claim_matches_package_ownership(self) -> None:
        audit_package_launcher(
            INSTALLER_PATH.read_text(encoding="utf-8"),
            LAUNCHER_SOURCE_PATH.read_text(encoding="utf-8"),
        )

    def test_gate_fails_closed_when_a_required_semantic_fact_is_removed(self) -> None:
        documents = {
            label: path.read_text(encoding="utf-8")
            for label, path in GUIDANCE_PATHS.items()
        }
        documents["server instructions"] = documents["server instructions"].replace(
            "runtime_session_id",
            "runtime handle",
        )
        with self.assertRaisesRegex(
            GuidanceContractError,
            "runtime_session_id binding",
        ):
            audit_guidance(documents)

    def test_gate_fails_closed_when_write_enters_read_only_registry(self) -> None:
        profile = strict_json_load(REGISTRY_PATH)
        profile["read_only_tools"] = sorted(
            [*profile["read_only_tools"], "godot_apply_transaction"]
        )
        profile["digest"] = _registry_digest(profile)
        with self.assertRaisesRegex(GuidanceContractError, "write tool entered"):
            audit_registry(
                profile,
                REGISTRY_SOURCE_PATH.read_text(encoding="utf-8"),
            )


if __name__ == "__main__":
    unittest.main()
