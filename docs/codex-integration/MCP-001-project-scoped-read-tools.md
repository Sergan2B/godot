# MCP-001 — Project-scoped Godot read tools

**Status:** Implemented and live-verified on Windows x86_64 for Sprint 2

**MCP protocol:** `2025-11-25`

**Server:** `godot-codex-mcp` `0.1.0`

## Purpose

This contract exposes the first model-facing projection of a live Godot editor.
The server is a project-scoped stdio MCP process and is read-only in Sprint 2.
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

## Security and limits

All three tools are annotated read-only. Tool input is an empty object with
additional properties rejected. The MCP process writes protocol messages only
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
