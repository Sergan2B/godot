# Sprint 10 plan — compound changes and automatic validation

**Status:** Complete locally on macOS arm64; source-bound evidence is published
by the final evidence-only commit

**Milestone:** Read/Write Beta Foundation / M3

**Duration:** 2 weeks

**Baseline:** `e811560c8e57c3bb84be2265d23a0449f55ab13a`

**Hard dependencies:** Sprint 9 source-bound macOS arm64 closeout, Bridge RPC
1.7 guarded transactions, the 36-tool MCP surface, Sprint 6 semantic index,
Sprint 7 editor/history revisions, and Sprint 8 runtime diagnostics.

**Normative contracts:** the new
[VALIDATION-001](VALIDATION-001-automatic-validation-and-rollback.md),
[WRITE-001](WRITE-001-editor-transactions-and-undo.md),
[PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md),
[MCP-001](MCP-001-project-scoped-read-tools.md),
[INDEX-001](INDEX-001-semantic-index-storage-and-migrations.md),
[EVIDENCE-001](EVIDENCE-001-semantic-facts-and-evidence.md),
[SCENE-001](SCENE-001-scene-node-resource-model.md),
[SCRIPT-001](SCRIPT-001-gdscript-and-csharp-adapters.md),
[EDITOR-001](EDITOR-001-live-editor-context.md),
[RUNTIME-001](RUNTIME-001-debugger-and-runtime-observation.md),
[PRODUCT-001](PRODUCT-001-semantic-bridge-vision-and-plan.md), and
[ARCHITECTURE-001](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md).
`VALIDATION-001` is created and frozen together with this plan by `S10-01`.

## 1. Outcome and fixed decisions

Sprint 10 implements atomic multi-operation transactions: Codex can prepare
one bounded semantic change set, obtain approval, apply it as one editor-native
action, persist only an explicit file scope, wait for editor/index convergence,
run required validation, and return an evidence-backed report. A required
validation failure invokes the selected rollback policy. The transaction is
successful only when its final state is proved; a timeout, conflict, or
recovery gap never becomes an optimistic pass.

- Bridge RPC `1.8` is an additive compatible extension. Sessions negotiated at
  `1.0` through `1.7` retain their exact earlier schemas and behavior.
- Sprint 9 capability `transaction.scene_v1` remains available and continues
  to mean exactly one operation. Sprint 10 adds `transaction.change_set_v1`
  and `validation.automatic_v1`; it does not silently widen the 1.7 contract.
- A change set contains `1..16` ordered semantic operations. All operations
  bind one project, one editor session, at most one saved open scene, one
  anchor native history, exact resource/script identities, and one immutable
  revision/hash precondition vector.
- The initial closed operation union contains the eight Sprint 9 operations
  plus create resource, update resource, and update GDScript source. Resource
  delete/rename/move, scene root replacement, C# source edits, imported assets,
  arbitrary file patches, and runtime-object mutation remain unavailable.
- Scene persistence is an explicit `save_scope`, not an implicit side effect.
  It accepts opaque IDs for the already-open saved scene and affected
  project-owned resources/scripts. It never accepts a directory, glob,
  absolute path, save-as destination, or unrelated dirty scene.
- Create resource is limited to `Gradient`, `Curve`, `Curve2D`, `Curve3D`,
  `Animation`, `StandardMaterial3D`, and `CanvasItemMaterial` at a normalized
  new `res://*.tres` destination. Update resource preserves identity/UID and
  changes only the per-class properties frozen by VALIDATION-001. Binary
  `.res`, imported artifacts, `.godot`, `project.godot`, editor settings, and
  `res://addons` are outside the initial scope.
- Script source updates target an existing project-owned `.gd` script by
  opaque resource identity, exact content hash, and bounded non-overlapping
  UTF-8 edits. They are not a general raw-file tool. Unsaved ScriptEditor
  conflicts, external changes, invalid encoding, symlinks, and generated or
  imported files fail before commit.
- Script write, reload, reparse, editor diagnostics, index invalidation, and
  semantic visibility are one observed pipeline. A successful file write
  without a proven reload/reparse barrier cannot pass validation.
- All editor-memory operations and their persistence hooks are represented by
  one `EditorUndoRedoManager` action in one anchor history. Resource-only
  change sets use the global history; a change set containing scene operations
  uses that scene's history. Cross-history and multi-scene change sets are
  rejected.
- `S10-01` includes a blocking native feasibility spike for mixed
  scene/script/resource apply, Undo, and Redo. No compound executor merges
  until one registered native action proves zero mutation during registration,
  deterministic all-or-nothing do/undo, observable failure, and no unsafe
  Redo gap.
- File outputs are fully serialized and validated into a private bounded
  staging area before the native commit point. Exact preimages and postimage
  hashes live in a separate private rollback escrow, never in the sanitized
  transaction journal, MCP output, logs, or evidence.
- The atomicity guarantee covers Bridge/editor/index-visible state and
  recoverable project files. Portable filesystems cannot hide each rename
  from an unrelated process reading the directory concurrently; the contract
  promises bounded staging, guarded replacement, recovery, and no published
  successful generation until the complete batch is proven.
- Standard Godot Undo/Redo invokes one retained compound action context.
  Before changing memory or disk, that context rechecks every expected
  postimage/preimage. A conflict performs zero compound work and is reported
  honestly; it never partially restores or overwrites an external change.
- Prepare and preview remain content/history read-only. They may allocate
  bounded plan, baseline, staging metadata, and journal records, but may not
  write a project-content file, change editor state, advance revisions, reload
  a script, or start a runtime.
- One immutable preview binds the ordered operations, dependency order,
  affected entities/files, persistence scope, validation policy, rollback
  policy, risk, confirmation requirement, limits, and preconditions. Reorder,
  scope expansion, policy weakening, or payload change invalidates its digest.
