# Sprint 3, Stage 2 — storage spike and decision D-05

**Status:** Complete locally; `D-05` selects the segment store

**Planned duration:** 3 working days

**Sprint package:** `S3-03` / `IDX-001A`

**Parent plan:** [SPRINT-3-PLAN.md](SPRINT-3-PLAN.md)

**Exit:** The full decision profile passes on the primary local macOS host, one backend
is selected by the frozen correctness-first rule, and the physical decision is recorded
in `INDEX-001`. Windows/Linux portability verification remains a separate later gate.

## 1. Outcome and boundary

Stage 2 implements a storage-neutral Rust contract, two test-only physical candidates,
and a process-level benchmark/fault harness. Both candidates ingest the same normalized
generations and are judged by correctness before speed.

This stage does not connect persistence to Bridge RPC, the Godot resource adapter, or
MCP. The chosen backend moves into production implementation during `S3-06`; neither
spike backend is a hidden production default while `D-05` is pending.

## 2. Delivered implementation

| Output | Artifact | Behavior |
|---|---|---|
| Storage-neutral API | `godot-codex-mcp/crates/index-store` | `IndexRead`, `IndexWriteTransaction`, `GenerationBuilder`, `MigrationRunner`, `IncrementalBatch`, resource DTOs, validation digest, direct/reverse query semantics |
| SQLite candidate | `tests/codex/storage_spike/src/sqlite.rs` | Bundled SQLite, WAL, `synchronous=FULL`, foreign keys, inactive staging rows, idempotent transactional activation, indexed direct/reverse lookup, bounded retired-generation GC |
| Segment candidate | `tests/codex/storage_spike/src/segment.rs` | Immutable SHA-256-addressed 256-way data/lookup shards, length-prefixed records, direct/reverse parity, idempotent unique-marker activation, bounded manifest/segment GC |
| Harness | `tests/codex/storage_spike` | Exact deterministic datasets, raw samples, parent-killed workers, corruption/quarantine, locking, migration, packaging/license manifests, decision scoring, strict evidence merge |
| Local evidence | `tests/codex/evidence/sprint-3-storage-spike.json` | Full macOS decision profile, raw samples, source digest, backend configs, manifests, scores, and selected backend |

Both physical candidates implement the storage-neutral interfaces. Test-only
`FaultInjection` remains outside the production crate and supports `capture`,
`staging`, `pre_commit`, and `post_commit` boundaries. Graceful cases return
`cancelled`; hard cases signal readiness, are killed by the parent process, reopen the
writer lease, and retry a durable post-commit generation idempotently.

## 3. Frozen spike protocol

- Canonical correctness input is every phase of the Stage 1 oracle: 18 baseline
  resources, 12 direct edges, and all rename/delete/re-add/reimport/content/gap
  transitions.
- `reference-medium` uses seed `D05-1`, 10,000 resources, 50,000 edges, cycles, and
  exact fan-in targets of 0, 1, 10, 100, and 1,000 owners.
- `stress-large` uses 100,000 resources and 500,000 edges and must survive compatible
  reopen with the same validation digest.
- The timed run performs five fresh builds, 50 UID-preserving rename commits, 1,000
  warmups, and 10,000 reverse queries returning a bounded page of 200 owners.
- Write amplification is changed 4 KiB durable bytes divided by the canonical JSON
  size of the same storage-neutral rename batch. Raw block and normalized-byte counts
  are retained. Artifact size is measured after retaining the active and one retired
  generation; unbounded historical accumulation is not compared as production behavior.
- A candidate is disqualified by an oracle/parity error, partial generation,
  cancellation/crash recovery error, corrupt cache returned as current, migration or
  process-lock failure, external runtime dependency, query p95 above 300 ms, or rename
  p95 above two seconds.

When both candidates qualify, the score weights are rename 30%, query p95 25%, full
build 20%, write amplification 15%, artifact size 5%, and packaging 5%. The local
decision uses same-host normalized scores and a deterministic 1,000-resample bootstrap
interval. A difference below five points or overlapping intervals selects bundled
SQLite as the documented tie-break; otherwise the higher score wins. The merge utility
remains available for later locally produced platform reports, but remote CI and
three-OS evidence are not part of this local completion gate.

