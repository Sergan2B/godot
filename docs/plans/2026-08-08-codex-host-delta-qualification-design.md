# Codex Host Delta Qualification Design

Date: 2026-08-08

Status: approved design

Scope: Sprint 11 technical/private-alpha qualification on macOS arm64

## Problem

Codex App, its bundled CLI, the standalone CLI, and the official IDE extension
are rolling host surfaces. Their versions, code signatures, and artifact hashes
can change independently of Godot Codex. The current Sprint 11 acquisition
workflow binds real App/CLI/IDE evidence to those exact host coordinates, but
does not provide a short qualification path when only the host coordinates
change. In practice, an ordinary Codex update has repeatedly caused the
operator to repeat package installation, setup, Trust handling, fault recovery,
surface capture, and the full App/CLI/IDE task pack.

That invalidation boundary is too broad. A host update does not change the
frozen Godot Codex package, Godot build, Bridge protocol, schemas, tool registry,
transaction policy, or package regression evidence. Repeating those gates is
costly and does not add relevant confidence.

Package `0.1.19` already supports a detached, digest-bound
`godot-codex-surface-compatibility-bundle/1.0` which can replace only the exact
App/CLI/IDE surface coordinates. The missing component is a release-side delta
qualifier that decides which evidence is affected, executes only the necessary
host checks, and issues that existing bundle without rebuilding the package.

## Goals

- Make an ordinary Codex update a bounded, automatic host qualification.
- Keep package `0.1.19` and all host-independent evidence valid when its bytes
  and frozen product coordinates have not changed.
- Require no package reinstall, project setup/repair, Trust interaction, fault
  matrix, or manual accept/decline/cancel/timeout sequence for a host-only
  update whose interaction contract is unchanged.
- Detect contract-sensitive changes and route them to targeted surface checks.
- Preserve fail-closed, exact-coordinate, digest-bound compatibility decisions.
- Keep technical/private-alpha compatibility separate from deferred usability
  and commercial-beta claims.

## Non-goals

- Automatically trust a Codex project or suppress host security prompts.
- Allow arbitrary future Codex versions through a semver range.
- Teach the installed package to download or self-authorize compatibility data.
- Replace full package qualification after changes to Godot Codex, Godot,
  Bridge, schemas, registry, or transaction semantics.
- Claim that protocol compatibility proves UI usability or commercial readiness.

## Decision

Implement a capability-based host delta qualifier outside the frozen package.
The qualifier consumes verified installed host artifacts, the embedded package
matrix, the last accepted host profile, and deterministic probe results. It
produces an acquisition receipt plus the already-supported detached compatibility
bundle and host-coordinate profile.

Do not modify or rebuild the `0.1.19` package for this work. The release-side
qualifier, schemas, tests, and documentation are not installed package payload.
The installed package continues to verify and atomically apply the resulting
bundle through its existing `compatibility install` workflow.

Rejected alternatives:

1. **Manual exact allowlist update.** Safe, but still requires human work on
   every rolling release and preserves the current operational failure.
2. **Version-range compatibility.** Convenient, but an alpha host can change
   MCP, app-server, approval, or extension behavior without a useful semver
   boundary. This would turn exact support into an unsafe assumption.

## Qualification Layers

### Layer 1: frozen package qualification

This evidence belongs to the package coordinate and remains valid while all of
the following stay byte-for-byte or canonically unchanged:

- Godot Codex package and launcher;
- Godot prerequisite and Bridge profile;
- MCP protocol requirements;
- schemas and canonical tool registry;
- transaction and validation policy;
- package regressions, same-project ownership, multi-project isolation, and
  reproducibility evidence.

A change in this layer requires full package qualification. A Codex host update
alone never invalidates it.

### Layer 2: host compatibility qualification

This evidence belongs to exact App/CLI/IDE host coordinates. It measures:

- versions and target architecture;
- signed artifact identities and complete extension-tree hashes;
- executable provenance;
- negotiated MCP protocol;
- initialization and tool discovery;
- required host interaction capabilities;
- project-scoped status and bounded semantic reads.