- Intrinsic editor postconditions, editor/index convergence, bounded pre/post
  semantic comparison, and diagnostics delta are required checks. Optional run
  validation is requested explicitly and creates a fresh Sprint 8
  `runtime_session_id`.
- Validation is asynchronous after apply. Apply returns the transaction state;
  status/events and the paginated validation report expose progress. Client
  timeout never cancels native commit, persistence, validation, or rollback
  after their respective commit points.
- Diagnostics policy distinguishes pre-existing findings from findings
  introduced by the transaction. New errors fail. New warnings are reported
  by default and may be configured to fail; unchanged pre-existing findings
  do not become a false transaction failure.
- A required check that is skipped, stale, truncated beyond its proof budget,
  disconnected, or unable to reach the expected revision is `inconclusive`,
  not `passed`.
- Automatic rollback is exact and conflict guarded. It may restore only while
  the transaction still owns the newest native action and every persisted
  postimage hash matches. Otherwise it becomes `rollback_blocked` or
  `in_doubt`, preserves foreign state, and returns explicit remediation.
- `rolled_back` and `failed_rolled_back` require proof of the original
  editor/file/hash/graph state. Merely issuing Undo or copying a backup is not
  proof.
- Confirmation remains host-owned. The default is `always_ask`. A user may
  explicitly grant a short editor-session policy for low-risk, memory-only
  change sets. Delete, script/resource content, persistence, runtime launch,
  and any elevated risk always require transaction-specific confirmation.
- The model cannot supply or weaken confirmation policy, approval receipt,
  validation outcome, or rollback proof in tool arguments. Tool annotations
  remain advisory metadata.
- The blocking qualification coordinate is local macOS arm64. Windows, Linux,
  hosted CI, model-facing validation, and embedded UI remain `not_run`.

## 2. Gates and deliverables

| Gate | Deliverable | Exit condition |
|---|---|---|
| `S10-01` | This plan, `VALIDATION-001`, WRITE-001 v2 decisions | Atomicity, persistence, report, rollback, confirmation, retention, and native feasibility are frozen |
| `S10-02` | Bridge RPC 1.8 schemas and capability profile | Strict change-set/validation DTOs pass and 1.0–1.7 remain compatible |
| `S10-03` | Compound planner and immutable preview | Ordered operations, dependencies, scopes, baselines, limits, and hashes are frozen without mutation |
| `S10-04` | Compound native executor | One anchor history action applies or restores all in-memory operations with no partial observable state |
| `S10-05` | Resource create/update executors | Allowlisted `.tres` resources round-trip with UID and property evidence |
| `S10-06` | GDScript edit, reload, and reparse pipeline | Bounded source edits produce revision-bound diagnostics and exact Undo/Redo |
| `S10-07` | Explicit scene/resource/script persistence | Only approved files are staged, saved, guarded, recoverable, and source-hash visible |
| `S10-08` | Validation coordinator and structured report | Required checks have ordered lifecycle, immutable evidence, pagination, and honest status |
| `S10-09` | Pre/post semantic graph comparison | Affected closure reaches the expected index revision and returns exact bounded deltas |
| `S10-10` | Diagnostics and optional runtime validation | New diagnostics and a fresh bounded runtime session feed the report without blocking Bridge |
| `S10-11` | Rollback policy, escrow, and recovery | Validation failure restores the proven pre-state or returns an honest blocked/in-doubt result |
| `S10-12` | Confirmation policies and advanced MCP surface | Trusted policy grants, change-set tools, validation reports, and reset semantics pass |
| `S10-13` | Independent scenario fixture and model-free workflow | Compound apply/save/validate/rollback/Undo faults are independently detected |
| `S10-14` | macOS arm64 M3 qualification | Clean source-bound evidence proves the read/write beta checklist |

Each implementation commit builds and passes its relevant tests. Generated
evidence never shares a commit with production, fixture, oracle, source-scope,
runner, or validator changes.

### S10-01 — Freeze VALIDATION-001 and the atomicity boundary

**Depends on:** Sprint 9 closeout evidence validates at the current checkout.

**Changes:** create
`VALIDATION-001-automatic-validation-and-rollback.md`; revise WRITE-001 to a
versioned Sprint 10 extension; amend PROTOCOL-001 and MCP-001 drafts. Freeze:

- compound identity, ordering, dependencies, preconditions, and idempotency;
- one-scene/one-history scope and the resource-only global-history rule;
- memory, disk, index, validation, Undo/Redo, and crash atomicity claims;
- staging/preimage escrow ownership, permissions, lifetime, and cleanup;
- save scope and project-owned file rules;
- validation check taxonomy, required/optional semantics, report schema, and
  result precedence;
- rollback policy, conflict behavior, recovery authority, and remediation;
- session confirmation grant trust and risk ceiling;
- hard limits, errors, capability downgrade, and Sprint 11 deferrals.

Run a tests-enabled editor spike proving a retained `CompoundActionContext`
can preflight and execute one do/undo callback for mixed scene, script, and
resource state, expose its result after native commit, and avoid partial work
when a preimage/postimage guard fails. Test manual Undo/Redo, an unrelated
newer action, editor shutdown, do/undo callback failure, and script reload.

**Done when:** the architecture's open “atomicity scene + script change”
decision is closed with executable evidence. If the spike cannot support the
promised one-action semantics, implementation stops and the roadmap scope is
revised explicitly; the plan does not silently call several actions atomic.

### S10-02 — Add the Bridge RPC 1.8 profile

**Depends on:** S10-01.

