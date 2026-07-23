# Sprint 9 plan — safe editor transactions

**Status:** Complete. S9-01 through S9-12 are implemented, locally qualified
on macOS arm64, and closed by source-bound evidence. A native Windows x86_64
qualification coordinate was added afterward; see
[SPRINT-9-WINDOWS.md](SPRINT-9-WINDOWS.md).

**Milestone:** Write Foundation

**Duration:** 2 weeks

**Baseline:** `3e30a2bd0182c68b5e5a2726c176c3ddfeb48f1c`

**Hard dependencies:** Sprint 6 static evidence/query contracts and the
Sprint 7 live editor/revision/history contracts. Sprint 8 is complete and
remains a regression gate, but runtime validation is not required to apply a
Sprint 9 transaction.

**Normative contracts:** the new
[WRITE-001](WRITE-001-editor-transactions-and-undo.md),
[EDITOR-001](EDITOR-001-live-editor-context.md),
[PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md),
[MCP-001](MCP-001-project-scoped-read-tools.md),
[EVIDENCE-001](EVIDENCE-001-semantic-facts-and-evidence.md),
[PRODUCT-001](PRODUCT-001-semantic-bridge-vision-and-plan.md), and
[ARCHITECTURE-001](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md).
`WRITE-001` is frozen by `S9-01`; S9-02 implements its strict Bridge RPC 1.7
schemas, S9-03 provides immutable preparation, S9-04 activates the guarded
apply/status/Undo core, and S9-05 through S9-07 register the closed structural,
property, existing-script, and same-scene signal executors. S9-08 adds the
bounded Rust recovery coordinator and S9-09 publishes the guarded 36-tool MCP
surface.

## 1. Outcome and fixed decisions

Sprint 9 introduces the first project-content mutations, but only as
revision-guarded actions in Godot's native editor history. A successful
transaction changes an open scene in editor memory, marks the corresponding
history dirty, is visible through the Sprint 7 live overlay, and has one exact
Undo path. It does not save a scene or patch a source file.

- Bridge RPC `1.7` is an additive compatible extension. Clients negotiated at
  `1.0` through `1.6` see no transaction capability or fields.
- The capability is `transaction.scene_v1`. Capability presence does not imply
  write readiness; editor/session/scene state and approval readiness are
  reported separately.
- One Sprint 9 transaction contains exactly one semantic operation. The
  executor may register several native do/undo calls to implement that one
  operation, but multi-operation change sets remain Sprint 10.
- Supported operations are create, delete, and reparent node; set property;
  attach and detach an existing project script; and connect and disconnect a
  signal.
- Targets are saved scenes currently open in the bound editor session. Closed
  scene file mutation, cross-scene reparenting, scene-root replacement, and
  runtime-object mutation fail closed.
- Prepare and preview never mutate scene content, editor history, selection,
  source files, or revisions. They may allocate a bounded prepared transaction
  and sanitized generated journal metadata. This is the Sprint 9 dry-run
  contract; there is no second executor-specific dry-run path that could drift
  from the preview used by apply.
- Apply is a separate request bound to the transaction ID, immutable preview
  digest, exact approval scope, expiry, and current revision coordinates.
- A boolean, free-form string, capability token, or repeated apply call is not
  proof of user approval. The approval transport/trust binding is a blocking
  `S9-01` decision and must be proven with the actual local MCP host before any
  apply surface merges. If no verifiable approval result can reach the
  sidecar, apply remains unavailable rather than weakening this rule.
- All scene mutations go through `EditorUndoRedoManager`. Production Bridge
  code does not directly mutate a scene and then manufacture a history entry
  afterward.
- Every accepted prepare attempt receives a fresh opaque `transaction_id`.
  An idempotency-key replay with the same canonical request resolves to the
  existing transaction; reuse with different content is an error.
- Immediately before native action creation, apply rechecks project ID,
  editor session, scene identity/revision, operation sequence, object
  existence/type/ownership, preview digest, approval, idempotency, and limits.
- No apply is cancellable after the commit point. Response loss after that
  point produces status reconciliation by transaction ID and never a blind
  replay.
- `godot_undo_transaction` may undo only when that transaction is the newest
  applicable action in its exact native history. It never skips or undoes an
  unrelated user/plugin action.
