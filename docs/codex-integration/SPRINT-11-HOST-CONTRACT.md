# Sprint 11 host contract — External Codex Beta candidate

**Status:** S11-01 candidate observed; the current App-bundled CLI
action-only `accept`, `decline`, `cancel`, and `timeout` paths are
live-proven. Closed evidence/acquisition schemas and their validators are
source-frozen, but C11-0 remains open until the real App/official-IDE form,
root, launcher, config-reload, surface-authority, and human-usability probes
pass

**Observed:** 2026-07-24, Asia/Bishkek

**Sprint 10 input:** source
`b225f77acf48648ef6f59a76ff5a7dbb824da7bb`, evidence-only commit
`815bccedddcd7c64a387dc8e9d5de2e941003fea`, evidence file SHA-256
`9785a50e1be25f511acd6bd67f6642998821f700aea31a0bb4a7c1670f5b1611`

**Parent plan:** [SPRINT-11-PLAN](SPRINT-11-PLAN.md)

## 1. Authority and support boundary

The beta uses one local stdio `godot-codex-mcp` process for the exact trusted
project root. Codex App, CLI, and the official Codex extension for VS Code
Stable are the required clients. Cursor and a shell command that happens to
launch Cursor do not qualify the IDE gate.

The candidate records implementation presence separately from a passed user
workflow. Version strings, embedded schemas, or feature flags do not prove
that accept, decline, cancel, and timeout work end to end. A surface becomes
supported only after its packaged, redacted S11-13 trace passes the canonical
parity comparator.

Project setup cannot grant Codex trust, weaken sandbox or host approvals, or
pre-authorize a semantic transaction. Project `.codex/config.toml` is
effective only for a trusted project. A write additionally requires the
transaction's exact MCP form elicitation.

## 2. Official Codex contract coordinate

The current official Codex manual was fetched on
`2026-07-24T17:21:27+0600`.

```text
manual sha256:6cff0c5701d5a8b10ae497b5dda6ee70f478518d4fa029558168ee2bcc18918b
outline sha256:a9a9b6e6e26427885f160fa022f90a9e9c5e0f221c195716542cf69d68a8b4e0
```

