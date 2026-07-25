# Sprint 11 plan — external Codex productization

**Status:** In progress; non-qualifying source implementation is under local
verification, while S11-01 packaged host feasibility and the S11-12–14
external qualification gates still block beta promotion

**Milestone:** External Codex Beta

**Duration:** 2 weeks

**Sprint 10 input baseline:** qualified source
`b225f77acf48648ef6f59a76ff5a7dbb824da7bb`; evidence-only commit
`815bccedddcd7c64a387dc8e9d5de2e941003fea`; evidence SHA-256
`9785a50e1be25f511acd6bd67f6642998821f700aea31a0bb4a7c1670f5b1611`

**Hard dependencies:** Sprint 10 M3 source-bound macOS arm64 qualification;
Bridge RPC 1.8; the 40-tool MCP surface; persistent resource/scene/script
index; live editor/runtime projections; compound transactions, validation,
rollback, and host-owned confirmation.

**Normative project contracts:**
[PRODUCT-001](PRODUCT-001-semantic-bridge-vision-and-plan.md),
[ARCHITECTURE-001](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md),
[RELEASE-001](RELEASE-001-release-1.0-acceptance-and-evidence-plan.md),
[PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md),
[MCP-001](MCP-001-project-scoped-read-tools.md),
[INDEX-001](INDEX-001-semantic-index-storage-and-migrations.md),
[EVIDENCE-001](EVIDENCE-001-semantic-facts-and-evidence.md),
[EDITOR-001](EDITOR-001-live-editor-context.md),
[RUNTIME-001](RUNTIME-001-debugger-and-runtime-observation.md),
[WRITE-001](WRITE-001-editor-transactions-and-undo.md), and
[VALIDATION-001](VALIDATION-001-automatic-validation-and-rollback.md).