- A user may invoke standard Godot Undo/Redo. Transaction status and revisions
  reconcile from native history events rather than hiding that action.
- Sprint 9 intrinsic validation checks the requested editor postcondition,
  native history entry, live scene revision, and absence of partial state.
  Diagnostics, test execution, runtime validation, pre/post semantic graph
  comparison, and policy-driven rollback belong to Sprint 10.
- No supported operation writes or saves `.tscn`, `.tres`, `.gd`, or `.cs`.
  The journal lives only in generated project-private state, contains bounded
  metadata and hashes, and is not an alternative source of scene truth.
- The original blocking acceptance coordinate is local macOS arm64. Native
  Windows x86_64 can now produce a separate source-bound evidence artifact.
  Linux, remote CI, model-facing validation, and approval UX outside the proven
  local host are reported honestly as `not_run`.

## 2. Gates and deliverables

| Gate | Deliverable | Exit condition |
|---|---|---|
| `S9-01` | This plan, `WRITE-001`, protocol/MCP amendments, approval decision | Lifecycle, authority, receipt binding, operations, limits, errors, retention, and Sprint 10 boundary are frozen |
| `S9-02` | Bridge RPC 1.7 schema/profile | Strict transaction DTOs and vectors pass; 1.0–1.6 compatibility remains green |
| `S9-03` | Transaction coordinator, immutable preview, revision guards | Prepare is read-only; stale/conflicted plans cannot reach the executor |
| `S9-04` | Apply/status/Undo core, idempotency, commit-point recovery | Exactly-once native action and status reconciliation pass response-loss faults |
| `S9-05` | Create/delete/reparent executor | Structural operations preserve ownership and fully Undo on supported scene topologies |
| `S9-06` | Set-property executor and safe write projection | Typed values apply within hard limits and restore the exact previous Variant |
| `S9-07` | Attach/detach script and connect/disconnect signal executors | Existing project scripts and signal connections round-trip through Undo |
| `S9-08` | Rust transaction coordinator and sanitized journal | Prepared/committed/in-doubt state survives the allowed sidecar reconnect cases without replay |
| `S9-09` | MCP preparation, apply, status, and Undo tools | Exact closed registry, approval enforcement, annotations, and safe summaries pass |
| `S9-10` | Independent fixture/oracle and fault matrix | Stale, fault, disconnect, native action, Undo/Redo, and raw-write denial are independently proven |
| `S9-11` | Model-free editor transaction workflow | MCP preview/apply/readback/Undo works without source-file changes or half-applied state |
| `S9-12` | macOS arm64 gate and source-bound closeout evidence | Rebuilt artifacts pass contracts, recovery, security, cleanup, SLO, and full Undo gates |

Each implementation commit builds and passes its relevant tests. Generated
evidence never shares a commit with code, fixture, oracle, source-scope, or
validator changes.

### S9-01 — Freeze WRITE-001 and the approval boundary

**Depends on:** Sprint 7 closeout; Sprint 8 regressions available.

**Changes:** create `WRITE-001-editor-transactions-and-undo.md`; amend
PROTOCOL-001 and MCP-001; freeze the state machine, transaction authority,
single-operation scope, public DTOs, intrinsic validation, native-history
rules, journal retention, errors, hard limits, and explicit Sprint 10 deferrals.
Run an approval-binding spike against the actual local MCP host. The preferred
path is a standards-based host confirmation/elicitation result converted by
the sidecar into a one-time opaque receipt bound to transaction ID, preview
digest, scope, nonce, and expiry. If that is unavailable, WRITE-001 must choose
and prove another explicit local confirmation channel; annotations alone and
`approved: true` are forbidden fallbacks.

**Tests/review:** threat-oriented contract review; approval replay, scope
escalation, expiry, wrong digest, unsupported host, and third-party-client
cases; trace every roadmap acceptance criterion to an executable gate.

**Done when:** no implementation decision about approval authority, commit
point, Undo ownership, persistence, expiry, or recovery remains implicit, and
the selected approval path has a model-free proof with the real host protocol.

The S9-01 server profile is MCP `2025-11-25`; the real-host probe also accepts
negotiated `2025-06-18` when, and only when, the host explicitly advertises
standard form elicitation. The bounded qualification trace records the exact
negotiated revision and is deleted after validation.

