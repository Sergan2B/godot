#!/usr/bin/env python3
"""Run the test-only Sprint 9 approval probe through the installed Codex host."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]
MCP_WORKSPACE = REPO_ROOT / "godot-codex-mcp"
PROBE_BINARY = MCP_WORKSPACE / "target" / "debug" / "examples" / "approval_probe"
PROBE_SERVER = "godot_s9_approval_probe"
PROBE_TOOL = "godot_s9_approval_probe"


class ApprovalProbeError(RuntimeError):
    """Raised when the local host cannot prove the approval boundary."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ApprovalProbeError(message)


def toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=True)


def resolve_codex(explicit: Path | None) -> Path:
    if explicit is not None:
        candidate = explicit.expanduser().resolve()
    else:
        discovered = shutil.which("codex")
        if discovered is None:
            app_binary = Path("/Applications/ChatGPT.app/Contents/Resources/codex")
            require(app_binary.is_file(), "Codex CLI was not found")
            candidate = app_binary
        else:
            candidate = Path(discovered).resolve()
    require(candidate.is_file(), "Codex CLI path is not a regular file")
    require(os.access(candidate, os.X_OK), "Codex CLI path is not executable")
    return candidate


def build_probe() -> None:
    subprocess.run(
        [
            "cargo",
            "build",
            "--locked",
            "-p",
            "godot-codex-mcp-server",
            "--example",
            "approval_probe",
        ],
        cwd=MCP_WORKSPACE,
        check=True,
    )
    require(PROBE_BINARY.is_file(), "approval probe binary was not produced")


def validate_trace(path: Path) -> dict[str, Any]:
    require(path.is_file(), "Codex host did not produce an approval trace")
    if os.name == "posix":
        mode = stat.S_IMODE(path.stat().st_mode)
        require(mode == 0o600, f"approval trace mode is {mode:o}, expected 600")
    raw = path.read_bytes()
    require(len(raw) <= 8_192, "approval trace exceeds 8 KiB")
    require(str(REPO_ROOT).encode() not in raw, "approval trace leaks the project root")
    for forbidden in (b'"mac"', b'"session_token"', b'"proof"'):
        require(forbidden not in raw, f"approval trace contains forbidden field {forbidden!r}")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ApprovalProbeError("approval trace is not strict UTF-8 JSON") from error
    require(isinstance(value, dict), "approval trace root must be an object")
    require(value.get("schema_version") == "approval-probe/1.0", "trace schema differs")
    require(value.get("status") == "ok", f"approval failed: {value.get('code')}")
    require(value.get("code") == "approval_accepted", "approval was not accepted")
    require(value.get("action") == "accept", "elicitation action differs")
    require(value.get("confirmed") is True, "confirm=true was not received")
    require(value.get("receipt_eligible") is True, "receipt eligibility was not proven")
    require(value.get("project_mutated") is False, "probe reported a project mutation")
    host = value.get("host")
    require(isinstance(host, dict), "host metadata is missing")
    require(host.get("form_elicitation") is True, "host did not advertise form elicitation")
    require(isinstance(host.get("name"), str) and host["name"], "host name is missing")
    require(isinstance(host.get("version"), str) and host["version"], "host version is missing")
    require(
        host.get("protocol_version") == "2025-11-25",
        "Codex host negotiated an unexpected MCP protocol",
    )
    return value


def run_host(codex: Path, output: Path, timeout: int) -> None:
    require(sys.stdin.isatty() and sys.stdout.isatty(), "interactive TTY is required")
    server_prefix = f"mcp_servers.{PROBE_SERVER}"
    args = ["--output", str(output), "--timeout", str(timeout)]
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
        f"{server_prefix}.command={toml_string(str(PROBE_BINARY))}",
        "-c",
        f"{server_prefix}.args={json.dumps(args)}",
        "-c",
        f"{server_prefix}.enabled=true",
        (
            f"Call the {PROBE_TOOL} MCP tool exactly once. It is a test-only Sprint 9 "
            "approval probe and must not modify any project file. When its form elicitation "
            "appears, leave no field implicit: explicitly set confirm to true and submit it. "
            "After the tool finishes, reply with only: probe complete"
        ),
    ]
    completed = subprocess.run(command, cwd=REPO_ROOT, check=False)
    require(completed.returncode == 0, f"Codex host exited with {completed.returncode}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", type=Path, help="explicit Codex CLI path")
    parser.add_argument("--timeout", type=int, default=120)
    args = parser.parse_args()
    require(1 <= args.timeout <= 120, "--timeout must be between 1 and 120 seconds")
    return args


def main() -> int:
    try:
        args = parse_args()
        require(sys.platform == "darwin", "S9-01 host qualification requires macOS")
        require(platform.machine() == "arm64", "S9-01 host qualification requires arm64")
        codex = resolve_codex(args.codex)
        build_probe()
        with tempfile.TemporaryDirectory(prefix="godot-codex-s9-approval-") as temporary:
            output = Path(temporary) / "approval-trace.json"
            run_host(codex, output, args.timeout)
            trace = validate_trace(output)
        print(
            json.dumps(
                {
                    "status": "PASS",
                    "host": trace["host"],
                    "receipt_eligible": True,
                    "project_mutated": False,
                },
                sort_keys=True,
            )
        )
        return 0
    except (ApprovalProbeError, OSError, subprocess.SubprocessError) as error:
        print(f"S9 approval host probe failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
