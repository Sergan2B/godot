# MCP-001 — Project-scoped Godot read tools

**Status:** Sprint 2/3/4 tools live-verified on macOS arm64 and Windows x86_64;
the Sprint 5 symbol tools, Sprint 6 find-usages/context surfaces, Sprint 7
sixteen-tool live-editor surface, Sprint 8 twenty-five-tool runtime surface,
and Sprint 9 thirty-six-tool guarded transaction surface pass their local
gates; the Sprint 10 forty-tool compound surface is proposed

**MCP protocol:** `2025-11-25`

**Server:** `godot-codex-mcp` `0.1.0`

## Purpose

This contract exposes model-facing projections of the live Godot editor and its
persistent resource/scene/script semantic index. The server is a project-scoped
stdio MCP process and remains read-only through Sprint 6.
It does not contain OpenAI credentials, call a model, mutate the project, or
return cached editor state as current after the bridge becomes unavailable.
Sprint 7 remains read-only and is governed by
[EDITOR-001](EDITOR-001-live-editor-context.md). Sprint 8 adds local game
process controls without adding project-content write tools and is governed by
[RUNTIME-001](RUNTIME-001-debugger-and-runtime-observation.md). Sprint 9
implements guarded editor transactions through
[WRITE-001](WRITE-001-editor-transactions-and-undo.md). Sprint 10 adds bounded
change sets and automatic validation through
[VALIDATION-001](VALIDATION-001-automatic-validation-and-rollback.md).

## Lifecycle and project binding

The binary is launched as:

```text
godot-codex-mcp --project-root <project-root>
```

The root is canonicalized before discovery. The server reads bridge discovery
only from `<project-root>/.godot/codex`, performs the Bridge RPC mutual
authentication flow, verifies the exact `project_id`, and activates a live
snapshot only after all chunks and checksums validate.

MCP initialization succeeds while Godot is offline so clients can inspect the
server status. Tool calls then return a structured retryable
`editor_state_unavailable` execution error with the replica status; they never
return an empty success or an unproven stale snapshot.

## Common result envelope

Every successful tool result returns an object with:

- `schema_version: "1.0"`;
- `project_id` and `editor_session_id`;
- `capabilities_used`;
- the Bridge RPC `revision_vector`;
- `status`, `partial_reasons`, and `diagnostics`;
- normalized `entities`, `facts`, and `evidence`;
- `limits_applied`.

Persistent-index tools use the equivalent immutable-generation envelope:
`project_id`, logical `schema_version`, `generation_id`, `index_revision`, relevant
resource/scene revisions, `freshness`, `status`, diagnostics, checkpoint, pagination,
and evidence. They never combine records from different generations.

The same object is returned as MCP `structuredContent` and as canonical JSON in
a text content block. When an output schema is advertised, the structured
content must validate against it.

## Tools

### `godot_get_editor_state`

No parameters. Returns bridge/sidecar readiness, the bound project and editor
session, current scene identity, dirty state, selection count, capabilities,
and the current revision vector.

### `godot_get_current_scene`

No parameters. Returns the current live scene and its bounded node projection.
Each node includes a session-scoped opaque ID, exact `NodePath`, Godot type,
owner path, and attached script path. It does not claim persistent node identity.

### `godot_get_selected_nodes`

No parameters. Returns the current editor selection. Selected node entities
include the current scene and dirty state plus bounded editor-visible properties
projected from the live object.
Every property fact carries `source: "live_editor_property"`,
`freshness: "current"`, and the applicable `scene_revision`.

### `godot_get_resource_dependencies` / `godot_find_resource_owners`

Accept one canonical `uid://` or normalized `res://` selector and return direct
forward/reverse edges from one current persistent resource generation.

### `godot_get_scene_graph`

Accepts one opaque scene ID, `uid://`, or normalized `res://` selector and returns a
deterministically paginated composed node-occurrence graph with canonical definitions,
origin scene, parent/owner, instance chain, groups, connections, and diagnostics.

### `godot_inspect_node`

Accepts exactly one opaque node ID, or `scene` plus a safe relative node-only
`node_path`. It returns effective property values and declaration provenance,
attached scripts/resources, groups, connections, and animation references.

