#!/usr/bin/env python3
"""Prove concurrent project isolation with two real Godot/sidecar pairs.

The emitted report is a bounded, non-qualifying test artifact. It deliberately
contains only digests of opaque identities, never project roots, process IDs,
discovery endpoints, tokens, cursors, or native handles.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import contextlib
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Mapping, TypeVar, cast

SCRIPT_IMPORT_ROOT = Path(__file__).resolve().parent
if str(SCRIPT_IMPORT_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPT_IMPORT_ROOT))

try:
    from tests.codex import sprint9_model_free_live as s9
    from tests.codex import sprint10_model_free_live as s10
    from tests.codex import sprint11_acquisition_paths as acquisition_paths
    from tests.codex import sprint11_packaged_fixture as packaged_fixture
except ModuleNotFoundError:  # Direct execution from tests/codex.
    import sprint9_model_free_live as s9
    import sprint10_model_free_live as s10
    import sprint11_acquisition_paths as acquisition_paths
    import sprint11_packaged_fixture as packaged_fixture

SCRIPT_ROOT = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_ROOT.parent.parent
REGISTRY_PROFILE = (
    REPOSITORY_ROOT / "godot-codex-mcp/product/registry-profile.v1.json"
)

REPORT_SCHEMA = "s11-multi-project-live/1.0"
REPORT_LIMIT = 65_536
MAX_PROJECT_SOURCE_FILES = 4_096
MAX_PROJECT_SOURCE_FILE_BYTES = 16 * 1024 * 1024
MAX_PROJECT_SOURCE_BYTES = 64 * 1024 * 1024
MAX_EXECUTABLE_BYTES = 2 * 1024 * 1024 * 1024
DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
PROJECT_RE = re.compile(r"project:sha256:[0-9a-f]{64}\Z")
EDITOR_RE = re.compile(r"editor:[0-9a-f]{32}\Z")
RUNTIME_RE = re.compile(r"runtime:[0-9a-f]{32}\Z")
TRANSACTION_RE = re.compile(r"transaction:[0-9a-f]{32}\Z")
PACKAGE_VERSION_RE = re.compile(r"[0-9A-Za-z][0-9A-Za-z.+-]{0,127}\Z")

ISOLATION_CASE_EXPECTATIONS: dict[str, frozenset[str]] = {
    "copied_discovery": frozenset(
        {
            "bridge_discovery_stale",
            "bridge_unreachable",
            "editor_offline",
            "project_binding_mismatch",
        }
    ),
    "copied_token": frozenset(
        {"bridge_authentication_failed", "editor_offline"}
    ),
    "swapped_discovery": frozenset(
        {
            "bridge_discovery_stale",
            "bridge_unreachable",
            "editor_offline",
            "project_binding_mismatch",
        }
    ),
    "swapped_token": frozenset(
        {"bridge_authentication_failed", "editor_offline"}
    ),
    "swapped_config": frozenset({"project_binding_mismatch"}),
    "swapped_root": frozenset({"project_binding_mismatch"}),
    "swapped_cwd": frozenset({"project_binding_mismatch"}),
    "editor_restart": frozenset({"editor_reconnected"}),
    "cache_rebuild": frozenset({"cache_rebuilt"}),
    "package_mismatch": frozenset({"package_binding_mismatch"}),
    "version_mismatch": frozenset({"package_version_mismatch"}),
}
ISOLATION_CASE_ORDER = tuple(ISOLATION_CASE_EXPECTATIONS)
ISOLATION_PROOF_FIELDS = {
    "no_fallback",
    "no_cross_project_data",
    "no_cross_project_mutation",
    "no_cross_project_approval",
    "no_cross_project_transaction",
    "cleanup",
}
ISOLATION_CASE_PROOF_SOURCES = {
    "copied_discovery": "live_two_project_processes",
    "copied_token": "live_two_project_processes",
    "swapped_discovery": "live_two_project_processes",
    "swapped_token": "live_two_project_processes",
    "swapped_config": (
        "public_runner_prelaunch_authority_plus_external_surface_attestation"
    ),
    "swapped_root": (
        "public_runner_prelaunch_authority_plus_external_surface_attestation"
    ),
    "swapped_cwd": (
        "public_runner_prelaunch_authority_plus_external_surface_attestation"
    ),
    "editor_restart": "live_two_project_processes",
    "cache_rebuild": "live_two_project_processes",
    "package_mismatch": (
        "package_bound_prelaunch_authority_plus_external_surface_attestation"
    ),
    "version_mismatch": (
        "package_bound_prelaunch_authority_plus_external_surface_attestation"
    ),
}

FOREIGN_EXPECTATIONS = {
    "approval_binding": "transaction_not_found",
    "cursor": "stale_cursor",
    "project_id": "wrong_project",
    "runtime_session_id": "stale_runtime_session",
    "transaction_id": "transaction_not_found",
    "validation_report_id": "validation_report_not_found",
}

T = TypeVar("T")


def _exact(value: Any, fields: set[str], label: str) -> dict[str, Any]:
    s9.require(isinstance(value, dict), f"{label} is not an object")
    record = cast(dict[str, Any], value)
    s9.require(set(record) == fields, f"{label} fields differ")
    return record


def _registry_profile() -> dict[str, Any]:
    value = json.loads(REGISTRY_PROFILE.read_text(encoding="utf-8"))
    s9.require(isinstance(value, dict), "canonical registry profile is malformed")
    tools = value.get("tools")
    read_only = value.get("read_only_tools")
    s9.require(
        value.get("schema_version") == "godot-codex-registry-profile/1.0"
        and value.get("tool_count") == 41
        and isinstance(tools, list)
        and len(tools) == 41
        and tools == sorted(set(tools))
        and isinstance(read_only, list)
        and read_only == sorted(set(read_only))
        and set(read_only) <= set(tools)
        and DIGEST_RE.fullmatch("sha256:" + str(value.get("digest", "")))
        is not None,
        "canonical registry profile differs",
    )
    return cast(dict[str, Any], value)


def _install_sprint11_client() -> dict[str, Any]:
    profile = _registry_profile()
    tools = set(cast(list[str], profile["tools"]))
    read_only = set(cast(list[str], profile["read_only_tools"]))
    s9.TOOL_NAMES = tools
    s9.MUTATING_TOOLS = tools - read_only
    s9.ModelFreeMcpClient = s10.Sprint10McpClient
    return profile


def _parallel(
    first: Callable[[], T],
    second: Callable[[], T],
) -> tuple[T, T]:
    with concurrent.futures.ThreadPoolExecutor(
        max_workers=2,
        thread_name_prefix="s11-project",
    ) as executor:
        first_future = executor.submit(first)
        second_future = executor.submit(second)
        return first_future.result(), second_future.result()


@contextlib.contextmanager
def _managed_fixture(
    session: s9.FixtureSession,
) -> Any:
    """Clean partial editor/sidecar startup if FixtureSession.__enter__ fails."""

    try:
        yield session.__enter__()
    finally:
        session.__exit__(*sys.exc_info())


def _project_source_hashes(project_root: Path) -> dict[str, str]:
    hashes: dict[str, str] = {}
    total_bytes = 0
    for path in sorted(project_root.rglob("*")):
        if ".godot" in path.parts:
            continue
        try:
            metadata = path.lstat()
        except OSError as error:
            raise s9.WorkflowError("project source path is unavailable") from error
        s9.require(
            not stat.S_ISLNK(metadata.st_mode),
            "project source tree contains a symlink",
        )
        if stat.S_ISDIR(metadata.st_mode):
            continue
        s9.require(
            stat.S_ISREG(metadata.st_mode)
            and len(hashes) < MAX_PROJECT_SOURCE_FILES,
            "project source tree shape exceeds its bound",
        )
        relative = path.relative_to(project_root).as_posix()
        try:
            payload = acquisition_paths.read_regular_file(
                path,
                maximum=MAX_PROJECT_SOURCE_FILE_BYTES,
            )
        except acquisition_paths.AcquisitionPathError as error:
            raise s9.WorkflowError(
                "project source file cannot be read safely"
            ) from error
        total_bytes += len(payload)
        s9.require(
            total_bytes <= MAX_PROJECT_SOURCE_BYTES,
            "project source tree exceeds its byte bound",
        )
        hashes[relative] = hashlib.sha256(payload).hexdigest()
    return hashes


def _identity_digest(kind: str, value: str) -> str:
    material = b"godot-codex/s11-multi-project-identity/v1\0"
    material += kind.encode("utf-8") + b"\0" + value.encode("utf-8")
    return "sha256:" + hashlib.sha256(material).hexdigest()


def _different_digest(value: str) -> str:
    s9.require(DIGEST_RE.fullmatch(value) is not None, "digest is malformed")
    replacement = "0" if value[7] != "0" else "1"
    return value[:7] + replacement + value[8:]


def _artifact_digest(path: Path) -> str:
    try:
        return acquisition_paths.sha256_regular_file(
            path,
            maximum=MAX_EXECUTABLE_BYTES,
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise s9.WorkflowError("executable artifact cannot be hashed safely") from error


def _isolation_case(
    case_id: str,
    observed_code: str,
    **proofs: bool,
) -> dict[str, Any]:
    s9.require(
        case_id in ISOLATION_CASE_EXPECTATIONS
        and observed_code in ISOLATION_CASE_EXPECTATIONS[case_id],
        f"isolation case {case_id} outcome differs",
    )
    proof_fields_match = set(proofs) == ISOLATION_PROOF_FIELDS
    failed_proofs = sorted(
        name for name, value in proofs.items() if value is not True
    )
    s9.require(
        proof_fields_match and not failed_proofs,
        f"isolation case {case_id} proof differs "
        f"(fields_match={proof_fields_match}, failed={failed_proofs})",
    )
    return {
        "case_id": case_id,
        "observed_code": observed_code,
        "proof_source": ISOLATION_CASE_PROOF_SOURCES[case_id],
        **proofs,
    }


def _validated_fault_matrix(value: Any) -> list[dict[str, Any]]:
    s9.require(
        isinstance(value, list) and len(value) == len(ISOLATION_CASE_ORDER),
        "isolation fault matrix coverage differs",
    )
    matrix = cast(list[Any], value)
    observed: list[str] = []
    validated: list[dict[str, Any]] = []
    for index, item in enumerate(matrix):
        record = _exact(
            item,
            {
                "case_id",
                "observed_code",
                "proof_source",
                *ISOLATION_PROOF_FIELDS,
            },
            f"isolation fault case {index}",
        )
        case_id = record["case_id"]
        observed_code = record["observed_code"]
        s9.require(
            isinstance(case_id, str)
            and isinstance(observed_code, str)
            and case_id in ISOLATION_CASE_EXPECTATIONS
            and observed_code in ISOLATION_CASE_EXPECTATIONS[case_id]
            and record["proof_source"]
            == ISOLATION_CASE_PROOF_SOURCES[case_id]
            and all(record[field] is True for field in ISOLATION_PROOF_FIELDS),
            f"isolation fault case {index} differs",
        )
        observed.append(case_id)
        validated.append(record)
    s9.require(
        tuple(observed) == ISOLATION_CASE_ORDER,
        "isolation fault matrix order/kinds differ",
    )
    return validated


def _probe_sidecar_version(sidecar: Path, timeout: float) -> str:
    try:
        result = subprocess.run(
            [str(sidecar), "--version"],
            cwd=sidecar.parent,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=min(max(timeout, 1.0), 10.0),
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise s9.WorkflowError("package version probe failed") from error
    s9.require(
        result.returncode == 0
        and len(result.stdout) <= 512
        and len(result.stderr) <= 512
        and result.stderr == b"",
        "package version probe differs",
    )
    try:
        output = result.stdout.decode("utf-8").strip()
    except UnicodeDecodeError as error:
        raise s9.WorkflowError("package version probe is not UTF-8") from error
    prefix = "godot-codex-mcp "
    version = output.removeprefix(prefix)
    s9.require(
        output.startswith(prefix)
        and PACKAGE_VERSION_RE.fullmatch(version) is not None,
        "package version probe differs",
    )
    return version


def _require_package_authority(
    sidecar: Path,
    *,
    expected_sha256: str,
    expected_version: str,
    observed_sha256: str,
    observed_version: str,
) -> None:
    s9.require(
        DIGEST_RE.fullmatch(expected_sha256) is not None
        and DIGEST_RE.fullmatch(observed_sha256) is not None
        and PACKAGE_VERSION_RE.fullmatch(expected_version) is not None
        and PACKAGE_VERSION_RE.fullmatch(observed_version) is not None,
        "package authority coordinate is malformed",
    )
    s9.require(
        _artifact_digest(sidecar) == observed_sha256,
        "package artifact changed after measurement",
    )
    if expected_sha256 != observed_sha256:
        raise s9.WorkflowError("package_binding_mismatch")
    if expected_version != observed_version:
        raise s9.WorkflowError("package_version_mismatch")


def _canonical_project_root(path: Path) -> Path:
    try:
        canonical = path.resolve(strict=True)
    except OSError as error:
        raise s9.WorkflowError("project_binding_mismatch") from error
    s9.require(
        canonical.is_dir()
        and (canonical / "project.godot").is_file()
        and not path.is_symlink(),
        "project_binding_mismatch",
    )
    return canonical


def _require_launch_authority(
    *,
    expected_root: Path,
    config_root: Path,
    argument_root: Path,
    cwd: Path,
) -> Path:
    expected = _canonical_project_root(expected_root)
    coordinates = tuple(
        _canonical_project_root(path)
        for path in (config_root, argument_root, cwd)
    )
    if any(coordinate != expected for coordinate in coordinates):
        raise s9.WorkflowError("project_binding_mismatch")
    return expected


def _preflight_negative_case(
    case_id: str,
    invoke: Callable[[], None],
) -> dict[str, Any]:
    expected_codes = ISOLATION_CASE_EXPECTATIONS[case_id]
    s9.require(
        len(expected_codes) == 1,
        f"isolation case {case_id} is not a preflight-only case",
    )
    expected = next(iter(expected_codes))
    try:
        invoke()
    except s9.WorkflowError as error:
        observed = str(error)
    else:
        raise s9.WorkflowError(f"isolation case {case_id} did not fail closed")
    s9.require(observed == expected, f"isolation case {case_id} outcome differs")
    return _isolation_case(
        case_id,
        observed,
        no_fallback=True,
        no_cross_project_data=True,
        no_cross_project_mutation=True,
        no_cross_project_approval=True,
        no_cross_project_transaction=True,
        cleanup=True,
    )


def _read(client: s9.ModelFreeMcpClient, timeout: float) -> dict[str, Any]:
    coordinates, _, _ = s9.read_coordinates(client, timeout)
    return coordinates


def _connection_status(
    client: s9.ModelFreeMcpClient,
    expected_project_id: str,
    timeout: float,
) -> dict[str, Any]:
    project_scope = expected_project_id.removeprefix("project:sha256:")
    deadline = time.monotonic() + timeout
    status: dict[str, Any] = {}
    while time.monotonic() < deadline:
        status, is_error, _ = client.tool("godot_get_connection_status", {})
        bridge = status.get("bridge")
        cache = status.get("static_cache")
        components = status.get("components")
        if (
            not is_error
            and status.get("status") == "ready"
            and status.get("project_scope") == project_scope
            and isinstance(status.get("package_version"), str)
            and bool(status["package_version"])
            and isinstance(bridge, dict)
            and bridge.get("condition") == "ready"
            and bridge.get("negotiated_protocol") == "1.8"
            and isinstance(cache, dict)
            and cache.get("condition") == "online_current"
            and cache.get("schema") == "1.3"
            and isinstance(cache.get("generation"), str)
            and isinstance(cache.get("revisions"), dict)
            and isinstance(components, dict)
            and components.get("editor") == "ready"
            and components.get("transactions") == "ready"
        ):
            break
        time.sleep(0.05)
    else:
        raise s9.WorkflowError(
            f"project-scoped connection/cache/version status differs: {status}"
        )
    encoded = json.dumps(status, sort_keys=True, separators=(",", ":"))
    s9.require(
        expected_project_id not in encoded
        and "session.token" not in encoded
        and ".sock" not in encoded
        and "/tmp/" not in encoded,
        "connection status leaked a private binding",
    )
    return status


def _discovery_binding(
    project_root: Path,
    expected: Mapping[str, Any],
) -> dict[str, str]:
    discovery_path = project_root / ".godot/codex/bridge.json"
    try:
        discovery_bytes = acquisition_paths.read_regular_file(
            discovery_path,
            maximum=16_384,
        )
    except acquisition_paths.AcquisitionPathError as error:
        raise s9.WorkflowError("discovery record cannot be read safely") from error
    discovery = s9.strict_json_text(discovery_bytes.decode("utf-8"))
    s9.require(
        set(discovery)
        == {
            "created_at",
            "discovery_schema",
            "editor_session_id",
            "endpoint",
            "pid",
            "project_id",
            "protocol_versions",
            "token_file",
            "transport",
        }
        and discovery["discovery_schema"] == 1
        and discovery["project_id"] == expected["project_id"]
        and discovery["editor_session_id"] == expected["editor_session_id"]
        and discovery["token_file"] == ".godot/codex/session.token"
        and isinstance(discovery["endpoint"], str)
        and discovery["protocol_versions"]
        == [f"1.{minor}" for minor in range(8, -1, -1)]
        and discovery["transport"] in {"uds", "tcp_loopback"},
        "discovery project/session/protocol binding differs",
    )
    token_path = project_root / cast(str, discovery["token_file"])
    try:
        token = acquisition_paths.read_regular_file(token_path, maximum=32)
    except acquisition_paths.AcquisitionPathError as error:
        raise s9.WorkflowError("session token cannot be read safely") from error
    s9.require(len(token) == 32, "session token byte count differs")
    return {
        "endpoint_sha256": s9.safe_digest(cast(str, discovery["endpoint"])),
        "token_sha256": s9.safe_digest(token),
    }


@dataclass(frozen=True)
class _PrivateFile:
    path: Path
    payload: bytes
    mode: int

    @classmethod
    def capture(cls, path: Path, *, maximum: int) -> _PrivateFile:
        try:
            metadata = path.lstat()
            payload = acquisition_paths.read_regular_file(
                path,
                maximum=maximum,
            )
        except (OSError, acquisition_paths.AcquisitionPathError) as error:
            raise s9.WorkflowError(
                "private project binding cannot be captured safely"
            ) from error
        s9.require(
            stat.S_ISREG(metadata.st_mode)
            and not path.is_symlink()
            and metadata.st_mode & 0o777 == 0o600,
            "private project binding mode/type differs",
        )
        return cls(path=path, payload=payload, mode=metadata.st_mode & 0o777)

    def restore(self) -> None:
        _replace_private_file(self.path, self.payload, self.mode)


def _replace_private_file(path: Path, payload: bytes, mode: int = 0o600) -> None:
    s9.require(
        path.parent.is_dir()
        and not path.parent.is_symlink()
        and 0 < len(payload) <= 16_384
        and mode == 0o600,
        "private project binding replacement differs",
    )
    descriptor = -1
    temporary: Path | None = None
    try:
        descriptor, name = tempfile.mkstemp(
            prefix=".s11-binding.",
            dir=path.parent,
        )
        temporary = Path(name)
        os.fchmod(descriptor, mode)
        with os.fdopen(descriptor, "wb", closefd=True) as stream:
            descriptor = -1
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        temporary.replace(path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except OSError as error:
        raise s9.WorkflowError(
            "private project binding replacement failed"
        ) from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        if temporary is not None and temporary.exists():
            temporary.unlink()


@contextlib.contextmanager
def _fault_probe(
    sidecar: Path,
    project_root: Path,
    timeout: float,
) -> Any:
    _require_launch_authority(
        expected_root=project_root,
        config_root=project_root,
        argument_root=project_root,
        cwd=project_root,
    )
    process = s9.LineProcess(
        [str(sidecar), "--project-root", str(project_root)],
        cwd=project_root,
        environment=os.environ.copy(),
    )
    client = s10.Sprint10McpClient(
        process,
        project_root=project_root,
        timeout=timeout,
        supports_form=True,
    )
    try:
        client.initialize()
        yield client
    finally:
        process.stop()
        s9.require(
            process.process.poll() is not None,
            "fault probe process did not stop",
        )


def _wait_fault_status(
    client: s9.ModelFreeMcpClient,
    *,
    allowed_codes: frozenset[str],
    timeout: float,
) -> tuple[dict[str, Any], str]:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] = {}
    while time.monotonic() < deadline:
        status, is_error, _ = client.tool("godot_get_connection_status", {})
        last = status
        diagnostic = status.get("diagnostic")
        code = diagnostic.get("code") if isinstance(diagnostic, dict) else None
        bridge = status.get("bridge")
        if (
            not is_error
            and isinstance(code, str)
            and code in allowed_codes
            and status.get("status") != "ready"
            and isinstance(bridge, dict)
            and bridge.get("condition") != "ready"
        ):
            return status, code
        time.sleep(0.05)
    raise s9.WorkflowError(f"fault status did not fail closed: {last}")


def _fault_probe_evidence(
    client: s10.Sprint10McpClient,
    *,
    local_project_id: str,
    foreign_node_name: str,
    foreign_project_id: str,
    foreign_transaction_id: str,
    foreign_workflow: Mapping[str, Any],
    allowed_codes: frozenset[str],
    timeout: float,
) -> tuple[str, bool, bool, bool]:
    status, code = _wait_fault_status(
        client,
        allowed_codes=allowed_codes,
        timeout=timeout,
    )
    encoded_status = json.dumps(
        status,
        allow_nan=False,
        sort_keys=True,
        separators=(",", ":"),
    )
    no_fallback = (
        foreign_project_id not in encoded_status
        and foreign_node_name not in encoded_status
    )
    graph, graph_error, _ = client.tool(
        "godot_get_scene_graph",
        {"scene": "res://main.tscn", "limit": 200},
    )
    graph_text = json.dumps(
        graph,
        allow_nan=False,
        sort_keys=True,
        separators=(",", ":"),
    )
    no_foreign_data = (
        foreign_project_id not in graph_text
        and foreign_node_name not in graph_text
        and (
            graph_error
            or graph.get("project_id") == local_project_id
        )
    )
    s9.require(
        no_fallback and no_foreign_data,
        "fault probe isolation projection differs "
        f"(code={code}, "
        f"status_contains_foreign_project="
        f"{foreign_project_id in encoded_status}, "
        f"status_contains_foreign_node={foreign_node_name in encoded_status}, "
        f"graph_error={graph_error}, "
        f"graph_contains_foreign_project={foreign_project_id in graph_text}, "
        f"graph_contains_foreign_node={foreign_node_name in graph_text}, "
        f"graph_local_project_match="
        f"{graph.get('project_id') == local_project_id}, "
        f"graph_fields={sorted(graph)})",
    )
    foreign_status, status_error, _ = client.tool(
        "godot_get_transaction_status",
        {"transaction_id": foreign_transaction_id},
    )
    foreign_prepared = cast(
        Mapping[str, Any],
        foreign_workflow["prepared"],
    )
    foreign_before = cast(Mapping[str, Any], foreign_workflow["before"])
    elicitation_before = client.elicitation_total
    foreign_apply, apply_error, _ = client.tool(
        "godot_apply_transaction",
        {
            "transaction_id": foreign_prepared["change_set_id"],
            "preview_digest": foreign_prepared["preview_digest"],
            "expected_scene_revision": foreign_before["scene_revision"],
            "expected_operation_seq": foreign_before["operation_seq"],
        },
    )
    status_code = (
        foreign_status.get("error", {}).get("code")
        if isinstance(foreign_status.get("error"), dict)
        else None
    )
    apply_code = (
        foreign_apply.get("error", {}).get("code")
        if isinstance(foreign_apply.get("error"), dict)
        else None
    )
    fault_errors = {
        "editor_offline",
        "bridge_unreachable",
        "bridge_discovery_stale",
        "bridge_authentication_failed",
        "project_binding_mismatch",
        "transaction_not_found",
    } | set(allowed_codes)
    arguments = {
        "project_id": foreign_project_id,
        "idempotency_key": (
            "idempotency:"
            + hashlib.sha256(
                f"s11:fault:{code}:{foreign_project_id}".encode()
            ).hexdigest()[:32]
        ),
        "coordinates": {
            "editor_session_id": "editor:" + "f" * 32,
            "scene_id": "scene:" + "f" * 32,
            "scene_revision": 0,
            "operation_seq": 0,
            "resource_revision": 0,
            "script_graph_revision": 0,
        },
        "operations": [
            {
                "kind": "create_node",
                "alias": "alias:foreign-fault",
                "parent_node_id": "editor-node:" + "f" * 32,
                "godot_type": "Node",
                "name": "ForeignFault",
            }
        ],
        "save_scope": {"paths": []},
        "validation_policy": {
            "rollback": "on_required_failure",
            "warnings": "allow",
            "runtime": "skip",
        },
    }
    rejected, is_error, _ = client.tool(
        "godot_prepare_change_set",
        arguments,
    )
    no_mutation = (
        is_error
        and isinstance(rejected.get("error"), dict)
        and client.elicitation_total == 0
    )
    no_transaction = (
        status_error
        and apply_error
        and status_code in fault_errors
        and apply_code in fault_errors
        and client.approval is None
        and client.elicitation_total == elicitation_before
        and foreign_transaction_id
        not in json.dumps(
            [foreign_status, foreign_apply],
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        )
    )
    return code, no_fallback and no_foreign_data, no_mutation, no_transaction


def _assert_committed_workflow(
    client: s9.ModelFreeMcpClient,
    workflow: Mapping[str, Any],
    timeout: float,
) -> dict[str, Any]:
    current = _read(client, timeout)
    expected = cast(Mapping[str, Any], workflow["after"])
    for field in (
        "project_id",
        "editor_session_id",
        "scene_id",
        "scene_revision",
        "operation_seq",
        "action_count",
    ):
        s9.require(
            current[field] == expected[field],
            f"project state changed during isolation fault: {field}",
        )
    prepared = cast(Mapping[str, Any], workflow["prepared"])
    _transaction_status(
        client,
        cast(str, prepared["change_set_id"]),
        expected_state="committed",
        timeout=timeout,
        context="private_fault_committed",
        report_id=cast(str, workflow["report_id"]),
    )
    return current


def _run_private_binding_fault(
    *,
    case_id: str,
    sidecar: Path,
    session_a: s9.FixtureSession,
    session_b: s9.FixtureSession,
    client_a: s10.Sprint10McpClient,
    client_b: s10.Sprint10McpClient,
    workflow_a: Mapping[str, Any],
    workflow_b: Mapping[str, Any],
    timeout: float,
) -> tuple[
    dict[str, Any],
    s10.Sprint10McpClient,
    s10.Sprint10McpClient,
]:
    s9.require(
        case_id
        in {
            "copied_discovery",
            "copied_token",
            "swapped_discovery",
            "swapped_token",
        }
        and session_a.project_root is not None
        and session_b.project_root is not None
        and session_a.sidecar_process is not None
        and session_b.sidecar_process is not None,
        "private binding fault setup differs",
    )
    paths = {
        "a_discovery": session_a.project_root / ".godot/codex/bridge.json",
        "b_discovery": session_b.project_root / ".godot/codex/bridge.json",
        "a_token": session_a.project_root / ".godot/codex/session.token",
        "b_token": session_b.project_root / ".godot/codex/session.token",
    }
    snapshots = {
        name: _PrivateFile.capture(
            path,
            maximum=16_384 if name.endswith("discovery") else 32,
        )
        for name, path in paths.items()
    }
    before_approvals = {
        "a": client_a.elicitation_total,
        "b": client_b.elicitation_total,
    }
    affected = ("a",) if case_id.startswith("copied_") else ("a", "b")
    probe_results: list[tuple[str, bool, bool, bool]] = []
    restored = False
    modified: set[str] = set()
    try:
        session_a.sidecar_process.stop()
        if "b" in affected:
            session_b.sidecar_process.stop()
        if case_id == "copied_discovery":
            modified.add("a_discovery")
            _replace_private_file(
                paths["a_discovery"],
                snapshots["b_discovery"].payload,
            )
        elif case_id == "copied_token":
            modified.add("a_token")
            _replace_private_file(
                paths["a_token"],
                snapshots["b_token"].payload,
            )
        elif case_id == "swapped_discovery":
            modified.update({"a_discovery", "b_discovery"})
            _replace_private_file(
                paths["a_discovery"],
                snapshots["b_discovery"].payload,
            )
            _replace_private_file(
                paths["b_discovery"],
                snapshots["a_discovery"].payload,
            )
        else:
            modified.update({"a_token", "b_token"})
            _replace_private_file(
                paths["a_token"],
                snapshots["b_token"].payload,
            )
            _replace_private_file(
                paths["b_token"],
                snapshots["a_token"].payload,
            )

        for label in affected:
            project_root = (
                session_a.project_root if label == "a" else session_b.project_root
            )
            local_workflow = workflow_a if label == "a" else workflow_b
            foreign_workflow = workflow_b if label == "a" else workflow_a
            foreign_after = cast(
                Mapping[str, Any],
                foreign_workflow["after"],
            )
            foreign_prepared = cast(
                Mapping[str, Any],
                foreign_workflow["prepared"],
            )
            with _fault_probe(sidecar, project_root, timeout) as probe:
                probe_results.append(
                    _fault_probe_evidence(
                        probe,
                        local_project_id=cast(
                            str,
                            cast(Mapping[str, Any], local_workflow["after"])[
                                "project_id"
                            ],
                        ),
                        foreign_node_name=cast(
                            str,
                            foreign_workflow["node_name"],
                        ),
                        foreign_project_id=cast(
                            str,
                            foreign_after["project_id"],
                        ),
                        foreign_transaction_id=cast(
                            str,
                            foreign_prepared["change_set_id"],
                        ),
                        foreign_workflow=foreign_workflow,
                        allowed_codes=ISOLATION_CASE_EXPECTATIONS[case_id],
                        timeout=timeout,
                    )
                )
        for name in modified:
            snapshots[name].restore()
        restored = all(
            acquisition_paths.read_regular_file(
                snapshot.path,
                maximum=16_384,
            )
            == snapshot.payload
            for snapshot in snapshots.values()
        )
    finally:
        if not restored:
            for name in modified:
                snapshots[name].restore()

    client_a = cast(s10.Sprint10McpClient, session_a.start_sidecar())
    if "b" in affected:
        client_b = cast(s10.Sprint10McpClient, session_b.start_sidecar())
    _assert_committed_workflow(client_a, workflow_a, timeout)
    _assert_committed_workflow(client_b, workflow_b, timeout)
    after_approvals = {
        "a": client_a.elicitation_total,
        "b": client_b.elicitation_total,
    }
    # Restarted MCP clients have fresh counters; the fault probes themselves
    # must have observed zero elicitations, while the already committed
    # workflows are proven by their unchanged transaction/readback state.
    approvals_isolated = (
        all(result[2] for result in probe_results)
        and after_approvals["a"] == 0
        and after_approvals["b"]
        == (0 if "b" in affected else before_approvals["b"])
    )
    s9.require(
        probe_results
        and all(result[0] in ISOLATION_CASE_EXPECTATIONS[case_id] for result in probe_results),
        f"isolation case {case_id} diagnostic differs",
    )
    record = _isolation_case(
        case_id,
        probe_results[0][0],
        no_fallback=all(result[1] for result in probe_results),
        no_cross_project_data=all(result[1] for result in probe_results),
        no_cross_project_mutation=all(result[2] for result in probe_results),
        no_cross_project_approval=approvals_isolated,
        no_cross_project_transaction=all(
            result[3] for result in probe_results
        ),
        cleanup=restored,
    )
    return record, client_a, client_b


def _run_runtime(
    client: s9.ModelFreeMcpClient,
    expected: Mapping[str, Any],
    timeout: float,
) -> tuple[dict[str, Any], dict[str, Any]]:
    started, is_error, _ = client.tool("godot_run_project", {})
    s9.require(not is_error, f"runtime start failed: {started}")
    runtime_session_id = started.get("runtime_session_id")
    s9.require(
        isinstance(runtime_session_id, str)
        and RUNTIME_RE.fullmatch(runtime_session_id) is not None,
        "runtime start omitted its opaque session identity",
    )
    deadline = time.monotonic() + timeout
    last: dict[str, Any] = {}
    while time.monotonic() < deadline:
        tree, tree_error, _ = client.tool(
            "godot_get_runtime_tree",
            {"runtime_session_id": runtime_session_id, "limit": 200},
        )
        last = tree
        if (
            not tree_error
            and tree.get("project_id") == expected["project_id"]
            and tree.get("editor_session_id") == expected["editor_session_id"]
            and tree.get("runtime_session_id") == runtime_session_id
            and tree.get("state") == "running"
            and isinstance(tree.get("nodes"), list)
            and len(tree["nodes"]) >= 1
        ):
            return started, tree
        time.sleep(0.05)
    raise s9.WorkflowError(f"runtime tree did not become live: {last}")


def _prepare_apply(
    client: s10.Sprint10McpClient,
    *,
    label: str,
    project_root: Path,
    timeout: float,
) -> dict[str, Any]:
    current, history, state, _ = s9.stable_read(client, timeout)
    coordinates = s10._coordinates(current, history, state)
    root_id = cast(Mapping[str, str], coordinates["node_ids"])["."]
    node_name = f"Isolated{label.upper()}"
    source_before_prepare = _project_source_hashes(project_root)
    key = hashlib.sha256(f"s11:multi:{label}".encode()).hexdigest()[:32]
    arguments = {
        "project_id": coordinates["project_id"],
        "idempotency_key": f"idempotency:{key}",
        "coordinates": {
            "editor_session_id": coordinates["editor_session_id"],
            "scene_id": coordinates["scene_id"],
            "scene_revision": coordinates["scene_revision"],
            "operation_seq": coordinates["operation_seq"],
            "resource_revision": coordinates["resource_revision"],
            "script_graph_revision": coordinates["script_graph_revision"],
        },
        "operations": [
            {
                "kind": "create_node",
                "alias": "alias:created",
                "parent_node_id": root_id,
                "godot_type": "Node",
                "name": node_name,
            }
        ],
        "save_scope": {"paths": []},
        "validation_policy": {
            "rollback": "on_required_failure",
            "warnings": "allow",
            "runtime": "skip",
        },
    }
    prepared, is_error, _ = client.tool(
        "godot_prepare_change_set",
        arguments,
    )
    s9.require(not is_error, f"transaction prepare failed: {prepared}")
    s9.require(
        prepared.get("state") == "previewed"
        and s10.CHANGE_SET_RE.fullmatch(str(prepared.get("change_set_id")))
        is not None
        and s10.DIGEST_RE.fullmatch(str(prepared.get("preview_digest")))
        is not None
        and prepared.get("operation_count") == 1
        and source_before_prepare == _project_source_hashes(project_root),
        "prepared transaction identity/state differs",
    )
    client.expect_change_set(prepared)
    applied, apply_error, _ = client.tool(
        "godot_apply_transaction",
        {
            "transaction_id": prepared["change_set_id"],
            "preview_digest": prepared["preview_digest"],
            "expected_scene_revision": coordinates["scene_revision"],
            "expected_operation_seq": coordinates["operation_seq"],
        },
        timeout=max(timeout, 15.0),
    )
    approval = client.clear_approval()
    s9.require(
        not apply_error
        and approval is not None
        and approval.calls == 1
        and approval.transaction_id == prepared["change_set_id"]
        and approval.preview_digest == prepared["preview_digest"]
        and approval.scope == "change_set.atomic"
        and approval.risk == prepared["risk"]
        and approval.operation_kind == "change_set"
        and approval.decision == "accept",
        f"transaction apply or exact approval failed: {applied}",
    )
    terminal, _ = s10._wait_terminal(
        client,
        cast(str, prepared["change_set_id"]),
        max(timeout, 15.0),
    )
    report_id = terminal.get("validation_report_id")
    s9.require(
        terminal.get("state") == "committed"
        and isinstance(report_id, str)
        and s10.REPORT_RE.fullmatch(report_id) is not None,
        f"transaction did not commit with validation: {terminal}",
    )
    report = _validation_report(
        client,
        report_id,
        transaction_id=cast(str, prepared["change_set_id"]),
        preview_digest=cast(str, prepared["preview_digest"]),
    )
    after_current, after_history, after_state = s10._wait_scene_node(
        client,
        node_name,
        True,
        timeout,
    )
    after = s10._coordinates(after_current, after_history, after_state)
    s9.require(
        node_name in cast(set[str], after["node_paths"])
        and int(after["action_count"]) == int(coordinates["action_count"]) + 1,
        "transaction readback or native history differs",
    )
    return {
        "prepared": prepared,
        "terminal": terminal,
        "report": report,
        "report_id": report_id,
        "before": coordinates,
        "after": after,
        "node_name": node_name,
        "approval_provider": "mcp_action_only_form_v1",
        "approval_calls": 1,
    }


def _validation_report(
    client: s10.Sprint10McpClient,
    report_id: str,
    *,
    transaction_id: str,
    preview_digest: str,
) -> dict[str, Any]:
    first, is_error, _ = client.tool(
        "godot_get_validation_report",
        {"report_id": report_id, "page": 0},
    )
    page_count = first.get("page_count")
    s9.require(
        not is_error
        and first.get("report_id") == report_id
        and isinstance(page_count, int)
        and not isinstance(page_count, bool)
        and 1 <= page_count <= 4,
        f"validation report first page differs: {first}",
    )
    pages = [first]
    for page in range(1, page_count):
        item, page_error, _ = client.tool(
            "godot_get_validation_report",
            {"report_id": report_id, "page": page},
        )
        s9.require(not page_error, f"validation report page failed: {item}")
        pages.append(item)
    report_digest = first.get("report_digest")
    s9.require(
        isinstance(report_digest, str)
        and s10.DIGEST_RE.fullmatch(report_digest) is not None,
        "validation report digest differs",
    )
    content = ""
    for page, item in enumerate(pages):
        text = item.get("content")
        s9.require(
            item.get("report_id") == report_id
            and item.get("report_digest") == report_digest
            and item.get("page") == page
            and item.get("page_count") == page_count
            and isinstance(text, str)
            and item.get("content_bytes") == len(text.encode("utf-8"))
            and len(text.encode("utf-8")) <= 65_536,
            "validation report pagination differs",
        )
        content += text
    s9.require(
        len(content.encode("utf-8")) <= 262_144,
        "validation report exceeds retained byte bound",
    )
    parsed = s9.strict_json_text(content)
    checks = parsed.get("checks")
    outcomes = (
        {
            item.get("check"): item.get("outcome")
            for item in checks
            if isinstance(item, dict)
        }
        if isinstance(checks, list)
        else {}
    )
    s9.require(
        parsed.get("report_id") == report_id
        and parsed.get("change_set_id") == transaction_id
        and parsed.get("preview_digest") == preview_digest
        and parsed.get("report_digest") == report_digest
        and parsed.get("outcome") == "passed"
        and parsed.get("affected_closure_only") is True
        and set(outcomes)
        == {
            "diagnostics",
            "index_convergence",
            "intrinsic",
            "persistence",
            "reload_reparse",
            "runtime",
            "semantic_graph",
        }
        and all(
            outcomes[name] == "passed"
            for name in set(outcomes) - {"runtime"}
        )
        and outcomes.get("runtime") == "skipped",
        "validation report outcome/binding differs",
    )
    return {
        "report_digest": report_digest,
        "outcome": "passed",
        "page_count": page_count,
    }


def _wait_scene_cursor(
    client: s9.ModelFreeMcpClient,
    timeout: float,
) -> str:
    deadline = time.monotonic() + timeout
    last: dict[str, Any] = {}
    while time.monotonic() < deadline:
        page, is_error, _ = client.tool(
            "godot_get_scene_graph",
            {"scene": "res://main.tscn", "limit": 1},
        )
        last = page
        cursor = page.get("next_cursor")
        if not is_error and isinstance(cursor, str) and cursor:
            return cursor
        time.sleep(0.05)
    raise s9.WorkflowError(
        f"scene graph did not yield a cross-project cursor probe: {last}"
    )


def _foreign_error(
    client: s9.ModelFreeMcpClient,
    tool: str,
    arguments: Mapping[str, Any],
    *,
    kind: str,
    timeout: float,
) -> dict[str, Any]:
    expected = FOREIGN_EXPECTATIONS[kind]
    deadline = time.monotonic() + timeout
    content: dict[str, Any] = {}
    while time.monotonic() < deadline:
        content, is_error, _ = client.tool(tool, arguments)
        error = content.get("error")
        if (
            is_error
            and isinstance(error, dict)
            and error.get("code") == expected
        ):
            break
        if (
            not is_error
            or not isinstance(error, dict)
            or error.get("retryable") is not True
        ):
            raise s9.WorkflowError(
                f"foreign {kind} did not fail closed as {expected}: {content}"
            )
        time.sleep(0.05)
    else:
        raise s9.WorkflowError(
            f"foreign {kind} did not converge to {expected}: {content}"
        )
    return {
        "kind": kind,
        "error_code": expected,
        "is_error": True,
        "mutation": False,
    }


def _foreign_approval_error(
    client: s10.Sprint10McpClient,
    foreign_workflow: Mapping[str, Any],
    timeout: float,
) -> dict[str, Any]:
    prepared = cast(Mapping[str, Any], foreign_workflow["prepared"])
    before = cast(Mapping[str, Any], foreign_workflow["before"])
    elicitation_before = client.elicitation_total
    record = _foreign_error(
        client,
        "godot_apply_transaction",
        {
            "transaction_id": prepared["change_set_id"],
            "preview_digest": prepared["preview_digest"],
            "expected_scene_revision": before["scene_revision"],
            "expected_operation_seq": before["operation_seq"],
        },
        kind="approval_binding",
        timeout=timeout,
    )
    s9.require(
        client.approval is None
        and client.elicitation_total == elicitation_before,
        "foreign transaction reached or reused an approval boundary",
    )
    return record


def _transaction_status(
    client: s9.ModelFreeMcpClient,
    transaction_id: str,
    *,
    expected_state: str,
    timeout: float,
    context: str,
    report_id: str | None = None,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    status: dict[str, Any] = {}
    while time.monotonic() < deadline:
        status, is_error, _ = client.tool(
            "godot_get_transaction_status",
            {"transaction_id": transaction_id},
        )
        if (
            not is_error
            and status.get("change_set_id") == transaction_id
            and status.get("state") == expected_state
            and (
                report_id is None
                or status.get("validation_report_id") == report_id
            )
        ):
            return status
        time.sleep(0.05)
    raise s9.WorkflowError(
        f"{context} transaction status differs "
        f"(expected_transaction_id={transaction_id}, "
        f"expected_state={expected_state}, expected_report_id={report_id}, "
        f"is_error={is_error}, "
        f"change_set_id_match={status.get('change_set_id') == transaction_id}, "
        f"state_match={status.get('state') == expected_state}, "
        f"report_id_match="
        f"{report_id is None or status.get('validation_report_id') == report_id}): "
        f"{status}"
    )


def _undo_workflow(
    client: s10.Sprint10McpClient,
    workflow: Mapping[str, Any],
    timeout: float,
) -> dict[str, Any]:
    prepared = cast(Mapping[str, Any], workflow["prepared"])
    terminal = cast(Mapping[str, Any], workflow["terminal"])
    after = cast(Mapping[str, Any], workflow["after"])
    transaction_id = cast(str, prepared["change_set_id"])
    undo, is_error, _ = client.tool(
        "godot_undo_transaction",
        {
            "transaction_id": transaction_id,
            "expected_transaction_seq": terminal["transaction_seq"],
            "expected_scene_revision": after["scene_revision"],
            "expected_operation_seq": after["operation_seq"],
        },
    )
    s9.require(
        not is_error and undo.get("state") == "undone",
        f"targeted Undo failed: {undo}",
    )
    current, history, state = s10._wait_scene_node(
        client,
        cast(str, workflow["node_name"]),
        False,
        timeout,
    )
    restored = s10._coordinates(current, history, state)
    s9.require(
        workflow["node_name"] not in cast(set[str], restored["node_paths"]),
        "targeted Undo readback differs",
    )
    _transaction_status(
        client,
        transaction_id,
        expected_state="undone",
        timeout=timeout,
        context="targeted_undo",
    )
    return restored


def _stop_runtime(
    client: s9.ModelFreeMcpClient,
    runtime_session_id: str,
) -> bool:
    stopped, is_error, _ = client.tool(
        "godot_stop_project",
        {"runtime_session_id": runtime_session_id},
    )
    return not is_error and stopped.get("state") == "stopped"


def _run_editor_restart_case(
    *,
    session_a: s9.FixtureSession,
    client_a: s10.Sprint10McpClient,
    client_b: s10.Sprint10McpClient,
    workflow_a: Mapping[str, Any],
    workflow_b: Mapping[str, Any],
    timeout: float,
) -> tuple[dict[str, Any], s10.Sprint10McpClient]:
    before_a = _read(client_a, timeout)
    before_b = _read(client_b, timeout)
    approvals = (client_a.elicitation_total, client_b.elicitation_total)
    old_editor = cast(str, before_a["editor_session_id"])
    session_a.stop_editor(graceful=False)
    during_b = _read(client_b, timeout)
    new_editor = session_a.start_editor()
    deadline = time.monotonic() + timeout
    after_a: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        try:
            candidate = _read(client_a, min(timeout, 5.0))
        except s9.WorkflowError:
            time.sleep(0.05)
            continue
        if candidate.get("editor_session_id") == new_editor:
            after_a = candidate
            break
        time.sleep(0.05)
    if after_a is None:
        client_a = cast(s10.Sprint10McpClient, session_a.start_sidecar())
        after_a = _read(client_a, timeout)
    after_b = _read(client_b, timeout)
    prepared_a = cast(Mapping[str, Any], workflow_a["prepared"])
    old_status, old_error, _ = client_a.tool(
        "godot_get_transaction_status",
        {"transaction_id": prepared_a["change_set_id"]},
    )
    old_code = (
        old_status.get("error", {}).get("code")
        if isinstance(old_status.get("error"), dict)
        else None
    )
    old_transaction_closed = (
        (
            not old_error
            and old_status.get("state") in {"undone", "expired", "failed"}
        )
        or (
            old_error
            and old_code
            in {
                "transaction_not_found",
                "transaction_expired",
                "stale_editor_session",
            }
        )
    )
    prepared_b = cast(Mapping[str, Any], workflow_b["prepared"])
    _transaction_status(
        client_b,
        cast(str, prepared_b["change_set_id"]),
        expected_state="undone",
        timeout=timeout,
        context="editor_restart_sibling",
    )
    b_preserved = all(
        during_b[field] == before_b[field] == after_b[field]
        for field in (
            "project_id",
            "editor_session_id",
            "scene_id",
            "scene_revision",
            "operation_seq",
            "action_count",
        )
    )
    a_bound = (
        new_editor != old_editor
        and after_a["editor_session_id"] == new_editor
        and after_a["project_id"] == before_a["project_id"]
        and after_a["project_id"] != after_b["project_id"]
    )
    approval_preserved = (
        client_a.approval is None
        and client_b.approval is None
        and client_b.elicitation_total == approvals[1]
        and client_a.elicitation_total in {0, approvals[0]}
    )
    return (
        _isolation_case(
            "editor_restart",
            "editor_reconnected",
            no_fallback=a_bound,
            no_cross_project_data=a_bound and b_preserved,
            no_cross_project_mutation=b_preserved,
            no_cross_project_approval=approval_preserved,
            no_cross_project_transaction=old_transaction_closed,
            cleanup=(
                session_a.editor is not None
                and session_a.editor.poll() is None
            ),
        ),
        client_a,
    )


def _run_cache_rebuild_case(
    *,
    session_a: s9.FixtureSession,
    client_a: s10.Sprint10McpClient,
    client_b: s10.Sprint10McpClient,
    workflow_a: Mapping[str, Any],
    workflow_b: Mapping[str, Any],
    timeout: float,
) -> tuple[dict[str, Any], s10.Sprint10McpClient]:
    s9.require(
        session_a.project_root is not None
        and session_a.sidecar_process is not None,
        "cache rebuild fixture is incomplete",
    )
    before_a = _read(client_a, timeout)
    before_b = _read(client_b, timeout)
    approvals = (client_a.elicitation_total, client_b.elicitation_total)
    cache = session_a.project_root / ".godot/codex/index"
    held = session_a.project_root / ".godot/codex/.s11-index-held"
    s9.require(
        cache.is_dir()
        and not cache.is_symlink()
        and not held.exists(),
        "cache rebuild source differs",
    )
    session_a.sidecar_process.stop()
    cache.replace(held)
    rebuilt = False
    try:
        client_a = cast(s10.Sprint10McpClient, session_a.start_sidecar())
        status = _connection_status(
            client_a,
            cast(str, before_a["project_id"]),
            timeout,
        )
        rebuilt = (
            cache.is_dir()
            and not cache.is_symlink()
            and status["static_cache"]["condition"] == "online_current"
        )
        after_a = _read(client_a, timeout)
        during_b = _read(client_b, timeout)
        prepared_a = cast(Mapping[str, Any], workflow_a["prepared"])
        prepared_b = cast(Mapping[str, Any], workflow_b["prepared"])
        _transaction_status(
            client_a,
            cast(str, prepared_a["change_set_id"]),
            expected_state="undone",
            timeout=timeout,
            context="cache_rebuild_target",
        )
        _transaction_status(
            client_b,
            cast(str, prepared_b["change_set_id"]),
            expected_state="undone",
            timeout=timeout,
            context="cache_rebuild_sibling",
        )
    finally:
        if held.exists():
            if rebuilt and cache.is_dir() and not cache.is_symlink():
                shutil.rmtree(held)
            else:
                if cache.exists() and cache.is_dir() and not cache.is_symlink():
                    shutil.rmtree(cache)
                held.replace(cache)
    a_bound = (
        after_a["project_id"] == before_a["project_id"]
        and after_a["editor_session_id"] == before_a["editor_session_id"]
        and after_a["project_id"] != during_b["project_id"]
    )
    b_preserved = all(
        during_b[field] == before_b[field]
        for field in (
            "project_id",
            "editor_session_id",
            "scene_id",
            "scene_revision",
            "operation_seq",
            "action_count",
        )
    )
    approval_preserved = (
        client_a.approval is None
        and client_b.approval is None
        and client_a.elicitation_total == 0
        and client_b.elicitation_total == approvals[1]
    )
    return (
        _isolation_case(
            "cache_rebuild",
            "cache_rebuilt",
            no_fallback=rebuilt and a_bound,
            no_cross_project_data=a_bound and b_preserved,
            no_cross_project_mutation=b_preserved,
            no_cross_project_approval=approval_preserved,
            no_cross_project_transaction=True,
            cleanup=not held.exists(),
        ),
        client_a,
    )


def _project_record(
    coordinates: Mapping[str, Any],
    runtime_session_id: str,
    transaction_id: str,
    report_id: str,
) -> dict[str, Any]:
    return {
        "project_identity_sha256": _identity_digest(
            "project", cast(str, coordinates["project_id"])
        ),
        "editor_identity_sha256": _identity_digest(
            "editor", cast(str, coordinates["editor_session_id"])
        ),
        "runtime_identity_sha256": _identity_digest(
            "runtime", runtime_session_id
        ),
        "transaction_identity_sha256": _identity_digest(
            "transaction", transaction_id
        ),
        "validation_report_identity_sha256": _identity_digest(
            "validation_report", report_id
        ),
        "operation_seq": int(coordinates["operation_seq"]),
        "action_count": int(coordinates["action_count"]),
        "approval_provider": "mcp_action_only_form_v1",
        "validation_outcome": "passed",
        "readback": True,
        "targeted_undo": True,
    }


def validate_report(report: Any) -> dict[str, Any]:
    encoded = json.dumps(
        report,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    s9.require(0 < len(encoded) <= REPORT_LIMIT, "multi-project report is unbounded")
    root = _exact(
        report,
        {
            "schema_version",
            "status",
            "platform",
            "protocol",
            "bridge_rpc",
            "artifacts",
            "registry",
            "projects",
            "assertions",
            "source_integrity",
            "cleanup",
        },
        "multi-project report",
    )
    s9.require(
        root["schema_version"] == REPORT_SCHEMA
        and root["status"] == "passed"
        and root["platform"] == "macos-arm64"
        and root["protocol"] == s9.MCP_PROTOCOL
        and root["bridge_rpc"] == "1.8",
        "multi-project report header differs",
    )
    artifacts = _exact(
        root["artifacts"],
        {"godot_sha256", "sidecar_sha256"},
        "artifact binding",
    )
    s9.require(
        all(
            isinstance(value, str)
            and DIGEST_RE.fullmatch(value) is not None
            for value in artifacts.values()
        ),
        "artifact binding differs",
    )
    registry = _exact(root["registry"], {"tools", "digest"}, "registry proof")
    s9.require(
        registry["tools"] == 41
        and isinstance(registry["digest"], str)
        and DIGEST_RE.fullmatch(registry["digest"]) is not None,
        "registry proof differs",
    )
    projects = _exact(root["projects"], {"a", "b"}, "projects")
    identity_fields = {
        "project_identity_sha256",
        "editor_identity_sha256",
        "runtime_identity_sha256",
        "transaction_identity_sha256",
        "validation_report_identity_sha256",
        "operation_seq",
        "action_count",
        "approval_provider",
        "validation_outcome",
        "readback",
        "targeted_undo",
    }
    project_records = {
        label: _exact(projects[label], identity_fields, f"project {label}")
        for label in ("a", "b")
    }
    for label, project in project_records.items():
        for field in identity_fields:
            if field.endswith("_sha256"):
                s9.require(
                    isinstance(project[field], str)
                    and DIGEST_RE.fullmatch(project[field]) is not None,
                    f"project {label} identity digest differs",
                )
        for field in ("operation_seq", "action_count"):
            s9.require(
                isinstance(project[field], int)
                and not isinstance(project[field], bool)
                and 0 <= project[field] <= 9_007_199_254_740_991,
                f"project {label} {field} differs",
            )
        s9.require(
            project["approval_provider"] == "mcp_action_only_form_v1"
            and project["validation_outcome"] == "passed"
            and project["readback"] is True
            and project["targeted_undo"] is True,
            f"project {label} transaction proof differs",
        )
    assertions = _exact(
        root["assertions"],
        {
            "parallel_reads",
            "parallel_runtime_start",
            "parallel_prepare",
            "parallel_apply",
            "separate_approvals",
            "validation_readback",
            "targeted_undo",
            "identity_distinct",
            "foreign_bindings",
            "target_restart",
            "fault_matrix",
        },
        "isolation assertions",
    )
    for field in (
        "parallel_reads",
        "parallel_runtime_start",
        "parallel_prepare",
        "parallel_apply",
        "separate_approvals",
        "validation_readback",
        "targeted_undo",
    ):
        s9.require(assertions[field] is True, f"{field} proof differs")
    distinct = _exact(
        assertions["identity_distinct"],
        {"project", "editor", "runtime", "transaction", "validation_report"},
        "identity distinction",
    )
    for kind in distinct:
        digest_field = f"{kind}_identity_sha256"
        s9.require(
            distinct[kind] is True
            and project_records["a"][digest_field]
            != project_records["b"][digest_field],
            f"{kind} identity is not distinct",
        )
    bindings = assertions["foreign_bindings"]
    s9.require(
        isinstance(bindings, list) and len(bindings) == len(FOREIGN_EXPECTATIONS),
        "foreign binding coverage differs",
    )
    observed_kinds: set[str] = set()
    for index, binding in enumerate(bindings):
        record = _exact(
            binding,
            {"kind", "error_code", "is_error", "mutation"},
            f"foreign binding {index}",
        )
        kind = record["kind"]
        s9.require(
            isinstance(kind, str)
            and kind in FOREIGN_EXPECTATIONS
            and kind not in observed_kinds
            and record["error_code"] == FOREIGN_EXPECTATIONS[kind]
            and record["is_error"] is True
            and record["mutation"] is False,
            f"foreign binding {index} differs",
        )
        observed_kinds.add(kind)
    s9.require(
        observed_kinds == set(FOREIGN_EXPECTATIONS),
        "foreign binding kinds differ",
    )
    restart = _exact(
        assertions["target_restart"],
        {
            "target",
            "component",
            "process_instance_changed",
            "target_reconnected",
            "target_identity_preserved",
            "sibling_process_preserved",
            "sibling_editor_preserved",
            "sibling_runtime_preserved",
            "sibling_transaction_preserved",
            "sibling_validation_report_preserved",
            "sibling_readback_preserved",
            "sibling_revisions_preserved",
        },
        "target restart",
    )
    s9.require(
        restart["target"] == "a"
        and restart["component"] == "sidecar"
        and all(
            restart[field] is True
            for field in set(restart) - {"target", "component"}
        ),
        "target restart isolation differs",
    )
    _validated_fault_matrix(assertions["fault_matrix"])
    source = _exact(root["source_integrity"], {"a", "b"}, "source integrity")
    cleanup = _exact(
        root["cleanup"],
        {
            "runtime_a_stopped",
            "runtime_b_stopped",
            "editor_a_stopped",
            "editor_b_stopped",
            "sidecar_a_stopped",
            "sidecar_b_stopped",
        },
        "cleanup",
    )
    s9.require(
        source == {"a": True, "b": True}
        and all(value is True for value in cleanup.values()),
        "source integrity or cleanup differs",
    )
    return root


def run_live(
    godot: Path,
    sidecar: Path,
    timeout: float,
    *,
    expected_sidecar_sha256: str,
    expected_package_version: str,
) -> dict[str, Any]:
    observed_sidecar_sha256 = _artifact_digest(sidecar)
    observed_package_version = _probe_sidecar_version(sidecar, timeout)
    _require_package_authority(
        sidecar,
        expected_sha256=expected_sidecar_sha256,
        expected_version=expected_package_version,
        observed_sha256=observed_sidecar_sha256,
        observed_version=observed_package_version,
    )
    profile = _install_sprint11_client()
    session_a = s9.FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    session_b = s9.FixtureSession(godot=godot, sidecar=sidecar, timeout=timeout)
    body: dict[str, Any] | None = None
    fault_matrix: list[dict[str, Any]] = []
    runtime_ids: dict[str, str] = {}
    runtime_stopped = {"a": False, "b": False}
    try:
        with _managed_fixture(session_a), _managed_fixture(session_b):
            s9.require(
                session_a.project_root is not None
                and session_b.project_root is not None
                and session_a.client is not None
                and session_b.client is not None
                and session_a.sidecar_process is not None
                and session_b.sidecar_process is not None,
                "concurrent fixture pair is incomplete",
            )
            s9.require(
                isinstance(session_a.client, s10.Sprint10McpClient)
                and isinstance(session_b.client, s10.Sprint10McpClient),
                "production-equivalent approval clients are unavailable",
            )
            client_a = session_a.client
            client_b = session_b.client
            source_before = {
                "a": _project_source_hashes(session_a.project_root),
                "b": _project_source_hashes(session_b.project_root),
            }

            coordinates_a, coordinates_b = _parallel(
                lambda: _read(client_a, timeout),
                lambda: _read(client_b, timeout),
            )
            for label, coordinates in (
                ("a", coordinates_a),
                ("b", coordinates_b),
            ):
                s9.require(
                    PROJECT_RE.fullmatch(str(coordinates["project_id"])) is not None
                    and EDITOR_RE.fullmatch(str(coordinates["editor_session_id"]))
                    is not None,
                    f"project {label} live identities are malformed",
                )
            s9.require(
                coordinates_a["project_id"] != coordinates_b["project_id"]
                and coordinates_a["editor_session_id"]
                != coordinates_b["editor_session_id"],
                "project/editor identities collided",
            )
            status_a, status_b = _parallel(
                lambda: _connection_status(
                    client_a,
                    cast(str, coordinates_a["project_id"]),
                    timeout,
                ),
                lambda: _connection_status(
                    client_b,
                    cast(str, coordinates_b["project_id"]),
                    timeout,
                ),
            )
            discovery_a, discovery_b = _parallel(
                lambda: _discovery_binding(
                    session_a.project_root,
                    coordinates_a,
                ),
                lambda: _discovery_binding(
                    session_b.project_root,
                    coordinates_b,
                ),
            )
            cache_a = session_a.project_root / ".godot/codex/index"
            cache_b = session_b.project_root / ".godot/codex/index"
            s9.require(
                client_a.project_root.resolve(strict=True)
                == session_a.project_root.resolve(strict=True)
                and client_b.project_root.resolve(strict=True)
                == session_b.project_root.resolve(strict=True)
                and session_a.project_root.resolve(strict=True)
                != session_b.project_root.resolve(strict=True)
                and cache_a.resolve(strict=True)
                != cache_b.resolve(strict=True)
                and not cache_a.is_symlink()
                and not cache_b.is_symlink()
                and discovery_a["endpoint_sha256"]
                != discovery_b["endpoint_sha256"]
                and discovery_a["token_sha256"]
                != discovery_b["token_sha256"]
                and status_a["project_scope"] != status_b["project_scope"],
                "project config/root/discovery/token/cache bindings collided",
            )

            (run_a, tree_a), (run_b, tree_b) = _parallel(
                lambda: _run_runtime(client_a, coordinates_a, timeout),
                lambda: _run_runtime(client_b, coordinates_b, timeout),
            )
            runtime_ids = {
                "a": cast(str, run_a["runtime_session_id"]),
                "b": cast(str, run_b["runtime_session_id"]),
            }
            s9.require(
                runtime_ids["a"] != runtime_ids["b"],
                "runtime session identities collided",
            )

            workflow_a, workflow_b = _parallel(
                lambda: _prepare_apply(
                    client_a,
                    label="a",
                    project_root=session_a.project_root,
                    timeout=timeout,
                ),
                lambda: _prepare_apply(
                    client_b,
                    label="b",
                    project_root=session_b.project_root,
                    timeout=timeout,
                ),
            )
            prepared_a = cast(Mapping[str, Any], workflow_a["prepared"])
            prepared_b = cast(Mapping[str, Any], workflow_b["prepared"])
            transaction_ids = {
                "a": cast(str, prepared_a["change_set_id"]),
                "b": cast(str, prepared_b["change_set_id"]),
            }
            report_ids = {
                "a": cast(str, workflow_a["report_id"]),
                "b": cast(str, workflow_b["report_id"]),
            }
            s9.require(
                transaction_ids["a"] != transaction_ids["b"]
                and report_ids["a"] != report_ids["b"],
                "transaction or validation-report identities collided",
            )
            status_b_before = _transaction_status(
                client_b,
                transaction_ids["b"],
                expected_state="committed",
                timeout=timeout,
                context="pre_fault_sibling",
                report_id=report_ids["b"],
            )

            # Host/config coordinates are checked before process creation. A
            # swapped value therefore has no opportunity to discover another
            # root, open its cache/journal, or reach an approval boundary.
            fault_matrix.extend(
                [
                    _preflight_negative_case(
                        "swapped_config",
                        lambda: _require_launch_authority(
                            expected_root=session_a.project_root,
                            config_root=session_b.project_root,
                            argument_root=session_a.project_root,
                            cwd=session_a.project_root,
                        ),
                    ),
                    _preflight_negative_case(
                        "swapped_root",
                        lambda: _require_launch_authority(
                            expected_root=session_a.project_root,
                            config_root=session_a.project_root,
                            argument_root=session_b.project_root,
                            cwd=session_a.project_root,
                        ),
                    ),
                    _preflight_negative_case(
                        "swapped_cwd",
                        lambda: _require_launch_authority(
                            expected_root=session_a.project_root,
                            config_root=session_a.project_root,
                            argument_root=session_a.project_root,
                            cwd=session_b.project_root,
                        ),
                    ),
                    _preflight_negative_case(
                        "package_mismatch",
                        lambda: _require_package_authority(
                            sidecar,
                            expected_sha256=_different_digest(
                                observed_sidecar_sha256
                            ),
                            expected_version=observed_package_version,
                            observed_sha256=observed_sidecar_sha256,
                            observed_version=observed_package_version,
                        ),
                    ),
                    _preflight_negative_case(
                        "version_mismatch",
                        lambda: _require_package_authority(
                            sidecar,
                            expected_sha256=observed_sidecar_sha256,
                            expected_version=(
                                "1" if observed_package_version == "0" else "0"
                            ),
                            observed_sha256=observed_sidecar_sha256,
                            observed_version=observed_package_version,
                        ),
                    ),
                ]
            )

            metadata_faults: list[dict[str, Any]] = []
            for case_id in (
                "copied_discovery",
                "copied_token",
                "swapped_discovery",
                "swapped_token",
            ):
                record, client_a, client_b = _run_private_binding_fault(
                    case_id=case_id,
                    sidecar=sidecar,
                    session_a=session_a,
                    session_b=session_b,
                    client_a=client_a,
                    client_b=client_b,
                    workflow_a=workflow_a,
                    workflow_b=workflow_b,
                    timeout=timeout,
                )
                metadata_faults.append(record)

            foreign_project_arguments = dict(
                s9.operation_arguments("create_node", coordinates_a)
            )
            foreign_project_arguments["project_id"] = coordinates_b["project_id"]
            foreign_project_arguments["idempotency_key"] = (
                "idempotency:"
                + hashlib.sha256(b"s11:foreign-project").hexdigest()[:32]
            )
            _, cursor_b = _parallel(
                lambda: _wait_scene_cursor(client_a, timeout),
                lambda: _wait_scene_cursor(client_b, timeout),
            )
            foreign = [
                _foreign_error(
                    client_a,
                    "godot_prepare_create_node",
                    foreign_project_arguments,
                    kind="project_id",
                    timeout=timeout,
                ),
                _foreign_error(
                    client_a,
                    "godot_get_transaction_status",
                    {"transaction_id": transaction_ids["b"]},
                    kind="transaction_id",
                    timeout=timeout,
                ),
                _foreign_approval_error(client_a, workflow_b, timeout),
                _foreign_error(
                    client_a,
                    "godot_get_validation_report",
                    {"report_id": report_ids["b"], "page": 0},
                    kind="validation_report_id",
                    timeout=timeout,
                ),
                _foreign_error(
                    client_a,
                    "godot_get_runtime_tree",
                    {
                        "runtime_session_id": runtime_ids["b"],
                        "limit": 200,
                    },
                    kind="runtime_session_id",
                    timeout=timeout,
                ),
                _foreign_error(
                    client_a,
                    "godot_get_scene_graph",
                    {
                        "scene": "res://main.tscn",
                        "limit": 1,
                        "cursor": cursor_b,
                    },
                    kind="cursor",
                    timeout=timeout,
                ),
            ]

            before_fault_a_pid = session_a.sidecar_process.process.pid
            before_fault_b_pid = session_b.sidecar_process.process.pid
            session_a.sidecar_process.stop()
            sibling_during_fault = _read(client_b, timeout)
            sibling_tree, sibling_tree_error, _ = client_b.tool(
                "godot_get_runtime_tree",
                {
                    "runtime_session_id": runtime_ids["b"],
                    "limit": 200,
                },
            )
            s9.require(
                not sibling_tree_error
                and sibling_tree.get("runtime_session_id") == runtime_ids["b"]
                and sibling_tree.get("state") == "running",
                f"project B runtime failed during project A fault: {sibling_tree}",
            )
            status_b_during = _transaction_status(
                client_b,
                transaction_ids["b"],
                expected_state="committed",
                timeout=timeout,
                context="sidecar_fault_sibling",
                report_id=report_ids["b"],
            )
            report_b_during = _validation_report(
                client_b,
                report_ids["b"],
                transaction_id=transaction_ids["b"],
                preview_digest=cast(
                    str,
                    cast(Mapping[str, Any], workflow_b["prepared"])[
                        "preview_digest"
                    ],
                ),
            )
            (
                sibling_current_during,
                sibling_history_during,
                sibling_state_during,
            ) = s10._wait_scene_node(
                client_b,
                cast(str, workflow_b["node_name"]),
                True,
                timeout,
            )
            sibling_coordinates_during = s10._coordinates(
                sibling_current_during,
                sibling_history_during,
                sibling_state_during,
            )

            client_a = session_a.start_sidecar()
            s9.require(
                session_a.sidecar_process is not None
                and isinstance(client_a, s10.Sprint10McpClient),
                "project A sidecar did not restart",
            )
            after_fault_a_pid = session_a.sidecar_process.process.pid
            after_fault_b_pid = session_b.sidecar_process.process.pid
            reconnected_a = _read(client_a, timeout)
            sibling_after = _read(client_b, timeout)
            sibling_tree_after, sibling_tree_after_error, _ = client_b.tool(
                "godot_get_runtime_tree",
                {
                    "runtime_session_id": runtime_ids["b"],
                    "limit": 200,
                },
            )
            status_b_after = _transaction_status(
                client_b,
                transaction_ids["b"],
                expected_state="committed",
                timeout=timeout,
                context="sidecar_reconnect_sibling",
                report_id=report_ids["b"],
            )
            report_b_after = _validation_report(
                client_b,
                report_ids["b"],
                transaction_id=transaction_ids["b"],
                preview_digest=cast(
                    str,
                    cast(Mapping[str, Any], workflow_b["prepared"])[
                        "preview_digest"
                    ],
                ),
            )
            (
                sibling_current_after,
                sibling_history_after,
                sibling_state_after,
            ) = s10._wait_scene_node(
                client_b,
                cast(str, workflow_b["node_name"]),
                True,
                timeout,
            )
            sibling_coordinates_after = s10._coordinates(
                sibling_current_after,
                sibling_history_after,
                sibling_state_after,
            )
            recovered_a_tree, recovered_a_error, _ = client_a.tool(
                "godot_get_runtime_tree",
                {
                    "runtime_session_id": runtime_ids["a"],
                    "limit": 200,
                },
            )
            s9.require(
                not recovered_a_error
                and recovered_a_tree.get("runtime_session_id")
                == runtime_ids["a"],
                f"project A runtime did not reconnect: {recovered_a_tree}",
            )

            after_foreign_a = _read(client_a, timeout)
            after_foreign_b = _read(client_b, timeout)
            for label, committed, after in (
                (
                    "a",
                    cast(Mapping[str, Any], workflow_a["after"]),
                    after_foreign_a,
                ),
                (
                    "b",
                    cast(Mapping[str, Any], workflow_b["after"]),
                    after_foreign_b,
                ),
            ):
                s9.require(
                    after["project_id"] == committed["project_id"]
                    and after["editor_session_id"]
                    == committed["editor_session_id"]
                    and after["scene_id"] == committed["scene_id"]
                    and after["operation_seq"] == committed["operation_seq"]
                    and after["action_count"] == committed["action_count"],
                    f"project {label} changed during isolation probes",
                )

            restored_a, restored_b = _parallel(
                lambda: _undo_workflow(client_a, workflow_a, timeout),
                lambda: _undo_workflow(client_b, workflow_b, timeout),
            )
            runtime_stopped_a, runtime_stopped_b = _parallel(
                lambda: _stop_runtime(client_a, runtime_ids["a"]),
                lambda: _stop_runtime(client_b, runtime_ids["b"]),
            )
            runtime_stopped = {
                "a": runtime_stopped_a,
                "b": runtime_stopped_b,
            }
            s9.require(
                runtime_stopped_a and runtime_stopped_b,
                "concurrent runtime cleanup failed",
            )

            editor_restart, client_a = _run_editor_restart_case(
                session_a=session_a,
                client_a=client_a,
                client_b=client_b,
                workflow_a=workflow_a,
                workflow_b=workflow_b,
                timeout=timeout,
            )
            cache_rebuild, client_a = _run_cache_rebuild_case(
                session_a=session_a,
                client_a=client_a,
                client_b=client_b,
                workflow_a=workflow_a,
                workflow_b=workflow_b,
                timeout=timeout,
            )
            case_records = {
                record["case_id"]: record
                for record in [
                    *metadata_faults,
                    *fault_matrix,
                    editor_restart,
                    cache_rebuild,
                ]
            }
            s9.require(
                set(case_records) == set(ISOLATION_CASE_ORDER),
                "isolation fault matrix construction differs",
            )
            fault_matrix = [
                case_records[case_id] for case_id in ISOLATION_CASE_ORDER
            ]

            source_after = {
                "a": _project_source_hashes(session_a.project_root),
                "b": _project_source_hashes(session_b.project_root),
            }
            source_integrity = {
                label: source_before[label] == source_after[label]
                for label in ("a", "b")
            }
            s9.require(
                all(source_integrity.values()),
                "multi-project probes changed project-content bytes",
            )
            sibling_status_preserved = all(
                status.get("transaction_id") == transaction_ids["b"]
                and status.get("state") == "committed"
                and status.get("validation_report_id") == report_ids["b"]
                and status.get("preview_digest")
                == status_b_before.get("preview_digest")
                for status in (status_b_during, status_b_after)
            )
            body = {
                "schema_version": REPORT_SCHEMA,
                "status": "passed",
                "platform": "macos-arm64",
                "protocol": s9.MCP_PROTOCOL,
                "bridge_rpc": "1.8",
                "artifacts": {
                    "godot_sha256": _artifact_digest(godot),
                    "sidecar_sha256": _artifact_digest(sidecar),
                },
                "registry": {
                    "tools": 41,
                    "digest": "sha256:" + cast(str, profile["digest"]),
                },
                "projects": {
                    "a": _project_record(
                        coordinates_a,
                        runtime_ids["a"],
                        transaction_ids["a"],
                        report_ids["a"],
                    ),
                    "b": _project_record(
                        coordinates_b,
                        runtime_ids["b"],
                        transaction_ids["b"],
                        report_ids["b"],
                    ),
                },
                "assertions": {
                    "parallel_reads": True,
                    "parallel_runtime_start": True,
                    "parallel_prepare": True,
                    "parallel_apply": True,
                    "separate_approvals": (
                        workflow_a["approval_calls"] == 1
                        and workflow_b["approval_calls"] == 1
                        and workflow_a["approval_provider"]
                        == workflow_b["approval_provider"]
                        == "mcp_action_only_form_v1"
                    ),
                    "validation_readback": (
                        workflow_a["report"]["outcome"] == "passed"
                        and workflow_b["report"]["outcome"] == "passed"
                    ),
                    "targeted_undo": (
                        workflow_a["node_name"]
                        not in cast(set[str], restored_a["node_paths"])
                        and workflow_b["node_name"]
                        not in cast(set[str], restored_b["node_paths"])
                    ),
                    "identity_distinct": {
                        "project": True,
                        "editor": True,
                        "runtime": True,
                        "transaction": True,
                        "validation_report": True,
                    },
                    "foreign_bindings": foreign,
                    "target_restart": {
                        "target": "a",
                        "component": "sidecar",
                        "process_instance_changed": (
                            before_fault_a_pid != after_fault_a_pid
                        ),
                        "target_reconnected": True,
                        "target_identity_preserved": (
                            reconnected_a["project_id"]
                            == coordinates_a["project_id"]
                            and reconnected_a["editor_session_id"]
                            == coordinates_a["editor_session_id"]
                            and recovered_a_tree["runtime_session_id"]
                            == runtime_ids["a"]
                        ),
                        "sibling_process_preserved": (
                            before_fault_b_pid == after_fault_b_pid
                        ),
                        "sibling_editor_preserved": (
                            sibling_during_fault["editor_session_id"]
                            == coordinates_b["editor_session_id"]
                            and sibling_after["editor_session_id"]
                            == coordinates_b["editor_session_id"]
                        ),
                        "sibling_runtime_preserved": (
                            sibling_tree_after_error is False
                            and sibling_tree_after.get("runtime_session_id")
                            == runtime_ids["b"]
                        ),
                        "sibling_transaction_preserved": (
                            sibling_status_preserved
                        ),
                        "sibling_validation_report_preserved": (
                            report_b_during == workflow_b["report"]
                            and report_b_after == workflow_b["report"]
                        ),
                        "sibling_readback_preserved": (
                            sibling_coordinates_during
                            == workflow_b["after"]
                            and sibling_coordinates_after
                            == workflow_b["after"]
                        ),
                        "sibling_revisions_preserved": (
                            sibling_during_fault["operation_seq"]
                            == workflow_b["after"]["operation_seq"]
                            and sibling_after["operation_seq"]
                            == workflow_b["after"]["operation_seq"]
                            and sibling_during_fault["action_count"]
                            == workflow_b["after"]["action_count"]
                            and sibling_after["action_count"]
                            == workflow_b["after"]["action_count"]
                        ),
                    },
                    "fault_matrix": fault_matrix,
                },
                "source_integrity": source_integrity,
            }
    finally:
        # Managed fixtures own process cleanup. Runtime stop is attempted above
        # so the editor never abandons a game child during normal success.
        pass

    s9.require(body is not None, "multi-project report body is missing")
    body["cleanup"] = {
        "runtime_a_stopped": runtime_stopped["a"],
        "runtime_b_stopped": runtime_stopped["b"],
        "editor_a_stopped": (
            session_a.editor is None or session_a.editor.poll() is not None
        ),
        "editor_b_stopped": (
            session_b.editor is None or session_b.editor.poll() is not None
        ),
        "sidecar_a_stopped": (
            session_a.sidecar_process is None
            or session_a.sidecar_process.process.poll() is not None
        ),
        "sidecar_b_stopped": (
            session_b.sidecar_process is None
            or session_b.sidecar_process.process.poll() is not None
        ),
    }
    return validate_report(body)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--godot", type=Path, required=True)
    parser.add_argument("--sidecar", type=Path, required=True)
    parser.add_argument("--expected-sidecar-sha256", required=True)
    parser.add_argument("--expected-package-version", required=True)
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--report", type=Path)
    packaged_fixture.add_arguments(parser)
    arguments = parser.parse_args()
    try:
        packaged_fixture.activate_from_arguments(arguments)
        godot = arguments.godot.resolve(strict=True)
        sidecar = arguments.sidecar.resolve(strict=True)
        report = run_live(
            godot,
            sidecar,
            arguments.timeout,
            expected_sidecar_sha256=arguments.expected_sidecar_sha256,
            expected_package_version=arguments.expected_package_version,
        )
        encoded = json.dumps(
            report,
            ensure_ascii=False,
            indent=2,
            allow_nan=False,
            sort_keys=True,
        ) + "\n"
        if arguments.report is not None:
            acquisition_paths.atomic_write_new_file(
                arguments.report,
                encoded.encode("utf-8"),
                prefix=".s11-multi-project-report.",
            )
        print(encoded, end="")
        return 0
    except (
        OSError,
        json.JSONDecodeError,
        s9.WorkflowError,
        acquisition_paths.AcquisitionPathError,
        packaged_fixture.PackagedFixtureError,
    ) as error:
        print(f"Sprint 11 multi-project gate failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
