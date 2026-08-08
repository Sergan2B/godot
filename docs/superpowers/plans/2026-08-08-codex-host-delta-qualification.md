# Codex Host Delta Qualification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a one-command, fail-closed host-delta qualifier that requalifies rolling Codex App/CLI/IDE coordinates without rebuilding package `0.1.19` or repeating setup, Trust, fault, form, and full surface-capture workflows.

**Architecture:** Keep the frozen package and its package-bound evidence immutable. Add release-side Python acquisition tooling that measures signed host coordinates, projects the relevant Codex feature/app-server contract, probes each distinct official client through app-server JSON-RPC, classifies the delta, issues the already-supported detached compatibility bundle, and composes a separate technical/private-alpha receipt. Preserve the existing External Beta validator unchanged so deferred human usability remains fail-closed.

**Tech Stack:** Python 3.12 standard library (`argparse`, `dataclasses`, `enum`, `hashlib`, `json`, `plistlib`, `selectors`, `subprocess`, `tempfile`, `unittest`), JSON Schema draft 2020-12, existing Rust `godot-codex compatibility install` consumer, Codex app-server JSON-RPC v2, macOS `codesign`.

## Global Constraints

- Package `0.1.19`, its archive, detached manifest, launcher, sidecar, Godot prerequisite, Bridge protocol, product schemas, and registry remain byte-for-byte unchanged.
- Do not modify `godot-codex-mcp/crates/`, `godot-codex-mcp/product/`, or `godot-codex-mcp/schemas/` for this feature; new release-side schemas live under `tests/codex/schemas/`.
- The supported target is exactly macOS arm64.
- Host authorization remains exact-coordinate, signature-verified, digest-bound, sequence-monotonic, and fail-closed.
- A host-only run must not call package install, setup, setup repair, project Trust, fault injection, semantic approval forms, or manual surface capture.
- Reuse only an already configured, package-verified disposable qualification fixture; never mutate a developer project.
- Deduplicate live probes by exact client artifact SHA-256, but keep separate App/CLI/IDE provenance coordinates.
- `supported` in the generated surface bundle means technical MCP compatibility only; it does not satisfy S11-12 human usability or authorize External Codex Beta/commercial claims.
- Leave `tests/codex/sprint11_acceptance.py` and `sprint-11-external-codex-beta-macos.json` semantics unchanged.
- Every subprocess has a fixed timeout, bounded captured output, sanitized environment, exact executable path, and verified clean shutdown.
- No failed or interrupted run may modify active compatibility state.

## File Map

- Create `tests/codex/schemas/sprint11-host-delta-measurement.schema.json`: strict measured-host and contract-projection schema.
- Create `tests/codex/schemas/sprint11-host-smoke.schema.json`: strict per-client app-server smoke report schema.
- Create `tests/codex/schemas/sprint11-host-delta-receipt.schema.json`: strict classifier, probe binding, and bundle-issuance receipt schema.
- Create `tests/codex/schemas/sprint11-technical-private-alpha.schema.json`: package/host evidence composition schema that explicitly preserves usability deferral.
- Create `tests/codex/sprint11_host_delta.py`: pure strict-JSON loaders, delta model, validators, profile/bundle builders, and receipt builder.
- Create `tests/codex/test_sprint11_host_delta.py`: pure unit and schema-oracle tests.
- Create `tests/codex/sprint11_host_delta_measure.py`: signed artifact/version measurement plus relevant feature/app-server projection.
- Create `tests/codex/test_sprint11_host_delta_measure.py`: hermetic filesystem, command-output, projection, and redaction tests.
- Create `tests/codex/sprint11_host_app_server.py`: bounded line-delimited JSON-RPC client and one-client MCP smoke implementation.
- Create `tests/codex/test_sprint11_host_app_server.py`: fake app-server process tests and JSON-RPC fixture tests.
- Create `tests/codex/sprint11_host_delta_qualify.py`: one-command orchestration, detached bundle issuance, optional package-owned preview/apply, and atomic private acquisition output.
- Create `tests/codex/test_sprint11_host_delta_qualify.py`: orchestration, interruption, no-op, apply, and fail-closed tests.
- Create `tests/codex/sprint11_technical_acceptance.py`: technical/private-alpha compositor over immutable package receipts and the active host-delta receipt.
- Create `tests/codex/test_sprint11_technical_acceptance.py`: package-evidence reuse and commercial-claim rejection tests.
- Modify `docs/codex-integration/SPRINT-11-HOST-CONTRACT.md`: add the host-delta authority and decision matrix.
- Modify `docs/codex-integration/EXTERNAL-CODEX-BETA-GUIDE.md`: replace the routine post-update manual loop with the one-command workflow.
- Modify `docs/codex-integration/SPRINT-11-TECHNICAL-QUALIFICATION.md`: bind the new technical/private-alpha evidence path and preserve S11-12 deferral.
- Modify `tests/codex/README.md`: document hermetic and real host-delta commands.

---

### Task 1: Strict Host-Delta Contracts and Pure Classifier