### S9-02 — Add the Bridge RPC 1.7 transaction profile

**Depends on:** S9-01.

**Status:** Complete. RPC 1.7 negotiation, the initially unavailable
capability, closed DTO validation, shared Rust/C++ vectors, safe limits,
downgrade behavior, and fail-closed routing are implemented. S9-04 now updates
that capability with live coordinator/scene/approval/busy readiness; S9-09
publishes the guarded production MCP write tools after those gates.

**Changes:** additive 1.7 negotiation; `transaction.scene_v1` capability and
limits; `transaction.prepare`, `transaction.apply`, `transaction.status`, and
`transaction.undo`; `transaction.event`; strict operation/preview/status/
approval/error schemas; session-scoped transaction coordinates; positive and
named-negative fixtures. Transaction and idempotency IDs outlive an individual
request/connection but never an incompatible project/editor scope.

**Tests:** canonical Draft 2020-12 bundle, duplicate/non-finite JSON rejection,
closed union discrimination, downgrade/upgrade negotiation, unknown operation,
unsafe ID/path/value, missing precondition, malformed digest/receipt, excessive
preview/operation/journal payload, illegal lifecycle transition, and all
Bridge RPC 1.0–1.6 regressions.

**Done when:** C++ and Rust agree on every 1.7 DTO and error, older clients
cannot reach write methods, and no schema accepts raw ObjectID, pointer, native
history ID, absolute path, or unbounded Variant content.

### S9-03 — Build the coordinator and immutable preview

**Depends on:** S9-02.

**Status:** Complete. The bounded coordinator, immutable digest-bound preview,
idempotency, revision guards, expiry/conflict invalidation, time-sliced
resolution, and read-only macOS fixture gate are implemented.

**Changes:** editor-side `TransactionCoordinator` and bounded prepared store;
fresh IDs; canonical request digest; idempotency lookup; time-sliced preflight;
scene/node resolution on the main thread; operation-specific semantic preview;
risk and approval scope; affected entity hints; exact preconditions; inverse
metadata held only as long as required; expiry/conflict invalidation on live
events, scene close, editor-session replacement, and project reset.

Prepare does not call `create_action`, alter a Node/Resource, advance a
revision, change selection, or mark history unsaved. Re-preparing after a stale
revision produces a new transaction and preview rather than silently updating
the old one.

**Tests:** byte-for-byte scene and history snapshot before/after prepare;
canonical idempotency replay; different-payload key conflict; stale revision,
closed scene, wrong project/session, ambiguous node, locked inherited/instance
boundary, expired plan, event-gap invalidation, preview bounds, and no main-
thread slice beyond the Bridge budget.

**Done when:** every apply-relevant fact is frozen in an immutable preview and
no stale or unsupported request can reach native action construction.

### S9-04 — Implement apply, status, Undo, and recovery semantics

**Depends on:** S9-03 and the proven S9-01 approval binding.

**Status:** Complete. One-time approval verification, final preflight,
exactly-once apply latches, native commit correlation, bounded status/events,
response-loss recovery, targeted Undo, and native Undo/Redo reconciliation are
implemented and locally proven with a dev-only fixture executor. No production
operation executor is registered before S9-05 through S9-07.

**Changes:** receipt verification; last-moment preflight; one in-flight apply
per scene history; explicit commit point; native action correlation;
transaction event/status publication; intrinsic postcondition check;
idempotent apply response; targeted top-of-history Undo; native Undo/Redo
reconciliation; disconnect and response-loss handling; terminal-result latch.

The coordinator distinguishes failures before the commit point from uncertain
delivery after it. A pre-commit failure leaves no mutation and returns a safe
retry/new-preview action. A lost response after commit returns or later
reconciles to `in_doubt`/committed status and never invokes the executor again.

**Tests:** expired/wrong/scope-mismatched/replayed approval; stale change in the
last frame before apply; duplicate concurrent apply; cancellation before
commit; cancellation ignored after commit; transport loss immediately before
and after commit; sidecar restart; duplicate terminal callbacks; unrelated
native action above the transaction; manual Undo/Redo; editor close/reset.

**Done when:** each accepted apply produces zero or one native action, every
result is recoverable by transaction ID, and targeted Undo cannot affect an
unrelated action.

