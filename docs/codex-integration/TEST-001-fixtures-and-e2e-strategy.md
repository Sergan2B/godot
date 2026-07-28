# TEST-001 — fixtures, external surfaces, and source-bound evidence

**Status:** Sprint 11 slice frozen; the broader 1.0 platform/model/Dock matrix
remains open

**Version:** 1.0

**Date:** 2026-07-24

**Parents:** [MASTER_SPRINT_ROADMAP](MASTER_SPRINT_ROADMAP.md),
[RELEASE-001](RELEASE-001-release-1.0-acceptance-and-evidence-plan.md), and
[SPRINT-11-PLAN](SPRINT-11-PLAN.md)

## 1. Purpose and authority

This contract defines the test artifacts and proof rules used to qualify the
local macOS arm64 External Codex Beta. It closes the Sprint 11 evidence format
without claiming that the wider Release 1.0 matrix is complete.

The implementation, oracle, runner, schemas, fixtures, prompt pack, package,
surface traces, usability reports, and final evidence have distinct roles:

- production code produces behavior;
- fixtures create known state and faults;
- an independent model-free oracle determines the expected semantic result;
- a runner acquires machine evidence without changing its pass rules;
- JSON Schemas reject missing, ambiguous, or additional authoritative fields;
- pre-acquired App/CLI/IDE and human reports prove workflows that cannot be
  manufactured by the final automated validator;
- the final evidence document binds all inputs and contains no executable
  acceptance logic.

No test may infer success from a version string, process exit alone, prose
answer, screenshot appearance, or the existence of a schema. Missing,
unreadable, stale, mismatched, `not_run`, `partial`, `blocked`, and
`inconclusive` evidence are not `passed`.

## 2. Immutable coordinates and source scope

Sprint 11 starts from this immutable Sprint 10 pair:

```text
qualified source:
  b225f77acf48648ef6f59a76ff5a7dbb824da7bb
Sprint 10 evidence-only commit:
  815bccedddcd7c64a387dc8e9d5de2e941003fea
Sprint 10 evidence file SHA-256:
  9785a50e1be25f511acd6bd67f6642998821f700aea31a0bb4a7c1670f5b1611
```

The Sprint 11 source coordinate is the clean commit immediately before final
evidence acquisition. The final evidence-only commit is a different
coordinate and may add only:

```text
tests/codex/evidence/sprint-11-external-codex-beta-macos.json
```

The validator must reject:

- a dirty file in the declared Sprint 11 source scopes;
- a source commit that already contains the final evidence path;
- a final evidence document that validates a different source tree;
- an implementation, fixture, oracle, runner, schema, prompt, report, or
  documentation change in the evidence-only commit;
- an artifact rebuilt after acquisition or a package hash absent from the
  detached manifest.

Generated build outputs and user-local installed files never enter the source
scope. Every source-scope exclusion is explicit, narrow, and tested; an
unmatched path cannot silently become out of scope.

## 3. Required fixtures

Sprint 11 composes the existing qualified fixtures instead of replacing their
independent oracles.

| Fixture | Required state and faults | Principal proof |
|---|---|---|
| semantic golden | stable resources, scenes, scripts, identities, evidence, pagination | saved-query and offline equivalence |
| live editor | dirty state, selection, history, revision conflicts, reconnect | live overlay and stale-guard behavior |
| runtime diagnostic | remote tree, runtime-only node, deliberate warning/error/stack, viewport marker, hang/crash | runtime session and recovery behavior |
| transaction recovery | single/compound prepare/apply/validation/Undo, response loss, restart, rollback faults | exactly-once and project integrity |
| external beta | clean project setup, closed editor cache, two canonical roots, swapped identifiers/config, package paths | setup/doctor/offline/isolation/package behavior |

Every fixture has a versioned manifest, golden pre-state, expected post-state,
reset command, integrity digest, and maximum runtime. Fixture code may inject
faults but may not decide whether the product passed.

Two-project tests use separately created canonical roots, discovery bindings,
tokens, stores, journals, sessions, cursors, transactions, reports, and
sidecars. Copying or swapping any one of those inputs must fail closed rather
than select the other project.

## 4. Test layers

The acceptance stack is cumulative:

1. schema and pure unit tests cover closed DTOs, limits, state machines,
   compatibility rules, normalization, redaction, and deterministic rendering;
