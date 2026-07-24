# WRITE-001 — Editor transactions, approval, and native Undo

**Status:** Sprint 9 single-operation and Sprint 10 compound extensions
implemented and source-bound locally on macOS arm64

**Contract version:** `1.0` for single-operation transactions; `2.0` compound
extension

**Wire version:** Bridge RPC `1.7` single-operation and `1.8` compound
profiles implemented

**MCP protocol:** server profile `2025-11-25`; form-compatible negotiated
fallback `2025-06-18`

**Parent documents:** [PRODUCT-001](PRODUCT-001-semantic-bridge-vision-and-plan.md),
[ARCHITECTURE-001](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md),
[EDITOR-001](EDITOR-001-live-editor-context.md),
[PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md), and
[MCP-001](MCP-001-project-scoped-read-tools.md), and
[VALIDATION-001](VALIDATION-001-automatic-validation-and-rollback.md)

## 1. Purpose and authority

This contract defines the first project-content writes available through the
Godot Bridge. A Sprint 9 transaction changes one saved scene that is open in
the bound editor session, uses Godot's native editor history, and exposes a
bounded preview and exact recovery status. It changes editor memory and dirty
state only. It does not save or raw-patch a project file.

Authority is deliberately split:

- the MCP host owns the user interaction and returns a standard form
  elicitation decision;
- the Rust sidecar owns model-facing preparation, approval routing, receipt
  creation, redaction, idempotency lookup, and the sanitized journal;
- the editor-only Bridge owns live object resolution, final preflight, receipt
  verification, native action construction, commit, postcondition checks, and
  Undo correlation;
- `EditorUndoRedoManager` is the only production mutation authority.

Tool annotations are advisory metadata. A capability token proves that the
sidecar may connect to the local Bridge, but neither one proves user approval.
The Bridge does not receive account identity, prompts, free-form statements of
consent, or an MCP client brand as authority.

## 2. Sprint 9 operation boundary

One transaction contains exactly one operation from this closed set:

| Operation | Approval scope | Required inverse |
|---|---|---|
| create node | `scene.node.create` | remove the exact created node |
| delete node | `scene.node.delete` | restore node, parent, index, owner, transform |
| reparent node | `scene.node.reparent` | restore parent, index, owner, transform |
| set property | `scene.property.set` | restore the canonical prior Variant |
| attach script | `scene.script.attach` | restore the prior script reference |
| detach script | `scene.script.detach` | restore the detached script reference |
| connect signal | `scene.signal.connect` | disconnect the exact connection |
| disconnect signal | `scene.signal.disconnect` | reconnect exact target, flags, and binds |

Targets MUST be saved scenes currently open in `editor_session_id`. Scene-root
replacement, cross-scene reparenting, runtime objects, closed-scene mutation,
raw file destinations, source text, built-in script creation, arbitrary
Callables, and unsupported native handles fail before action creation.

Risk is a closed `write|destructive` classification. Adding a node, a first
script attachment, or a new connection may be `write`. Removing, moving, or
replacing existing state, including property writes, is `destructive`.
`godot_apply_transaction` is conservatively annotated destructive for every
operation because MCP annotations cannot vary by prepared transaction.

Multi-operation changes, resource creation, save, source patching, validation
through diagnostics/tests/runtime, and policy rollback belong to the bounded
Sprint 10 version `2.0` extension in §16 and VALIDATION-001.

## 3. Identity, revisions, and idempotency

Every transaction binds all of these coordinates:

- canonical `project_id` and the authenticated project binding;
- `editor_session_id`;
- opaque `scene_id`, exact `scene_revision`, and opaque `history_id`;
- exact observed `operation_seq`;
- operation-specific live node IDs and safe resource identities;
- caller `idempotency_key` and canonical operation digest;
- immutable `preview_digest`, approval scope, risk, and expiry;
- the hard limits applied during preflight.

`transaction_id` is a random 128-bit value rendered as
`transaction:<32 lowercase hex>`. It is not derived from a path, PID, pointer,
ObjectID, property value, account, or request ID. It survives an individual
Bridge connection but never an editor-session or project boundary.

Every accepted prepare attempt gets a fresh transaction ID. A replay of the
same idempotency key and byte-identical canonical operation returns the same
prepared transaction. Reusing that key for different content is
`idempotency_conflict`. Request IDs are transport correlation only and never
replace transaction or idempotency IDs.

Prepare does not advance any editor revision. A committed native action, native
Undo, and native Redo advance the scene and history coordinates according to
EDITOR-001. `transaction_seq` starts at 1 and advances exactly once for each
public transaction state transition.