### S9-05 — Add create, delete, and reparent operations

**Depends on:** S9-04.

**Status:** Complete. Production create/reparent/delete executors now perform
one bounded native action, publish post-commit identities only after intrinsic
validation, and retain deleted subtrees through native Undo references. Focused
C++ and local macOS gates prove exact topology, owner, order, Node2D/Node3D
transform, connection, duplicate/recovery, targeted/native Undo/Redo, fault,
source-hash, and 1000-node boundary behavior. S9-10–S9-12 complete the
independent oracle, model-free workflow, and source-bound closeout evidence.

**Changes:** operation validators and native do/undo registrations for node
creation, subtree deletion, and same-scene reparenting. Preserve deterministic
names, sibling position, owner, editable-instance rules, and applicable
Node2D/Node3D global transform policy. Return the final opaque editor node ID
only after commit. Scene roots, foreign owners, closed scenes, cross-scene
moves, and non-editable inherited/instanced content fail before action creation.

Delete retains the live subtree only through native Undo references; the
sanitized journal never serializes the subtree. Undo restores exact parent,
sibling index, owner relationships, supported transforms, and connections
that Godot preserves for the node object.

**Tests:** create/Undo/Redo, name collision, nested owner, saved instance and
editable child, locked inherited/instance rejection, root rejection, subtree
limit, delete/Undo identity, reparent in both directions, transform retention,
mid-registration fault, commit fault, and live overlay/history revisions.

**Done when:** each supported structural operation is one native history action
and pre/post/Undo scene oracles match exactly.

### S9-06 — Add typed set-property operations

**Depends on:** S9-04.

**Changes:** property metadata validation, read-only/editor-visibility checks,
safe writable Variant projection, strict type compatibility, bounded resource
references, exact old-value capture for native Undo, redacted preview, and
postcondition verification. Custom setters execute only through the normal
Godot property path after all preconditions pass.

Unsupported handles, Callables, Signals, RIDs, arbitrary Objects, absolute
paths, oversized/cyclic write payloads, nonexistent/read-only properties, and
unbound external resources fail closed. A write may narrow but never raise the
server hard limits.

**Tests:** scalar/vector/color/enum/resource reference round-trips, nullable
values, wrong type, nonexistent/read-only property, custom setter side effect,
unsafe handles/paths, cyclic/oversized containers, old-value redaction,
fault-before-commit, exact Undo/Redo, and revision/history observation.

**Done when:** the property after apply equals the canonical requested Variant
and Undo restores the canonical prior Variant without exposing unsafe values.

### S9-07 — Add script attachment and signal connection operations

**Depends on:** S9-04.

**Changes:** attach/detach only existing project scripts through canonical
resource identity; validate script/base compatibility without editing source;
connect/disconnect only declared signals and resolvable same-scene target
methods; preserve supported flags/binds; exact inverse registration and
semantic preview. Built-in script creation, script text patches, arbitrary
Callable payloads, runtime connections, and cross-scene targets are rejected.

**Tests:** attach, replace, detach, and Undo/Redo; missing/wrong-base/external
script; source hash unchanged; connect/disconnect round-trip; duplicate and
missing connection; bad signal/method; flags/binds bounds; deleted target;
stale revision; fault matrix; updated scene/live/history evidence.

**Done when:** script and signal operations round-trip through native Undo with
no script or scene file write and no false semantic connection fact.

### S9-08 — Build the Rust coordinator and sanitized journal

**Depends on:** S9-02–S9-04.

**Changes:** new `transactions` crate; strict Bridge client DTOs; canonical
plan/digest construction; approval routing; project-private journal with its
own schema/version; bounded recovery index; status/event reconciliation;
prepared expiry; conflict invalidation from editor events; redaction before
persistence and logs. The journal stores IDs, states, timestamps, scopes,
revisions, operation kind, affected opaque entities, and hashes—not full node
subtrees, source text, secrets, raw native IDs, or unrestricted property values.

**Tests:** strict DTOs, canonical digest vectors, journal permissions and
atomicity, corrupt/truncated/duplicate records, migration/rebuild, max-count/
byte GC, prepared sidecar restart, editor-session replacement, committed
response loss, in-doubt reconciliation, no replay, expiry, redaction, and two-
project isolation.

