# Sprint 3 Stage 4 — persistent resource index and MCP

**Status:** Complete and locally verified on macOS arm64

**Scope:** `S3-06`–`S3-08`

**Baseline:** `ee96aa2c51` on `codex/integration`

**Parent:** [SPRINT-3-PLAN.md](SPRINT-3-PLAN.md)

**Evidence:**
[`tests/codex/evidence/sprint-3-stage-4-index-mcp-macos.json`](../../tests/codex/evidence/sprint-3-stage-4-index-mcp-macos.json)

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

## 5. Implemented result

| Layer | Completed behavior |
|---|---|
| Store | Production `segment-v1`, 256 content-addressed shards, separate direct/reverse/lookup shards, writer lease, immutable reader snapshots, atomic commit marker, retention/GC, migration, corruption detection |
| Normalizer | NFC/path security, frozen UID/path-content/edge identities, bounded four-worker streaming SHA-256, authority normalization, deterministic diagnostics/digest, exact incremental diff |
| Ingestion | Disk-backed bounded snapshot spool, separate Bridge session, durable batch ID/checksum checkpoint, same-session catch-up, new-session rebuild, gap invalidation/full rebuild, quarantine, cancellation |
| MCP | Exactly five read-only tools; two resource tools with default 50 / maximum 200, one-generation pagination, five-minute HMAC cursor, exact/partial results, safe structured errors |

The full-snapshot validator deliberately accepts each unchanged record's last observed
resource revision in `1..=snapshot_revision`; incremental upserts remain bound exactly
to their batch revision. Resource snapshot requests use the schema-valid 30-second RPC
deadline while the negotiated bulk transfer is allowed its full 120-second timeout.
Both rules were found and verified by the large journal-gap live phase.

## 6. Local model-free evidence

The macOS arm64 gate launched the real editor and release sidecar against fresh
short-path fixture copies. All eight phases passed: base, UID rename, UID-less rename,
delete, re-add, reimport, content edit, and journal gap. Every phase matched the frozen
18-resource/12-edge oracle (17 resources and three diagnostics after delete), including
direct/reverse parity and partial missing/stale results.

| Measurement | Local result |
|---|---:|
| Resource query | p50 `0.186 ms`; p95 `0.351 ms` |
| Status/ping | p50 `0.188 ms`; p95 `0.244 ms` |
| Combined startup/change visibility samples | p50 `2140.192 ms`; p95 `2508.333 ms` |
| Ordinary incremental visibility | `158.291–428.113 ms` in the final run |
| Same-session sidecar reopen | `54.673 ms`; identical generation, revision, and immutable segment set |
| Forced 1818-resource gap rebuild | `39988.478 ms`; full-snapshot checkpoint activated at index revision 2 |

The evidence preserves every raw timing sample. It contains no canonical project path,
tokens, source bytes, or transport endpoint.

## 7. Final local regression

| Gate | Result |
|---|---|
| Production Rust workspace format/test/Clippy/release | Passed; 31 unit tests plus doc tests |
| Bridge conformance format/test/Clippy/release | Passed; 11 Rust tests |
| Storage spike production adapter | Passed; 9 tests |
| Resource oracle contracts and all live phases | Passed; 11 Python tests and all 8 fixture phases |
| Full local Godot suite | Passed; 1437 cases, 425938 assertions, 3 skipped |
| Sprint 2 live editor → Bridge → sidecar → MCP regression | Passed on a fresh temporary project copy |
| Stage 4 editor → persistent index → both MCP tools | Passed on all 8 phases |

Warnings deliberately emitted by unrelated negative/full-suite tests did not produce a
test failure. The final Godot status was `SUCCESS` with zero failed cases or assertions.

## 8. Remaining Sprint 3 work

`S3-06`, `S3-07`, and `S3-08` are complete for the local implementation stream.
`S3-09` remains the separate Windows/macOS cross-platform live resource-index smoke,
and `S3-10` remains the final Sprint-wide audit. Windows x86_64, Linux x86_64, and
remote CI are explicitly `not_run` by this Stage 4 evidence.