### `godot_search_symbols`

Accepts a non-empty declaration name, `match` (`exact` or `prefix`), optional
language/kind/script filters, limit, and cursor. It returns only named persistent
declarations in deterministic order; parameters, locals, and lambdas are not in
the public search surface.

### `godot_inspect_symbol`

Accepts exactly one opaque `symbol_id`, or a script selector together with a
canonical qualified name. It returns declaration, signature/type/visibility,
owner, outgoing exact/dynamic relations, scene attachments, diagnostics, and
content-hash-bound source ranges. It deliberately keeps reverse lookup in the
dedicated unified tool below.

### `godot_find_usages`

Accepts exactly one canonical entity/resource/scene/node/script-symbol/signal
target plus optional source-kind, confidence, and project/scene/script scope
filters. It returns deterministic reverse facts from one pinned generation,
with complete evidence, conflicts, partial-domain reasons, bounded pagination,
and signed cursors that bind every selector and filter.

### Sprint 7 live editor tools

`godot_get_open_scenes`, `godot_get_inspector_state`,
`godot_get_open_scripts`, `godot_get_editor_history`,
`godot_get_diagnostics`, and `godot_get_viewport_state` expose the focused
read-only domains from `EDITOR-001`. Existing editor/scene/node/usage tools gain
an additive disk/editor/effective overlay. Optional expected session, event, and
scene-revision preconditions fail closed instead of returning stale content.

### Sprint 8 runtime tools

`godot_get_runtime_tree`, `godot_inspect_runtime_object`,
`godot_get_stack_trace`, and `godot_capture_viewport` expose the bounded
runtime projection. `godot_run_project`, `godot_run_current_scene`,
`godot_stop_project`, `godot_pause_project`, and `godot_continue_project`
control only the single local game owned by the bound editor.

`godot_get_diagnostics` accepts additive `scope: editor|runtime` and defaults
to `editor` for compatibility. `godot_get_editor_state` includes a bounded
runtime summary. Read tools accept optional expected runtime coordinates;
control, object, stack, and capture requests require the runtime session they
target. Runtime tree pages use signed cursors bound to the project, runtime
session/sequence, tree snapshot, filters, and page limit.

Runtime session IDs, object/stack IDs, event sequences, limits, and capture
dimensions are constrained in the closed input schemas before Bridge access.
Runtime errors project only a stable code, retryability, and validated current
runtime coordinates. Capture returns metadata in structured/text content and
the verified PNG bytes exactly once as MCP image content; base64url, callback
paths, and native handles are never part of the MCP result.

### Sprint 9 transaction tools

The production tools are
`godot_prepare_create_node`, `godot_prepare_delete_node`,
`godot_prepare_reparent_node`, `godot_prepare_set_property`,
`godot_prepare_attach_script`, `godot_prepare_detach_script`,
`godot_prepare_connect_signal`, `godot_prepare_disconnect_signal`,
`godot_apply_transaction`, `godot_get_transaction_status`, and
`godot_undo_transaction`.

Preparation inputs bind project, editor session, open scene, exact revisions,
one operation, and a required idempotency key. They never accept approval
material or a raw file destination. Preparation is non-read-only because it
allocates bounded state, but is non-destructive and idempotent for the same
canonical request. Status is read-only. Apply and Undo are non-read-only and
destructive. All eleven tools have `openWorldHint: false`.

`godot_apply_transaction` accepts transaction ID, preview digest, and expected
revision coordinates only. Inside the call, the sidecar requires client
`elicitation.form`, displays the immutable bounded preview/risk/scope, and
accepts only MCP action `accept` with required `confirm: true`. The sidecar
then sends an internal one-time receipt to Bridge. Receipt, nonce, MAC,
`approved`, free-form consent, and account identity are forbidden MCP fields.

The server profile remains MCP `2025-11-25`. Negotiated `2025-06-18` is an
allowed form-only compatibility floor because that revision introduced the
same standard action/content contract. In either revision, capability presence
is mandatory; client name/version and OpenAI-specific form extensions do not
substitute for `elicitation.form`.

