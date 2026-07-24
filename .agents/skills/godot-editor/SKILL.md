---
name: godot-editor
description: Inspect, diagnose, run, and make guarded editor-native changes in the exact Godot project exposed by the Godot Codex MCP server. Use for saved semantic questions, live editor or runtime state, Godot errors and stack traces, scene/resource/script changes, validation and Undo, offline-cache work, or connection/setup troubleshooting.
---

# Godot editor workflow

Use only the project-scoped `godot_editor` MCP server. Preserve returned
project, editor, runtime, revision, transaction, and evidence coordinates; do
not reconstruct opaque IDs, reuse them in another project, or switch to raw
file editing for an open scene.

## Establish authority

1. Call `godot_get_connection_status`.
2. If it returns a configuration, compatibility, binding, authentication, or
   cache error, follow its remediation. When asked to run local diagnostics,
   use the package-owned absolute launcher `"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" doctor --project-root <path> --json`;
   never substitute a PATH basename or checkout binary, and do not guess from
   process presence.
3. Read `godot://project/summary` for saved-project orientation.
4. Treat `offline_cached` as saved semantic authority only. Do not report
   editor/runtime state from a previous session.

Bind follow-up calls to the returned revisions when the tool accepts guards.
On a stale/conflict response, reread and reconsider; never weaken the guard.

If doctor returns `project_config_invalid` with
`repair_project_config`, do not hand-edit TOML. For a requested repair:

1. Preview with
   `"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" setup --repair --project-root <path> --dry-run --json`.
2. Inspect the redacted, expiring preview and retain its exact digest. Repair
   derives profile/guidance from a valid private setup receipt.
3. Apply only that digest with
   `"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" setup --apply-plan sha256:<digest>`
   after the user's normal approval.
4. Rerun the package-owned doctor command and restart only when reported.

Repair can touch only a missing/drifted receipt-owned `godot_editor` stanza
and refresh its receipt. It preserves unrelated valid TOML/comments and file
mode. Missing/invalid/foreign receipts, unowned tables, malformed TOML,
symlinks, oversized inputs, stale plans, or changed package identity must fail
closed and remain non-mutating.

## Choose the narrow workflow

- Saved structure/dependencies: use project or scene resources, then the
  resource, scene, symbol, or usage tool needed for the question.
- Live editor: read editor state before current scene, selection, Inspector,
  tabs, history, diagnostics, or viewport metadata.
- Runtime: start the current scene/project only when requested, retain the new
  `runtime_session_id`, inspect tree/object/diagnostic/stack evidence, capture
  a bounded viewport only when useful, then pause/continue/stop as requested.
- Offline: use only saved project/resource/scene/script/symbol/usages
  surfaces. Live, runtime, transaction, validation, policy, and Undo calls
  must fail closed.

For semantic claims, cite returned `evidence_id` values and preserve
`exact`/`probable`/`dynamic`/partial distinctions.

## Make a guarded change

Use this sequence:

1. Read a consistent current snapshot.
2. Prepare the smallest supported operation or compound change set.
3. Inspect the immutable preview, affected entities/files, risk, save scope,
   validation policy, preconditions, and digest. Prepare must not mutate.
4. Call apply only for the unchanged transaction/digest/revisions. Let the
   host request normal tool approval and the server request its exact semantic
   form confirmation. Never put approval in tool input or claim it from prose.
5. Query status after timeout/response loss; never replay apply after the
   commit point.
6. Read back affected facts and obtain the complete validation report.
7. Use targeted transaction Undo only when requested and verify the restored
   editor/file/index state. Respect intervening native history actions and
   `in_doubt`/rollback-blocked remediation.

Do not save unrelated files, mutate runtime instances, bypass supported
operations with shell/source patches, or treat a successful write response as
validation proof.

## Keep control layers distinct

- Project trust enables project config; it is not write approval.
- Codex sandbox/tool approval permits a tool call; it is not transaction
  confirmation.
- The form confirms one exact preview; it is not reusable authority.
- Validation proves postconditions; it is not approval.
- Native Undo/Redo state is authoritative; do not synthesize an inverse.

Never expose discovery endpoints, session tokens, proofs, approval receipts,
absolute private paths, native handles, unrestricted source/property values,
or data from another project.