**Files:**
- Create: `tests/codex/schemas/sprint11-host-delta-measurement.schema.json`
- Create: `tests/codex/schemas/sprint11-host-smoke.schema.json`
- Create: `tests/codex/schemas/sprint11-host-delta-receipt.schema.json`
- Create: `tests/codex/sprint11_host_delta.py`
- Create: `tests/codex/test_sprint11_host_delta.py`

**Interfaces:**
- Consumes: strict dictionaries loaded from the frozen package manifest, embedded compatibility matrix, previous accepted host profile/receipt, current measurement, and per-client smoke reports.
- Produces: `DeltaClass`, `DeltaAssessment`, `classify_delta(...)`, `build_host_profile(...)`, `build_surface_bundle(...)`, `validate_smoke_report(...)`, and `build_host_delta_receipt(...)` for Tasks 2–5.

- [ ] **Step 1: Write failing classifier and schema-oracle tests**

Add tests that define the public Python interface and exact classification precedence:

```python
class HostDeltaModelTests(unittest.TestCase):
    def test_coordinate_only_reuses_unchanged_product_and_interaction_contract(self) -> None:
        assessment = host_delta.classify_delta(
            package_manifest=package_manifest("0.1.19"),
            embedded_matrix=matrix("0.1.19"),
            previous_profile=profile(app_version="26.803.41515"),
            current_profile=profile(app_version="26.810.10000"),
            previous_contract=contract("sha256:" + "1" * 64),
            current_contract=contract("sha256:" + "1" * 64),
        )
        self.assertEqual(assessment.delta_class, host_delta.DeltaClass.COORDINATE_ONLY)
        self.assertEqual(assessment.required_probes, ("host_smoke",))

    def test_interaction_digest_change_is_targeted(self) -> None:
        assessment = host_delta.classify_delta(
            package_manifest=package_manifest("0.1.19"),
            embedded_matrix=matrix("0.1.19"),
            previous_profile=profile(),
            current_profile=profile(),
            previous_contract=contract("sha256:" + "1" * 64),
            current_contract=contract("sha256:" + "2" * 64),
        )
        self.assertEqual(assessment.delta_class, host_delta.DeltaClass.INTERACTION_SENSITIVE)
        self.assertEqual(assessment.required_probes, ("host_smoke", "interaction_contract"))

    def test_product_change_has_priority_and_cannot_issue_bundle(self) -> None:
        assessment = host_delta.classify_delta(
            package_manifest=package_manifest("0.1.20"),
            embedded_matrix=matrix("0.1.19"),
            previous_profile=profile(),
            current_profile=profile(),
            previous_contract=contract("sha256:" + "1" * 64),
            current_contract=contract("sha256:" + "1" * 64),
        )
        self.assertEqual(assessment.delta_class, host_delta.DeltaClass.PRODUCT_CONTRACT)
        with self.assertRaisesRegex(host_delta.HostDeltaError, "full package qualification"):
            host_delta.build_surface_bundle(
                assessment=assessment,
                matrix=matrix("0.1.19"),
                profile=profile(),
                profile_sha256=digest("a"),
                sequence=1,
            )
```

Add schema-oracle tests asserting `additionalProperties: false`, exact required fields, surface enum `app|cli|ide`, delta enum `coordinate_only|interaction_sensitive|surface_structural|product_contract|unclassifiable`, and outcome enum `compatible_observed|compatible_bundle_issued|targeted_surface_check_required|full_package_qualification_required`.

- [ ] **Step 2: Run the focused tests and verify the missing module/schema failure**

Run:

```bash
python3 -m unittest tests.codex.test_sprint11_host_delta -v
```

Expected: FAIL because `tests.codex.sprint11_host_delta` and the three schema files do not exist.

- [ ] **Step 3: Create strict schemas with closed bindings**

Define the receipt root with these required members and no unknown fields:

```json
{
  "schema_version": "s11-host-delta-receipt/1.0",
  "status": "passed",
  "outcome": "compatible_bundle_issued",
  "delta_class": "coordinate_only",
  "affected_surfaces": ["app"],
  "required_probes": ["host_smoke"],
  "bindings": {
    "package_manifest_sha256": "sha256:<64 lowercase hex>",
    "baseline_matrix_sha256": "sha256:<64 lowercase hex>",
    "previous_profile_sha256": "sha256:<64 lowercase hex>",
    "measurement_sha256": "sha256:<64 lowercase hex>",
    "host_profile_sha256": "sha256:<64 lowercase hex>",
    "bundle_sha256": "sha256:<64 lowercase hex>",
    "contract_projection_sha256": "sha256:<64 lowercase hex>",
    "smoke_report_sha256": ["sha256:<64 lowercase hex>"]
  },
  "bundle": {
    "bundle_id": "host-delta-1-0123456789ab",
    "sequence": 1,
    "qualification": "supported"
  },
  "assertions": {
    "package_unchanged": true,
    "all_required_surfaces_measured": true,
    "all_distinct_clients_probed": true,
    "project_integrity_preserved": true,
    "manual_interaction_absent": true,
    "external_beta_claimed": false
  },
  "redaction": {
    "absolute_paths_absent": true,
    "account_identity_absent": true,
    "artifact_content_absent": true,
    "environment_secrets_absent": true
  }
}
```

Use `$defs` for prefixed SHA-256, safe token, target coordinate, signature coordinate, command binding, surface measurement, and probe binding so the measurement and smoke schemas reuse the same closed shapes.