## 4. Lifecycle

The public state machine is:

```text
preparing → previewed → awaiting_approval → applying
          → applied → validating → committed → undone

previewed/awaiting_approval → conflicted | expired | rejected
preparing/applying          → failed, only before commit entry
applying/validating         → failed_rolled_back | in_doubt
in_doubt                    → committed | undone | failed_rolled_back
undone                      → committed, only after native Redo
```

State meaning is fixed:

- `preparing`: bounded, read-only main-thread preflight is running;
- `previewed`: immutable preview exists and no approval request is active;
- `awaiting_approval`: the plan is eligible for a new explicit confirmation;
- `applying`: final guards are running or native commit has started;
- `applied`: `commit_action()` returned and the native history advanced;
- `validating`: intrinsic editor postconditions are being checked;
- `committed`: requested postcondition and history correlation are proven;
- `undone`: exact native Undo is observed and the inverse state is proven;
- `conflicted`: a bound target or revision changed before commit;
- `expired`: the prepared lifetime ended before commit;
- `rejected`: the user explicitly declined this prepared transaction;
- `failed`: the attempt failed while no native action could have committed;
- `failed_rolled_back`: a failed postcondition was followed by a proven exact
  top-of-history Undo;
- `in_doubt`: commit may have begun and the current result is not yet proven.

`in_doubt` is an observation and recovery state, never permission to retry.
Only `transaction.status` and native-history/revision reconciliation may move it
to a proven state.

Decline is terminal for the prepared transaction. Cancel, elicitation timeout,
or host disconnect leaves it in `awaiting_approval` until the plan expires; a
later attempt creates a new elicitation and a new nonce.

## 5. Prepare and preview

Prepare MUST NOT call `create_action`, mutate a Node or Resource, mark a scene
dirty, change selection, save a file, or advance editor/history revisions. It
may allocate one bounded prepared record and sanitized journal metadata.

Before producing a preview, the Bridge validates the complete operation:

- project/editor/scene/history coordinates are current;
- all referenced objects exist and belong to the expected scene;
- owner and editable inherited/instance boundaries permit the operation;
- operation-specific type, cycle, script, signal, and property rules pass;
- the inverse can be registered using native references;
- all payload, subtree, Variant, preview, and queue limits are known.

The preview contains operation kind, affected opaque entities, bounded
before/after semantic summaries, risk, approval scope, dirty/save effect,
preconditions, intrinsic validation plan, expiry, and explicit truncation. It
never contains an absolute path, full deleted subtree, unrestricted property
value, source text, receipt, session token, native history ID, or object handle.

The Bridge emits the exact strict UTF-8 `preview_payload_json` used for hashing.
It has recursively sorted object keys and no insignificant whitespace. The
sidecar hashes those exact bytes without reserialization:

```text
preview_digest = "sha256:" || lower_hex(SHA256(preview_payload_json_bytes))
```

The sidecar may shape a human view but cannot alter the machine preview or
digest. Re-previewing after any change creates a new transaction rather than
silently refreshing the old plan.

## 6. Approval interaction

Sprint 9 requires one explicit confirmation for every transaction. Session and
persistent approval policies are not accepted.

When `godot_apply_transaction` is called, the sidecar first verifies that the
MCP client declared form elicitation. An absent capability returns
`approval_host_unsupported`; `clientInfo.name` or version is diagnostic only
and cannot enable writes.

The server advertises MCP `2025-11-25`. A client may negotiate
`2025-06-18`, the revision that introduced the same standard form
`elicitation/create` action/content contract used here. Such a downgrade is
eligible only when `elicitation.form` is explicitly present. Older revisions,
URL-only elicitation, and proprietary form extensions are unsupported for
approval. The local S9-01 Codex qualification records the exact negotiated
revision instead of inferring it from the server profile.

The Sprint 11 External Codex Beta host profile sends standard
`elicitation/create` form mode with an empty object schema:

```json
{
  "mode": "form",
  "message": "<bounded operation, scope, risk, affected entities and expiry>",
  "requestedSchema": {
    "type": "object",
    "properties": {}
  }
}
```

Only the host-owned `action: accept` with absent or exactly empty content is
approval-eligible. Any non-empty content is `approval_invalid`. `action:
decline`, `action: cancel`, timeout, and transport failure map to distinct
errors and never call apply. An empty form is intentional: Codex clients render
the three protocol actions as Allow/Deny/Cancel, so decline cannot be confused
with submitting a boolean field value.