2. Rust/C++/Python integration tests cover Bridge RPC 1.0–1.8, MCP
   `2025-11-25`, the form-compatible `2025-06-18` floor, index, runtime,
   transactions, validation, setup, doctor, cache, and package boundaries;
   the macOS arm64 offline subprocess test runs the actual compiled sidecar
   from a validator-owned installed-package layout against a real disk segment
   store and source-hash authority, rather than an in-process MCP router;
3. source-bound model-free live acquisition exercises the exact local Godot
   Bridge prerequisite and the sidecar at `bin/godot-codex-mcp` from the
   detached package; Sprint 9 is split into operation, negative, and fault
   shards so each child retains the 180-second fail-closed bound;
4. package tests install to a temporary user-local root and exercise clean
   install, upgrade, rollback, uninstall, spaces, Unicode, missing/wrong Godot
   prerequisite, and an empty `PATH`;
5. external-surface acquisition runs the same canonical pack separately in
   Codex App, Codex CLI, and the official Codex extension for VS Code Stable;
6. human acquisition runs task-based usability with the frozen package and
   rubric;
7. the final validator checks all bound artifacts and release projections
   without opening a UI, invoking a model, installing software, or prompting a
   participant.

The canonical automated preflight executes the real Python and locked Cargo
gate commands. `--timeout` is a fail-closed per-command bound; it is not a
status or duration supplied by evidence:

```sh
.venv/bin/python tests/codex/sprint11_acceptance.py --timeout 180
```

Its result is explicitly non-qualifying because App/IDE interaction and human
acquisition are pre-acquired external facts. Final validation reruns the same
automated gates and then consumes the source-bound candidate evidence:

```sh
.venv/bin/python tests/codex/sprint11_packaged_regressions.py \
  --artifact-root '<detached-package-root>' \
  --package-manifest '<detached-package-manifest>' \
  --godot '<exact-arm64-Godot-prerequisite>' \
  --output-root tests/codex/acquisition/sprint11/package-live \
  --timeout 180
```

This is a separate acquisition profile, not part of the short automated
preflight. It runs eleven package-live commands: Sprint 6, Sprint 7, Sprint 8,
two Sprint 9 operation shards, one fast-negative shard, one approval-timeout
shard, two Sprint 9 fault shards, Sprint 10, and the Sprint 11 same-project
single-owner/takeover regression. Every command opts into the additive Sprint
11 registry while the unqualified Sprint 6–10 runners retain their original
exact registry gates by default. The acquisition refuses a
changed runner or fixture, a `HEAD` different from the package source, a
mismatched package sidecar/Godot prerequisite, an oversized report,
incomplete shard coverage, or a child exceeding 180 seconds. It publishes
reports plus one closed
`s11-packaged-regression-receipt/1.0` receipt atomically.

The exact detached manifest bytes, receipt, and reports are committed with the
other pre-acquired surface and usability artifacts. Final validation loads
them from the evidence source commit and independently rechecks runner hashes,
command templates, fixture hashes, semantic report projections, aggregate
Sprint 9 coverage, and package/Godot hashes:

```sh
.venv/bin/python tests/codex/sprint11_acceptance.py \
  --validate tests/codex/evidence/sprint-11-external-codex-beta-macos.json \
  --artifact-root '<detached-package-root>' \
  --timeout 180
```

The validator rejects an artifact created after the frozen acquisition window
or not bound to the same package/source/fixture coordinate. The evidence
`gates` object binds only canonical runner/definition digests. Passing state,
command output digests, and duration are observations produced by the current
runner invocation; self-declared `status`/`duration_ms` fields are invalid.

## 5. Canonical registries and schemas

The committed machine-readable contracts are:

```text
godot-codex-mcp/product/compatibility-matrix.v1.json
godot-codex-mcp/product/registry-profile.v1.json
godot-codex-mcp/schemas/godot_codex/compatibility-matrix.schema.json
godot-codex-mcp/schemas/godot_codex/sprint11-evidence.schema.json
godot-codex-mcp/schemas/godot_codex/sprint11-packaged-regression-receipt.schema.json
godot-codex-mcp/schemas/godot_codex/sprint11-surface-trace.schema.json
tests/codex/prompts/sprint11-external-beta-v1.json
```

The acceptance runner consumes these files directly. A rendered Markdown
table or duplicated constant is explanatory only and must be checked against
its canonical JSON source.

The MCP profile is exact:

```text
tools: 41
fixed resources: 4
resource templates: 1
```

Names, URIs, schemas, annotations, instruction digest, diagnostic codes,
remediation IDs, profile allowlists, and compatibility rows are unique. Exact
set equality is required; a count alone cannot pass.

## 6. Surface trace contract