**Changes:** additive negotiation for `transaction.change_set_v1` and
`validation.automatic_v1`; strict change-set, operation, save-scope, baseline,
validation-policy, rollback-policy, report-summary, and recovery DTOs.
Bridge RPC 1.8 adds `transaction.prepare_change_set`,
`transaction.validation_complete`, `transaction.rollback`, and
bound validation-result DTOs while extending transaction status/events with
1.8-only fields. Existing `transaction.apply`, `transaction.status`, and
`transaction.undo` retain their 1.7 request meaning and gain fields only on a
negotiated 1.8 session. The complete validation report is composed and stored
by the sidecar and exposed through MCP; Bridge receives only the exact bound
summary and digest needed to finalize or roll back its native transaction.

The Bridge validates the final validation completion against transaction ID,
applied revision coordinates, report digest, check summary, and coordinator
lease. Sidecar prose or an unbound boolean cannot finalize or roll back a
transaction.

**Tests:** canonical Draft 2020-12 bundle; positive vectors for every new
operation/policy/state; duplicate keys and non-finite JSON; order/dependency
cycles; cross-scene/history mixes; missing hashes; unsafe scopes; oversized
patches/reports; illegal lifecycle transitions; false pass/rollback proof;
minor downgrade; and every Bridge RPC 1.0–1.7 regression.

**Done when:** C++ and Rust agree on every 1.8 DTO, lower minors cannot observe
or invoke 1.8 behavior, and no schema accepts absolute paths, globs, native
handles, unrestricted source text, approval material, or self-declared proof.

### S10-03 — Build the compound planner and immutable preview

**Depends on:** S10-02.

**Changes:** extend the prepared store and Rust coordinator for ordered
change-set plans. Canonicalize operations, build an explicit dependency DAG,
reject cycles and contradictory writes, resolve all entities on the main
thread, capture exact revisions/content hashes/index generation, compute the
affected closure seed, classify risk, derive confirmation requirements, and
build one bounded preview/digest.

Planner rules include:

- references to a resource or node created earlier in the same change set use
  plan-local opaque aliases, never guessed future native IDs;
- delete dominates later access and is rejected when a later operation
  references the deleted entity;
- property/script/resource writes to the same field/range are either
  canonicalized deterministically or rejected as conflicting;
- save scope is a subset of affected, project-owned, resolvable targets;
- validation and rollback policies may be strengthened by server policy but
  never weakened by the caller;
- all preflight work is time-sliced before staging or native action creation.

**Tests:** 1/16/17 operations; stable ordering; forward aliases; cycles;
duplicate/conflicting edits; stale scene/resource/script/index coordinates;
unsaved script conflict; scope expansion; idempotency replay/conflict;
preview/digest stability across C++/Rust; no revisions/history/files changed;
expiry and event-gap invalidation; and bounded main-thread slices.

**Done when:** every do/undo, persistence, validation, and rollback-relevant
fact is frozen before approval, and prepare remains byte-for-byte read-only for
project content.

### S10-04 — Implement the compound native executor

**Depends on:** S10-03 and the successful S10-01 spike.

**Changes:** `CompoundTransactionExecutor` and retained
`CompoundActionContext`; final whole-set preflight; one anchor history;
operation dependency ordering; precomputed do/undo records; one native action
registration; synchronous bounded apply/restore entry points; exact result
latch; committed entity mapping; revision publication only after the complete
batch; manual native Undo/Redo reconciliation.

No child executor may call `commit_action()`. It contributes a validated
detached step to the compound context. Context preflights all guards before
executing its first step. An internal step failure runs the already prepared
inverse steps in reverse order before returning. Status claims a zero-net
failure only after every precondition and pre-state check succeeds.

**Tests:** mixed create/set/connect; delete/reparent ordering; plan aliases;
failure at every step before/after its do call; inverse failure; duplicate
callback; unrelated history action; manual Undo/Redo; response loss at the
compound commit point; one revision/event publication; and no intermediate
Bridge snapshot or MCP success.

**Done when:** the fixture observes either the exact pre-state or exact
post-state, one native action owns the entire change set, and standard Undo
restores the complete in-memory pre-state.

### S10-05 — Add create/update resource operations

**Depends on:** S10-04.

**Changes:** the fixed `Gradient`, `Curve`, `Curve2D`, `Curve3D`, `Animation`,
`StandardMaterial3D`, and `CanvasItemMaterial` allowlist; the closed
VALIDATION-001 property table; plan-local resource aliases; create at a
normalized new `.tres` destination; update an existing project-owned resource
by opaque ID/UID; strict safe Variant projection; UID/path collision
protection; guarded existing-texture references; deterministic serialization;
exact old property/preimage capture; index invalidation and committed entity
evidence.

Create refuses overwrite. Update refuses imported/generated/binary/external/
addon targets and preserves UID. No operation renames, moves, deletes, imports,
or writes a raw serialized blob.

**Tests:** create with references; update scalar/container/resource reference;
UID preservation; path collision; wrong type/read-only property; unsafe
Variant; cyclic/oversized resource graph; foreign/imported/addon target;
serialization failure; apply/Undo/Redo; save/no-save modes; and resource graph
visibility.

**Done when:** allowlisted resources round-trip through the compound action,
only the explicit save scope changes disk, and index/evidence resolves the
same identity after save and Undo.

### S10-06 — Add bounded GDScript edits, reload, and reparse

**Depends on:** S10-04.

**Changes:** existing `.gd` script source operation with exact content hash,
ordered non-overlapping UTF-8 range edits, normalized line endings, bounded
replacement/result size, redacted unified preview, ScriptEditor conflict
checks, staged postimage, editor reload, GDScript parse/analyze barrier,
diagnostic revision, script graph invalidation, and exact preimage Undo/Redo.

The adapter resolves the canonical project-owned script from its opaque
resource identity. It never accepts a free destination or changes `.cs`,
`.gdextension`, imported/generated files, symlinks, editor/plugin code, or an
unrelated unsaved buffer. Parse errors are valid validation findings, not a
license to publish a successful transaction.