Immutable Sprint 9 evidence used the earlier required `confirm: true` content
profile. That remains historical input evidence, not the External Beta host
binding. Moving to the action-only profile does not widen authority: the
message still carries the exact transaction ID, digest, scope, risk, and
bounded immutable preview, while only the interactive host can return the
accept action.

For an eligible low-risk memory-only change set, the request may additionally
advertise `_meta.persist: ["session"]`. A session grant is created only when
the host returns `action: accept` and host-owned response
`_meta.persist: "session"`. Missing, malformed, `always`, model input, or
request metadata alone never creates a grant.

The model-facing apply input contains only transaction ID, preview digest, and
expected revision coordinates. `approved`, approval receipt, arbitrary proof,
free-form user text, account identity, and capability token are forbidden
fields. A tool annotation or a repeated call is not approval.

## 7. Approval receipt

After an eligible elicitation result, the sidecar creates one receipt:

```json
{
  "kind": "mcp_form_v1",
  "scope": "scene.node.create",
  "nonce": "<43 unpadded base64url characters>",
  "issued_at_ms": 1784690000000,
  "expires_at_ms": 1784690030000,
  "mac": "<43 unpadded base64url characters>"
}
```

The 32-byte approval key is:

```text
approval_key = HMAC-SHA-256(
  bridge_session_token,
  UTF8("godot-codex/approval-key/v1")
)
```

The receipt MAC uses this exact canonical byte sequence:

```text
UTF8("godot-codex/approval-receipt/v1") || 0x00
|| LP(project_id)
|| LP(editor_session_id)
|| LP(scene_id)
|| LP(transaction_id)
|| LP(preview_digest)
|| LP(scope)
|| LP(risk)
|| U64BE(scene_revision)
|| U64BE(operation_seq)
|| U64BE(issued_at_ms)
|| U64BE(expires_at_ms)
|| NONCE32
```

`LP(value)` is `U32BE(byte_length(UTF8(value))) || UTF8(value)`. Integers are
unsigned and in the protocol-safe range. `NONCE32` is the raw decoding of the
unpadded base64url nonce. The wire MAC is unpadded base64url of
`HMAC-SHA-256(approval_key, canonical_bytes)`. S9 contract fixtures contain the
cross-language golden vector.

The Bridge verifies the MAC constant-time and then rechecks kind, scope, risk,
timestamps, project/editor/scene/transaction identities, digest, revisions,
prepared state, and nonce uniqueness. Maximum clock skew is two seconds. The
nonce is consumed before entry to the commit point; any reuse is
`approval_replayed`.

The receipt expires 30 seconds after acceptance. Expiry after the commit point
does not change a committed action. Approval material is memory-only and is
invalid after sidecar restart. The journal may retain kind, scope, decision
time, expiry, and a SHA-256 receipt hash, but never nonce, MAC, elicitation
content, account identity, or the session token.

This receipt proves that the authenticated local sidecar observed an eligible
response from its bound MCP peer. It does not prove user identity and does not
protect against a compromised process running as the same trusted OS account.

## 8. Apply and commit point

Immediately before native action construction, apply rechecks every bound
coordinate, object, operation precondition, limit, digest, approval field,
nonce, idempotency entry, and per-history concurrency guard. Any mismatch exits
without creating an action.

The executor registers all do methods/properties, undo methods/properties, and
required native references before commit. No do method may run during
registration. The commit point begins immediately before entering
`EditorUndoRedoManager::commit_action()`. Once entry begins:

- cancellation is ignored;
- the approval nonce remains consumed;
- apply is never retried automatically or by idempotency replay;
- loss of response produces `in_doubt` until status reconciliation.

A pre-commit failure proves zero mutation and may return a new-preview action.
After `commit_action()` returns, the Bridge verifies one new native action,
expected scene/history revisions, dirty state, and the operation-specific
postcondition. A failed postcondition may invoke only exact guarded
top-of-history Undo. `failed_rolled_back` is reported only after the complete
pre-state is proven; otherwise the transaction is `in_doubt`.

Sprint 9 intrinsic validation does not run diagnostics, tests, the game, or a
semantic graph comparison. Those checks cannot change the meaning of a
committed S9 action when added in Sprint 10.

## 9. Native history and targeted Undo

Each transaction owns exactly one native action in one exact
`EditorUndoRedoManager` history. Internal correlation may retain the native
history identifier and action version, but neither crosses Bridge RPC or MCP as
a raw handle.