Each canonical App/CLI/IDE semantic trace binds exactly the fields closed by
`sprint11-surface-trace.schema.json`: trace kind/status, surface and exact host
coordinate; source commit; package-manifest, fixture, registry, prompt-pack,
host, MCP, and Godot artifact digests; the exact MCP registry and instructions
digest; eighteen normalized semantic assertions; four form outcome classes; a
relational revision timeline; and the redaction declaration. Clean source
digest, compatibility-matrix digest, package contents, and acquisition report
digests are bound by final evidence rather than duplicated as extra trace
members. Every assertion ID selects a closed projection schema with required
semantic fields; an empty projection or a projection shaped for another ID is
invalid.

The recorder produces a separate bounded acquisition journal. Surface evidence
binds it by committed path and full-file SHA-256; the trace binds the same
full-file digest, ordered event hash-chain, and event count. Validation
recomputes the chain and requires exact surface, host, package, fixture,
registry, prompt, MCP, and Godot equality between trace and journal. A trace
without its exact journal cannot qualify.

Recorder metadata binds the exact detached
`sprint11-package-manifest.json`, supplied as a regular file beside the
metadata. The recorder validates that detached manifest against the source,
matrix, registry, Godot, and MCP bindings, then separately verifies the
installed `package-manifest.json`, `VERSION`, ownership marker, and MCP
executable against the detached manifest's content records. The detached and
installed manifests have intentionally different SHA-256 values; substituting
one digest for the other or rebuilding either file invalidates the capture.

Neither the recorder nor a model-authored trace can prove that stdin belonged
to the claimed App, CLI, or IDE session. Recorder output is therefore labelled
`surface_transport_capture`, not `real_surface`. Qualification additionally
loads one separate
`s11-surface-acquisition-authority/1.0` artifact per surface from the exact
evidence Git commit. Its external operator attests the observed official host
session, recorder-to-host binding, exact project root and nested cwd
resolution, project-config reload, package launcher, sandbox approval, and
accept/decline/cancel/timeout form outcomes. The same real-host attestation
records rejection of foreign config/root/cwd and mismatched package
digest/version with no sibling-project fallback. The authority binds the
exact trace, journal, package, host-provenance measurement, fixture, prompt,
host, client, and MCP digests.

Git and SHA-256 prove which authority and capture bytes were reviewed; they do
not cryptographically prove process ancestry or the operator's observation.
That fact is an explicit external-operator trust boundary, not a claim the
recorder can manufacture. Relabelling or recomputing a transport capture
without the separate canonical authority path remains nonqualifying.

The recorder passes stdio
bytes through unchanged and retains only content-minimized protocol metadata:
direction, method, safe tool/resource names, exact registry enumeration, form
action class, bounded status/error tokens, byte/frame counts, and child/
recorder integrity. It never retains arguments, tool content, form content,
source text, native IDs, private paths, or secrets. Its failure, truncation,
unanswered form, or wrapped-server crash invalidates the acquisition; it
cannot drop, reorder, synthesize, retry, or interpret a product message. The
semantic trace is derived and validated separately, and raw private transport
data is not retained.

Surface equality uses three classes:

1. Persistent project/resource/scene/script identities, normalized facts,
   source mappings, evidence, diagnostic codes, error classes, limits, and
   policy decisions compare exactly.
2. Editor/runtime sessions, snapshots, transactions, validation reports,
   cursors, and other ephemeral identities compare through one total,
   injective, kind-preserving bijection per isolated run.
3. Revisions compare relationally: monotonicity, causal order, guard binding,
   invalidation, and creation of a fresh session after restart.

Each preview digest must verify against the exact canonical preview bytes and
local revision/session coordinates in its own run. Raw preview digests and
revision magnitudes need not match across isolated runs.

Allowed differences are limited to UI layout, host/model prose, timestamps,
request/thread IDs, latency inside the same SLO class, and model-selected call
ordering that preserves all required semantic assertions. Normalization must
not ignore a missing call, changed fact/evidence/error/action, widened scope,
weakened guard, different approval outcome, or invalid local digest.

## 7. Approval and sandbox evidence

Host tool approval and semantic transaction form confirmation are independent
control layers and are recorded as separate event classes.

Every qualified surface must exercise:

| Outcome | Required result |
|---|---|
| host `accept` on the exact action-only form | one eligible receipt bound to the exact preview; apply may proceed once |
| decline | terminal rejection; no apply/mutation |
| cancel | no receipt or mutation; prepared plan remains only as allowed by its lifecycle |
| timeout | no receipt or mutation; retry creates a new elicitation nonce |
| missing form capability | structured read-only failure; no fallback |

