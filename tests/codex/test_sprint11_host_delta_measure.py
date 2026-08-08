from __future__ import annotations

import copy
import hashlib
import json
import plistlib
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tests.codex import sprint11_host_delta_measure as measure
from tests.codex.test_sprint11_host_delta import matrix, profile


def digest(character: str) -> str:
    return "sha256:" + character * 64


def feature_rows() -> list[dict[str, object]]:
    return [
        {"name": "auth_elicitation", "maturity": "stable", "enabled": True},
        {
            "name": "exec_permission_approvals",
            "maturity": "under development",
            "enabled": False,
        },
        {"name": "guardian_approval", "maturity": "stable", "enabled": True},
        {
            "name": "request_permissions_tool",
            "maturity": "under development",
            "enabled": False,
        },
        {
            "name": "tool_call_mcp_elicitation",
            "maturity": "stable",
            "enabled": True,
        },
    ]


def schema_fixture(
    *,
    actions: list[str] | None = None,
    extra: object | None = None,
) -> dict[str, dict[str, object]]:
    actions = actions or ["accept", "decline", "cancel"]
    documents: dict[str, dict[str, object]] = {
        "v1/InitializeParams.json": {
            "title": "InitializeParams",
            "type": "object",
            "properties": {
                "capabilities": {
                    "$ref": "#/definitions/InitializeCapabilities"
                },
                "clientInfo": {"type": "object"},
            },
            "required": ["clientInfo"],
            "definitions": {
                "InitializeCapabilities": {
                    "type": "object",
                    "properties": {
                        "mcpServerOpenaiFormElicitation": {"type": "boolean"}
                    },
                }
            },
        },
        "v1/InitializeResponse.json": {
            "title": "InitializeResponse",
            "type": "object",
            "properties": {
                "codexHome": {"type": "string"},
                "platformFamily": {"type": "string"},
                "platformOs": {"type": "string"},
                "userAgent": {"type": "string"},
            },
            "required": ["codexHome", "platformFamily", "platformOs", "userAgent"],
        },
        "v2/ThreadStartParams.json": {
            "title": "ThreadStartParams",
            "type": "object",
            "properties": {
                "approvalPolicy": {"$ref": "#/definitions/AskForApproval"},
                "cwd": {"type": "string"},
            },
            "definitions": {
                "AskForApproval": {
                    "oneOf": [
                        {"enum": ["untrusted", "on-request", "never"]},
                        {
                            "type": "object",
                            "properties": {
                                "granular": {
                                    "type": "object",
                                    "properties": {
                                        "mcp_elicitations": {"type": "boolean"},
                                        "request_permissions": {"type": "boolean"},
                                    },
                                    "required": ["mcp_elicitations"],
                                }
                            },
                            "required": ["granular"],
                        },
                    ]
                }
            },
        },
        "v2/ListMcpServerStatusParams.json": {
            "title": "ListMcpServerStatusParams",
            "type": "object",
            "properties": {"threadId": {"type": ["string", "null"]}},
        },
        "v2/ListMcpServerStatusResponse.json": {
            "title": "ListMcpServerStatusResponse",
            "type": "object",
            "properties": {"data": {"type": "array"}},
            "required": ["data"],
        },
        "v2/McpServerToolCallParams.json": {
            "title": "McpServerToolCallParams",
            "type": "object",
            "properties": {
                "arguments": True,
                "server": {"type": "string"},
                "threadId": {"type": "string"},
                "tool": {"type": "string"},
            },
            "required": ["server", "threadId", "tool"],
        },
        "v2/McpServerToolCallResponse.json": {
            "title": "McpServerToolCallResponse",
            "type": "object",
            "properties": {
                "content": {"type": "array"},
                "isError": {"type": ["boolean", "null"]},
                "structuredContent": True,
            },
            "required": ["content"],
        },
        "McpServerElicitationRequestParams.json": {
            "title": "McpServerElicitationRequestParams",
            "type": "object",
            "properties": {
                "serverName": {"type": "string"},
                "threadId": {"type": "string"},
                "turnId": {"type": ["string", "null"]},
            },
            "required": ["serverName", "threadId"],
            "definitions": {
                "McpServerElicitationMode": {
                    "enum": ["openai/form"],
                    "type": "string",
                }
            },
        },
        "McpServerElicitationRequestResponse.json": {
            "title": "McpServerElicitationRequestResponse",
            "type": "object",
            "properties": {
                "action": {
                    "$ref": "#/definitions/McpServerElicitationAction"
                },
                "content": True,
            },
            "required": ["action"],
            "definitions": {
                "McpServerElicitationAction": {
                    "enum": actions,
                    "type": "string",
                }
            },
        },
    }
    if extra is not None:
        documents["Unrelated.json"] = {"unrelated": extra}
    return documents


