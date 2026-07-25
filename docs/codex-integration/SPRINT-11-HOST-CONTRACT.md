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
- relative project-config paths resolve from the owning `.codex` directory;
- the official IDE extension identifier is `openai.chatgpt`;
- MCP stdio configuration supports command, args, cwd, required state,
  enabled/disabled tools, timeouts, and approval modes;
- app-server exposes `mcpServer/elicitation/request`, accept/decline/cancel
  results, `serverRequest/resolved`, and the
  `mcpServerOpenaiFormElicitation` initialization capability.

A historical locally generated app-server JSON Schema from Codex
`0.145.0-alpha.30` contains the same request, response, capability, and
`openai/form` definitions. That older coordinate is not the current
qualification candidate and proves neither implementation nor a UI workflow
for the current `0.146.0-alpha.3.1` CLI.

## 3. Qualification candidate matrix

The following block is rendered exactly from
`godot-codex-mcp/product/host-coordinate-profile.v1.json`; acceptance rejects
any Markdown/JSON drift.

<!-- BEGIN S11 HOST COORDINATE AUTHORITY -->
| Surface | Host identifier | Host version/build | Host artifact SHA-256 | Client version/SHA-256 | IDE shell | Qualification |
|---|---|---|---|---|---|---|
| app | `com.openai.codex` | `26.721.31836` / `5828` | `sha256:1e69df41e05969f1487dfdc9f72a600ef3b7627a7f8ee9d481cb8f88400eda45` | `0.146.0-alpha.3.1` / `sha256:a2b6198fd61327f54542716bd96e588c5b10789522fee4bbacaeff1aa7836efb` | — | candidate |
| cli | `codex-cli` | `0.146.0-alpha.3.1` / `0.146.0-alpha.3.1` | `sha256:a2b6198fd61327f54542716bd96e588c5b10789522fee4bbacaeff1aa7836efb` | `0.146.0-alpha.3.1` / `sha256:a2b6198fd61327f54542716bd96e588c5b10789522fee4bbacaeff1aa7836efb` | — | candidate |
| ide | `openai.chatgpt` | `26.721.30844` / `26.721.30844` | `sha256:3ff47b070a08d02acc9c596756b017cf264e3dd4169002613a9171e1219778b0` | `0.146.0-alpha.3` / `sha256:5ab45f8f9819c120bede3743f896e70da47ffe920b48d9a04cc25ecc9e2dd757` | com.microsoft.VSCode 1.127.0 `sha256:d2dbd60db1c2e63e6b844a1c13b61e6de12bc20391c8ec1bf7bf663b67b105a1` | candidate |
<!-- END S11 HOST COORDINATE AUTHORITY -->

| Component | Exact observed coordinate | Candidate state |
|---|---|---|
| OS | macOS `26.5.2` build `25F84`, `arm64` | candidate |
| Codex desktop | bundle `com.openai.codex`, version `26.721.31836`, build `5828`, team `2DC432GLL2` | static capability proven; live workflow pending |
| App-bundled Codex CLI | `0.146.0-alpha.3.1`, `arm64` | action-only empty form live-proved exact `accept`, `decline`, `cancel`, and `timeout`; full packaged surface workflow pending |
| App-bundled CLI artifact | `sha256:a2b6198fd61327f54542716bd96e588c5b10789522fee4bbacaeff1aa7836efb` | candidate artifact identity |
| VS Code Stable | `1.127.0`, commit `4fe60c8b1cdac1c4c174f2fb180d0d758272d713`, `arm64`, team `UBF8T346G9` | candidate |
| Official Codex extension | `openai.chatgpt@26.721.30844`, `darwin-arm64` | installed candidate; live workflow pending |
| Extension package | `sha256:497c84587406f0cb7022dece0752202dc4490781fde5bc88469207ab9b5cac13` | candidate artifact identity |
| Extension Codex CLI | `bin/macos-aarch64/codex`, `0.146.0-alpha.3`, `sha256:5ab45f8f9819c120bede3743f896e70da47ffe920b48d9a04cc25ecc9e2dd757`, team `2DC432GLL2` | candidate embedded client |
| MCP protocol | `2025-11-25`; form-compatible floor `2025-06-18` | candidate |
| Bridge | RPC `1.8` exact profile | candidate |
| Godot × Codex package | workspace `0.1.0` at baseline | final package coordinate pending |
| Godot Bridge prerequisite | `bin/godot.macos.editor.dev.arm64`, `4.8.dev.codex.11785494e`, `sha256:907001ec5c88b11173795859f9cb3c859b6ac4b5bc862da9ace92b6dd016f401` | exact local candidate; detached package binding pending |

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
probes summarized above prove `cancel` and `timeout` for
`0.146.0-alpha.3.1`.

The current four-outcome result proves the isolated action boundary for this
CLI coordinate, but not App, IDE, the full packaged workflow, or human
usability. The current project root is trusted and app-server resolves a
distinct project configuration layer. The layer is empty because the
repository intentionally has no applied `.codex/config.toml` yet; setup and
effective reload therefore remain unproved.

## 4. Current blockers and fail-closed interpretation

VS Code Stable now contains the official
`openai.chatgpt@26.721.30844` extension. Installation is only acquisition; no
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
`<data-root>/current/bin/godot-codex-mcp`, never a PATH basename, and doctor
must compare that exact effective command. Until the packaged probes pass,
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
  "$HOME/.vscode/extensions/openai.chatgpt-26.721.30844-darwin-arm64/package.json"
shasum -a 256 \
  "$HOME/.vscode/extensions/openai.chatgpt-26.721.30844-darwin-arm64/bin/macos-aarch64/codex"
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
  --extension-root "$HOME/.vscode/extensions/openai.chatgpt-26.721.30844-darwin-arm64" \
  --extension-package-json "$HOME/.vscode/extensions/openai.chatgpt-26.721.30844-darwin-arm64/package.json" \
  --ide-client "$HOME/.vscode/extensions/openai.chatgpt-26.721.30844-darwin-arm64/bin/macos-aarch64/codex" \
  --output-root tests/codex/acquisition/sprint11/host-provenance
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