**Current Codex host contract:** local stdio MCP servers are supported by the
Codex desktop app, Codex CLI, and Codex IDE extension, and those surfaces share
the same `config.toml` layers. Project `.codex/config.toml` is loaded only for
a trusted project. Repository skills live under `.agents/skills`, while
durable repository guidance lives in `AGENTS.md`. The S11 contract is based on
the current official
[MCP setup](https://learn.chatgpt.com/docs/extend/mcp),
[project configuration](https://learn.chatgpt.com/docs/config-file/config-advanced#project-config-files-codexconfigtoml),
[AGENTS.md guidance](https://learn.chatgpt.com/docs/agent-configuration/agents-md),
[skills guidance](https://learn.chatgpt.com/docs/build-skills), and
[sandbox/approval model](https://learn.chatgpt.com/docs/sandboxing).
These assumptions are rechecked and version-bound by `S11-01`.

The Sprint 10 pair above is immutable input evidence. It validated before
Sprint 11 work began; later Sprint 11 checkouts are not re-labelled as the
Sprint 10 source coordinate.

Current local observations, the official-manual digest, exact candidate
coordinates, and open feasibility blockers are recorded in
[SPRINT-11-HOST-CONTRACT](SPRINT-11-HOST-CONTRACT.md).

## 1. Outcome and fixed decisions

Sprint 11 turns the qualified semantic bridge into an installable, diagnosable
project-scoped beta for daily use from external Codex surfaces. A new user can
install two local binaries, preview and approve project configuration, start a
task in the exact Godot project, understand ready/offline/error state, and run
the same read/runtime/write workflow from the desktop app, CLI, and supported
IDE extension.

- Bridge RPC remains `1.8`. Sprint 11 adds no new editor semantic capability
  and does not redefine transaction, validation, revision, or evidence
  semantics.
- MCP-001 becomes final version `1.0` for the External Codex Beta. Existing
  forty tool names and behaviors are frozen; later changes must be additive or
  explicitly versioned.
- Sprint 11 adds one read-only tool,
  `godot_get_connection_status`, and one fixed resource,
  `godot://connection/status`. The production registry becomes exactly 41
  tools, four fixed resources, and one resource template. Fixed resources and
  templates are counted and tested as separate MCP surfaces.
- Connection status is served by the sidecar even when Bridge discovery,
  editor, runtime, or static cache is unavailable. It returns bounded project,
  component, compatibility, cache, and remediation state without secrets,
  native handles, absolute roots, or stale live data.
- The sidecar remains a local stdio MCP server and contains no OpenAI API key,
  model client, user credential, remote listener, or surface-specific semantic
  adapter.
- One canonical project root owns one sidecar process, index, transaction
  journal, discovery binding, and MCP server instance. Support for multiple
  open Godot projects uses isolated project-scoped sidecar processes, not a
  global router or automatic editor selection.
- A Codex task receives the `.codex/config.toml` layer for its exact trusted
  project. A task for project A cannot enumerate, connect to, query, run, or
  mutate project B even when both editors and sidecars are active.
- The new operations binary is `godot-codex`; the MCP stdio binary remains
  `godot-codex-mcp`. Initial commands are `setup`, `doctor`, and `version`.
- `godot-codex setup` never edits a project on invocation alone. It first
  emits an exact bounded plan/diff and requires an interactive accept or a
  non-interactive acceptance bound to that plan digest.
- Setup merges only its owned MCP table and optional guidance artifacts. It
  preserves unrelated TOML, comments, AGENTS instructions, skills, and
  configuration. Conflicting ownership fails closed; no `--force` silently
  replaces user content.
- Setup offers explicit `read-only` and `full-beta` profiles. Both use a
  closed generated tool allowlist from the canonical 41-tool registry.
  `full-beta` configures host approval mode for writes; it never disables the
  transaction's nested form confirmation or Codex sandbox.
- Generated project config contains no token, approval grant, API key,
  absolute project path, or machine-local credential. The MCP binary must be
  installed through the package's user-local launcher. S11-01 must prove the
  launcher is visible to App, CLI, and VS Code; setup and doctor fail closed
  on a missing host-visible path instead of embedding an unshareable path.
- Doctor is model-free, network-free, non-mutating by default, and available
  before the MCP server starts. It distinguishes binary, project/config,
  trust/effective-config, discovery, protocol version, authentication,
  binding, index/cache, and surface restart failures.
- Doctor JSON uses a versioned closed schema, stable diagnostic codes,
  severity, component, observed/supported version fields, and actionable
  remediation. Default output redacts absolute roots and private discovery
  data; a local user may explicitly request path detail.
- Offline mode is read-only. A validated compatible static generation may
  answer saved resource/scene/script/find-usages queries when the editor is
  closed. Live editor, runtime, history, run control, screenshot, prepare,
  apply, validation, policy, and Undo tools fail with a structured offline
  error.
- Static facts are `current` only after the sidecar proves current source
  hashes and a complete compatible generation. An unverified retained
  generation is `stale`/unavailable and never masquerades as current. Previous
  editor/runtime overlays are excluded.
- Reconnect to an editor always authenticates the exact discovery binding,
  obtains a new live full snapshot when required, and atomically overlays the
  offline static generation. Cache state never bypasses editor-session
  revisions.
- Compatibility is data, not a permissive guess. A machine-readable matrix
  binds Godot build/commit, Bridge range, sidecar/package version, MCP
  protocol, index schema, host surface/version, IDE host, OS, and architecture.
- Unsupported or untested combinations are `incompatible` or `not_tested`;
  they do not become supported through version-string prefix matching. A
  compatible lower Bridge minor may expose its honest reduced capability set.
- The required IDE surface is the official Codex extension on VS Code Stable.
  Cursor and other compatible editors may be recorded as `not_tested`, but
  they do not substitute for the required VS Code gate.
- Qualification candidates observed on 2026-07-25 are Codex desktop app
  `com.openai.codex` `26.721.41059` build `5848` (team `2DC432GLL2`),
  its bundled Codex CLI `0.146.0-alpha.3.1`
  (`sha256:6d8be49e49751554df16572369e636cbe02c84b208cad3dc35528c846eeca223`),
  VS Code Stable `1.127.0` commit
  `4fe60c8b1cdac1c4c174f2fb180d0d758272d713`, and official extension
  `openai.chatgpt@26.721.30844`. The extension embeds Codex CLI
  `0.146.0-alpha.3` at `bin/macos-aarch64/codex`
  (`sha256:5ab45f8f9819c120bede3743f896e70da47ffe920b48d9a04cc25ecc9e2dd757`).
  These are observations, not support promises; S11-01 records the exact
  coordinates used by final evidence. On the current App-bundled CLI,
  action-only `accept`, `decline`, `cancel`, and `timeout` are live-proven.
  This isolated lifecycle proof does not qualify the App, IDE, or full
  packaged CLI surface workflow.
- MCP server instructions become normative workflow guidance. Their first 512
  characters explain project binding, connection status, offline limits, and
  the requirement to preview/approve writes. Instructions cannot grant
  authority or weaken tool schemas.
- The repository guidance package contains a concise `AGENTS.md` fragment and
  a repo-scoped `.agents/skills/godot-editor/SKILL.md`. The skill teaches the
  status/read/runtime/transaction/validation/Undo workflow; it does not
  implement a second client or embed secrets/config.
- Surface parity compares canonical tool inputs/results, entity IDs, facts,
  evidence, revisions, transaction previews, approval decisions, validation
  reports, and errors through the normalization policy in §3.8. It never
  requires raw equality of session-scoped IDs or revision counters from
  isolated runs. It intentionally ignores UI layout, model prose, request
  IDs, timestamps, and host-specific presentation.
- Writes must prove real `elicitation.form` support on every qualified
  surface. A host without the required interaction is read-only for writes;
  no boolean, prose, shell prompt, or pre-approved config substitutes.
- The beta package is local macOS arm64, versioned, checksum-bound, and
  reproducible. It is bound to one exact separately distributed Godot Bridge
  build; the Godot artifact is a prerequisite and is not silently replaced by
  a developer build. Signing/notarization and three-OS installer qualification
  remain later release work; the package reports that limitation honestly.
- The blocking Sprint 11 evidence includes real App, CLI, and IDE surface
  traces plus a task-based usability report. Model-free protocol tests alone
  cannot close External Codex Beta.

## 2. Gates and deliverables

| Gate | Deliverable | Exit condition |
|---|---|---|
| `S11-01` | This plan, final MCP-001 v1 decisions, support/version matrix draft | Surface, config, offline, package, doctor, parity, approval, and support boundaries are frozen |
| `S11-02` | Final MCP v1 profile: 41 tools, four fixed resources, one template | Schemas, descriptions, instructions, annotations, output contracts, and compatibility pass |
| `S11-03` | Machine-readable compatibility matrix | Doctor and acceptance consume exact supported/tested component coordinates |
| `S11-04` | `godot-codex doctor` and diagnostic engine | Required fault classes produce distinct stable codes and remediation |
| `S11-05` | Consent-bound setup/config repair | Dry-run/digest acceptance and ownership-preserving TOML merge pass |
| `S11-06` | Connection status and remediation projection | Ready/offline/syncing/incompatible/auth/config states remain available and bounded |
| `S11-07` | Offline validated static-cache mode | Saved semantic queries work honestly; every live/runtime/write path fails closed |
| `S11-08` | Concurrent multi-project isolation | Two editor/sidecar/task bindings remain independent under faults and reconnect |
| `S11-09` | Skill, AGENTS, setup, approval, sandbox, and troubleshooting guidance | A user follows one current package-owned workflow without stale read-only instructions |
| `S11-10` | Reproducible external beta package/installer | Versioned macOS arm64 archive installs both binaries and verifies manifest/hashes |
| `S11-11` | Surface recorder and canonical parity comparator | One redacted trace schema compares App, CLI, and IDE semantics |
| `S11-12` | Task-based usability qualification | Independent users connect, diagnose, work, and recover without developer assistance |
| `S11-13` | Real external-surface acceptance workflow | Read/runtime/write/offline/multi-project scenarios pass on all required surfaces |
| `S11-14` | Source/package-bound External Codex Beta evidence | Clean local macOS evidence and acceptance report validate from the final package |

Primary roadmap artifacts are the external beta installer/package, setup and
troubleshooting guide, `godot-codex doctor`, final MCP-001 v1, compatibility
matrix, guidance package, and external Codex acceptance report.

Every implementation commit builds and passes its relevant gates. Generated
evidence never shares a commit with production, package, docs, fixture, trace
normalizer, oracle, source-scope, runner, or validator changes.

### S11-01 — Freeze the External Codex Beta contract

**Depends on:** the immutable Sprint 10 input pair recorded above validated
before Sprint 11 work began.

**Changes:** revise MCP-001 from accumulated sprint draft to final `1.0`;
approve the implemented VALIDATION-001 status; update stale sidecar/config/
AGENTS documentation; freeze:

- exact 41-tool, four-fixed-resource, one-template registry and MCP protocol
  floor;
- server instructions, tool titles/descriptions, schemas, annotations, and
  error/remediation vocabulary;
- one-sidecar-per-project topology and multi-project isolation;
- offline static authority and prohibited domains;
- setup ownership, consent, merge/remove, and non-interactive acceptance;
- doctor checks, JSON schema, exit codes, redaction, and remediation;
- package layout, versioning, install root, rollback, and unsupported claims;
- required App/CLI/IDE surface/version coordinates;
- approval/sandbox expectations and surface interaction requirements;
- parity normalization, usability rubric, and evidence retention.

Re-fetch the current official Codex manual and record its source digest/date.
Probe the installed desktop app, CLI, official IDE extension, and VS Code.
Freeze exact qualification versions in a machine-readable candidate matrix.
The required IDE is VS Code Stable with the official Codex extension; absence
of that extension blocks IDE acceptance instead of silently using Cursor.

Before any write-capable implementation is accepted, run a minimal real stdio
MCP fixture separately from App, CLI, and the official VS Code extension. Each
surface must prove enumeration, tool invocation, and form
accept/decline/cancel/timeout. Embedded schemas, feature flags, app-server
capabilities, or client names are only feasibility evidence. If a required
surface cannot complete the real form lifecycle, Sprint 11 remains blocked;
there is no boolean, prose, config, or shell fallback.

The same feasibility gate proves:

- whether project config `cwd` is config-relative or task-relative when a task
  starts from a nested directory;
- the portable rule for locating exactly one owning `project.godot` root, with
  ambiguity rejected;
- the user-local launcher path visible to GUI App, CLI, and VS Code without
  editing a shell profile;
- effective reload/restart behavior for a non-empty project config;
- the exact official extension version used by the IDE candidate.

Freeze one exact Godot Bridge artifact as an external prerequisite, including
build/commit, SHA-256, architecture, installation guidance, and compatibility
row. S11 does not qualify against an unrecorded developer binary.

Add a closed S11 evidence contract before implementation. Canonical,
non-secret prompts live in a version-controlled prompt pack; evidence records
its path, ID, and digest instead of participant free-form prompts. Human
usability acquisition is a separate prerequisite with consent, pseudonymous
participant IDs, a frozen package, defect/retest rules, and an independent
participant. The automated acceptance wrapper validates the acquired,
source-bound reports; it does not pretend to conduct human or GUI sessions
within its timeout.

Sprint 11 closes only the local macOS external-client slice:
`external_codex_beta_macos = passed`. Formal release criteria remain
`R1-08 = partial` because Dock is deferred and `R1-10 = partial` because
Windows, Linux, and final uninstall are not run. The roadmap Beta Gate remains
`not_reached`.

**Tests/review:** contract trace from every roadmap bullet and criterion;
threat review of config writes, launcher lookup, root/cwd ambiguity, project
trust, multi-project confusion, offline staleness, trace leakage, approvals,
and package replacement; manual-vs-local capability comparison; real
three-surface form/root/launcher feasibility; exact Godot artifact binding;
evidence and participant acquisition contract review.

**Done when:** no implementation decision about support, installation,
configuration ownership, root/launcher semantics, Godot distribution, offline
authority, status, compatibility, parity, evidence acquisition, or surface
qualification remains implicit, and the real three-surface feasibility gate
has no blocker.

### S11-02 — Finalize MCP-001 v1 and the connection-status surface

**Depends on:** S11-01.

**Changes:** add `godot_get_connection_status` and
`godot://connection/status`; freeze the exact registry at 41 tools, four fixed
resources, and one resource template; give every tool a stable title, concise
usage-oriented description, closed input/output schema, annotations, examples
where ambiguity exists, and bounded error mapping. Add server instructions
with a self-contained first 512 characters and a 4096-byte maximum.

The production `tools/list` route, rather than a private test-only router,
publishes all 41 output schemas. Each schema has mutually exclusive closed
success/error variants, unique required fields, safe JSON integer bounds, and
no non-standard unsigned formats. Fixed DTO objects are recursively closed;
project-defined semantic dictionaries use one explicitly marked finite-depth,
key/value/collection-bounded dynamic-map profile. The result normalizer emits
exactly one canonical JSON text block equivalent to `structuredContent`,
preserves separately validated non-text blocks, and replaces invalid server
output with `invalid_tool_result`.

Connection status contains:

- project identity/hash scope and sidecar/package version;
- `ready`, `connecting`, `syncing`, `offline_cached`, `offline_empty`,
  `incompatible`, `auth_failed`, `misconfigured`, or `overloaded`;
- Bridge/editor/runtime availability and negotiated protocol/capabilities;
- static cache schema, generation, source-hash verification, age, and
  freshness;
- outstanding recovery/transaction condition when safe;
- stable remediation code and safe next action.

It never returns the session token, endpoint, PID, absolute project root,
native ID, approval/policy grant, source text, or stale live/runtime payload.
Existing 40 tools retain their public names and successful-result meanings.

**Tests:** exact registry/resources on a real in-process MCP transport;
Draft 2020-12 compilation of every wire-published schema; duplicate-required,
closed-object, explicit-dynamic-map, safe-integer, and success/error
meta-validation; `structuredContent`/canonical-text equivalence; preservation
of image content; rejection of unknown fields, wrong types, malformed or mixed
errors, and over-depth values; description/instruction budgets; annotations;
offline invocation; tool timeout; output limits; secret/path scan; MCP
`2025-11-25`; form-compatible `2025-06-18` approval floor; all Sprint 6–10
tool regressions.

**Done when:** App, CLI, and IDE enumerate and understand the same bounded MCP
v1 contract, and status remains callable before a Bridge connection exists.

### S11-03 — Freeze and enforce the compatibility matrix

**Depends on:** S11-01.

**Changes:** add a canonical JSON Schema, machine-readable matrix, and rendered
Markdown table covering:

- beta package/sidecar/operations CLI semantic version and artifact hash;
- Godot fork commit/build identifier and platform target;
- Bridge RPC major/minor range and capability profile;
- MCP protocol and elicitation requirements;
- index/journal/report schema versions;
- Codex desktop app build;
- Codex CLI version;
- official Codex IDE extension and VS Code versions;
- OS/architecture and qualification status.

Evaluation statuses are `supported`, `compatible_reduced`, `incompatible`,
and `not_tested`. An exact observed host coordinate may additionally have the
matrix qualification `candidate`; it evaluates fail-closed as `not_tested`
with no enabled capabilities until the required real surface evidence
promotes it. Rules use parsed versions and exact artifact/build identities.
The embedded matrix describes rules and component coordinates and is consumed
by doctor, setup, server initialization, and acceptance; duplicated
hand-maintained version ranges are forbidden. A detached package manifest
binds the matrix hash and binary/artifact hashes. No binary or embedded matrix
contains a self-referential hash of itself.

**Tests:** exact candidate rows and fail-closed evaluation; explicitly
qualified supported row; compatible lower Bridge minor; future minor; wrong
major; stale sidecar; wrong architecture; schema mismatch; missing host
interaction; untested Cursor row; malformed/duplicate/overlapping rules;
matrix hash mismatch; documentation rendering parity.

**Done when:** every component combination has one deterministic capability
outcome and neither doctor nor MCP guesses support independently.

### S11-04 — Implement `godot-codex doctor`

**Depends on:** S11-03.

**Changes:** new Rust operations CLI crate and shared read-only diagnostics
engine. Command:

```text
godot-codex doctor --project-root <path> [--surface auto|app|cli|ide]
                   [--require-editor] [--json] [--show-paths]
```

Checks run in deterministic layers:

1. operations/MCP binary presence, executable bit, architecture, version, and
   package-manifest hash;
2. canonical project root and `project.godot`;
3. `.codex/config.toml` parse, owned MCP table, tool profile, timeouts, and
   expected effective Codex configuration;
4. project trust/restart symptoms without pretending to read private host
   trust state;
5. discovery existence, age, permissions, schema, lock/process liveness, and
   endpoint safety;
6. Bridge compatibility, authentication, editor session, and exact project
   binding;
7. index/cache schema, manifest, source hashes, corruption/rebuild state;
8. MCP initialization, status tool, registry hash, instructions, and required
   form interaction capability when the surface exposes it.

One machine-readable diagnostic/remediation registry is normative for doctor,
connection status, MCP errors, setup, and troubleshooting. Public codes
include `binary_missing`, `binary_not_executable`, `binary_arch_mismatch`,
`project_invalid`, `project_config_missing`, `project_config_invalid`,
`project_config_not_effective`, `bridge_discovery_missing`,
`bridge_discovery_stale`, `permissions_invalid`, `bridge_unreachable`,
`bridge_version_incompatible`, `bridge_authentication_failed`,
`project_binding_mismatch`, `static_cache_unavailable`,
`static_cache_incompatible`, `static_cache_corrupt`,
`static_cache_rebuilding`, and `ready`. A doctor `check_id` may describe an
internal phase, but its public `code` cannot rename the corresponding MCP
condition.

Canonical JSON reports schema, overall status, checks, versions, safe
observations, remediation IDs, and whether the editor is required. Exit codes:
`0` healthy/ready, `1` healthy offline or warning, `2` config/project,
`3` binary/package, `4` discovery/transport, `5` compatibility,
`6` authentication/binding, and `7` cache/internal failure.

**Tests:** each named fault in isolation and combinations; deterministic
precedence; offline with/without cache; changed discovery during probe; token/
root redaction; JSON/no-ANSI; `--show-paths`; no mutation/network/model call;
deadline; two projects; and a golden troubleshooting mapping.

**Done when:** the acceptance-required missing binary, stale discovery,
version mismatch, auth issue, and config issue are unambiguously different and
lead to actionable safe remediation.

### S11-05 — Implement consent-bound setup and configuration repair

**Depends on:** S11-02 through S11-04.

**Changes:** commands:

```text
godot-codex setup --project-root <path>
                  --profile read-only|full-beta
                  [--guidance none|agents|skill|all]
                  [--dry-run] [--json]

godot-codex setup --apply-plan <sha256>
godot-codex setup --remove --project-root <path>
```

Interactive setup always prints the exact target files, owned TOML table,
tool profile, guidance changes, package version, restart/trust instructions,
and diff before default-no confirmation. Non-interactive apply requires the
digest from a still-current dry-run plan; a changed file, package, project,
profile, or plan invalidates it.

The TOML editor atomically merges only
`[mcp_servers.godot_editor]`, using:

- an exact absolute `command` owned by the package: installed layouts use
  `<data-root>/current/bin/godot-codex-mcp` only after `current` resolves to
  `<data-root>/versions/<exact-package-version>` and package ownership and
  checksums verify; an explicitly supplied unpacked package uses its canonical
  sibling `bin/godot-codex-mcp`;
- `args = ["--project-root", "."]`;
- `cwd = ".."` because project-config-relative paths resolve from the owning
  `.codex` directory, so this starts the server at the repository root even
  when the task starts in a nested directory;
- `required = true`;
- bounded startup/tool timeouts appropriate for validation;
- the exact generated tool allowlist;
- `default_tools_approval_mode = "writes"` for `full-beta`.

Read-only excludes run controls and all mutation/approval/policy tools.
Full-beta includes all 41 tools while retaining host and semantic
confirmations. Existing conflicting `godot_editor` ownership fails with a
diff/remediation; unrelated config is preserved. Remove deletes only an exact
setup-owned stanza/artifact whose receipt/digest still matches.

Setup stores a private, bounded ownership receipt at
`.godot/codex/setup-receipt-v1.json`. The TOML table is owned only while its
canonical digest and ownership marker match that receipt. Receipt package
identities are historical metadata after an installer upgrade or explicit
rollback and are never resolved or executed as current authority. A new
configure, repair, or remove plan instead binds the currently verified package
version, manifest identity, launcher path, and executable bytes; apply
re-resolves those current bindings. Exact receipt-owned projects can therefore
preview and consent to a receipt migration in either version direction, while
a package switch after preview invalidates the plan. Reports never expose the
absolute launcher. A generated skill file is owned only when setup created it
and its whole-file digest still matches. An AGENTS fragment uses unique
begin/end markers plus a receipt-bound block digest; setup never claims the
rest of the file. Remove refuses any artifact changed outside those exact
ownership rules.

**Tests:** empty/existing/commented TOML; both profiles; all guidance modes;
decline/EOF/non-TTY; stale/wrong plan digest; symlink/path swap; permissions;
atomic-write fault; conflicting table; unrelated keys/comments preserved;
remove after user edit; configure/repair/remove after package upgrade and
rollback; current-link switch between preview/apply; no historical launcher
resolution; two project receipts from different versions; no secrets/absolute
root; git diff scope; generated config accepted by the frozen App, CLI, and
IDE parsers from both root and a nested task directory.

**Done when:** a user can configure the exact trusted project without manual
TOML editing, while automation cannot turn a preview into permission for a
different change.

### S11-06 — Add connection state and remediation semantics

**Depends on:** S11-02 and S11-04.

**Changes:** central `ConnectionHealth` state machine shared by MCP status,
doctor, stderr diagnostics, and setup verification. Normalize Bridge errors,
replica/index state, recovery state, and compatibility into stable status and
remediation IDs:

- `start_matching_editor`;
- `open_exact_project`;
- `restart_codex_surface`;
- `wait_for_full_sync`;
- `run_godot_codex_doctor`;
- `repair_project_config`;
- `fix_private_permissions`;
- `upgrade_godot_bridge`;
- `upgrade_godot_codex`;
- `rebuild_static_cache`;
- `resolve_transaction_recovery`.

Transitions are ordered and monotonic within an observation: starting,
discovering, authenticating, syncing, ready, disconnecting, offline, and
incompatible. Status reports the newest proven layer and never hides a more
severe binding/auth/version failure behind generic offline.

**Tests:** every transition and error mapping; reconnect/event gap; editor
replacement; cache rebuild; transaction in-doubt; status during shutdown;
bounded backoff; repeated polling; safe stderr; doctor/MCP parity; and no
absolute path or secret in model-facing remediation.

**Done when:** users and Codex receive the same diagnosis class and next action
from status and doctor without exposing machine-private details.

### S11-07 — Implement honest offline/static-cache mode

**Depends on:** S11-06.

**Changes:** startup without Bridge loads only a complete compatible project-
bound static generation, verifies manifest/schema/project identity and current
saved-source hashes, then reports `offline_cached`. If hashes changed, rebuild
from saved files before calling facts current. If validation/rebuild cannot
finish, retain only an explicitly stale generation or return unavailable.

The exact offline-eligible tool allowlist is
`godot_find_resource_owners`, `godot_find_usages`,
`godot_get_connection_status`, `godot_get_resource_dependencies`,
`godot_get_scene_graph`, `godot_inspect_node`, `godot_inspect_symbol`, and
`godot_search_symbols`. Project and scene summary resources may use the same
verified static authority; connection status is always readable. Eligible
saved-index results include project/generation/index/source revisions,
freshness, editor/runtime unavailability, omitted domains, and cache age.

Offline-forbidden surfaces are live selection/Inspector/open tabs/history/
viewport, editor-only diagnostics, all runtime observations/controls/capture,
all prepare/apply/status/Undo/confirmation-policy/validation transaction
operations, and any request requiring a current editor session. They return
`editor_offline` or `runtime_unavailable`, never an empty success. A central
router guard enforces this partition before request deserialization for all
thirty-three forbidden tools. Authentication, compatibility, synchronization,
discovery, and configuration states retain their precise public status and
diagnostic code instead of being collapsed into generic offline.

On reconnect, the sidecar authenticates the exact project/editor, completes a
full live snapshot, atomically activates overlays, and only then reports ready.
Previous editor/runtime sessions remain excluded or explicitly stale.

**Tests:** clean offline start; actual missing/corrupt/source-stale authority;
incompatible cache; changed saved file; deleted/renamed resource; interrupted
rebuild; exact static query matrix; every forbidden tool with empty input over
a real MCP transport in offline-cached, offline-empty, authentication,
compatibility, connecting, syncing, discovery-stale, and misconfigured states;
no stale overlay; reconnect/full snapshot; editor crash; two roots; index
limits/SLO; and no project mutation.

The canonical automated gate includes
`crates/godot-codex-mcp/tests/offline_subprocess.rs`. On macOS arm64 it runs
the actual compiled sidecar from a validator-owned installed-package layout,
uses the production package/config startup checks and stdio MCP transport,
and opens a real project-bound segment store plus offline-authority manifest.
It proves verified saved queries/status and fail-closed source-stale and
corrupt-store behavior with Bridge unavailable. This deterministic gate
does not claim the later live reconnect/full-snapshot acquisition, which
still requires the exact Godot Bridge prerequisite.

**Done when:** closing Godot gives useful saved-project answers and a clear
offline status while making live/runtime/write unavailability impossible to
misinterpret.

### S11-08 — Prove concurrent multi-project isolation

**Depends on:** S11-05 through S11-07.

**Changes:** two canonical fixtures and two independent editor/sidecar/Codex
task bindings active concurrently. Each process owns its root-local discovery,
token, cache, journals, status, and config. Add process-instance labels only
inside the test harness; production identity remains project/session based.

No server enumerates sibling roots or globally scans for bridge discovery.
Same server name `godot_editor` is allowed because each Codex task loads the
project-scoped config for its exact trusted root. If two editors attempt the
same root, the existing lock/conflict semantics remain authoritative and
doctor reports the ambiguity.

**Tests:** simultaneous reads, runtime sessions, prepared writes, approvals,
validation reports, and Undo in A/B; identical filenames/entities; copied
discovery/token; swapped config/root/cwd; one editor crash/restart; one cache
rebuild; simultaneous package/version mismatch; process cleanup; and negative
cross-project selectors/IDs/cursors/transaction IDs. The closed, bounded live
report requires all eleven ordered isolation cases; each case carries explicit
`no_fallback`, `no_cross_project_data`, `no_cross_project_mutation`,
`no_cross_project_approval`, `no_cross_project_transaction`, and `cleanup`
proofs rather than relying on one aggregate pass flag. Every case also names
its proof source. Discovery/token/restart/cache cases come from the two live
project processes. Config/root/cwd and package/version cases combine the
public runner's prelaunch authority gate with the separately Git-bound real
App/CLI/IDE operator attestations; a local helper alone cannot qualify those
host-owned coordinates.

**Done when:** failure or activity in one project changes no state, result,
status, revision, evidence, approval, or recovery record in the other.

### S11-09 — Publish the guidance and troubleshooting package

**Depends on:** S11-02 and S11-05 through S11-08.

**Changes:** update `docs/codex-integration/templates/AGENTS.godot.md` from its
obsolete read-only scope; add a distributable
`.agents/skills/godot-editor/SKILL.md`; add setup, quickstart, approvals/
sandbox, offline, multi-project, compatibility, troubleshooting, package
rollback, and known-limit documentation.

AGENTS guidance stays concise and durable:

- use connection status before assuming the editor/runtime is available;
- prefer semantic MCP facts/evidence over raw Godot serialization;
- preserve project/session/revision coordinates and uncertainty;
- use prepare/preview/apply/validation/Undo for supported writes;
- do not bypass the bridge with raw open-scene/source writes;
- respect Codex sandbox/host approval in addition to semantic confirmation.

The skill uses progressive disclosure and provides exact workflows for
orientation, saved/live/runtime questions, diagnostics, compound change,
validation failure, Undo, offline work, and doctor remediation. It calls only
the common MCP contract and never assumes App-, CLI-, or IDE-only behavior.

**Tests/review:** skill frontmatter/links; AGENTS scope; stale tool/count/
protocol/version scan; command copy/paste tests; negative unsafe guidance;
approval/sandbox explanation review; docs from packaged paths; redaction;
fresh-user comprehension rubric.

**Done when:** package, setup output, AGENTS, skill, MCP instructions, doctor,
and troubleshooting use one vocabulary and no document still describes the
Sprint 2/9 read-only or 36-tool product.

### S11-10 — Build the reproducible external beta package

**Depends on:** S11-03 through S11-09.

**Changes:** reproducible macOS arm64 archive containing:

- `godot-codex` and `godot-codex-mcp` release binaries;
- package manifest, compatibility matrix, registry/schema hashes, SHA-256
  checksums, Godot license, the committed deterministic third-party Rust
  license bundle, and source commit; the bundle is regenerated and checked
  from the frozen macOS arm64 production dependency closure before packaging,
  and contains no machine-local source paths;
- a prerequisite manifest for the exact separately distributed macOS arm64
  `Godot.app` Bridge build, including build/commit, artifact SHA-256, expected
  install path, and verification command; the archive does not duplicate that
  Godot artifact;
- config templates for read-only/full-beta;
- AGENTS fragment, repo skill, quickstart, and troubleshooting;
- an owner-only local install/rollback script.

Install root is versioned under the user's local data directory; visible
commands use managed links in a user bin directory. Install verifies target,
archive manifest, hashes, existing ownership, free space, and permissions
before replacement. It does not use `sudo`, edit shell profiles, trust a
project, write project config, open a network listener, or download code.
Previous managed version remains available for bounded rollback.

The beta artifact is explicitly not a notarized three-platform release.
Unsigned/ad-hoc local status, supported OS/architecture, and Gatekeeper
remediation are documented without encouraging security bypass.

**Tests:** two reproducible builds; manifest/hash mismatch; wrong architecture;
partial install; permission/symlink/path swap; existing foreign binary/link;
upgrade/downgrade/rollback; PATH missing; package extraction traversal;
paths containing spaces and Unicode; exact Godot prerequisite present/missing/
wrong hash; licenses; clean process/temp files; installed doctor/setup/MCP
smoke. License tests run the generator in `--check` mode and reject a stale or
tampered committed bundle and a package whose license digest/content record
differs.

**Done when:** a clean macOS user account can install and verify the package
without Rust, source checkout, admin privileges, or manual binary copying.

### S11-11 — Build the surface trace recorder and parity comparator

**Depends on:** S11-02, S11-03, and S11-10.

**Changes:** test-only stdio recorder shim and canonical trace schema. The shim
passes MCP bytes unchanged, records bounded protocol metadata and canonical
tool/resource/elicitation messages, redacts secrets/absolute roots/source
content, and binds each trace to package, project fixture, surface, host
version, registry hash, and revision timeline. Evidence binds the exact
recorder journal path and full-file SHA-256; the trace repeats that digest and
binds an ordered event hash-chain and event count. The validator recomputes all
three and rejects a trace whose journal surface/host/package bindings differ.
The recorder labels this artifact `surface_transport_capture`: stdio bytes do
not by themselves prove that the claimed App/CLI/IDE process owned stdin.

A qualifying surface additionally requires one canonical
`authority.json` under
`tests/codex/acquisition/sprint11/surfaces/<app|cli|ide>/`. The closed
`s11-surface-acquisition-authority/1.0` document is a separate external
operator attestation binding the exact trace/journal bytes, host-provenance
measurement, package, fixture, prompt pack, host/client artifacts, and MCP
binary. It attests the observed official host session, recorder stdio binding,
root/nested-cwd behavior, config reload, package launcher, sandbox approval,
all four form outcomes, foreign config/root/cwd rejection, package
digest/version rejection before launch, and absence of cross-project
fallback. Git proves the reviewed bytes but not human observation or process
ancestry; that limitation is recorded as an explicit trust boundary and
never described as cryptographic proof.

Comparator asserts semantic parity after one deterministic alpha-renaming map
per isolated run:

- connection/offline status and remediation;
- saved/live/runtime entities and evidence;
- cursors, freshness, conflicts, and revision coordinates;
- transaction preview/digest/risk/scope;
- host form action class without storing approval content;
- validation report and affected entities;
- structured errors and retry/remediation.

Persistent project/resource/scene/script identities, normalized facts, source
mapping, diagnostic codes, and bounded policies compare exactly.
Editor/runtime sessions, snapshots, transactions, reports, cursors, and
ephemeral entity IDs compare through a required bijection. Revisions compare
relationally: monotonicity, ordering, guard binding, and the creation of a new
session on restart. Each preview digest must match its own local canonical
preview and coordinates; raw digests from isolated sessions need not be equal.
Model workflows compare required semantic assertions, not an identical count
or order of tool calls.

The comparator ignores host/model prose, UI layout, thread/request IDs,
timestamps, latency within SLO, and ordering explicitly declared
non-semantic.

Each of the eighteen assertion IDs has a closed, required projection shape;
`{}` is never semantic evidence.
The exact set is `approval.accept`, `approval.cancel`, `approval.decline`,
`approval.timeout`, `connection.status`, `host.approval_layers`,
`host.config_reload`, `host.launcher`, `host.offline_status`,
`host.unsupported_form`, `multi_project.reject`, `offline.saved_query`,
`runtime.error_stack`, `saved.current_scene`, `transaction.apply`,
`transaction.preview`, `transaction.undo`, and `validation.result`.

**Tests:** golden App/CLI/IDE traces; reordered JSON keys; different request
IDs/timestamps; real semantic divergence; missing call; changed entity ID,
revision, evidence, approval action, or error; redaction leakage; truncation;
recorder crash and pass-through integrity; missing/swapped/tampered journal;
event reorder and stale journal digest; missing/recomputed/cross-surface
authority; false host/root/cwd/launcher/form observations.

**Done when:** one machine-readable comparator detects semantic divergence
without requiring screenshots or treating prose as the API contract.

### S11-12 — Run task-based usability qualification

**Depends on:** S11-09 through S11-11.

**Changes:** frozen participant script and rubric with no developer coaching.
At least three clean-start runs include one participant not involved in the
implementation. Tasks:

1. install the package and connect a trusted fixture project;
2. identify the current scene and explain one fact with evidence;
3. diagnose a deliberately closed editor and use offline context;
4. distinguish missing binary, stale discovery, version, auth, and config
   faults with doctor;
5. run the scene and find an intentional runtime error/stack;
6. preview, approve, apply, validate, and Undo a compound change;
7. open a second project and prove project isolation.

Record task success, time, wrong turns, help usage, remediation success,
approval comprehension, sandbox comprehension, and unresolved defects.
Targets: first useful status within 5 minutes, connected live query within
15 minutes, zero secret copying, zero manual TOML requirement, zero project
isolation/integrity failure, and successful recovery from every injected
doctor fault.

Human acquisition runs outside the automated 180-second validator. It uses a
frozen package and task pack, documented consent, pseudonymous participant
IDs, and a defect/retest policy. The validator later checks the bound reports,
rubric completeness, and every participant's exact trace, rubric, consent
receipt, defect ledger, and external-operator authority artifact from the
source commit. All five files live under
`tests/codex/acquisition/sprint11/human/s11u-run-<32-lowercase-hex>/` with
fixed filenames; source templates and arbitrary repository paths are
ineligible.

Implementation independence is not a cryptographic claim. Git and SHA-256
bind the reviewed bytes, while a study operator externally attests that a real
person participated, consent was observed before tasks, and whether that
person was implementation-independent. The operator observation is the
explicit trust boundary: neither a self-declared report boolean nor a
relabelled/rehashed synthetic bundle qualifies.

**Tests/review:** identical clean fixture/package; randomized fault order;
redacted observation notes; machine trace bound to each semantic assertion;
no participant accounts/tokens/prompts in evidence; defect severity rubric;
repeat after any high/critical usability fix.

**Done when:** documentation and tools, not developer intervention, are
sufficient for the required daily workflow.

### S11-13 — Qualify real App, CLI, and IDE workflows

**Depends on:** S11-11 and S11-12.

**Changes:** run the same canonical task pack from:

- the Codex desktop app local project;
- Codex CLI from the exact project root;
- the official Codex IDE extension on the frozen VS Code Stable version.

Each surface uses the packaged binaries and project config, connects to the
same fixture state in isolated runs, emits a redacted recorder trace, and
executes status, saved/live query, runtime error/stack, compound preview/form
approval/apply/validation/Undo, offline query, and multi-project negative
scenarios.

The write scenario is blocked unless the surface proves standard form
elicitation. Decline/cancel/timeout are exercised. Host sandbox approval and
semantic transaction confirmation are recorded as distinct control layers.
The comparator applies §3.8 alpha-renaming and relational revision rules; UI,
prose, and isolated-run opaque values may differ.

**Tests:** exact host versions; registry/instructions visibility; config reload/
restart behavior; task cwd/root; form capability; read/runtime/write traces;
offline status; two projects; surface restart; recorder integrity; parity
comparison. The short `previous_sprint_contracts` gate runs unit and protocol
regressions; the real `previous_sprint_regressions` gate is satisfied only by
the separately acquired, package-bound Sprint 6–10 receipt described in
S11-14.

**Done when:** the main read/write/runtime workflow passes on all three
required surfaces and every semantic assertion named by the roadmap is equal
after the normative normalization rules.

### S11-14 — Qualify macOS arm64 and close External Codex Beta

**Depends on:** all implementation, docs, package, usability, and surface gates
complete.

**Changes:** no implementation changes. From the clean package source, first
run the source-bound package-live Sprint 6–10 acquisition. Commit its detached
manifest, nine bounded live reports, closed receipt, required surface traces/
journals, and the usability report plus each participant's four primary human
artifacts and separate external-operator authority as acquisition artifacts.
Then run the source/package-bound acceptance wrapper and add only
`tests/codex/evidence/sprint-11-external-codex-beta-macos.json` in the final
evidence-only commit.

Immediately after freezing that clean package coordinate, acquire installed
host provenance with the public wrapper and eight explicit installed paths:

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

The repository-local output directory is new and contains two authorities
with different roles. `receipt.json` is the acquisition envelope binding the
package-source commit, runner, both schemas, coordinate profile, command, and
output digest. `measurement.json` is the bounded, content-free measurement of
the installed App/CLI/IDE artifacts and verified signatures. Evidence binds
both paths and digests; recorder journals and surface traces bind the
measurement digest, never the envelope digest.

**Tests:** contracts/schema/registry; Rust fmt/full locked tests/Clippy;
Bridge RPC 1.0–1.8 regressions; release builds and reproducibility; package
install/upgrade/rollback; setup/config ownership; doctor fault matrix; status/
offline/multi-project; App/CLI/IDE trace parity; usability report; approval/
sandbox/trust review; permissions/redaction/cleanup; Sprint 6–10 regressions;
artifact/source hashes.

**Done when:** immutable evidence validates against a clean source commit and
the exact package, all six Sprint 11 roadmap criteria pass, required external
surfaces are `passed`, and the release projection explicitly records
`external_codex_beta_macos = passed`, `R1-08 = partial`, `R1-10 = partial`,
and formal Beta Gate `not_reached`. Windows/Linux/remote CI/notarization/Dock
remain explicit `not_run` rather than implied beta support.

## 3. External beta contract

### 3.1. Process and project topology

```text
Codex task rooted at Project A
  └─ project .codex/config.toml
       └─ godot-codex-mcp --project-root .
            ├─ Project A static index/journals
            └─ authenticated Bridge for Project A editor only

Codex task rooted at Project B
  └─ independent config, process, state, token, and Bridge binding
```

No discovery registry exists above a project root. The sidecar canonicalizes
its one configured root before opening index or discovery state. A selector,
cursor, entity, transaction, report, runtime session, or editor session from
another project fails binding checks and cannot cause fallback discovery.

The package and acquisition runners apply bounded lifecycle tracking to the
fixed, source-bound command graph used by the gates. Every package-owned layer
that constructs a replacement environment must preserve the unrecorded
process-scope markers; nested bounded runners retain outer markers and add
their own. Process groups, marker inventory, and stable Darwin process
identities fail the gate when an observed descendant survives timeout or
normal completion.

This lifecycle gate is not a security sandbox for arbitrary executables. It
does not claim kernel-enforced containment of adversarial code that
deliberately clears the marker and races an `exec`/`fork`/`exit` sequence
between observations. Such a process is outside the frozen qualification
graph. Adding a privileged or entitled macOS process supervisor belongs to
the deferred release-grade security hardening scope. Acceptance item 15's
process-cleanup proof applies to the reviewed package-owned graph and cannot
be generalized into an unprivileged sandbox claim.

### 3.2. MCP v1 and connection state

The final public MCP profile is:

| Item | Sprint 11 value |
|---|---|
| Server | `godot-codex-mcp` beta package version |
| Transport | Local stdio |
| MCP protocol | `2025-11-25` |
| Approval compatibility floor | `2025-06-18` with `elicitation.form` |
| Tools | Exactly 41 |
| Fixed resources | Project, editor, runtime, and connection summaries |
| Resource templates | Scene summary only |
| Bridge RPC | Current 1.8, compatible reduced lower minors |
| Model/API credentials | None |

`godot_get_connection_status` is sidecar-authoritative for process,
compatibility, cache, and remediation state. It is not evidence that an editor
fact is current. Live/runtime facts still require their authoritative session
and revision envelopes.

### 3.3. Setup authority and project trust

Setup permission is separate from Codex project trust:

- setup may write only after the user accepts its exact plan;
- Codex still ignores project config until the host considers the project
  trusted;
- setup cannot mark a project trusted or manipulate host trust state;
- a host restart/reload may be required after config or skill changes;
- doctor reports symptoms and documented remediation without claiming private
  host state it cannot observe.

Generated config uses only public project-supported keys. It does not redirect
model providers, authentication, telemetry, notifications, or host metadata.
Those remain user/admin configuration outside this integration.

### 3.4. Doctor result contract

One check result contains:

```json
{
  "check_id": "discovery.freshness",
  "component": "bridge_discovery",
  "status": "fail",
  "code": "bridge_discovery_stale",
  "summary": "Godot discovery belongs to a process that is no longer reachable.",
  "observed_version": null,
  "supported": null,
  "remediation_id": "start_matching_editor",
  "retryable": true
}
```

Summaries are bounded and safe. JSON never includes session token, HMAC proof,
endpoint, raw PID, source content, approval data, or absolute root by default.
Human output may show an explicitly requested safe local path with
`--show-paths`; recorded evidence always uses the redacted form.

### 3.5. Offline authority matrix

| Domain | Offline behavior |
|---|---|
| Connection/project status | Available |
| Saved resource dependencies/owners | Available after source-hash validation |
| Saved scene graph/node semantics | Available after source-hash validation |
| Saved script symbols/usages/diagnostics | Available after source-hash validation |
| Live scenes/selection/Inspector/history/viewport | Unavailable |
| Runtime tree/object/stack/capture/control | Unavailable |
| Prepare/apply/status/validation/Undo/policies | Unavailable |
| Previous live/runtime overlays | Excluded or explicitly stale, never current |

Offline results say which domains are absent and bind the static generation.
They never imply that the editor has no dirty changes; they say the editor
state is unknown/unavailable.

### 3.6. Compatibility and capability outcome

Compatibility is evaluated from the package matrix before semantic work:

1. platform and architecture;
2. package manifest integrity;
3. project/index schema;
4. Bridge major/minor and capability intersection;
5. MCP host protocol and form interaction;
6. surface/version qualification.

`compatible_reduced` may enable honest read-only or earlier-sprint capability
sets. It cannot enable a tool whose Bridge, host interaction, index schema, or
transaction contract is missing. `not_tested` is never promoted to supported
by doctor.

### 3.7. Guidance precedence

- MCP server instructions describe cross-tool protocol workflow.
- The repo skill describes reusable Godot semantic workflows and loads only
  when relevant.
- `AGENTS.md` describes durable project conventions, commands, and safety.
- The current task prompt supplies one-off intent.
- Codex sandbox and approval policy remain host controls.
- Semantic preview/form confirmation remains transaction authority.

No guidance layer may widen a tool schema, alter project binding, declare stale
facts current, or bypass approval.

### 3.8. Surface parity

Exact-equality fields:

- project ID and persistent resource/scene/script identities;
- normalized facts/source mapping, confidence, freshness, evidence, conflicts,
  and partial reasons;
- risk, scope, affected persistent entities/files, and limits;
- approval action class and resulting semantic transaction state;
- validation/rollback outcome and structured diagnostics;
- error code, retryability, compatibility, and remediation ID.

Bijective alpha-renaming fields:

- editor/runtime sessions, snapshots, transactions, reports, cursors, and
  session-scoped node/object/stack identifiers;
- any evidence identity derived solely from one renamed ephemeral coordinate.

Relational fields:

- revisions and event sequences are monotonic and preserve causal order;
- restart creates a fresh session and invalidates prior guards;
- a preview digest verifies against its own canonical preview and local
  coordinates; raw digests do not compare across isolated runs;
- generation transitions and approval/validation ordering obey the same state
  machine.

Allowed differences:

- model prose, explanation ordering, and UI labels;
- host request/thread IDs and timestamps;
- visual presentation and confirmation layout;
- alpha-renamed opaque values and valid isolated-run revision magnitudes;
- model-selected call count/order when all required assertions are observed;
- latency inside the same accepted SLO/status path.

### 3.9. Initial operational limits and SLOs

| Limit/SLO | Initial target |
|---|---:|
| Doctor total without editor | 3 seconds |
| Doctor total with Bridge probe | 10 seconds |
| Connection-status result | 32 KiB |
| Server instructions | 4096 bytes; first 512 self-contained |
| Tool title / description | 128 / 1024 UTF-8 characters |
| Setup plan/diff | 256 KiB |
| Setup plan lifetime | 10 minutes |
| Diagnostic checks | 64 |
| Compatibility matrix | 256 KiB / 256 rows |
| Offline cache activation | p95 ≤ 2 seconds when hashes unchanged |
| Offline static query | Existing query SLO, no live fallback |
| Reconnect to ready | p95 ≤ 5 seconds on reference fixture |
| Surface semantic parity | Zero differences in required fields |
| First useful status in usability run | ≤ 5 minutes |
| First connected live query | ≤ 15 minutes |

Existing MCP, Bridge, index, runtime, transaction, validation, report, and
screenshot limits remain authoritative and cannot be raised by config,
package, setup, doctor, skill, AGENTS, or host surface.

### 3.10. Structured operational errors

MCP-001 freezes at least:

- `package_invalid`;
- `binary_missing`;
- `project_config_missing`;
- `project_config_invalid`;
- `project_config_not_effective`;
- `project_untrusted_or_restart_required`;
- `editor_offline`;
- `static_cache_unavailable`;
- `static_cache_stale`;
- `static_cache_incompatible`;
- `bridge_discovery_stale`;
- `bridge_version_incompatible`;
- `bridge_authentication_failed`;
- `project_binding_mismatch`;
- `surface_unsupported`;
- `surface_restart_required`;
- `host_interaction_unsupported`;
- `capability_unavailable`;
- every existing domain-specific Sprint 2–10 error.

Errors include stable code, safe message, retryability, status, compatibility,
and remediation ID. They never echo private paths, tokens, endpoints, raw host
configuration, source text, prompts, approval material, or another project.

## 4. Commit boundaries

The packaged launcher, non-empty project-config reload, and real App/CLI/IDE
form probes require binaries built from a clean, reviewable source commit.
Consequently one non-qualifying bootstrap series, `C11-B`, is allowed after
the fail-closed contracts, candidate matrix, evidence schemas, acquisition
validators, and local regressions are frozen. This breaks the otherwise
circular requirement to prove a package before any source commit exists.
`C11-B` cannot promote a host coordinate from `candidate`, close S11-01,
enable a write claim, or produce final evidence. If a packaged probe finds a
source, config, doc, fixture, recorder, oracle, rubric, source-scope, runner,
or validator defect, all acquired output is discarded, the fix is committed
to its semantic boundary, and the package and external gates restart.

| Boundary | Allowed content | Required gate before commit |
|---|---|---|
| `C11-B non-qualifying bootstrap` | Frozen Sprint 11 contracts and the production/test/package sources required to build the first clean candidate | closed schemas and acquisition validators; fail-closed candidate matrix; local contracts, Rust workspace, Sprint 6–10 regressions, and no support promotion |
| `C11-0 host qualification freeze` | S11-01 host/manual probes and frozen Godot/host/release-scope decisions; no final evidence | real App/CLI/official-IDE form/root/launcher/config-reload feasibility against a `C11-B` or later clean package |
| `C11-1 MCP profile` | S11-02 final MCP docs, registry schemas/descriptions/instructions, status tool | exact 41-tool/four-fixed/one-template contract + Sprint 6–10 regressions |
| `C11-2 compatibility/doctor` | S11-03/04 matrix, schema, operations CLI, diagnostic engine | doctor golden/fault matrix + package/version negatives |
| `C11-3 setup/guidance` | S11-05/09 TOML merge, config templates, AGENTS, skill, docs | consent/digest/ownership tests + docs copy/paste gate |
| `C11-4 status/offline` | S11-06/07 health state, offline static mode, reconnect | offline domain matrix + stale/cache/reconnect faults |
| `C11-5 multi-project/package` | S11-08/10 isolation harness, installer/package/rollback | two-project live gate + reproducible install smoke |
| `C11-6 parity/usability` | S11-11/12 recorder, comparator, rubric, non-final reports | trace oracle + clean-start usability acceptance |
| `C11-7 external surfaces` | S11-13 App/CLI/IDE traces and source-bound acceptance runner | all three surface traces pass comparator |
| `C11-8 evidence` | S11-14 final evidence JSON only | `sprint11_acceptance.py --validate` |

Sprint 11 carries a narrow hosted-CI waiver only for the local macOS
App/CLI/VS Code and human-observation gates that cannot execute on the current
remote runners. Compensating controls are a clean source coordinate, locked
full Rust/Python regressions, two reproducible package builds, exact installed
host provenance, package-bound live reports, and reviewed external
attestations. The waiver does not apply to remotely runnable tests and does
not satisfy the formal multi-platform Beta Gate.

If a qualifying run requires a source, config, doc, package, fixture, recorder,
oracle, rubric, source-scope, runner, or validator fix, its evidence is
discarded. The fix enters the correct boundary, is committed, and the complete
gate restarts from a clean coordinate and rebuilt package.

## 5. External Codex Beta acceptance

Sprint 11 passes only when independent evidence proves:

1. the final MCP profile contains exactly 41 tools, four fixed resources, and
   one resource template with wire-published closed top-level schemas,
   finite-bounded nested values, useful descriptions, correct annotations, and
   bounded instructions;
2. setup performs no write before exact consent and non-interactive acceptance
   cannot replay against a changed plan/project/package/config;
3. setup preserves unrelated project config/guidance and removal cannot delete
   content it no longer owns;
4. generated config is accepted by the frozen App/CLI/IDE host versions and
   loads only for the exact trusted project task;
5. doctor distinguishes missing binary, stale discovery, version mismatch,
   authentication/binding, config/trust/restart, and cache issues;
6. connection status is available without Bridge and matches doctor diagnosis;
7. closing Godot produces `offline_cached` or an honest unavailable/stale
   state, never an empty project or current live/runtime projection;
8. eligible saved resource/scene/script/usages queries work from a verified
   offline generation with exact project/generation/freshness evidence;
9. every live/runtime/write/validation/Undo surface fails closed offline;
10. reconnect requires exact project authentication and a full live activation
    before current editor/runtime facts return;
11. two open projects keep discovery, tokens, processes, caches, revisions,
    entities, cursors, runtime sessions, transactions, reports, approvals, and
    faults isolated;
12. copied/swapped config/discovery/IDs cannot redirect a task to the other
    project or trigger automatic fallback;
13. the compatibility matrix drives package, setup, doctor, MCP readiness, and
    evidence with no divergent hard-coded version rules;
14. the beta archive is reproducible, hash-bound, path-safe, user-local, and
    installable without Rust/admin/network/project-config side effects;
15. AGENTS, skill, MCP instructions, setup, doctor, and troubleshooting agree
    on project binding, offline limits, evidence, approvals, sandbox, and Undo;
16. the desktop app, CLI, and official IDE extension expose the same registry,
    server instructions, connection state, and semantic results;
17. the canonical read scenario returns equal persistent identities, evidence,
    confidence, freshness, and facts across all three surfaces, while
    ephemeral identities/revisions satisfy §3.8 renaming/relational rules;
18. the runtime scenario creates a new session and returns the same bounded
    error/stack/source mapping across all three surfaces;
19. the write scenario shows semantically equal previews/risk/scope, each local
    digest verifies its own preview, requires real form confirmation, returns
    an alpha-renamed equivalent validation report, and fully Undoes;
20. decline/cancel/timeout and a host without interaction fail identically and
    never accept an approval boolean/prose/config substitute;
21. source-bound external-operator attestations and the corresponding exact
    primary bundles show task-based users install, connect, diagnose
    offline/faults, perform the daily workflow, and isolate two projects
    without developer intervention; Git proves artifact provenance, not the
    operator's human-observation claims;
22. recorder/evidence/log/package/config/cache scans contain no tokens, proofs,
    absolute private roots, unrestricted source/property values, participant
    free-form prompts, account identity, native handles, or cross-project
    data; evidence binds the version-controlled canonical prompt pack by path,
    ID, and SHA-256;
23. all Sprint 6–10 semantic/runtime/transaction/validation regressions remain
    green from the packaged binaries;
24. Windows, Linux, remote CI, Cursor, signing/notarization, and embedded Dock
    are explicitly `not_run` or deferred, not implied by External Codex Beta.

The canonical automated preflight runs every Python/locked-Cargo gate command
and reports observations computed by that invocation:

```sh
.venv/bin/python tests/codex/sprint11_acceptance.py --timeout 180
```

It is explicitly non-qualifying because it does not synthesize App/IDE or
human evidence and does not launch the long package-live regression matrix.
The package-live matrix is acquired separately from the exact detached
package and Godot prerequisite:

```sh
.venv/bin/python tests/codex/sprint11_packaged_regressions.py \
  --artifact-root '<detached-package-root>' \
  --package-manifest '<detached-package-manifest>' \
  --godot '<exact-arm64-Godot-prerequisite>' \
  --output-root tests/codex/acquisition/sprint11/package-live \
  --timeout 180
```

It runs Sprint 6, Sprint 7, Sprint 8, two Sprint 9 operation shards, one
fast-negative shard, one approval-timeout shard, two Sprint 9 fault shards,
and Sprint 10. Every
command uses the packaged `bin/godot-codex-mcp`, binds its report to the
package sidecar and Godot hashes, and opts into the additive 41-tool Sprint 11
registry. Default Sprint 6–10 runner profiles preserve their original exact
registry assertions. No Sprint 9 child may exceed 180 seconds.

The acquisition publishes its ten reports and
`s11-packaged-regression-receipt/1.0` receipt atomically. The final validator
loads those committed artifacts from the evidence source commit, recomputes
their hashes, checks exact runner/fixture/package bindings and aggregate
coverage, and rejects a status-only substitute.

The concurrent project gate is acquired independently from the same detached
package coordinate:

```sh
.venv/bin/python tests/codex/sprint11_external_acquisitions.py multi-project \
  --artifact-root '<detached-package-root>' \
  --package-manifest '<detached-package-manifest>' \
  --godot '<exact-arm64-Godot-prerequisite>' \
  --output-root tests/codex/acquisition/sprint11/multi-project \
  --timeout 180
```

Its public child command binds the expected package version and packaged
sidecar SHA-256 before either fixture starts. The bounded report is validated
against `sprint11-multi-project-report.schema.json` and contains the exact
ordered cases `copied_discovery`, `copied_token`, `swapped_discovery`,
`swapped_token`, `swapped_config`, `swapped_root`, `swapped_cwd`,
`editor_restart`, `cache_rebuild`, `package_mismatch`, and
`version_mismatch`. Every case independently proves no fallback, no
cross-project data/mutation/approval/transaction leakage, and cleanup. The
source-bound acquisition receipt additionally binds that complete matrix,
the package version, report, runner, fixtures, and final process cleanup.

After real package-live reports, surface traces, their exact recorder
journals, and human reports have been acquired separately, qualifying
validation is:

```sh
.venv/bin/python tests/codex/sprint11_acceptance.py \
  --validate tests/codex/evidence/sprint-11-external-codex-beta-macos.json \
  --artifact-root '<detached-package-root>' \
  --timeout 180
```

The wrapper refuses a dirty Sprint 11 source scope, non-matching package, or
missing/freshly generated human or GUI acquisition artifact. The candidate
evidence must be either the sole untracked file over its source commit or the
sole change in an evidence-only child commit. It runs automated gates and
loads every human trace, rubric, consent receipt, defect ledger, and authority
as exact Git bytes before accepting the aggregate usability projection.
validates the pre-acquired, source-bound reports. Evidence stores only the
canonical runner and gate definition digest; it cannot declare its own passing
status or duration.

1. final MCP schema/registry/resource/instruction/description contracts;
2. Rust formatting, full locked workspace tests, and deny-warning Clippy;
3. Bridge RPC 1.0–1.8 contract regressions and independently validated,
   source/package-bound Sprint 6–10 live reports;
4. compatibility matrix/schema/rendered-doc consistency;
5. operations CLI and complete doctor fault matrix;
6. setup consent/digest/TOML ownership/config-parser profiles;
7. connection state, offline cache, reconnect, and forbidden-tool matrix;
8. concurrent two-editor/two-sidecar/two-project isolation;
9. reproducible release builds, package, install, upgrade, and rollback;
10. guidance links/copy-paste/stale-contract/security scans;
11. recorder/comparator golden and leakage tests;
12. task-based usability report validation;
13. real desktop App, CLI, and IDE read/runtime/write/offline traces;
14. canonical semantic parity comparison;
15. process/temp/permissions/listener/redaction and source-tree cleanup;
16. source/package/artifact SHA-256 binding and atomic evidence publication.

After all implementation/package/docs/traces are committed, the final evidence
is generated from a clean coordinate and committed alone. The validator
accepts the source commit before that evidence-only commit and its exact parent
afterward only when the child changes the single evidence path.

## 6. Explicitly deferred

Embedded Godot Dock/app-server client; ChatGPT web/local-config parity; remote
or HTTP MCP; plugin/marketplace distribution; global multi-project router;
cross-project queries or transactions; background daemon; automatic project
trust; automatic shell-profile edits; system/admin install; package download;
auto-update; final uninstall lifecycle; signed/notarized artifacts; Windows/
Linux package qualification; hosted CI; enterprise managed configuration;
third-party MCP clients; Cursor/other editor support; permanent approval;
offline live/runtime/write behavior; and release-grade telemetry/performance/
security hardening are outside Sprint 11.

Sprint 12 consumes the same final MCP v1 and project-scoped setup to build the
embedded Codex client shell through app-server. It must not introduce a second
Godot semantic client, index, transaction path, or approval model.
