from __future__ import annotations

import json
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

from tests.codex import sprint11_host_app_server as app_server


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
REGISTRY = REPOSITORY_ROOT / "godot-codex-mcp/product/registry-profile.v1.json"
PROJECT_ID = "project:sha256:" + "a" * 64


class HostAppServerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.project = self.root / "project"
        self.project.mkdir()
        (self.project / "project.godot").write_text(
            '[application]\nconfig/name="Fixture"\n',
            encoding="utf-8",
        )
        self.launcher = self.root / "godot-codex"
        self.launcher.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.launcher.chmod(0o700)
        self.log = self.root / "server-log.json"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _fake_server(self, mode: str = "ok") -> Path:
        registry = json.loads(REGISTRY.read_text(encoding="utf-8"))
        client = self.root / f"fake-codex-{mode}"
        source = f'''#!{sys.executable}
import json, os, signal, sys, time
MODE = {mode!r}
LOG = {str(self.log)!r}
PROJECT = {str(self.project)!r}
PROJECT_ID = {PROJECT_ID!r}
TOOLS = {registry["tools"]!r}
RESOURCES = {registry["fixed_resources"]!r}
TEMPLATES = {registry["resource_templates"]!r}
methods = []
def save():
    with open(LOG, "w", encoding="utf-8") as stream:
        json.dump({{"methods": methods, "argv": sys.argv[1:], "cwd": os.getcwd(), "codex_home": os.environ.get("CODEX_HOME")}}, stream)
def send(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\\n")
    sys.stdout.flush()
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    methods.append(method)
    save()
    if MODE == "crash":
        sys.exit(9)
    if MODE == "timeout":
        time.sleep(600)
    if MODE == "malformed":
        sys.stdout.write("{{bad\\n")
        sys.stdout.flush()
        continue
    if MODE == "oversized":
        sys.stdout.write("{{\\\"value\\\":\\\"" + "x" * (2 * 1024 * 1024 + 32) + "\\\"}}\\n")
        sys.stdout.flush()
        continue
    if MODE == "notification_flood":
        for index in range(4097):
            send({{"jsonrpc": "2.0", "method": "fixture/progress", "params": {{"index": index}}}})
    if method == "initialize":
        assert request["params"]["capabilities"]["mcpServerOpenaiFormElicitation"] is True
        send({{"jsonrpc": "2.0", "id": request["id"], "result": {{"userAgent": "fixture", "platformFamily": "unix", "platformOs": "macos", "codexHome": "redacted"}}}})
    elif method == "notifications/initialized":
        continue
    elif method == "thread/start":
        assert request["params"] == {{"cwd": PROJECT, "ephemeral": True, "sandbox": "read-only", "approvalPolicy": "never"}}
        if MODE == "duplicate_response":
            send({{"jsonrpc": "2.0", "id": 1, "result": {{}}}})
        send({{"jsonrpc": "2.0", "id": request["id"], "result": {{"thread": {{"id": "thread-fixture"}}}}}})
    elif method == "mcpServerStatus/list":
        assert request["params"] == {{"threadId": "thread-fixture", "detail": "full"}}
        tools = TOOLS[:-1] if MODE == "registry_mismatch" else TOOLS
        result = {{
            "data": [{{
                "name": "godot_editor",
                "authStatus": "unsupported",
                "tools": {{name: {{"name": name, "inputSchema": {{}}}} for name in tools}},
                "resources": [{{"name": uri, "uri": uri}} for uri in RESOURCES],
                "resourceTemplates": [{{"name": uri, "uriTemplate": uri}} for uri in TEMPLATES]
            }}],
            "nextCursor": None
        }}
        send({{"jsonrpc": "2.0", "id": request["id"], "result": result}})
    elif method == "mcpServer/tool/call":
        params = request["params"]
        assert params["server"] == "godot_editor"
        assert params["threadId"] == "thread-fixture"
        assert params["arguments"] == {{}}
        if params["tool"] == "godot_get_connection_status":
            payload = {{
                "schema_version": "godot-connection-status/1.1",
                "status": "ready",
                "project_scope": "b" * 64 if MODE == "wrong_scope" else "a" * 64,
                "bridge": {{"condition": "ready", "negotiated_protocol": "1.8"}},
                "static_cache": {{"condition": "online_current"}}
            }}
        elif params["tool"] == "godot_get_current_scene":
            payload = {{
                "project_id": PROJECT_ID,
                "snapshot_id": "snapshot:fixture",
                "freshness": "current",
                "evidence": [] if MODE == "missing_evidence" else [{{"source": "live_editor_snapshot", "freshness": "current"}}]
            }}
        else:
            raise AssertionError(params["tool"])
        send({{
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {{
                "content": [],
                "isError": MODE == "tool_error",
                "structuredContent": payload
            }}
        }})
save()
if MODE == "ignore_shutdown":
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    while True:
        time.sleep(60)
'''
        client.write_text(source, encoding="utf-8")
        client.chmod(0o700)
        return client

    def _options(self, mode: str = "ok", *, timeout: float = 3.0) -> app_server.SmokeOptions:
        client = self._fake_server(mode)
        digest = app_server.safe_file_digest(client, label="fake client")[0]
        return app_server.SmokeOptions(
            client=client,
            expected_client_sha256=digest,
            launcher=self.launcher,
            project_root=self.project,
            expected_project_id=PROJECT_ID,
            expected_registry=REGISTRY,
            output=self.root / f"smoke-{mode}.json",
            surfaces=("app", "cli"),
            timeout_seconds=timeout,
        )

    def test_smoke_uses_direct_app_server_mcp_calls_without_turn_start(self) -> None:
        options = self._options()
        report = app_server.run_client_smoke(options)
        self.assertEqual(report["status"], "passed")
        self.assertEqual(report["registry"]["tools"], 41)
        self.assertEqual(report["surfaces"], ["app", "cli"])
        self.assertTrue(report["assertions"]["model_turn_absent"])
        observed = json.loads(self.log.read_text(encoding="utf-8"))
        self.assertEqual(
            observed["methods"],
            [
                "initialize",
                "notifications/initialized",
                "thread/start",
                "mcpServerStatus/list",
                "mcpServer/tool/call",
                "mcpServer/tool/call",
            ],
        )
        self.assertNotIn("turn/start", observed["methods"])
        self.assertEqual(observed["cwd"], str(self.project))
        self.assertNotEqual(observed["codex_home"], str(Path.home() / ".codex"))
        encoded = json.dumps(report, sort_keys=True)
        self.assertNotIn(str(self.root), encoded)
        self.assertNotIn(PROJECT_ID, encoded)
        self.assertEqual(json.loads(options.output.read_text(encoding="utf-8")), report)

    def test_protocol_and_contract_failures_are_closed(self) -> None:
        for mode in (
            "malformed",
            "duplicate_response",
            "notification_flood",
            "oversized",
            "crash",
            "tool_error",
            "wrong_scope",
            "missing_evidence",
            "registry_mismatch",
        ):
            with self.subTest(mode=mode):
                self.log.unlink(missing_ok=True)
                options = self._options(mode)
                with self.assertRaises(app_server.AppServerProtocolError):
                    app_server.run_client_smoke(options)
                self.assertFalse(options.output.exists())

    def test_timeout_kills_only_the_exact_child(self) -> None:
        options = self._options("timeout", timeout=0.25)
        with self.assertRaisesRegex(app_server.AppServerProtocolError, "deadline"):
            app_server.run_client_smoke(options)
        observed = json.loads(self.log.read_text(encoding="utf-8"))
        self.assertEqual(observed["methods"], ["initialize"])

    def test_close_forces_an_uncooperative_child_to_exit(self) -> None:
        options = self._options("ignore_shutdown", timeout=0.5)
        started = time.monotonic()
        with self.assertRaisesRegex(app_server.AppServerProtocolError, "cleanly"):
            app_server.run_client_smoke(options)
        self.assertLess(time.monotonic() - started, 4.0)

    def test_wrong_expected_client_digest_is_rejected_before_start(self) -> None:
        options = self._options()
        options = app_server.dataclasses.replace(
            options,
            expected_client_sha256="sha256:" + "0" * 64,
        )
        with self.assertRaisesRegex(app_server.AppServerProtocolError, "client digest"):
            app_server.run_client_smoke(options)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