**Done when:** reconnect can explain and reconcile every retained transaction
without treating journal data as authoritative scene state or reapplying a
write.

### S9-09 — Publish the MCP transaction surface

**Depends on:** S9-01 and S9-05–S9-08.

**Changes:** eleven closed tools:

- `godot_prepare_create_node`;
- `godot_prepare_delete_node`;
- `godot_prepare_reparent_node`;
- `godot_prepare_set_property`;
- `godot_prepare_attach_script`;
- `godot_prepare_detach_script`;
- `godot_prepare_connect_signal`;
- `godot_prepare_disconnect_signal`;
- `godot_apply_transaction`;
- `godot_get_transaction_status`;
- `godot_undo_transaction`.

The registry grows from 25 to 36 tools. Preparation tools are non-read-only
because they allocate bounded transaction state, but are non-destructive and
idempotent under their required idempotency key. Status is read-only. Apply and
Undo are non-read-only and destructive. Every tool has `openWorldHint: false`.
Apply is unavailable without the S9-01 approval mechanism; tool annotations
are UX metadata, not the receipt itself.

Preparation results contain the opaque transaction ID, lifecycle state,
operation summary, affected entities, semantic preview, risk/approval scope,
preview digest, expiry, current revision vector, and applied limits. Status
contains no native history ID or retained mutation payload. Raw scene/resource/
script file paths are never accepted as arbitrary write destinations.

**Tests:** exact 36-tool registry; annotations; closed inputs; required project,
editor, scene, revision, and idempotency coordinates; safe result/error
projection; unsupported approval host; stale/tampered transaction/digest;
wrong project; offline/read-only/incompatible states; output bounds; no raw
write tool; and all 25 Sprint 8 tools unchanged.

**Done when:** Codex can prepare, inspect, explicitly approve, apply, query, and
Undo a basic editor transaction using only MCP, while no public tool can bypass
the transaction coordinator.

### S9-10 — Freeze the independent transaction fixture and fault oracle

**Depends on:** S9-03–S9-09.

**Status:** Complete. The closed manifest, independent golden oracle, bounded
fixture, deterministic fault markers, safe projections, source fingerprinting,
and direct Bridge regression runners are implemented and locally verified.

**Changes:** hashed fixture manifest; saved open scene with owned, instanced,
inherited, locked, transform, script, signal, property, and bounded-large
cases; deterministic editor commands for external native mutation, standard
Undo/Redo, scene close/reopen, and fault points; golden pre/apply/Undo truth;
strict independent validator; source-hash oracle. The oracle imports no
production transaction, preview, executor, journal, or merge code.

**Fault points:** after prepare; after approval but before final preflight;
after native action creation but before commit; immediately before commit;
immediately after commit but before Bridge response; after Bridge response but
before sidecar journal acknowledgement; sidecar disconnect/restart while
prepared and after commit. Faults are deterministic and disabled outside the
fixture/test profile.

**Tests:** closed sorted manifest and hashes; strict JSON; wrong preview/digest/
inverse/history/revision truth; missing operation/fault case; weakened limits;
false no-mutation or full-Undo claims; secret/path/native-ID scan; independent
recalculation of scene topology, property/script/signal state, revisions,
history, and unchanged project-content bytes.

**Done when:** the oracle independently detects stale apply, approval bypass,
duplicate apply, half-applied state, incorrect Undo, source-file writes, and
unsafe journal/output content.

### S9-11 — Run the model-free editor transaction workflow

**Depends on:** S9-10.

**Status:** Complete. The deterministic MCP client exercises all eight
operation families through real form elicitation, targeted and native
Undo/Redo, opaque intervening actions, approval/idempotency negatives, and the
seven-scenario crash/disconnect matrix. Its report contains only bounded
observations and hashed transaction identities.

**Changes:** temporary fixture copy and real editor/Bridge/sidecar workflow for
each operation family. The runner records prepared and committed transaction
IDs, previews/digests, revision/history transitions, fault results, readback,
native and MCP Undo, and cleanup without recording full sensitive values.