If form elicitation is absent, apply returns `approval_host_unsupported` and
write readiness is false. Decline is terminal for the prepared transaction;
cancel or timeout may prompt again before plan expiry. Session/persistent
approval policies are deferred. The test-only `godot_s9_approval_probe` lives
in a separate example binary and is never part of this registry.

### Sprint 10 compound tools — proposed

Sprint 10 adds four closed tools after the compound executor, persistence,
validation, rollback, and confirmation-policy gates:

- `godot_prepare_change_set`;
- `godot_get_validation_report`;
- `godot_get_confirmation_policy`;
- `godot_reset_confirmation_policy`.

The registry becomes exactly forty tools only at S10-12. Existing apply,
status, Undo, and all thirty-six Sprint 9 tools retain their current schemas
and semantics.

Change-set preparation accepts the closed operation union, exact coordinates,
save/validation/rollback policies, and a required idempotency key. It allocates
bounded state but mutates no editor or project content. It never accepts an
approval receipt, confirmation grant, validation result, rollback proof,
absolute path, directory, glob, or raw file payload.

Validation report and confirmation-policy reads are read-only. Policy reset is
non-read-only and non-destructive; it can only remove a host-issued grant.
Apply confirmation remains nested form elicitation. The default is
`always_ask`; the only grant is a 15-minute project/editor-session-bound
`allow_low_risk_for_session` for memory-only low-risk sets. Persistence,
delete, source/resource content, runtime launch, elevated risk, or changed
scope always prompts again.

## Resources

`godot://project/summary` is the fixed 4096-byte conservative context resource.
`godot://editor/summary` is the fixed 4096-byte live editor context resource.
`godot://runtime/summary` is the fixed 4096-byte runtime lifecycle and
diagnostic summary resource. `godot://scene/{scene_id}/summary` is the
2048-byte scene template. All four return
canonical JSON with revision coordinates, truncation, and omitted counts. They
are read-only snapshots; subscriptions and list-change notifications remain
disabled.

## Security and limits

The implemented registry contains exactly thirty-six tools: Sprint 6 has ten,
Sprint 7 adds six, Sprint 8 adds nine, and Sprint 9 adds eleven guarded
transaction tools after the Bridge executor and approval path. Observation and
capture tools are annotated
read-only and non-destructive. Run, pause, and continue are non-read-only and
non-destructive. Stop is non-read-only and destructive. All tools reject
additional input properties.
Index query limits default to 50 and accept 1–200. Signed cursors expire after five
minutes and bind project, tool, normalized selector/filters, limit, generation,
index revision, resource revision, and the applicable scene/script revisions.
The MCP process writes protocol messages only
to stdout and diagnostics only to stderr. Ordinary logs must not include the
session token, proof, absolute project root, source text, prompts, or property
values.

Bridge limits apply before MCP shaping: 1 MiB ordinary message, 8 MiB hard
message maximum, 512 KiB snapshot chunks, Variant depth 8, and 1000 container
items. Inspector projection is limited to 256 KiB per selected node and 4 MiB
per snapshot; identity fields are limited to 1024 characters. Truncation is
explicit in `limits_applied` and diagnostics.

Runtime limits additionally cap tree nodes/depth at 10000/256, object results
at 512 properties and 256 KiB, diagnostics at 200 records and 256 KiB, stacks
at 64 records and 128 frames each, and screenshots at 1280x720 and 512 KiB.
Raw ObjectID, PID, RID, graphics handles, absolute paths, stack locals, and
screenshot temp paths are forbidden in model-facing output.

Sprint 9 approval limits are a 120-second form interaction, 30-second receipt
lifetime, 8 KiB approval message, and one pending elicitation per transaction.
Approval content, receipt bytes, session tokens, unrestricted property values,
and absolute paths are forbidden in MCP output, logs, journal, and evidence.

## Sprint 2 acceptance

With the smoke fixture open, the user selects `CharacterBody2D`, changes
`movement_speed` without saving, and asks an external Codex about the current
selection without supplying a path or value. The tool result must contain the
exact `Player` NodePath, `res://player.gd`, the unsaved value, dirty state,
live-editor evidence, and a current scene revision. A second unsaved change
must produce a larger revision and the new value.