`transaction.undo` and `godot_undo_transaction` are allowed only when:

- the transaction is `committed`;
- its editor session and history still exist;
- its action is the newest undoable action in that history;
- no unknown user/plugin/native action is above it;
- current revisions match the observed top-of-history state.

The Bridge calls native Undo for that history. It never executes a stored
manual inverse while bypassing native history and never skips an unrelated
action. Failure of any guard is `transaction_not_undoable` with safe current
coordinates.

Users may use standard Godot Undo/Redo. History events reconcile the known
transaction to `undone` or `committed`. Unknown actions remain opaque and still
advance `operation_seq` and applicable scene revisions.

## 10. Retention, reconnect, and recovery

Prepared records expire after 300 seconds. At most 64 prepared transactions
exist per project and only one transaction may be applying in one scene
history. Expiry or conflict removes inverse native references as soon as safe.

The generated project-private journal is a bounded recovery index, not scene
truth. It may contain transaction/state/sequence IDs, timestamps, operation
kind, scope/risk, revision coordinates, affected opaque IDs, digests, safe
errors, and receipt hash. It must not contain source text, full subtrees,
unrestricted Variant values, secrets, absolute paths, raw IDs, nonce, or MAC.

A sidecar reconnect may reconstruct status from Bridge and current native
history. It never reapplies a transaction because a response or journal
acknowledgement was lost. Editor-session replacement expires all prepared and
approval state. A retained committed record from an old connection remains
queryable only when the same editor session proves its history correlation.

## 11. Hard limits

| Limit | Maximum |
|---|---:|
| Semantic operations per transaction | 1 |
| Prepared transactions per project | 64 |
| Applying transactions per scene history | 1 |
| Prepared lifetime | 300 seconds |
| Form elicitation timeout | 120 seconds |
| Receipt lifetime after acceptance | 30 seconds |
| Clock skew | 2 seconds |
| Approval message | 8,192 bytes |
| Encoded preview | 65,536 bytes |
| Encoded operation payload | 65,536 bytes |
| Safe Variant depth/items/string | 8 / 1,000 / 16,384 characters |
| Structural subtree preflight | 1,000 nodes |
| Retained journal records | 1,024 |
| Retained journal bytes | 8 MiB |
| Transaction status result | 65,536 bytes |
| Apply/Undo request deadline | 5,000 ms |

Clients may request lower values. No request, environment setting, tool input,
or negotiated minor may raise these values without a contract revision and
named-negative fixture updates.

## 12. Structured errors

Every error contains a stable code, safe message, retryability, transaction
phase when known, opaque transaction ID when safe, and current revision/status
coordinates when available. It never echoes approval material, unrestricted
values, absolute paths, native IDs, or journal records.

| Code | Retry/action |
|---|---|
| `approval_required` | run a new explicit approval interaction |
| `approval_host_unsupported` | no retry until host capability changes |
| `approval_declined` | terminal; create a new transaction if desired |
| `approval_cancelled` | may ask again before prepared expiry |
| `approval_timeout` | may ask again before prepared expiry |
| `approval_invalid` | no apply; ask again with the fixed form |
| `approval_scope_mismatch` | create a new preview/approval |
| `approval_replayed` | never retry apply; query status |
| `preview_mismatch` | create a new preview |
| `transaction_not_found` | no blind recreation or apply |
| `transaction_expired` | prepare again from current state |
| `transaction_conflicted` | reread state and prepare again |
| `transaction_busy` | bounded retry only before commit |
| `transaction_too_large` | narrow the requested operation |
| `transaction_in_doubt` | status reconciliation only |
| `transaction_not_undoable` | do not skip unrelated native actions |
| `idempotency_conflict` | use a new key for different content |
| `stale_editor_state` | read current editor coordinates |
| `stale_scene_revision` | read current scene and re-prepare |
| `scene_not_open` | open the saved scene explicitly |
| `scene_operation_unsupported` | choose a supported semantic operation |
| `node_not_editable` | no mutation; choose an editable target |
| `node_ownership_invalid` | no mutation; fix ownership/target |
| `property_not_writable` | no mutation |
| `property_value_unsupported` | use the safe writable projection |
| `script_incompatible` | choose an existing compatible script |
| `signal_connection_invalid` | correct the declared same-scene connection |
| `transaction_apply_failed` | retry only when status proves pre-commit failure |
| `transaction_undo_failed` | inspect current history/status; no blind inverse |

## 13. MCP and Bridge mapping

