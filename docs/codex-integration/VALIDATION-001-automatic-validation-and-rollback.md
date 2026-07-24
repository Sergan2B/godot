# VALIDATION-001 — Compound validation, persistence, and guarded rollback

**Status:** Accepted and source-bound locally on macOS arm64 through the
Sprint 10 M3 evidence

**Contract version:** `1.0`

**Wire version:** Bridge RPC `1.8`

**MCP protocol:** `2025-11-25`

**Parent documents:** [WRITE-001](WRITE-001-editor-transactions-and-undo.md),
[PROTOCOL-001](PROTOCOL-001-bridge-rpc-v1.md),
[MCP-001](MCP-001-project-scoped-read-tools.md),
[INDEX-001](INDEX-001-semantic-index-storage-and-migrations.md),
[EDITOR-001](EDITOR-001-live-editor-context.md), and
[RUNTIME-001](RUNTIME-001-debugger-and-runtime-observation.md)

## 1. Purpose and authority

This contract extends, but does not redefine, Sprint 9. A Sprint 10 change set
contains `1..16` ordered semantic operations, one immutable preview, one
host-owned confirmation decision, one native editor action, an explicit
persistence scope, an automatic validation plan, and one guarded rollback
policy.

Authority remains split:

- the MCP host owns confirmation and may issue one bounded session policy;
- the Rust sidecar owns planning coordination, the sanitized journal,
  validation orchestration, report storage, pagination, and reconciliation;
- the editor Bridge owns entity resolution, whole-set preflight, native action
  construction, project-file staging, persistence, reload, and exact rollback;
- the semantic index and runtime debugger supply revision-bound observations,
  never self-declared pass/fail authority;
- `EditorUndoRedoManager` remains the only native editor-history authority.

A boolean, prose result, tool annotation, model argument, cached observation,
or unbound report cannot approve, validate, commit, or roll back a change set.

## 2. Compatibility and capability boundary

Bridge RPC `1.8` adds capabilities `transaction.change_set_v1` and
`validation.automatic_v1`. Sessions negotiated at `1.0` through `1.7` omit
every 1.8-only capability, method, field, state, limit, error, and event.
Capability presence and readiness remain separate.

Sprint 9 capability `transaction.scene_v1` continues to mean exactly one
operation with its existing preview, approval, apply, status, and Undo
semantics. A 1.8 change set never appears as a widened 1.7 transaction.

The new Bridge methods are:

- `transaction.prepare_change_set`;
- `transaction.validation_complete`;
- `transaction.rollback`.

Existing `transaction.apply`, `transaction.status`, `transaction.undo`, and
`transaction.event` gain only negotiated 1.8 result/event projections. The
sidecar stores the complete report. The Bridge receives only the bound report
digest, summary, applied coordinates, validation lease, and rollback decision.

## 3. Change-set identity and operation boundary

Every prepared change set binds:

- project ID, editor session ID, negotiated minor, and capability set;
- transaction ID, required idempotency key, operation order, and dependency
  graph;
- at most one saved open scene and exactly one anchor history;
- exact scene, operation, project, resource, script, and index revisions;
- exact content hashes and UIDs where available;
- plan-local aliases for entities created by earlier operations;
- affected semantic closure seed and explicit `save_scope`;
- validation, diagnostics, runtime, rollback, and confirmation policies;
- applied limits, creation/expiry times, canonical preview bytes, and digest.

The closed operation union is the eight WRITE-001 operations plus:

- `create_resource`;
- `update_resource`;
- `update_gdscript_source`.

Resource-only change sets use the global history. A change set containing scene
operations uses that scene's history. Multi-scene and cross-history sets fail
before native action construction.

## 4. Resource allowlist

Create and update are limited to these exact built-in classes and properties:

| Class | Writable properties |
|---|---|
| `Gradient` | `interpolation_mode`, `interpolation_color_space`, `offsets`, `colors` |
| `Curve` | `min_domain`, `max_domain`, `min_value`, `max_value`, `bake_resolution` |
| `Curve2D` | `bake_interval` |
| `Curve3D` | `closed`, `bake_interval`, `up_vector_enabled` |
| `Animation` | `length`, `loop_mode`, `step` |
| `CanvasItemMaterial` | `blend_mode`, `light_mode`, `particles_animation`, `particles_anim_h_frames`, `particles_anim_v_frames`, `particles_anim_loop` |
| `StandardMaterial3D` | `render_priority`, `transparency`, `blend_mode`, `cull_mode`, `shading_mode`, `vertex_color_use_as_albedo`, `albedo_color`, `albedo_texture`, `metallic`, `metallic_specular`, `roughness`, `emission_enabled`, `emission`, `emission_energy_multiplier`, `emission_texture` |