This layer is replaceable by an independently released compatibility bundle.

### Layer 3: human usability qualification

Trust presentation, approval wording, discoverability, accessibility, and
operator comprehension are human usability properties. They remain deferred
for the solo-developer technical/private-alpha track. They are required before
an External Codex Beta or commercial usability claim, but they are not repeated
for routine development host updates.

## Delta Classifier

The qualifier compares the new measured host profile to the last accepted
profile and the embedded product matrix before running probes.

| Delta class | Examples | Required action |
|---|---|---|
| `coordinate_only` | version, signature identity, or artifact hash changed; declared capabilities unchanged | bounded automatic host smoke |
| `interaction_sensitive` | MCP negotiation, elicitation/form capability, approval action set, app-server protocol, or sandbox contract changed | targeted interaction probes for affected surfaces |
| `surface_structural` | App client path, CLI packaging, IDE extension layout, executable ownership, or host provenance model changed | targeted provenance plus affected surface workflow |
| `product_contract` | package, Godot, Bridge, schema, registry, or transaction policy changed | stop; full package qualification |
| `unclassifiable` | missing baseline, ambiguous artifact, unexpected fields, unsigned artifact, or probe disagreement | fail closed; no bundle |

Classification is based on measured facts and probe output, never only on a
version string.

## Automatic Host Smoke

The bounded smoke uses clean, disposable projects and isolated data roots. It
does not mutate a developer project and does not reuse an active project
session.

For each required surface it proves the relevant host client path:

- **App:** signed desktop bundle provenance, exact bundled client identity, and
  a headless app-server/client MCP session rather than GUI automation;
- **CLI:** exact standalone or App-bundled CLI process from the clean project
  root;
- **IDE:** signed IDE shell provenance, complete official extension tree, exact
  embedded client identity, and the extension/client MCP startup path.

Every surface smoke must prove:

1. MCP initialization completes within a fixed deadline.
2. The expected protocol is negotiated.
3. The canonical tool registry is complete and unchanged.
4. `godot_get_connection_status` binds the exact disposable project.
5. One bounded saved semantic fact is returned with eligible evidence.
6. Offline and session-busy states remain honestly classified.
7. The host terminates cleanly and leaves no project-session owner.

The interaction-sensitive path additionally uses a synthetic, non-mutating
elicitation server to prove the exact accept, decline, cancel, and timeout
protocol actions. It verifies protocol behavior, not UI wording or usability.
A disposable exact-Undo transaction may run only when a write-path capability
changed; it must restore the fixture and prove the final saved digest.

## Outputs and Authority

One successful run writes a private acquisition directory containing:

- `host-delta-measurement.json`;
- `host-delta-receipt.json`;
- `host-coordinate-profile.json`;
- `surface-compatibility-bundle.json`;
- raw SHA-256 files for every published input;
- redacted per-surface probe reports.

The receipt outcome is `compatible_observed` until the release command verifies
all required reports and issues the detached bundle. A successfully issued
bundle may mark the exact surface coordinates `supported` for the technical MCP
contract. In this context, `supported` does not imply that S11-12 usability or
External Codex Beta has passed.

The bundle remains bound to:

- exact package version and target;
- embedded baseline matrix digest;
- monotonically increasing sequence;
- exact detached host-profile digest;
- one complete, unique App/CLI/IDE rule set.

The installed package applies it with the existing two-phase preview/apply
command. Equal/lower sequence, source replacement, digest drift, partial surface
sets, symlinks, and stale preview plans fail closed.

## Operator Experience

The normal post-update workflow is one release-side command, for example:

```sh
python3 -E -s -S tests/codex/sprint11_host_delta_qualify.py \
  --package-manifest /path/to/sprint11-package-manifest.json \
  --installed-root /path/to/verified/install \
  --previous-profile /path/to/last/host-coordinate-profile.json \
  --output /path/to/private/acquisition
```

