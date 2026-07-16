# Sprint 3 Stage 4 — persistent resource index and MCP

**Status:** In progress

**Scope:** `S3-06`–`S3-08`

**Baseline:** `ee96aa2c51` on `codex/integration`

**Parent:** [SPRINT-3-PLAN.md](SPRINT-3-PLAN.md)

## 1. Outcome

Stage 4 connects the verified Bridge RPC 1.2 resource stream to the selected
`segment-v1` store, maintains a current committed resource graph across full snapshots,
incremental batches, restarts, and invalidation, and exposes two read-only MCP tools:

- `godot_get_resource_dependencies` for direct outgoing dependencies;
- `godot_find_resource_owners` for direct reverse owners.

Scene nodes, subresources, recursive graph traversal, `S3-09` cross-platform evidence,
and `S3-10` final Sprint audit remain outside this stage.

## 2. Frozen query contract

Both tools accept exactly `resource`, optional `limit`, and optional `cursor`. Resource
is a canonical `uid://` or `res://` selector. Limit defaults to 50 and is bounded to
1..200. Cursors are HMAC-authenticated, expire after at most five minutes, and bind the
project, tool, selector, limit, generation, index revision, and offset.

The public availability errors are distinct:

- `index_not_ready` means no committed compatible generation exists;
- `index_not_current` means a generation exists but has not been validated against the
  connected editor or lost continuity;
- `stale_cursor` means pagination cannot safely resume against the pinned generation.

Missing and stale dependency targets remain partial successful results with diagnostics;
they are not collapsed into tool failures.

## 3. Execution gates

1. Promote the measured segment candidate into the production `index-store` crate and
   retain the spike as a production-backend fault harness.
2. Add a storage-neutral normalizer and bounded source hasher for frozen resource,
   dependency, diagnostic, identity, and checkpoint records.
3. Add a resource coordinator using a separate authenticated Bridge connection for full
   snapshots, exact deltas, cancellation, reconnect, gap recovery, and cache validation.
4. Add the two MCP tools over one pinned immutable generation with deterministic
   pagination and structured errors.
5. Prove the golden direct/reverse graph, mutation phases, same-session reopen,
   corruption recovery, cursor isolation, and existing Sprint 1/2 regressions locally.

Remote CI is not part of this stage. Windows/Linux remain `not_run` until the separate
cross-platform gate produces evidence.

## 4. Commit boundaries

1. Contract and stage-plan freeze.
2. Production `segment-v1` store and recovery.
3. Resource normalizer and hashing.
4. Incremental coordinator and freshness state.
5. MCP tools and cursors.
6. Local live gate and evidence.
7. Completion documentation.