Material texture values may reference only an existing indexed project-owned
`Texture2D` through its opaque resource identity and exact current hash. The
operation does not modify that texture. Internal properties, tracks, curve
point blobs, `next_pass`, subresource creation, dynamic shader parameters,
custom scripted Resources, and every unlisted property are rejected.

Create requires an unused normalized `res://*.tres` destination. Update
requires a current opaque identity and preserves path and UID. Binary `.res`,
imported/generated targets, `.godot`, `project.godot`, addons, symlinks,
external paths, overwrite, delete, rename, and move are forbidden.

## 5. Native compound atomicity

S10-01 must prove the following with a tests-enabled native spike before the
production compound executor is enabled:

1. registering the action mutates no editor state or project file;
2. one retained callback can preflight all scene/resource/script guards before
   its first do step;
3. all steps apply in dependency order under one anchor action;
4. a step failure runs already-prepared inverse steps in reverse order;
5. do/undo result is observable after `commit_action()` returns;
6. standard Undo and Redo invoke the same retained context;
7. a mismatched preimage/postimage performs zero compound work;
8. editor shutdown cannot publish a false committed or rolled-back result.

No child executor may call `commit_action()`. It contributes a detached,
preflighted step and exact inverse to the retained compound context.

The contract guarantees exact pre-state or post-state across Bridge/editor/
index-visible state and recoverable project files. It does not claim an
OS-wide atomic multi-file view for unrelated external readers.

## 6. Persistence and rollback escrow

Persistence is explicit. `save_scope` may contain only the already-open saved
scene and affected project-owned resources/scripts identified by opaque IDs.
Directories, globs, absolute paths, save-as destinations, and unrelated dirty
content are invalid.

Before the native commit point the Bridge:

1. resolves the complete scope;
2. serializes and validates every postimage into a private staging directory;
3. records typed relative targets and exact pre/post hashes;
4. captures exact preimages in a separate owner-only rollback escrow;
5. verifies roots, links, permissions, free space, sizes, and current hashes;
6. fsyncs the escrow manifest before persistence can become authorized.

Staging and escrow never appear in Bridge RPC, MCP, ordinary logs, the
sanitized transaction journal, or evidence. `save_scope: none` leaves all
project-content bytes unchanged.

An active escrow survives only the apply/validation/rollback recovery window.
After a proven committed or rolled-back result it is deleted. Corrupt or
inconsistent escrow is quarantined and produces `in_doubt`; it is never used
to start or replay a semantic operation.

## 7. Validation lifecycle and result precedence

The 1.8 lifecycle is:

```text
preparing → previewed → awaiting_approval → applying → applied
                                                   ↓
                 save_scope != none → persisting → reloading
                                                   ↓
                                               validating
                                            ↙             ↘
                           validation_passed          validation_failed
                                  ↓                         ↓
                             committed               rollback_pending
                                                          ↓
                                                    rolling_back
                                                   ↙            ↘
                                            rolled_back   rollback_blocked

uncertain post-commit boundary → in_doubt → reconciliation only
native Redo after rollback     → reapplied_validation_required
```

Skipped phases are explicit report records. Transaction sequence and report
sequence are monotonic and separate from editor/runtime/index revisions.

Required checks, in order, are:

1. intrinsic whole-set editor postconditions and exactly one native action;
2. explicit persistence result or proof that save scope was none;
3. script/resource reload and editor filesystem convergence where affected;
4. live overlay/index convergence to expected revisions;
5. bounded pre/post affected semantic graph comparison;
6. diagnostics delta.

Optional runtime validation runs the current scene or project through the
Sprint 8 contract and always creates a fresh runtime session.

A required check is `inconclusive`, never `passed`, when it is skipped, stale,
truncated beyond its proof budget, disconnected, timed out, or unable to reach
the expected authoritative revision. New errors fail. New warnings are
reported by default and fail only under `fail_on_new_warning`. Unchanged
pre-existing findings do not fail the transaction.

Result precedence is:

1. `in_doubt` for uncertain native or file outcome;
2. `rollback_blocked` or `rollback_in_doubt` for an unproven requested restore;
3. `failed_rolled_back` only after exact baseline proof;
4. `validation_failed` for a proven required failure without rollback;
5. `validation_inconclusive` for missing required proof;
6. `committed` only when every required check passed.

## 8. Validation report

The immutable report binds:

- transaction ID, report ID, preview digest, and report digest;
- validation policy and exact baseline/post revision vector;
- affected opaque entities and typed relative file labels;
- per-check state, timing, evidence IDs, bounds, and truncation;
- persistence, reload, semantic delta, diagnostic delta, and runtime summary;
- rollback outcome and safe remediation.

Reports contain no unrestricted source, preimage/postimage bytes, approval or
policy grant, absolute root, native handles, stack locals, or escrow content.
Pages are deterministic, signed to transaction/report/revision coordinates,
and replay-safe.

## 9. Rollback, Undo, Redo, and recovery