def measured_surfaces(*, shared_app_cli: bool) -> list[dict[str, object]]:
    value = copy.deepcopy(profile()["surfaces"])
    app_digest = digest("a")
    value[0]["client_artifact_sha256"] = app_digest
    value[1]["client_artifact_sha256"] = app_digest if shared_app_cli else digest("c")
    value[2]["client_artifact_sha256"] = digest("b")
    return value


class HostDeltaMeasurementTests(unittest.TestCase):
    def test_contract_projection_ignores_unrelated_app_server_schema(self) -> None:
        first = measure.project_contract(schema_fixture(extra={"unrelated": 1}), feature_rows())
        second = measure.project_contract(schema_fixture(extra={"unrelated": 2}), feature_rows())
        self.assertEqual(
            first["contract_projection_sha256"],
            second["contract_projection_sha256"],
        )

    def test_contract_projection_binds_elicitation_actions(self) -> None:
        changed = schema_fixture(actions=["accept", "cancel"])
        with self.assertRaisesRegex(measure.HostMeasurementError, "elicitation action set"):
            measure.project_contract(changed, feature_rows())

    def test_client_groups_deduplicate_app_and_cli_binary(self) -> None:
        groups = measure.client_groups(measured_surfaces(shared_app_cli=True))
        self.assertEqual(
            groups,
            ((digest("a"), ("app", "cli")), (digest("b"), ("ide",))),
        )

    def test_client_groups_require_each_surface_exactly_once(self) -> None:
        for surfaces in (
            measured_surfaces(shared_app_cli=True)[:2],
            [*measured_surfaces(shared_app_cli=True), measured_surfaces(shared_app_cli=True)[0]],
        ):
            with self.subTest(count=len(surfaces)):
                with self.assertRaises(measure.HostMeasurementError):
                    measure.client_groups(surfaces)

    def test_version_and_feature_parsers_are_exact(self) -> None:
        self.assertEqual(
            measure.parse_codex_version(b"codex-cli 0.147.0-alpha.6.5\n"),
            "0.147.0-alpha.6.5",
        )
        rows = measure.parse_feature_rows(
            b"\n".join(
                f"{row['name']:<38}  {row['maturity']:<18}  {str(row['enabled']).lower()}".encode()
                for row in feature_rows()
            )
            + b"\nunrelated stable true\n"
        )
        self.assertEqual(rows, feature_rows())
        for output in (
            b"codex 0.1\n",
            b"codex-cli ../../bad\n",
            b"codex-cli 0.1\nextra\n",
            b"x" * (measure.MAX_VERSION_BYTES + 1),
        ):
            with self.subTest(output=output):
                with self.assertRaises(measure.HostMeasurementError):
                    measure.parse_codex_version(output)

    def test_signature_coordinate_rejects_the_wrong_team(self) -> None:
        observed = {
            "verified": True,
            "mode": "strict",
            "identifier": "codex",
            "team_id": "WRONGTEAM",
            "cdhash": "a" * 40,
        }
        with mock.patch.object(measure, "measure_code_signature", return_value=observed):
            with self.assertRaisesRegex(measure.HostMeasurementError, "code-signature"):
                measure._signature_coordinate(
                    Path("/fixture/client"),
                    deep=False,
                    identifier="codex",
                    team_id="2DC432GLL2",
                )

    def test_feature_projection_rejects_missing_or_duplicate_rows(self) -> None:
        missing = feature_rows()[:-1]
        duplicate = [*feature_rows(), feature_rows()[0]]
        for rows in (missing, duplicate):
            with self.subTest(rows=rows):
                with self.assertRaises(measure.HostMeasurementError):
                    measure.project_features(rows)

    def test_measurement_is_redacted_bound_and_published_atomically(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            app_bundle = root / "ChatGPT.app"
            app_executable = app_bundle / "Contents/MacOS/ChatGPT"
            app_client = app_bundle / "Contents/Resources/codex"
            vscode_bundle = root / "Visual Studio Code.app"
            vscode_executable = vscode_bundle / "Contents/MacOS/Electron"
            extension_root = root / "openai.chatgpt-1.2.3-darwin-arm64"
            extension_package = extension_root / "package.json"
            ide_client = extension_root / "bin/codex"
            for path in (app_executable, app_client, vscode_executable, ide_client):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(
                    b"ide fixture executable\n"
                    if path == ide_client
                    else b"fixture executable\n"
                )
                path.chmod(0o700)
            info_plist = app_bundle / "Contents/Info.plist"
            with info_plist.open("wb") as stream:
                plistlib.dump(
                    {
                        "CFBundleIdentifier": "com.openai.codex",
                        "CFBundleShortVersionString": "26.810.10000",
                        "CFBundleVersion": "7000",
                    },
                    stream,
                )
            extension_package.write_text(
                json.dumps({"name": "chatgpt", "publisher": "openai", "version": "1.2.3"}),
                encoding="utf-8",
            )
            matrix_path = root / "matrix.json"
            matrix_bytes = json.dumps(matrix(), separators=(",", ":")).encode()
            matrix_path.write_bytes(matrix_bytes)
            profile_document = profile()
            profile_document["compatibility_matrix"]["sha256"] = (
                "sha256:" + hashlib.sha256(matrix_bytes).hexdigest()
            )
            profile_path = root / "profile.json"
            profile_path.write_text(json.dumps(profile_document), encoding="utf-8")
            output = root / "measurement.json"

            options = measure.MeasureOptions(
                app_bundle=app_bundle,
                app_executable=app_executable,
                app_client=app_client,
                cli_client=app_client,
                vscode_bundle=vscode_bundle,
                vscode_executable=vscode_executable,
                extension_root=extension_root,
                extension_package_json=extension_package,
                ide_client=ide_client,
                embedded_profile=profile_path,
                embedded_matrix=matrix_path,
                output=output,
                timeout_seconds=180.0,
            )

            def signature(path: Path, *, deep: bool, runner: object = None) -> dict[str, object]:
                del runner
                if path == app_bundle:
                    identifier, team = "com.openai.codex", "2DC432GLL2"
                elif path == vscode_bundle:
                    identifier, team = "com.microsoft.VSCode", "UBF8T346G9"
                else:
                    identifier, team = "codex", "2DC432GLL2"
                return {
                    "verified": True,
                    "mode": "deep_strict" if deep else "strict",
                    "identifier": identifier,
                    "team_id": team,
                    "cdhash": "a" * 40,
                }

            def execute(argv: tuple[str, ...], timeout: float, operation: str) -> measure.CommandResult:
                del timeout
                if operation == "vscode_version":
                    stdout = b"1.131.0\nfixture-commit\narm64\n"
                elif operation.endswith("_version"):
                    stdout = b"codex-cli 0.147.0-alpha.6.5\n"
                elif operation.endswith("_features"):
                    stdout = b"\n".join(
                        f"{row['name']:<38}  {row['maturity']:<18}  {str(row['enabled']).lower()}".encode()
                        for row in feature_rows()
                    ) + b"\n"
                elif operation.endswith("_schema"):
                    destination = Path(argv[-1])
                    for name, document in schema_fixture().items():
                        path = destination / name
                        path.parent.mkdir(parents=True, exist_ok=True)
                        path.write_text(json.dumps(document), encoding="utf-8")
                    stdout = b""
                else:
                    raise AssertionError(operation)
                return measure.CommandResult(
                    operation=operation,
                    returncode=0,
                    duration_ms=1,
                    stdout=stdout,
                    stderr=b"",
                )

            with mock.patch.object(measure, "measure_code_signature", side_effect=signature), mock.patch.object(
                measure, "run_bounded_command", side_effect=execute
            ):
                result = measure.measure_host_delta(options)

            self.assertTrue(output.is_file())
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            self.assertEqual(json.loads(output.read_text(encoding="utf-8")), result)
            schema = json.loads(
                (
                    Path(__file__).resolve().parent
                    / "schemas/sprint11-host-delta-measurement.schema.json"
                ).read_text(encoding="utf-8")
            )
            self.assertEqual(set(schema["required"]), set(result))
            self.assertEqual(
                result["client_groups"],
                [
                    {
                        "client_artifact_sha256": result["surfaces"][0]["client_artifact_sha256"],
                        "surfaces": ["app", "cli"],
                    },
                    {
                        "client_artifact_sha256": result["surfaces"][2]["client_artifact_sha256"],
                        "surfaces": ["ide"],
                    },
                ],
            )
            encoded = json.dumps(result, sort_keys=True)
            self.assertNotIn(str(root), encoded)
            self.assertNotIn("fixture executable", encoded)
            self.assertTrue(result["assertions"]["manual_interaction_absent"])

    def test_output_preexistence_and_symlink_inputs_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            target = root / "target"
            target.write_bytes(b"x")
            link = root / "link"
            link.symlink_to(target)
            with self.assertRaises(measure.HostMeasurementError):
                measure.safe_file_digest(link, label="fixture")
            output = root / "measurement.json"
            output.write_text("occupied", encoding="utf-8")
            with self.assertRaisesRegex(measure.HostMeasurementError, "already exists"):
                measure.publish_measurement(output, {"status": "would-overwrite"})


if __name__ == "__main__":
    unittest.main()
