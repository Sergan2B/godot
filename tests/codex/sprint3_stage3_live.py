#!/usr/bin/env python3
"""Run the local Sprint 3 Stage 3 resource bridge acceptance gate."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

from resource_graph_fixture import (
    GOLDEN_PATH,
    PROJECT_SOURCE,
    WORK_MARKER,
    apply_phase,
    restore_project,
    strict_json_load,
)

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent.parent
DRIVER = SCRIPT_DIR / "resource_graph_live_driver.gd"
DEFAULT_CLIENT = REPOSITORY_ROOT / "godot-codex-mcp" / "target" / "debug" / "examples" / "resource_graph_live"
EVIDENCE_PATH = SCRIPT_DIR / "evidence" / "sprint-3-stage-3-bridge.json"
PHASES = (
    "base",
    "rename_uid",
    "rename_uidless",
    "delete",
    "re_add",
    "reimport",
    "content_edit",
    "journal_gap",
)


class LiveGateError(RuntimeError):
    """Raised when the live resource bridge differs from its oracle."""


def wait_for(path: Path, timeout: float, description: str) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.05)
    raise LiveGateError(f"timed out waiting for {description}")


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def fixture_digest() -> str:
    digest = hashlib.sha256()
    for path in sorted(PROJECT_SOURCE.rglob("*")):
        if path.is_file() and ".godot" not in path.parts:
            digest.update(path.relative_to(PROJECT_SOURCE).as_posix().encode("utf-8"))
            digest.update(b"\0")
            digest.update(path.read_bytes())
            digest.update(b"\0")
    return digest.hexdigest()


def file_map(root: Path) -> dict[Path, str]:
    result: dict[Path, str] = {}
    for path in root.rglob("*"):
        if not path.is_file() or ".godot" in path.parts or path.name == WORK_MARKER:
            continue
        result[path.relative_to(root)] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def apply_prepared_diff(project: Path, prepared: Path) -> None:
    current = file_map(project)
    desired = file_map(prepared)
    for relative in sorted(current.keys() - desired.keys(), reverse=True):
        (project / relative).unlink()
    for relative in sorted(desired):
        if current.get(relative) == desired[relative]:
            continue
        source = prepared / relative
        destination = project / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        temporary = destination.with_name(destination.name + ".codex-stage3-tmp")
        shutil.copy2(source, temporary)
        os.replace(temporary, destination)
    for directory in sorted(
        (path for path in project.rglob("*") if path.is_dir() and ".godot" not in path.parts),
        key=lambda path: len(path.parts),
        reverse=True,
    ):
        try:
            directory.rmdir()
        except OSError:
            pass


def build_client(client: Path) -> None:
    result = subprocess.run(
        [
            "cargo",
            "build",
            "--manifest-path",
            str(REPOSITORY_ROOT / "godot-codex-mcp" / "Cargo.toml"),
            "-p",
            "godot-codex-bridge-client",
            "--example",
            "resource_graph_live",
        ],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=180,
    )
    if result.returncode != 0 or not client.exists():
        raise LiveGateError("could not build the resource_graph_live client")


def start_editor(
    godot: Path,
    project: Path,
    log_path: Path,
    mutation_count: int,
    evidence_telemetry: bool = False,
) -> tuple[subprocess.Popen[str], Any]:
    log = log_path.open("w", encoding="utf-8")
    command = [
        str(godot),
        "--editor",
        "--headless",
        "--verbose",
        "--path",
        str(project),
        "--script",
        str(DRIVER),
        "--",
        "--external-mutation",
        f"--mutation-count={mutation_count}",
    ]
    environment = os.environ.copy()
    if evidence_telemetry:
        environment["GODOT_CODEX_EVIDENCE_TELEMETRY"] = "1"
    process = subprocess.Popen(
        command,
        cwd=REPOSITORY_ROOT,
        env=environment,
        stdout=log,
        stderr=subprocess.STDOUT,
        text=True,
    )
    return process, log


def start_client(
    client: Path,
    project: Path,
    output_path: Path,
    error_path: Path,
    delta_count: int,
) -> tuple[subprocess.Popen[str], Any, Any]:
    output = output_path.open("w", encoding="utf-8")
    error = error_path.open("w", encoding="utf-8")
    command = [str(client), str(project), "--signal-ready", "--details"]
    if delta_count == 1:
        command.append("--poll-delta")
    elif delta_count > 1:
        command.append(f"--poll-deltas={delta_count}")
    process = subprocess.Popen(
        command,
        cwd=REPOSITORY_ROOT,
        stdout=output,
        stderr=error,
        text=True,
    )
    return process, output, error


def finish_process(process: subprocess.Popen[str], timeout: float, name: str) -> None:
    try:
        return_code = process.wait(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        process.kill()
        process.wait(timeout=5)
        raise LiveGateError(f"{name} timed out") from error
    if return_code != 0:
        raise LiveGateError(f"{name} exited with status {return_code}")


def stop_editor(process: subprocess.Popen[str], project: Path) -> None:
    (project / ".godot" / "codex-resource-live-done").touch()
    try:
        process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def base_oracle() -> dict[str, Any]:
    golden = strict_json_load(GOLDEN_PATH)
    return next(phase for phase in golden["phases"] if phase["name"] == "base")


def verify_snapshot(output: dict[str, Any], oracle: dict[str, Any]) -> None:
    if output.get("protocol_version") != "1.2" or output.get("resource_graph_available") is not True:
        raise LiveGateError("Bridge RPC 1.2 resource capabilities were not negotiated")
    snapshot = output["snapshot"]
    expected_paths = sorted(resource["path"] for resource in oracle["resources"])
    if snapshot["resource_paths"] != expected_paths:
        raise LiveGateError("live resource path set differs from the Stage 1 oracle")
    if snapshot["dependency_count"] != len(oracle["dependencies"]):
        raise LiveGateError("live dependency count differs from the Stage 1 oracle")
    if snapshot["diagnostic_count"] != len(oracle["diagnostics"]):
        raise LiveGateError("live diagnostic count differs from the Stage 1 oracle")
    expected_dependency_facts = sorted(
        (
            dependency.get("target_uid") or "",
            dependency["fallback_path"],
            dependency["resolution"],
        )
        for dependency in oracle["dependencies"]
    )
    observed_dependency_facts = sorted(
        (
            dependency.get("target_uid") or "",
            dependency["fallback_path"],
            dependency["resolution"],
        )
        for dependency in snapshot["dependencies"]
    )
    if observed_dependency_facts != expected_dependency_facts:
        raise LiveGateError("live dependency facts differ from the Stage 1 oracle")
    if sorted(diagnostic["code"] for diagnostic in snapshot["diagnostics"]) != sorted(
        diagnostic["code"] for diagnostic in oracle["diagnostics"]
    ):
        raise LiveGateError("live diagnostic codes differ from the Stage 1 oracle")


def operation_matches(operation: dict[str, Any], kind: str, path: str) -> bool:
    if operation.get("kind") != kind:
        return False
    if kind == "remove":
        return operation.get("path") == path
    if kind == "move":
        return operation.get("to_path") == path
    operation_path: object = operation.get("value", {}).get("resource", {}).get("path")
    return operation_path == path


def verify_phase_delta(phase: str, output: dict[str, Any]) -> list[dict[str, Any]]:
    deltas = output.get("deltas") or [output.get("delta")]
    if any(delta is None for delta in deltas):
        raise LiveGateError(f"{phase} did not produce a delta result")
    if phase == "journal_gap":
        if len(deltas) != 1 or deltas[0].get("status") != "gap":
            raise LiveGateError("journal_gap did not require a fresh snapshot")
        return deltas
    for delta in deltas:
        if delta.get("status") != "batch" or delta["resource_revision"] != delta["previous_resource_revision"] + 1:
            raise LiveGateError(f"{phase} broke delta revision continuity")
    operations = [operation for delta in deltas for operation in delta.get("operations", [])]
    expected = {
        "rename_uid": ("move", "res://resources/renamed/shared_leaf_renamed.tres"),
        "rename_uidless": ("upsert", "res://resources/renamed/twin_a.tres"),
        "delete": ("remove", "res://resources/unique_leaf.res"),
        "reimport": ("reimport", "res://assets/imported_icon.svg"),
        "content_edit": ("upsert", "res://resources/shared_leaf.tres"),
    }
    if phase == "re_add":
        required = (
            ("remove", "res://resources/unique_leaf.res"),
            ("upsert", "res://resources/unique_leaf.res"),
        )
        if not all(
            any(operation_matches(operation, kind, path) for operation in operations) for kind, path in required
        ):
            raise LiveGateError("re_add did not produce remove followed by upsert")
    elif phase in expected:
        kind, path = expected[phase]
        if not any(operation_matches(operation, kind, path) for operation in operations):
            raise LiveGateError(f"{phase} did not produce its required {kind} operation")
        if phase == "rename_uidless" and not any(
            operation_matches(operation, "remove", "res://resources/twin_a.tres") for operation in operations
        ):
            raise LiveGateError("UID-less rename did not remove the previous path identity")
    return deltas


def create_gap_resources(project: Path, count: int = 1800) -> None:
    root = project / "generated_gap"
    root.mkdir(parents=True, exist_ok=True)
    for index in range(count):
        path = root / f"item_{index:04d}.tres"
        path.write_text(
            f'[gd_resource type="Resource" format=3]\n\n[resource]\nresource_name = "journal_gap_{index:04d}"\n',
            encoding="utf-8",
        )


def run_phase(
    phase: str,
    godot: Path,
    client: Path,
    run_root: Path,
    oracle: dict[str, Any],
) -> dict[str, Any]:
    phase_root = run_root / f"p{PHASES.index(phase)}"
    project = phase_root / "p"
    prepared = phase_root / "m"
    phase_root.mkdir(parents=True)
    shutil.copytree(PROJECT_SOURCE, project, ignore=shutil.ignore_patterns(".godot"))
    mutation_count = 2 if phase == "re_add" else 1
    editor, editor_log = start_editor(godot, project, phase_root / "godot.log", mutation_count)
    client_process: subprocess.Popen[str] | None = None
    client_output = None
    client_error = None
    try:
        wait_for(project / ".godot" / "codex" / "bridge.json", 30, f"{phase} discovery")
        delta_count = 0 if phase == "base" else mutation_count
        client_process, client_output, client_error = start_client(
            client,
            project,
            phase_root / "client.json",
            phase_root / "client.err",
            delta_count,
        )
        wait_for(
            project / ".godot" / "codex-resource-live-client-ready",
            30,
            f"{phase} snapshot",
        )
        if phase == "re_add":
            resource = project / "resources" / "unique_leaf.res"
            resource.unlink()
            (project / ".godot" / "codex-resource-live-mutate-1").touch()
            wait_for(
                project / ".godot" / "codex-resource-live-delta-1-ready",
                60,
                "re_add removal delta",
            )
            shutil.copy2(PROJECT_SOURCE / "resources" / "unique_leaf.res", resource)
            (project / ".godot" / "codex-resource-live-mutate-2").touch()
        elif phase == "journal_gap":
            create_gap_resources(project)
            (project / ".godot" / "codex-resource-live-mutate").touch()
        elif phase != "base":
            restore_project(prepared)
            apply_phase(prepared, phase, godot if phase == "rename_uid" else None)
            apply_prepared_diff(project, prepared)
            (project / ".godot" / "codex-resource-live-mutate").touch()
        else:
            (project / ".godot" / "codex-resource-live-mutate").touch()
        finish_process(client_process, 120, f"{phase} client")
        client_output.close()
        client_error.close()
        client_output = None
        client_error = None
        output = json.loads((phase_root / "client.json").read_text(encoding="utf-8"))
        verify_snapshot(output, oracle)
        deltas = [] if phase == "base" else verify_phase_delta(phase, output)
        return {
            "phase": phase,
            "protocol_version": output["protocol_version"],
            "resource_count": output["snapshot"]["resource_count"],
            "dependency_count": output["snapshot"]["dependency_count"],
            "diagnostic_count": output["snapshot"]["diagnostic_count"],
            "snapshot_checksum": output["snapshot"]["checksum"],
            "delta_statuses": [delta.get("status", "batch") for delta in deltas],
            "delta_operation_counts": [delta.get("operation_count", 0) for delta in deltas],
            "passed": True,
        }
    finally:
        if client_process is not None and client_process.poll() is None:
            client_process.kill()
            client_process.wait(timeout=5)
        if client_output is not None:
            client_output.close()
        if client_error is not None:
            client_error.close()
        stop_editor(editor, project)
        editor_log.close()
        log_text = (phase_root / "godot.log").read_text(encoding="utf-8", errors="replace")
        if "SCRIPT ERROR" in log_text:
            raise LiveGateError(f"{phase} editor reported a script error")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--godot", type=Path, required=True)
    parser.add_argument("--client", type=Path, default=DEFAULT_CLIENT)
    parser.add_argument("--evidence", type=Path, default=EVIDENCE_PATH)
    arguments = parser.parse_args()
    godot = arguments.godot.resolve()
    client = arguments.client.resolve()
    if not godot.is_file():
        raise LiveGateError("Godot executable does not exist")
    build_client(client)
    before = fixture_digest()
    oracle = base_oracle()
    # macOS Unix-domain sockets have a small sockaddr_un path ceiling.
    with tempfile.TemporaryDirectory(prefix="cs3-", dir="/tmp") as temporary:
        run_root = Path(temporary)
        phase_results = [run_phase(phase, godot, client, run_root, oracle) for phase in PHASES]
    after = fixture_digest()
    if before != after:
        raise LiveGateError("canonical resource graph fixture bytes changed")
    evidence = {
        "schema_version": 1,
        "stage": "Sprint 3 Stage 3",
        "platform": "macOS arm64",
        "execution": "local",
        "remote_ci": False,
        "fixture_digest_before": before,
        "fixture_digest_after": after,
        "canonical_fixture_unchanged": True,
        "phases": phase_results,
        "result": "passed",
    }
    arguments.evidence.parent.mkdir(parents=True, exist_ok=True)
    arguments.evidence.write_text(canonical_json(evidence), encoding="utf-8")
    print(canonical_json(evidence), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