The contract was checked against the official documentation for
[MCP setup](https://learn.chatgpt.com/docs/extend/mcp),
[project configuration](https://learn.chatgpt.com/docs/config-file/config-advanced#project-config-files-codexconfigtoml),
[IDE settings](https://learn.chatgpt.com/docs/developer-settings?surface=ide),
[AGENTS.md](https://learn.chatgpt.com/docs/agent-configuration/agents-md),
[skills](https://learn.chatgpt.com/docs/build-skills),
[sandbox and approvals](https://learn.chatgpt.com/docs/sandboxing), and
[app-server](https://learn.chatgpt.com/docs/app-server#mcp-server-elicitation-requests).

The checked contract establishes:

- App, CLI, and IDE share the Codex configuration layers;
- project `.codex/config.toml` is loaded only for a trusted project;
- the qualification-candidate Codex `0.146.0-alpha.9.2` MCP launcher uses the
  task runtime directory only as the fallback when stdio `cwd` is omitted;
  setup therefore forbids an explicit relative `cwd` and uses a package-owned
  prelaunch command which resolves the nearest Godot root from that host-owned
  coordinate and verifies the setup receipt before starting the sidecar;
- the official IDE extension identifier is `openai.chatgpt`;
- MCP stdio configuration supports command, args, cwd, explicit per-server
  environment, required state, enabled/disabled tools, timeouts, and approval
  modes;
- app-server exposes `mcpServer/elicitation/request`, accept/decline/cancel
  results, `serverRequest/resolved`, and the
  `mcpServerOpenaiFormElicitation` initialization capability.

A historical locally generated app-server JSON Schema from Codex
`0.145.0-alpha.30` contains the same request, response, capability, and
`openai/form` definitions. That older coordinate is not the current
qualification candidate and proves neither implementation nor a UI workflow
for the current `0.146.0-alpha.9.2` CLI.

## 3. Qualification candidate matrix

The following block is rendered exactly from
`godot-codex-mcp/product/host-coordinate-profile.v1.json`; acceptance rejects
any Markdown/JSON drift.

<!-- BEGIN S11 HOST COORDINATE AUTHORITY -->
| Surface | Host identifier | Host version/build | Host artifact SHA-256 | Client version/SHA-256 | IDE shell | Qualification |
|---|---|---|---|---|---|---|
| app | `com.openai.codex` | `26.727.51351` / `6119` | `sha256:e184ce460ed0565166e295507ad9ae6d7003fa8bd5d2e2f2f9ee8b5602feb2d4` | `0.146.0-alpha.9.2` / `sha256:d96ae1ca1ff6fc8587842fa04c92d3ee4d31651a811c2f89b65fcfd9c28473e2` | — | candidate |
| cli | `codex-cli` | `0.146.0-alpha.9.2` / `0.146.0-alpha.9.2` | `sha256:d96ae1ca1ff6fc8587842fa04c92d3ee4d31651a811c2f89b65fcfd9c28473e2` | `0.146.0-alpha.9.2` / `sha256:d96ae1ca1ff6fc8587842fa04c92d3ee4d31651a811c2f89b65fcfd9c28473e2` | — | candidate |
| ide | `openai.chatgpt` | `26.721.41059` / `26.721.41059` | `sha256:ea66cea39f5c40d83079fe200251ac698afe285e4cb30d335e4ee6517ee7b8aa` | `0.146.0-alpha.3.1` / `sha256:fa0cb7c5f80e6a192563fcb1d9f98857f4a808a28cb29289400ed7110291bce4` | com.microsoft.VSCode 1.130.0 `sha256:e1e3268741a2658a22b31e82b58a42fa48be73f64fc2de006be48a2ba136b930` | candidate |
<!-- END S11 HOST COORDINATE AUTHORITY -->

| Component | Exact observed coordinate | Candidate state |
|---|---|---|
| OS | macOS `26.5.2` build `25F84`, `arm64` | candidate |
| Codex desktop | bundle `com.openai.codex`, version `26.727.51351`, build `6119`, team `2DC432GLL2` | static capability proven; live workflow pending |
| App-bundled Codex CLI | `0.146.0-alpha.9.2`, `arm64` | full packaged surface workflow pending |
| App-bundled CLI artifact | `sha256:d96ae1ca1ff6fc8587842fa04c92d3ee4d31651a811c2f89b65fcfd9c28473e2` | candidate artifact identity |
| VS Code Stable | `1.130.0`, commit `1b6a188127eeaf9194f945eb6eb89a657e93c54c`, `arm64`, team `UBF8T346G9` | candidate |
| Official Codex extension | `openai.chatgpt@26.721.41059`, `darwin-arm64` | installed candidate; live workflow pending |
| Extension package | `sha256:fa2a88ea55413183654f5613b14e744517ad7626d26a673403bdde83c73adea7` | candidate artifact identity |
| Extension Codex CLI | `bin/macos-aarch64/codex`, `0.146.0-alpha.3.1`, `sha256:fa0cb7c5f80e6a192563fcb1d9f98857f4a808a28cb29289400ed7110291bce4`, team `2DC432GLL2` | candidate embedded client |
| MCP protocol | `2025-11-25`; form-compatible floor `2025-06-18` | candidate |
| Bridge | RPC `1.8` exact profile | candidate |
| Godot × Codex package | workspace `0.1.11` | same-project lease contention, long project-root UDS transport, accepted-socket subprocess inheritance, full-beta doctor registry probing, current host-measurement guidance, receipt-1.1 config fault harness, isolated-install package-owned guidance, and App-safe fail-closed project-bound MCP prelaunch fixed; independently installable host-surface bundle added |
| Godot Bridge prerequisite | `bin/godot.macos.editor.dev.arm64`, `4.8.dev.codex.336fc9a13`, `sha256:2166f3c6b7784cc7259a08a9636aafe89c8d6bebcbdc05c2b1d8d76933373dff` | exact local candidate; detached package binding pending |

### 3.1 Independent host-surface compatibility updates

Package `0.1.11` keeps package, Godot, protocol, schema, registry, Bridge, and
Cursor coordinates embedded and immutable. A separately released
`godot-codex-surface-compatibility-bundle/1.0` may replace only the complete
App/CLI/IDE surface snapshot for that exact embedded matrix. This permits a
new Codex host qualification without rebuilding or silently changing the
sidecar.

Every bundle is bound to:

- the exact package version and target;
- the canonical SHA-256 of the embedded baseline matrix;
- a monotonically increasing, JavaScript-safe sequence;
- the raw SHA-256 of a detached, exact host-coordinate profile;
- a complete, unique App/CLI/IDE rule set.

Installation requires the bundle, its detached host profile, and the bundle
SHA-256 published through the trusted release channel. The CLI rechecks both
files during preview and apply, persists a private expiring plan, rejects
equal or lower sequences, and commits the bundle plus profile as one atomic
private state document. Unknown fields, malformed files, binding drift,
source replacement, replay, downgrade, symlinks, or an invalid active state
fail closed. No bundle can alter a project, trust, Codex config, Godot,
package binaries, protocols, schemas, registry, Bridge capabilities, or
transaction policy.

```sh
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  compatibility install \
  --bundle /path/to/surface-compatibility-bundle.json \
  --host-profile /path/to/host-coordinate-profile.json \
  --expected-sha256 sha256:<published-bundle-digest> \
  --dry-run --json

"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  compatibility install --apply-plan sha256:<preview-plan-digest> --json

"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  compatibility status --json
```

The expected bundle SHA-256 is an explicit offline release-authority input;
the bundle is not self-authenticating and must not be accepted from an
untrusted channel. Doctor reports the effective matrix digest. If an active
bundle becomes invalid, doctor reports `package_invalid` instead of silently
falling back to embedded host claims.

The current App-bundled CLI reports `tool_call_mcp_elicitation` as stable.
Real stdio probes through `tests/codex/sprint11_approval_host_probe.py`
negotiated MCP `2025-06-18`, received an action-only `openai/form` with an
empty object schema, and proved all four protocol outcomes:

| Requested outcome | Exact host action | Stable result code | Receipt eligible | Mutation |
|---|---|---|---|---|
| accept | `accept` | `approval_accepted` | yes | no |
| decline | `decline` | `approval_declined` | no | no |
| cancel | `cancel` | `approval_cancelled` | no | no |
| timeout | no host response before the server deadline | `approval_timeout` | no | no |

The empty schema is intentional. In the installed CLI it renders distinct
`Allow`, `Deny`, and `Cancel` actions, so `Deny` produces the protocol's
`decline` action rather than an accepted boolean value. The probe accepts no
content substitute and reported `project_mutated: false` for every outcome.
Only the exact host `accept` action is receipt-eligible.

Earlier working notes recorded four action-only outcomes against
`0.145.0-alpha.30`. That result is historical only and does not qualify or
substitute for the current candidate. The independent current-coordinate
probes summarized above prove `cancel` and `timeout` for the historical
`0.146.0-alpha.3.1` coordinate; the current `0.146.0-alpha.9.2` coordinate
requires a fresh four-outcome capture.

The current four-outcome result proves the isolated action boundary for this
CLI coordinate, but not App, IDE, the full packaged workflow, or human
usability. The current project root is trusted and app-server resolves a
distinct project configuration layer. The layer is empty because the
repository intentionally has no applied `.codex/config.toml` yet; setup and
effective reload therefore remain unproved.

## 4. Current blockers and fail-closed interpretation

VS Code Stable now contains the official
`openai.chatgpt@26.721.41059` extension. Installation is only acquisition; no
IDE MCP workflow or form outcome is qualified yet, and Cursor cannot replace
that gate.

`/usr/local/bin/code` currently resolves to Cursor. Every S11 runner must use
the explicit VS Code Stable executable until the shell command is repaired:

```text
/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code
```

App has static form support. Its bundled CLI has current real action-only
`accept`, `decline`, `cancel`, and `timeout` traces, while the full packaged
surface trace is not yet qualified. All App/IDE outcomes and independent
human usability remain pending. Root/cwd semantics, effective non-empty
project config, and packaged launcher execution also remain pending. The
launcher decision is fixed: setup must write the verified absolute
`<data-root>/current/bin/godot-codex` operations launcher with
`args = ["mcp", "--project-root", "."]` and no explicit `cwd`, never a PATH
basename or an embedded project cwd. Prelaunch must prove the host-owned task root,
receipt-owned config, and exact sidecar package before replacing itself with
`<data-root>/current/bin/godot-codex-mcp`; doctor must compare that exact
effective command plus the owned
`GODOT_CODEX_DATA_ROOT=<data-root>` server environment. The explicit
per-server value is required because Codex hosts do not promise to forward an
installer shell environment to MCP children. Until the packaged probes pass,
C11-0 and write qualification remain open; a host without the required
interaction is read-only.

Package and acquisition subprocess cleanup is qualified only for the frozen,
source-bound runner graph. Owned clean-environment boundaries preserve the
unrecorded scope markers, and the runners fail closed on observed surviving
descendants. This is lifecycle safety, not a kernel-enforced sandbox for an
adversarial executable that deliberately clears those markers. A privileged
or entitled macOS supervisor remains deferred release-grade hardening and is
not implied by the External Codex Beta claim.

## 5. Reproducible probe commands

```sh
sw_vers
uname -m

/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' \
  '/Applications/ChatGPT.app/Contents/Info.plist'
/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
  '/Applications/ChatGPT.app/Contents/Info.plist'
/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' \
  '/Applications/ChatGPT.app/Contents/Info.plist'

/Applications/ChatGPT.app/Contents/Resources/codex --version
shasum -a 256 /Applications/ChatGPT.app/Contents/Resources/codex
/Applications/ChatGPT.app/Contents/Resources/codex features list

'/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code' \
  --version
'/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code' \
  --list-extensions --show-versions
shasum -a 256 \
  "$HOME/.vscode/extensions/openai.chatgpt-26.721.41059-darwin-arm64/package.json"
shasum -a 256 \
  "$HOME/.vscode/extensions/openai.chatgpt-26.721.41059-darwin-arm64/bin/macos-aarch64/codex"
readlink /usr/local/bin/code
```

Probe output included in final evidence must be normalized and redacted. It
must not record the user home, project absolute root, account, tokens,
discovery endpoint, PID, prompts, source text, or approval content.

After the package source coordinate is frozen, acquire the installed-host
authority from that exact clean commit with all eight installed paths
specified explicitly and a new repository-local output directory:

```sh
python3 tests/codex/sprint11_external_acquisitions.py host-provenance \
  --app-bundle '/Applications/ChatGPT.app' \
  --app-executable '/Applications/ChatGPT.app/Contents/MacOS/ChatGPT' \
  --app-client '/Applications/ChatGPT.app/Contents/Resources/codex' \
  --vscode-bundle '/Applications/Visual Studio Code.app' \
  --vscode-executable '/Applications/Visual Studio Code.app/Contents/MacOS/Code' \
  --extension-root "$HOME/.vscode/extensions/openai.chatgpt-26.721.41059-darwin-arm64" \
  --extension-package-json "$HOME/.vscode/extensions/openai.chatgpt-26.721.41059-darwin-arm64/package.json" \
  --ide-client "$HOME/.vscode/extensions/openai.chatgpt-26.721.41059-darwin-arm64/bin/macos-aarch64/codex" \
  --output-root tests/codex/acquisition/sprint11/host-provenance-v019
```

The output directory must not already exist. `receipt.json` is the
source-bound acquisition envelope: it binds the package-source commit, public
runner, measurement and envelope schemas, host-coordinate profile, exact
redacted command template, and produced measurement digest.
`measurement.json` is the separate content-free installed-artifact
measurement for App, CLI, and IDE, including complete extension-tree and
verified code-signature projections. Surface recorder metadata and final
traces bind the `measurement.json` SHA-256, not the envelope SHA-256. Final
evidence binds both files and acceptance permits only those two newly produced
host-provenance paths after the package commit.

## 6. Frozen decisions and open no-go checks

Frozen:

- Bridge RPC remains `1.8`; Sprint 11 adds no editor semantics.
- MCP-001 targets `1.0` with exactly 41 tools, four fixed resources, and one
  scene-summary resource template.
- One canonical project root owns one sidecar and all of its state.
- Offline authority is limited to a source-hash-verified saved generation.
- Setup writes only its owned table/artifacts after exact digest-bound consent.
- Doctor is model-free, network-free, deterministic, and read-only by default.
- Compatibility comes from the package matrix rather than prefix guesses.
- S11 can close only the local macOS external-client slice; formal release
  `R1-08`/`R1-10` stay partial and Beta Gate is not reached.

Open, blocking C11-0:

- prove the complete real form lifecycle on App and official IDE;
- acquire the full packaged CLI surface trace and independent human-usability
  report; the isolated CLI action-only lifecycle alone is not surface
  qualification;
- prove root/cwd and non-empty project-config reload from a nested task;
- prove one package launcher is visible to all three surfaces;
- bind the exact separately distributed Godot Bridge prerequisite into the
  detached package manifest;
- acquire records against the source-frozen evidence schemas, canonical prompt
  pack, and human acquisition contract; their presence in source is not an
  external observation.
