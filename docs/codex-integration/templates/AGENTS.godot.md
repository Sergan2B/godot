# Godot semantic context for Codex

This repository exposes project-scoped Godot context and guarded editor
transactions through MCP. Treat semantic evidence as context, never as
permission to mutate the editor or project.

## Context order

1. Call `godot_get_connection_status` before assuming that saved cache, live
   editor, runtime, or write state is available.
2. Read `godot://project/summary` before answering a project-wide question.
3. For a scene question, read `godot://scene/{scene_id}/summary`, then use
   `godot_get_scene_graph` or `godot_inspect_node` only when more detail is
   needed.
4. For impact, rename, dependency, or usage questions, resolve a canonical
   identity and call `godot_find_usages`.
5. Use `godot_search_symbols` to discover saved declarations and
   `godot_inspect_symbol` for their forward relations.
6. An offline verified cache can answer only the documented saved semantic
   queries; it cannot represent current editor or runtime state. Live,
   runtime, and write/transaction operations are unavailable offline.
7. For runtime diagnosis, start the project explicitly, bind all observations
   to the returned `runtime_session_id`, and stop it when the task is done.
8. For a change, use read → prepare → inspect immutable preview → user form
   approval → apply → validation/readback → targeted Undo when requested.

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

## Safety, approval, and scope

- Every MCP process is bound to this exact trusted Godot project. Never reuse
  identities, cursors, sessions, transactions, or reports across projects.
- Do not use returned paths to read outside `res://`, discover credentials, or
  bypass project isolation.
- Do not expose Bridge endpoints, editor session tokens, absolute filesystem
  paths, raw imported payloads, or source text that the tools did not return.
- Running, pausing, stopping, screenshots, and writes are separate operations;
  a read request does not authorize them.
- A write requires the host's normal tool approval and the transaction's exact
  form confirmation. Never synthesize approval, pass it as tool input, reuse a
  receipt, or weaken Codex sandbox settings.
- Prepare is non-destructive but allocates bounded sidecar state; it must not
  mutate editor or project content. Apply only the unchanged digest and
  revision guards that the user reviewed. If status is stale, expired,
  offline, `in_doubt`, or conflicted, stop and follow the returned remediation.
- After a timeout or response loss, query transaction status. Never replay
  apply after the commit point may have been reached.
- Validation success and native Undo are observable states, not assumptions.
  Verify readback/status, and do not save or patch source files outside the
  approved transaction save scope.

## Package diagnostics

When connection status requests doctor or setup remediation, use the
package-owned absolute launcher:

```sh
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  doctor --project-root <path> --json
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  setup --project-root <path> --dry-run --json
```

Never substitute a `PATH` basename or a checkout-relative binary. Setup
previews exact owned changes and defaults to no; apply only its unchanged
digest after normal approval.
