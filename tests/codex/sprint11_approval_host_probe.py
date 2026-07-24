#!/usr/bin/env python3
"""Exercise one real Codex host form outcome without touching a Godot project."""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import signal
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path
from typing import Any

try:
    from tests.codex import sprint11_acquisition_paths as acquisition_paths
    from tests.codex import sprint9_approval_host_probe as sprint9_probe
except ModuleNotFoundError:  # Direct ``python tests/codex/...`` invocation.
    import sprint11_acquisition_paths as acquisition_paths
    import sprint9_approval_host_probe as sprint9_probe  # type: ignore[no-redef]

REPO_ROOT = Path(__file__).resolve().parents[2]
PROBE_BINARY = sprint9_probe.PROBE_BINARY
PROBE_SERVER = "godot_s11_approval_probe"
PROBE_TOOL = sprint9_probe.PROBE_TOOL
SUPPORTED_FORM_PROTOCOLS = sprint9_probe.SUPPORTED_FORM_PROTOCOLS
DECISIONS = ("accept", "decline", "cancel", "timeout")
SERVER_NAME_RE = re.compile(r"[A-Za-z0-9_-]{1,64}\Z")
OUTCOME = {
    "accept": ("approval_accepted", "ok", True, True),
    "decline": ("approval_declined", "error", False, None),
    "cancel": ("approval_cancelled", "error", False, None),
    "timeout": ("approval_timeout", "error", False, None),
}