**Tests:** one/multiple edits; Unicode and line-ending normalization; stale
hash/range; overlap; invalid UTF-8; oversized patch/result; unsaved buffer and
external-change conflicts; introduced/resolved syntax error; reload timeout;
script attachment identity; semantic symbols/references refresh; exact
apply/Undo/Redo; and no source content in journal/log/evidence.

**Done when:** applied script bytes, editor resource, parser/analyzer
diagnostics, and script index all converge on one proven content generation,
and Undo restores the prior generation.

### S10-07 — Implement explicit scoped persistence

**Depends on:** S10-05 and S10-06.

**Changes:** private project-local staging/escrow coordinator; serialize every
requested scene/resource/script postimage before commit; validate file types,
ownership, roots, symlinks, sizes, expected preimage hashes, free space, and
permissions; create owner-only escrow; register one compound persistence step;
guarded replacement; editor filesystem rescan/reload; postimage verification;
cleanup and crash marker.

Save semantics:

- an open scene is selected by scene ID and saves only to its existing
  canonical `.tscn` path;
- affected resources/scripts are named by opaque IDs and resolved internally;
- unrelated dirty scenes, resources, script buffers, project settings, and
  imports are never saved;
- `save_scope: none` leaves every project-content byte unchanged;
- a post-commit external modification blocks Undo/rollback instead of being
  overwritten;
- durable escrow exists only while a commit/recovery window requires it;
  retained native history keeps bounded in-memory preimages for normal
  Undo/Redo after the durable escrow is deleted.

**Tests:** exact/no save; mixed three-file save; staging and each replacement
fault; disk full/permission/symlink/path swap; external edit before and after
commit; editor crash at each marker; stale escrow recovery/quarantine;
permissions; max files/bytes; cleanup; unchanged unrelated hashes; native
Undo/Redo persisted bytes; and editor/index resynchronization.

**Done when:** success proves every scoped postimage and every unscoped file
unchanged; recovery proves either all scoped postimages or all preimages
without trusting incomplete journal state.

### S10-08 — Build validation orchestration and structured reports

**Depends on:** S10-04 and S10-07.

**Changes:** sidecar `ValidationCoordinator`; immutable baseline; check DAG;
deadlines and cancellation boundaries; transaction/status/event integration;
report store; canonical report digest; pagination; redaction; finalization
handshake with Bridge; retained safe summaries in the transaction journal.

Each check reports:

- stable check ID, kind, required flag, status, start/end, and safe reason;
- exact input/output revision coordinates and content/index hashes;
- affected opaque entities and bounded evidence references;
- diagnostics delta or semantic delta summary when applicable;
- limits/truncation and whether truncation prevents proof;
- runtime session/exit metadata when applicable;
- rollback decision/outcome when applicable.

Statuses are `pending`, `running`, `passed`, `failed`, `inconclusive`,
`timed_out`, `cancelled`, and `not_run`. Overall precedence is rollback
failure/in-doubt, required failure, required inconclusive/timeout, passed, then
optional findings. `not_run` is valid only for an unrequested optional check.

**Tests:** deterministic DAG; duplicate/late completion; stale report inputs;
required skipped; optional timeout; pagination stability; restart recovery;
tampered report digest; output bounds; redaction; no false pass; and status
reconciliation after lost finalization response.

**Done when:** every terminal transaction links one immutable validation
report or an explicit reason no report can be authoritative, and status never
derives success from prose.

### S10-09 — Compare bounded pre/post semantic graphs

**Depends on:** S10-03 and S10-08.

**Changes:** capture a baseline seed at prepare; wait after apply/save/reload
for index and live overlay to reach the expected editor/file generations;
compute the bounded affected closure; compare canonical entities, facts,
relations, confidence, freshness, conflicts, and evidence; emit added,
removed, changed, unresolved, and unexpectedly affected summaries.

The comparison does not diff arbitrary serialized JSON or the whole project.
It uses canonical semantic identities and exact source generations. A gap,
rebuild, stale overlay, exceeded proof limit, or failure to reach the expected
revision is inconclusive. Truncation may summarize extra changes but may not
hide whether the required postcondition was proved.

**Tests:** node/signal/script/resource deltas; UID-stable resource update;
expected vs unexpected affected entity; unchanged graph; index lag; event gap;
rebuild during validation; identity replacement; conflict facts; limit edge;
rollback returning to baseline; and an independent golden graph oracle.

**Done when:** the report proves the requested semantic effects and identifies
unexpected affected entities without claiming whole-project equivalence.

### S10-10 — Add diagnostics and optional runtime validation

**Depends on:** S10-06, S10-08, and S10-09.

**Changes:** collect pre/post editor, GDScript parser/analyzer, and index
diagnostics by exact generation; normalize and diff stable finding identities;
classify introduced/resolved/persisting findings; enforce error/warning policy.
For an explicitly requested run check, use Sprint 8 lifecycle to start current
scene or project, require a new runtime session, observe bounded errors,
warnings, stacks, and exit state, then stop/clean up.

Runtime validation has an exact mode, deadline, observation window, expected
exit policy, and source-mapping requirement. It never reuses a previous
runtime session. Timeout/crash/disconnect remains a report result and does not
block Bridge or trigger a blind rerun.

**Tests:** new/resolved/pre-existing error and warning; diagnostic identity
across line shifts; parse/reload race; stale diagnostic; clean run; intentional
runtime error with stack/evidence; crash; hang/timeout; disconnect; wrong/
reused runtime session; stop cleanup; optional not-run; required inconclusive;
and Sprint 8 regression.

**Done when:** a report can distinguish “the project already had this finding”
from “this change introduced it,” and an optional run produces bounded
session-scoped evidence without compromising transaction recovery.

### S10-11 — Implement rollback policy and recovery

**Depends on:** S10-07 through S10-10.