Traces retain only the outcome class and receipt eligibility. They never store
form content, approval receipt/MAC/nonce, host decision text, account data, or
prompts. Non-empty form content, a boolean tool argument, config flag, prose
consent, shell prompt, tool annotation, or prior host approval cannot
substitute for the standard host action.

## 8. Canonical task pack and usability reports

The canonical prompt/task pack is version-controlled and contains only
project-independent user goals and expected semantic assertion IDs. It must
not reveal node paths, property values, stack frames, tree contents, resource
owners, screenshots, fault answers, or other context that the integration is
supposed to discover.

Final evidence records only the pack ID, repository-relative path, and
SHA-256. Participant free-form prompts, accounts, tokens, and personally
identifying data are never retained.

Human acquisition requires at least three clean-start runs, including one
participant externally attested as independent of implementation. Each
machine-readable aggregate entry binds the frozen source/package/fixture/task
pack and five exact Git artifacts:

- primary trace, rubric, consent receipt, and defect ledger;
- a separate external-operator authority cross-binding the pseudonymous
  participant, run, canonical acquisition directory, and all four exact-byte
  digests;
- task results, bounded timing, wrong-turn counts, help used, remediation
  outcomes, and unresolved defect IDs;
- comprehension of project binding, offline limits, sandbox approval,
  semantic confirmation, validation, and Undo;
- redaction and cleanup declarations.

The five files use fixed names below
`tests/codex/acquisition/sprint11/human/s11u-run-<32-lowercase-hex>/`.
Templates, synthetic fixtures, and arbitrary repository paths cannot be
relabelled into acquisition inputs. Git and SHA-256 establish exact-byte
provenance only; a study operator's claims that a real person participated,
consented before tasks, and was implementation-independent are an explicit
external trust boundary, not a cryptographically proved identity claim.

Developer coaching invalidates the task. A high/critical product or
instruction defect requires a source commit, rebuilt package, and complete
retest; it cannot be waived inside final evidence.

## 9. Package and prerequisite proof

The macOS arm64 package uses a detached canonical manifest. It binds both
user-facing binaries, schemas/registry/matrix/guidance, install metadata, and
the exact separately distributed Godot Bridge prerequisite. The archive hash
is recorded beside, not inside, the archive manifest so no self-referential
hash is needed.

The package run must prove:

- deterministic file order, timestamps, modes, ownership normalization, and
  byte-for-byte rebuild;
- no absolute source/build/user path;
- only expected executable files and no setuid/quarantine bypass;
- temporary user-local install root with no admin/network requirement;
- no project/config mutation before setup consent;
- rollback and uninstall remove only package-owned files;
- a missing, wrong-version, wrong-architecture, or wrong-hash Godot
  prerequisite fails with a stable remediation.

Developer binaries may be candidates but are not release evidence until their
exact identity is bound by the detached package manifest.

## 10. Redaction and boundedness

Source, package, traces, reports, logs, cache samples, doctor/setup output, and
final evidence are scanned for:

- Bridge tokens/proofs/endpoints, approval receipts/nonces/MACs, API keys, and
  environment secrets;
- absolute private roots, account/home names, PIDs, native handles/history
  IDs, unrestricted source/property content, prompts, and participant data;
- data belonging to the second project;
- unbounded trees, properties, diagnostics, stacks, screenshots, diffs,
  reports, traces, or error echoes.

A redaction marker must preserve field meaning and reason. Silently deleting a
required semantic field cannot turn a leaking record into a pass.

## 11. Final evidence and release projection

The final JSON contains closed sections for source, package, Godot
prerequisite, host coordinate, registry/contracts, automated gates,
surface traces, usability, security/redaction, SLOs, regressions, artifacts,
limitations, and release projection. Each gate records command/runner ID,
status, duration, and bound artifact digests; it does not embed raw logs.

Sprint 11 can pass only with:

```text
external_codex_beta_macos = passed
R1-08 = partial
R1-10 = partial
formal_beta_gate = not_reached
```

The partial release requirements explicitly name the missing Dock,
Windows/Linux, final uninstall/platform, signing/notarization, remote CI, and
any other unrun Release 1.0 coordinate. They are not silently inherited from
the local macOS App/CLI/IDE slice.

Any source-affecting fix discards the current qualifying run. After all
non-evidence changes are committed, the entire clean package/surface/usability
acquisition and automated gate is repeated. Only then may the evidence-only
commit be created.
