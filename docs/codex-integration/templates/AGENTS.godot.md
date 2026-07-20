# Godot semantic context for Codex

This repository exposes read-only, project-scoped Godot context through MCP.
Treat the semantic index as saved-project evidence, not as permission to edit
Godot files or operate the editor.

## Context order

1. Read `godot://project/summary` before answering a project-wide question.
2. For a scene question, read `godot://scene/{scene_id}/summary`, then use
   `godot_get_scene_graph` or `godot_inspect_node` only when more detail is
   needed.
3. For impact, rename, dependency, or usage questions, resolve a canonical
   identity and call `godot_find_usages`.
4. Use `godot_search_symbols` to discover saved declarations and
   `godot_inspect_symbol` for their forward relations.

## Evidence rules

- Cite returned `evidence_id` values for semantic claims.
- Say whether a result is `exact`, `probable`, `dynamic`, partial, or
  unavailable. Never promote weaker evidence to exact.
- Preserve conflicts and multiple provenance records; do not choose an
  arbitrary winner or report duplicate facts as separate usages.
- A partial response covers only the current domains named by its revision
  vector. Do not describe an unavailable scene or script domain as empty.
- Do not infer identities from similar names or paths. Resolve the canonical
  resource, scene, node, symbol, or signal selector first.

## Safety and scope

- MCP resources and tools are read-only and bound to this Godot project.
- Do not use returned paths to read outside `res://`, discover credentials, or
  bypass project isolation.
- Do not expose Bridge endpoints, editor session tokens, absolute filesystem
  paths, raw imported payloads, or source text that the tools did not return.
- Ask before making project changes. A semantic answer alone grants no write,
  import, run, or editor-control authority.