class ApprovalProbeError(RuntimeError):
    """The selected host outcome was not proven exactly."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ApprovalProbeError(message)


def _strict_json(raw: bytes) -> dict[str, Any]:
    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in items:
            require(key not in value, "approval trace has a duplicate member")
            value[key] = item
        return value

    def constant(_name: str) -> None:
        raise ApprovalProbeError("approval trace has a non-finite number")

    try:
        value = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=pairs,
            parse_constant=constant,
        )
    except (UnicodeError, json.JSONDecodeError) as error:
        raise ApprovalProbeError("approval trace is not strict JSON") from error
    require(isinstance(value, dict), "approval trace root is not an object")
    return value


def validate_trace(path: Path, decision: str) -> dict[str, Any]:
    require(decision in DECISIONS, "approval decision differs")
    require(path.is_file() and not path.is_symlink(), "approval trace is unavailable")
    if os.name == "posix":
        require(
            stat.S_IMODE(path.stat().st_mode) == 0o600,
            "approval trace permissions differ",
        )
    raw = path.read_bytes()
    require(len(raw) <= 8_192, "approval trace exceeds 8 KiB")
    require(str(REPO_ROOT).encode() not in raw, "approval trace leaks project root")
    for forbidden in (
        b'"mac"',
        b'"nonce"',
        b'"proof"',
        b'"receipt"',
        b'"session_token"',
        b'"source_text"',
    ):
        require(forbidden not in raw, "approval trace contains opaque/private material")
    value = _strict_json(raw)
    require(
        set(value)
        == {
            "schema_version",
            "status",
            "code",
            "host",
            "action",
            "confirmed",
            "receipt_eligible",
            "project_mutated",
        },
        "approval trace fields differ",
    )
    code, status, receipt_eligible, confirmed = OUTCOME[decision]
    require(value["schema_version"] == "approval-probe/1.0", "trace schema differs")
    require(value["status"] == status and value["code"] == code, "outcome code differs")
    require(value["action"] == decision, "form action differs")
    require(value["confirmed"] is confirmed, "confirmation projection differs")
    require(
        value["receipt_eligible"] is receipt_eligible,
        "receipt eligibility differs",
    )
    require(value["project_mutated"] is False, "approval probe mutated the project")
    host = value["host"]
    require(
        isinstance(host, dict)
        and set(host) == {
            "name",
            "version",
            "protocol_version",
            "form_elicitation",
        },
        "host projection differs",
    )
    require(
        isinstance(host["name"], str)
        and host["name"]
        and isinstance(host["version"], str)
        and host["version"]
        and host["protocol_version"] in SUPPORTED_FORM_PROTOCOLS
        and host["form_elicitation"] is True,
        "host did not prove standard form support",
    )
    return value


def _instruction(decision: str) -> str:
    actions = {
        "accept": "choose the form's Allow action",
        "decline": "use the form's decline action without submitting any content",
        "cancel": "use the form's cancel action without submitting any content",
        "timeout": (
            "leave the form unanswered; do not accept, decline, cancel, or submit content"
        ),
    }
    return (
        f"Call the {PROBE_TOOL} MCP tool exactly once. It is a test-only Sprint 11 "
        "form lifecycle probe and cannot modify project files. When the form appears, "
        f"{actions[decision]}. Do not call any other MCP tool. After the tool returns, "
        "reply with only: probe complete"
    )


def configured_mcp_server_names(
    _codex: Path,
    *,
    config_path: Path | None = None,
) -> tuple[str, ...]:
    """Return enabled names backed by complete user config tables.

    ``codex mcp list`` also contains connector/plugin servers. Overriding only
    ``enabled`` for one of those creates an incomplete table and can make the
    host reject its transport, so only tables already present in config.toml
    are eligible for isolation overrides.
    """

    if config_path is None:
        configured_home = os.environ.get("CODEX_HOME")
        root = Path(configured_home) if configured_home else Path.home() / ".codex"
        config_path = root / "config.toml"
    try:
        raw = config_path.read_bytes()
    except OSError as error:
        raise ApprovalProbeError("Codex config could not be read") from error
    require(len(raw) <= 1_048_576, "Codex config exceeds the probe bound")
    try:
        value = tomllib.loads(raw.decode("utf-8"))
    except (UnicodeError, tomllib.TOMLDecodeError) as error:
        raise ApprovalProbeError("Codex config is invalid") from error
    servers = value.get("mcp_servers", {})
    require(isinstance(servers, dict) and len(servers) <= 128, "Codex MCP config differs")
    names: list[str] = []
    for name, item in servers.items():
        require(
            isinstance(name, str) and SERVER_NAME_RE.fullmatch(name) is not None,
            "Codex MCP server name is unsafe",
        )
        require(isinstance(item, dict), "Codex MCP config table differs")
        if name != PROBE_SERVER and item.get("enabled", True) is not False:
            names.append(name)
    return tuple(sorted(set(names)))


def _stop_host(process: subprocess.Popen[bytes]) -> None:
    for _ in range(2):
        if process.poll() is not None:
            return
        process.send_signal(signal.SIGINT)
        time.sleep(0.25)
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)


def run_host(
    codex: Path,
    output: Path,
    *,
    decision: str,
    timeout: int,
    disabled_servers: tuple[str, ...] = (),
) -> None:
    require(decision in DECISIONS, "approval decision differs")
    require(sys.stdin.isatty() and sys.stdout.isatty(), "interactive TTY is required")
    require(output.is_absolute() and not output.exists(), "output must be a new absolute path")
    server_prefix = f"mcp_servers.{PROBE_SERVER}"
    command = [
        str(codex),
        "--no-alt-screen",
        "-C",
        str(REPO_ROOT),
        "-a",
        "on-request",
        "-s",
        "read-only",
        "-c",
        f"{server_prefix}.command={sprint9_probe.toml_string(str(PROBE_BINARY))}",
        "-c",
        (
            f"{server_prefix}.args="
            + json.dumps(
                [
                    "--output",
                    str(output),
                    "--timeout",
                    str(timeout),
                    "--action-only",
                ]
            )
        ),
        "-c",
        f"{server_prefix}.enabled=true",
    ]
    for name in disabled_servers:
        require(SERVER_NAME_RE.fullmatch(name) is not None, "unsafe MCP server name")
        command.extend(["-c", f"mcp_servers.{name}.enabled=false"])
    command.append(_instruction(decision))
    process = subprocess.Popen(command, cwd=REPO_ROOT)
    deadline = time.monotonic() + timeout + 180
    trace_seen_at: float | None = None
    while process.poll() is None:
        if output.is_file():
            trace_seen_at = trace_seen_at or time.monotonic()
            if time.monotonic() - trace_seen_at >= 1:
                _stop_host(process)
                break
        if time.monotonic() >= deadline:
            _stop_host(process)
            raise ApprovalProbeError("Codex host probe exceeded its deadline")
        time.sleep(0.1)
    require(output.is_file(), "Codex host exited before producing an approval trace")


def parse_args(arguments: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", type=Path)
    parser.add_argument("--decision", required=True, choices=DECISIONS)
    parser.add_argument("--timeout", type=int, default=30)
    parser.add_argument("--output", type=Path)
    result = parser.parse_args(arguments)
    require(1 <= result.timeout <= 120, "--timeout must be between 1 and 120")
    return result


def main(arguments: list[str] | None = None) -> int:
    temporary: tempfile.TemporaryDirectory[str] | None = None
    staged_output: acquisition_paths.StagedFile | None = None
    try:
        options = parse_args(arguments)
        require(sys.platform == "darwin", "host qualification requires macOS")
        require(platform.machine() == "arm64", "host qualification requires arm64")
        codex = sprint9_probe.resolve_codex(options.codex)
        sprint9_probe.build_probe()
        disabled_servers = configured_mcp_server_names(codex)
        if options.output is None:
            temporary = tempfile.TemporaryDirectory(
                prefix=f"godot-codex-s11-{options.decision}-"
            )
            output = Path(temporary.name) / "approval-trace.json"
        else:
            staged_output = acquisition_paths.prepare_staged_file(
                options.output.expanduser(),
                prefix=f".s11-human-{options.decision}.",
            )
            output = staged_output.staged_path
        run_host(
            codex,
            output,
            decision=options.decision,
            timeout=options.timeout,
            disabled_servers=disabled_servers,
        )
        trace = validate_trace(output, options.decision)
        if staged_output is not None:
            staged_output.publish()
        print(
            json.dumps(
                {
                    "status": "PASS",
                    "decision": options.decision,
                    "host": trace["host"],
                    "receipt_eligible": trace["receipt_eligible"],
                    "project_mutated": False,
                    "trace_retained": options.output is not None,
                },
                sort_keys=True,
            )
        )
        return 0
    except (
        ApprovalProbeError,
        OSError,
        subprocess.SubprocessError,
        acquisition_paths.AcquisitionPathError,
    ) as error:
        print(f"S11 approval host probe failed: {error}", file=sys.stderr)
        return 1
    finally:
        if temporary is not None:
            temporary.cleanup()
        if (
            staged_output is not None
            and staged_output.staging_directory.exists()
        ):
            shutil.rmtree(staged_output.staging_directory, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
