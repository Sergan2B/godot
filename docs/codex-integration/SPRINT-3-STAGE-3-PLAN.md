# Sprint 3 Stage 3 — Bridge RPC 1.2 and ResourceGraphAdapter

**Status:** Complete and locally verified on macOS arm64

**Completed:** 2026-07-16

**Scope:** `S3-04` and `S3-05`

**Baseline:** `6115a83a12` on `codex/integration`

**Parent:** [SPRINT-3-PLAN.md](SPRINT-3-PLAN.md)

**Evidence:**
[`tests/codex/evidence/sprint-3-stage-3-bridge.json`](../../tests/codex/evidence/sprint-3-stage-3-bridge.json)

## 1. Completed outcome

Stage 3 adds the production transport boundary between Godot editor resource facts and
the future sidecar index:

- Bridge RPC negotiates `1.2`, `1.1`, or `1.0` without changing the `1.0`/`1.1`
  message contracts;
- `ResourceGraphAdapter` captures a deterministic editor-owned resource catalog on the
  main thread;
- `ResourceDeltaJournal` provides continuous bounded batches, coalescing, and exact gap
  detection;
- resource snapshots use the existing authenticated stream with per-chunk checksum,
  cumulative ACK, a 32 MiB unacknowledged window, cancellation, and a 120-second
  timeout;
- the Rust `BridgeClient` exposes strict typed snapshot streaming, materialized
  snapshots, and `Current | Batch | Gap` delta polling.

No `IndexStore`, hashing, normalization, reconciliation, or resource MCP tools are
wired into this stage. Those remain `S3-06`–`S3-08`.

## 2. Bridge RPC 1.2 contract

The major-v1 schema bundle now contains strict resource DTOs and canonical valid and
invalid fixtures. A 1.2 initialization advertises:

- `resource.uid_dependencies` version `1.0`;
- `resource.incremental_index` version `1.0`.

The new methods are:

- `resource.snapshot.get` for one frozen resource generation;
- `resource.delta.get(after_resource_revision)` for exactly one next batch, `current`,
  or a structured gap/future-revision error.

Resource facts contain only editor authority: UID or `uid_missing`, `res://` path,
Godot type, source/import state, mtime, size, declared dependencies, resolution, and
diagnostics. Entity/edge IDs, content hashes, and UID-less content generations remain
sidecar responsibilities.

The fixed resource errors are implemented as:

- `capability_unavailable`;
- `resource_catalog_building`;
- `resource_snapshot_in_progress`;
- `resource_journal_gap`;
- `invalid_revision`;
- `resource_limit_exceeded`.

For 1.2, the revision vector requires `resource_revision`. Older profiles do not
receive that field. A future major-one minor is capped at 1.2; a 1.2 client accepts a
1.1/1.0 downgrade and keeps the Sprint 2 editor surface while resource methods fail
explicitly.

## 3. Godot adapter and journal

`ResourceGraphAdapter` traverses `EditorFileSystem` resumably and consults
`ResourceLoader::get_dependencies(..., add_types=true)` without recursively loading
the graph. It listens to `filesystem_changed`, `resources_reimported`, and
`resources_reload`. The initial complete generation is resource revision 1.

Snapshots freeze the catalog until the last cumulative chunk ACK, disconnect,
cancellation, or timeout. Editor changes during that interval retain a dirty refresh
request and become the next delta after release. Only one resource snapshot is active
at a time.

The shared main-thread ceiling remains 2 ms/frame:

- when resource work is pending, dispatcher work receives up to 1.5 ms and resource
  work receives 0.5 ms;
- otherwise the dispatcher may use the full 2 ms.

The hard limits are:

| Limit | Value |
|---|---:|
| Resources | 250,000 |
| Dependencies | 2,000,000 |
| Dependencies per resource | 4,096 |
| UTF-8 resource path | 1,024 bytes |
| Produced snapshot payload | 256 KiB |
| Negotiated snapshot ceiling | 512 KiB |
| Unacknowledged snapshot window | 32 MiB |
| Delta batch | 512 KiB |
| Journal retention | 4,096 batches / 16 MiB |
| Snapshot timeout | 120 seconds |

The journal coalesces repeated upserts, UID move chains, add/remove pairs, and
add/remove/add sequences before committing a revision. UID-less rename remains
`remove + upsert`. Oversized batches clear continuity and require a new snapshot; no
partial exact result is served.

## 4. Rust production client

`godot-codex-bridge-client` now exports:

- `BridgeClient::connect`;
- `NegotiatedBridgeProfile`;
- `ResourceSnapshotSink` and `stream_resource_snapshot`;
- `get_resource_snapshot`;
- `get_next_resource_delta` returning `ResourceDeltaPoll::{Current, Batch, Gap}`;
- strict DTOs for resource observations, dependencies, diagnostics, snapshot
  messages, and all four delta operation variants.

Before handing a message to a consumer, the client checks connection context,
protocol/domain, ordering, revision continuity, strict fields, path/UID policy,
payload equivalence, checksum, counts, and hard limits. Validation or sink failure
sends `stream.cancel`; every accepted chunk is cumulatively ACKed.

## 5. Local acceptance record

All acceptance work ran locally. No GitHub CI wait or remote-platform claim is part of
this completion record.

| Gate | Result |
|---|---|
| macOS arm64 developer build with tests | Passed |
| Focused `[CodexBridge]` C++ suite | 32 cases, 4,781 assertions passed |
| Full local Godot suite | 1,437 cases, 425,938 assertions passed; 3 skipped |
| Major-v1 Rust schema/conformance suite | 11 tests passed |
| Stage 1 Python contract suite | 11 tests passed |
| Stage 1 Godot API oracle, all 8 phases | Passed |
| Stage 3 live resource bridge, all 8 phases | Passed |
| Rust workspace tests | 20 tests passed |
| Rust format, Clippy `-D warnings`, release build | Passed |
| Sprint 1 authenticated live conformance | 82 cases passed |
| Sprint 2 editor → bridge → sidecar → MCP regression | Passed |

The Stage 3 live gate checks the exact 18-resource/12-dependency/2-diagnostic base
projection and the independent `rename_uid`, `rename_uidless`, `delete`, `re_add`,
`reimport`, `content_edit`, and `journal_gap` phases. The gap phase creates an
oversized live delta and verifies a typed `Gap`, not a truncated batch. The canonical
fixture digest is identical before and after the run.

Reproduce it with:

```sh
python3 tests/codex/sprint3_stage3_live.py \
  --godot bin/godot.macos.editor.dev.arm64
```

## 6. Remaining boundary and next gate

Windows x86_64 and Linux portability for Stage 3 are `not_run`; remote CI is also
`not_run`. These states are explicit and do not weaken the successful local macOS
gate.

The next task is `S3-06`: feed this verified wire API into the selected segment-store
`IndexStore`, calculate sidecar-owned identity/content generations, and commit atomic
static generations. Bridge transport semantics should not be expanded during that
work unless a reproduced contract defect requires it.
