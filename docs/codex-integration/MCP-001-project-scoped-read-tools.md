# MCP-001 — Project-scoped Godot read tools

**Status:** Sprint 2/3/4 tools live-verified on macOS arm64 and Windows x86_64;
Sprint 5 symbol tools locally verified through `S5-08`

**MCP protocol:** `2025-11-25`

**Server:** `godot-codex-mcp` `0.1.0`

## Purpose

This contract exposes model-facing projections of the live Godot editor and its
persistent resource/scene/script semantic index. The server is a project-scoped
stdio MCP process and remains read-only through Sprint 5.
It does not contain OpenAI credentials, call a model, mutate the project, or
return cached editor state as current after the bridge becomes unavailable.

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
content-hash-bound source ranges. It deliberately omits general reverse usages,
which remain Sprint 6 scope.

## Security and limits

All nine tools are annotated read-only and reject additional input properties.
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

## Sprint 2 acceptance

With the smoke fixture open, the user selects `CharacterBody2D`, changes
`movement_speed` without saving, and asks an external Codex about the current
selection without supplying a path or value. The tool result must contain the
exact `Player` NodePath, `res://player.gd`, the unsaved value, dirty state,
live-editor evidence, and a current scene revision. A second unsaved change
must produce a larger revision and the new value.