- [ ] **Step 4: Implement the pure model and classification precedence**

Implement these exact types:

```python
class DeltaClass(str, enum.Enum):
    COORDINATE_ONLY = "coordinate_only"
    INTERACTION_SENSITIVE = "interaction_sensitive"
    SURFACE_STRUCTURAL = "surface_structural"
    PRODUCT_CONTRACT = "product_contract"
    UNCLASSIFIABLE = "unclassifiable"

@dataclasses.dataclass(frozen=True)
class DeltaAssessment:
    delta_class: DeltaClass
    affected_surfaces: tuple[str, ...]
    required_probes: tuple[str, ...]
    reasons: tuple[str, ...]
```

Implement these exact call signatures:

- `classify_delta(*, package_manifest: Mapping[str, Any], embedded_matrix: Mapping[str, Any], previous_profile: Mapping[str, Any] | None, current_profile: Mapping[str, Any], previous_contract: Mapping[str, Any] | None, current_contract: Mapping[str, Any]) -> DeltaAssessment`
- `build_host_profile(*, embedded_profile: Mapping[str, Any], measured_surfaces: Sequence[Mapping[str, Any]], qualification: str, profile_id: str) -> dict[str, Any]`
- `build_surface_bundle(*, assessment: DeltaAssessment, matrix: Mapping[str, Any], profile: Mapping[str, Any], profile_sha256: str, sequence: int) -> dict[str, Any]`

Classification precedence is `product_contract` → `unclassifiable` → `surface_structural` → `interaction_sensitive` → `coordinate_only`. Treat missing previous contract as `interaction_sensitive` bootstrap only when all three current surfaces are fully measured and all contract projections are present; otherwise return `unclassifiable`.

Build a complete App/CLI/IDE bundle with qualification `supported`, target copied from `matrix["package"]["target"]`, `baseline_matrix_sha256` computed from order-preserving compact JSON of the embedded matrix, and ID `host-delta-{sequence}-{profile_sha256[7:19]}`. Reject zero/non-JavaScript-safe sequences and every delta class except `coordinate_only` or a fully satisfied `interaction_sensitive` assessment.

- [ ] **Step 5: Run focused tests**

Run:

```bash
python3 -m unittest tests.codex.test_sprint11_host_delta -v
```

Expected: PASS for classification, bundle shape, sequence, digest, strict JSON, unknown-field, missing-surface, and redaction tests.

- [ ] **Step 6: Commit the contract/model slice**

```bash
git add tests/codex/schemas/sprint11-host-delta-measurement.schema.json tests/codex/schemas/sprint11-host-smoke.schema.json tests/codex/schemas/sprint11-host-delta-receipt.schema.json tests/codex/sprint11_host_delta.py tests/codex/test_sprint11_host_delta.py
git commit -m "test: define host delta qualification contract"
```

---

### Task 2: Signed Host Measurement and Interaction-Contract Projection

**Files:**
- Create: `tests/codex/sprint11_host_delta_measure.py`
- Create: `tests/codex/test_sprint11_host_delta_measure.py`
- Reuse without modifying: `tests/codex/sprint11_host_provenance.py`

**Interfaces:**
- Consumes: canonical paths for ChatGPT.app executable/bundled client, standalone CLI, VS Code executable, official extension tree/package.json/embedded client, embedded host profile, and embedded matrix.
- Produces: `measure_host_delta(options: MeasureOptions) -> dict[str, Any]`, including `surfaces`, `client_groups`, `feature_projection`, `app_server_projection`, and `contract_projection_sha256`.

- [ ] **Step 1: Write failing measurement/projection tests**

Use temporary regular files, fake executable scripts, synthetic `Info.plist`, extension `package.json`, and generated app-server schema fixtures. Cover version extraction, client deduplication, relevant feature projection, elicitation action projection, and structural delta detection:

```python
def test_contract_projection_ignores_unrelated_app_server_schema(self) -> None:
    first = measure.project_contract(schema_fixture(extra={"unrelated": 1}), feature_rows())
    second = measure.project_contract(schema_fixture(extra={"unrelated": 2}), feature_rows())
    self.assertEqual(first["contract_projection_sha256"], second["contract_projection_sha256"])

def test_contract_projection_binds_elicitation_actions(self) -> None:
    changed = schema_fixture(actions=["accept", "cancel"])
    with self.assertRaisesRegex(measure.HostMeasurementError, "elicitation action set"):
        measure.project_contract(changed, feature_rows())

def test_client_groups_deduplicate_app_and_cli_binary(self) -> None:
    groups = measure.client_groups(measured_surfaces(shared_app_cli=True))
    self.assertEqual(groups, ((digest("a"), ("app", "cli")), (digest("b"), ("ide",))))
```

- [ ] **Step 2: Run tests and verify the missing module failure**

Run:

```bash
python3 -m unittest tests.codex.test_sprint11_host_delta_measure -v
```

Expected: FAIL because `sprint11_host_delta_measure.py` does not exist.

- [ ] **Step 3: Implement exact host coordinate discovery**

Add this immutable options type and the exact function signature
`measure_host_delta(options: MeasureOptions) -> dict[str, Any]`:

```python
@dataclasses.dataclass(frozen=True)
class MeasureOptions:
    app_bundle: Path
    app_executable: Path
    app_client: Path
    cli_client: Path
    vscode_bundle: Path
    vscode_executable: Path
    extension_root: Path
    extension_package_json: Path
    ide_client: Path
    embedded_profile: Path
    embedded_matrix: Path
    output: Path
    timeout_seconds: float = 30.0
```

Reuse `_canonical_nonsymlink_path`, `_safe_file_digest`, `stable_tree_digest`, and code-signature parsing from `sprint11_host_provenance.py`. Read App version/build from `CFBundleShortVersionString` and `CFBundleVersion`; parse exact `codex-cli <version>` from each `--version`; read extension version from `package.json`; parse the first VS Code `--version` line. Require the existing identifiers/team IDs (`com.openai.codex`/`2DC432GLL2`, `codex`/`2DC432GLL2`, `com.microsoft.VSCode`/`UBF8T346G9`) and reject symlinks, unstable files/trees, oversized output, non-arm64 hosts, or unexpected version formats.

- [ ] **Step 4: Implement relevant feature and app-server projections**

Run each distinct client with bounded output:

```bash
<client> features list
<client> app-server generate-json-schema --out <new-private-temp-directory>
```

Project only these feature rows, including maturity and enabled state:

```python
RELEVANT_FEATURES = (
    "auth_elicitation",
    "exec_permission_approvals",
    "guardian_approval",
    "request_permissions_tool",
    "tool_call_mcp_elicitation",
)
```

Project only these generated schemas and semantics:

```python
APP_SERVER_FILES = (
    "v1/InitializeParams.json",
    "v1/InitializeResponse.json",
    "v2/ThreadStartParams.json",
    "v2/ListMcpServerStatusParams.json",
    "v2/ListMcpServerStatusResponse.json",
    "v2/McpServerToolCallParams.json",
    "v2/McpServerToolCallResponse.json",
    "McpServerElicitationRequestParams.json",
    "McpServerElicitationRequestResponse.json",
)
REQUIRED_ELICITATION_ACTIONS = ("accept", "decline", "cancel")
```

Canonicalize only the required properties/enums/required-lists for `initialize`, `thread/start`, `mcpServerStatus/list`, `mcpServer/tool/call`, `openai/form`, and the three elicitation actions. Hash the projection, not the entire generated schema bundle, so unrelated app-server additions remain `coordinate_only`.

- [ ] **Step 5: Emit only redacted measurement data**

Write the new output atomically with mode `0600`. Include digests, byte counts, versions, signature coordinates, surface-to-client-group membership, relevant feature projection, relevant app-server projection, and command exit/duration/digest bindings. Exclude paths, environment, generated schema bodies, app contents, usernames, tokens, and raw command output.

- [ ] **Step 6: Run focused measurement tests**

Run:

```bash
python3 -m unittest tests.codex.test_sprint11_host_delta_measure -v
```

Expected: PASS, including unstable-file, symlink, bad signature, duplicate surface, unsafe version, output bound, unrelated schema, and missing elicitation action failures.

- [ ] **Step 7: Commit the measurement slice**

```bash
git add tests/codex/sprint11_host_delta_measure.py tests/codex/test_sprint11_host_delta_measure.py
git commit -m "test: measure rolling Codex host deltas"
```

---

### Task 3: Deterministic App-Server MCP Smoke Without a Model or GUI

**Files:**
- Create: `tests/codex/sprint11_host_app_server.py`
- Create: `tests/codex/test_sprint11_host_app_server.py`

**Interfaces:**
- Consumes: one exact client executable, its expected SHA-256, a package-owned launcher, an already configured disposable project, expected project ID, expected registry profile, and fixed timeout.
- Produces: `run_client_smoke(options: SmokeOptions) -> dict[str, Any]` conforming to `sprint11-host-smoke/1.0`.

- [ ] **Step 1: Write failing JSON-RPC lifecycle tests**

Create a fake line-delimited app-server process that validates request order and returns deterministic responses. Assert the client sends:

1. `initialize` with `mcpServerOpenaiFormElicitation: true`.
2. `notifications/initialized`.
3. `thread/start` with the exact fixture cwd, `ephemeral: true`, `sandbox: read-only`, and `approvalPolicy: never`.
4. `mcpServerStatus/list` with `detail: full` and the returned thread ID.
5. `mcpServer/tool/call` for `godot_get_connection_status`.
6. `mcpServer/tool/call` for `godot_get_current_scene`.

```python
def test_smoke_uses_direct_app_server_mcp_calls_without_turn_start(self) -> None:
    report = app_server.run_client_smoke(fixture_options(self.fake_server))
    self.assertEqual(report["status"], "passed")
    self.assertEqual(report["registry"]["tools"], 41)
    self.assertTrue(report["assertions"]["model_turn_absent"])
    self.assertNotIn("turn/start", self.fake_server.methods)
```

Also test timeout, malformed JSON, duplicate response ID, notification flood, oversized line, server crash, tool error, wrong project scope, missing evidence, registry mismatch, and forced-kill cleanup.

- [ ] **Step 2: Run tests and verify the missing module failure**

Run:

```bash
python3 -m unittest tests.codex.test_sprint11_host_app_server -v
```

Expected: FAIL because `sprint11_host_app_server.py` does not exist.

- [ ] **Step 3: Implement the bounded line JSON-RPC client**

Implement `AppServerProtocolError(RuntimeError)` and `JsonRpcLineClient` with
the exact methods `request(method: str, params: Mapping[str, Any]) -> dict[str,
Any]`, `notify(method: str, params: Mapping[str, Any]) -> None`, and `close() ->
None`. `request` returns one strict JSON-RPC result object; `notify` writes one
notification; `close` performs the bounded exact-child shutdown described
below. The module-level entry point is `run_client_smoke(options: SmokeOptions)
-> dict[str, Any]`.

```python
@dataclasses.dataclass(frozen=True)
class SmokeOptions:
    client: Path
    expected_client_sha256: str
    launcher: Path
    project_root: Path
    expected_project_id: str
    expected_registry: Path
    output: Path
    timeout_seconds: float = 60.0
```

Use `selectors.DefaultSelector` and monotonic deadlines. Bound each JSON line to 2 MiB, total output to 16 MiB, outstanding requests to one, notifications to 4096, and process lifetime to `timeout_seconds`. Send SIGINT, then terminate, then kill only the exact child process if bounded graceful shutdown fails.

- [ ] **Step 4: Start the official client app-server with isolated configuration**

Execute the exact client with:

```text
<client> app-server --stdio --strict-config
  -c mcp_servers.godot_editor.command="<package-owned-launcher>"
  -c mcp_servers.godot_editor.args=["mcp"]
  -c mcp_servers.godot_editor.cwd="<exact-project-root>"
  -c mcp_servers.godot_editor.required=true
```

Use a new mode-0700 temporary `CODEX_HOME` containing no config, copy no authentication material, preserve only locale plus validated `GODOT_CODEX_PROCESS_SCOPE_<48 hex>` markers, and set process cwd to the exact fixture root. The smoke performs no model turn and therefore needs no account/network authorization.

- [ ] **Step 5: Validate inventory, status, and semantic evidence**

From `mcpServerStatus/list`, require one `godot_editor` server, 41 tools, four fixed resources, one resource template, and exact canonical names/digest from `registry-profile.v1.json`. From direct tool calls require:

```python
status["schema_version"] == "godot-connection-status/1.1"
status["status"] == "ready"
status["project_scope"] == expected_project_id
status["bridge"]["negotiated_protocol"] == "1.8"
status["static_cache"]["condition"] == "online_current"
scene["project_id"] == expected_project_id
scene["freshness"] == "current"
scene["evidence"][0]["evidence_id"].startswith("evidence:")
```

Record only canonical counts, protocol/status values, project/evidence SHA-256 projections, client digest, command digest, duration, and cleanup assertions. Never retain raw project IDs, node data, paths, app-server events, or semantic content.

- [ ] **Step 6: Run focused app-server tests**

Run:

```bash
python3 -m unittest tests.codex.test_sprint11_host_app_server -v
```

Expected: PASS for lifecycle, inventory, tool calls, redaction, deadlines, and exact-child cleanup.

- [ ] **Step 7: Commit the app-server smoke slice**

```bash
git add tests/codex/sprint11_host_app_server.py tests/codex/test_sprint11_host_app_server.py
git commit -m "test: add model-free Codex host smoke"
```

---

### Task 4: One-Command Qualification, Bundle Issuance, and Atomic Apply

**Files:**
- Create: `tests/codex/sprint11_host_delta_qualify.py`
- Create: `tests/codex/test_sprint11_host_delta_qualify.py`
- Reuse without modifying: `godot-codex-mcp/crates/operations/src/compatibility_bundle.rs`

**Interfaces:**
- Consumes: Task 1 model, Task 2 measurement, Task 3 smoke, frozen package manifest/artifact root, installed launcher, embedded and previous profiles, existing configured fixture roots, and official host paths.
- Produces: private acquisition directory, detached host profile, compatibility bundle, host-delta receipt, SHA-256 files, and optional package-owned install report.

- [ ] **Step 1: Write failing end-to-end orchestration tests with fake commands**

Cover coordinate-only success, bootstrap success, interaction-sensitive stop, product-contract stop, duplicate-client deduplication, unchanged-coordinate no-op, preview failure, apply failure, interrupted run, stale output directory, and active-sequence advance:

```python
def test_coordinate_only_runs_one_smoke_per_distinct_client_and_applies(self) -> None:
    result = qualify.qualify(options(shared_app_cli=True, apply=True))
    self.assertEqual(result["outcome"], "compatible_bundle_applied")
    self.assertEqual(self.fake_smoke.calls, 2)
    self.assertEqual(self.fake_launcher.commands, ["compatibility status", "compatibility install --dry-run", "compatibility install --apply-plan"])

def test_product_contract_change_never_invokes_launcher_apply(self) -> None:
    with self.assertRaisesRegex(qualify.QualificationError, "full_package_qualification_required"):
        qualify.qualify(options(package_version="0.1.20", apply=True))
    self.assertNotIn("compatibility install", " ".join(self.fake_launcher.commands))
```

