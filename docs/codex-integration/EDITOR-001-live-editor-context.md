# EDITOR-001 — Live editor context and native history

**Status:** Approved Sprint 7 contract

**Contract version:** `1.0`

**Wire version:** Bridge RPC `1.5`

**Parent documents:** [PRODUCT-001](PRODUCT-001-semantic-bridge-vision-and-plan.md),
[ARCHITECTURE-001](ARCHITECTURE-001-bridge-sidecar-index-and-evidence-plan.md),
[PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md), and
[MCP-001](MCP-001-project-scoped-read-tools.md)

## 1. Authority and lifetime

The persistent index describes saved disk state. A validated editor snapshot
describes the current bound `editor_session_id`. For a field included in a
complete live coverage domain, editor state is authoritative; the corresponding
disk value remains available as comparison evidence. An absent live field does
not erase disk state unless the snapshot explicitly provides a tombstone from a
complete domain.

Live entities and snapshots are never written to the persistent segment store.
They are retired on scene close and discarded on disconnect, event gap, project
invalidation, or editor-session change. A client MUST NOT label retained data
`current` after any such boundary.

## 2. Common envelope

Every live editor response carries:

```json
{
  "schema_version": "editor/1.0",
  "project_id": "project:sha256:...",
  "editor_session_id": "editor:...",
  "snapshot_id": "snapshot:...",
  "revision_vector": {
    "event_seq": 42,
    "project_revision": 7,
    "operation_seq": 3,
    "resource_revision": 5,
    "scene_graph_revision": 6,
    "script_graph_revision": 4,
    "scene_revisions": { "scene:...": 2 }
  },
  "coverage": [],
  "partial_reasons": [],
  "evidence": [],
  "diagnostics": [],
  "limits_applied": {}
}
```

The normalized value projection distinguishes `disk_state`, `editor_state`,
and `effective_state`. `effective_state.source` identifies the winning source;
dirty editor data wins only for covered fields. A difference is reported as a
`disk_live_conflict` diagnostic and never hidden by the merge.

## 3. Identity

`live_scene_id`, `live_node_id`, `live_object_id`, `live_script_id`, and
`history_id` are opaque and stable only inside one editor session. Saved
entities also carry an optional canonical `disk_entity_id`. Unsaved entities
have no disk ID. Raw Godot `ObjectID`, pointer, RID, texture handle, endpoint,
or capability token MUST NOT cross the Bridge or MCP boundary.

Selection ordering is canonicalized by live identity, not pointer or UI
enumeration order. Each selected node retains its own identity and reports
whether it is the primary Inspector object.

## 4. Revisions and stale reads

- `event_seq` advances for every externally visible editor-context transition.
- `project_revision` advances for an accepted disk/editor model mutation or
  affected reimport.
- A `scene_revision` advances for structural/property changes, save, native
  action, Undo, or Redo associated with that live scene.
- `operation_seq` advances exactly once for each observed native history
  transition, including an opaque/unknown transition.
- Pure tab, selection, Inspector, script-focus/selection, or viewport changes
  advance only `event_seq`.

Every live tool accepts optional `expected_editor_session_id` and
`expected_event_seq`; scene reads additionally accept
`expected_scene_revision`. A mismatch returns `stale_editor_state` or
`stale_scene_revision`, `retryable: true`, and current coordinates without
returning the requested stale content.

## 5. Domain entities

An open scene reports its live/disk IDs, tab index, title, normalized path,
current and dirty flags, history ID, scene revision, root identity, and
coverage. A complete scene graph contains at most 1000 live nodes. An absent
node becomes a tombstone only when the scene domain is complete.

Selection contains at most 256 nodes. Inspector state contains one Node,
Resource, or opaque Object and at most 512 editor-visible properties with type,
editable/read-only and revert metadata, projected value, and optional disk
comparison.

Open scripts contain at most 256 tabs and report live/disk identity, normalized
path or built-in identity, active and dirty state, disk/editor SHA-256, and at
most 32 selections. Public line and column coordinates are one-based and ranges
are end-exclusive. Source text is not part of this contract.

A native history summary reports opaque `history_id`, optional live scene,
`action_name`, `saved_state` (`saved`, `unsaved`, or `unknown`), `can_undo`,
`can_redo`, `transition_kind` (`commit`, `undo`, `redo`, `clear`, or `unknown`),
and `last_operation_seq`. Native payload is always opaque. Unknown plugin
operations receive no inferred target, value, or other fabricated detail.

Diagnostics expose at most the newest 200 records, 16384 UTF-8 bytes per
message, and 262144 bytes per snapshot. Records carry severity, source, repeat
count, output sequence, redaction status, and omitted counts. Runtime meaning,
stack frames, and runtime entities are not inferred from Output text.

Viewport state exposes only kind, active/visible flags, logical/render size,
scale, and available camera/projection metadata. Screenshot capability is
reported as unavailable and deferred to Sprint 8.

## 6. Bounded Variant projection

Projection limits are depth 8, 1000 entries per container, 16384 characters per
string, and 65536 encoded bytes per value. The first compound value has a
snapshot-local `reference_id`; repeat visits return a reference and do not
recurse. Dictionary ordering is deterministic. Resources become safe UID/path
references and unsupported Objects become typed opaque values.

Depth, count, byte, unsupported-type, and getter failures produce explicit
`truncated`/`omitted_reason` results and diagnostics while the containing
snapshot continues. A cheap `size_hint` may be emitted, but projection MUST NOT
perform an otherwise forbidden traversal merely to calculate it.

## 7. Overlay composition

The sidecar maintains one immutable live snapshot per bound session. Scene,
node, and usage queries compose this snapshot with one immutable persistent
generation. They MUST NOT combine snapshots or disk generations across
revision coordinates.

Complete live facts suppress their exact superseded disk facts while retaining
disk evidence and comparison values. Partial or truncated coverage never
suppresses an unknown disk fact and never creates an absence tombstone. Dirty
scripts mark saved symbol results `may_be_stale_for_editor`; Sprint 7 does not
parse the unsaved buffer.

## 8. Lifecycle and recovery

Scene close retires that scene before the next successful read. Reimport
invalidates affected bindings and requires a fresh disk/live composition.
Project switch invalidates discovery and the entire live overlay; the sidecar
does not silently rebind to another project. Event gaps and reconnects require
a checksum-verified full snapshot before live state becomes current again.

## 9. Security and performance

All surfaces are read-only, project/session bound, bounded, paginated where
applicable, and redacted before model-facing output. Absolute project roots,
transport endpoints, tokens, proofs, raw IDs, source text, and pixel data are
excluded.

Current scene/selection reads have p95 at most 500 ms, editor changes become
query-visible within 2 seconds p95, control ping remains at most 200 ms p95, and
adapter work yields within the 2000 microsecond per-frame dispatcher budget.