**Workflow:** read scene/history/revisions; prepare and prove no mutation;
exercise stale rejection; obtain the real local approval result; apply;
read back through Sprint 7 scene/node/history tools; reconcile status; Undo;
verify exact golden state and source hashes; exercise native Undo/Redo and an
opaque intervening action; execute the response-loss/in-doubt matrix through
the same editor and Bridge.

**Tests:** all basic operations, wrong coordinates, expiry, duplicate keys/
apply, unsupported locked nodes, journal restart, source-byte equality,
process/temp cleanup, redaction, and continued responsiveness of Sprint 7/8
read/runtime tools.

**Done when:** no scenario needs a raw file edit, implicit save, editor/Bridge
restart, unrelated Undo, or manual repair of fixture state.

### S9-12 — Qualify macOS arm64 and close the sprint

**Depends on:** all implementation and gate commits complete.

**Status:** Complete. Qualifying source commit
`38e33a3a2d0bf1b533a378a7ee9fa429f9a4771f` passed every local gate. Evidence
was published alone in commit
`0255da9686d5be8c1362e2f2d2c33eaf54b8a3c2` and passed post-commit validation.

**Changes:** no production implementation changes. The source-bound acceptance
wrapper published only
`tests/codex/evidence/sprint-9-editor-transactions-macos.json`.

**Tests:** fixture/oracle and Python policy regressions; Rust fmt, full
workspace tests, and deny-warning Clippy; Bridge schema conformance; tests-
enabled Godot editor build; focused `*CodexS9*` C++ profiles; release sidecar;
model-free transaction workflow; Sprint 7/8 regression profiles; journal
permissions/cleanup/redaction; artifact/source hashes.

**Done when:** immutable evidence validates against the current clean source
coordinate, every acceptance transaction has a unique ID and complete Undo
proof, no source-content hash changed, and external gates remain honestly
`not_run`. This condition is satisfied.

## 3. Transaction contract

### 3.1. Lifecycle

The public S9 lifecycle is:

```text
preparing → previewed → awaiting_approval → applying → applied
                                                │          ↓
                                                │      validating → committed → undone
                                                │             └→ failed_rolled_back
                                                └→ in_doubt → status reconciliation

previewed/awaiting_approval → conflicted | expired | rejected
preparing/applying          → failed (only when no native action committed)
```

`in_doubt` is not permission to retry. It is a recoverable observation state.
Every transition has one monotonic transaction sequence and advances the
editor `operation_seq` only where WRITE-001 explicitly requires it. Scene
revision changes only for an actual mutation, native Undo, or native Redo.

S9 `validating` proves only intrinsic editor postconditions. Broader semantic,
diagnostic, test, and runtime validation is added in Sprint 10 without changing
the meaning of an already committed S9 action.

### 3.2. Identity and preconditions

Every prepared transaction binds:

- `project_id` and canonical project binding;
- `editor_session_id`;
- opaque scene entity ID and exact `scene_revision`;
- opaque native-history entity ID and observed `operation_seq`;
- operation-specific source/target editor node IDs;
- canonical operation digest and caller idempotency key;
- preview digest, risk class, approval scope, nonce, and expiry;
- hard limits and affected-entity hints.

`transaction_id` is an opaque 128-bit random identity rendered as
`transaction:<32 lowercase hex>`. It is never derived from a node path,
property value, account, PID, pointer, or native history ID. A transaction from
another project/editor session is not stale-but-usable; it is rejected.

### 3.3. Operation boundary

| Operation | Required preflight | Exact inverse |
|---|---|---|
| create node | parent editable, type instantiable, name/owner valid | remove the exact created node/reference |
| delete node | non-root, editable ownership, bounded subtree | restore node, parent, index, owner, transform |
| reparent node | same scene, editable source/target, no cycle | restore parent, index, owner, transform |
| set property | property exists/writable, safe type/value | restore canonical old Variant |
| attach script | existing project script, compatible base | restore prior script reference |
| detach script | current script matches preview | restore detached script reference |
| connect signal | declared signal/target method, no duplicate | disconnect exact connection |
| disconnect signal | exact connection exists | reconnect exact flags/binds |

Preflight validates the whole semantic operation before `create_action`.
Registration faults before `commit_action` leave no mutation. The executor
uses native references for object lifetime but never exports them across the
main-thread or protocol boundary.

### 3.4. Preview and approval