**Changes:** policies `never`, `on_required_failure`, and `on_any_failure`;
default `on_required_failure`; exact rollback lease; top-of-history and
postimage guards; reverse compound execution; persisted preimage restore;
script/resource reload; index convergence; baseline graph verification;
rollback report; crash/reconnect recovery; escrow GC/quarantine.

Rollback begins only from an applied transaction and only after the validation
coordinator records a bound failure/inconclusive result allowed by policy.
Manual user/plugin edits or external file changes never get overwritten.
When guards fail, status is `rollback_blocked`; uncertain native/file outcome
is `in_doubt`. Recovery may finish an already-authorized exact restore but may
not create a new change set, expand scope, or rerun validation blindly.

Native Redo after an automatic rollback is treated as a deliberate reapply:
the known transaction becomes `reapplied_validation_required`, persistence
guards run through the retained compound context, and no saved/validated claim
is reused. If a safe retained context is unavailable, Redo performs zero
compound work and publishes a conflict.

**Tests:** each policy; error/warning/runtime failure; rollback at every
memory/file step; unrelated action above transaction; external postimage
change; sidecar/editor crash in validation and rollback; lost rollback
response; duplicate command; baseline proof failure; redo after rollback;
escrow corruption; no secret leakage; and no false `rolled_back`.

**Done when:** every failed required-validation scenario ends in a proven
pre-state or an honest blocked/in-doubt state with foreign work preserved.

### S10-12 — Publish confirmation policies and the advanced MCP surface

**Depends on:** S10-01 and S10-08 through S10-11.

**Changes:** four new closed tools:

- `godot_prepare_change_set`;
- `godot_get_validation_report`;
- `godot_get_confirmation_policy`;
- `godot_reset_confirmation_policy`.

The production registry grows from 36 to 40 tools. Existing Sprint 9
operation-specific preparation/apply/status/Undo tools remain unchanged.
`godot_prepare_change_set` accepts the closed operation union, exact
coordinates, save/validation/rollback policy, and idempotency key. It is
non-read-only, non-destructive, and does not accept approval material.
Validation report and policy read are read-only. Policy reset is
non-read-only, non-destructive, and can only narrow authority.

The apply elicitation shows the immutable compound preview and lets the user
choose:

- `always_ask`, the default;
- `allow_low_risk_for_session`, limited to 15 minutes, the exact
  project/editor session, and low-risk memory-only scope.

The second mode never covers delete, source/resource content, save, runtime
launch, imported/generated targets, elevated risk, or a scope not shown in the
grant form. There is no `never`, permanent, cross-session, model-selected, or
free-text bypass. The user can reset the grant immediately.

Validation reports are paginated and bound to transaction/report IDs and
revision coordinates. They expose semantic and diagnostic evidence, not full
source/preimage/escrow content. All forty tools have exact schemas and
`openWorldHint: false`.

**Tests:** exact registry and annotations; host without elicitation; every
policy choice; grant expiry/session/project/risk/scope mismatch; reset;
tampered preview; decline/cancel/timeout; change-set and report bounds;
pagination replay; offline/incompatible/read-only states; and all 36 Sprint 9
tools unchanged.

**Done when:** Codex can prepare, approve, apply, observe validation, inspect
the structured report, and Undo one compound workflow through MCP, while no
tool can self-approve, weaken validation, expand save scope, or read escrow.

### S10-13 — Freeze the scenario fixture and model-free M3 workflow

**Depends on:** S10-03 through S10-12.

**Changes:** independent hashed transaction scenario fixtures with one saved
open scene, attached GDScript, signal target, allowlisted `.tres` resource, UID
references, pre-existing warning, intentional parse/runtime failures,
external-edit commands, large bounded cases, and deterministic native actions.
Add golden pre/apply/save/validate/rollback/Undo truth, independent semantic
graph oracle, source/hash oracle, strict report validator, and fault
controller.

The primary workflow:

1. read editor/index/diagnostic baseline;
2. prepare a change set that adds a node, connects a signal, updates a
   resource, edits its script, and explicitly saves the affected scope;
3. prove prepare is read-only and stale changes fail;
4. approve through the real local host path and apply;
5. wait for reload/index convergence and diagnostics;
6. optionally run the scene and capture runtime evidence;
7. inspect the structured validation report;
8. use standard Godot Undo and prove exact editor/file/graph restoration;
9. rerun with an intentional failure and prove policy rollback;
10. execute persistence, validation, crash, disconnect, and recovery faults.

The oracle imports no production planner, executor, serializer, graph-diff,
validation, rollback, or report code.

**Tests:** all operation families; 16-operation boundary; each new structured
error; wrong project/session/scope/hash/revision; approval policies; fault at
every lifecycle/persistence/validation/rollback boundary; native Undo/Redo;
external action/file conflict; large reports; cleanup; redaction; independent
graph/diagnostic/file hashes; and Sprint 6–9 regressions.

**Done when:** the oracle detects partial apply/save/rollback, stale graph,
false clean diagnostics, reused runtime evidence, unsafe overwrite, approval
bypass, source leakage, and a report claiming more proof than its limits.

### S10-14 — Qualify local macOS arm64 and close M3

**Depends on:** all implementation and gate commits complete.

**Changes:** no implementation changes. Run the source-bound acceptance
wrapper and add only
`tests/codex/evidence/sprint-10-read-write-beta-macos.json`.

**Tests:** contracts and named negatives; Rust fmt/full locked tests/Clippy;
Bridge RPC 1.0–1.8 schemas; tests-enabled Godot editor build and focused
`*CodexS10*` suites; release sidecar; model-free compound workflow; native
Undo/Redo; save/reload/index/diagnostics/runtime validation; rollback/recovery;
policy/permissions/cleanup/redaction scans; and Sprint 6–9 regressions.