Rollback policies are `never`, `on_required_failure`, and `on_any_failure`.
The default is `on_required_failure`.

Automatic rollback may start only after a bound validation result and only
when:

- the transaction owns the newest native action;
- every editor postcondition still matches;
- every persisted postimage hash still matches;
- the exact retained inverse and valid escrow are available.

Rollback restores memory and files, reloads affected content, waits for index
convergence, and compares against the baseline. `rolled_back` and
`failed_rolled_back` require all proofs. An unrelated action or external file
change returns `rollback_blocked` without modifying foreign work.

Response loss never authorizes replay. Recovery may finish an already
authorized exact persistence or restore step when its durable marker and
hashes determine one outcome. Otherwise it returns `in_doubt`.

Native Redo after automatic rollback becomes
`reapplied_validation_required`; no old save or validation proof is reused.
Targeted MCP Undo retains the Sprint 9 newest-action guard.

## 10. Confirmation policy

`always_ask` is the default and requires a new exact form confirmation.
The host may offer `allow_low_risk_for_session` for at most 15 minutes, bound
to the exact project and editor session.

The grant covers only low-risk memory-only change sets. Delete, resource or
source content, persistence, runtime launch, elevated risk, changed preview,
expanded scope, session replacement, and expired policy always require a new
transaction-specific confirmation.

The model cannot supply or weaken confirmation policy, approval receipt,
validation result, rollback proof, or grant material. Grants do not cross MCP
as ordinary input/output and are not written to the sanitized journal.

## 11. Hard limits

| Limit | Maximum |
|---|---:|
| Operations per change set | 16 |
| Open scenes / anchor histories | 1 / 1 |
| Persisted files | 8 |
| Created resources | 4 |
| Script range edits | 64 |
| Encoded request / preview | 262,144 bytes each |
| Replacement / resulting script | 65,536 / 524,288 bytes |
| Serialized scene/resource postimage | 2 MiB each |
| Durable rollback escrow | 8 MiB |
| Affected entities / facts | 2,000 / 10,000 |
| Diagnostic records | 500 |
| Report page / pages / retained total | 65,536 bytes / 4 / 262,144 bytes |
| Required non-runtime validation | 30 seconds |
| Runtime observation | 30 seconds |
| Total validation / rollback | 60 seconds each |
| Session confirmation grant | 15 minutes |
| Active persistence lease | 1 per project |

Sprint 9 Variant, journal, request, approval, dispatcher, and screenshot limits
remain ceilings. Raising a limit requires a contract revision and named
negative fixtures.

## 12. Structured errors

The stable error set includes:

- `change_set_invalid`, `change_set_dependency_cycle`,
  `change_set_conflict`, `change_set_too_large`,
  `change_set_history_mismatch`;
- `resource_operation_unsupported`, `resource_path_conflict`,
  `resource_uid_conflict`;
- `script_source_conflict`, `script_edit_invalid`;
- `persistence_scope_invalid`, `persistence_preflight_failed`,
  `persistence_failed`, `persistence_in_doubt`;
- `reload_failed`, `index_convergence_timeout`;
- `validation_failed`, `validation_inconclusive`, `validation_timed_out`,
  `validation_report_not_found`, `validation_report_mismatch`;
- `rollback_required`, `rollback_blocked`, `rollback_failed`,
  `rollback_in_doubt`;
- `confirmation_policy_expired`, `confirmation_policy_scope_mismatch`;
- every applicable Sprint 9 approval, stale, transaction, and Undo error.

Errors expose only phase, retry/action guidance, safe opaque IDs, and current
coordinates. Retryability is phase-specific. No error echoes source,
preimages, approval material, absolute paths, native IDs, or escrow metadata.

## 13. Acceptance and deferrals

The contract closes only when an independent fixture proves:

- prepare/staging is project-content and editor-history read-only;
- a mixed change set creates one native action and exact full Undo;
- explicit save changes only the approved scope;
- reload, diagnostics, index, and semantic graph reach bound revisions;
- validation failures trigger the selected policy;
- rollback either proves the complete baseline or reports blocked/in-doubt;
- crash/disconnect/response loss never replays apply or overwrites a conflict;
- confirmation grants cannot authorize elevated or persistent scope;
- reports, journal, logs, escrow metadata, and evidence pass redaction scans;
- Bridge RPC 1.0–1.7 and Sprint 6–9 regressions remain green.

Multi-scene/cross-history transactions, resource move/delete/rename, scene
save-as, closed-scene writes, binary `.res`, imported/addon/settings writes,
custom scripted Resources, ShaderMaterial and particle materials, GDScript
create/delete/rename, C#, arbitrary file patches, runtime mutation, persistent
Undo after editor restart, permanent approval, embedded UI, Windows/Linux
qualification, hosted CI, and model-facing qualification are deferred.