Preview is both human-readable and machine-checkable. It includes operation
kind, bounded before/after semantic summary, affected entities, dirty/save
effect, risk, preconditions, validation plan, expiry, and SHA-256 digest. It
does not include an absolute path, raw ID, entire deleted subtree, source text,
or unrestricted property value.

Approval is exact-scope and single-use. The receipt must bind transaction ID,
preview digest, project/editor session, approval scope, issued/expiry times,
and nonce. Bridge verifies the opaque sidecar proof but does not learn an
OpenAI account or accept human prose. Re-previewing, changing the operation,
changing revisions, or expiring the receipt requires new approval.

### 3.5. Initial hard limits

S9-01 may lower these values after fixture calibration but must not raise them
without contract and named-negative updates:

| Limit | Initial hard maximum |
|---|---:|
| Semantic operations per transaction | 1 |
| Prepared transactions per project | 64 |
| Concurrent applying transactions per scene history | 1 |
| Prepared/approval lifetime | 300 seconds |
| Encoded preview | 65,536 bytes |
| Encoded operation payload | 65,536 bytes |
| Safe Variant depth/items/string | 8 / 1,000 / 16,384 characters |
| Structural subtree preflight | 1,000 nodes |
| Retained journal records | 1,024 |
| Retained journal bytes | 8 MiB |
| Transaction status result | 65,536 bytes |
| Apply/Undo request deadline | 5,000 ms |

Preflight may be time-sliced. Native commit is admitted only after bounds are
known; it is never split across frames in a way that exposes partial state.

### 3.6. Structured errors

WRITE-001 freezes exact retryability and safe current coordinates for at least:

- `transaction_not_found`;
- `transaction_expired`;
- `transaction_conflicted`;
- `transaction_busy`;
- `transaction_too_large`;
- `transaction_in_doubt`;
- `transaction_not_undoable`;
- `idempotency_conflict`;
- `preview_mismatch`;
- `approval_required`;
- `approval_invalid`;
- `approval_scope_mismatch`;
- `stale_editor_state` and `stale_scene_revision`;
- `scene_not_open` and `scene_operation_unsupported`;
- `node_not_editable` and `node_ownership_invalid`;
- `property_not_writable` and `property_value_unsupported`;
- `script_incompatible`;
- `signal_connection_invalid`;
- `transaction_apply_failed` and `transaction_undo_failed`.

Errors return only opaque IDs, bounded semantic reasons, and safe current
revision/status coordinates. They never echo approval proofs, unrestricted
values, native IDs, absolute paths, or raw journal records.

## 4. Commit boundaries

| Boundary | Allowed content | Required gate before commit |
|---|---|---|
| `C9-1 contracts` | S9-01/02 plan, WRITE/protocol/MCP docs, schemas, vectors, profile tests | schema conformance + `CodexS9BridgeProfile` |
| `C9-2 coordinator` | S9-03/04 editor transaction lifecycle, guards, approval verification, status/Undo core | tests-enabled editor build + `CodexS9TransactionLifecycle` |
| `C9-3 node/property executor` | S9-05/06 structural and property operations with focused tests | `CodexS9TransactionExecutor` + source/hash oracle |
| `C9-4 script/signal executor` | S9-07 script and signal operations with focused tests | `CodexS9TransactionBindings` + source/hash oracle |
| `C9-5 sidecar/MCP` | S9-08/09 Rust coordinator, journal, approval route, tools | fmt + full workspace tests + clippy |
| `C9-6 fixture/gate` | S9-10/11 fixture, oracle, runners, policy tests, source scope | Python regressions + model-free live workflow |
| `C9-7 evidence` | S9-12 evidence JSON only | `sprint9_acceptance.py --validate` |

If a qualifying run requires a source fix, its evidence is discarded. The fix
enters the correct implementation boundary, the source is committed, and the
complete gate restarts from a clean coordinate.

## 5. Fixture and local acceptance

Sprint 9 passes only when the independent oracle proves all of the following:

1. prepare returns a unique transaction, bounded preview, exact preconditions,
   digest, risk, approval scope, and expiry without changing scene, history,
   revisions, selection, or project-content bytes;
2. stale editor/scene/history coordinates and changed targets are rejected
   before native action creation;
3. apply cannot run without the proven local approval result and rejects wrong,
   expired, replayed, or scope-mismatched approval;