**Done when:** immutable evidence validates against a clean source coordinate,
the read/write beta checklist passes, all compound workflows have exact
transaction/report IDs and full state proof, and external gates remain
honestly `not_run`.

## 3. Compound transaction and validation contract

### 3.1. Lifecycle

The public Sprint 10 lifecycle extends, but does not redefine, Sprint 9:

```text
preparing → previewed → awaiting_approval → applying
                                                ↓
                                             applied
                                                ↓
                         save_scope != none → persisting
                                                ↓
                                             reloading
                                                ↓
                                             validating
                                           ↙            ↘
                                  validation_passed   validation_failed
                                         ↓                ↓
                                    committed      rollback_pending
                                                           ↓
                                                     rolling_back
                                                    ↙            ↘
                                             rolled_back   rollback_blocked

any post-commit uncertain boundary → in_doubt → reconciliation only
manual Redo after rollback         → reapplied_validation_required
```

Skipped phases are recorded explicitly. Transaction sequence is monotonic.
Scene/project/operation/index/script/resource/runtime coordinates advance only
at their authoritative boundaries. Validation report sequence is separate
from editor event sequence and cannot manufacture editor freshness.

### 3.2. Change-set identity and atomicity

Every plan binds:

- project/editor session, protocol minor, capability set, and policy grant;
- opaque `transaction_id`, caller idempotency key, and canonical operation DAG;
- one scene/history anchor or the resource-only global history;
- exact scene/operation/project revisions and index generation/revision;
- exact resource/script IDs, UIDs where available, and content hashes;
- plan-local aliases for entities created by earlier operations;
- affected closure seed and explicit persistence scope;
- validation, diagnostics, runtime, rollback, and confirmation policies;
- preview/report algorithms and applied hard limits;
- creation/expiry timestamps and canonical digest.

Change-set atomicity means:

1. prepare and staging publish no project mutation;
2. final preflight covers the entire set before the first do step;
3. the main-thread native callback does not service another Bridge mutation
   or snapshot while executing the bounded compound step;
4. step failure restores completed steps before returning;
5. file replacements use staged postimages and exact guarded preimages;
6. editor/index success is published only after the whole set converges;
7. recovery never guesses, replays apply, or overwrites a mismatched file.

This guarantee does not claim an OS-wide multi-file transaction visible to
arbitrary external readers. Such a claim is explicitly outside the contract.

### 3.3. Closed operation union

| Operation | Key preconditions | Exact inverse |
|---|---|---|
| Sprint 9 node/property/script-link/signal operations | Existing WRITE-001 guards | Existing native inverse, lifted into compound context |
| create resource | allowlisted type, unused `.tres`, valid properties | remove exact created resource if postimage still matches |
| update resource | project-owned identity/UID, writable properties, exact hash | restore canonical properties and serialized preimage |
| update GDScript source | existing `.gd`, exact hash/ranges, no buffer conflict | restore exact UTF-8 preimage and reload/reparse |

Scene save is not a semantic operation. It is an explicit persistence scope
over the post-state produced by the operation set. This prevents a save-only
request from masquerading as a content change and keeps affected-file approval
visible.

### 3.4. Persistence and preimage escrow

The escrow is separate from the sanitized journal because it may contain
project content. It is located under a private generated transaction directory,
uses owner-only access, rejects links/reparse traversal, has a schema and
manifest of relative typed targets plus hashes, and is bounded before apply.
Its contents are never serialized into Bridge RPC, MCP, logs, or evidence.

Durable escrow is retained only across the apply/validation/rollback recovery
window. After a committed/rolled-back terminal result and cleanup proof,
durable bytes are deleted. Normal native Undo/Redo relies on the bounded
in-memory action context while that editor history exists. Persistent Undo
after editor restart is not promised.

If the editor crashes with an active marker, the next compatible local
coordinator may:

- verify the exact project and transaction manifest;
- compare each current file with its preimage/postimage hash;
- complete a fully determined pending replacement or restore;
- quarantine inconsistent/corrupt escrow and report `in_doubt`;
- never apply a new semantic operation from escrow.

### 3.5. Validation policy and report

Required checks:

1. intrinsic compound editor postconditions and one native action;
2. explicit persistence result or proof that save scope was none;
3. script/resource reload and editor filesystem convergence where affected;
4. index/live-overlay convergence to expected revisions;
5. affected pre/post semantic graph comparison;
6. diagnostics delta.

Optional check:

- run current scene or project through the Sprint 8 runtime contract.

Configurable policy fields are closed enums: run mode, new-warning treatment,
per-check deadline, total deadline within server maxima, and rollback policy.
The client may request stricter behavior or lower limits. It cannot disable the
intrinsic, convergence, graph, or diagnostics checks.

The immutable report includes transaction/report IDs, preview/report digests,
policy, baseline/post coordinates, affected entities/files, check records,
diagnostic delta, semantic delta, runtime evidence summary, persistence
summary, rollback outcome, limits/truncation, and safe remediation.

### 3.6. Semantic graph comparison

The pre baseline records canonical entity/fact keys and source generations for
the bounded affected seed. The post comparison waits for the expected revision
barrier, recomputes the same closure plus newly created aliases, and classifies:

- expected additions/removals/changes;
- unexpected affected entities/facts;
- unresolved or conflicted identities;
- confidence/freshness changes;
- validation evidence linking the transaction to the report.

No graph comparison may upgrade confidence or hide conflicts. A partial graph
can support a partial diagnostic summary but cannot satisfy a required proof
whose affected closure exceeded limits.

### 3.7. Diagnostics and run validation

Diagnostic identity uses source, stable code, severity, canonical entity/file,
range fingerprint, and source generation. Reports contain bounded messages and
locations but not unrestricted source lines, stack locals, or project roots.