## 4. Fault and recovery coverage

| Gate | Required observation |
|---|---|
| Graceful cancellation | Capture/staging/pre-commit keep the prior generation; post-commit exposes the new generation exactly once |
| Hard crash | Parent kills the child at each durable boundary; writer reopen selects the required generation and post-commit retry is idempotent |
| Concurrent reads | Four concurrent readers observe only the complete old or complete new generation during staging/activation |
| Process lock | A second writer returns `store_busy`; after normal exit or kill the store reopens without manual lock deletion |
| Corruption | Metadata/manifest, resource, reverse index, project binding, and incompatible schema cannot be returned current; quarantine leaves project bytes unchanged |
| Migration | Physical v1 scans resource lookup; v2 materializes it through a new generation; logical resources/edges remain equal and rerun is idempotent |
| Packaging | Segment and bundled-SQLite feature builds are standalone binaries; target-specific package/source/license manifests and binary sizes are recorded |

## 5. Local macOS completion evidence

**Verification date:** 2026-07-15

**Host:** macOS arm64, Rust 1.94.1

**Provenance:** local worktree evidence (`git_dirty=true`) with an explicit source-tree
digest. Publication and remote CI are separately authorized activities.

**Raw evidence:**
`tests/codex/evidence/sprint-3-storage-spike.json`

**Evidence SHA-256:**
`954f8bff429a42911287c99cc9f9739ab671d6434c9fe87253f2b549afd2a191`

| Metric | SQLite | Segment |
|---|---:|---:|
| Correctness/recovery gates | 12/12 passed | 12/12 passed |
| Detailed fault/corruption coordinates | 13/13 passed | 13/13 passed |
| Full build p50 | 787.63 ms | 12,875.79 ms |
| UID rename p50 / p95 | 1,265.21 / 1,618.29 ms | 408.98 / 478.45 ms |
| 200-owner cached query p50 / p95 | 0.181 / 0.204 ms | 0.012 / 0.015 ms |
| Rename changed 4 KiB blocks | 18,168 | 245 |
| Normalized rename bytes | 3,175 | 3,175 |
| Write amplification | 23,438.15× | 316.07× |
| Bounded artifact size | 212.38 MiB | 45.49 MiB |
| Feature binary size | 3.59 MiB | 1.64 MiB |
| Target-specific Cargo packages | 45 | 32 |
| Packages with declared license | 45/45 | 32/32 |
| Local weighted score | 0.3508 | 0.8122 |
| Bootstrap score 95% CI | 0.3474–0.3556 | 0.8118–0.8138 |

Both candidates satisfy the initial SLOs and every local correctness gate. `D-05`
selects the segment candidate because faster rename/query behavior, lower write
amplification, and smaller artifacts outweigh its slower full build.

## 6. Reproduction

```bash
cargo test --locked --manifest-path godot-codex-mcp/Cargo.toml \
  -p godot-codex-index-store
cargo test --locked --all-features \
  --manifest-path tests/codex/storage_spike/Cargo.toml
cargo run --locked --release \
  --manifest-path tests/codex/storage_spike/Cargo.toml -- \
  run --backend all --dataset all --repo-root "$PWD" \
  --output tests/codex/evidence/sprint-3-storage-spike.json
```

Add `--quick` only for development; it records `profile=quick` and omits the stress and
packaging gates, so it cannot qualify the local decision. The full command above writes
the canonical local evidence directly.

## 7. Completion record and remaining portability work

The full local matrix passed and selected `segment`; `INDEX-001` records its physical
schema, locking, flush, recovery, migration, and the measured SQLite rejection rationale.
Stage 2 and `S3-03` are therefore complete for the local implementation stream, and
`S3-04` may begin.

Windows and Linux process-lock/reopen behavior is still `not_run`. It remains a later
portability/release check and must not be described as passed. It does not require a
GitHub workflow, push, or remote CI; if performed, its raw reports are produced by the
same CLI on those hosts.