- [ ] **Step 2: Run tests and verify the missing orchestrator failure**

Run:

```bash
python3 -m unittest tests.codex.test_sprint11_host_delta_qualify -v
```

Expected: FAIL because `sprint11_host_delta_qualify.py` does not exist.

- [ ] **Step 3: Implement one closed CLI**

Expose:

```text
python3 -E -s -S tests/codex/sprint11_host_delta_qualify.py
  --package-manifest <absolute-file>
  --artifact-root <absolute-directory>
  --installed-launcher <absolute-file>
  --previous-profile <absolute-file>
  --previous-receipt <absolute-file-or-none>
  --app-bundle <absolute-directory>
  --app-executable <absolute-file>
  --app-client <absolute-file>
  --cli-client <absolute-file>
  --vscode-bundle <absolute-directory>
  --vscode-executable <absolute-file>
  --extension-root <absolute-directory>
  --extension-package-json <absolute-file>
  --ide-client <absolute-file>
  --project app=<absolute-configured-fixture>
  --project cli=<absolute-configured-fixture>
  --project ide=<absolute-configured-fixture>
  --output <new-absolute-directory>
  --timeout 90
  --apply
  --json
```

Reject relative paths, symlinks, existing output, duplicate/missing surfaces, unsupported target, extra environment authority, and timeout outside 5–180 seconds. Verify `install.sh verify`-equivalent manifest content bindings without invoking install or setup.

- [ ] **Step 4: Orchestrate measure, classify, and deduplicated smoke**

Use Task 2 to emit measurement, Task 1 to classify, and Task 3 once per distinct client digest. For `coordinate_only`, run only host smoke. For bootstrap (`previous_receipt` absent), require full current feature/app-server projection plus all client smokes. For `interaction_sensitive`, stop with `targeted_surface_check_required` unless an exact targeted interaction report was supplied and validated. For `surface_structural`, stop with the affected surface list. For `product_contract`, stop before any host probe.

- [ ] **Step 5: Issue detached profile and bundle into a staging directory**

Build a supported App/CLI/IDE profile by preserving the embedded profile's matrix and server-instruction bindings and replacing only its surfaces. Read current sequence using:

```bash
<installed-launcher> compatibility status --json
```

Use sequence `1` when source is embedded, otherwise `previous_sequence + 1`. Write canonical JSON plus raw `.sha256` files with mode `0600` under a new mode-0700 staging directory. Validate every generated document again from disk before promotion.

- [ ] **Step 6: Preview and apply only after all evidence is durable**

When `--apply` is present, call:

```bash
<installed-launcher> compatibility install \
  --bundle <staged-bundle> \
  --host-profile <staged-profile> \
  --expected-sha256 <raw-bundle-sha256> \
  --dry-run --json

<installed-launcher> compatibility install \
  --apply-plan <exact-plan-digest> --json

<installed-launcher> compatibility status --json
```

Require the apply report and final status to match bundle ID, sequence, bundle digest, profile digest, package `0.1.19`, and `installed_bundle` source. If preview/apply/status fails or differs, keep the acquisition as failed private evidence, leave the prior active state intact where the package guarantees atomicity, and never synthesize success.

- [ ] **Step 7: Atomically finalize the acquisition directory**

Write `host-delta-receipt.json` last. Sync files and staging directory, rename the exact staging directory to the requested new output, sync its parent, and emit only:

```json
{
  "schema_version": "s11-host-delta-command/1.0",
  "status": "passed",
  "outcome": "compatible_bundle_applied",
  "delta_class": "coordinate_only",
  "bundle_id": "host-delta-1-0123456789ab",
  "sequence": 1
}
```

- [ ] **Step 8: Run focused orchestration tests**

Run:

```bash
python3 -m unittest tests.codex.test_sprint11_host_delta_qualify -v
```

Expected: PASS, including no install/setup strings in command history, no duplicate client smoke, no partial active-state claim, and exact sequence advancement.

- [ ] **Step 9: Commit the qualifier slice**

```bash
git add tests/codex/sprint11_host_delta_qualify.py tests/codex/test_sprint11_host_delta_qualify.py
git commit -m "test: automate Codex host delta qualification"
```

---

### Task 5: Separate Technical/Private-Alpha Evidence Composition

**Files:**
- Create: `tests/codex/schemas/sprint11-technical-private-alpha.schema.json`
- Create: `tests/codex/sprint11_technical_acceptance.py`
- Create: `tests/codex/test_sprint11_technical_acceptance.py`
- Do not modify: `tests/codex/sprint11_acceptance.py`

**Interfaces:**
- Consumes: frozen `0.1.19` detached manifest, package-live receipt with its bound same-project report, multi-project receipt, reproducibility receipt, new host-delta measurement/receipt/bundle/profile/smoke reports, and active compatibility status. The new measurement replaces superseded exact-host provenance; an old host-provenance receipt is not required.
- Produces: `s11-technical-private-alpha/1.0` evidence and validation summary; never External Beta evidence.

- [ ] **Step 1: Write failing composition tests**

Define tests proving evidence reuse and claim separation:

```python
def test_host_delta_reuses_exact_package_bound_receipts(self) -> None:
    report = technical.compose(valid_inputs())
    self.assertEqual(report["status"], "passed")
    self.assertEqual(report["track"], "technical_private_alpha")
    self.assertEqual(report["usability"], "deferred_unacquired")
    self.assertTrue(report["assertions"]["package_evidence_reused"])
    self.assertFalse(report["claims"]["external_codex_beta"])
    self.assertFalse(report["claims"]["commercial_ready"])

def test_changed_package_digest_rejects_reused_receipts(self) -> None:
    inputs = valid_inputs()
    inputs.host_delta["bindings"]["package_manifest_sha256"] = digest("f")
    with self.assertRaisesRegex(technical.TechnicalAcceptanceError, "package binding"):
        technical.compose(inputs)
```

Also reject candidate surface qualification, absent App/CLI/IDE, mismatched active sequence, manual-interaction assertion false, stale/failed smoke, missing same-project takeover, and any usability/commercial claim set true.

- [ ] **Step 2: Run tests and verify missing module/schema failure**

Run:

```bash
python3 -m unittest tests.codex.test_sprint11_technical_acceptance -v
```

Expected: FAIL because the compositor and schema do not exist.

- [ ] **Step 3: Implement the closed technical evidence schema**

Require these top-level fields:

```json
{
  "schema_version": "s11-technical-private-alpha/1.0",
  "status": "passed",
  "track": "technical_private_alpha",
  "package": {},
  "package_evidence": {},
  "host_compatibility": {},
  "usability": "deferred_unacquired",
  "claims": {
    "technical_private_alpha": true,
    "external_codex_beta": false,
    "commercial_ready": false
  },
  "assertions": {
    "package_evidence_reused": true,
    "host_delta_applied": true,
    "same_project_takeover_passed": true,
    "multi_project_isolation_passed": true,
    "all_host_clients_smoked": true,
    "manual_surface_loop_not_required": true
  },
  "redaction": {}
}
```

Every nested evidence input is represented by repository-relative path, raw SHA-256, schema version, status, source commit, and package-manifest binding; no evidence body is copied into the summary.

- [ ] **Step 4: Implement source-bound composition and validation**

Reuse strict JSON, Git blob-at-commit, digest, and package receipt validators from `sprint11_external_acquisitions.py` where their semantics are package-bound. Add host-delta-specific validation rather than importing the External Beta surface/human validators. Require:

- package version `0.1.19` and exact detached manifest digest;
- package-live receipt contains passing Sprint 6–10 and same-project gates;
- multi-project and reproducibility receipts pass and bind the same package source ancestry;
- the new host-delta measurement independently proves current signed-host provenance and replaces any superseded exact-host provenance receipt;
- host-delta profile/bundle/receipt/smokes bind the same package manifest and baseline matrix;
- active compatibility status matches the exact bundle/profile/sequence;
- all three surfaces are supported and all distinct clients have passing smoke;
- usability is exactly `deferred_unacquired` and commercial claims are false.

- [ ] **Step 5: Add generation and validation CLI modes**

Expose:

```text
python3 -E -s -S tests/codex/sprint11_technical_acceptance.py compose
  --package-manifest <file>
  --package-live-receipt <file>
  --multi-project-receipt <file>
  --reproducibility-receipt <file>
  --host-delta-directory <directory>
  --active-compatibility-status <file>
  --output <new-file>

python3 -E -s -S tests/codex/sprint11_technical_acceptance.py validate
  --evidence <file>
  --artifact-root <directory>
```

Both modes emit canonical redacted JSON. `compose` writes mode `0600` and refuses an existing output. `validate` reruns closed schema/binding checks but does not rerun package or host acquisition.

- [ ] **Step 6: Run focused technical acceptance tests**

Run:

```bash
python3 -m unittest tests.codex.test_sprint11_technical_acceptance -v
```

Expected: PASS for evidence reuse, source/digest binding, host application, claim separation, and all negative cases.

- [ ] **Step 7: Commit the technical evidence slice**

```bash
git add tests/codex/schemas/sprint11-technical-private-alpha.schema.json tests/codex/sprint11_technical_acceptance.py tests/codex/test_sprint11_technical_acceptance.py
git commit -m "test: compose Sprint 11 technical host evidence"
```

---

### Task 6: Documentation, Full Regression, and Current-Host Acquisition

**Files:**
- Modify: `docs/codex-integration/SPRINT-11-HOST-CONTRACT.md`
- Modify: `docs/codex-integration/EXTERNAL-CODEX-BETA-GUIDE.md`
- Modify: `docs/codex-integration/SPRINT-11-TECHNICAL-QUALIFICATION.md`
- Modify: `tests/codex/README.md`
- Create after real acquisition: `tests/codex/acquisition/sprint11/host-delta-v0119-current/measurement.json`
- Create after real acquisition: `tests/codex/acquisition/sprint11/host-delta-v0119-current/host-delta-receipt.json`
- Create after real acquisition: `tests/codex/acquisition/sprint11/host-delta-v0119-current/host-coordinate-profile.json`
- Create after real acquisition: `tests/codex/acquisition/sprint11/host-delta-v0119-current/surface-compatibility-bundle.json`
- Create after real acquisition: `tests/codex/acquisition/sprint11/technical-private-alpha-v0119/evidence.json`