Bridge RPC 1.7 specifies capability `transaction.scene_v1`, methods
`transaction.prepare`, `transaction.apply`, `transaction.status`, and
`transaction.undo`, plus `transaction.event`. Capability presence and write
readiness are separate. Sessions negotiated at 1.0–1.6 omit every transaction
method, capability, payload, limit, and event.

MCP exposes eight operation-specific preparation tools plus apply, status, and
Undo. Preparation is non-read-only because it allocates bounded state, but is
non-destructive and idempotent for one required key. Status is read-only. Apply
and Undo are non-read-only and destructive. Every tool is closed-world and
rejects additional properties.

S9-01 does not register these tools. S9-09 registers the eleven tools after the
executor/coordinator gates, making the production registry exactly 36 tools.
The test-only
`godot_s9_approval_probe` is a separate example binary and is never part of the
production server.

## 14. Security and threat decisions

| Threat | Required control |
|---|---|
| Model self-approval | no approval boolean/text/receipt in MCP input |
| Tool annotation treated as authority | mandatory nested form elicitation |
| Client without interaction support | capability check and fail closed |
| Digest/scope substitution | receipt MAC binds exact transaction fields |
| Replay | random nonce, bounded consumed-nonce set, status reconciliation |
| Stale editor target | final revision/object preflight before action creation |
| Cancellation during mutation | explicit commit point; ignore after entry |
| Lost response | `in_doubt`; query status; never replay apply |
| Unrelated action above transaction | targeted Undo refuses |
| Secret/value leakage | bounded semantic preview and sanitized journal |
| Raw scene/source modification | no file destination or save method |
| Third-party MCP client | same form capability and response checks as Codex |

The local OS account, Codex host, sidecar, and Bridge binary form the Sprint 9
computing trust boundary. Identity-bearing or independently authenticated
product approval UI is deferred to Sprint 13.

## 15. S9-01 acceptance

S9-01 is closed only when:

1. the test-only server proves form capability gating and a required boolean;
2. accept/true is the only receipt-eligible result;
3. false/missing, decline, cancel, timeout, and unsupported host fail closed;
4. the installed macOS arm64 Codex host produces a bounded safe protocol trace;
5. the receipt golden vector and all field-mutation negatives validate;
6. WRITE-001, PROTOCOL-001, MCP-001, and the Sprint 9 plan agree;
7. at the S9-01 boundary, the production registry remains exactly 25 and
   contains no apply tool;
8. Bridge RPC 1.0–1.6 and Sprint 7/8 regressions remain unchanged;
9. no project scene/resource/script bytes change during the approval probe.

No production apply surface may merge until this contract and approval path
remain green in S9-02 through S9-04.

## 16. Sprint 10 compound extension

WRITE-001 version `2.0` adds change sets without widening version `1.0`.
`transaction.scene_v1` remains a one-operation editor-memory transaction.
Negotiated Bridge RPC 1.8 capability `transaction.change_set_v1` owns ordered
multi-operation plans, explicit persistence, validation, and guarded rollback
as specified by VALIDATION-001.

The compound extension fixes these invariants:

- `1..16` operations, at most one saved open scene, and one anchor history;
- one immutable preview and one native `EditorUndoRedoManager` action;
- all do/undo steps are detached from child executors and retained by one
  compound context;
- save is an explicit approved scope, never an apply side effect;
- project files are staged and hash-guarded before native commit;
- required validation is asynchronous but mandatory before `committed`;
- automatic rollback is allowed only with newest-action and exact postimage
  guards;
- a missing, stale, truncated, disconnected, or timed-out required proof is
  inconclusive rather than successful;
- standard Undo/Redo and targeted Undo never skip unrelated work;
- response loss allows reconciliation only, never apply replay.

The Sprint 10 operation union adds allowlisted `.tres` create/update and
bounded existing `.gd` source edits. The exact resource classes/properties,
script-edit constraints, persistence/escrow boundary, report, confirmation
grant, rollback policies, errors, and limits are normative in VALIDATION-001.

## 17. Explicit deferrals

For WRITE-001 version `1.0`, atomic multi-operation transactions, scene save,
resource creation/update, script/source patching, runtime validation, semantic
graph comparison, policy rollback, and session confirmation remain outside
the contract. Version `2.0` admits only the bounded Sprint 10 forms in
VALIDATION-001. Runtime mutation, arbitrary file patches, verified user
identity, embedded Godot approval UI, persistent Undo across editor restart,
Windows/Linux qualification, and hosted CI remain deferred.