Runtime validation always creates a new session. Its report binds the exact
runtime session to the transaction's post-state evidence. A clean exit proves
only the requested observation window and run mode. Crash, timeout,
disconnect, missing source mapping, or required new error is not a pass.

### 3.8. Rollback and Undo

Rollback policy is part of the approved preview. It cannot expand scope after
apply. Automatic rollback:

1. freezes the bound validation result;
2. verifies transaction is the newest owned action;
3. verifies every current memory/file postcondition;
4. invokes the retained exact compound inverse;
5. reloads/reparses affected content;
6. waits for editor/index convergence;
7. compares against the baseline;
8. publishes `rolled_back` only after all proofs pass.

Targeted MCP Undo after a committed transaction uses the same guards and native
history rules. Standard Godot Undo/Redo is observed and reconciled. Neither
path skips unrelated work or manually writes a stored inverse around native
history.

### 3.9. Confirmation policies

`always_ask` requires a fresh bounded host form for every change set.
`allow_low_risk_for_session` requires a user-selected grant in that form and
binds project, editor session, risk ceiling, allowed memory-only scopes,
issued/expiry times, and nonce. Maximum lifetime is 15 minutes and editor
session replacement revokes it.

Any delete, source/resource content edit, persistence, runtime launch, elevated
risk, changed preview, changed scope, or policy expansion requires a new
transaction-specific confirmation. The policy grant never crosses MCP as an
input or output and is not written to the normal journal.

### 3.10. Initial hard limits

S10-01 may lower these values after fixture calibration. Raising one requires
a contract revision and named-negative fixture update.

| Limit | Initial hard maximum |
|---|---:|
| Semantic operations per change set | 16 |
| Open scenes / anchor histories | 1 / 1 |
| Persisted files per transaction | 8 |
| Created resources per transaction | 4 |
| Script range edits per transaction | 64 |
| Encoded change-set payload | 262,144 bytes |
| Encoded preview | 262,144 bytes |
| Replacement script bytes / resulting script | 65,536 / 524,288 bytes |
| Serialized resource or scene postimage | 2 MiB each |
| Durable rollback escrow | 8 MiB |
| Affected semantic entities / facts | 2,000 / 10,000 |
| Diagnostic records | 500 |
| Validation report page / pages | 65,536 bytes / 4 |
| Validation report retained total | 262,144 bytes |
| Required non-runtime validation | 30 seconds |
| Runtime validation observation | 30 seconds |
| Total validation and rollback | 60 seconds each |
| Session confirmation grant | 15 minutes |
| Active compound persistence lease | 1 per project |

All Sprint 9 Variant, preview, approval, journal, request, dispatcher, and
runtime screenshot limits remain hard ceilings unless VALIDATION-001 explicitly
sets a lower compound limit.

### 3.11. Structured errors

VALIDATION-001 freezes exact retryability and safe coordinates for at least:

- `change_set_invalid`;
- `change_set_dependency_cycle`;
- `change_set_conflict`;
- `change_set_too_large`;
- `change_set_history_mismatch`;
- `resource_operation_unsupported`;
- `resource_path_conflict`;
- `resource_uid_conflict`;
- `script_source_conflict`;
- `script_edit_invalid`;
- `persistence_scope_invalid`;
- `persistence_preflight_failed`;
- `persistence_failed`;
- `persistence_in_doubt`;
- `reload_failed`;
- `index_convergence_timeout`;
- `validation_failed`;
- `validation_inconclusive`;
- `validation_timed_out`;
- `validation_report_not_found`;
- `validation_report_mismatch`;
- `rollback_required`;
- `rollback_blocked`;
- `rollback_failed`;
- `rollback_in_doubt`;
- `confirmation_policy_expired`;
- `confirmation_policy_scope_mismatch`;
- all applicable Sprint 9 transaction/approval/stale errors.

Errors contain safe opaque IDs, phase, retry/action, and current coordinates.
They never echo full source, preimages/postimages, approval/policy grants,
absolute paths, native IDs, stack locals, or escrow records.

## 4. Commit boundaries

| Boundary | Allowed content | Required gate before commit |
|---|---|---|
| `C10-1 contracts` | S10-01/02 plan, VALIDATION/WRITE/protocol/MCP docs, schemas, vectors, native spike | schema conformance + `CodexS10AtomicitySpike` |
| `C10-2 compound core` | S10-03/04 planner, store, coordinator, native context | tests-enabled build + `CodexS10CompoundLifecycle` |
| `C10-3 persistence operations` | S10-05/06/07 resource, script, save, staging/escrow | `CodexS10Persistence` + file/hash oracle |
| `C10-4 validation core` | S10-08/09 report coordinator and graph comparison | Rust workspace gates + independent graph oracle |
| `C10-5 runtime and rollback` | S10-10/11 diagnostics, runtime, rollback/recovery | `CodexS10ValidationRecovery` + fault matrix |
| `C10-6 sidecar/MCP` | S10-12 policies, tools, report pagination | fmt + full workspace tests + Clippy + exact registry |
| `C10-7 fixture/gate` | S10-13 fixture, oracle, runner, validators, source scope | Python regressions + model-free M3 workflow |
| `C10-8 evidence` | S10-14 evidence JSON only | `sprint10_acceptance.py --validate` |

If a qualifying run requires a source, fixture, oracle, runner, scope, or
validator fix, its evidence is discarded. The fix enters the appropriate
boundary, is committed, and the complete qualification restarts from a clean
coordinate.

## 5. Fixture and local M3 acceptance

Sprint 10 passes only when the independent oracle proves:

1. one immutable preview describes the exact ordered operations, aliases,
   affected entities/files, save scope, policies, limits, and preconditions;
2. prepare/staging changes no editor state, history, revision, project-content
   byte, script reload, diagnostic generation, index generation, or runtime;
3. stale scene/resource/script/index state and changed save scope fail before
   the native commit point;