4. an idempotent replay never creates a second transaction/action, while a
   different request with the same key fails;
5. create, delete, reparent, and set-property each produce one native action,
   correct live-overlay evidence, dirty state, and monotonic revisions;
6. attach/detach script and connect/disconnect signal change only editor memory
   and preserve all fixture source-file hashes;
7. standard Godot Undo restores exact topology, owners, order, transforms,
   properties, script references, signal connections, history state, and
   revisions; native Redo reapplies the same known transaction;
8. targeted MCP Undo refuses when an unrelated native action is newer and
   never skips that action;
9. faults before commit leave the exact pre-state and no history action;
10. response loss after commit reconciles by transaction ID without a second
    apply, duplicate terminal result, or false rollback claim;
11. prepared sidecar restart changes no scene content; editor reset expires
    session-scoped prepared transactions;
12. deleted subtrees, property values, scripts, approval material, native IDs,
    and absolute paths do not leak into journal, logs, MCP errors, or evidence;
13. oversized subtrees/values/previews/journals fail within negotiated limits
    without an unbounded main-thread stall;
14. no Bridge/MCP method can raw-write or save an open scene, script, or
    resource;
15. two project bindings cannot prepare, query, approve, apply, or Undo each
    other's transactions;
16. all Bridge RPC 1.0–1.6, Sprint 7 live-editor, and Sprint 8 runtime
    regressions remain green.

The qualifying command on macOS arm64 is:

```sh
.venv/bin/python tests/codex/sprint9_acceptance.py --timeout 60
```

The native Windows x86_64 coordinate uses the same wrapper:

```powershell
python tests\codex\sprint9_acceptance.py --timeout 90
```

Windows-specific prerequisites, behavior, and evidence naming are documented
in [SPRINT-9-WINDOWS.md](SPRINT-9-WINDOWS.md).

The wrapper refuses a dirty Sprint 9 source scope or an existing evidence
path. It executes, in order:

1. strict fixture/oracle and named-negative policy regressions;
2. Rust formatting, full locked/offline workspace tests, and deny-warning
   Clippy;
3. Bridge schema/conformance bundle including all older minor profiles;
4. tests-enabled native Godot editor build for the local qualifying coordinate
   and focused `*CodexS9*` tests;
5. release sidecar build;
6. model-free prepare/no-mutation/stale/approval/apply/readback/Undo workflow;
7. deterministic pre/post-commit fault and reconnect matrix;
8. Sprint 7/8 regression profiles;
9. source-byte, journal-permission, cleanup, redaction, and process scans;
10. source/artifact SHA-256 binding and atomic evidence publication.

All implementation, fixture, runner, and validator changes are committed
before this command. The generated evidence is committed alone as
an evidence-only commit for the local coordinate, then revalidated with the
matching canonical evidence path. For macOS:

```sh
.venv/bin/python tests/codex/sprint9_acceptance.py \
  --validate tests/codex/evidence/sprint-9-editor-transactions-macos.json
```

The validator accepts `HEAD == source.commit` before the evidence commit and
`HEAD^ == source.commit` afterward only when the child commit changes exactly
that evidence file.

Initial SLOs are prepare/preview p95 at most 500 ms, transaction status p95 at
most 200 ms, apply and Undo confirmation/visibility p95 at most 2 seconds,
stale rejection before any mutation, and zero unhandled Bridge dispatcher
slices beyond 2,000 microseconds on the reference fixture. Correctness, full
Undo, and absence of source-file writes override latency: a timeout after the
commit point becomes `in_doubt`, never an automatic replay.

## 6. Explicitly deferred

Atomic multi-operation change sets, create/update resource, scene save,
script/source text patching, C# edits, subresource creation, cross-scene
transactions, closed-scene file mutation, arbitrary raw file writes, automatic
diagnostics/test/runtime validation, pre/post semantic graph comparison,
policy-driven rollback after broader validation, MCP redo, runtime-object
  mutation, persistent Undo across editor restart, Linux qualification, hosted
  CI, and product approval UI beyond the proven local acceptance path are
  outside Sprint 9.

Sprint 10 consumes the committed transaction foundation and Sprint 8 runtime
projection to add compound semantic changes, validation, and rollback policy.
