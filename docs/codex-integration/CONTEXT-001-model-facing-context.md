# CONTEXT-001 — model-facing Godot context

**Status:** Sprint 6 contract freeze candidate

**Version:** 1.0

## 1. Purpose

Model-facing context is a deterministic bounded projection of semantic facts.
It is not an alternate index, does not contain raw source, and always exposes
the revision and evidence coordinates from which it was built.

## 2. `godot_find_usages`

The tenth read-only MCP tool accepts one closed `target` variant:

- `entity_id` with one canonical opaque entity ID;
- `resource` with a `uid://` or normalized `res://` selector;
- `scene` with a UID, path, or canonical scene ID;
- `node_path` with scene selector and relative node-only NodePath;
- `script_symbol` with script selector and canonical qualified name;
- `signal` with scene selector, emitter NodePath, and bounded signal name.

Optional filters are `source_kinds`, `confidence`, and a tagged scope:
`project`, `scene`, or `script`. Limits default to 50 and accept 1–200. Cursor,
target, filter, and scope fields reject unknown members.

The response contains project/generation/revision coordinates, normalized target,
ordered usages, evidence, conflicts, partial reasons, `total_matches`, truncation,
and a signed `next_cursor`. Results never infer an exact target from text.

## 3. Stable MCP resources

`godot://project/summary` returns project readiness, revision vector, domain
coverage, entity counts, important scenes/resources/scripts, diagnostics,
high-degree relations, evidence IDs, truncation, and omitted counts.

`godot://scene/{scene_id}/summary` returns scene identity and inheritance, root
structure, instances, attached scripts, signals, groups, dependencies,
diagnostics, evidence IDs, truncation, and omitted counts. The scene ID is a
strict percent-encoded canonical identity; malformed or unknown URIs fail
without filesystem access.

Both resources return canonical JSON as `application/json`. Project summaries
use budget 4096 and scene summaries budget 2048. The method
`utf8_byte_upper_bound_v1` guarantees serialized UTF-8 bytes do not exceed the
declared token limit. Mandatory metadata is never dropped; optional arrays are
added in deterministic priority/order until the next complete item would exceed
the limit.

## 4. Semantic-first guidance

For project-wide comprehension, clients read the project summary first. For
scene questions they read the scene summary, then use inspection tools for
detail. For impact or usage questions they resolve an identity and call
`godot_find_usages`. Answers distinguish exact, probable, dynamic, partial, and
unavailable state and cite evidence IDs instead of inventing source claims.

The bundled Godot `AGENTS.md` template repeats these rules for Codex without
granting write authority or bypassing project-scoped MCP isolation.