4. a compound create-node/connect-signal/update-resource/update-script change
   creates exactly one transaction and one native action;
5. a fault at every operation boundary leaves the exact pre-state or restores
   it before any successful result;
6. create/update resource preserves the promised UID/identity rules and
   updates reverse semantic evidence;
7. GDScript changes reload/reparse and produce revision-bound diagnostics and
   symbol/reference updates;
8. only explicitly scoped files change; every unrelated project-content hash
   remains equal;
9. staged replacement, disk fault, crash, and reconnect produce the complete
   preimage or postimage set, never an unreported mixed success;
10. index/live overlay reaches the expected post revisions before graph or
    diagnostics validation passes;
11. the semantic report contains expected and unexpected affected entities,
    pre/post revisions, evidence, limits, and no false whole-project claim;
12. introduced, resolved, and pre-existing diagnostics are classified
    independently and the configured warning policy is enforced;
13. optional run validation creates a new runtime session, reports bounded
    errors/stacks/exit state, stops cleanly, and handles crash/hang/disconnect;
14. required validation failure invokes the selected rollback policy and
    proves memory, files, reload state, diagnostics, and graph returned to
    baseline;
15. an unrelated native action or external file edit blocks rollback/Undo
    without skipping or overwriting foreign work and without partial inverse;
16. standard Godot Undo restores the complete committed result and Redo
    requires fresh validation evidence;
17. apply/finalization/rollback response loss reconciles by transaction ID and
    never replays a change or reuses an old validation report;
18. session confirmation grants cannot authorize elevated, persistent,
    destructive, runtime, wrong-project, wrong-session, expired, or expanded
    scope;
19. oversized operations, scripts, resources, escrow, graphs, diagnostics,
    reports, and runtime observations fail within bounded time and memory;
20. journals, escrow metadata, MCP output, logs, diagnostics, and evidence leak
    no approval material, unrestricted source/preimages, native handles,
    absolute roots, or stack locals;
21. all Bridge RPC 1.0–1.7, the 36 Sprint 9 tools, and Sprint 6–9 acceptance
    regressions remain green;
22. the read/write beta checklist maps every roadmap criterion to immutable
    source-bound evidence.

The qualifying command is:

```sh
.venv/bin/python tests/codex/sprint10_acceptance.py --timeout 120
```

The wrapper refuses a dirty Sprint 10 source scope or an existing final
evidence path. It runs:

1. strict contracts, schema vectors, fixture manifests, and named negatives;
2. Rust formatting, full locked/offline workspace tests, and deny-warning
   Clippy;
3. Bridge RPC 1.0–1.8 conformance;
4. tests-enabled macOS arm64 editor build and focused `*CodexS10*` tests;
5. release sidecar and exact 40-tool MCP registry;
6. compound apply/save/reload/index/diagnostics/graph/run/Undo workflow;
7. validation failure and rollback policy matrix;
8. operation/persistence/runtime/crash/disconnect/recovery fault matrix;
9. session confirmation policy and approval negatives;
10. Sprint 6–9 regression profiles;
11. source bytes, UID, journal/escrow permissions, cleanup, process, and
    redaction scans;
12. independent report/read-write-beta checklist validation;
13. source/artifact SHA-256 binding and atomic evidence publication.

Initial SLOs are prepare p95 at most 750 ms for the reference change set,
non-runtime validation p95 at most 5 seconds after apply, validation status p95
at most 200 ms, report page p95 at most 300 ms, rollback visibility p95 at most
3 seconds excluding bounded index convergence, runtime validation within its
30-second observation limit, and zero unhandled Bridge dispatcher slices above
2,000 microseconds outside the bounded native compound callback.

Correctness overrides latency. A deadline can produce `timed_out`,
`rollback_pending`, `rollback_blocked`, or `in_doubt`; it can never publish a
false pass, discard escrow early, overwrite a conflict, or replay apply.

## 6. Local closeout

All `S10-01` through `S10-14` deliverables are implemented. Bridge RPC 1.8
publishes the additive `transaction.change_set_v1` and
`validation.automatic_v1` capabilities while the 1.0–1.7 matrix remains
unchanged. The MCP registry contains exactly 40 tools, and compound apply is
still unreachable without a host-owned approval binding.

The independent oracle freezes all 11 operation kinds, the 1/16/17 operation
boundary, alias/dependency negatives, rollback and confirmation matrices,
report limits, and recovery scenarios. The model-free live path proves:

```text
read → prepare/replay → approve → apply → validate/report
     → readback → status → targeted Undo → verify
```

The qualifying macOS arm64 artifact is
`tests/codex/evidence/sprint-10-read-write-beta-macos.json`. It is generated
only from a clean implementation commit by
`tests/codex/sprint10_acceptance.py`, validates its own source/artifact hashes,
and is committed without production, fixture, oracle, runner, validator, or
documentation changes. Windows, Linux, hosted CI, and model-facing
qualification remain explicit `not_run` coordinates.

## 7. Explicitly deferred

Multi-scene and cross-history atomic transactions; resource delete/rename/move;
scene save-as; closed-scene mutation; binary `.res`; imported assets; addon,
project settings, editor settings, input map, and autoload writes; script
create/delete/rename; C# or native-extension source editing; arbitrary raw file
patches; full-project graph equivalence; general test-command execution;
unbounded runtime observation; runtime-object mutation; persistent Undo after
editor restart; permanent or `never_ask` approval; embedded approval UI;
App/CLI/IDE/Dock parity; Windows/Linux qualification; hosted CI; installer,
doctor, and offline write mode are outside Sprint 10.

Sprint 11 consumes the M3 read/write beta contract to finalize external Codex
tool schemas, setup/doctor, multi-project behavior, compatibility, and surface
parity without introducing a second transaction or validation implementation.