On success, a wrapper previews and applies the exact generated compatibility
bundle through the package-owned launcher. There are no model tool approvals,
semantic transaction forms, Trust prompts, or repeated CLI conversations in
the coordinate-only path.

The command prints one of three concise outcomes:

- `compatible_bundle_applied` — normal update complete;
- `targeted_surface_check_required` — a named contract-sensitive delta needs a
  bounded additional check;
- `full_package_qualification_required` — a host-only update cannot explain the
  product contract change.

## Failure Handling

- All probes have explicit process and synchronization deadlines.
- A failed or interrupted run never updates active compatibility state.
- Temporary projects and processes are identified by private run IDs and
  cleaned only after exact ownership validation.
- Existing valid compatibility remains active if the new candidate fails.
- No failure is represented as indefinite `syncing`.
- Diagnostic output identifies the delta class, affected surface, stable code,
  and next action without exposing account identity, tokens, project paths, or
  owner process information.
- A later retry may reuse immutable measurements only when their complete input
  digest still matches; live probe results are never relabelled.

## Evidence and Acceptance Changes

Sprint 11 validation must stop treating every host coordinate change as an
invalidation of all technical evidence. The final validator will evaluate
separate bindings:

- package-bound evidence references the unchanged package manifest digest;
- host-bound evidence references the active compatibility bundle and host
  profile digests;
- human usability evidence remains independently deferred/unacquired.

A later host delta may replace only host provenance and affected surface probe
receipts. It must not require new package-live, multi-project, same-project,
reproducibility, or prior-sprint receipts when their package binding is
unchanged.

The private-alpha technical result may pass with S11-12 deferred, but final
validators must continue to reject External Codex Beta and commercial claims
until the human gate is independently satisfied.

## Test Strategy

### Unit and schema tests

- deterministic delta classification for every class;
- strict schema validation and unknown-field rejection;
- exact binding between receipt, profile, bundle, matrix, and probe reports;
- sequence, replay, downgrade, digest, symlink, and source-replacement failures;
- no promotion from `compatible_observed` when a required surface is absent.

### Hermetic integration tests

- coordinate-only update reuses package-bound evidence;
- interaction capability change runs only interaction probes;
- registry or Bridge change requires full qualification;
- one failed surface prevents bundle issuance and leaves active state unchanged;
- interrupted run is recoverable or safely discardable by exact run identity;
- a successful bundle installs through the existing package-owned preview/apply
  workflow.

### Real local acquisition

- measure the current signed App, CLI, VS Code, and official extension;
- execute the bounded App/CLI/IDE smoke on fresh disposable projects;
- publish a redacted host-delta receipt and exact detached bundle;
- apply it to the verified `0.1.19` installation;
- prove `doctor` and a new task accept the exact current host coordinates;
- prove no package reinstall, setup, Trust interaction, or full surface capture
  occurred.

## Rollout

1. Add strict schemas and the model-free delta classifier.
2. Add per-surface bounded probes and hermetic fixtures.
3. Add bundle generation and bind it to the existing installer workflow.
4. Teach Sprint 11 acceptance to compose package-bound and host-bound evidence.
5. Run the qualifier once against the currently installed Codex hosts and
   package `0.1.19`.
6. Retire the armed manual App capture if it is still unclaimed; do not consume
   or relabel it.
7. Document the one-command post-update workflow.

This rollout intentionally avoids a new Godot Codex package build. If
implementation later discovers that the installed compatibility-bundle
consumer itself must change, that is a `product_contract` delta and must be
reported before any package version is advanced.

## Success Criteria

- A coordinate-only Codex update is qualified and applied without operator
  interaction beyond starting one command.
- No unchanged package-bound gate is rerun.
- No project setup, Trust, fault injection, manual forms, or GUI capture is
  required for that path.
- Contract-sensitive changes trigger only the smallest relevant targeted gate.
- Product-contract changes still fail closed and require full qualification.
- Technical support and deferred usability/commercial claims remain explicitly
  distinguishable in every report.