**Interfaces:**
- Consumes: all implementation tasks plus current signed official hosts, verified `0.1.19` installation, existing package-bound receipts, and configured disposable qualification fixtures.
- Produces: documented one-command workflow, green full regressions, active exact-host bundle, and validated technical/private-alpha evidence.

- [ ] **Step 1: Update the host contract and beta guide**

Document the exact delta matrix and normal command. State explicitly:

```text
Ordinary Codex version/hash update:
  measure -> project contract -> app-server smoke -> issue/apply bundle
  no install, setup, Trust, faults, forms, or surface capture

MCP/elicitation/app-server projection change:
  targeted_surface_check_required

Package/Godot/Bridge/schema/registry change:
  full_package_qualification_required
```

Keep the manual App/CLI/IDE capture section labelled only for External Beta/human usability or a targeted UI-contract investigation, not routine host updates.

- [ ] **Step 2: Update technical qualification and test README**

Record that package-bound receipts remain valid by manifest digest, host-bound evidence is replaceable by bundle/profile digest, S11-12 remains deferred, and the output supports only technical/private-alpha claims. Include the exact hermetic unit command and real one-command acquisition CLI from Task 4.

- [ ] **Step 3: Run all new tests together**

Run:

```bash
python3 -m unittest \
  tests.codex.test_sprint11_host_delta \
  tests.codex.test_sprint11_host_delta_measure \
  tests.codex.test_sprint11_host_app_server \
  tests.codex.test_sprint11_host_delta_qualify \
  tests.codex.test_sprint11_technical_acceptance -v
```

Expected: PASS.

- [ ] **Step 4: Run the complete Python acceptance/regression suite**

Run:

```bash
python3 -m unittest discover -s tests/codex -p 'test_*.py' -v
```

Expected: PASS with no existing External Beta gate weakened.

- [ ] **Step 5: Run source-bound Rust regressions and lint**

Run:

```bash
cargo test --manifest-path godot-codex-mcp/Cargo.toml --workspace
cargo clippy --manifest-path godot-codex-mcp/Cargo.toml --workspace --all-targets -- -D warnings
```

Expected: PASS. Although no Rust/package source changes are planned, this proves the existing bundle consumer still accepts the generated contract and no source-bound gate regressed.

- [ ] **Step 6: Verify package `0.1.19` before real host acquisition**

Run the package-owned verifier from the frozen artifact and save only the redacted result:

```bash
<0.1.19-package>/install.sh verify
<installed-launcher> version
<installed-launcher> compatibility status --json
```

Expected: version `0.1.19`, package verification passed, and either embedded or an already valid installed bundle status. Do not run install or setup.

- [ ] **Step 7: Run the current-host one-command qualification**

Invoke Task 4 with the measured current App/CLI/IDE paths, three already configured disposable `0.1.19` fixture roots, a new private acquisition output, `--apply`, and `--json`. Expected terminal outcome:

```json
{
  "schema_version": "s11-host-delta-command/1.0",
  "status": "passed",
  "outcome": "compatible_bundle_applied",
  "delta_class": "coordinate_only"
}
```

If the contract projection differs, stop honestly at `targeted_surface_check_required`; do not fall back to the old full manual loop.

- [ ] **Step 8: Validate active compatibility and compose technical evidence**

Capture `compatibility status --json`, require exact installed bundle/profile digests, then run Task 5 `compose` and `validate`. Expected evidence status is `passed`, track is `technical_private_alpha`, usability is `deferred_unacquired`, and both External Beta/commercial claims are false.

- [ ] **Step 9: Retire the obsolete manual App capture through package-owned lifecycle commands**

Query exact surface-capture status. If the old run is still unclaimed and armed, cancel that exact run through the package-owned command. If it is expired, invoke only the documented package-owned expired-run cleanup. If claimed/finalized, stop and report its exact lifecycle state rather than deleting or relabelling it. Confirm no capture process or project-session owner remains.

- [ ] **Step 10: Commit documentation and real acquisition evidence separately**

```bash
git add docs/codex-integration/SPRINT-11-HOST-CONTRACT.md docs/codex-integration/EXTERNAL-CODEX-BETA-GUIDE.md docs/codex-integration/SPRINT-11-TECHNICAL-QUALIFICATION.md tests/codex/README.md
git commit -m "docs: replace routine host recapture with delta qualification"

git add tests/codex/acquisition/sprint11/host-delta-v0119-current tests/codex/acquisition/sprint11/technical-private-alpha-v0119
git commit -m "evidence: qualify current Codex hosts for 0.1.19"
```

- [ ] **Step 11: Perform the completion audit**

Verify each design success criterion against authoritative output:

- one command qualified the current rolling hosts;
- package archive/manifest hashes still equal the frozen `0.1.19` values;
- no install/setup/Trust/fault/form/capture command occurred;
- package-live, same-project, multi-project, and reproducibility receipts were reused by exact digest;
- all distinct official clients passed direct app-server inventory/status/semantic calls;
- active bundle/profile/sequence match the host-delta receipt;
- technical/private-alpha evidence validates;
- External Beta and commercial claims remain false;
- obsolete manual capture is no longer armed and no project session is held.

Only after every item is proven may the host-delta work be reported complete.
